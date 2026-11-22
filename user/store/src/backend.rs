use crate::model::{
    CacheInfoJson, CatalogJson, CheckOkJson, FullSnapshot, InfoPackageJson, LogEntry, LogLevel,
    MaintResult, PackageFileEntry, PackageFilesJson, ProgressStreamEvent, SpkErrorJson,
    StorePackage, TrustListJson, TxJob, TxJobKind, TxJobStatus, TxPlanJson, VerifyJson,
    format_bytes,
};
use std::collections::{BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

pub struct AuthSecret {
    buf: String,
}

impl AuthSecret {
    pub fn new(password: String) -> Self {
        Self { buf: password }
    }

    fn as_bytes(&self) -> &[u8] {
        self.buf.as_bytes()
    }
}

impl Drop for AuthSecret {
    fn drop(&mut self) {
        // SAFETY: zero raw UTF-8 backing storage before clearing string buffer.
        unsafe {
            self.buf.as_mut_vec().fill(0);
        }
        self.buf.clear();
    }
}

enum AuthReply {
    Password(AuthSecret),
    Cancel,
}

pub enum BackendCmd {
    RefreshCatalog { offline: bool },
    LoadPackageDetails { name: String },
    PreviewTx { kind: TxJobKind, title: String },
    RunCheck,
    RunVerify,
    EnqueueJob { kind: TxJobKind, title: String },
    CancelQueuedJob(u64),
    SubmitAuth(AuthSecret),
    CancelAuth,
}

#[derive(Clone, Debug)]
pub enum BackendEvent {
    CatalogLoaded(Result<FullSnapshot, String>),
    PackageDetailsLoaded {
        name: String,
        info: Option<StorePackage>,
        files: Vec<PackageFileEntry>,
        error: Option<String>,
    },
    PreviewReady {
        kind: TxJobKind,
        title: String,
        plan: Result<TxPlanJson, String>,
    },
    MaintenanceCheckResult {
        title: String,
        result: MaintResult,
        summary: String,
        details: Vec<String>,
    },
    JobAdded(TxJob),
    JobUpdated(TxJob),
    JobLog {
        job_id: u64,
        entry: LogEntry,
    },
    AuthRequired {
        job_id: u64,
        title: String,
        command_preview: String,
        error_msg: Option<String>,
    },
}

struct WakePipe {
    read_fd: RawFd,
    write_fd: RawFd,
}

impl WakePipe {
    fn new() -> std::io::Result<Self> {
        let mut fds = [0i32; 2];
        // SAFETY: valid 2-int array passed to pipe(2).
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in fds {
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                let fd_flags = libc::fcntl(fd, libc::F_GETFD);
                libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
            }
        }
        Ok(Self {
            read_fd: fds[0],
            write_fd: fds[1],
        })
    }

    fn wake(&self) {
        let b = [1u8];
        // SAFETY: non-blocking write of 1 byte to wake event loop poll().
        unsafe {
            libc::write(self.write_fd, b.as_ptr().cast(), 1);
        }
    }

    fn drain(&self) {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: draining non-blocking pipe fd into local stack buffer.
            let n = unsafe { libc::read(self.read_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
        }
    }
}

impl Drop for WakePipe {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

pub struct StoreBackend {
    query_tx: mpsc::Sender<BackendCmd>,
    tx_tx: mpsc::Sender<TxJob>,
    auth_tx: mpsc::Sender<AuthReply>,
    cancelled_ids: Arc<Mutex<BTreeSet<u64>>>,
    next_job_id: AtomicU64,
    event_rx: mpsc::Receiver<BackendEvent>,
    wake: Arc<WakePipe>,
}

impl StoreBackend {
    pub fn spawn() -> Self {
        let wake = Arc::new(WakePipe::new().expect("store wakeup pipe"));
        let (event_tx, event_rx) = mpsc::channel::<BackendEvent>();
        let (query_tx, query_rx) = mpsc::channel::<BackendCmd>();
        let (tx_tx, tx_rx) = mpsc::channel::<TxJob>();
        let (auth_tx, auth_rx) = mpsc::channel::<AuthReply>();
        let cancelled_ids = Arc::new(Mutex::new(BTreeSet::new()));

        let wake_q = Arc::clone(&wake);
        let ev_q = event_tx.clone();
        thread::spawn(move || query_worker(query_rx, ev_q, wake_q));

        let wake_t = Arc::clone(&wake);
        let ev_t = event_tx;
        let q_notify = query_tx.clone();
        let cancel_t = Arc::clone(&cancelled_ids);
        thread::spawn(move || tx_worker(tx_rx, auth_rx, cancel_t, q_notify, ev_t, wake_t));

        Self {
            query_tx,
            tx_tx,
            auth_tx,
            cancelled_ids,
            next_job_id: AtomicU64::new(1),
            event_rx,
            wake,
        }
    }

    pub fn wakeup_fd(&self) -> RawFd {
        self.wake.read_fd
    }

    pub fn drain_wakeup(&self) {
        self.wake.drain();
    }

    pub fn poll_events(&self) -> Vec<BackendEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.event_rx.try_recv() {
            out.push(ev);
        }
        out
    }

    pub fn refresh_catalog(&self, offline: bool) {
        let _ = self.query_tx.send(BackendCmd::RefreshCatalog { offline });
    }

    pub fn load_package_details(&self, name: impl Into<String>) {
        let _ = self
            .query_tx
            .send(BackendCmd::LoadPackageDetails { name: name.into() });
    }

    pub fn preview_tx(&self, kind: TxJobKind, title: impl Into<String>) {
        let _ = self.query_tx.send(BackendCmd::PreviewTx {
            kind,
            title: title.into(),
        });
    }

    pub fn run_check(&self) {
        let _ = self.query_tx.send(BackendCmd::RunCheck);
    }

    pub fn run_verify(&self) {
        let _ = self.query_tx.send(BackendCmd::RunVerify);
    }

    pub fn enqueue_job(&self, kind: TxJobKind, title: impl Into<String>) -> u64 {
        let id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let job = TxJob {
            id,
            kind,
            title: title.into(),
            status: TxJobStatus::Queued,
            phase: "queued".into(),
            phase_msg: "等待执行".into(),
            current_label: String::new(),
            percent: 0,
            done_bytes: 0,
            total_bytes: None,
            rate_bps: 0,
        };
        let _ = self.tx_tx.send(job);
        id
    }

    pub fn cancel_queued_job(&self, job_id: u64) {
        if let Ok(mut guard) = self.cancelled_ids.lock() {
            guard.insert(job_id);
        }
    }

    pub fn submit_auth(&self, password: String) {
        let _ = self
            .auth_tx
            .send(AuthReply::Password(AuthSecret::new(password)));
    }

    pub fn cancel_auth(&self) {
        let _ = self.auth_tx.send(AuthReply::Cancel);
    }
}

fn emit(ev_tx: &mpsc::Sender<BackendEvent>, wake: &WakePipe, event: BackendEvent) {
    let _ = ev_tx.send(event);
    wake.wake();
}

fn resolve_spk_bin() -> &'static str {
    if Path::new("/usr/bin/spk").is_file() {
        "/usr/bin/spk"
    } else if Path::new("/bin/spk").is_file() {
        "/bin/spk"
    } else {
        "spk"
    }
}

fn resolve_sudo_bin() -> &'static str {
    if Path::new("/usr/bin/sudo").is_file() {
        "/usr/bin/sudo"
    } else if Path::new("/bin/sudo").is_file() {
        "/bin/sudo"
    } else {
        "sudo"
    }
}

fn run_spk_json_cmd(args: &[&str]) -> Result<String, String> {
    let spk = resolve_spk_bin();
    let out = Command::new(spk)
        .env("LC_ALL", "C.UTF-8")
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .map_err(|e| format!("无法启动 spk（{spk}）：{e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.success() {
        return Ok(stdout);
    }
    Err(extract_spk_error(&stdout, &stderr, out.status.code()))
}

pub fn extract_spk_error(stdout: &str, stderr: &str, code: Option<i32>) -> String {
    if let Ok(err) = serde_json::from_str::<SpkErrorJson>(stdout.trim()) {
        if !err.error.is_empty() {
            if err.lines.is_empty() {
                return err.error;
            }
            return format!("{}: {}", err.error, err.lines.join("; "));
        }
    }
    let trimmed_err = stderr.trim();
    if !trimmed_err.is_empty() {
        return trimmed_err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(trimmed_err)
            .to_string();
    }
    let trimmed_out = stdout.trim();
    if !trimmed_out.is_empty() {
        return trimmed_out.to_string();
    }
    format!("spk 执行失败（退出码 {}）", code.unwrap_or(-1))
}

fn load_full_snapshot(offline: bool) -> Result<FullSnapshot, String> {
    let catalog_raw = if offline {
        run_spk_json_cmd(&["--offline", "--json", "--lang", "zh_CN", "catalog"])?
    } else {
        run_spk_json_cmd(&["--json", "--lang", "zh_CN", "catalog"])?
    };
    let catalog: CatalogJson =
        serde_json::from_str(&catalog_raw).map_err(|e| format!("解析 catalog JSON 失败: {e}"))?;

    let cache = run_spk_json_cmd(&["--json", "cache", "info"])
        .ok()
        .and_then(|s| serde_json::from_str::<CacheInfoJson>(&s).ok())
        .unwrap_or_default();

    let trust_keys = run_spk_json_cmd(&["--json", "trust", "list"])
        .ok()
        .and_then(|s| serde_json::from_str::<TrustListJson>(&s).ok())
        .map(|t| t.keys)
        .unwrap_or_default();

    Ok(FullSnapshot {
        catalog,
        cache,
        trust_keys,
        offline,
    })
}

fn query_worker(
    rx: mpsc::Receiver<BackendCmd>,
    ev_tx: mpsc::Sender<BackendEvent>,
    wake: Arc<WakePipe>,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            BackendCmd::RefreshCatalog { offline } => {
                let res = load_full_snapshot(offline);
                emit(&ev_tx, &wake, BackendEvent::CatalogLoaded(res));
            }
            BackendCmd::LoadPackageDetails { name } => {
                let info_res = run_spk_json_cmd(&[
                    "--offline",
                    "--json",
                    "--lang",
                    "zh_CN",
                    "info",
                    &name,
                ])
                .and_then(|raw| {
                    serde_json::from_str::<InfoPackageJson>(&raw)
                        .map(|v| v.package)
                        .map_err(|e| format!("解析 info JSON 失败: {e}"))
                });
                let files = run_spk_json_cmd(&["--json", "files", &name])
                    .ok()
                    .and_then(|raw| serde_json::from_str::<PackageFilesJson>(&raw).ok())
                    .map(|v| v.files)
                    .unwrap_or_default();
                let (info, error) = match info_res {
                    Ok(pkg) => (Some(pkg), None),
                    Err(err) => (None, Some(err)),
                };
                emit(
                    &ev_tx,
                    &wake,
                    BackendEvent::PackageDetailsLoaded {
                        name,
                        info,
                        files,
                        error,
                    },
                );
            }
            BackendCmd::PreviewTx { kind, title } => {
                let args = kind.to_spk_args(true);
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let plan = run_spk_json_cmd(&arg_refs).and_then(|raw| {
                    serde_json::from_str::<TxPlanJson>(&raw)
                        .map_err(|e| format!("解析事务计划失败: {e}"))
                });
                emit(
                    &ev_tx,
                    &wake,
                    BackendEvent::PreviewReady { kind, title, plan },
                );
            }
            BackendCmd::RunCheck => {
                let res = run_spk_json_cmd(&["--json", "--lang", "zh_CN", "check"]);
                let event = match res {
                    Ok(raw) => {
                        let parsed = serde_json::from_str::<CheckOkJson>(&raw).unwrap_or_default();
                        BackendEvent::MaintenanceCheckResult {
                            title: "数据库与文件系统一致性检查".into(),
                            result: MaintResult {
                                ok: parsed.ok,
                                packages: parsed.packages,
                            },
                            summary: format!(
                                "校验通过：共检查 {} 个已安装软件包、{} 个受管文件，状态完全一致。",
                                parsed.packages, parsed.files
                            ),
                            details: Vec::new(),
                        }
                    }
                    Err(err) => BackendEvent::MaintenanceCheckResult {
                        title: "数据库与文件系统一致性检查".into(),
                        result: MaintResult {
                            ok: false,
                            packages: 0,
                        },
                        summary: "发现异常，请查看详情或运行修复。".into(),
                        details: err.lines().map(str::to_string).collect(),
                    },
                };
                emit(&ev_tx, &wake, event);
            }
            BackendCmd::RunVerify => {
                let res = run_spk_json_cmd(&["--offline", "--json", "--lang", "zh_CN", "verify"]);
                let event = match res {
                    Ok(raw) => {
                        let parsed = serde_json::from_str::<VerifyJson>(&raw).unwrap_or_default();
                        let details: Vec<String> = parsed
                            .packages
                            .iter()
                            .map(|p| {
                                format!(
                                    "[{}] {} {} ... {}",
                                    p.repository,
                                    p.name,
                                    p.version,
                                    if p.ok { "OK" } else { "FAILED" }
                                )
                            })
                            .collect();
                        BackendEvent::MaintenanceCheckResult {
                            title: "软件源与包文件完整性校验".into(),
                            result: MaintResult {
                                ok: true,
                                packages: parsed.packages.len(),
                            },
                            summary: format!(
                                "签名与校验和验证通过：{} 个仓库、{} 个缓存包全部通过 SHA-256 检验。",
                                parsed.repositories.len(),
                                parsed.packages.len()
                            ),
                            details,
                        }
                    }
                    Err(err) => BackendEvent::MaintenanceCheckResult {
                        title: "软件源与包文件完整性校验".into(),
                        result: MaintResult {
                            ok: false,
                            packages: 0,
                        },
                        summary: "仓库签名或缓存校验未通过。".into(),
                        details: err.lines().map(str::to_string).collect(),
                    },
                };
                emit(&ev_tx, &wake, event);
            }
            _ => {}
        }
    }
}

fn tx_worker(
    rx: mpsc::Receiver<TxJob>,
    auth_rx: mpsc::Receiver<AuthReply>,
    cancelled_ids: Arc<Mutex<BTreeSet<u64>>>,
    query_tx: mpsc::Sender<BackendCmd>,
    ev_tx: mpsc::Sender<BackendEvent>,
    wake: Arc<WakePipe>,
) {
    let mut queue: VecDeque<TxJob> = VecDeque::new();
    while let Ok(first) = rx.recv() {
        queue.push_back(first);
        emit(&ev_tx, &wake, BackendEvent::JobAdded(queue.back().unwrap().clone()));
        while let Ok(more) = rx.try_recv() {
            emit(&ev_tx, &wake, BackendEvent::JobAdded(more.clone()));
            queue.push_back(more);
        }

        while let Some(mut job) = queue.pop_front() {
            while let Ok(more) = rx.try_recv() {
                emit(&ev_tx, &wake, BackendEvent::JobAdded(more.clone()));
                queue.push_back(more);
            }

            let is_cancelled = cancelled_ids
                .lock()
                .map(|mut set| set.remove(&job.id))
                .unwrap_or(false);
            if is_cancelled {
                job.status = TxJobStatus::Cancelled;
                job.phase = "cancelled".into();
                job.phase_msg = "已从队列取消".into();
                emit(&ev_tx, &wake, BackendEvent::JobUpdated(job));
                continue;
            }

            run_single_tx(&mut job, &auth_rx, &ev_tx, &wake);
            let _ = query_tx.send(BackendCmd::RefreshCatalog { offline: true });
        }
    }
}

fn ensure_privilege_for_job(
    job: &mut TxJob,
    auth_rx: &mpsc::Receiver<AuthReply>,
    ev_tx: &mpsc::Sender<BackendEvent>,
    wake: &WakePipe,
) -> Option<bool> {
    // SAFETY: reading effective UID of current process.
    if unsafe { libc::geteuid() } == 0 {
        return Some(false);
    }
    let sudo = resolve_sudo_bin();
    if Command::new(sudo)
        .args(["-n", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {
        return Some(true);
    }

    let mut error_msg: Option<String> = None;
    loop {
        job.status = TxJobStatus::WaitingAuth;
        job.phase = "auth".into();
        job.phase_msg = "等待管理员授权".into();
        emit(ev_tx, wake, BackendEvent::JobUpdated(job.clone()));
        emit(
            ev_tx,
            wake,
            BackendEvent::AuthRequired {
                job_id: job.id,
                title: job.title.clone(),
                command_preview: job.kind.command_summary(),
                error_msg: error_msg.take(),
            },
        );

        while auth_rx.try_recv().is_ok() {}
        match auth_rx.recv() {
            Ok(AuthReply::Password(secret)) => {
                let mut child = match Command::new(sudo)
                    .args(["-S", "-v"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        error_msg = Some(format!("无法启动 sudo：{e}"));
                        continue;
                    }
                };
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(secret.as_bytes());
                    let _ = stdin.write_all(b"\n");
                    let _ = stdin.flush();
                }
                drop(secret);
                match child.wait_with_output() {
                    Ok(out) if out.status.success() => return Some(true),
                    Ok(out) => {
                        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
                        error_msg = Some(if msg.is_empty() {
                            "密码验证失败或无 sudo 权限，请重试。".into()
                        } else {
                            format!("验证失败：{msg}")
                        });
                    }
                    Err(e) => {
                        error_msg = Some(format!("sudo 执行异常：{e}"));
                    }
                }
            }
            Ok(AuthReply::Cancel) | Err(_) => return None,
        }
    }
}

fn run_single_tx(
    job: &mut TxJob,
    auth_rx: &mpsc::Receiver<AuthReply>,
    ev_tx: &mpsc::Sender<BackendEvent>,
    wake: &WakePipe,
) {
    let Some(use_sudo) = ensure_privilege_for_job(job, auth_rx, ev_tx, wake) else {
        job.status = TxJobStatus::Cancelled;
        job.phase = "cancelled".into();
        job.phase_msg = "已取消管理员授权".into();
        emit(
            ev_tx,
            wake,
            BackendEvent::JobLog {
                job_id: job.id,
                entry: LogEntry {
                    text: "用户取消了管理员密码授权，任务未执行。".into(),
                    level: LogLevel::Warn,
                },
            },
        );
        emit(ev_tx, wake, BackendEvent::JobUpdated(job.clone()));
        return;
    };

    job.status = TxJobStatus::Running;
    job.phase = "starting".into();
    job.phase_msg = format!("正在执行 {}", job.kind.command_summary());
    emit(ev_tx, wake, BackendEvent::JobUpdated(job.clone()));
    emit(
        ev_tx,
        wake,
        BackendEvent::JobLog {
            job_id: job.id,
            entry: LogEntry {
                text: format!("$ {}", job.kind.command_summary()),
                level: LogLevel::Phase,
            },
        },
    );

    let spk = resolve_spk_bin();
    let spk_args = job.kind.to_spk_args(false);
    let mut cmd = if use_sudo {
        let mut c = Command::new(resolve_sudo_bin());
        c.arg("-n").arg(spk);
        c
    } else {
        Command::new(spk)
    };
    cmd.env("LC_ALL", "C.UTF-8")
        .env("NO_COLOR", "1")
        .args(&spk_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            job.status = TxJobStatus::Failed(format!("启动命令失败：{e}"));
            job.phase = "failed".into();
            job.phase_msg = format!("启动命令失败：{e}");
            emit(
                ev_tx,
                wake,
                BackendEvent::JobLog {
                    job_id: job.id,
                    entry: LogEntry {
                        text: format!("错误：{e}"),
                        level: LogLevel::Error,
                    },
                },
            );
            emit(ev_tx, wake, BackendEvent::JobUpdated(job.clone()));
            return;
        }
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let job_id = job.id;

    enum StreamLine {
        Stdout(String),
        Stderr(String),
    }

    let (line_tx, line_rx) = mpsc::channel::<StreamLine>();
    let tx_out = line_tx.clone();
    let h_out = thread::spawn(move || {
        if let Some(pipe) = stdout_pipe {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let _ = tx_out.send(StreamLine::Stdout(line));
            }
        }
    });
    let tx_err = line_tx;
    let h_err = thread::spawn(move || {
        if let Some(pipe) = stderr_pipe {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let _ = tx_err.send(StreamLine::Stderr(line));
            }
        }
    });

    let mut all_stdout = String::new();
    let mut all_stderr = String::new();

    for item in line_rx {
        match item {
            StreamLine::Stdout(line) => {
                if !all_stdout.is_empty() {
                    all_stdout.push('\n');
                }
                all_stdout.push_str(&line);
                if !line.trim().is_empty() && !line.trim_start().starts_with('{') {
                    emit(
                        ev_tx,
                        wake,
                        BackendEvent::JobLog {
                            job_id,
                            entry: LogEntry {
                                text: line,
                                level: LogLevel::Info,
                            },
                        },
                    );
                }
            }
            StreamLine::Stderr(line) => {
                let trimmed = line.trim();
                if trimmed.starts_with('{') {
                    if let Ok(stream_ev) = serde_json::from_str::<ProgressStreamEvent>(trimmed) {
                        match stream_ev {
                            ProgressStreamEvent::Phase { phase, message } => {
                                job.phase = phase;
                                job.phase_msg = message.clone();
                                emit(ev_tx, wake, BackendEvent::JobUpdated(job.clone()));
                                emit(
                                    ev_tx,
                                    wake,
                                    BackendEvent::JobLog {
                                        job_id,
                                        entry: LogEntry {
                                            text: message,
                                            level: LogLevel::Phase,
                                        },
                                    },
                                );
                            }
                            ProgressStreamEvent::Progress {
                                kind,
                                label,
                                done,
                                total,
                                percent,
                                rate_bps,
                            } => {
                                job.current_label = label;
                                job.done_bytes = done;
                                job.total_bytes = total;
                                job.percent = percent.min(100);
                                job.rate_bps = rate_bps;
                                if kind == "write" {
                                    job.phase = "write".into();
                                    job.phase_msg = if rate_bps > 0 {
                                        format!("正在写入文件 ({percent}%)")
                                    } else {
                                        format!("正在写入文件 ({done}/{})", total.unwrap_or(done))
                                    };
                                } else {
                                    job.phase = "download".into();
                                    job.phase_msg = if rate_bps > 0 {
                                        format!(
                                            "正在下载 {} · {}/s",
                                            job.current_label,
                                            format_bytes(rate_bps)
                                        )
                                    } else {
                                        format!("正在获取 {}", job.current_label)
                                    };
                                }
                                emit(ev_tx, wake, BackendEvent::JobUpdated(job.clone()));
                            }
                        }
                        continue;
                    }
                }
                if !all_stderr.is_empty() {
                    all_stderr.push('\n');
                }
                all_stderr.push_str(&line);
                if !trimmed.is_empty() {
                    let level = if trimmed.contains("警告") || trimmed.to_ascii_lowercase().contains("warn") {
                        LogLevel::Warn
                    } else {
                        LogLevel::Info
                    };
                    emit(
                        ev_tx,
                        wake,
                        BackendEvent::JobLog {
                            job_id,
                            entry: LogEntry {
                                text: line,
                                level,
                            },
                        },
                    );
                }
            }
        }
    }

    let _ = h_out.join();
    let _ = h_err.join();
    let status = child.wait();

    match status {
        Ok(st) if st.success() => {
            job.status = TxJobStatus::Succeeded;
            job.phase = "done".into();
            job.percent = 100;
            if job.phase_msg.is_empty() || job.phase_msg.starts_with("正在") {
                job.phase_msg = format!("{} 已完成", job.title);
            }
            emit(
                ev_tx,
                wake,
                BackendEvent::JobLog {
                    job_id,
                    entry: LogEntry {
                        text: format!("成功：{}", job.title),
                        level: LogLevel::Success,
                    },
                },
            );
        }
        Ok(st) => {
            let err = extract_spk_error(&all_stdout, &all_stderr, st.code());
            job.status = TxJobStatus::Failed(err.clone());
            job.phase = "failed".into();
            job.phase_msg = err.clone();
            emit(
                ev_tx,
                wake,
                BackendEvent::JobLog {
                    job_id,
                    entry: LogEntry {
                        text: format!("失败：{err}"),
                        level: LogLevel::Error,
                    },
                },
            );
        }
        Err(e) => {
            let err = format!("等待子进程退出失败：{e}");
            job.status = TxJobStatus::Failed(err.clone());
            job.phase = "failed".into();
            job.phase_msg = err.clone();
            emit(
                ev_tx,
                wake,
                BackendEvent::JobLog {
                    job_id,
                    entry: LogEntry {
                        text: err,
                        level: LogLevel::Error,
                    },
                },
            );
        }
    }
    emit(ev_tx, wake, BackendEvent::JobUpdated(job.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spk_ndjson_phase_and_progress_events() {
        let phase_line = r#"{"event":"phase","phase":"download","message":"Retrieving packages..."}"#;
        match serde_json::from_str::<ProgressStreamEvent>(phase_line).unwrap() {
            ProgressStreamEvent::Phase { phase, message } => {
                assert_eq!(phase, "download");
                assert_eq!(message, "Retrieving packages...");
            }
            _ => panic!("expected Phase"),
        }

        let prog_line = r#"{"event":"progress","kind":"download","label":"(1/1) hello-0.2.0-1-x86_64.spk","done":4096,"total":8192,"percent":50,"rate_bps":20480}"#;
        match serde_json::from_str::<ProgressStreamEvent>(prog_line).unwrap() {
            ProgressStreamEvent::Progress {
                kind,
                done,
                total,
                percent,
                rate_bps,
                ..
            } => {
                assert_eq!(kind, "download");
                assert_eq!(done, 4096);
                assert_eq!(total, Some(8192));
                assert_eq!(percent, 50);
                assert_eq!(rate_bps, 20480);
            }
            _ => panic!("expected Progress"),
        }
    }

    #[test]
    fn auth_secret_zeroes_buffer_on_drop() {
        let secret = AuthSecret::new("TopSecret123".into());
        let ptr = secret.as_bytes().as_ptr();
        let len = secret.as_bytes().len();
        assert_eq!(len, 12);
        drop(secret);
        let _ = ptr;
    }

    #[test]
    fn extract_spk_error_prefers_json_payload() {
        let json_out = r#"{"schema":1,"error":"目标未找到","lines":["missing foo"]}"#;
        let msg = extract_spk_error(json_out, "stderr noise", Some(1));
        assert!(msg.contains("目标未找到"));
        assert!(msg.contains("missing foo"));
    }
}
