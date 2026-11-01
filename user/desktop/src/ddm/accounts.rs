/*
 *
 *       user/desktop/src/ddm/accounts.rs
 *       Atomic account, group, home-directory and first-boot transactions
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Account creation is one transaction from the administrator's point of
//! view: passwd, shadow and group are replaced atomically per file, and the
//! OOBE completion marker is written only after the home directory exists.
//! If the machine loses power halfway through, the marker is absent; the daemon
//! will not re-run OOBE once a human account exists, so an interrupted setup
//! cannot be turned into an account-takeover screen.

use crate::ddm::auth::{days_since_epoch, hash_password, valid_home, UserRecord};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub struct AccountSpec<'a> {
    pub username: &'a str,
    pub full_name: &'a str,
    pub password: &'a str,
    pub minimum_uid: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemSettings<'a> {
    pub hostname: &'a str,
    pub timezone: &'a str,
    pub locale: &'a str,
    pub keyboard: &'a str,
    pub auto_login: bool,
}

pub fn create_user(
    spec: AccountSpec<'_>,
    settings: &SystemSettings<'_>,
) -> Result<UserRecord, String> {
    if crate::ddm::config::is_valid_user_name(spec.username) == false {
        return Err("用户名不合法".to_string());
    }
    if !valid_full_name(spec.full_name) {
        return Err("显示名称不合法".to_string());
    }
    if !valid_hostname(settings.hostname) {
        return Err("主机名不合法".to_string());
    }
    let existing = crate::ddm::auth::lookup_user(spec.username);
    if existing.is_some() {
        return Err("该用户名已存在".to_string());
    }
    let passwd = read_database("/etc/passwd")?;
    let shadow = read_database("/etc/shadow")?;
    let mut group = read_database("/etc/group")?;
    let uid = next_id(&passwd, spec.minimum_uid).ok_or("无法分配 UID")?;
    let gid = next_gid(&group, uid).ok_or("无法分配 GID")?;
    let home = format!("/home/{}", spec.username);
    let shell = "/bin/sh";
    let full_name = sanitize_gecos(spec.full_name);
    let hash = hash_password(spec.password.as_bytes())?;

    let passwd_line = format!(
        "{}:x:{}:{}:{}:{}:{}\n",
        spec.username, uid, gid, full_name, home, shell
    );
    let shadow_line = format!(
        "{}:{}:{}:0:99999:7:-1:-1::\n",
        spec.username,
        hash,
        days_since_epoch().max(1)
    );
    let group_line = format!("{}:x:{}:\n", spec.username, gid);

    ensure_group(&mut group, "wheel", 10)?;
    ensure_group(&mut group, "video", 27)?;
    ensure_group(&mut group, "input", 97)?;

    let new_passwd = append_unique(passwd, &passwd_line, spec.username);
    let new_shadow = append_unique(shadow, &shadow_line, spec.username);
    let mut new_group = append_unique(group, &group_line, spec.username);
    add_member(&mut new_group, "wheel", spec.username);
    add_member(&mut new_group, "video", spec.username);
    add_member(&mut new_group, "input", spec.username);

    // Apply system settings first.  If a setting cannot be written, no account
    // is published, so retrying the wizard starts from a clean state.
    apply_settings(settings)?;

    // Create the home directory before publishing the account.  If this fails
    // the databases remain untouched.
    create_home(&home, uid, gid)?;

    // Publish shadow and group first, and passwd last.  A crash before the
    // passwd rename leaves no visible account, so a retried OOBE can use the
    // same name safely.  The reverse order would create a login-less account.
    if let Err(error) = write_database("/etc/shadow", &new_shadow, 0o600) {
        return Err(format!("写入 /etc/shadow 失败: {error}"));
    }
    if let Err(error) = write_database("/etc/group", &new_group, 0o644) {
        return Err(format!("写入 /etc/group 失败: {error}"));
    }
    if let Err(error) = write_database("/etc/passwd", &new_passwd, 0o644) {
        return Err(format!("写入 /etc/passwd 失败: {error}"));
    }

    let user = UserRecord {
        name: spec.username.to_string(),
        uid,
        gid,
        gecos: full_name,
        home,
        shell: shell.to_string(),
    };
    Ok(user)
}

/// Remove every human account and its home directory, then leave the marker
/// state that makes DDM launch OOBE on the next start.  A progress marker makes
/// the operation resumable if power is lost after the database update but before
/// the home directories are gone.
pub fn factory_reset() -> Result<Vec<String>, String> {
    let progress = Path::new("/var/lib/spaceos/factory-reset-in-progress");
    write_secure(progress, b"in-progress\n", 0o600)?;

    let passwd = read_database("/etc/passwd")?;
    let shadow = read_database("/etc/shadow")?;
    let group = read_database("/etc/group")?;

    let mut humans = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for line in &passwd {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 4 {
            continue;
        }
        if let Ok(uid) = fields[2].parse::<u32>() {
            if uid >= 1000 && uid != u32::MAX {
                names.insert(fields[0].to_string());
                humans.push(fields[0].to_string());
            }
        }
    }

    let new_shadow: Vec<String> = shadow
        .into_iter()
        .filter(|line| {
            line.split(':')
                .next()
                .map(|name| !names.contains(name))
                .unwrap_or(true)
        })
        .collect();
    let mut new_group = Vec::new();
    for line in group {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 3 {
            new_group.push(line);
            continue;
        }
        if fields[2]
            .parse::<u32>()
            .is_ok_and(|gid| gid >= 1000 && gid != u32::MAX)
        {
            continue;
        }
        let mut fields: Vec<String> = fields.iter().map(|field| field.to_string()).collect();
        while fields.len() < 4 {
            fields.push(String::new());
        }
        fields[3] = fields[3]
            .split(',')
            .filter(|member| !member.is_empty() && !names.contains(*member))
            .collect::<Vec<_>>()
            .join(",");
        new_group.push(fields.join(":"));
    }
    let new_passwd: Vec<String> = passwd
        .into_iter()
        .filter(|line| {
            line.split(':')
                .next()
                .map(|name| !names.contains(name))
                .unwrap_or(true)
        })
        .collect();

    // Publish the account removal before deleting homes.  If this is interrupted,
    // the progress marker makes the next boot retry the cleanup.
    write_database("/etc/shadow", &new_shadow, 0o600)?;
    write_database("/etc/group", &new_group, 0o644)?;
    write_database("/etc/passwd", &new_passwd, 0o644)?;

    for name in &humans {
        if !crate::ddm::config::is_valid_account_name(name) {
            continue;
        }
        let home = Path::new("/home").join(name);
        if let Ok(metadata) = fs::symlink_metadata(&home) {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                let _ = fs::remove_dir_all(&home);
            }
        }
    }
    // Also remove orphan homes left by an older interrupted transaction.
    if let Ok(entries) = fs::read_dir("/home") {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(metadata) = fs::symlink_metadata(&path) {
                use std::os::unix::fs::MetadataExt;
                if metadata.is_dir() && !metadata.file_type().is_symlink() && metadata.uid() >= 1000
                {
                    let _ = fs::remove_dir_all(&path);
                }
            }
        }
    }

    let _ = fs::remove_file("/var/lib/spaceos/oobe-complete");
    let _ = fs::remove_file("/var/lib/spaceos/factory-reset");
    let _ = fs::remove_file("/var/lib/ddm/state");
    if let Ok(entries) = fs::read_dir("/var/lib/uxconf/channels") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let _ = fs::remove_dir_all(&path);
            } else {
                let _ = fs::remove_file(&path);
            }
        }
    }
    let _ = fs::remove_file(progress);
    Ok(names.into_iter().collect())
}

pub fn mark_oobe_complete(username: &str) -> Result<(), String> {
    let directory = Path::new("/var/lib/spaceos");
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = directory.join("oobe-complete");
    let content = format!(
        "version=1\ncompleted_by={}\ncompleted_at={}\n",
        username,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    );
    write_secure(&path, content.as_bytes(), 0o644)
}

pub fn apply_settings(settings: &SystemSettings<'_>) -> Result<(), String> {
    if valid_hostname(settings.hostname) {
        write_secure(
            Path::new("/etc/hostname"),
            settings.hostname.as_bytes(),
            0o644,
        )?;
        let hostname = std::ffi::CString::new(settings.hostname)
            .map_err(|_| "hostname contains NUL".to_string())?;
        let result = unsafe { libc::sethostname(hostname.as_ptr(), settings.hostname.len()) };
        if result != 0 {
            // The file is still correct for the next boot; do not fail setup.
            eprintln!("ddm: sethostname failed: {}", io::Error::last_os_error());
        }
    }

    if is_safe_zone(settings.timezone) {
        let zone = format!("/usr/share/zoneinfo/{}", settings.timezone);
        if Path::new(&zone).exists() {
            let _ = fs::remove_file("/etc/localtime");
            let _ = std::os::unix::fs::symlink(&zone, "/etc/localtime");
            let posix = uxconf::posix_tz_for_zone(settings.timezone);
            let _ = write_secure(Path::new("/etc/TZ"), format!("{posix}\n").as_bytes(), 0o644);
            let _ = write_secure(
                Path::new("/etc/timezone"),
                format!("{}\n", settings.timezone).as_bytes(),
                0o644,
            );
        }
    }

    let locale = if settings.locale == "en_US" {
        "en_US.UTF-8"
    } else {
        "zh_CN.UTF-8"
    };
    write_secure(
        Path::new("/etc/locale.conf"),
        format!("LANG={locale}\n").as_bytes(),
        0o644,
    )?;
    write_secure(
        Path::new("/etc/profile"),
        format!(
            "export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin\n\
             export TERM=${{TERM:-xterm-256color}}\n\
             export COLORTERM=truecolor\n\
             export LANG={locale}\n\
             export PS1='\\u@\\h:\\w\\$ '\n"
        )
        .as_bytes(),
        0o644,
    )?;

    if is_safe_keyboard(settings.keyboard) {
        write_secure(
            Path::new("/etc/X11/xorg.conf.d/20-input.conf"),
            format!(
                "Section \"InputClass\"\n    Identifier \"keyboard\"\n    MatchIsKeyboard \"on\"\n    Driver \"libinput\"\n    Option \"XkbLayout\" \"{}\"\nEndSection\n\n\
                 Section \"InputClass\"\n    Identifier \"pointer\"\n    MatchIsPointer \"on\"\n    Driver \"libinput\"\nEndSection\n\n\
                 Section \"InputClass\"\n    Identifier \"tablet\"\n    MatchIsTablet \"on\"\n    Driver \"libinput\"\nEndSection\n",
                settings.keyboard
            )
            .as_bytes(),
            0o644,
        )?;
    }
    Ok(())
}

pub fn apply_keyboard_now(keyboard: &str, display: &str, auth_file: &Path) -> Result<(), String> {
    if !is_safe_keyboard(keyboard) {
        return Err("键盘布局不合法".to_string());
    }
    let status = std::process::Command::new("/usr/bin/setxkbmap")
        .args(["-display", display, "-layout", keyboard])
        .env("XAUTHORITY", auth_file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("setxkbmap 返回失败".to_string())
    }
}

pub fn next_id(passwd: &[String], minimum_uid: u32) -> Option<u32> {
    let mut used = std::collections::BTreeSet::new();
    for line in passwd {
        let fields: Vec<&str> = line.split(':').collect();
        if let Some(uid) = fields.get(2).and_then(|value| value.parse::<u32>().ok()) {
            used.insert(uid);
        }
    }
    (minimum_uid..=60_000).find(|uid| !used.contains(uid))
}

pub fn next_gid(group: &[String], preferred: u32) -> Option<u32> {
    let mut used = std::collections::BTreeSet::new();
    for line in group {
        let fields: Vec<&str> = line.split(':').collect();
        if let Some(gid) = fields.get(2).and_then(|value| value.parse::<u32>().ok()) {
            used.insert(gid);
        }
    }
    if preferred >= 1000 && !used.contains(&preferred) {
        return Some(preferred);
    }
    (1000..=60_000).find(|gid| !used.contains(gid))
}

fn read_database(path: &str) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
    Ok(text.lines().map(ToString::to_string).collect())
}

fn write_database(path: &str, lines: &[String], mode: u32) -> Result<(), String> {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    write_secure(Path::new(path), text.as_bytes(), mode)
}

fn append_unique(mut lines: Vec<String>, line: &str, name: &str) -> Vec<String> {
    lines.retain(|existing| {
        existing
            .split(':')
            .next()
            .map(|value| value != name)
            .unwrap_or(true)
    });
    lines.push(line.trim_end().to_string());
    lines
}

fn ensure_group(lines: &mut Vec<String>, name: &str, gid: u32) -> Result<(), String> {
    if lines
        .iter()
        .any(|line| line.split(':').next() == Some(name))
    {
        return Ok(());
    }
    lines.push(format!("{name}:x:{gid}:"));
    Ok(())
}

fn add_member(lines: &mut [String], group: &str, member: &str) {
    for line in lines.iter_mut() {
        let mut fields: Vec<String> = line.split(':').map(ToString::to_string).collect();
        if fields.first().map(String::as_str) != Some(group) {
            continue;
        }
        while fields.len() < 4 {
            fields.push(String::new());
        }
        let mut members: Vec<&str> = fields[3]
            .split(',')
            .filter(|item| !item.is_empty())
            .collect();
        if !members.contains(&member) {
            members.push(member);
            fields[3] = members.join(",");
            *line = fields.join(":");
        }
        return;
    }
}

pub fn create_home(home: &str, uid: u32, gid: u32) -> Result<(), String> {
    if !valid_home(home) {
        return Err("主目录路径不合法".to_string());
    }
    let path = Path::new(home);
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() {
            return Err("同名主目录已存在且不是目录".to_string());
        }
    } else {
        fs::create_dir_all(path).map_err(|error| format!("创建主目录失败: {error}"))?;
    }
    for relative in [
        ".config",
        ".cache",
        ".local",
        ".local/share",
        ".local/state",
    ] {
        let directory = path.join(relative);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o700));
    }
    if let Ok(entries) = fs::read_dir("/etc/skel") {
        for entry in entries.flatten() {
            let target = path.join(entry.file_name());
            if !target.exists() {
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        let _ = fs::copy(entry.path(), &target);
                        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
                    }
                }
            }
        }
    }
    chown_recursive(path, uid, gid)?;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    Ok(())
}

pub fn chown_recursive(path: &Path, uid: u32, gid: u32) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    chown(path, uid, gid)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            chown_recursive(&entry.path(), uid, gid)?;
        }
    }
    Ok(())
}

pub fn chown(path: &Path, uid: u32, gid: u32) -> Result<(), String> {
    crate::ddm::xauth::chown(path, uid, gid).map_err(|error| error.to_string())
}

pub fn write_secure(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary = path.file_name().unwrap_or_default().to_os_string();
    temporary.push(format!(".tmp.{}", std::process::id()));
    let temporary = path.with_file_name(temporary);
    let _ = fs::remove_file(&temporary);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    if let Err(error) = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

pub fn set_auto_login(state_dir: &Path, user: &str, enabled: bool) -> Result<(), String> {
    fs::create_dir_all(state_dir).map_err(|error| error.to_string())?;
    let path = state_dir.join("state");
    let mut lines: Vec<String> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with("auto_login=") && !line.starts_with("auto_login_user="))
        .map(ToString::to_string)
        .collect();
    lines.push(format!("auto_login={}", u8::from(enabled)));
    lines.push(format!("auto_login_user={user}"));
    write_secure(&path, format!("{}\n", lines.join("\n")).as_bytes(), 0o600)
}

pub fn sanitize_gecos(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| !c.is_control() && *c != ':')
        .take(64)
        .collect();
    cleaned.trim().to_string()
}

pub fn valid_full_name(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 64
        && !trimmed.chars().any(|c| c.is_control() || c == ':')
}

pub fn valid_hostname(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

pub fn is_safe_zone(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
        && !value.starts_with('/')
        && !value.contains("..")
}

pub fn is_safe_keyboard(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_first_free_ids() {
        let passwd = vec!["root:x:0:0:root:/root:/bin/sh".to_string()];
        assert_eq!(next_id(&passwd, 1000), Some(1000));
        let passwd = vec![
            "root:x:0:0:root:/root:/bin/sh".to_string(),
            "alice:x:1000:1000::/home/alice:/bin/sh".to_string(),
        ];
        assert_eq!(next_id(&passwd, 1000), Some(1001));
    }

    #[test]
    fn group_membership_is_deduplicated() {
        let mut groups = vec!["wheel:x:10:alice".to_string(), "video:x:27:".to_string()];
        add_member(&mut groups, "wheel", "alice");
        add_member(&mut groups, "wheel", "bob");
        add_member(&mut groups, "video", "alice");
        assert_eq!(groups[0], "wheel:x:10:alice,bob");
        assert_eq!(groups[1], "video:x:27:alice");
    }

    #[test]
    fn validates_system_values() {
        assert!(valid_hostname("spaceos-9"));
        assert!(!valid_hostname("-bad"));
        assert!(is_safe_zone("Asia/Shanghai"));
        assert!(!is_safe_zone("../../etc"));
        assert!(is_safe_keyboard("us"));
        assert!(!is_safe_keyboard("us;rm"));
    }
}
