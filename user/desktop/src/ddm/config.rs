/*
 *
 *       user/desktop/src/ddm/config.rs
 *       DDM configuration parser and validation
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Small, deliberately non-magical INI parser for `/etc/ddm/ddm.conf`.
//! Unknown keys are rejected instead of silently ignored so a typo cannot
//! quietly weaken the login policy.

use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG: &str = "/etc/ddm/ddm.conf";

#[derive(Clone, Debug)]
pub struct Config {
    pub display: String,
    pub xserver: String,
    pub xserver_args: Vec<String>,
    pub greeter_user: String,
    pub greeter_program: String,
    pub greeter_args: Vec<String>,
    pub oobe_program: String,
    pub oobe_args: Vec<String>,
    pub session_dir: PathBuf,
    pub default_session: String,
    pub allow_root: bool,
    pub minimum_uid: u32,
    pub max_failures: u32,
    pub lockout_seconds: u64,
    pub failure_delay_ms: u64,
    pub allow_power: bool,
    pub auto_login: bool,
    pub auto_login_user: String,
    pub wallpaper: String,
    pub runtime_dir: PathBuf,
    pub state_dir: PathBuf,
    pub log_dir: PathBuf,
    pub oobe_marker: PathBuf,
    pub xorg_log: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            display: ":0".to_string(),
            xserver: "/usr/bin/Xorg".to_string(),
            xserver_args: vec![
                "-novtswitch".to_string(),
                "-nolisten".to_string(),
                "tcp".to_string(),
                "-noreset".to_string(),
            ],
            greeter_user: "ddm".to_string(),
            greeter_program: "/usr/bin/spui".to_string(),
            greeter_args: vec!["greeter".to_string()],
            oobe_program: "/usr/bin/spui".to_string(),
            oobe_args: vec!["oobe".to_string()],
            session_dir: PathBuf::from("/usr/share/xsessions"),
            default_session: "spui".to_string(),
            allow_root: false,
            minimum_uid: 1000,
            max_failures: 5,
            lockout_seconds: 300,
            failure_delay_ms: 600,
            allow_power: true,
            auto_login: false,
            auto_login_user: String::new(),
            wallpaper: "/usr/share/spui/wallpapers/alpine-peaks.jpg".to_string(),
            runtime_dir: PathBuf::from("/run/ddm"),
            state_dir: PathBuf::from("/var/lib/ddm"),
            log_dir: PathBuf::from("/var/log/ddm"),
            oobe_marker: PathBuf::from("/var/lib/spaceos/oobe-complete"),
            xorg_log: PathBuf::from("/var/log/xorg.log"),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        Self::parse(&text).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut config = Self::default();
        for (number, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') {
                if !line.ends_with(']') {
                    return Err(format!("line {}: malformed section", number + 1));
                }
                let section = line[1..line.len() - 1].trim().to_ascii_lowercase();
                if section != "general" && section != "display" && section != "security" {
                    return Err(format!("line {}: unknown section [{section}]", number + 1));
                }
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {}: expected key=value", number + 1));
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            let unquoted = strip_quotes(value);
            match key.as_str() {
                "display" => config.display = unquoted.to_string(),
                "xserver" => config.xserver = unquoted.to_string(),
                "xserverargs" => config.xserver_args = split_words(unquoted)?,
                "greeteruser" => config.greeter_user = unquoted.to_string(),
                "greeterprogram" => config.greeter_program = unquoted.to_string(),
                "greeterargs" => config.greeter_args = split_words(unquoted)?,
                "oobeprogram" => config.oobe_program = unquoted.to_string(),
                "oobeargs" => config.oobe_args = split_words(unquoted)?,
                "sessiondir" => config.session_dir = PathBuf::from(unquoted),
                "defaultsession" => config.default_session = unquoted.to_string(),
                "allowroot" => config.allow_root = parse_bool(unquoted, number + 1)?,
                "minimumuid" => config.minimum_uid = parse_u32(unquoted, number + 1)?,
                "maxfailures" => config.max_failures = parse_u32(unquoted, number + 1)?,
                "lockoutseconds" => config.lockout_seconds = parse_u64(unquoted, number + 1)?,
                "failuredelayms" => config.failure_delay_ms = parse_u64(unquoted, number + 1)?,
                "allowpower" => config.allow_power = parse_bool(unquoted, number + 1)?,
                "autologin" => config.auto_login = parse_bool(unquoted, number + 1)?,
                "autologinuser" => config.auto_login_user = unquoted.to_string(),
                "wallpaper" => config.wallpaper = unquoted.to_string(),
                "runtimedir" => config.runtime_dir = PathBuf::from(unquoted),
                "statedir" => config.state_dir = PathBuf::from(unquoted),
                "logdir" => config.log_dir = PathBuf::from(unquoted),
                "oobemarker" => config.oobe_marker = PathBuf::from(unquoted),
                "xorglog" => config.xorg_log = PathBuf::from(unquoted),
                _ => return Err(format!("line {}: unknown key {key}", number + 1)),
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_display(&self.display) {
            return Err(format!("invalid Display {:?}; expected :N", self.display));
        }
        if !self.xserver.starts_with('/') {
            return Err("Xserver must be an absolute path".to_string());
        }
        if !self.greeter_program.starts_with('/') || !self.oobe_program.starts_with('/') {
            return Err("GreeterProgram/OobeProgram must be absolute paths".to_string());
        }
        if !is_valid_account_name(&self.greeter_user) || self.greeter_user == "root" {
            return Err("GreeterUser is not a valid unprivileged account name".to_string());
        }
        if self.minimum_uid < 1000 {
            return Err(
                "MinimumUid must be >= 1000; root GUI login is controlled by AllowRoot".to_string(),
            );
        }
        if !(1..=100).contains(&self.max_failures) {
            return Err("MaxFailures must be in 1..=100".to_string());
        }
        if self.lockout_seconds > 86_400 {
            return Err("LockoutSeconds must be <= 86400".to_string());
        }
        if self.failure_delay_ms > 10_000 {
            return Err("FailureDelayMs must be <= 10000".to_string());
        }
        if self.auto_login && self.auto_login_user.is_empty() {
            return Err("AutoLogin=true requires AutoLoginUser".to_string());
        }
        if !self.auto_login_user.is_empty() && !is_valid_user_name(&self.auto_login_user) {
            return Err("AutoLoginUser is not a valid account name".to_string());
        }
        if !self.session_dir.is_absolute()
            || !self.runtime_dir.is_absolute()
            || !self.state_dir.is_absolute()
            || !self.log_dir.is_absolute()
            || !self.oobe_marker.is_absolute()
            || !self.xorg_log.is_absolute()
        {
            return Err("runtime/state/log/marker paths must be absolute".to_string());
        }
        Ok(())
    }

    pub fn display_number(&self) -> u32 {
        self.display
            .trim_start_matches(':')
            .split('.')
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    pub fn x_socket(&self) -> PathBuf {
        PathBuf::from(format!("/tmp/.X11-unix/X{}", self.display_number()))
    }

    pub fn x_lock(&self) -> PathBuf {
        PathBuf::from(format!("/tmp/.X{}-lock", self.display_number()))
    }
}

pub fn is_valid_user_name(name: &str) -> bool {
    if !is_valid_account_name(name) {
        return false;
    }
    !matches!(
        name,
        "root"
            | "ddm"
            | "daemon"
            | "bin"
            | "sys"
            | "sync"
            | "games"
            | "man"
            | "lp"
            | "mail"
            | "news"
            | "uucp"
            | "proxy"
            | "www-data"
            | "backup"
            | "list"
            | "irc"
            | "nobody"
            | "messagebus"
            | "sshd"
            | "polkitd"
            | "systemd"
    )
}

/// Account-name syntax without the reserved human-name list.  Used for the
/// dedicated greeter account (`ddm`), which is intentionally a system account.
pub fn is_valid_account_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 || name.starts_with('-') {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn is_valid_display(display: &str) -> bool {
    let Some(rest) = display.strip_prefix(':') else {
        return false;
    };
    let number = rest.split('.').next().unwrap_or(rest);
    !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())
}

fn strip_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn parse_bool(value: &str, line: usize) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        _ => Err(format!("line {line}: expected boolean, got {value:?}")),
    }
}

fn parse_u32(value: &str, line: usize) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("line {line}: expected unsigned integer, got {value:?}"))
}

fn parse_u64(value: &str, line: usize) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("line {line}: expected unsigned integer, got {value:?}"))
}

/// POSIX-like word splitting for command-line arguments.  Quotes are removed;
/// backslash escapes the next character.  This is intentionally not a shell:
/// DDM never evaluates configured text through `/bin/sh`.
pub fn split_words(value: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut have_word = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            have_word = true;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(mark) = quote {
            if ch == mark {
                quote = None;
            } else {
                current.push(ch);
            }
            have_word = true;
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                have_word = true;
            }
            c if c.is_whitespace() => {
                if have_word {
                    words.push(std::mem::take(&mut current));
                    have_word = false;
                }
            }
            c => {
                current.push(c);
                have_word = true;
            }
        }
    }
    if escaped || quote.is_some() {
        return Err("unterminated escape or quote in argument list".to_string());
    }
    if have_word {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults_and_overrides() {
        let config = Config::parse(
            "[General]\nDisplay = :1\nAllowRoot = false\nXserverArgs = -novtswitch \"-nolisten tcp\"\n",
        )
        .expect("config");
        assert_eq!(config.display, ":1");
        assert_eq!(config.xserver_args, ["-novtswitch", "-nolisten tcp"]);
        assert!(!config.allow_root);
        assert_eq!(config.display_number(), 1);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::parse("[General]\nAllowR00t = true\n").is_err());
    }

    #[test]
    fn rejects_missing_autologin_user() {
        assert!(Config::parse("[General]\nAutoLogin = true\n").is_err());
    }

    #[test]
    fn validates_user_names() {
        assert!(is_valid_user_name("alice"));
        assert!(is_valid_user_name("a_b-9"));
        assert!(!is_valid_user_name("Root"));
        assert!(!is_valid_user_name("root"));
        assert!(!is_valid_user_name("9alice"));
        assert!(!is_valid_user_name("-alice"));
    }

    #[test]
    fn splits_quoted_words() {
        assert_eq!(
            split_words("a 'b c' \"d e\" f\\ g").unwrap(),
            ["a", "b c", "d e", "f g"]
        );
        assert!(split_words("'unterminated").is_err());
    }
}
