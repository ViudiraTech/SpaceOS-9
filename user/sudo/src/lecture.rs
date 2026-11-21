//
//       user/sudo/src/lecture.rs
//       First lecture, first successful authorization and password prompts
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::io;

use crate::config::{Defaults, LectureMode};
use crate::paths::Paths;
use crate::util::{check_secure_file, ensure_dir, remove_if_exists, write_private};

pub const LECTURE_EN: &str = "\
We trust you have received the usual lecture from the local System Administrator.
It usually boils down to these three things:

    #1) Respect the privacy of others.
    #2) Think before you type.
    #3) With great power comes great responsibility.
";

pub const LECTURE_ZH: &str = "\
我们信任您已经从系统管理员那里了解了日常注意事项。
总结起来无外乎这三点：

    #1) 尊重别人的隐私。
    #2) 输入前要先考虑（后果和风险）。
    #3) 权力越大，责任越大。
";

pub fn lang_is_zh() -> bool {
    for variable in ["SPK_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(variable) {
            if value.starts_with("zh") {
                return true;
            }
            if !value.is_empty() {
                return false;
            }
        }
    }
    false
}

pub fn lecture_text(defaults: &Defaults) -> io::Result<String> {
    if let Some(path) = &defaults.lecture_file {
        let path = std::path::Path::new(path);
        check_secure_file(path, 0o644)?;
        return fs::read_to_string(path);
    }
    Ok(if lang_is_zh() {
        LECTURE_ZH.to_string()
    } else {
        LECTURE_EN.to_string()
    })
}

pub fn password_prompt() -> &'static str {
    if lang_is_zh() {
        "为安全起见，您键入的密码将不可见。\n\n密码："
    } else {
        "Password: "
    }
}

pub fn needs_lecture(paths: &Paths, uid: u32, mode: LectureMode) -> bool {
    match mode {
        LectureMode::Never => false,
        LectureMode::Always => true,
        LectureMode::Once => !paths.lectured(uid).exists(),
    }
}

pub fn mark_lectured(paths: &Paths, uid: u32) -> io::Result<()> {
    ensure_dir(&paths.lectured_dir(), 0o700)?;
    write_private(&paths.lectured(uid), b"lectured\n", 0o600)
}

pub fn needs_first_auth(paths: &Paths, uid: u32) -> bool {
    !paths
        .var
        .join("lib/sudo/first-auth")
        .join(uid.to_string())
        .exists()
}

pub fn mark_first_auth(paths: &Paths, uid: u32) -> io::Result<()> {
    let dir = paths.var.join("lib/sudo/first-auth");
    ensure_dir(&dir, 0o700)?;
    write_private(&dir.join(uid.to_string()), b"authorized\n", 0o600)
}

pub fn first_auth_text(empty_password: bool) -> String {
    if lang_is_zh() {
        let mut text = String::from(":: 首次授权成功。此提示仅显示一次。\n");
        if empty_password {
            text.push_str("警告：该账户当前使用空密码，请运行 passwd 设置密码。\n");
        }
        text
    } else {
        let mut text =
            String::from(":: First successful authorization. This notice is shown once.\n");
        if empty_password {
            text.push_str("warning: this account has an empty password; set one with passwd.\n");
        }
        text
    }
}

pub fn clear_first_auth(paths: &Paths, uid: u32) -> io::Result<()> {
    remove_if_exists(&paths.var.join("lib/sudo/first-auth").join(uid.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lecture_texts_are_the_plan_baseline() {
        assert!(LECTURE_EN.contains("#1) Respect the privacy of others."));
        assert!(LECTURE_ZH.contains("#1) 尊重别人的隐私。"));
        assert!(LECTURE_EN.contains("#3) With great power comes great responsibility."));
        assert!(LECTURE_ZH.contains("#3) 权力越大，责任越大。"));
    }

    #[test]
    fn lecture_and_first_auth_markers() {
        let dir = tempdir().unwrap();
        let paths = Paths::rooted(dir.path());
        assert!(needs_lecture(&paths, 1000, LectureMode::Once));
        assert!(needs_first_auth(&paths, 1000));
        mark_lectured(&paths, 1000).unwrap();
        mark_first_auth(&paths, 1000).unwrap();
        assert!(!needs_lecture(&paths, 1000, LectureMode::Once));
        assert!(!needs_first_auth(&paths, 1000));
        assert!(needs_lecture(&paths, 1000, LectureMode::Always));
        assert!(!needs_lecture(&paths, 1000, LectureMode::Never));
        assert!(first_auth_text(true).contains("空密码"));
        clear_first_auth(&paths, 1000).unwrap();
        assert!(needs_first_auth(&paths, 1000));
    }
}
