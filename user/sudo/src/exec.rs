//
//       user/sudo/src/exec.rs
//       Program resolution, environment sanitising and target-user exec
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeSet;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::User;

pub struct RunSpec<'a> {
    pub program: &'a Path,
    pub args: &'a [String],
    pub target_user: &'a User,
    pub target_gid: u32,
    pub groups: &'a [u32],
    pub login: bool,
    pub shell: bool,
    pub preserve_env: bool,
    pub env_keep: &'a BTreeSet<String>,
    pub secure_path: &'a str,
    pub cwd: &'a Path,
    pub invoking_user: &'a User,
    pub invoking_uid: u32,
    pub invoking_gid: u32,
}

pub fn resolve_program(program: &str, secure_path: &str) -> Result<PathBuf, String> {
    if program.is_empty() {
        return Err("empty command".into());
    }
    if program.contains('/') {
        if !program.starts_with('/') {
            return Err(format!("command '{program}' is not an absolute path"));
        }
        return validate_program(Path::new(program));
    }
    for directory in secure_path.split(':').filter(|dir| !dir.is_empty()) {
        let candidate = Path::new(directory).join(program);
        if is_executable(&candidate) {
            return Ok(std::fs::canonicalize(&candidate).unwrap_or(candidate));
        }
    }
    Err(format!(
        "command '{program}' not found in secure_path ({secure_path})"
    ))
}

fn validate_program(path: &Path) -> Result<PathBuf, String> {
    if !is_executable(path) {
        return Err(format!("{} is not executable", path.display()));
    }
    Ok(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn shell_command(args: &[String]) -> String {
    args.join(" ")
}

pub fn run(spec: RunSpec<'_>) -> Result<i32, String> {
    let mut command = Command::new(spec.program);
    if spec.login || spec.shell {
        let shell = if spec.target_user.shell.is_empty() {
            "/bin/sh"
        } else {
            spec.target_user.shell.as_str()
        };
        command = Command::new(shell);
        if spec.args.is_empty() {
            command.arg(if spec.login { "-l" } else { "-s" });
        } else {
            command.arg("-c").arg(shell_command(spec.args));
        }
        if spec.login {
            let argv0 = format!(
                "-{}",
                Path::new(shell)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            command.arg0(argv0);
        }
    } else {
        command.args(spec.args);
    }

    command
        .env_clear()
        .current_dir(spec.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    command.env("PATH", spec.secure_path);
    command.env("HOME", &spec.target_user.home);
    command.env("USER", &spec.target_user.name);
    command.env("LOGNAME", &spec.target_user.name);
    command.env("SHELL", &spec.target_user.shell);
    command.env("SUDO_USER", &spec.invoking_user.name);
    command.env("SUDO_UID", spec.invoking_uid.to_string());
    command.env("SUDO_GID", spec.invoking_gid.to_string());
    command.env(
        "SUDO_COMMAND",
        format!("{} {}", spec.program.display(), shell_command(spec.args)).trim_end(),
    );

    for name in ["TERM", "LANG", "LANGUAGE", "TZ"] {
        if let Ok(value) = std::env::var(name) {
            command.env(name, value);
        }
    }
    for (name, value) in std::env::vars() {
        if name.starts_with("LC_") {
            command.env(name, value);
        }
    }
    if spec.preserve_env {
        for name in spec.env_keep {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
    }

    let target_uid = spec.target_user.uid;
    let target_gid = spec.target_gid;
    let groups = spec.groups.to_vec();
    if target_uid == 0 && target_gid == 0 {
        // The setuid sudo process is already effective root; set all IDs to
        // root without touching supplementary groups.
        command.uid(0).gid(0);
    } else {
        // setgroups must run before setgid/setuid; CommandExt::groups is not
        // ordered for this case on all std versions.
        unsafe {
            command.pre_exec(move || {
                if libc::setgroups(groups.len(), groups.as_ptr()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setgid(target_gid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setuid(target_uid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let status = command
        .status()
        .map_err(|error| format!("cannot execute {}: {error}", spec.program.display()))?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}
