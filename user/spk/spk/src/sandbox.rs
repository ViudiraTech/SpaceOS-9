//
//       user/spk/spk/src/sandbox.rs
//       Script sandbox launcher, staging verification and hook execution
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use spk_core::fsutil::{sha256_bytes, sha256_file};
use spk_core::manifest::Manifest;

use crate::error::{Result, SpkError};
use crate::i18n::{self, Lang};
use crate::state::Paths;

pub const SCRIPT_UID: u32 = 61111;
pub const SCRIPT_GID: u32 = 61111;
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);
const OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageEntry {
    pub kind: u8,
    pub sha256: String,
    pub size: u64,
}

fn stage_entry(path: &Path) -> Result<StageEntry> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        let bytes = target.to_string_lossy();
        Ok(StageEntry {
            kind: 2,
            sha256: sha256_bytes(bytes.as_bytes()),
            size: bytes.len() as u64,
        })
    } else if meta.is_dir() {
        Ok(StageEntry {
            kind: 1,
            sha256: String::new(),
            size: 0,
        })
    } else {
        Ok(StageEntry {
            kind: 0,
            sha256: sha256_file(path)?,
            size: meta.len(),
        })
    }
}

fn snapshot_walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, StageEntry>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| SpkError::General("staging path escaped its root".into()))?;
        let logical = format!("/{}", relative.to_string_lossy());
        out.insert(logical, stage_entry(&path)?);
        if entry.file_type()?.is_dir() {
            snapshot_walk(root, &path, out)?;
        }
    }
    Ok(())
}

pub fn snapshot_stage(stage: &Path) -> Result<BTreeMap<String, StageEntry>> {
    let mut out = BTreeMap::new();
    snapshot_walk(stage, stage, &mut out)?;
    Ok(out)
}

/// After a scriptlet, every path must still be declared by the manifest.
/// Scriptlets may rewrite declared files, but may not add undeclared files or
/// remove/change the type of declared ones.
pub fn verify_stage(
    manifest: &Manifest,
    before: &BTreeMap<String, StageEntry>,
    after: &BTreeMap<String, StageEntry>,
    lang: Lang,
) -> Result<()> {
    let declared: BTreeSet<&str> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    for (path, entry) in after {
        if declared.contains(path.as_str()) {
            continue;
        }
        if entry.kind == 1
            && declared
                .iter()
                .any(|declared| declared.starts_with(&format!("{path}/")))
        {
            continue;
        }
        return Err(SpkError::Script(i18n::staging_undeclared(lang, path)));
    }
    for path in &declared {
        let before_entry = before.get(*path);
        let after_entry = after.get(*path);
        let (Some(before_entry), Some(after_entry)) = (before_entry, after_entry) else {
            return Err(SpkError::Script(i18n::staging_removed(lang, path)));
        };
        if before_entry.kind != after_entry.kind {
            return Err(SpkError::Script(i18n::staging_type_changed(lang, path)));
        }
        if before_entry.kind == 2 && before_entry.sha256 != after_entry.sha256 {
            return Err(SpkError::Script(i18n::staging_symlink_changed(lang, path)));
        }
    }
    Ok(())
}

fn chown_tree(path: &Path, uid: u32, gid: u32) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    let text = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| SpkError::General("staging path contains NUL".into()))?;
    let rc = unsafe { libc::lchown(text.as_ptr(), uid, gid) };
    if rc != 0 {
        return Err(SpkError::General(format!(
            "lchown {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            chown_tree(&entry?.path(), uid, gid)?;
        }
    }
    Ok(())
}

fn script_dir(paths: &Paths, txid: &str, label: &str) -> Result<PathBuf> {
    let dir = paths.scripts.join(txid).join(label);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn write_runner(dir: &Path, install_script: Option<&str>) -> Result<()> {
    if let Some(script) = install_script {
        let path = dir.join(".INSTALL");
        fs::write(&path, script)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555))?;
    }
    let runner = dir.join("runner.sh");
    fs::write(
        &runner,
        "#!/bin/sh\n. \"${SPK_SCRIPT_INSTALL:-/spk-scripts/.INSTALL}\"\nfn=\"$1\"\nshift\nif command -v \"$fn\" >/dev/null 2>&1; then\n  \"$fn\" \"$@\"\nfi\n",
    )?;
    fs::set_permissions(&runner, fs::Permissions::from_mode(0o555))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555))?;
    Ok(())
}

fn helper_path() -> Result<PathBuf> {
    if let Ok(current) = std::env::current_exe() {
        let sibling = current.with_file_name("spk-sandbox");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    for candidate in ["/usr/lib/spk/spk-sandbox", "/usr/bin/spk-sandbox"] {
        if Path::new(candidate).is_file() {
            return Ok(PathBuf::from(candidate));
        }
    }
    Err(SpkError::General(
        "spk-sandbox helper is not installed; reinstall spk".into(),
    ))
}

struct CommandResult {
    code: i32,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

fn pump(
    mut reader: impl Read + Send + 'static,
    sink: Arc<std::sync::Mutex<Vec<u8>>>,
    total: Arc<AtomicUsize>,
    abort: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let used = total.fetch_add(count, Ordering::Relaxed) + count;
                    if used > OUTPUT_LIMIT {
                        abort.store(true, Ordering::Relaxed);
                        break;
                    }
                    if let Ok(mut sink) = sink.lock() {
                        sink.extend_from_slice(&buffer[..count]);
                    }
                }
            }
        }
    })
}

fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
    abort: &AtomicBool,
) -> Result<(i32, bool)> {
    let deadline = Instant::now() + timeout;
    loop {
        if abort.load(Ordering::Relaxed) {
            let _ = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.kill();
            let status = child.wait()?;
            return Ok((status.code().unwrap_or(1), true));
        }
        if let Some(status) = child.try_wait()? {
            return Ok((status.code().unwrap_or(1), false));
        }
        if Instant::now() >= deadline {
            let _ = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.kill();
            let status = child.wait()?;
            return Ok((status.code().unwrap_or(1), true));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_capture(mut command: Command, timeout: Duration) -> Result<CommandResult> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| SpkError::General(format!("cannot start sandbox: {error}")))?;
    let stdout = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr = Arc::new(std::sync::Mutex::new(Vec::new()));
    let total = Arc::new(AtomicUsize::new(0));
    let abort = Arc::new(AtomicBool::new(false));
    let out_handle = pump(
        child.stdout.take().expect("piped stdout"),
        stdout.clone(),
        total.clone(),
        abort.clone(),
    );
    let err_handle = pump(
        child.stderr.take().expect("piped stderr"),
        stderr.clone(),
        total.clone(),
        abort.clone(),
    );
    let (code, timed_out) = wait_with_timeout(&mut child, timeout, &abort)?;
    let _ = out_handle.join();
    let _ = err_handle.join();
    let stdout = String::from_utf8_lossy(&stdout.lock().unwrap()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.lock().unwrap()).into_owned();
    if total.load(Ordering::Relaxed) > OUTPUT_LIMIT {
        return Err(SpkError::Script(format!(
            "sandbox output exceeded {} bytes",
            OUTPUT_LIMIT
        )));
    }
    Ok(CommandResult {
        code,
        stdout,
        stderr,
        timed_out,
    })
}

fn sandbox_command(
    paths: &Paths,
    txid: &str,
    label: &str,
    stage: &Path,
    scripts: &Path,
    program: &str,
    args: &[String],
) -> Result<Command> {
    let helper = helper_path()?;
    let root = paths.sandbox.join(txid).join(label);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root)?;
    let mut command = Command::new(helper);
    command
        .arg("--root")
        .arg(&root)
        .arg("--stage")
        .arg(stage)
        .arg("--source")
        .arg(&paths.root)
        .arg("--scripts")
        .arg(scripts)
        .arg("--uid")
        .arg(SCRIPT_UID.to_string())
        .arg("--gid")
        .arg(SCRIPT_GID.to_string())
        .arg("--")
        .arg(program)
        .args(args);
    Ok(command)
}

fn print_script_output(label: &str, result: &CommandResult, lang: Lang) {
    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
        if !result.stdout.ends_with('\n') {
            println!();
        }
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
        if !result.stderr.ends_with('\n') {
            eprintln!();
        }
    }
    let _ = (label, lang);
}

fn check_result(label: &str, function: &str, result: CommandResult, lang: Lang) -> Result<()> {
    print_script_output(label, &result, lang);
    if result.timed_out {
        return Err(SpkError::Script(i18n::script_timeout(lang, function)));
    }
    if result.code != 0 {
        return Err(SpkError::Script(i18n::script_failed(
            lang,
            function,
            result.code,
        )));
    }
    Ok(())
}

/// Run an `.INSTALL` function against a staged package.
#[allow(clippy::too_many_arguments)]
pub fn run_install_script(
    paths: &Paths,
    txid: &str,
    package: &str,
    stage: &Path,
    script: &str,
    function: &str,
    args: &[String],
    lang: Lang,
    sandboxed: bool,
) -> Result<()> {
    chown_tree(stage, SCRIPT_UID, SCRIPT_GID)?;
    let dir = script_dir(paths, txid, &format!("{package}-{function}"))?;
    write_runner(&dir, Some(script))?;
    println!("{}", i18n::running_script(lang, function, package));
    let mut argv = vec![String::from("/spk-scripts/runner.sh"), function.to_string()];
    argv.extend(args.iter().cloned());
    let result = if sandboxed {
        let command = sandbox_command(
            paths,
            txid,
            &format!("{package}-{function}"),
            stage,
            &dir,
            "/bin/sh",
            &argv,
        )?;
        spawn_capture(command, SCRIPT_TIMEOUT)?
    } else {
        eprintln!("{}", i18n::sandbox_disabled(lang, package));
        let shell = if Path::new("/bin/busybox").is_file() {
            "/bin/busybox"
        } else {
            "/bin/sh"
        };
        // The runner expects the function name as its first argument.
        let mut fixed = Command::new(shell);
        if shell == "/bin/busybox" {
            fixed.arg("sh");
        }
        fixed
            .arg(dir.join("runner.sh"))
            .arg(function)
            .args(args)
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("HOME", "/tmp")
            .env("SPK_ROOT", stage)
            .env("SPK_SOURCE", &paths.root)
            .env("SPK_SANDBOX", "0")
            .env("SPK_SCRIPT_INSTALL", dir.join(".INSTALL"))
            .env("LANG", "C");
        spawn_capture_root(fixed, SCRIPT_TIMEOUT)?
    };
    check_result(package, function, result, lang)
}

/// Run a package-supplied hook command in the sandbox.
pub fn run_package_hook(
    paths: &Paths,
    txid: &str,
    package: &str,
    command_text: &str,
    stage: &Path,
    lang: Lang,
) -> Result<()> {
    chown_tree(stage, SCRIPT_UID, SCRIPT_GID)?;
    let dir = script_dir(paths, txid, &format!("{package}-hook"))?;
    write_runner(&dir, None)?;
    let argv = vec![String::from("-c"), command_text.to_string()];
    let command = sandbox_command(
        paths,
        txid,
        &format!("{package}-hook"),
        stage,
        &dir,
        "/bin/sh",
        &argv,
    )?;
    let result = spawn_capture(command, SCRIPT_TIMEOUT)?;
    check_result(package, "hook", result, lang)
}

/// Run a trusted system hook as root (timeout and process group still apply).
pub fn run_system_hook(command_text: &str, lang: Lang) -> Result<()> {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(command_text);
    command.env_clear();
    command.env(
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    );
    command.env("LANG", "C");
    command.env("SPK_SANDBOX", "0");
    let result = spawn_capture_root(command, SCRIPT_TIMEOUT)?;
    check_result("system-hook", "hook", result, lang)
}

fn spawn_capture_root(mut command: Command, timeout: Duration) -> Result<CommandResult> {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            let _ = libc::setsid();
            let _ = libc::setpgid(0, 0);
            Ok(())
        });
    }
    spawn_capture(command, timeout)
}

/// Run a package trigger script after commit; `/source` is the new real root.
#[allow(clippy::too_many_arguments)]
pub fn run_trigger(
    paths: &Paths,
    txid: &str,
    package: &str,
    version: &str,
    trigger: &str,
    action: &str,
    changed: &[String],
    lang: Lang,
) -> Result<()> {
    let empty = paths
        .sandbox
        .join(txid)
        .join(format!("{package}-trigger-stage"));
    let _ = fs::remove_dir_all(&empty);
    fs::create_dir_all(&empty)?;
    let dir = script_dir(paths, txid, &format!("{package}-trigger"))?;
    write_runner(&dir, None)?;
    let mut argv = vec![
        format!("/source/usr/lib/spk/triggers/{package}.trigger"),
        action.to_string(),
    ];
    argv.extend(changed.iter().cloned());
    println!("{}", i18n::trigger_executing(lang, package, version));
    let command = sandbox_command(
        paths,
        txid,
        &format!("{package}-trigger"),
        &empty,
        &dir,
        "/bin/sh",
        &argv,
    )?;
    let result = spawn_capture(command, SCRIPT_TIMEOUT)?;
    if result.timed_out {
        return Err(SpkError::Script(i18n::script_timeout(lang, trigger)));
    }
    if result.code != 0 {
        return Err(SpkError::Script(i18n::script_failed(
            lang,
            trigger,
            result.code,
        )));
    }
    print_script_output(package, &result, lang);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spk_core::manifest::Manifest;

    fn manifest(script_text: &str) -> Manifest {
        Manifest::from_toml(&format!(
            r#"name = "t"
epoch = 0
version = "1.0"
release = 1
arch = "x86_64"
abi = "spaceos-1"
summary = "test"
license = "MIT"
scripts = true
[[files]]
path = "/usr/share/t"
mode = "0644"
sha256 = "{}"
size = {}
"#,
            sha256_bytes(script_text.as_bytes()),
            script_text.len()
        ))
        .unwrap()
    }

    #[test]
    fn staging_rejects_undeclared_and_removed_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("usr/share/t");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"base").unwrap();
        let manifest = manifest("base");
        let before = snapshot_stage(dir.path()).unwrap();
        fs::write(&file, b"modified").unwrap();
        let after = snapshot_stage(dir.path()).unwrap();
        verify_stage(&manifest, &before, &after, crate::i18n::Lang::En).unwrap();

        fs::write(dir.path().join("usr/share/extra"), b"extra").unwrap();
        let after = snapshot_stage(dir.path()).unwrap();
        assert!(verify_stage(&manifest, &before, &after, crate::i18n::Lang::En).is_err());

        fs::remove_file(dir.path().join("usr/share/extra")).unwrap();
        fs::remove_file(&file).unwrap();
        let after = snapshot_stage(dir.path()).unwrap();
        assert!(verify_stage(&manifest, &before, &after, crate::i18n::Lang::En).is_err());
    }
}
