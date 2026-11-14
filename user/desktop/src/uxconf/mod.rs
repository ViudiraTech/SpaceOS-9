/*
 *
 *       user/desktop/src/uxconf/mod.rs
 *       Core types, channel/property validation and typed desktop configuration model
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Hierarchical configuration library modeled after XFCE's `xfconf`.
//! Configuration entries are organized into top-level **channels** (e.g.
//! `appearance`, `desktop`, `panel`, `wm`, `display`, `input`, `terminal`,
//! `files`, `editor`, `datetime`) and slash-delimited **property paths**
//! (e.g. `/theme/mode`, `/mouse/natural-scroll`).

pub mod client;
pub mod daemon;
pub mod net;
pub mod protocol;
pub mod schema;
pub mod storage;

use std::fmt;

pub const WALLPAPER_DIR: &str = "/usr/share/spui/wallpapers";

/// Error returned by `uxconf` validation, storage or IPC operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfError {
    InvalidChannel(String),
    InvalidProperty(String),
    InvalidValue(String),
    TypeMismatch {
        expected: ValueKind,
        actual: ValueKind,
    },
    NotFound,
    Io(String),
    Protocol(String),
}

impl fmt::Display for ConfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannel(c) => write!(f, "invalid channel name: {c:?}"),
            Self::InvalidProperty(p) => write!(f, "invalid property path: {p:?}"),
            Self::InvalidValue(v) => write!(f, "invalid property value: {v}"),
            Self::TypeMismatch { expected, actual } => {
                write!(
                    f,
                    "type mismatch: expected {}, got {}",
                    expected.as_str(),
                    actual.as_str()
                )
            }
            Self::NotFound => write!(f, "property not found"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
        }
    }
}

impl std::error::Error for ConfError {}

/// Scalar value tag supported by `uxconfd`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Bool,
    Int,
    Float,
    String,
}

impl ValueKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::String => "string",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ConfError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bool" | "boolean" => Ok(Self::Bool),
            "int" | "i64" | "integer" => Ok(Self::Int),
            "float" | "f64" | "double" => Ok(Self::Float),
            "string" | "str" => Ok(Self::String),
            other => Err(ConfError::InvalidValue(format!("unknown type {other:?}"))),
        }
    }
}

/// Strongly-typed configuration property value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

impl Value {
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Bool(_) => ValueKind::Bool,
            Self::Int(_) => ValueKind::Int,
            Self::Float(_) => ValueKind::Float,
            Self::String(_) => ValueKind::String,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Parse a user-supplied string given an explicit `ValueKind`.
    pub fn parse_with_kind(kind: ValueKind, raw: &str) -> Result<Self, ConfError> {
        match kind {
            ValueKind::Bool => match raw.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Ok(Self::Bool(true)),
                "false" | "0" | "no" | "off" => Ok(Self::Bool(false)),
                _ => Err(ConfError::InvalidValue(format!("invalid boolean {raw:?}"))),
            },
            ValueKind::Int => raw
                .trim()
                .parse::<i64>()
                .map(Self::Int)
                .map_err(|_| ConfError::InvalidValue(format!("invalid integer {raw:?}"))),
            ValueKind::Float => {
                let parsed = raw
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| ConfError::InvalidValue(format!("invalid float {raw:?}")))?;
                if !parsed.is_finite() {
                    return Err(ConfError::InvalidValue("non-finite float".to_string()));
                }
                Ok(Self::Float(parsed))
            }
            ValueKind::String => Ok(Self::String(raw.to_string())),
        }
    }

    /// Infer a `Value` type from untyped CLI text when no schema entry exists.
    pub fn infer_from_str(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("true") {
            return Self::Bool(true);
        }
        if trimmed.eq_ignore_ascii_case("false") {
            return Self::Bool(false);
        }
        if let Ok(n) = trimmed.parse::<i64>() {
            return Self::Int(n);
        }
        if trimmed.contains('.') {
            if let Ok(f) = trimmed.parse::<f64>() {
                if f.is_finite() {
                    return Self::Float(f);
                }
            }
        }
        Self::String(raw.to_string())
    }

    /// Serialize into single-line wire/storage form (`type:payload`).
    pub fn encode_typed(&self) -> String {
        match self {
            Self::Bool(v) => format!("bool:{v}"),
            Self::Int(v) => format!("int:{v}"),
            Self::Float(v) => {
                let mut text = format!("{v}");
                if !text.contains('.') && !text.contains('e') && !text.contains('E') {
                    text.push_str(".0");
                }
                format!("float:{text}")
            }
            Self::String(s) => format!("string:\"{}\"", escape_string(s)),
        }
    }

    /// Deserialize from single-line wire/storage form (`type:payload`).
    pub fn decode_typed(raw: &str) -> Result<Self, ConfError> {
        let raw = raw.trim();
        let Some((tag, rest)) = raw.split_once(':') else {
            return Err(ConfError::InvalidValue(format!(
                "missing type prefix in {raw:?}"
            )));
        };
        let kind = ValueKind::parse(tag)?;
        let rest = rest.trim();
        match kind {
            ValueKind::Bool => match rest {
                "true" => Ok(Self::Bool(true)),
                "false" => Ok(Self::Bool(false)),
                _ => Err(ConfError::InvalidValue(format!(
                    "bad bool literal {rest:?}"
                ))),
            },
            ValueKind::Int => rest
                .parse::<i64>()
                .map(Self::Int)
                .map_err(|_| ConfError::InvalidValue(format!("bad int literal {rest:?}"))),
            ValueKind::Float => {
                let v = rest
                    .parse::<f64>()
                    .map_err(|_| ConfError::InvalidValue(format!("bad float literal {rest:?}")))?;
                if !v.is_finite() {
                    return Err(ConfError::InvalidValue("non-finite float".to_string()));
                }
                Ok(Self::Float(v))
            }
            ValueKind::String => {
                let inner = rest
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .ok_or_else(|| {
                        ConfError::InvalidValue(format!("unquoted string literal {rest:?}"))
                    })?;
                Ok(Self::String(unescape_string(inner)?))
            }
        }
    }

    /// Human-readable plain output for CLI queries (`uxconf-query -p ...`).
    pub fn display_plain(&self) -> String {
        match self {
            Self::Bool(v) => v.to_string(),
            Self::Int(v) => v.to_string(),
            Self::Float(v) => format!("{v}"),
            Self::String(s) => s.clone(),
        }
    }
}

fn escape_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

fn unescape_string(input: &str) -> Result<String, ConfError> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let Some(esc) = chars.next() else {
                return Err(ConfError::InvalidValue(
                    "trailing backslash in string".to_string(),
                ));
            };
            match esc {
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                other => {
                    return Err(ConfError::InvalidValue(format!(
                        "unsupported escape \\{other}"
                    )));
                }
            }
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

/// Validate and normalize a channel identifier (case-insensitive `[a-z0-9_-]`, 1..=64 bytes).
pub fn validate_channel(raw: &str) -> Result<String, ConfError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(ConfError::InvalidChannel(raw.to_string()));
    }
    let lower = trimmed.to_ascii_lowercase();
    if !lower
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(ConfError::InvalidChannel(raw.to_string()));
    }
    Ok(lower)
}

/// Validate a hierarchical property path (`/segment/...`).
/// When `allow_root` is true, `"/"` is accepted (used for subtree queries and resets).
pub fn validate_property(raw: &str, allow_root: bool) -> Result<&str, ConfError> {
    if raw.is_empty() || raw.len() > 128 || !raw.starts_with('/') {
        return Err(ConfError::InvalidProperty(raw.to_string()));
    }
    if raw == "/" {
        return if allow_root {
            Ok("/")
        } else {
            Err(ConfError::InvalidProperty(raw.to_string()))
        };
    }
    if raw.ends_with('/') {
        return Err(ConfError::InvalidProperty(raw.to_string()));
    }
    for seg in &raw.split('/').collect::<Vec<_>>()[1..] {
        if seg.is_empty() || *seg == "." || *seg == ".." {
            return Err(ConfError::InvalidProperty(raw.to_string()));
        }
        if !seg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(ConfError::InvalidProperty(raw.to_string()));
        }
    }
    Ok(raw)
}

/// Parse `#RRGGBB` or `0xRRGGBB` into packed `0x00RRGGBB`.
pub fn parse_hex_rgb(raw: &str) -> Option<u32> {
    let hex = raw
        .trim()
        .strip_prefix('#')
        .or_else(|| raw.trim().strip_prefix("0x"))
        .or_else(|| raw.trim().strip_prefix("0X"))
        .unwrap_or_else(|| raw.trim());
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().map(|v| v & 0x00ff_ffff)
}

/// Format packed `0x00RRGGBB` as `#rrggbb`.
pub fn format_hex_rgb(rgb: u32) -> String {
    format!("#{:06x}", rgb & 0x00ff_ffff)
}

/// Property change notification delivered to subscribers.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeEvent {
    pub channel: String,
    pub property: String,
    pub value: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeMode {
    Dark,
    Light,
}

impl ThemeMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("light") {
            Self::Light
        } else {
            Self::Dark
        }
    }
}

/// Visual chrome / widget geometry family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeStyle {
    Spui,
    Win98,
}

impl ThemeStyle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spui => "spui",
            Self::Win98 => "win98",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("win98") {
            Self::Win98
        } else {
            Self::Spui
        }
    }
}

/// Built-in complete desktop theme packs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemePreset {
    Spui4,
    Win98,
}

impl ThemePreset {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Spui4 => "spui-4",
            Self::Win98 => "win98",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Spui4 => "SPUI 4.0",
            Self::Win98 => "Windows 98",
        }
    }

    pub const fn subtitle(self) -> &'static str {
        match self {
            Self::Spui4 => "现代圆角纯色风格 · 深浅双色可调",
            Self::Win98 => "经典 98 立体浮雕边框 · #008080 青绿桌面",
        }
    }

    pub const fn style(self) -> ThemeStyle {
        match self {
            Self::Spui4 => ThemeStyle::Spui,
            Self::Win98 => ThemeStyle::Win98,
        }
    }

    pub const fn from_style(style: ThemeStyle) -> Self {
        match style {
            ThemeStyle::Spui => Self::Spui4,
            ThemeStyle::Win98 => Self::Win98,
        }
    }

    pub fn parse(raw: &str) -> Self {
        let lower = raw.trim().to_ascii_lowercase();
        if lower == "win98" || lower == "windows98" {
            Self::Win98
        } else {
            Self::Spui4
        }
    }

    /// Canonical `(channel, property, value)` bundle applied when switching theme packs.
    pub fn properties(self) -> Vec<(&'static str, &'static str, Value)> {
        match self {
            Self::Spui4 => vec![
                ("appearance", "/theme/id", Value::String("spui-4".to_string())),
                ("appearance", "/theme/style", Value::String("spui".to_string())),
                ("appearance", "/theme/mode", Value::String("dark".to_string())),
                (
                    "appearance",
                    "/theme/accent",
                    Value::String("#3b82f6".to_string()),
                ),
                ("appearance", "/font/scale", Value::Int(100)),
                (
                    "desktop",
                    "/backdrop/style",
                    Value::String("cover".to_string()),
                ),
                (
                    "desktop",
                    "/backdrop/wallpaper",
                    Value::String(WALLPAPERS[0].file.to_string()),
                ),
                (
                    "desktop",
                    "/backdrop/solid-color",
                    Value::String("#1e293b".to_string()),
                ),
            ],
            Self::Win98 => vec![
                ("appearance", "/theme/id", Value::String("win98".to_string())),
                ("appearance", "/theme/style", Value::String("win98".to_string())),
                ("appearance", "/theme/mode", Value::String("light".to_string())),
                (
                    "appearance",
                    "/theme/accent",
                    Value::String("#000080".to_string()),
                ),
                ("appearance", "/font/scale", Value::Int(100)),
                (
                    "desktop",
                    "/backdrop/style",
                    Value::String("solid".to_string()),
                ),
                (
                    "desktop",
                    "/backdrop/solid-color",
                    Value::String("#008080".to_string()),
                ),
            ],
        }
    }
}

pub const THEME_PRESETS: [ThemePreset; 2] = [ThemePreset::Spui4, ThemePreset::Win98];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackdropStyle {
    Cover,
    Contain,
    Stretch,
    Solid,
}

impl BackdropStyle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cover => "cover",
            Self::Contain => "contain",
            Self::Stretch => "stretch",
            Self::Solid => "solid",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "contain" => Self::Contain,
            "stretch" => Self::Stretch,
            "solid" => Self::Solid,
            _ => Self::Cover,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelPosition {
    Bottom,
    Top,
}

impl PanelPosition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Top => "top",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("top") {
            Self::Top
        } else {
            Self::Bottom
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonLayout {
    Right,
    Left,
}

impl ButtonLayout {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("left") {
            Self::Left
        } else {
            Self::Right
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleClickAction {
    Maximize,
    Minimize,
    None,
}

impl DoubleClickAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Maximize => "maximize",
            Self::Minimize => "minimize",
            Self::None => "none",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "minimize" => Self::Minimize,
            "none" => Self::None,
            _ => Self::Maximize,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusMode {
    Click,
    Sloppy,
}

impl FocusMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Sloppy => "sloppy",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("sloppy") {
            Self::Sloppy
        } else {
            Self::Click
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowPlacement {
    Cascade,
    Center,
}

impl WindowPlacement {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cascade => "cascade",
            Self::Center => "center",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("center") {
            Self::Center
        } else {
            Self::Cascade
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Beam,
    Underline,
}

impl CursorStyle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Beam => "beam",
            Self::Underline => "underline",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "beam" => Self::Beam,
            "underline" => Self::Underline,
            _ => Self::Block,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetMode {
    Dhcp,
    Static,
}

impl NetMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dhcp => "dhcp",
            Self::Static => "static",
        }
    }

    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("static") {
            Self::Static
        } else {
            Self::Dhcp
        }
    }
}

pub struct WallpaperInfo {
    pub file: &'static str,
    pub label: &'static str,
}

pub const WALLPAPERS: [WallpaperInfo; 4] = [
    WallpaperInfo {
        file: "alpine-peaks.jpg",
        label: "雪峰晴空",
    },
    WallpaperInfo {
        file: "misty-pines.jpg",
        label: "松林远湾",
    },
    WallpaperInfo {
        file: "coastal-horizon.jpg",
        label: "海岸礁石",
    },
    WallpaperInfo {
        file: "emerald-forest.jpg",
        label: "翠谷溪林",
    },
];

pub const ACCENT_PRESETS: [(u32, &str); 9] = [
    (0x3b82f6, "湛蓝"),
    (0x0ea5e9, "天青"),
    (0x10b981, "翡翠"),
    (0xf59e0b, "琥珀"),
    (0xe11d48, "绯红"),
    (0x8b5cf6, "紫罗兰"),
    (0x6366f1, "靛蓝"),
    (0x64748b, "石板灰"),
    (0x000080, "98 深蓝"),
];

pub const SOLID_BG_PRESETS: [(u32, &str); 7] = [
    (0x1e293b, "深岩灰蓝"),
    (0x008080, "Win98 青绿"),
    (0x18181b, "石墨纯黑"),
    (0x0f3d3e, "深海墨绿"),
    (0x2e1f47, "暮色暗紫"),
    (0x334155, "冷钢灰"),
    (0x475569, "雾霭蓝灰"),
];

pub const RESOLUTIONS: [(&str, &str); 5] = [
    ("1024x768", "1024 × 768 (4:3)"),
    ("1280x800", "1280 × 800 (16:10)"),
    ("1440x900", "1440 × 900 (16:10)"),
    ("1600x900", "1600 × 900 (16:9)"),
    ("1920x1080", "1920 × 1080 (16:9)"),
];

pub const TIMEZONES: [(&str, &str); 5] = [
    ("UTC", "协调世界时 (UTC)"),
    ("Asia/Shanghai", "中国标准时间 (UTC+8)"),
    ("Asia/Tokyo", "日本标准时间 (UTC+9)"),
    ("Europe/London", "欧洲/伦敦"),
    ("America/New_York", "美洲/纽约"),
];

/// Resolve an IANA timezone name (e.g. `"Asia/Shanghai"`) to its POSIX `TZ` rule string
/// from the TZif v2/v3 footer of `/usr/share/zoneinfo/<tz>`, falling back to built-in rules.
pub fn posix_tz_for_zone(tz: &str) -> String {
    if !tz.is_empty() && !tz.contains("..") && !tz.starts_with('/') {
        let path = format!("/usr/share/zoneinfo/{tz}");
        if let Ok(bytes) = std::fs::read(&path) {
            if let Some(end) = bytes.iter().rposition(|&b| b == b'\n') {
                if let Some(start) = bytes[..end].iter().rposition(|&b| b == b'\n') {
                    if let Ok(rule) = std::str::from_utf8(&bytes[start + 1..end]) {
                        let trimmed = rule.trim();
                        if !trimmed.is_empty() {
                            return trimmed.to_string();
                        }
                    }
                }
            }
        }
    }
    match tz {
        "Asia/Shanghai" => "CST-8".to_string(),
        "Asia/Tokyo" => "JST-9".to_string(),
        "Europe/London" => "GMT0BST,M3.5.0/1,M10.5.0".to_string(),
        "America/New_York" => "EST5EDT,M3.2.0,M11.1.0".to_string(),
        "UTC" => "UTC0".to_string(),
        other => other.to_string(),
    }
}

/// Size preset for a Windows 8 / 8.1 Start Screen live or app tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartTileSize {
    Small,
    Medium,
    Wide,
    Large,
}

impl StartTileSize {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Wide => "wide",
            Self::Large => "large",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "小 (1x1)",
            Self::Medium => "中 (2x2)",
            Self::Wide => "宽 (4x2)",
            Self::Large => "大 (4x4)",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "small" | "s" => Some(Self::Small),
            "medium" | "m" => Some(Self::Medium),
            "wide" | "w" => Some(Self::Wide),
            "large" | "l" => Some(Self::Large),
            _ => None,
        }
    }

    /// Footprint in base Start Screen grid units `(cols, rows)`.
    pub fn grid_span(self) -> (i32, i32) {
        match self {
            Self::Small => (1, 1),
            Self::Medium => (2, 2),
            Self::Wide => (4, 2),
            Self::Large => (4, 4),
        }
    }
}

/// Persisted tile entry on the Windows 8 / 8.1 Start Screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartTileSpec {
    pub id: String,
    pub size: StartTileSize,
}

/// Validate that a tile identifier only uses safe ASCII slug characters.
fn is_valid_tile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Parse a serialized `/panel/start-tiles` specification, deduplicating tile IDs.
pub fn parse_start_tiles(raw: &str) -> Vec<StartTileSpec> {
    let mut out = Vec::new();
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (id, size) = match trimmed.split_once(':') {
            Some((id, sz)) => (
                id.trim(),
                StartTileSize::parse(sz).unwrap_or(StartTileSize::Medium),
            ),
            None => (trimmed, StartTileSize::Medium),
        };
        if !is_valid_tile_id(id) || out.iter().any(|t: &StartTileSpec| t.id == id) {
            continue;
        }
        out.push(StartTileSpec {
            id: id.to_string(),
            size,
        });
    }
    out
}

/// Serialize Start Screen tiles back into canonical `id:size` form.
pub fn encode_start_tiles(tiles: &[StartTileSpec]) -> String {
    tiles
        .iter()
        .filter(|t| is_valid_tile_id(&t.id))
        .map(|t| format!("{}:{}", t.id, t.size.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Typed in-memory snapshot of all SPUI desktop and built-in application channels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopConfig {
    // appearance
    pub theme_id: String,
    pub theme_style: ThemeStyle,
    pub mode: ThemeMode,
    pub accent: u32,
    pub font_scale: i32,
    // desktop
    pub wallpaper: String,
    pub backdrop_style: BackdropStyle,
    pub solid_color: u32,
    pub desktop_icons_visible: bool,
    pub desktop_icon_size: i32,
    // panel
    pub panel_position: PanelPosition,
    pub panel_size: i32,
    pub panel_show_pinned: bool,
    pub panel_start_fullscreen: bool,
    pub panel_start_tiles: String,
    // wm
    pub wm_button_layout: ButtonLayout,
    pub wm_double_click: DoubleClickAction,
    pub wm_focus_mode: FocusMode,
    pub wm_auto_raise: bool,
    pub wm_placement: WindowPlacement,
    // display
    pub display_resolution: String,
    pub display_blank_timeout_sec: i32,
    // input
    pub mouse_left_handed: bool,
    pub mouse_speed: i32,
    pub mouse_double_click_ms: u64,
    pub mouse_natural_scroll: bool,
    pub key_repeat_enabled: bool,
    pub key_repeat_delay_ms: i32,
    pub key_repeat_rate_hz: i32,
    // sound
    pub sound_master_volume: i32,
    pub sound_muted: bool,
    pub sound_enabled: bool,
    pub sound_event_startup: bool,
    pub sound_event_ui: bool,
    pub sound_event_notify: bool,
    pub sound_event_alert: bool,
    pub sound_event_volume: bool,
    pub sound_theme: String,
    // terminal
    pub term_font_size: i32,
    pub term_cursor_style: CursorStyle,
    pub term_scrollback_lines: usize,
    // files
    pub files_show_hidden: bool,
    pub files_folders_first: bool,
    pub files_single_click: bool,
    // editor
    pub editor_font_size: i32,
    pub editor_show_line_numbers: bool,
    pub editor_highlight_active_line: bool,
    pub editor_tab_width: usize,
    // datetime
    pub clock_24h: bool,
    pub clock_seconds: bool,
    pub clock_show_date: bool,
    pub timezone: String,
    // network
    pub net_enabled: bool,
    pub net_mode: NetMode,
    pub net_address: String,
    pub net_prefixlen: i32,
    pub net_gateway: String,
    pub net_dns: String,
    pub net_apply_serial: i64,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            theme_id: ThemePreset::Spui4.id().to_string(),
            theme_style: ThemeStyle::Spui,
            mode: ThemeMode::Dark,
            accent: ACCENT_PRESETS[0].0,
            font_scale: 100,
            wallpaper: WALLPAPERS[0].file.to_string(),
            backdrop_style: BackdropStyle::Cover,
            solid_color: SOLID_BG_PRESETS[0].0,
            desktop_icons_visible: true,
            desktop_icon_size: 40,
            panel_position: PanelPosition::Bottom,
            panel_size: 46,
            panel_show_pinned: true,
            panel_start_fullscreen: true,
            panel_start_tiles: crate::schema::DEFAULT_START_TILES.to_string(),
            wm_button_layout: ButtonLayout::Right,
            wm_double_click: DoubleClickAction::Maximize,
            wm_focus_mode: FocusMode::Click,
            wm_auto_raise: false,
            wm_placement: WindowPlacement::Cascade,
            display_resolution: "1280x800".to_string(),
            display_blank_timeout_sec: 0,
            mouse_left_handed: false,
            mouse_speed: 3,
            mouse_double_click_ms: 380,
            mouse_natural_scroll: false,
            key_repeat_enabled: true,
            key_repeat_delay_ms: 400,
            key_repeat_rate_hz: 30,
            sound_master_volume: 80,
            sound_muted: false,
            sound_enabled: true,
            sound_event_startup: true,
            sound_event_ui: true,
            sound_event_notify: true,
            sound_event_alert: true,
            sound_event_volume: true,
            sound_theme: "modern-glass".to_string(),
            term_font_size: 14,
            term_cursor_style: CursorStyle::Block,
            term_scrollback_lines: 1000,
            files_show_hidden: false,
            files_folders_first: true,
            files_single_click: false,
            editor_font_size: 14,
            editor_show_line_numbers: true,
            editor_highlight_active_line: true,
            editor_tab_width: 4,
            clock_24h: true,
            clock_seconds: false,
            clock_show_date: true,
            timezone: "UTC".to_string(),
            net_enabled: true,
            net_mode: NetMode::Dhcp,
            net_address: "10.0.2.15".to_string(),
            net_prefixlen: 24,
            net_gateway: "10.0.2.2".to_string(),
            net_dns: "10.0.2.3".to_string(),
            net_apply_serial: 0,
        }
    }
}

impl DesktopConfig {
    pub fn wallpaper_path(&self) -> String {
        format!("{WALLPAPER_DIR}/{}", self.wallpaper)
    }

    /// Apply a built-in theme preset bundle in memory (used by tests & offline previews).
    pub fn apply_theme_preset(&mut self, preset: ThemePreset) -> bool {
        let mut changed = false;
        for (ch, prop, val) in preset.properties() {
            changed |= self.apply_property(ch, prop, &val);
        }
        changed
    }

    /// Apply a single `(channel, property, value)` update, returning `true` if the snapshot changed.
    pub fn apply_property(&mut self, channel: &str, property: &str, value: &Value) -> bool {
        let before = self.clone();
        match (channel, property) {
            ("appearance", "/theme/id") => {
                if let Some(s) = value.as_str() {
                    let parsed = ThemePreset::parse(s);
                    self.theme_id = parsed.id().to_string();
                }
            }
            ("appearance", "/theme/style") => {
                if let Some(s) = value.as_str() {
                    self.theme_style = ThemeStyle::parse(s);
                }
            }
            ("appearance", "/theme/mode") => {
                if let Some(s) = value.as_str() {
                    self.mode = ThemeMode::parse(s);
                }
            }
            ("appearance", "/theme/accent") => {
                if let Some(rgb) = value.as_str().and_then(parse_hex_rgb) {
                    self.accent = rgb;
                }
            }
            ("appearance", "/font/scale") => {
                if let Some(v) = value.as_int() {
                    self.font_scale = (v as i32).clamp(85, 125);
                }
            }
            ("desktop", "/backdrop/wallpaper") => {
                if let Some(s) = value.as_str() {
                    if !s.is_empty() && !s.contains('/') && !s.contains("..") {
                        self.wallpaper = s.to_string();
                    }
                }
            }
            ("desktop", "/backdrop/style") => {
                if let Some(s) = value.as_str() {
                    self.backdrop_style = BackdropStyle::parse(s);
                }
            }
            ("desktop", "/backdrop/solid-color") => {
                if let Some(rgb) = value.as_str().and_then(parse_hex_rgb) {
                    self.solid_color = rgb;
                }
            }
            ("desktop", "/icons/visible") => {
                if let Some(b) = value.as_bool() {
                    self.desktop_icons_visible = b;
                }
            }
            ("desktop", "/icons/size") => {
                if let Some(v) = value.as_int() {
                    self.desktop_icon_size = (v as i32).clamp(28, 56);
                }
            }
            ("panel", "/position") => {
                if let Some(s) = value.as_str() {
                    self.panel_position = PanelPosition::parse(s);
                }
            }
            ("panel", "/size") => {
                if let Some(v) = value.as_int() {
                    self.panel_size = (v as i32).clamp(34, 60);
                }
            }
            ("panel", "/show-pinned") => {
                if let Some(b) = value.as_bool() {
                    self.panel_show_pinned = b;
                }
            }
            ("panel", "/start-fullscreen") => {
                if let Some(b) = value.as_bool() {
                    self.panel_start_fullscreen = b;
                }
            }
            ("panel", "/start-tiles") => {
                if let Some(s) = value.as_str() {
                    let parsed = parse_start_tiles(s);
                    self.panel_start_tiles = encode_start_tiles(&parsed);
                }
            }
            ("wm", "/button-layout") => {
                if let Some(s) = value.as_str() {
                    self.wm_button_layout = ButtonLayout::parse(s);
                }
            }
            ("wm", "/double-click-action") => {
                if let Some(s) = value.as_str() {
                    self.wm_double_click = DoubleClickAction::parse(s);
                }
            }
            ("wm", "/focus-mode") => {
                if let Some(s) = value.as_str() {
                    self.wm_focus_mode = FocusMode::parse(s);
                }
            }
            ("wm", "/auto-raise") => {
                if let Some(b) = value.as_bool() {
                    self.wm_auto_raise = b;
                }
            }
            ("wm", "/placement") => {
                if let Some(s) = value.as_str() {
                    self.wm_placement = WindowPlacement::parse(s);
                }
            }
            ("display", "/resolution") => {
                if let Some(s) = value.as_str() {
                    if !s.is_empty() {
                        self.display_resolution = s.to_string();
                    }
                }
            }
            ("display", "/blank-timeout-sec") => {
                if let Some(v) = value.as_int() {
                    self.display_blank_timeout_sec = (v as i32).clamp(0, 7200);
                }
            }
            ("input", "/mouse/left-handed") => {
                if let Some(b) = value.as_bool() {
                    self.mouse_left_handed = b;
                }
            }
            ("input", "/mouse/speed") => {
                if let Some(v) = value.as_int() {
                    self.mouse_speed = (v as i32).clamp(1, 5);
                }
            }
            ("input", "/mouse/double-click-ms") => {
                if let Some(v) = value.as_int() {
                    self.mouse_double_click_ms = (v.clamp(150, 900)) as u64;
                }
            }
            ("input", "/mouse/natural-scroll") => {
                if let Some(b) = value.as_bool() {
                    self.mouse_natural_scroll = b;
                }
            }
            ("input", "/keyboard/repeat-enabled") => {
                if let Some(b) = value.as_bool() {
                    self.key_repeat_enabled = b;
                }
            }
            ("input", "/keyboard/repeat-delay-ms") => {
                if let Some(v) = value.as_int() {
                    self.key_repeat_delay_ms = (v as i32).clamp(150, 1000);
                }
            }
            ("input", "/keyboard/repeat-rate-hz") => {
                if let Some(v) = value.as_int() {
                    self.key_repeat_rate_hz = (v as i32).clamp(5, 80);
                }
            }
            ("sound", "/master-volume") => {
                if let Some(v) = value.as_int() {
                    self.sound_master_volume = (v as i32).clamp(0, 100);
                }
            }
            ("sound", "/muted") => {
                if let Some(b) = value.as_bool() {
                    self.sound_muted = b;
                }
            }
            ("sound", "/system-sounds-enabled") => {
                if let Some(b) = value.as_bool() {
                    self.sound_enabled = b;
                }
            }
            ("sound", "/event-startup") => {
                if let Some(b) = value.as_bool() {
                    self.sound_event_startup = b;
                }
            }
            ("sound", "/event-ui") => {
                if let Some(b) = value.as_bool() {
                    self.sound_event_ui = b;
                }
            }
            ("sound", "/event-notify") => {
                if let Some(b) = value.as_bool() {
                    self.sound_event_notify = b;
                }
            }
            ("sound", "/event-alert") => {
                if let Some(b) = value.as_bool() {
                    self.sound_event_alert = b;
                }
            }
            ("sound", "/event-volume") => {
                if let Some(b) = value.as_bool() {
                    self.sound_event_volume = b;
                }
            }
            ("sound", "/theme") => {
                if let Some(s) = value.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() && trimmed.len() <= 32 {
                        self.sound_theme = trimmed.to_string();
                    }
                }
            }
            ("terminal", "/font-size") => {
                if let Some(v) = value.as_int() {
                    self.term_font_size = (v as i32).clamp(11, 22);
                }
            }
            ("terminal", "/cursor-style") => {
                if let Some(s) = value.as_str() {
                    self.term_cursor_style = CursorStyle::parse(s);
                }
            }
            ("terminal", "/scrollback-lines") => {
                if let Some(v) = value.as_int() {
                    self.term_scrollback_lines = (v.clamp(100, 20000)) as usize;
                }
            }
            ("files", "/show-hidden") => {
                if let Some(b) = value.as_bool() {
                    self.files_show_hidden = b;
                }
            }
            ("files", "/folders-first") => {
                if let Some(b) = value.as_bool() {
                    self.files_folders_first = b;
                }
            }
            ("files", "/single-click-open") => {
                if let Some(b) = value.as_bool() {
                    self.files_single_click = b;
                }
            }
            ("editor", "/font-size") => {
                if let Some(v) = value.as_int() {
                    self.editor_font_size = (v as i32).clamp(11, 22);
                }
            }
            ("editor", "/show-line-numbers") => {
                if let Some(b) = value.as_bool() {
                    self.editor_show_line_numbers = b;
                }
            }
            ("editor", "/highlight-active-line") => {
                if let Some(b) = value.as_bool() {
                    self.editor_highlight_active_line = b;
                }
            }
            ("editor", "/tab-width") => {
                if let Some(v) = value.as_int() {
                    self.editor_tab_width = (v.clamp(2, 8)) as usize;
                }
            }
            ("datetime", "/clock-24h") => {
                if let Some(b) = value.as_bool() {
                    self.clock_24h = b;
                }
            }
            ("datetime", "/clock-seconds") => {
                if let Some(b) = value.as_bool() {
                    self.clock_seconds = b;
                }
            }
            ("datetime", "/clock-show-date") => {
                if let Some(b) = value.as_bool() {
                    self.clock_show_date = b;
                }
            }
            ("datetime", "/timezone") => {
                if let Some(s) = value.as_str() {
                    if !s.is_empty() && !s.contains("..") {
                        self.timezone = s.to_string();
                    }
                }
            }
            ("network", "/enabled") => {
                if let Some(b) = value.as_bool() {
                    self.net_enabled = b;
                }
            }
            ("network", "/mode") => {
                if let Some(s) = value.as_str() {
                    self.net_mode = NetMode::parse(s);
                }
            }
            ("network", "/address") => {
                if let Some(s) = value.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() && trimmed.len() <= 64 {
                        self.net_address = trimmed.to_string();
                    }
                }
            }
            ("network", "/prefixlen") => {
                if let Some(v) = value.as_int() {
                    self.net_prefixlen = (v as i32).clamp(1, 32);
                }
            }
            ("network", "/gateway") => {
                if let Some(s) = value.as_str() {
                    let trimmed = s.trim();
                    if trimmed.len() <= 64 {
                        self.net_gateway = trimmed.to_string();
                    }
                }
            }
            ("network", "/dns") => {
                if let Some(s) = value.as_str() {
                    let trimmed = s.trim();
                    if trimmed.len() <= 128 {
                        self.net_dns = trimmed.to_string();
                    }
                }
            }
            ("network", "/apply-serial") => {
                if let Some(v) = value.as_int() {
                    self.net_apply_serial = v;
                }
            }
            _ => {}
        }
        *self != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{channel_defaults, validate_schema_type, BUILTIN_CHANNELS};
    use crate::storage::{encode_channel_conf, parse_channel_conf};
    use std::collections::BTreeMap;

    #[test]
    fn channel_and_property_validation_rejects_malformed_paths() {
        assert_eq!(validate_channel("Appearance").unwrap(), "appearance");
        assert!(validate_channel("").is_err());
        assert!(validate_channel("../etc").is_err());
        assert!(validate_channel("bad/name").is_err());

        assert_eq!(
            validate_property("/theme/mode", false).unwrap(),
            "/theme/mode"
        );
        assert_eq!(validate_property("/", true).unwrap(), "/");
        assert!(validate_property("/", false).is_err());
        assert!(validate_property("theme/mode", false).is_err());
        assert!(validate_property("/theme/mode/", false).is_err());
        assert!(validate_property("/theme//mode", false).is_err());
        assert!(validate_property("/theme/../mode", false).is_err());
    }

    #[test]
    fn conf_text_roundtrips_escaped_strings_and_all_scalar_types() {
        let mut map = BTreeMap::new();
        map.insert("/flag".to_string(), Value::Bool(true));
        map.insert("/count".to_string(), Value::Int(-42));
        map.insert("/ratio".to_string(), Value::Float(1.25));
        map.insert(
            "/greeting".to_string(),
            Value::String("line1\n\"quoted\"\t\\slash".to_string()),
        );

        let rendered = encode_channel_conf("custom", &map);
        let with_comments = format!("# leading comment\n\n{rendered}\n# trailing\n");
        let parsed = parse_channel_conf(&with_comments, "custom");
        assert_eq!(parsed, map);
    }

    #[test]
    fn builtin_schemas_match_desktop_config_defaults_and_type_checks() {
        let mut cfg = DesktopConfig::default();
        for ch in BUILTIN_CHANNELS {
            let defaults = channel_defaults(ch);
            assert!(!defaults.is_empty(), "empty defaults for {ch}");
            for (prop, val) in defaults {
                validate_schema_type(ch, prop, &val).unwrap();
                assert!(
                    !cfg.apply_property(ch, prop, &val),
                    "schema default {ch}:{prop} diverged from DesktopConfig::default()"
                );
            }
        }
        assert!(validate_schema_type("appearance", "/font/scale", &Value::Bool(true)).is_err());
        assert!(validate_schema_type("custom", "/anything", &Value::Bool(true)).is_ok());
    }

    #[test]
    fn start_tiles_parse_and_encode_roundtrip() {
        let parsed = parse_start_tiles(crate::schema::DEFAULT_START_TILES);
        assert_eq!(
            encode_start_tiles(&parsed),
            crate::schema::DEFAULT_START_TILES
        );

        let sanitized =
            parse_start_tiles("status-clock:wide, bad/id:small, status-clock:small, app-term:l");
        assert_eq!(sanitized.len(), 2);
        assert_eq!(sanitized[0].id, "status-clock");
        assert_eq!(sanitized[0].size, StartTileSize::Wide);
        assert_eq!(sanitized[1].id, "app-term");
        assert_eq!(sanitized[1].size, StartTileSize::Large);
    }

    #[test]
    fn theme_preset_roundtrip_and_overrides() {
        let mut cfg = DesktopConfig::default();
        assert_eq!(cfg.theme_id, "spui-4");
        assert_eq!(cfg.theme_style, ThemeStyle::Spui);

        assert!(cfg.apply_theme_preset(ThemePreset::Win98));
        assert_eq!(cfg.theme_id, "win98");
        assert_eq!(cfg.theme_style, ThemeStyle::Win98);
        assert_eq!(cfg.mode, ThemeMode::Light);
        assert_eq!(cfg.accent, 0x000080);
        assert_eq!(cfg.backdrop_style, BackdropStyle::Solid);
        assert_eq!(cfg.solid_color, 0x008080);

        // User override keeps Win98 chrome while changing accent cleanly.
        assert!(cfg.apply_property(
            "appearance",
            "/theme/accent",
            &Value::String("#e11d48".to_string())
        ));
        assert_eq!(cfg.theme_style, ThemeStyle::Win98);
        assert_eq!(cfg.accent, 0xe11d48);

        assert!(cfg.apply_theme_preset(ThemePreset::Spui4));
        assert_eq!(cfg.theme_id, "spui-4");
        assert_eq!(cfg.theme_style, ThemeStyle::Spui);
        assert_eq!(cfg.mode, ThemeMode::Dark);
        assert_eq!(cfg.backdrop_style, BackdropStyle::Cover);
    }
}
