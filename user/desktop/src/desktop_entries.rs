/*
 *
 *       user/desktop/src/desktop_entries.rs
 *       Freedesktop .desktop entry parser, built-in app registry and live hot-reload fingerprint
 *
 *       2026/11/22 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Discovers installed applications from built-in SPUI applets and standard
//! Freedesktop `.desktop` files under `/usr/share/applications/` and
//! `~/.local/share/applications/`, so packaged apps appear automatically on
//! installation and disappear on removal.

use crate::icons::Icon;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const SYSTEM_APPLICATIONS_DIR: &str = "/usr/share/applications";
pub const APPS_RELOAD_STAMP: &str = "/run/spui-apps.stamp";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCategory {
    System,
    Productivity,
    Media,
    Games,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppLaunch {
    Builtin(&'static str),
    External { exec: Vec<String> },
}

#[derive(Clone, Debug)]
pub struct ResolvedApp {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub keywords: String,
    pub icon: Icon,
    pub category: AppCategory,
    pub launch: AppLaunch,
    pub desktop_shortcut: bool,
}

struct BuiltinSpec {
    id: &'static str,
    name: &'static str,
    subtitle: &'static str,
    keywords: &'static str,
    icon: Icon,
    category: AppCategory,
    external_bin: Option<&'static str>,
}

const BUILTIN_SPECS: [BuiltinSpec; 11] = [
    BuiltinSpec {
        id: "files",
        name: "文件管理器",
        subtitle: "浏览目录与系统文件",
        keywords: "files folder directory dolphin 文件 目录",
        icon: Icon::Files,
        category: AppCategory::Productivity,
        external_bin: None,
    },
    BuiltinSpec {
        id: "term",
        name: "终端",
        subtitle: "交互式命令行会话",
        keywords: "term terminal shell sh bash console 终端 命令行",
        icon: Icon::Terminal,
        category: AppCategory::System,
        external_bin: None,
    },
    BuiltinSpec {
        id: "monitor",
        name: "系统监视器",
        subtitle: "CPU、内存、进程性能监控与管理",
        keywords: "monitor taskmgr top htop task manager 监视器 性能 进程 任务管理器",
        icon: Icon::Monitor,
        category: AppCategory::System,
        external_bin: None,
    },
    BuiltinSpec {
        id: "edit",
        name: "文本编辑器",
        subtitle: "查看与编辑文本文档",
        keywords: "edit editor text notepad kwrite 文本 编辑器",
        icon: Icon::Editor,
        category: AppCategory::Productivity,
        external_bin: None,
    },
    BuiltinSpec {
        id: "calc",
        name: "计算器",
        subtitle: "标准四则运算与即时数值计算",
        keywords: "calc calculator math 计算器 计算 数学",
        icon: Icon::Calculator,
        category: AppCategory::Productivity,
        external_bin: None,
    },
    BuiltinSpec {
        id: "mines",
        name: "扫雷",
        subtitle: "经典扫雷 · AI 约束推演与概率预测",
        keywords: "mines minesweeper game ai 扫雷 排雷 游戏 益智",
        icon: Icon::Minesweeper,
        category: AppCategory::Games,
        external_bin: None,
    },
    BuiltinSpec {
        id: "view",
        name: "看图工具",
        subtitle: "图像浏览、缩放与壁纸设定",
        keywords: "view image viewer photo picture 看图 图片 图像 相册",
        icon: Icon::ImageViewer,
        category: AppCategory::Media,
        external_bin: None,
    },
    BuiltinSpec {
        id: "music",
        name: "音乐播放器",
        subtitle: "播放示例 MP3 曲目",
        keywords: "music mp3 audio player media 音乐 播放器 音频 歌曲",
        icon: Icon::Music,
        category: AppCategory::Media,
        external_bin: None,
    },
    BuiltinSpec {
        id: "settings",
        name: "系统设置",
        subtitle: "外观、网络、桌面、输入设备与系统偏好",
        keywords: "settings network ip dhcp ethernet display wallpaper theme 外观 网络 壁纸 分辨率 设置",
        icon: Icon::Settings,
        category: AppCategory::System,
        external_bin: None,
    },
    BuiltinSpec {
        id: "help",
        name: "帮助手册",
        subtitle: "SpaceOS-9 桌面使用与快捷键指南",
        keywords: "help manual guide doc 帮助 手册 快捷键 文档 指南",
        icon: Icon::Help,
        category: AppCategory::System,
        external_bin: None,
    },
    BuiltinSpec {
        id: "browser",
        name: "Space 浏览器",
        subtitle: "网页浏览与 QuickJS 交互引擎",
        keywords: "browser web html internet js space 浏览器 网页 上网",
        icon: Icon::Browser,
        category: AppCategory::Productivity,
        external_bin: Some("space-browser"),
    },
];

pub fn resolve_binary(program: &str) -> Option<PathBuf> {
    let trimmed = program.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() || trimmed.contains('/') {
        if is_executable_file(candidate) {
            return Some(candidate.to_path_buf());
        }
        if candidate.is_absolute() {
            if let Ok(root) = std::env::var("SPUI_SYSROOT") {
                let rt = root.trim();
                if !rt.is_empty() {
                    let mapped = Path::new(rt.trim_end_matches('/'))
                        .join(trimmed.trim_start_matches('/'));
                    if is_executable_file(&mapped) {
                        return Some(mapped);
                    }
                }
            }
        }
        return None;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(trimmed);
            if is_executable_file(&sibling) {
                return Some(sibling);
            }
        }
    }
    for dir in ["/usr/bin", "/bin", "/usr/local/bin"] {
        let p = Path::new(dir).join(trimmed);
        if is_executable_file(&p) {
            return Some(p);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn builtin_apps() -> Vec<ResolvedApp> {
    let mut apps = Vec::new();
    for spec in &BUILTIN_SPECS {
        let launch = if let Some(bin) = spec.external_bin {
            let Some(path) = resolve_binary(bin) else {
                continue;
            };
            AppLaunch::External {
                exec: vec![path.to_string_lossy().into_owned()],
            }
        } else {
            AppLaunch::Builtin(spec.id)
        };
        apps.push(ResolvedApp {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            subtitle: spec.subtitle.to_string(),
            keywords: spec.keywords.to_string(),
            icon: spec.icon,
            category: spec.category,
            launch,
            desktop_shortcut: false,
        });
    }
    apps
}

fn parse_exec_tokens(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {
                if ch == '\\' {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                } else {
                    cur.push(ch);
                }
            }
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            None if ch == '\\' => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
        .into_iter()
        .filter(|tok| {
            !matches!(
                tok.as_str(),
                "%u" | "%U" | "%f" | "%F" | "%i" | "%c" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m"
            )
        })
        .collect()
}

fn icon_from_desktop_key(raw: &str) -> Icon {
    let key = raw.trim();
    if let Some(icon) = Icon::from_prop_name(key) {
        return icon;
    }
    match key.to_ascii_lowercase().as_str() {
        "store" | "space-store" | "system-software-install" => Icon::Store,
        "store-check" | "system-software-update" => Icon::StoreCheck,
        "browser" | "space-browser" | "web-browser" => Icon::Browser,
        "shield" | "dialog-password" | "security-high" => Icon::Shield,
        "utilities-terminal" | "terminal" => Icon::Terminal,
        "system-file-manager" | "files" => Icon::Files,
        "accessories-text-editor" | "editor" => Icon::Editor,
        "document" | "space-editor" | "x-office-document"
        | "application-vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Icon::Document
        }
        "preferences-system" | "settings" => Icon::Settings,
        "utilities-system-monitor" | "monitor" => Icon::Monitor,
        "accessories-calculator" | "calculator" => Icon::Calculator,
        "gwenview" | "image-viewer" => Icon::ImageViewer,
        "audio-x-generic" | "multimedia-player" | "music" => Icon::Music,
        "help-browser" | "help" => Icon::Help,
        "preferences-desktop-theme" | "preferences-desktop-theme-global" => Icon::Appearance,
        _ => Icon::Applications,
    }
}

fn category_from_desktop_keys(raw: &str) -> AppCategory {
    for part in raw.split(';') {
        match part.trim() {
            "Game" => return AppCategory::Games,
            "AudioVideo" | "Audio" | "Video" | "Graphics" | "Photography" => {
                return AppCategory::Media
            }
            "Office" | "Development" | "Network" | "Utility" | "TextEditor" => {
                return AppCategory::Productivity
            }
            _ => {}
        }
    }
    AppCategory::System
}

fn parse_bool(raw: &str) -> bool {
    matches!(raw.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

/// Parse one Freedesktop `[Desktop Entry]` file into a `ResolvedApp`.
/// Skips non-applications, `NoDisplay=true`/`Hidden=true`, or missing binaries
/// when `check_exec_exists` is true.
pub fn parse_desktop_entry(id: &str, content: &str, check_exec_exists: bool) -> Option<ResolvedApp> {
    let mut in_main = false;
    let mut seen_main = false;
    let mut fields: BTreeMap<String, String> = BTreeMap::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let section = &line[1..line.len() - 1];
            if section == "Desktop Entry" {
                in_main = true;
                seen_main = true;
            } else if seen_main {
                break;
            } else {
                in_main = false;
            }
            continue;
        }
        if !in_main {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            fields.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    if fields.get("Type").map(String::as_str) != Some("Application") {
        return None;
    }
    if fields.get("NoDisplay").is_some_and(|v| parse_bool(v))
        || fields.get("Hidden").is_some_and(|v| parse_bool(v))
    {
        return None;
    }

    let exec_raw = fields.get("Exec")?;
    let mut exec = parse_exec_tokens(exec_raw);
    if exec.is_empty() {
        return None;
    }

    if check_exec_exists {
        if let Some(try_exec) = fields.get("TryExec").filter(|s| !s.is_empty()) {
            resolve_binary(try_exec)?;
        }
        let resolved = resolve_binary(&exec[0])?;
        exec[0] = resolved.to_string_lossy().into_owned();
    }

    let name = fields
        .get("Name[zh_CN]")
        .or_else(|| fields.get("Name[zh]"))
        .or_else(|| fields.get("Name"))
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| id.to_string());

    let subtitle = fields
        .get("Comment[zh_CN]")
        .or_else(|| fields.get("Comment[zh]"))
        .or_else(|| fields.get("Comment"))
        .or_else(|| fields.get("GenericName[zh_CN]"))
        .or_else(|| fields.get("GenericName"))
        .cloned()
        .unwrap_or_default();

    let mut kw_parts = vec![name.clone(), subtitle.clone(), id.to_string()];
    for key in ["Keywords[zh_CN]", "Keywords", "GenericName[zh_CN]", "GenericName", "Name"] {
        if let Some(v) = fields.get(key) {
            kw_parts.push(v.replace(';', " "));
        }
    }
    let keywords = kw_parts.join(" ");

    let icon = fields
        .get("Icon")
        .map(|s| icon_from_desktop_key(s))
        .unwrap_or(Icon::Applications);
    let category = fields
        .get("Categories")
        .map(|s| category_from_desktop_keys(s))
        .unwrap_or(AppCategory::System);
    let desktop_shortcut = fields
        .get("X-SpaceOS-DesktopShortcut")
        .is_some_and(|v| parse_bool(v));

    Some(ResolvedApp {
        id: id.to_string(),
        name,
        subtitle,
        keywords,
        icon,
        category,
        launch: AppLaunch::External { exec },
        desktop_shortcut,
    })
}

fn system_applications_dir() -> PathBuf {
    if let Ok(root) = std::env::var("SPUI_SYSROOT") {
        let trimmed = root.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed.trim_end_matches('/'))
                .join(SYSTEM_APPLICATIONS_DIR.trim_start_matches('/'));
        }
    }
    PathBuf::from(SYSTEM_APPLICATIONS_DIR)
}

fn user_applications_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".local/share/applications"))
}

pub fn discover_applications() -> Vec<ResolvedApp> {
    let dirs: Vec<PathBuf> = [Some(system_applications_dir()), user_applications_dir()]
        .into_iter()
        .flatten()
        .collect();
    discover_applications_in(&dirs, true)
}

pub fn discover_applications_in(dirs: &[PathBuf], check_exec_exists: bool) -> Vec<ResolvedApp> {
    let mut apps = builtin_apps();
    let existing_ids: BTreeSet<String> = apps.iter().map(|a| a.id.clone()).collect();
    let mut from_disk: BTreeMap<String, ResolvedApp> = BTreeMap::new();

    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
            .collect();
        files.sort();
        for path in files {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let id = stem.to_string();
            if existing_ids.contains(&id) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(app) = parse_desktop_entry(&id, &text, check_exec_exists) {
                from_disk.insert(id, app);
            }
        }
    }

    apps.extend(from_disk.into_values());
    apps
}

fn hash_bytes(state: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *state ^= u64::from(b);
        *state = state.wrapping_mul(0x1000_0000_01b3);
    }
}

pub fn applications_fingerprint() -> u64 {
    let dirs: Vec<PathBuf> = [Some(system_applications_dir()), user_applications_dir()]
        .into_iter()
        .flatten()
        .collect();
    applications_fingerprint_in(&dirs, Some(Path::new(APPS_RELOAD_STAMP)))
}

pub fn applications_fingerprint_in(dirs: &[PathBuf], stamp_path: Option<&Path>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    if let Some(stamp) = stamp_path {
        if let Ok(raw) = fs::read(stamp) {
            hash_bytes(&mut hash, &raw);
        }
    }
    hash_bytes(
        &mut hash,
        if resolve_binary("space-browser").is_some() {
            b"browser:1"
        } else {
            b"browser:0"
        },
    );
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.ends_with(".desktop") {
                continue;
            }
            hash_bytes(&mut hash, name.as_bytes());
            if let Ok(meta) = fs::metadata(&path) {
                hash_bytes(&mut hash, &meta.len().to_le_bytes());
                let secs = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                hash_bytes(&mut hash, &secs.to_le_bytes());
            }
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zh_desktop_entry_and_shortcut_flag() {
        let desktop = "\
[Desktop Entry]
Type=Application
Name=Space Store
Name[zh_CN]=Space 商店
Comment[zh_CN]=浏览与安装软件包
Exec=/bin/sh %U
TryExec=/bin/sh
Icon=store
Categories=System;PackageManager;
X-SpaceOS-DesktopShortcut=true
";
        let app = parse_desktop_entry("space-store", desktop, true).expect("must parse");
        assert_eq!(app.id, "space-store");
        assert_eq!(app.name, "Space 商店");
        assert_eq!(app.subtitle, "浏览与安装软件包");
        assert_eq!(app.icon, Icon::Store);
        assert!(app.desktop_shortcut);
        assert_eq!(
            app.launch,
            AppLaunch::External {
                exec: vec!["/bin/sh".into()]
            }
        );
    }

    #[test]
    fn skips_nodisplay_and_missing_executable_entries() {
        let hidden = "\
[Desktop Entry]
Type=Application
Name=Theme Helper
Exec=/bin/sh
NoDisplay=true
";
        assert!(parse_desktop_entry("helper", hidden, false).is_none());

        let uninstalled = "\
[Desktop Entry]
Type=Application
Name=Space 商店
Exec=/nonexistent/space-store-missing
TryExec=/nonexistent/space-store-missing
X-SpaceOS-DesktopShortcut=true
";
        assert!(parse_desktop_entry("space-store", uninstalled, true).is_none());
    }

    struct TempDirGuard(PathBuf);
    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            let pid = std::process::id();
            let seq = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("{prefix}_{pid}_{seq}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fingerprint_changes_when_desktop_file_added_or_removed() {
        let dir = TempDirGuard::new("spui_desktop_fp");
        let dir_path = dir.path().to_path_buf();
        let before = applications_fingerprint_in(std::slice::from_ref(&dir_path), None);
        assert!(discover_applications_in(std::slice::from_ref(&dir_path), false)
            .iter()
            .all(|a| a.id != "sample-app"));

        let file = dir.path().join("sample-app.desktop");
        fs::write(
            &file,
            "[Desktop Entry]\nType=Application\nName=示例\nExec=/bin/sh\nX-SpaceOS-DesktopShortcut=true\n",
        )
        .unwrap();
        let after_add = applications_fingerprint_in(std::slice::from_ref(&dir_path), None);
        assert_ne!(before, after_add);
        assert!(discover_applications_in(std::slice::from_ref(&dir_path), true)
            .iter()
            .any(|a| a.id == "sample-app" && a.desktop_shortcut));

        fs::remove_file(&file).unwrap();
        let after_del = applications_fingerprint_in(std::slice::from_ref(&dir_path), None);
        assert_eq!(before, after_del);
    }

    #[test]
    fn builtin_apps_do_not_prepopulate_store_shortcuts() {
        let apps = builtin_apps();
        assert!(!apps.iter().any(|a| a.id == "store" || a.id == "store-check"));
    }
}
