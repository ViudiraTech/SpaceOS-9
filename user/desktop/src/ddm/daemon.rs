/*
 *
 *       user/desktop/src/ddm/daemon.rs
 *       DDM privileged daemon: X server, greeter socket, auth and session lifecycle
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The daemon is the only DDM component that runs as root.  It starts Xorg,
//! creates per-client Xauthority cookies, validates shadow passwords, and
//! forks sessions after dropping every supplementary group.  The greeter and
//! OOBE wizard connect over `/run/ddm/greeter.sock`; `SO_PEERCRED` limits that
//! socket to the dedicated `ddm` account.

use crate::apps::power_splash::SHUTDOWN_LOG_PATH;
use crate::ddm::accounts::{self, AccountSpec, SystemSettings};
use crate::ddm::auth::{self, AuthError, UserRecord};
use crate::ddm::config::{Config, DEFAULT_CONFIG};
use crate::ddm::sessions::{self, Session};
use crate::ddm::wire::{self, ClientKind, ErrorCode, Opcode, PowerAction, Reader, Writer};
use crate::ddm::xauth::{self, AuthEntry, COOKIE_LEN};
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::raw::c_int;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SOCKET_NAME: &str = "greeter.sock";
const POWER_SOCKET_NAME: &str = "power.sock";
const XORG_PID: &str = "xorg.pid";
const GREETER_PID: &str = "greeter.pid";
const SESSION_PID: &str = "session.pid";
const XAUTH: &str = "Xauthority";
const GREETER_XAUTH: &str = "greeter.Xauthority";
const MAX_USERNAME: usize = 64;
const MAX_SESSION_ID: usize = 128;
const MAX_PASSWORD: usize = 1024;
const MAX_FRAME_BUFFER: usize = 256 * 1024;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static RELOAD: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Greeter,
    Oobe,
}

impl Mode {
    fn client_kind(self) -> ClientKind {
        match self {
            Self::Greeter => ClientKind::Greeter,
            Self::Oobe => ClientKind::Oobe,
        }
    }

    fn as_u8(self) -> u8 {
        self.client_kind() as u8
    }
}

#[derive(Clone, Debug, Default)]
struct State {
    last_user: String,
    last_session: String,
    auto_login: bool,
    auto_login_user: String,
}

struct FailureState {
    count: u32,
    locked_until: Option<Instant>,
}

struct Client {
    fd: RawFd,
    buffer: Vec<u8>,
}

struct SessionProcess {
    pid: i32,
    uid: u32,
    user: String,
    id: String,
    auth_file: PathBuf,
    cookie: [u8; COOKIE_LEN],
}

pub struct Daemon {
    config: Config,
    ddm: UserRecord,
    state: State,
    failures: HashMap<String, FailureState>,
    dummy_hash: String,
    listener: Option<std::os::unix::net::UnixListener>,
    power_listener: Option<std::os::unix::net::UnixListener>,
    client: Option<Client>,
    xorg_pid: Option<i32>,
    xorg_started: Option<Instant>,
    greeter_pid: Option<i32>,
    greeter_cookie: Option<[u8; COOKIE_LEN]>,
    power_splash_pid: Option<i32>,
    power_in_progress: bool,
    session: Option<SessionProcess>,
    mode: Mode,
    sessions: Vec<Session>,
    restart_client_at: Option<Instant>,
    restart_xorg_at: Option<Instant>,
    shutting_down: bool,
}

pub fn run(arguments: &[String]) -> Result<(), String> {
    let mut config_path = PathBuf::from(DEFAULT_CONFIG);
    let mut check = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--check" => check = true,
            "--config" => {
                index += 1;
                let Some(value) = arguments.get(index) else {
                    return Err("--config requires a path".to_string());
                };
                config_path = PathBuf::from(value);
            }
            "--help" | "-h" => {
                println!(
                    "ddm: SpaceOS Display Manager daemon\n\
                     usage: spui ddm [--check] [--config PATH]\n\
                     \n\
                     --check   validate configuration, accounts and session files\n\
                     --config  use an alternate ddm.conf"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
        index += 1;
    }

    let config = Config::load(&config_path)?;
    let sessions = sessions::discover(&config.session_dir);
    let users = auth::human_users(config.minimum_uid);
    let oobe = needs_oobe(&config, &users)?;

    if check {
        println!("DDM configuration OK");
        println!("config: {}", config_path.display());
        println!("display: {}", config.display);
        println!("xserver: {}", config.xserver);
        println!("greeter user: {}", config.greeter_user);
        println!("sessions: {}", sessions.len());
        for session in &sessions {
            println!("  {} -> {} ({})", session.id, session.name, session.exec);
        }
        println!("human users: {}", users.len());
        for user in &users {
            println!("  {} (uid {})", user.name, user.uid);
        }
        println!("oobe needed: {}", if oobe { "yes" } else { "no" });
        return Ok(());
    }

    if unsafe { libc::geteuid() } != 0 {
        return Err("ddm must run as root".to_string());
    }
    if !Path::new(&config.greeter_program).is_file() {
        return Err(format!(
            "greeter program not found: {}",
            config.greeter_program
        ));
    }
    if !Path::new(&config.oobe_program).is_file() {
        return Err(format!("OOBE program not found: {}", config.oobe_program));
    }
    if !Path::new(&config.xserver).is_file() {
        return Err(format!("X server not found: {}", config.xserver));
    }

    let mut daemon = Daemon::new(config)?;
    daemon.cleanup_stale();
    daemon.setup_runtime()?;
    daemon.run_loop()
}

fn needs_oobe(config: &Config, users: &[UserRecord]) -> Result<bool, String> {
    if config.oobe_marker.is_file() {
        return Ok(false);
    }
    if users.is_empty() {
        return Ok(true);
    }
    // A human account exists but the marker is missing.  This is an interrupted
    // OOBE, not a reason to expose an account-creation screen again.
    Ok(false)
}

impl Daemon {
    fn new(config: Config) -> Result<Self, String> {
        let ddm = auth::lookup_user(&config.greeter_user)
            .ok_or_else(|| format!("greeter account {} does not exist", config.greeter_user))?;
        if ddm.uid == 0 {
            return Err("GreeterUser must not be root".to_string());
        }
        let state = read_state(&config.state_dir);
        let dummy_hash = auth::hash_password(b"ddm-dummy-password-for-timing")
            .map_err(|error| format!("cannot initialise crypt: {error}"))?;
        Ok(Self {
            config,
            ddm,
            state,
            failures: HashMap::new(),
            dummy_hash,
            listener: None,
            power_listener: None,
            client: None,
            xorg_pid: None,
            xorg_started: None,
            greeter_pid: None,
            greeter_cookie: None,
            power_splash_pid: None,
            power_in_progress: false,
            session: None,
            mode: Mode::Greeter,
            sessions: Vec::new(),
            restart_client_at: None,
            restart_xorg_at: None,
            shutting_down: false,
        })
    }

    fn bind_listener(
        socket: &Path,
        mode: u32,
        owner: Option<(u32, u32)>,
    ) -> Result<std::os::unix::net::UnixListener, String> {
        let _ = fs::remove_file(socket);
        let listener = std::os::unix::net::UnixListener::bind(socket)
            .map_err(|error| format!("bind {}: {error}", socket.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        xauth::set_mode(socket, mode).map_err(|error| error.to_string())?;
        if let Some((uid, gid)) = owner {
            xauth::chown(socket, uid, gid).map_err(|error| error.to_string())?;
        }
        Ok(listener)
    }

    fn setup_runtime(&mut self) -> Result<(), String> {
        create_dir(&self.config.runtime_dir, 0o711, 0, 0)?;
        create_dir(&self.config.state_dir, 0o700, 0, 0)?;
        create_dir(&self.config.log_dir, 0o755, 0, 0)?;
        create_dir(
            &self.runtime().join("greeter-home"),
            0o700,
            self.ddm.uid,
            self.ddm.gid,
        )?;
        create_dir(
            &self.runtime().join("greeter-runtime"),
            0o700,
            self.ddm.uid,
            self.ddm.gid,
        )?;
        if let Some(parent) = self.config.oobe_marker.parent() {
            create_dir(parent, 0o755, 0, 0)?;
        }
        if let Some(parent) = self.config.xorg_log.parent() {
            create_dir(parent, 0o755, 0, 0)?;
        }

        let socket = self.socket_path();
        self.listener = Some(Self::bind_listener(
            &socket,
            0o660,
            Some((0, self.ddm.gid)),
        )?);
        let power_socket = self.power_socket_path();
        self.power_listener = Some(Self::bind_listener(&power_socket, 0o666, None)?);
        Ok(())
    }

    fn run_loop(&mut self) -> Result<(), String> {
        install_signals();
        self.sessions = sessions::discover(&self.config.session_dir);
        self.apply_factory_reset();
        let users = auth::human_users(self.config.minimum_uid);
        self.mode = if needs_oobe(&self.config, &users)? {
            Mode::Oobe
        } else {
            if !self.config.oobe_marker.is_file() && !users.is_empty() {
                let _ = accounts::mark_oobe_complete("existing-account");
                self.log("OOBE marker was missing but human accounts exist; marked complete");
            }
            Mode::Greeter
        };

        self.start_xorg()?;
        if self.auto_login_target().is_some() && self.mode == Mode::Greeter {
            let user = self.auto_login_target().unwrap();
            let session = self.default_session();
            if let Err(error) = self.start_session(&user, &session) {
                self.log(&format!("auto-login failed: {error}"));
                self.start_client()?;
            }
        } else {
            self.start_client()?;
        }

        while !SHUTDOWN.load(Ordering::Relaxed) {
            if RELOAD.swap(false, Ordering::Relaxed) {
                if let Ok(config) = Config::load(Path::new(DEFAULT_CONFIG)) {
                    self.config = config;
                    self.sessions = sessions::discover(&self.config.session_dir);
                    self.log("configuration reloaded");
                }
            }
            self.reap_children();
            self.ensure_xorg();
            self.ensure_client();
            self.poll_once(100);
        }
        self.shutdown_children();
        Ok(())
    }

    fn apply_factory_reset(&mut self) {
        let request = Path::new("/var/lib/spaceos/factory-reset");
        let progress = Path::new("/var/lib/spaceos/factory-reset-in-progress");
        let requested = request.is_file()
            && fs::metadata(request)
                .map(|metadata| {
                    use std::os::unix::fs::MetadataExt;
                    metadata.uid() == 0
                })
                .unwrap_or(false);
        if !requested && !progress.is_file() {
            return;
        }
        self.log("factory reset requested; removing local accounts");
        match accounts::factory_reset() {
            Ok(users) => {
                self.audit(
                    "FACTORY_RESET",
                    "ddm",
                    &format!("removed={}", users.join(",")),
                );
                self.log("factory reset complete; next screen is OOBE");
                self.state = State::default();
                let _ = Command::new("/bin/systemctl")
                    .args(["restart", "uxconfd.service"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            Err(error) => {
                self.audit("FACTORY_RESET_FAIL", "ddm", &error);
                self.log(&format!("factory reset failed: {error}"));
            }
        }
    }

    fn runtime(&self) -> PathBuf {
        self.config.runtime_dir.clone()
    }

    fn socket_path(&self) -> PathBuf {
        self.runtime().join(SOCKET_NAME)
    }

    fn power_socket_path(&self) -> PathBuf {
        self.runtime().join(POWER_SOCKET_NAME)
    }

    fn auth_path(&self) -> PathBuf {
        self.runtime().join(XAUTH)
    }

    fn greeter_auth_path(&self) -> PathBuf {
        self.runtime().join(GREETER_XAUTH)
    }

    fn auto_login_target(&self) -> Option<String> {
        let user = if self.state.auto_login && !self.state.auto_login_user.is_empty() {
            self.state.auto_login_user.clone()
        } else if self.config.auto_login && !self.config.auto_login_user.is_empty() {
            self.config.auto_login_user.clone()
        } else {
            return None;
        };
        auth::lookup_user(&user)
            .filter(|record| record.is_login_shell() && record.uid >= self.config.minimum_uid)
            .map(|record| record.name)
    }

    fn default_session(&self) -> String {
        sessions::find(&self.sessions, &self.config.default_session)
            .or_else(|| self.sessions.first())
            .map(|session| session.id.clone())
            .unwrap_or_else(|| "spui".to_string())
    }

    fn start_xorg(&mut self) -> Result<(), String> {
        if self.xorg_pid.is_some() {
            return Ok(());
        }
        prepare_x_socket(&self.config)?;
        let cookie = xauth::random_cookie().map_err(|error| error.to_string())?;
        let entry = AuthEntry::wildcard_cookie(&cookie);
        self.greeter_cookie = Some(cookie);
        xauth::write_file(
            &self.auth_path(),
            std::slice::from_ref(&entry),
            0o600,
            Some((0, 0)),
        )
        .map_err(|error| format!("write Xauthority: {error}"))?;
        xauth::write_file(
            &self.greeter_auth_path(),
            std::slice::from_ref(&entry),
            0o600,
            Some((self.ddm.uid, self.ddm.gid)),
        )
        .map_err(|error| format!("write greeter Xauthority: {error}"))?;

        let log = open_log(&self.config.log_dir.join("xorg-stdio.log"), 0o600, None)?;
        let mut command = Command::new(&self.config.xserver);
        command.arg(&self.config.display);
        for argument in &self.config.xserver_args {
            command.arg(argument);
        }
        command
            .arg("-auth")
            .arg(self.auth_path())
            .arg("-logfile")
            .arg(&self.config.xorg_log)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", "/root")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(log));
        // Xorg performs its own setsid/setpgid and VT setup.  Detaching it in
        // pre_exec makes those calls fail with EPERM, after which the server
        // closes the console and exits during startup.
        let child = command
            .spawn()
            .map_err(|error| format!("start {}: {error}", self.config.xserver))?;
        let pid = child.id() as i32;
        self.xorg_pid = Some(pid);
        self.xorg_started = Some(Instant::now());
        write_pid(&self.runtime().join(XORG_PID), pid);
        self.log(&format!(
            "Xorg started as pid {pid} on {}",
            self.config.display
        ));

        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            // A Unix-domain socket is not a regular file; is_file() would never
            // become true and the daemon would tear down a healthy X server.
            if self.config.x_socket().exists() {
                return Ok(());
            }
            if !pid_alive(pid) {
                let mut status = 0;
                let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                self.xorg_pid = None;
                if reaped == pid {
                    self.log(&format!(
                        "Xorg pid {pid} exited before creating its socket: {}",
                        exit_description(status)
                    ));
                }
                return Err("Xorg exited before creating its socket".to_string());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        terminate_group(pid, Duration::from_secs(2));
        self.xorg_pid = None;
        Err("timed out waiting for the X11 socket".to_string())
    }

    fn ensure_xorg(&mut self) {
        if self.power_in_progress || self.xorg_pid.is_some() {
            return;
        }
        if let Some(deadline) = self.restart_xorg_at {
            if Instant::now() < deadline {
                return;
            }
        }
        self.stop_client();
        self.stop_session();
        if let Err(error) = self.start_xorg() {
            self.restart_xorg_at = Some(Instant::now() + Duration::from_secs(2));
            self.log(&format!("cannot restart Xorg: {error}"));
        } else {
            self.restart_xorg_at = None;
            if let Err(error) = self.start_client() {
                self.log(&format!("cannot restart greeter: {error}"));
            }
        }
    }

    fn start_client(&mut self) -> Result<(), String> {
        if self.client.is_some()
            || self.session.is_some()
            || self.greeter_pid.is_some()
            || self.xorg_pid.is_none()
        {
            return Ok(());
        }
        let (program, arguments) = match self.mode {
            Mode::Greeter => (&self.config.greeter_program, &self.config.greeter_args),
            Mode::Oobe => (&self.config.oobe_program, &self.config.oobe_args),
        };
        let mut argv = vec![program.clone()];
        argv.extend(arguments.iter().cloned());
        argv.push("--socket".to_string());
        argv.push(self.socket_path().to_string_lossy().into_owned());

        let log = open_log(&self.config.log_dir.join("greeter.log"), 0o600, None)?;
        let env = self.greeter_x11_env();
        let pid = spawn_as(
            &self.ddm,
            &argv,
            &env,
            Path::new("/"),
            log,
            Some(CString::new(self.ddm.name.as_str()).map_err(|_| "invalid greeter name")?),
        )?;
        self.greeter_pid = Some(pid);
        write_pid(&self.runtime().join(GREETER_PID), pid);
        self.log(&format!(
            "{} started as pid {} (uid {})",
            match self.mode {
                Mode::Greeter => "greeter",
                Mode::Oobe => "OOBE",
            },
            pid,
            self.ddm.uid
        ));
        Ok(())
    }

    fn greeter_x11_env(&self) -> Vec<(String, String)> {
        let mut env = self.base_greeter_env();
        env.push(("DISPLAY".to_string(), self.config.display.clone()));
        env.push((
            "XAUTHORITY".to_string(),
            self.greeter_auth_path().to_string_lossy().into_owned(),
        ));
        env.push((
            "HOME".to_string(),
            self.runtime()
                .join("greeter-home")
                .to_string_lossy()
                .into_owned(),
        ));
        env.push((
            "XDG_RUNTIME_DIR".to_string(),
            self.runtime()
                .join("greeter-runtime")
                .to_string_lossy()
                .into_owned(),
        ));
        env.push(("USER".to_string(), self.ddm.name.clone()));
        env.push(("LOGNAME".to_string(), self.ddm.name.clone()));
        env.push(("SHELL".to_string(), "/bin/nologin".to_string()));
        env
    }

    fn spawn_power_splash(&mut self, action: PowerAction) -> Result<(), String> {
        let mode = match action {
            PowerAction::Reboot => "reboot",
            PowerAction::PowerOff => "poweroff",
        };
        let argv = vec![
            self.config.greeter_program.clone(),
            "power-splash".to_string(),
            mode.to_string(),
        ];
        let log = open_log(&self.config.log_dir.join("power-splash.log"), 0o600, None)?;
        let env = self.greeter_x11_env();
        let pid = spawn_as(
            &self.ddm,
            &argv,
            &env,
            Path::new("/"),
            log,
            Some(CString::new(self.ddm.name.as_str()).map_err(|_| "invalid greeter name")?),
        )?;
        self.power_splash_pid = Some(pid);
        self.log(&format!("power splash ({mode}) started as pid {pid}"));
        Ok(())
    }

    fn base_greeter_env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            (
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
            ),
            ("LANG".to_string(), "zh_CN.UTF-8".to_string()),
            ("LC_ALL".to_string(), "zh_CN.UTF-8".to_string()),
        ];
        if let Ok(tz) = fs::read_to_string("/etc/TZ") {
            env.push(("TZ".to_string(), tz.trim().to_string()));
        }
        env
    }

    /// Rewrite the root-only server auth file from the daemon's own state.
    /// Every cookie uses the MIT-MAGIC-COOKIE-1 name, so entry identity is by
    /// position and ownership, never by matching a cookie name.
    fn rewrite_server_auth(&self) -> Result<(), String> {
        let mut entries = Vec::new();
        if let Some(cookie) = self.greeter_cookie {
            entries.push(AuthEntry::wildcard_cookie(&cookie));
        }
        if let Some(session) = self.session.as_ref() {
            entries.push(AuthEntry::wildcard_cookie(&session.cookie));
        }
        xauth::write_file(&self.auth_path(), &entries, 0o600, Some((0, 0)))
            .map_err(|error| format!("write server Xauthority: {error}"))
    }

    fn start_session(&mut self, username: &str, session_id: &str) -> Result<(), String> {
        if self.session.is_some() {
            return Err("a session is already active".to_string());
        }
        let user = auth::lookup_user(username).ok_or("account disappeared")?;
        if user.uid < self.config.minimum_uid && !(user.uid == 0 && self.config.allow_root) {
            return Err("account is outside the graphical login policy".to_string());
        }
        let session = sessions::find(&self.sessions, session_id)
            .or_else(|| sessions::find(&self.sessions, &self.config.default_session))
            .or_else(|| self.sessions.first())
            .cloned()
            .ok_or("no XDG session is installed")?;
        if !user.is_login_shell() {
            return Err("account has no login shell".to_string());
        }

        // The X server is restarted with a fresh cookie on logout.  Within one
        // authenticated session we reuse the cookie that the running server
        // already accepted for the greeter; the user's private copy is still
        // 0600 and never visible to other accounts.
        let cookie = self
            .greeter_cookie
            .ok_or("no active X authorization cookie")?;
        let entry = AuthEntry::wildcard_cookie(&cookie);
        let auth_file = PathBuf::from(&user.home).join(".Xauthority");
        if let Err(error) = xauth::write_file(
            &auth_file,
            std::slice::from_ref(&entry),
            0o600,
            Some((user.uid, user.gid)),
        ) {
            return Err(format!("write user Xauthority: {error}"));
        }

        // Keep exactly one cookie in the server file.  It is the same cookie
        // the greeter used, so the already-running server accepts the session.
        let server_entries = vec![AuthEntry::wildcard_cookie(&cookie)];
        if let Err(error) =
            xauth::write_file(&self.auth_path(), &server_entries, 0o600, Some((0, 0)))
        {
            let _ = fs::remove_file(&auth_file);
            return Err(format!("write server Xauthority: {error}"));
        }
        let runtime = PathBuf::from(format!("/run/user/{}", user.uid));
        if let Err(error) = create_dir(&runtime, 0o700, user.uid, user.gid) {
            let _ = fs::remove_file(&auth_file);
            let _ = self.rewrite_server_auth();
            return Err(error);
        }
        let log_path = self
            .config
            .log_dir
            .join(format!("session-{}.log", user.name));
        let log = match open_log(&log_path, 0o600, Some((user.uid, user.gid))) {
            Ok(log) => log,
            Err(error) => {
                let _ = fs::remove_file(&auth_file);
                let _ = self.rewrite_server_auth();
                return Err(error);
            }
        };
        let locale = read_locale();
        let tz = read_tz();
        let desktop_names = if session.desktop_names.is_empty() {
            "SPUI".to_string()
        } else {
            session.desktop_names.clone()
        };
        let mut env = vec![
            ("HOME".to_string(), user.home.clone()),
            ("USER".to_string(), user.name.clone()),
            ("LOGNAME".to_string(), user.name.clone()),
            ("SHELL".to_string(), user.shell.clone()),
            (
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
            ),
            ("DISPLAY".to_string(), self.config.display.clone()),
            (
                "XAUTHORITY".to_string(),
                auth_file.to_string_lossy().into_owned(),
            ),
            ("XDG_SESSION_TYPE".to_string(), "x11".to_string()),
            ("XDG_SESSION_CLASS".to_string(), "user".to_string()),
            (
                "XDG_SESSION_ID".to_string(),
                format!("ddm-{}-{}", user.uid, std::process::id()),
            ),
            (
                "XDG_RUNTIME_DIR".to_string(),
                runtime.to_string_lossy().into_owned(),
            ),
            (
                "XDG_DATA_DIRS".to_string(),
                "/usr/local/share:/usr/share".to_string(),
            ),
            (
                "XDG_CONFIG_DIRS".to_string(),
                "/etc/xdg:/usr/share/xdg".to_string(),
            ),
            ("XDG_CURRENT_DESKTOP".to_string(), desktop_names),
            ("DESKTOP_SESSION".to_string(), session.id.clone()),
            ("LANG".to_string(), locale),
            ("XDG_SEAT".to_string(), "seat0".to_string()),
            ("XDG_VTNR".to_string(), "1".to_string()),
            ("MAIL".to_string(), format!("/var/mail/{}", user.name)),
        ];
        if let Some(tz) = tz {
            env.push(("TZ".to_string(), tz));
        }
        let mut argv = vec!["/bin/sh".to_string()];
        argv.push("-c".to_string());
        argv.push(session.exec.clone());
        let name = CString::new(user.name.as_str()).map_err(|_| "invalid account name")?;
        let pid = match spawn_as(&user, &argv, &env, Path::new(&user.home), log, Some(name)) {
            Ok(pid) => pid,
            Err(error) => {
                let _ = fs::remove_file(&auth_file);
                let _ = self.rewrite_server_auth();
                return Err(error);
            }
        };
        self.session = Some(SessionProcess {
            pid,
            uid: user.uid,
            user: user.name.clone(),
            id: session.id.clone(),
            auth_file,
            cookie,
        });
        write_pid(&self.runtime().join(SESSION_PID), pid);
        self.state.last_user = user.name.clone();
        self.state.last_session = session.id.clone();
        write_state(&self.config.state_dir, &self.state);
        self.audit(
            "SESSION_START",
            &user.name,
            &format!("uid={} session={}", user.uid, session.id),
        );
        self.log(&format!(
            "session {} for {} started as pid {}",
            session.id, user.name, pid
        ));
        Ok(())
    }

    fn stop_session_keep_xorg(&mut self) {
        if let Some(session) = self.session.take() {
            terminate_group(session.pid, Duration::from_secs(3));
            let _ = xauth::remove_if_present(&session.auth_file);
            let _ = fs::remove_file(self.runtime().join(SESSION_PID));
            self.audit(
                "SESSION_STOP",
                &session.user,
                &format!("session={}", session.id),
            );
            self.log(&format!(
                "session {} for {} stopped",
                session.id, session.user
            ));
        }
    }

    fn stop_session(&mut self) {
        if self.session.is_some() {
            self.stop_session_keep_xorg();
            // Rotate the X cookie after every session.  Xorg is restarted with
            // a fresh server auth file before the next greeter is shown.
            self.stop_xorg();
        }
    }

    fn stop_xorg(&mut self) {
        if let Some(pid) = self.xorg_pid.take() {
            terminate_group(pid, Duration::from_secs(3));
            let _ = fs::remove_file(self.runtime().join(XORG_PID));
            self.log(&format!("Xorg pid {pid} stopped"));
        }
        let _ = fs::remove_file(self.config.x_socket());
        let _ = fs::remove_file(self.config.x_lock());
        self.xorg_started = None;
        self.greeter_cookie = None;
        let _ = fs::remove_file(self.auth_path());
    }

    fn stop_client(&mut self) {
        if let Some(client) = self.client.take() {
            unsafe {
                libc::close(client.fd);
            }
        }
        if let Some(pid) = self.greeter_pid.take() {
            terminate_group(pid, Duration::from_secs(2));
            let _ = fs::remove_file(self.runtime().join(GREETER_PID));
            self.log(&format!("greeter pid {pid} stopped"));
        }
    }

    fn stop_power_splash(&mut self) {
        if let Some(pid) = self.power_splash_pid.take() {
            terminate_group(pid, Duration::from_millis(800));
            self.log(&format!("power splash pid {pid} stopped"));
        }
    }

    fn shutdown_children(&mut self) {
        self.shutting_down = true;
        self.power_listener = None;
        self.stop_power_splash();
        self.stop_session();
        self.stop_client();
        if let Some(pid) = self.xorg_pid.take() {
            terminate_group(pid, Duration::from_secs(3));
            let _ = fs::remove_file(self.runtime().join(XORG_PID));
        }
        for name in [
            XAUTH,
            GREETER_XAUTH,
            SOCKET_NAME,
            POWER_SOCKET_NAME,
            XORG_PID,
            GREETER_PID,
            SESSION_PID,
        ] {
            let _ = fs::remove_file(self.runtime().join(name));
        }
        let _ = fs::remove_file(self.config.x_lock());
    }

    fn ensure_client(&mut self) {
        if self.power_in_progress
            || self.session.is_some()
            || self.client.is_some()
            || self.greeter_pid.is_some()
            || self.xorg_pid.is_none()
        {
            return;
        }
        if let Some(deadline) = self.restart_client_at {
            if Instant::now() < deadline {
                return;
            }
        }
        self.restart_client_at = Some(Instant::now() + Duration::from_millis(500));
        if let Err(error) = self.start_client() {
            self.log(&format!("cannot start greeter: {error}"));
        }
    }

    fn reap_children(&mut self) {
        loop {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid <= 0 {
                break;
            }
            let pid = pid as i32;
            let code = exit_description(status);
            if Some(pid) == self.xorg_pid {
                self.log(&format!("Xorg pid {pid} exited: {code}"));
                self.xorg_pid = None;
                self.stop_client();
                self.stop_session();
                if !self.power_in_progress {
                    self.restart_xorg_at = Some(Instant::now() + Duration::from_secs(2));
                }
            } else if Some(pid) == self.power_splash_pid {
                self.power_splash_pid = None;
                self.log(&format!("power splash pid {pid} exited: {code}"));
            } else if Some(pid) == self.greeter_pid {
                self.greeter_pid = None;
                let _ = fs::remove_file(self.runtime().join(GREETER_PID));
                self.log(&format!("greeter pid {pid} exited: {code}"));
                if self.session.is_none() && !self.power_in_progress {
                    self.restart_client_at = Some(Instant::now() + Duration::from_millis(200));
                }
            } else if self
                .session
                .as_ref()
                .is_some_and(|session| session.pid == pid)
            {
                let user = self
                    .session
                    .as_ref()
                    .map(|session| session.user.clone())
                    .unwrap_or_default();
                self.log(&format!("session pid {pid} ({user}) exited: {code}"));
                if self.power_in_progress {
                    self.stop_session_keep_xorg();
                } else {
                    self.stop_session();
                    self.mode = Mode::Greeter;
                    self.restart_client_at = None;
                }
            }
        }
    }

    fn poll_once(&mut self, timeout_ms: i32) {
        let listener_fd = self.listener.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(-1);
        let client_fd = self.client.as_ref().map_or(-1, |client| client.fd);
        let power_fd = self
            .power_listener
            .as_ref()
            .map(AsRawFd::as_raw_fd)
            .unwrap_or(-1);
        let mut fds = [
            libc::pollfd {
                fd: listener_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: client_fd,
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
            libc::pollfd {
                fd: power_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                self.log(&format!("poll failed: {error}"));
            }
            return;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            self.accept_client();
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            self.read_client();
        }
        if fds[2].revents & libc::POLLIN != 0 {
            self.accept_power_client();
        }
    }

    fn accept_power_client(&mut self) {
        let Some(listener) = self.power_listener.as_ref() else {
            return;
        };
        let (stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(error) => {
                self.log(&format!("power socket accept failed: {error}"));
                return;
            }
        };
        let fd = stream.into_raw_fd();
        let uid = match peer_uid(fd) {
            Ok(uid) => uid,
            Err(error) => {
                self.log(&format!("power SO_PEERCRED failed: {error}"));
                unsafe {
                    libc::close(fd);
                }
                return;
            }
        };
        let authorized = uid == 0 || self.session.as_ref().is_some_and(|s| s.uid == uid);
        if !authorized {
            self.log(&format!(
                "rejected unauthorized power connection from uid {uid}"
            ));
            let mut writer = Writer::new(Opcode::Error);
            writer
                .u16(ErrorCode::BadRequest as u16)
                .string("无权执行电源操作")
                .u32(0);
            write_fd_frame(fd, &writer.finish(), |msg| self.log(msg));
            unsafe {
                libc::close(fd);
            }
            return;
        }

        let action = match read_single_power_action(fd) {
            Ok(action) => action,
            Err(error) => {
                let mut writer = Writer::new(Opcode::Error);
                writer.u16(ErrorCode::Protocol as u16).string(&error).u32(0);
                write_fd_frame(fd, &writer.finish(), |msg| self.log(msg));
                unsafe {
                    libc::close(fd);
                }
                return;
            }
        };

        if !self.config.allow_power {
            let mut writer = Writer::new(Opcode::Error);
            writer
                .u16(ErrorCode::BadRequest as u16)
                .string("系统策略禁止电源操作")
                .u32(0);
            write_fd_frame(fd, &writer.finish(), |msg| self.log(msg));
            unsafe {
                libc::close(fd);
            }
            return;
        }

        write_fd_frame(fd, &Writer::new(Opcode::PowerOk).finish(), |msg| {
            self.log(msg)
        });
        unsafe {
            libc::close(fd);
        }
        self.initiate_power(action, &format!("uid:{uid}"));
    }

    fn accept_client(&mut self) {
        let Some(listener) = self.listener.as_ref() else {
            return;
        };
        let (stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(error) => {
                self.log(&format!("accept failed: {error}"));
                return;
            }
        };
        let fd = stream.into_raw_fd();
        let uid = match peer_uid(fd) {
            Ok(uid) => uid,
            Err(error) => {
                self.log(&format!("SO_PEERCRED failed: {error}"));
                unsafe {
                    libc::close(fd);
                }
                return;
            }
        };
        if uid != self.ddm.uid {
            self.log(&format!(
                "rejected greeter connection from uid {uid}, expected {}",
                self.ddm.uid
            ));
            unsafe {
                libc::close(fd);
            }
            return;
        }
        if self.client.is_some() {
            self.log("replacing an existing greeter connection");
            self.stop_client();
        }
        let client = Client {
            fd,
            buffer: Vec::new(),
        };
        self.client = Some(client);
        self.log("greeter connected");
        if let Err(error) = self.send_server_config() {
            self.log(&format!("cannot send server config: {error}"));
            self.stop_client();
        }
    }

    fn read_client(&mut self) {
        enum Outcome {
            Disconnected,
            Failed(String),
            Frames(Vec<Vec<u8>>),
        }
        let outcome = {
            let Some(client) = self.client.as_mut() else {
                return;
            };
            let mut chunk = [0u8; 8192];
            let count = unsafe {
                libc::recv(
                    client.fd,
                    chunk.as_mut_ptr() as *mut libc::c_void,
                    chunk.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if count == 0 {
                Outcome::Disconnected
            } else if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    return;
                }
                Outcome::Failed(format!("greeter read failed: {error}"))
            } else {
                client.buffer.extend_from_slice(&chunk[..count as usize]);
                if client.buffer.len() > MAX_FRAME_BUFFER {
                    Outcome::Failed("greeter frame buffer overflow".to_string())
                } else {
                    let mut frames = Vec::new();
                    let mut failure = None;
                    loop {
                        match wire::parse_frame(&mut client.buffer) {
                            Ok(Some(payload)) => frames.push(payload),
                            Ok(None) => break,
                            Err(error) => {
                                failure = Some(format!("malformed frame: {error}"));
                                break;
                            }
                        }
                    }
                    match failure {
                        Some(error) => Outcome::Failed(error),
                        None => Outcome::Frames(frames),
                    }
                }
            }
        };
        match outcome {
            Outcome::Disconnected => {
                self.log("greeter disconnected");
                self.stop_client();
                self.restart_client_at = Some(Instant::now() + Duration::from_millis(300));
            }
            Outcome::Failed(error) => self.fail_client(error),
            Outcome::Frames(frames) => {
                for payload in frames {
                    if let Err(error) = self.handle_frame(&payload) {
                        self.log(&format!("protocol error: {error}"));
                        let _ = self.send_error(ErrorCode::Protocol, &error, 0);
                    }
                }
            }
        }
    }

    fn fail_client(&mut self, error: String) {
        self.log(&error);
        self.stop_client();
        self.restart_client_at = Some(Instant::now() + Duration::from_millis(300));
    }

    fn handle_frame(&mut self, payload: &[u8]) -> Result<(), String> {
        let mut reader = Reader::new(payload);
        let opcode = reader.opcode()?;
        match opcode {
            Opcode::Hello => {
                let version = reader.u16()?;
                let kind = ClientKind::from_u8(reader.u8()?)
                    .ok_or_else(|| "unknown client kind".to_string())?;
                reader.finish()?;
                if version != wire::PROTOCOL_VERSION {
                    let _ = self.send_error(ErrorCode::Protocol, "协议版本不匹配", 0);
                    return Err(format!("protocol version {version}"));
                }
                if kind != self.mode.client_kind() {
                    let _ = self.send_error(ErrorCode::BadRequest, "客户端模式不匹配", 0);
                    return Err("client kind does not match daemon mode".to_string());
                }
                self.send_server_config()?;
            }
            Opcode::Login => {
                if self.mode != Mode::Greeter {
                    return self.err(ErrorCode::OobeDisabled, "当前处于首次设置模式");
                }
                let username = reader.string()?;
                let mut password = reader.field()?.to_vec();
                let session_id = reader.string()?;
                reader.finish()?;
                self.handle_login(&username, &password, &session_id);
                wipe(&mut password);
            }
            Opcode::OobeCommit => {
                if self.mode != Mode::Oobe {
                    return self.err(ErrorCode::OobeDisabled, "首次设置已完成");
                }
                let username = reader.string()?;
                let full_name = reader.string()?;
                let mut password = reader.field()?.to_vec();
                let timezone = reader.string()?;
                let locale = reader.string()?;
                let keyboard = reader.string()?;
                let hostname = reader.string()?;
                let auto_login = reader.bool()?;
                reader.finish()?;
                self.handle_oobe(
                    &username, &full_name, &password, &timezone, &locale, &keyboard, &hostname,
                    auto_login,
                );
                wipe(&mut password);
            }
            Opcode::Power => {
                let action = PowerAction::from_u8(reader.u8()?)
                    .ok_or_else(|| "unknown power action".to_string())?;
                reader.finish()?;
                self.handle_power(action);
            }
            Opcode::Ping => {
                reader.finish()?;
                self.send(&Writer::new(Opcode::Pong).finish());
            }
            Opcode::Goodbye => {
                reader.finish()?;
                self.stop_client();
                self.restart_client_at = Some(Instant::now() + Duration::from_millis(300));
            }
            _ => return Err("unexpected client opcode".to_string()),
        }
        Ok(())
    }

    fn handle_login(&mut self, username: &str, password: &[u8], session_id: &str) {
        if username.len() > MAX_USERNAME
            || password.len() > MAX_PASSWORD
            || session_id.len() > MAX_SESSION_ID
        {
            let _ = self.send_error(ErrorCode::BadRequest, "请求超出长度限制", 0);
            return;
        }
        if let Some(deadline) = self
            .failures
            .get(username)
            .and_then(|state| state.locked_until)
        {
            if Instant::now() < deadline {
                let wait = deadline.saturating_duration_since(Instant::now());
                let _ = self.send_error(
                    ErrorCode::AccountLocked,
                    "尝试次数过多，请稍后再试",
                    wait.as_millis().min(u32::MAX as u128) as u32,
                );
                self.audit("LOGIN_LOCKED", username, "rate-limited");
                return;
            }
        }

        if auth::lookup_user(username).is_none() {
            auth::dummy_verify(password, &self.dummy_hash);
        }
        let result = auth::authenticate(
            username,
            password,
            self.config.allow_root,
            self.config.minimum_uid,
        );
        match result {
            Ok(user) => {
                self.failures.remove(username);
                let session = if sessions::find(&self.sessions, session_id).is_some() {
                    session_id.to_string()
                } else {
                    self.default_session()
                };
                if let Err(error) = self.start_session(&user.name, &session) {
                    self.log(&format!("cannot start session for {}: {error}", user.name));
                    let _ = self.send_error(ErrorCode::SessionFailed, "无法启动桌面会话", 0);
                    return;
                }
                self.audit(
                    "LOGIN_OK",
                    &user.name,
                    &format!("uid={} session={}", user.uid, session),
                );
                self.send(&{
                    let mut writer = Writer::new(Opcode::LoginOk);
                    writer.string(&user.name).string(&session);
                    writer.finish()
                });
                self.stop_client();
                self.mode = Mode::Greeter;
            }
            Err(failure) => {
                let (delay, locked, retry) = {
                    let state = self
                        .failures
                        .entry(username.to_string())
                        .or_insert(FailureState {
                            count: 0,
                            locked_until: None,
                        });
                    state.count = state.count.saturating_add(1);
                    let backoff = 250u64.saturating_mul(1 << (state.count.min(5) - 1));
                    let delay = self
                        .config
                        .failure_delay_ms
                        .saturating_add(backoff)
                        .min(5000);
                    if state.count >= self.config.max_failures {
                        state.locked_until =
                            Some(Instant::now() + Duration::from_secs(self.config.lockout_seconds));
                        state.count = 0;
                    }
                    let locked = state.locked_until.is_some();
                    let retry = if locked {
                        (self.config.lockout_seconds * 1000).min(u32::MAX as u64) as u32
                    } else {
                        0
                    };
                    (delay, locked, retry)
                };
                self.audit(
                    "LOGIN_FAIL",
                    username,
                    &format!("reason={:?}", failure.error),
                );
                std::thread::sleep(Duration::from_millis(delay));
                let code = match failure.error {
                    AuthError::Denied => ErrorCode::AuthFailed,
                    AuthError::Expired | AuthError::MustChange => ErrorCode::AccountDisabled,
                    AuthError::Disabled | AuthError::Policy => ErrorCode::AccountDisabled,
                };
                let message = if locked {
                    "尝试次数过多，账户已暂时锁定"
                } else {
                    failure.message
                };
                let _ = self.send_error(code, message, retry);
                self.mode = Mode::Greeter;
            }
        }
    }

    fn handle_oobe(
        &mut self,
        username: &str,
        full_name: &str,
        password: &[u8],
        timezone: &str,
        locale: &str,
        keyboard: &str,
        hostname: &str,
        auto_login: bool,
    ) {
        if password.len() > MAX_PASSWORD
            || username.len() > MAX_USERNAME
            || full_name.len() > 128
            || timezone.len() > 64
            || locale.len() > 32
            || keyboard.len() > 32
            || hostname.len() > 64
        {
            let _ = self.send_error(ErrorCode::FieldInvalid, "输入超出长度限制", 0);
            return;
        }
        let users = auth::human_users(self.config.minimum_uid);
        if !users.is_empty() || self.config.oobe_marker.is_file() {
            let _ = self.send_error(ErrorCode::OobeConflict, "首次设置已完成", 0);
            return;
        }
        let password = match std::str::from_utf8(password) {
            Ok(value) => value,
            Err(_) => {
                let _ = self.send_error(ErrorCode::FieldInvalid, "密码必须是有效文本", 0);
                return;
            }
        };
        let settings = SystemSettings {
            hostname,
            timezone,
            locale,
            keyboard,
            auto_login,
        };
        match accounts::create_user(
            AccountSpec {
                username,
                full_name,
                password,
                minimum_uid: self.config.minimum_uid,
            },
            &settings,
        ) {
            Ok(user) => {
                if let Err(error) = accounts::mark_oobe_complete(&user.name) {
                    let _ = self.send_error(
                        ErrorCode::Internal,
                        &format!("无法写入首次设置完成标记: {error}"),
                        0,
                    );
                    return;
                }
                if auto_login {
                    let _ = accounts::set_auto_login(&self.config.state_dir, &user.name, true);
                    self.state.auto_login = true;
                    self.state.auto_login_user = user.name.clone();
                }
                let _ =
                    accounts::apply_keyboard_now(keyboard, &self.config.display, &self.auth_path());
                self.audit(
                    "OOBE_COMPLETE",
                    &user.name,
                    &format!("uid={} hostname={hostname}", user.uid),
                );
                self.log(&format!("OOBE completed for {}", user.name));
                let session = self.default_session();
                self.send(&{
                    let mut writer = Writer::new(Opcode::OobeOk);
                    writer.string(&user.name);
                    writer.finish()
                });
                self.stop_client();
                self.mode = Mode::Greeter;
                if let Err(error) = self.start_session(&user.name, &session) {
                    self.log(&format!("first login failed: {error}"));
                    self.start_client().ok();
                }
            }
            Err(error) => {
                self.audit("OOBE_FAIL", username, &format!("reason={error}"));
                let _ = self.send_error(ErrorCode::FieldInvalid, &error, 0);
            }
        }
    }

    fn handle_power(&mut self, action: PowerAction) {
        if !self.config.allow_power {
            let _ = self.send_error(ErrorCode::BadRequest, "系统策略禁止电源操作", 0);
            return;
        }
        let _ = self.send(&Writer::new(Opcode::PowerOk).finish());
        self.initiate_power(action, "greeter");
    }

    fn initiate_power(&mut self, action: PowerAction, actor: &str) {
        if self.power_in_progress {
            return;
        }
        self.power_in_progress = true;
        self.power_listener = None;
        let _ = fs::remove_file(self.power_socket_path());

        let action_name = match action {
            PowerAction::Reboot => "reboot",
            PowerAction::PowerOff => "poweroff",
        };
        self.audit(
            "POWER",
            actor,
            match action {
                PowerAction::Reboot => "reboot",
                PowerAction::PowerOff => "power-off",
            },
        );

        let log_path = Path::new(SHUTDOWN_LOG_PATH);
        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o644)
            .open(log_path)
        {
            let _ = fs::set_permissions(log_path, fs::Permissions::from_mode(0o644));
            let _ = writeln!(
                file,
                "[  0.000s] [DDM   ] Initiating {action_name} transition (actor={actor})..."
            );
        }

        // Launch the full-screen splash first while Xorg is still live so the
        // display transitions smoothly without exposing a blank screen or resetting Xorg.
        if self.xorg_pid.is_none() {
            if let Err(error) = self.start_xorg() {
                self.log(&format!("cannot ensure Xorg for power splash: {error}"));
            }
        }
        if self.xorg_pid.is_some() {
            match self.spawn_power_splash(action) {
                Ok(()) => std::thread::sleep(Duration::from_millis(140)),
                Err(error) => self.log(&format!("power splash failed to start: {error}")),
            }
        }

        self.stop_session_keep_xorg();
        self.stop_client();

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
            let _ = writeln!(
                file,
                "[  0.050s] [DDM   ] Handing {action_name} sequence to systemg PID 1 (/bin/systemctl --no-ddm {action_name})..."
            );
        }

        match Command::new("/bin/systemctl")
            .arg("--no-ddm")
            .arg(action_name)
            .status()
        {
            Ok(exit) if exit.success() => {
                self.log(&format!(
                    "handed {action_name} workflow cleanly to systemg PID 1"
                ));
                return;
            }
            Ok(exit) => self.log(&format!(
                "systemctl --no-ddm {action_name} exited with {exit}"
            )),
            Err(error) => self.log(&format!("failed to invoke systemctl: {error}")),
        }

        // Emergency fallback only if PID 1 is not systemg.
        self.shutdown_children();
        unsafe {
            libc::sync();
            let command = match action {
                PowerAction::Reboot => libc::RB_AUTOBOOT,
                PowerAction::PowerOff => libc::RB_POWER_OFF,
            };
            libc::reboot(command);
        }
        let command = match action {
            PowerAction::Reboot => "/sbin/reboot",
            PowerAction::PowerOff => "/sbin/poweroff",
        };
        let _ = Command::new(command).arg("-f").status();
        SHUTDOWN.store(true, Ordering::Relaxed);
    }

    fn send_server_config(&mut self) -> Result<(), String> {
        let users = auth::human_users(self.config.minimum_uid);
        let hostname = read_hostname();
        let mut writer = Writer::new(Opcode::ServerConfig);
        writer.u8(self.mode.as_u8());
        writer.string(&hostname);
        writer.string(&self.state.last_user);
        writer.string(&self.state.last_session);
        writer.bool(self.config.allow_power);
        writer.u16(self.sessions.len().min(u16::MAX as usize) as u16);
        for session in &self.sessions {
            writer.string(&session.id);
            writer.string(&session.name);
        }
        writer.u16(users.len().min(u16::MAX as usize) as u16);
        for user in &users {
            writer.string(&user.name);
            writer.string(&user.display_name());
        }
        writer.string(&read_locale());
        writer.string(&read_timezone());
        writer.string(&read_keyboard());
        writer.string("9.0");
        writer.string(&suggested_username(&users));
        writer.string(&self.config.wallpaper);
        let frame = writer.finish();
        self.send(&frame);
        Ok(())
    }

    fn send(&self, payload: &[u8]) {
        let Some(client) = self.client.as_ref() else {
            return;
        };
        write_fd_frame(client.fd, payload, |msg| self.log(msg));
    }

    fn send_error(&mut self, code: ErrorCode, message: &str, retry_ms: u32) -> Result<(), String> {
        let mut writer = Writer::new(Opcode::Error);
        writer.u16(code as u16).string(message).u32(retry_ms);
        self.send(&writer.finish());
        Ok(())
    }

    fn err(&mut self, code: ErrorCode, message: &str) -> Result<(), String> {
        self.send_error(code, message, 0)
    }

    fn audit(&self, event: &str, user: &str, detail: &str) {
        let line = format!(
            "{} event={} user={} {}\n",
            timestamp(),
            sanitize_log(event),
            sanitize_log(user),
            sanitize_log(detail)
        );
        let path = self.config.log_dir.join("auth.log");
        let _ = append_secure(&path, line.as_bytes(), 0o600);
        eprintln!("ddm-audit: {}", line.trim_end());
    }

    fn log(&self, message: &str) {
        let line = format!("{} {}\n", timestamp(), sanitize_log(message));
        let path = self.config.log_dir.join("ddm.log");
        let _ = append_secure(&path, line.as_bytes(), 0o600);
        eprint!("ddm: {}\n", sanitize_log(message));
    }

    fn cleanup_stale(&self) {
        for (name, predicate) in [
            (XORG_PID, "Xorg"),
            (GREETER_PID, "spui"),
            (SESSION_PID, "spui"),
        ] {
            let path = self.runtime().join(name);
            if let Some(pid) = read_pid(&path) {
                if pid_alive(pid) && cmdline_contains(pid, predicate) {
                    let _ = terminate_group(pid, Duration::from_secs(2));
                }
            }
            let _ = fs::remove_file(path);
        }
        for path in [
            self.auth_path(),
            self.greeter_auth_path(),
            self.socket_path(),
            self.power_socket_path(),
            self.runtime().join(XORG_PID),
            self.runtime().join(GREETER_PID),
            self.runtime().join(SESSION_PID),
        ] {
            let _ = fs::remove_file(path);
        }
    }
}

fn prepare_x_socket(config: &Config) -> Result<(), String> {
    if let Some(parent) = config.x_socket().parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o1777));
    }
    if config.x_lock().is_file() {
        if let Ok(text) = fs::read_to_string(config.x_lock()) {
            if let Ok(pid) = text.trim().parse::<i32>() {
                if pid_alive(pid) {
                    return Err(format!(
                        "display {} is already in use by pid {pid}",
                        config.display
                    ));
                }
            }
        }
        let _ = fs::remove_file(config.x_lock());
    }
    let _ = fs::remove_file(config.x_socket());
    Ok(())
}

fn peer_uid(fd: RawFd) -> io::Result<u32> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut _ as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

fn spawn_as(
    user: &UserRecord,
    argv: &[String],
    env: &[(String, String)],
    cwd: &Path,
    log: File,
    name: Option<CString>,
) -> Result<i32, String> {
    let program = argv.first().ok_or("empty command")?;
    let mut command = Command::new(program);
    command.args(&argv[1..]);
    command.env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    command.current_dir(cwd);
    command.stdin(Stdio::null());
    command
        .stdout(Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ))
        .stderr(Stdio::from(log));
    let uid = user.uid;
    let gid = user.gid;
    let name = name.unwrap_or_else(|| CString::new(user.name.as_str()).unwrap_or_default());
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::new(
                    io::Error::last_os_error().kind(),
                    format!("setsid: {}", io::Error::last_os_error()),
                ));
            }
            // setsid() already makes this process a session and process-group
            // leader; calling setpgid(0, 0) here fails with EPERM on Linux.
            if libc::initgroups(name.as_ptr(), gid) < 0 {
                return Err(io::Error::new(
                    io::Error::last_os_error().kind(),
                    format!("initgroups: {}", io::Error::last_os_error()),
                ));
            }
            if libc::setgid(gid) < 0 {
                return Err(io::Error::new(
                    io::Error::last_os_error().kind(),
                    format!("setgid({gid}): {}", io::Error::last_os_error()),
                ));
            }
            if libc::setuid(uid) < 0 {
                return Err(io::Error::new(
                    io::Error::last_os_error().kind(),
                    format!("setuid({uid}): {}", io::Error::last_os_error()),
                ));
            }
            libc::umask(0o022);
            Ok(())
        });
    }
    let child = command.spawn().map_err(|error| error.to_string())?;
    Ok(child.id() as i32)
}

fn write_fd_frame(fd: RawFd, payload: &[u8], mut on_error: impl FnMut(&str)) {
    let Ok(frame) = wire::encode_frame(payload) else {
        return;
    };
    let mut offset = 0usize;
    while offset < frame.len() {
        let written = unsafe {
            libc::send(
                fd,
                frame[offset..].as_ptr() as *const libc::c_void,
                frame.len() - offset,
                libc::MSG_NOSIGNAL,
            )
        };
        if written <= 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            on_error(&format!("socket write failed: {error}"));
            return;
        }
        offset += written as usize;
    }
}

fn read_single_power_action(fd: RawFd) -> Result<PowerAction, String> {
    let mut buffer = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if let Some(payload) = wire::parse_frame(&mut buffer).map_err(|e| e.to_string())? {
            let mut reader = Reader::new(&payload);
            let opcode = reader.opcode()?;
            if opcode != Opcode::Power {
                return Err("unexpected power socket opcode".to_string());
            }
            let action = PowerAction::from_u8(reader.u8()?)
                .ok_or_else(|| "unknown power action".to_string())?;
            reader.finish()?;
            return Ok(action);
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(100) as i32;
        let ready = unsafe { libc::poll(&mut pfd, 1, remaining_ms.max(10)) };
        if ready <= 0 {
            continue;
        }
        let mut chunk = [0u8; 512];
        let count = unsafe {
            libc::recv(
                fd,
                chunk.as_mut_ptr() as *mut libc::c_void,
                chunk.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if count == 0 {
            return Err("power client closed early".to_string());
        }
        if count < 0 {
            let err = io::Error::last_os_error();
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                continue;
            }
            return Err(err.to_string());
        }
        buffer.extend_from_slice(&chunk[..count as usize]);
        if buffer.len() > 1024 {
            return Err("power request frame too large".to_string());
        }
    }
    Err("timed out reading power request".to_string())
}

fn group_alive(pid: i32) -> bool {
    if pid_alive(pid) {
        return true;
    }
    unsafe {
        if libc::kill(-pid, 0) == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn terminate_group(pid: i32, grace: Duration) -> i32 {
    if pid <= 0 {
        return 0;
    }
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        let mut status = 0;
        let _ = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if !group_alive(pid) {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
    let kill_deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < kill_deadline {
        let mut status = 0;
        let _ = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if !group_alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    0
}

fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe {
        if libc::kill(pid, 0) != 0 {
            return io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        }
    }
    // A zombie still answers kill(0) until PID 1 reaps it, but it no longer
    // owns the display or the session process group.
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        if let Some(rest) = stat.rsplit_once(')') {
            if rest.1.trim_start().starts_with('Z') {
                return false;
            }
        }
    }
    true
}

fn cmdline_contains(pid: i32, needle: &str) -> bool {
    let path = format!("/proc/{pid}/cmdline");
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    text.split('\0').any(|part| part.contains(needle))
}

fn exit_description(status: i32) -> String {
    if libc::WIFEXITED(status) {
        format!("exit={}", libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        format!("signal={}", libc::WTERMSIG(status))
    } else {
        format!("status={status}")
    }
}

fn install_signals() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, handle_shutdown as *const () as usize);
        libc::signal(libc::SIGINT, handle_shutdown as *const () as usize);
        libc::signal(libc::SIGHUP, handle_reload as *const () as usize);
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
}

extern "C" fn handle_shutdown(_: c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

extern "C" fn handle_reload(_: c_int) {
    RELOAD.store(true, Ordering::Relaxed);
}

fn create_dir(path: &Path, mode: u32, uid: u32, gid: u32) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| format!("{}: {error}", path.display()))?;
    xauth::set_mode(path, mode).map_err(|error| error.to_string())?;
    xauth::chown(path, uid, gid).map_err(|error| error.to_string())?;
    Ok(())
}

fn open_log(path: &Path, mode: u32, owner: Option<(u32, u32)>) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(mode)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    xauth::set_mode(path, mode).map_err(|error| error.to_string())?;
    if let Some((uid, gid)) = owner {
        xauth::chown(path, uid, gid).map_err(|error| error.to_string())?;
    }
    Ok(file)
}

fn append_secure(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(mode)
        .open(path)?;
    xauth::set_mode(path, mode)?;
    file.write_all(bytes)?;
    file.flush()
}

fn write_pid(path: &Path, pid: i32) {
    let _ = accounts::write_secure(path, format!("{pid}\n").as_bytes(), 0o600);
}

fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "spaceos".to_string())
}

fn read_locale() -> String {
    let text = fs::read_to_string("/etc/locale.conf").unwrap_or_default();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("LANG=") {
            let value = value.trim();
            if value.starts_with("en_US") {
                return "en_US".to_string();
            }
            return "zh_CN".to_string();
        }
    }
    "zh_CN".to_string()
}

fn read_timezone() -> String {
    fs::read_to_string("/etc/timezone")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "UTC".to_string())
}

fn read_keyboard() -> String {
    let text = fs::read_to_string("/etc/X11/xorg.conf.d/20-input.conf").unwrap_or_default();
    for line in text.lines() {
        let parts: Vec<&str> = line.split('"').collect();
        if let Some(index) = parts.iter().position(|part| part.trim() == "XkbLayout") {
            if let Some(value) = parts.get(index + 2) {
                let value = value.trim();
                if !value.is_empty() {
                    return value.to_string();
                }
            }
        }
    }
    "us".to_string()
}

fn read_tz() -> Option<String> {
    fs::read_to_string("/etc/TZ")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn suggested_username(users: &[UserRecord]) -> String {
    for candidate in ["spaceos", "user", "admin"] {
        if crate::ddm::config::is_valid_user_name(candidate)
            && !users.iter().any(|user| user.name == candidate)
        {
            return candidate.to_string();
        }
    }
    for index in 1..100 {
        let candidate = format!("user{index}");
        if !users.iter().any(|user| user.name == candidate) {
            return candidate;
        }
    }
    "user".to_string()
}

fn read_state(directory: &Path) -> State {
    let path = directory.join("state");
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut state = State::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "last_user" => state.last_user = value.trim().to_string(),
            "last_session" => state.last_session = value.trim().to_string(),
            "auto_login" => state.auto_login = value.trim() == "1",
            "auto_login_user" => state.auto_login_user = value.trim().to_string(),
            _ => {}
        }
    }
    state
}

fn write_state(directory: &Path, state: &State) {
    let content = format!(
        "last_user={}\nlast_session={}\nauto_login={}\nauto_login_user={}\n",
        state.last_user,
        state.last_session,
        u8::from(state.auto_login),
        state.auto_login_user
    );
    let _ = accounts::write_secure(&directory.join("state"), content.as_bytes(), 0o600);
}

fn sanitize_log(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(512)
        .collect()
}

fn wipe(bytes: &mut Vec<u8>) {
    for byte in bytes.iter_mut() {
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
    bytes.clear();
}

#[allow(deprecated)]
fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as libc::time_t);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&seconds, &mut tm);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state() {
        let directory = std::env::temp_dir().join(format!("ddm-state-{}", std::process::id()));
        let _ = fs::create_dir_all(&directory);
        accounts::write_secure(
            &directory.join("state"),
            b"last_user=alice\nlast_session=spui\nauto_login=1\nauto_login_user=alice\n",
            0o600,
        )
        .unwrap();
        let state = read_state(&directory);
        assert_eq!(state.last_user, "alice");
        assert_eq!(state.last_session, "spui");
        assert!(state.auto_login);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn suggests_available_user() {
        let users = vec![UserRecord {
            name: "spaceos".to_string(),
            uid: 1000,
            gid: 1000,
            gecos: String::new(),
            home: "/home/spaceos".to_string(),
            shell: "/bin/sh".to_string(),
        }];
        assert_eq!(suggested_username(&users), "user");
    }
}
