//
//       user/sudo/src/app.rs
//       sudo/doas request flow: policy, auth, lecture, ticket, exec, audit
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::auth;
use crate::config::{self, Decision, Defaults, Doas, Group, Sudoers, User};
use crate::exec::{self, RunSpec};
use crate::lecture;
use crate::paths::Paths;
use crate::ticket;
use crate::util::{append_private, now};

pub struct Options {
    pub target_user: Option<String>,
    pub target_group: Option<String>,
    pub login: bool,
    pub shell: bool,
    pub preserve_env: bool,
    pub non_interactive: bool,
    pub stdin_password: bool,
    pub askpass: bool,
    pub doas: bool,
    pub command: Vec<String>,
}

fn real_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn tty_name() -> String {
    let mut buffer = [0 as libc::c_char; 256];
    let result = unsafe { libc::ttyname_r(0, buffer.as_mut_ptr(), buffer.len()) };
    if result == 0 {
        unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    } else {
        String::new()
    }
}

fn read_password_from_reader(reader: &mut dyn BufRead) -> Result<Option<String>, String> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).map_err(|error| error.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
}

fn read_password(stdin_password: bool) -> Result<Option<String>, String> {
    if stdin_password {
        let mut stdin = io::stdin().lock();
        return read_password_from_reader(&mut stdin);
    }
    let mut tty = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|error| format!("cannot open /dev/tty: {error}"))?;
    let fd = tty.as_raw_fd();
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut term) } != 0 {
        return Err("cannot read terminal attributes".into());
    }
    let original = term;
    term.c_lflag &= !libc::ECHO;
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, &term);
    }
    write!(tty, "{}", lecture::password_prompt()).map_err(|error| error.to_string())?;
    let _ = tty.flush();
    let mut line = String::new();
    let read =
        BufReader::new(tty.try_clone().map_err(|error| error.to_string())?).read_line(&mut line);
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, &original);
    }
    writeln!(tty).ok();
    let n = read.map_err(|error| error.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
}

fn resolve_askpass_program(explicit: Option<&Path>) -> Result<PathBuf, String> {
    let candidate = if let Some(path) = explicit {
        path.to_path_buf()
    } else if let Some(env) = std::env::var_os("SUDO_ASKPASS").filter(|v| !v.is_empty()) {
        PathBuf::from(env)
    } else {
        let default = PathBuf::from("/usr/bin/spui-askpass");
        if default.is_file() {
            default
        } else {
            return Err(
                "no askpass program specified (set SUDO_ASKPASS or install /usr/bin/spui-askpass)"
                    .into(),
            );
        }
    };
    if !candidate.is_absolute() {
        return Err(format!(
            "askpass program '{}' must be an absolute path",
            candidate.display()
        ));
    }
    let meta = fs::metadata(&candidate)
        .map_err(|error| format!("cannot run askpass '{}': {error}", candidate.display()))?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "askpass program '{}' is not executable",
            candidate.display()
        ));
    }
    Ok(candidate)
}

fn read_password_askpass(
    explicit_helper: Option<&Path>,
    prompt: &str,
    user: &User,
    uid: u32,
    euid: u32,
    attempt: u32,
    max_attempts: u32,
    last_error: Option<&str>,
) -> Result<Option<String>, String> {
    let helper = resolve_askpass_program(explicit_helper)?;
    let mut cmd = Command::new(&helper);
    cmd.arg(prompt)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for var in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "LANG",
        "LC_ALL",
        "LANGUAGE",
        "SPK_LANG",
        "TZ",
        "SPUI_AUTH_TITLE",
        "SPUI_AUTH_CMD",
    ] {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }
    cmd.env("SPUI_AUTH_ATTEMPT", (attempt + 1).to_string());
    cmd.env("SPUI_AUTH_MAX_ATTEMPTS", max_attempts.to_string());
    if let Some(err) = last_error {
        cmd.env("SPUI_AUTH_ERROR", err);
    }
    // Always run the graphical askpass dialog under the invoking user's UID/GID.
    if euid == 0 && uid != 0 {
        cmd.uid(user.uid).gid(user.gid);
    }
    let output = cmd
        .output()
        .map_err(|error| format!("cannot run askpass '{}': {error}", helper.display()))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let pw = text.lines().next().unwrap_or("").trim_end_matches('\r');
    Ok(Some(pw.to_string()))
}

fn authenticate_with<F>(
    paths: &Paths,
    defaults: &Defaults,
    user: &User,
    uid: u32,
    options: &Options,
    lecture_mode: bool,
    mut read_next: F,
) -> Result<bool, String>
where
    F: FnMut(u32, u32, Option<&str>) -> Result<Option<String>, String>,
{
    if options.non_interactive {
        return Err("a password is required and --non-interactive was given".into());
    }
    let lock_window = defaults.timestamp_timeout.max(300);
    if ticket::locked(paths, uid, defaults.passwd_tries, lock_window) {
        return Err("too many failed authentication attempts; try again later".into());
    }
    if lecture_mode
        && !options.stdin_password
        && !options.askpass
        && lecture::needs_lecture(paths, uid, defaults.lecture)
    {
        let text = lecture::lecture_text(defaults).map_err(|error| error.to_string())?;
        eprint!("{text}");
        eprintln!();
    }
    let max_attempts = if options.stdin_password {
        1
    } else {
        defaults.passwd_tries.max(1)
    };
    let mut last_error: Option<String> = None;
    for attempt in 0..max_attempts {
        let Some(mut password) = read_next(attempt, max_attempts, last_error.as_deref())? else {
            return Err(if options.askpass {
                "authentication cancelled"
            } else if attempt == 0 {
                "no password provided"
            } else {
                "incorrect password"
            }
            .into());
        };
        let verify = auth::verify_password(paths, &user.name, &password);
        let is_empty = password.is_empty();
        // Zero plaintext password immediately after verification.
        unsafe {
            password.as_mut_vec().fill(0);
        }
        password.clear();
        match verify {
            Ok(()) => {
                ticket::clear_failures(paths, uid).map_err(|error| error.to_string())?;
                if lecture_mode
                    && !options.stdin_password
                    && !options.askpass
                    && lecture::needs_lecture(paths, uid, defaults.lecture)
                {
                    lecture::mark_lectured(paths, uid).map_err(|error| error.to_string())?;
                }
                return Ok(is_empty);
            }
            Err(auth::AuthError::BadPassword) => {
                ticket::record_failure(paths, uid).map_err(|error| error.to_string())?;
                if attempt + 1 >= max_attempts {
                    return Err("incorrect password".into());
                }
                let remaining = max_attempts - attempt - 1;
                last_error = Some(if lecture::lang_is_zh() || options.askpass {
                    format!("密码错误，请重试（还可尝试 {remaining} 次）")
                } else {
                    "Sorry, try again.".into()
                });
                if !options.askpass {
                    if lecture::lang_is_zh() {
                        eprintln!("密码错误，请重试。");
                    } else {
                        eprintln!("Sorry, try again.");
                    }
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("incorrect password".into())
}

fn authenticate(
    paths: &Paths,
    defaults: &Defaults,
    user: &User,
    uid: u32,
    options: &Options,
    lecture_mode: bool,
) -> Result<bool, String> {
    let euid = effective_uid();
    authenticate_with(
        paths,
        defaults,
        user,
        uid,
        options,
        lecture_mode,
        |attempt, max_attempts, last_error| {
            if options.askpass {
                read_password_askpass(
                    None,
                    &lecture::password_prompt(),
                    user,
                    uid,
                    euid,
                    attempt,
                    max_attempts,
                    last_error,
                )
            } else {
                read_password(options.stdin_password)
            }
        },
    )
}

fn announce_first_auth(paths: &Paths, uid: u32, empty_password: bool) {
    if !lecture::needs_first_auth(paths, uid) {
        return;
    }
    eprintln!("{}", lecture::first_auth_text(empty_password));
    if let Err(error) = lecture::mark_first_auth(paths, uid) {
        eprintln!("warning: cannot record first authorization: {error}");
    }
}

#[allow(clippy::too_many_arguments)]
fn audit(
    paths: &Paths,
    uid: u32,
    user: &User,
    target: &User,
    tty: &str,
    cwd: &Path,
    command: &[String],
    result: &str,
    exit: i32,
) {
    let line = format!(
        "{} uid={} user={} target={} tty={} cwd={} command={:?} result={} exit={}",
        now(),
        uid,
        user.name,
        target.name,
        if tty.is_empty() { "-" } else { tty },
        cwd.display(),
        command,
        result,
        exit
    );
    if let Err(error) = append_private(&paths.audit(), &line) {
        eprintln!("warning: cannot write sudo audit log: {error}");
    }
}

fn find_target(users: &[User], requested: Option<&str>) -> Result<User, String> {
    let name = requested.unwrap_or("root");
    config::find_user(users, name)
        .cloned()
        .ok_or_else(|| format!("unknown user '{name}'"))
}

fn target_groups(
    groups: &[Group],
    target: &User,
    target_group: Option<&str>,
) -> Result<(u32, Vec<u32>, Vec<String>), String> {
    let gid = if let Some(name) = target_group {
        config::find_group(groups, name)
            .map(|group| group.gid)
            .ok_or_else(|| format!("unknown group '{name}'"))?
    } else {
        target.gid
    };
    let mut ids = config::user_group_ids(target, groups);
    if !ids.contains(&gid) {
        ids.push(gid);
    }
    let names: Vec<String> = groups
        .iter()
        .filter(|group| {
            group.gid == gid || group.members.iter().any(|member| member == &target.name)
        })
        .map(|group| group.name.clone())
        .collect();
    Ok((gid, ids, names))
}

fn load_policy(
    paths: &Paths,
    doas: bool,
) -> Result<(Option<Sudoers>, Option<Doas>, Defaults), String> {
    if doas {
        let (policy, from_file) = Doas::load(paths)?;
        if !from_file {
            eprintln!("warning: /etc/doas.conf not found; using built-in %wheel/%sudo rule");
        }
        let defaults = policy.defaults.clone();
        Ok((None, Some(policy), defaults))
    } else {
        let (policy, from_file) = Sudoers::load(paths)?;
        if !from_file {
            eprintln!("warning: /etc/sudoers not found; using built-in %wheel/%sudo rule");
        }
        let defaults = policy.defaults.clone();
        Ok((Some(policy), None, defaults))
    }
}

fn command_for_shell(options: &Options) -> Vec<String> {
    if options.login || options.shell {
        options.command.clone()
    } else {
        // exec::run already uses command[0] as the program; do not pass it
        // again as argv[1].
        options.command.get(1..).unwrap_or(&[]).to_vec()
    }
}

pub fn execute(paths: &Paths, options: Options) -> Result<i32, String> {
    let uid = real_uid();
    let euid = effective_uid();
    if euid != 0 && uid != 0 {
        return Err("sudo/doas must be installed setuid root".into());
    }
    let users = config::load_users(paths)?;
    let groups = config::load_groups(paths)?;
    let user = config::find_user_by_uid(&users, uid)
        .cloned()
        .ok_or_else(|| format!("uid {uid} has no passwd entry"))?;
    let target = find_target(&users, options.target_user.as_deref())?;
    let (target_gid, target_group_ids, target_group_names) =
        target_groups(&groups, &target, options.target_group.as_deref())?;

    if options.command.is_empty() && !options.login && !options.shell {
        return Err("no command specified".into());
    }

    let (sudoers, doas_policy, defaults) = load_policy(paths, options.doas)?;
    let invoking_groups = config::user_group_names(&user, &groups);
    let program = if options.login || options.shell {
        exec::resolve_program(&target.shell, &defaults.secure_path)?
    } else {
        exec::resolve_program(&options.command[0], &defaults.secure_path)?
    };
    let program_text = program.display().to_string();
    let decision = if let Some(policy) = &doas_policy {
        policy.check(&user, &invoking_groups, &target, &program_text)
    } else {
        sudoers
            .as_ref()
            .ok_or_else(|| "missing sudoers policy".to_string())?
            .check(
                &user,
                &invoking_groups,
                &target,
                &target_group_names,
                &program_text,
            )
    };
    let mut nopass = uid == 0;
    match decision {
        Decision::Allowed {
            nopass: allowed_nopass,
        } => nopass |= allowed_nopass,
        Decision::Denied if uid == 0 => {}
        Decision::Denied => {
            let tty = tty_name();
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
            audit(
                paths,
                uid,
                &user,
                &target,
                &tty,
                &cwd,
                &options.command,
                "denied",
                1,
            );
            return Err(format!("{} is not allowed", program.display()));
        }
    }

    let tty = tty_name();
    let mut empty_password = false;
    if uid != 0 && !nopass {
        let cached = !options.doas
            && ticket::valid(
                paths,
                uid,
                &tty,
                defaults.timestamp_timeout,
                defaults.timestamp_type_global,
            );
        if !cached {
            empty_password =
                match authenticate(paths, &defaults, &user, uid, &options, !options.doas) {
                    Ok(empty) => empty,
                    Err(error) => {
                        let tty = tty_name();
                        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
                        audit(
                            paths,
                            uid,
                            &user,
                            &target,
                            &tty,
                            &cwd,
                            &options.command,
                            "auth-failed",
                            1,
                        );
                        return Err(error);
                    }
                };
            if !options.doas {
                ticket::refresh(paths, uid, &tty, defaults.timestamp_timeout)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    if uid != 0 {
        announce_first_auth(paths, uid, empty_password);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    audit(
        paths,
        uid,
        &user,
        &target,
        &tty,
        &cwd,
        &options.command,
        "allowed",
        0,
    );
    let args = command_for_shell(&options);
    let code = exec::run(RunSpec {
        program: &program,
        argv0: options.command.first().map(String::as_str).unwrap_or(""),
        args: &args,
        target_user: &target,
        target_gid,
        groups: &target_group_ids,
        login: options.login,
        shell: options.shell,
        preserve_env: options.preserve_env,
        env_keep: &defaults.env_keep,
        secure_path: &defaults.secure_path,
        cwd: &cwd,
        invoking_user: &user,
        invoking_uid: uid,
        invoking_gid: user.gid,
    })?;
    audit(
        paths,
        uid,
        &user,
        &target,
        &tty,
        &cwd,
        &options.command,
        "executed",
        code,
    );
    Ok(code)
}

pub fn validate(paths: &Paths, options: &Options) -> Result<(), String> {
    if options.doas {
        return Err("doas has no ticket validation mode".into());
    }
    let uid = real_uid();
    if uid == 0 || effective_uid() != 0 {
        return if uid == 0 {
            Ok(())
        } else {
            Err("sudo must be installed setuid root".into())
        };
    }
    let users = config::load_users(paths)?;
    let user = config::find_user_by_uid(&users, uid)
        .cloned()
        .ok_or_else(|| format!("uid {uid} has no passwd entry"))?;
    let (policy, _, defaults) = load_policy(paths, false)?;
    let groups = config::load_groups(paths)?;
    let invoking_groups = config::user_group_names(&user, &groups);
    if policy
        .as_ref()
        .map(|policy| policy.list(&user, &invoking_groups).is_empty())
        .unwrap_or(true)
    {
        return Err("not allowed to run sudo".into());
    }
    let tty = tty_name();
    if ticket::valid(
        paths,
        uid,
        &tty,
        defaults.timestamp_timeout,
        defaults.timestamp_type_global,
    ) {
        ticket::refresh(paths, uid, &tty, defaults.timestamp_timeout)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    authenticate(paths, &defaults, &user, uid, options, true)?;
    ticket::refresh(paths, uid, &tty, defaults.timestamp_timeout).map_err(|error| error.to_string())
}

pub fn list(paths: &Paths, options: &Options) -> Result<(), String> {
    let uid = real_uid();
    let users = config::load_users(paths)?;
    let groups = config::load_groups(paths)?;
    let user = config::find_user_by_uid(&users, uid)
        .cloned()
        .ok_or_else(|| format!("uid {uid} has no passwd entry"))?;
    let invoking_groups = config::user_group_names(&user, &groups);
    if options.doas {
        let (policy, _) = Doas::load(paths)?;
        println!("{}", policy.list(&user, &invoking_groups).join("\n"));
    } else {
        let (policy, _) = Sudoers::load(paths)?;
        println!("{}", policy.list(&user, &invoking_groups).join("\n"));
    }
    Ok(())
}

pub fn clear_ticket(paths: &Paths, uid: u32) -> Result<(), String> {
    ticket::clear(paths, uid).map_err(|error| error.to_string())
}

pub fn clear_all_tickets(paths: &Paths) -> Result<(), String> {
    ticket::clear_all(paths).map_err(|error| error.to_string())
}

pub fn edit(paths: &Paths, options: Options) -> Result<i32, String> {
    let uid = real_uid();
    let euid = effective_uid();
    if euid != 0 && uid != 0 {
        return Err("sudo must be installed setuid root".into());
    }
    if options.command.is_empty() {
        return Err("sudo -e requires at least one file".into());
    }
    let users = config::load_users(paths)?;
    let groups = config::load_groups(paths)?;
    let user = config::find_user_by_uid(&users, uid)
        .cloned()
        .ok_or_else(|| format!("uid {uid} has no passwd entry"))?;
    let (sudoers, _, defaults) = load_policy(paths, false)?;
    let invoking_groups = config::user_group_names(&user, &groups);
    let target = find_target(&users, options.target_user.as_deref())?;
    let (_, _, target_group_names) = target_groups(&groups, &target, None)?;
    let sudoedit = "/usr/bin/sudoedit";
    let decision = sudoers
        .as_ref()
        .ok_or_else(|| "missing sudoers policy".to_string())?
        .check(
            &user,
            &invoking_groups,
            &target,
            &target_group_names,
            sudoedit,
        );
    let mut nopass = uid == 0;
    match decision {
        Decision::Allowed { nopass: allowed } => nopass |= allowed,
        Decision::Denied if uid == 0 => {}
        Decision::Denied => return Err("sudoedit is not allowed".into()),
    }
    let tty = tty_name();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let mut empty_password = false;
    if uid != 0 && !nopass {
        if !ticket::valid(
            paths,
            uid,
            &tty,
            defaults.timestamp_timeout,
            defaults.timestamp_type_global,
        ) {
            match authenticate(paths, &defaults, &user, uid, &options, true) {
                Ok(empty) => empty_password = empty,
                Err(error) => {
                    audit(
                        paths,
                        uid,
                        &user,
                        &target,
                        &tty,
                        &cwd,
                        &options.command,
                        "sudoedit-auth-failed",
                        1,
                    );
                    return Err(error);
                }
            }
            ticket::refresh(paths, uid, &tty, defaults.timestamp_timeout)
                .map_err(|error| error.to_string())?;
        }
        announce_first_auth(paths, uid, empty_password);
    }
    let editor = std::env::var("SUDO_EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let editor = exec::resolve_program(&editor, &defaults.secure_path)?;

    let temp_dir = PathBuf::from(format!("/var/tmp/sudo-{}", uid));
    fs::create_dir_all(&temp_dir).map_err(|error| error.to_string())?;
    fs::set_permissions(&temp_dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let temp_dir_c = std::ffi::CString::new(temp_dir.as_os_str().as_encoded_bytes())
        .map_err(|_| "temporary directory contains NUL".to_string())?;
    unsafe {
        libc::chown(temp_dir_c.as_ptr(), uid, user.gid);
    }
    let mut changed_any = false;
    for name in &options.command {
        let path = PathBuf::from(name);
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{} is not a regular file", path.display()));
        }
        let file_name = path
            .file_name()
            .ok_or_else(|| format!("invalid path {}", path.display()))?;
        let temp = temp_dir.join(file_name);
        let original = fs::read(&path).map_err(|error| error.to_string())?;
        fs::write(&temp, &original).map_err(|error| error.to_string())?;
        let temp_c = std::ffi::CString::new(temp.as_os_str().as_encoded_bytes())
            .map_err(|_| "temporary path contains NUL".to_string())?;
        unsafe {
            libc::chown(temp_c.as_ptr(), uid, user.gid);
        }
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
        let status = Command::new(&editor)
            .arg(&temp)
            .uid(uid)
            .gid(user.gid)
            .env_clear()
            .env("PATH", &defaults.secure_path)
            .env("HOME", &user.home)
            .env("USER", &user.name)
            .env("LOGNAME", &user.name)
            .env("TERM", std::env::var("TERM").unwrap_or_default())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| format!("cannot run editor {}: {error}", editor.display()))?;
        if !status.success() {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(format!("editor {} failed", editor.display()));
        }
        let edited = fs::read(&temp).map_err(|error| error.to_string())?;
        if edited != original {
            write_file_atomically(
                &path,
                &edited,
                metadata.mode() & 0o7777,
                metadata.uid(),
                metadata.gid(),
            )?;
            changed_any = true;
        }
        let _ = fs::remove_file(&temp);
    }
    let _ = fs::remove_dir(&temp_dir);
    audit(
        paths,
        uid,
        &user,
        &target,
        &tty,
        &cwd,
        &options.command,
        if changed_any {
            "sudoedit-changed"
        } else {
            "sudoedit"
        },
        0,
    );
    Ok(0)
}

fn write_file_atomically(
    path: &Path,
    data: &[u8],
    mode: u32,
    uid: u32,
    gid: u32,
) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("/"));
    let temp = parent.join(format!(
        ".sudoedit.{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&temp, data).map_err(|error| error.to_string())?;
    fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
        .map_err(|error| error.to_string())?;
    let temp_c = std::ffi::CString::new(temp.as_os_str().as_encoded_bytes())
        .map_err(|_| "temporary path contains NUL".to_string())?;
    unsafe {
        libc::chown(temp_c.as_ptr(), uid, gid);
    }
    fs::rename(&temp, path).map_err(|error| error.to_string())?;
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(command: &[&str], login: bool, shell: bool) -> Options {
        Options {
            target_user: None,
            target_group: None,
            login,
            shell,
            preserve_env: false,
            non_interactive: false,
            stdin_password: false,
            askpass: false,
            doas: false,
            command: command.iter().map(|value| value.to_string()).collect(),
        }
    }

    fn test_paths(password: &str) -> (tempfile::TempDir, Paths, User) {
        use sha_crypt::password_hash::PasswordHasher;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path());
        fs::create_dir_all(&paths.etc).unwrap();
        let hash = if password.is_empty() {
            String::new()
        } else {
            sha_crypt::ShaCrypt::SHA512
                .hash_password_with_salt(password.as_bytes(), b"testsalt")
                .unwrap()
                .to_string()
        };
        fs::write(
            paths.passwd(),
            "root:x:0:0:root:/root:/bin/sh\nalice:x:1000:1000:Alice:/home/alice:/bin/sh\n",
        )
        .unwrap();
        fs::write(
            paths.shadow(),
            format!("root:!::0:::::\nalice:{hash}:20000:0:99999:7:::\n"),
        )
        .unwrap();
        let user = User {
            name: "alice".into(),
            uid: 1000,
            gid: 1000,
            home: "/home/alice".into(),
            shell: "/bin/sh".into(),
        };
        (dir, paths, user)
    }

    #[test]
    fn direct_command_does_not_repeat_the_program() {
        assert_eq!(
            command_for_shell(&options(&["spk", "sync"], false, false)),
            vec!["sync".to_string()]
        );
    }

    #[test]
    fn shell_command_keeps_the_program() {
        assert_eq!(
            command_for_shell(&options(&["spk", "sync"], false, true)),
            vec!["spk".to_string(), "sync".to_string()]
        );
        assert!(command_for_shell(&options(&[], true, false)).is_empty());
    }

    #[test]
    fn stdin_eof_stops_without_locking_account() {
        let mut cursor = std::io::Cursor::new(b"" as &[u8]);
        assert_eq!(read_password_from_reader(&mut cursor).unwrap(), None);

        let (_dir, paths, user) = test_paths("secret");
        let defaults = Defaults::default();
        let mut opts = options(&["spk", "sync"], false, false);
        opts.stdin_password = true;

        // First wrong stdin password should fail cleanly after 1 attempt without triggering faillock.
        let mut calls = 0u32;
        let err = authenticate_with(&paths, &defaults, &user, 1000, &opts, false, |_, _, _| {
            calls += 1;
            Ok(Some("wrong".into()))
        })
        .unwrap_err();
        assert_eq!(err, "incorrect password");
        assert_eq!(calls, 1);
        assert!(!ticket::locked(&paths, 1000, defaults.passwd_tries, 300));

        // Second attempt with the real password must still succeed (not locked out).
        let ok = authenticate_with(&paths, &defaults, &user, 1000, &opts, false, |_, _, _| {
            Ok(Some("secret".into()))
        })
        .unwrap();
        assert!(!ok);
    }

    #[test]
    fn askpass_retries_propagate_error_and_cancel_does_not_lock() {
        let (_dir, paths, user) = test_paths("secret");
        let defaults = Defaults::default();
        let mut opts = options(&["spk", "sync"], false, false);
        opts.askpass = true;

        let mut seen_errors = Vec::new();
        let ok = authenticate_with(
            &paths,
            &defaults,
            &user,
            1000,
            &opts,
            false,
            |attempt, _, last_err| {
                seen_errors.push(last_err.map(str::to_string));
                Ok(Some(if attempt == 0 {
                    "wrong".into()
                } else {
                    "secret".into()
                }))
            },
        )
        .unwrap();
        assert!(!ok);
        assert_eq!(seen_errors.len(), 2);
        assert!(seen_errors[0].is_none());
        assert!(seen_errors[1].is_some());

        let err = authenticate_with(&paths, &defaults, &user, 1000, &opts, false, |_, _, _| {
            Ok(None)
        })
        .unwrap_err();
        assert_eq!(err, "authentication cancelled");
        assert!(!ticket::locked(&paths, 1000, defaults.passwd_tries, 300));
    }
}
