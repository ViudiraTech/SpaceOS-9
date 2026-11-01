/*
 *
 *       user/desktop/src/ddm/sessions.rs
 *       XDG session desktop-file discovery for DDM
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Session entries live in root-owned `/usr/share/xsessions/*.desktop`.
//! DDM never accepts a command from the greeter: the greeter sends a session
//! ID, and the daemon resolves it against this list.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub desktop_names: String,
    pub path: PathBuf,
}

pub fn discover(directory: &Path) -> Vec<Session> {
    let mut sessions = Vec::new();
    let Ok(entries) = fs::read_dir(directory) else {
        return fallback();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(id) = file_name.strip_suffix(".desktop") else {
            continue;
        };
        if id.is_empty() || id.contains('/') {
            continue;
        }
        if let Some(session) = parse_session(&path, id) {
            sessions.push(session);
        }
    }
    sessions.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    if sessions.is_empty() {
        fallback()
    } else {
        sessions
    }
}

pub fn find<'a>(sessions: &'a [Session], id: &str) -> Option<&'a Session> {
    sessions.iter().find(|session| session.id == id)
}

fn fallback() -> Vec<Session> {
    vec![Session {
        id: "spui".to_string(),
        name: "SpaceOS 桌面".to_string(),
        exec: "/usr/share/spui/session.sh".to_string(),
        desktop_names: "SPUI".to_string(),
        path: PathBuf::from("/usr/share/xsessions/spui.desktop"),
    }]
}

fn parse_session(path: &Path, id: &str) -> Option<Session> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut name = String::new();
    let mut localized = String::new();
    let mut exec = String::new();
    let mut try_exec = String::new();
    let mut desktop_names = String::new();
    let mut hidden = false;
    let mut no_display = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_desktop_entry = line.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" => name = value.to_string(),
            "Name[zh_CN]" | "Name[zh_CN.UTF-8]" => localized = value.to_string(),
            "Exec" => exec = value.to_string(),
            "TryExec" => try_exec = value.to_string(),
            "DesktopNames" => desktop_names = value.to_string(),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    if hidden || no_display {
        return None;
    }
    if exec.trim().is_empty() || exec.contains('\0') || exec.contains('\n') {
        return None;
    }
    if !try_exec.trim().is_empty() && !executable_exists(try_exec.trim()) {
        return None;
    }
    let name = if !localized.trim().is_empty() {
        localized
    } else if !name.trim().is_empty() {
        name
    } else {
        id.to_string()
    };
    Some(Session {
        id: id.to_string(),
        name,
        exec: strip_field_codes(&exec),
        desktop_names,
        path: path.to_path_buf(),
    })
}

fn executable_exists(value: &str) -> bool {
    if value.starts_with('/') {
        return Path::new(value).is_file();
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|directory| directory.join(value).is_file()))
        .unwrap_or(false)
}

/// XDG desktop files may contain `%f`, `%U` and friends.  DDM has no document
/// to pass, so remove the field code rather than handing it to a shell.
fn strip_field_codes(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(next) = chars.peek() {
                if next.is_ascii_alphanumeric() || *next == '%' {
                    chars.next();
                    continue;
                }
            }
        }
        output.push(ch);
    }
    output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_field_codes() {
        assert_eq!(strip_field_codes("spui shell %U"), "spui shell");
        assert_eq!(strip_field_codes("echo 100%%"), "echo 100");
    }

    #[test]
    fn validates_executable() {
        assert!(executable_exists("/bin/sh"));
        assert!(!executable_exists("/definitely/missing"));
    }
}
