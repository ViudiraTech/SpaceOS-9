/*
 *
 *       user/desktop/src/apps/settings.rs
 *       SPUI 4.0 System Settings: 11-category preferences & shared settings view
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mature, clutter-free System Settings view connected live to `uxconfd`.
//! Shared 1:1 between the standalone System Settings window (`spui settings`)
//! and the Start Screen Settings Center (`设置中心`), covering all eleven
//! categories: Appearance, Network, Desktop & Panel, Windows, Display, Sound,
//! Mouse & Keyboard, Applications, Date & Time, Boot Logs, and About.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect, Sprite};
use crate::icons::{self, Icon};
use crate::text::{Face, TextRenderer};
use crate::theme::Theme;
use crate::ui;
use crate::x11::{CursorKind, Key, Result};
use std::ffi::CStr;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uxconf::{
    format_hex_rgb, BackdropStyle, ButtonLayout, ConfError, CursorStyle, DesktopConfig,
    DoubleClickAction, FocusMode, NetMode, PanelPosition, ThemeMode, ThemePreset, Value,
    WindowPlacement, ACCENT_PRESETS, RESOLUTIONS, SOLID_BG_PRESETS, THEME_PRESETS, TIMEZONES,
    WALLPAPERS, WALLPAPER_DIR,
};
use x11rb::protocol::xproto::ConnectionExt;

extern "C" {
    fn tzset();
}

const SIDEBAR_W: i32 = 196;
const HEADER_H: i32 = 52;
const ROW_H: i32 = 56;
const THUMB_W: usize = 112;
const THUMB_H: usize = 66;

/// Abstraction over configuration storage and display queries so `SettingsState`
/// runs identically inside standalone `App` windows and the `Shell` Start Screen.
pub trait SettingsHost {
    fn config(&self) -> &DesktopConfig;
    fn root_resolution(&self) -> (u16, u16);
    fn set_config_property(&mut self, channel: &str, property: &str, value: Value);
    /// Apply a property in-process only, without the `uxconfd` round trip
    /// (atomic write + fsync + broadcast). Used for high-rate previews such as
    /// slider drags; the final value is committed with `set_config_property`.
    fn stage_config_property(&mut self, channel: &str, property: &str, value: Value);
    fn apply_theme_preset(&mut self, preset: ThemePreset) {
        for (channel, property, value) in preset.properties() {
            self.set_config_property(channel, property, value);
        }
    }
    fn reset_config_channel(&mut self, channel: &str);
    fn factory_reset(&mut self) -> Option<std::result::Result<(), ConfError>>;
}

impl SettingsHost for App {
    fn config(&self) -> &DesktopConfig {
        &self.config
    }

    fn root_resolution(&self) -> (u16, u16) {
        self.x
            .conn
            .get_geometry(self.x.root)
            .ok()
            .and_then(|c| c.reply().ok())
            .map_or((self.x.width, self.x.height), |g| (g.width, g.height))
    }

    fn set_config_property(&mut self, channel: &str, property: &str, value: Value) {
        App::set_config_property(self, channel, property, value);
    }

    fn stage_config_property(&mut self, channel: &str, property: &str, value: Value) {
        App::stage_config_property(self, channel, property, value);
    }

    fn reset_config_channel(&mut self, channel: &str) {
        App::reset_config_channel(self, channel);
    }

    fn factory_reset(&mut self) -> Option<std::result::Result<(), ConfError>> {
        self.uxconf.as_mut().map(|client| client.factory_reset())
    }
}

/// Visual presentation mode for the shared Settings view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsRenderStyle {
    /// Standalone window style with solid window fill and compact sidebar.
    Classic,
    /// Full-screen Start Screen `设置中心` style matching XJ380 cards & translucent chrome.
    Xj380,
}

/// Outcome returned from [`SettingsState::on_button`] for parent containers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsAction {
    pub redraw: bool,
    pub cursor: Option<CursorKind>,
    pub back_to_home: bool,
}

const FONT_SCALES: [(i64, &str); 3] = [(90, "紧凑"), (100, "标准"), (115, "大号")];
const DESKTOP_ICON_SIZES: [(i64, &str); 3] = [(32, "小"), (40, "标准"), (48, "大")];
const PANEL_SIZES: [(i64, &str); 3] = [(38, "紧凑"), (46, "标准"), (54, "宽大")];
const BLANK_TIMEOUTS: [(i64, &str); 4] = [
    (0, "从不"),
    (300, "5 分钟"),
    (600, "10 分钟"),
    (900, "15 分钟"),
];
const MOUSE_SPEEDS: [(i64, &str); 5] =
    [(1, "慢"), (2, "较慢"), (3, "适中"), (4, "较快"), (5, "快")];
const DOUBLE_CLICK_SPEEDS: [(i64, &str); 3] = [(250, "快"), (380, "适中"), (550, "慢")];
const KEY_DELAYS: [(i64, &str); 3] = [(250, "短"), (400, "适中"), (600, "长")];
const KEY_RATES: [(i64, &str); 3] = [(15, "慢"), (30, "适中"), (45, "快")];
const VOLUME_PRESETS: [(i64, &str); 4] = [(25, "25%"), (50, "50%"), (80, "80%"), (100, "100%")];
const CODE_FONT_SIZES: [(i64, &str); 4] = [(12, "12"), (14, "14"), (16, "16"), (18, "18")];
const SCROLLBACK_PRESETS: [(i64, &str); 3] =
    [(500, "500 行"), (1000, "1000 行"), (3000, "3000 行")];
const TAB_WIDTHS: [(i64, &str); 3] = [(2, "2 空格"), (4, "4 空格"), (8, "8 空格")];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Appearance,
    Network,
    DesktopPanel,
    Windows,
    Display,
    Sound,
    Input,
    Applications,
    DateTime,
    Logs,
    About,
}

impl Page {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Appearance => "外观",
            Self::Network => "网络",
            Self::DesktopPanel => "桌面与面板",
            Self::Windows => "窗口",
            Self::Display => "显示",
            Self::Sound => "声音与音效",
            Self::Input => "鼠标与键盘",
            Self::Applications => "应用程序",
            Self::DateTime => "日期与时间",
            Self::Logs => "启动日志",
            Self::About => "关于",
        }
    }

    pub const fn channels(self) -> &'static [&'static str] {
        match self {
            Self::Appearance => &["appearance"],
            Self::Network => &["network"],
            Self::DesktopPanel => &["desktop", "panel"],
            Self::Windows => &["wm"],
            Self::Display => &["display"],
            Self::Sound => &["sound"],
            Self::Input => &["input"],
            Self::Applications => &["terminal", "files", "editor"],
            Self::DateTime => &["datetime"],
            Self::Logs => &[],
            Self::About => &[],
        }
    }
}

/// Searchable index entry mapping any setting control back to its settings [`Page`].
#[derive(Clone, Copy, Debug)]
pub struct SettingSearchItem {
    pub page: Page,
    pub category: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub keywords: &'static str,
}

pub const SETTING_ITEMS: &[SettingSearchItem] = &[
    // 外观
    SettingSearchItem {
        page: Page::Appearance,
        category: "外观",
        title: "系统桌面主题",
        subtitle: "切换 SPUI 4.0 现代界面与经典 Windows 98 主题",
        keywords: "theme style win98 windows 98 classic spui 主题 经典 风格 复古",
    },
    SettingSearchItem {
        page: Page::Appearance,
        category: "外观",
        title: "外观模式",
        subtitle: "深色 / 浅色桌面主题切换",
        keywords: "theme dark light mode 深色 浅色 外观 主题",
    },
    SettingSearchItem {
        page: Page::Appearance,
        category: "外观",
        title: "强调色",
        subtitle: "选择按钮、选中高亮与磁贴强调色",
        keywords: "accent color blue 颜色 强调色 主题色",
    },
    SettingSearchItem {
        page: Page::Appearance,
        category: "外观",
        title: "界面字体缩放",
        subtitle: "紧凑 (90%) / 标准 (100%) / 大号 (115%)",
        keywords: "font scale dpi 缩放 字体大小",
    },
    // 网络
    SettingSearchItem {
        page: Page::Network,
        category: "网络",
        title: "有线网络开关与状态",
        subtitle: "启用或断开以太网接口连接",
        keywords: "network ethernet switch 有线 网络 网卡 开关",
    },
    SettingSearchItem {
        page: Page::Network,
        category: "网络",
        title: "IPv4 配置模式 (DHCP / 静态 IP)",
        subtitle: "自动获取租约或手动配置 IP / 子网掩码 / 网关 / DNS",
        keywords: "ipv4 dhcp static ip gateway dns 网关 子网 掩码 静态ip",
    },
    // 桌面与面板
    SettingSearchItem {
        page: Page::DesktopPanel,
        category: "桌面与面板",
        title: "桌面壁纸与背景样式",
        subtitle: "选择高清风景壁纸、填满方式或纯色背景",
        keywords: "wallpaper backdrop solid cover 桌面 壁纸 背景 纯色",
    },
    SettingSearchItem {
        page: Page::DesktopPanel,
        category: "桌面与面板",
        title: "桌面图标显示与尺寸",
        subtitle: "控制桌面快捷方式显隐与图标大小",
        keywords: "desktop icon size 桌面 图标 尺寸",
    },
    SettingSearchItem {
        page: Page::DesktopPanel,
        category: "桌面与面板",
        title: "系统面板位置与高度",
        subtitle: "任务栏停靠在底部/顶部，紧凑/标准/宽大及固定应用",
        keywords: "panel taskbar position size pinned 面板 任务栏 位置 高度 固定",
    },
    SettingSearchItem {
        page: Page::DesktopPanel,
        category: "桌面与面板",
        title: "全屏开始屏幕",
        subtitle: "切换 Windows 8 风格全屏开始屏幕与经典弹出菜单",
        keywords: "start menu fullscreen win8 tile 开始菜单 开始屏幕 全屏 磁贴",
    },
    // 窗口
    SettingSearchItem {
        page: Page::Windows,
        category: "窗口",
        title: "标题栏按钮位置与双击动作",
        subtitle: "左/右侧窗口控制按钮及双击最大化/最小化",
        keywords: "window titlebar button maximize minimize 窗口 标题栏 双击 按钮",
    },
    SettingSearchItem {
        page: Page::Windows,
        category: "窗口",
        title: "窗口焦点与新窗口放置策略",
        subtitle: "点击/悬停激活、自动前置与级联/居中布局",
        keywords: "focus raise cascade center 焦点 自动前置 级联 居中",
    },
    // 显示
    SettingSearchItem {
        page: Page::Display,
        category: "显示",
        title: "屏幕分辨率",
        subtitle: "通过 RandR 调整显示分辨率 (1024x768 - 1920x1080)",
        keywords: "display resolution xrandr 分辨率 显示器 屏幕",
    },
    SettingSearchItem {
        page: Page::Display,
        category: "显示",
        title: "空闲熄屏时间",
        subtitle: "无操作后自动进入屏保熄屏的等待时间",
        keywords: "blank screen timeout 熄屏 屏保 待机",
    },
    // 声音与音效
    SettingSearchItem {
        page: Page::Sound,
        category: "声音与音效",
        title: "主音量与静音",
        subtitle: "实时调节 ALSA 主音量滑块、presets 及静音开关",
        keywords: "sound audio volume mute alsa 声音 音量 静音 音频",
    },
    SettingSearchItem {
        page: Page::Sound,
        category: "声音与音效",
        title: "系统事件音效与试听",
        subtitle: "控制启动、界面、通知、告警、音量音效并支持试听",
        keywords: "sfx event preview notify alert 音效 提示音 试听",
    },
    // 鼠标与键盘
    SettingSearchItem {
        page: Page::Input,
        category: "鼠标与键盘",
        title: "鼠标主键、速度与自然滚动",
        subtitle: "右手/左手习惯、指针速度、双击间隔与滚动方向",
        keywords: "mouse speed click scroll hand 鼠标 指针 速度 双击 滚动",
    },
    SettingSearchItem {
        page: Page::Input,
        category: "鼠标与键盘",
        title: "键盘按键连发设置",
        subtitle: "连发开关、首次延迟与连发速率 (Hz)",
        keywords: "keyboard repeat delay rate 键盘 连发 延迟 速率",
    },
    // 应用程序
    SettingSearchItem {
        page: Page::Applications,
        category: "应用程序",
        title: "终端设置",
        subtitle: "字体大小、光标样式 (块状/竖线/下划线) 与回滚行数",
        keywords: "terminal cursor scrollback font 终端 光标 回滚",
    },
    SettingSearchItem {
        page: Page::Applications,
        category: "应用程序",
        title: "文件管理器设置",
        subtitle: "显示隐藏文件、文件夹优先排序与单击打开",
        keywords: "files hidden folders single click 文件管理器 隐藏文件 单击",
    },
    SettingSearchItem {
        page: Page::Applications,
        category: "应用程序",
        title: "文本编辑器设置",
        subtitle: "等宽字号、显示行号、高亮当前行与 Tab 缩进宽度",
        keywords: "editor line numbers tab width 编辑器 行号 缩进",
    },
    // 日期与时间
    SettingSearchItem {
        page: Page::DateTime,
        category: "日期与时间",
        title: "时钟格式与面板日期",
        subtitle: "24 小时制、显示秒数与任务栏显示日期",
        keywords: "clock time 24h seconds date 时钟 24小时 秒数 日期",
    },
    SettingSearchItem {
        page: Page::DateTime,
        category: "日期与时间",
        title: "系统时区",
        subtitle: "选择 UTC、上海、东京、伦敦或纽约时区",
        keywords: "timezone utc shanghai tokyo 时区 上海",
    },
    // 启动日志
    SettingSearchItem {
        page: Page::Logs,
        category: "启动日志",
        title: "启动与系统运行日志",
        subtitle: "筛选内核/服务/告警日志并检查详细信息",
        keywords: "logs kernel journal boot 启动日志 内核 日志",
    },
    // 关于
    SettingSearchItem {
        page: Page::About,
        category: "关于",
        title: "关于 SpaceOS 9 与硬件信息",
        subtitle: "查看主机名、CPU、内存、磁盘、内核版本",
        keywords: "about hardware os kernel cpu memory 关于 硬件 系统版本",
    },
    SettingSearchItem {
        page: Page::About,
        category: "关于",
        title: "恢复出厂设置",
        subtitle: "清除用户账户、主目录并重置配置进入 OOBE 向导",
        keywords: "factory reset oobe 重置 恢复出厂",
    },
];

/// Match search query against all system setting items across all 11 categories.
pub fn search_settings(query: &str) -> Vec<&'static SettingSearchItem> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let q_raw = query.trim().to_lowercase();
    SETTING_ITEMS
        .iter()
        .filter(|item| {
            item.category.to_lowercase().contains(&q_raw)
                || item.title.to_lowercase().contains(&q_raw)
                || item.subtitle.to_lowercase().contains(&q_raw)
                || item.keywords.to_lowercase().contains(&q_raw)
        })
        .collect()
}

const NAV: [(Page, &str, Icon); 11] = [
    (Page::Appearance, "外观", Icon::Appearance),
    (Page::Network, "网络", Icon::Network),
    (Page::DesktopPanel, "桌面与面板", Icon::Panel),
    (Page::Windows, "窗口", Icon::WindowManager),
    (Page::Display, "显示", Icon::Display),
    (Page::Sound, "声音与音效", Icon::Sound),
    (Page::Input, "鼠标与键盘", Icon::InputDevices),
    (Page::Applications, "应用程序", Icon::Applications),
    (Page::DateTime, "日期与时间", Icon::DateTime),
    (Page::Logs, "启动日志", Icon::FileText),
    (Page::About, "关于", Icon::About),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum HitTarget {
    Nav(usize),
    ResetPage,
    ThemePreset(usize),
    ThemeMode(ThemeMode),
    Accent(usize),
    FontScale(usize),
    NetEnabled,
    NetMode(usize),
    NetInput(usize),
    NetApply,
    NetRenew,
    NetRefresh,
    NetPingGateway,
    BackdropStyle(usize),
    Wallpaper(usize),
    SolidColor(usize),
    DesktopIconsVisible,
    DesktopIconSize(usize),
    PanelPosition(usize),
    PanelSize(usize),
    PanelShowPinned,
    PanelStartFullscreen,
    BackToHome,
    SearchJump(Page),
    WmButtonLayout(usize),
    WmDoubleClick(usize),
    WmFocusMode(usize),
    WmAutoRaise,
    WmPlacement(usize),
    Resolution(usize),
    BlankTimeout(usize),
    SoundMuted,
    SoundMasterSlider,
    SoundVolumePreset(usize),
    SoundEnabled,
    SoundEventStartup,
    SoundEventUi,
    SoundEventNotify,
    SoundEventAlert,
    SoundEventVolume,
    SoundPreview(usize),
    MouseHandedness(usize),
    MouseSpeed(usize),
    DoubleClickSpeed(usize),
    NaturalScroll,
    KeyRepeatEnabled,
    KeyRepeatDelay(usize),
    KeyRepeatRate(usize),
    TermFontSize(usize),
    TermCursorStyle(usize),
    TermScrollback(usize),
    FilesShowHidden,
    FilesFoldersFirst,
    FilesSingleClick,
    EditorFontSize(usize),
    EditorLineNumbers,
    EditorHighlightLine,
    EditorTabWidth(usize),
    Clock24h,
    ClockSeconds,
    ClockShowDate,
    Timezone(usize),
    FactoryReset,
    LogFilter(usize),
    LogRefresh,
    LogSelect(usize),
    JumpToLogs,
}

static FACTORY_RESET_ARMED: Mutex<Option<std::time::Instant>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogFilterType {
    All,
    Kernel,
    Services,
    ErrorsWarns,
}

#[derive(Clone, Debug)]
struct SystemLogEntry {
    #[allow(dead_code)]
    seq: u64,
    mono_us: u64,
    prio: u8,
    unit: String,
    tag: String,
    message: String,
}

struct LogViewerState {
    entries: Vec<SystemLogEntry>,
    filter: LogFilterType,
    selected: Option<usize>,
    total_count: usize,
    kernel_count: usize,
    service_count: usize,
    error_count: usize,
    warn_count: usize,
    uptime_str: String,
}

impl LogViewerState {
    fn load() -> Self {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::net::UnixStream;

        let mut entries = Vec::new();

        // 1. Ingest from /dev/kmsg
        if let Ok(mut nb_file) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(0o4000)
            .open("/dev/kmsg")
        {
            let mut buf = [0u8; 2048];
            while let Ok(n) = nb_file.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                    if let Some(comma1) = s.find(',') {
                        let pnum: u32 = s[..comma1].parse().unwrap_or(6);
                        let prio = (pnum & 7) as u8;
                        let rem = &s[comma1 + 1..];
                        if let Some(comma2) = rem.find(',') {
                            let seq: u64 = rem[..comma2].parse().unwrap_or(0);
                            let rem2 = &rem[comma2 + 1..];
                            if let Some(comma3) = rem2.find(',') {
                                let usec: u64 = rem2[..comma3].parse().unwrap_or(0);
                                if let Some(semi) = rem2.find(';') {
                                    let msg = rem2[semi + 1..].trim();
                                    entries.push(SystemLogEntry {
                                        seq,
                                        mono_us: usec,
                                        prio,
                                        unit: "kernel".to_string(),
                                        tag: "kernel".to_string(),
                                        message: msg.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Query journal daemon if active
        if let Ok(mut stream) = UnixStream::connect("/run/systemd/journal/query.sock") {
            let _ = stream.write_all(b"QUERY limit=2000\n");
            let reader = BufReader::new(stream);
            for line in reader.lines().flatten() {
                if line == "END" {
                    break;
                }
                if let Some(rest) = line.strip_prefix("ENTRY|") {
                    let parts: Vec<&str> = rest.split('|').collect();
                    if parts.len() >= 13 {
                        let seq: u64 = parts[0].parse().unwrap_or(0);
                        let mono_us: u64 = parts[2].parse().unwrap_or(0);
                        let prio: u8 = parts[3].parse().unwrap_or(6);
                        let unit = parts[9].to_string();
                        let comm = parts[10].to_string();
                        let ident = parts[11].to_string();
                        let msg = parts[12].replace("\\n", " ").replace("\\r", "");
                        let tag = if !ident.is_empty() {
                            ident
                        } else if !comm.is_empty() {
                            comm
                        } else {
                            unit.clone()
                        };

                        if unit == "kernel" && entries.iter().any(|e| e.mono_us == mono_us) {
                            continue;
                        }
                        entries.push(SystemLogEntry {
                            seq,
                            mono_us,
                            prio,
                            unit,
                            tag,
                            message: msg,
                        });
                    }
                }
            }
        }

        entries.sort_by_key(|e| e.mono_us);

        let total_count = entries.len();
        let mut kernel_count = 0;
        let mut service_count = 0;
        let mut error_count = 0;
        let mut warn_count = 0;

        for e in &entries {
            if e.unit == "kernel" {
                kernel_count += 1;
            } else {
                service_count += 1;
            }
            if e.prio <= 3 {
                error_count += 1;
            } else if e.prio == 4 {
                warn_count += 1;
            }
        }

        let uptime_str = if let Ok(s) = fs::read_to_string("/proc/uptime") {
            if let Some(sec_str) = s.split_whitespace().next() {
                if let Ok(sec) = sec_str.parse::<f64>() {
                    let s = sec as u64;
                    format!("{}分 {}秒", s / 60, s % 60)
                } else {
                    "--".to_string()
                }
            } else {
                "--".to_string()
            }
        } else {
            "--".to_string()
        };

        Self {
            entries,
            filter: LogFilterType::All,
            selected: None,
            total_count,
            kernel_count,
            service_count,
            error_count,
            warn_count,
            uptime_str,
        }
    }
}

struct HitBox {
    rect: Rect,
    target: HitTarget,
}

struct SystemInfo {
    hostname: String,
    os_name: String,
    kernel_arch: String,
    cpu: String,
    memory: String,
    disk: String,
}

impl SystemInfo {
    fn gather() -> Self {
        let hostname = fs::read_to_string("/etc/hostname")
            .ok()
            .and_then(|s| s.lines().next().map(str::trim).map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "spaceos".to_string());

        let os_raw = fs::read_to_string("/etc/os-release").unwrap_or_default();
        let pretty = os_raw
            .lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "SpaceOS 9".to_string());

        let (kernel, arch) = read_uname();
        let os_name = if arch.contains("64") {
            format!("{pretty} (64 位)")
        } else {
            pretty
        };

        Self {
            hostname,
            os_name,
            kernel_arch: format!("{kernel} ({arch})"),
            cpu: read_cpu_summary(),
            memory: read_mem_capacity(),
            disk: read_disk_capacity(),
        }
    }
}

fn read_uname() -> (String, String) {
    // SAFETY: `uts` is stack-allocated and zero-initialized before `libc::uname` populates its NUL-terminated fields.
    unsafe {
        let mut uts: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut uts) == 0 {
            let sys = CStr::from_ptr(uts.sysname.as_ptr()).to_string_lossy();
            let rel = CStr::from_ptr(uts.release.as_ptr()).to_string_lossy();
            let mach = CStr::from_ptr(uts.machine.as_ptr()).to_string_lossy();
            return (format!("{sys} {rel}"), mach.into_owned());
        }
    }
    ("SpaceKernel".to_string(), "x86_64".to_string())
}

fn read_cpu_summary() -> String {
    let raw = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut model = String::new();
    let mut cores = 0usize;
    for line in raw.lines() {
        if line.starts_with("processor") {
            cores += 1;
        } else if model.is_empty() {
            if let Some((k, val)) = line.split_once(':') {
                if matches!(k.trim(), "model name" | "Hardware" | "Processor" | "uarch") {
                    model = val.trim().to_string();
                }
            }
        }
    }
    let cores = cores.max(1);
    if model.is_empty() {
        format!("{cores} 核处理器")
    } else {
        format!("{model} ({cores} 核)")
    }
}

fn format_capacity_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: u64 = 1024 * 1024;
    if bytes >= (1024 * 1024 * 1024) {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else if bytes >= MIB {
        format!("{} MiB", bytes / MIB)
    } else {
        "未知".to_string()
    }
}

fn read_mem_capacity() -> String {
    let raw = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let total_kib = raw.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    });
    match total_kib {
        Some(kib) if kib > 0 => format_capacity_bytes(kib.saturating_mul(1024)),
        _ => "未知".to_string(),
    }
}

fn read_disk_capacity() -> String {
    // SAFETY: `c"/"` is a static NUL-terminated path and `vfs` is a valid writable `libc::statvfs` struct.
    unsafe {
        let mut vfs: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c"/".as_ptr(), &mut vfs) == 0 {
            let bsize = if vfs.f_frsize > 0 {
                vfs.f_frsize as u64
            } else {
                vfs.f_bsize as u64
            };
            let total = (vfs.f_blocks as u64).saturating_mul(bsize);
            if total > 0 {
                return format_capacity_bytes(total);
            }
        }
    }
    "未知".to_string()
}

fn apply_local_timezone(tz: &str) {
    if tz.is_empty() || tz.contains("..") || tz.starts_with('/') {
        return;
    }
    let posix_tz = uxconf::posix_tz_for_zone(tz);
    std::env::set_var("TZ", &posix_tz);
    // SAFETY: `tzset` refreshes libc timezone tables from `TZ` / `/etc/localtime` in this single-threaded process.
    unsafe {
        tzset();
    }
}

fn format_datetime_preview(cfg: &DesktopConfig) -> (String, String) {
    // SAFETY: `localtime_r` writes into stack-allocated zeroed `libc::tm` without touching shared static buffers.
    let (year, mon, mday, hour, min, sec, wday) = unsafe {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&stamp, &mut tm);
        (
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            tm.tm_wday,
        )
    };
    let time_str = if cfg.clock_24h {
        if cfg.clock_seconds {
            format!("{hour:02}:{min:02}:{sec:02}")
        } else {
            format!("{hour:02}:{min:02}")
        }
    } else {
        let period = if hour < 12 { "上午" } else { "下午" };
        let h12 = match hour % 12 {
            0 => 12,
            h => h,
        };
        if cfg.clock_seconds {
            format!("{period} {h12:02}:{min:02}:{sec:02}")
        } else {
            format!("{period} {h12:02}:{min:02}")
        }
    };
    let weekday = match wday {
        0 => "星期日",
        1 => "星期一",
        2 => "星期二",
        3 => "星期三",
        4 => "星期四",
        5 => "星期五",
        _ => "星期六",
    };
    let tz_label = TIMEZONES
        .iter()
        .find_map(|(id, name)| (*id == cfg.timezone).then_some(*name))
        .unwrap_or(cfg.timezone.as_str());
    let date_str = format!("{year} 年 {mon} 月 {mday} 日 {weekday} · {tz_label}");
    (time_str, date_str)
}

fn gcd_u32(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.max(1)
}

fn aspect_ratio_label(w: u16, h: u16) -> String {
    if w == 0 || h == 0 {
        return String::new();
    }
    let g = gcd_u32(u32::from(w), u32::from(h));
    let rw = u32::from(w) / g;
    let rh = u32::from(h) / g;
    match (rw, rh) {
        (8, 5) => "16:10".to_string(),
        _ => format!("{rw}:{rh}"),
    }
}

fn apply_display_mode(mode_arg: &str) {
    if !Path::new("/usr/bin/xrandr").exists() {
        return;
    }
    let ok = Command::new("/usr/bin/xrandr")
        .args(["--output", "Virtual-1", "--mode", mode_arg])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        let _ = Command::new("/usr/bin/xrandr")
            .args(["-s", mode_arg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn sidebar_width_for(bounds: Rect, style: SettingsRenderStyle) -> i32 {
    match style {
        SettingsRenderStyle::Classic => SIDEBAR_W,
        SettingsRenderStyle::Xj380 => {
            if bounds.w >= 960 {
                224
            } else {
                188
            }
        }
    }
}

fn header_height_for(style: SettingsRenderStyle) -> i32 {
    match style {
        SettingsRenderStyle::Classic => HEADER_H,
        SettingsRenderStyle::Xj380 => 54,
    }
}

fn scroll_viewport_in(bounds: Rect, style: SettingsRenderStyle) -> Rect {
    let sb = sidebar_width_for(bounds, style);
    let hh = header_height_for(style);
    Rect::new(
        bounds.x + sb,
        bounds.y + hh,
        (bounds.w - sb - 1).max(1),
        (bounds.h - hh - 1).max(1),
    )
}

fn load_wallpaper_thumbnails() -> [Option<Sprite>; 4] {
    std::array::from_fn(|i| {
        let path = format!("{WALLPAPER_DIR}/{}", WALLPAPERS[i].file);
        let decoded = icons::decode_jpeg(Path::new(&path))?;
        let opaque = decoded.scale_cover_opaque(THUMB_W, THUMB_H);
        let pixels = opaque.into_iter().map(|px| 0xff00_0000 | px).collect();
        Some(Sprite {
            width: THUMB_W as u32,
            height: THUMB_H as u32,
            pixels,
        })
    })
}

fn push_clipped_hit(hits: &mut Vec<HitBox>, viewport: Rect, rect: Rect, target: HitTarget) {
    let clipped = rect.intersect(viewport);
    if !clipped.is_empty() {
        hits.push(HitBox {
            rect: clipped,
            target,
        });
    }
}

fn find_hit(hits: &[HitBox], x: i32, y: i32) -> Option<HitTarget> {
    hits.iter()
        .rev()
        .find_map(|h| h.rect.contains(x, y).then_some(h.target))
}

fn nearest_preset_index(presets: &[(i64, &str)], value: i64) -> usize {
    presets
        .iter()
        .enumerate()
        .min_by_key(|(_, (v, _))| (*v - value).abs())
        .map_or(0, |(idx, _)| idx)
}

struct NetPageState {
    status: crate::netmgr::NetStatus,
    inputs: [String; 4],
    focused: Option<usize>,
    ping_note: String,
    last_refresh: Instant,
}

impl NetPageState {
    fn new(cfg: &DesktopConfig) -> Self {
        Self {
            status: crate::netmgr::query_status(),
            inputs: [
                cfg.net_address.clone(),
                cfg.net_prefixlen.to_string(),
                cfg.net_gateway.clone(),
                cfg.net_dns.clone(),
            ],
            focused: None,
            ping_note: String::new(),
            last_refresh: Instant::now(),
        }
    }

    fn sync_from_config(&mut self, cfg: &DesktopConfig) {
        self.inputs = [
            cfg.net_address.clone(),
            cfg.net_prefixlen.to_string(),
            cfg.net_gateway.clone(),
            cfg.net_dns.clone(),
        ];
    }

    fn refresh(&mut self) {
        self.status = crate::netmgr::query_status();
        self.last_refresh = Instant::now();
    }
}

/// Parse a category slug or identifier into a concrete [`Page`].
pub fn parse_initial_page(arg: Option<&str>) -> Page {
    match arg.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("network" | "net") => Page::Network,
        Some("desktop" | "panel") => Page::DesktopPanel,
        Some("windows" | "wm") => Page::Windows,
        Some("display") => Page::Display,
        Some("sound" | "audio" | "volume") => Page::Sound,
        Some("input") => Page::Input,
        Some("apps" | "applications") => Page::Applications,
        Some("datetime" | "time") => Page::DateTime,
        Some("logs" | "journal") => Page::Logs,
        Some("about") => Page::About,
        _ => Page::Appearance,
    }
}

fn apply_static_network_settings(
    host: &mut impl SettingsHost,
    net_state: &mut NetPageState,
    status: &mut String,
) {
    let addr_raw = net_state.inputs[0].trim();
    if addr_raw.parse::<Ipv4Addr>().is_err() {
        *status = "IPv4 地址格式无效（示例：10.0.2.15）".to_string();
        return;
    }
    let Some(prefixlen) = crate::netmgr::parse_prefix_or_netmask(&net_state.inputs[1]) else {
        *status = "子网前缀或掩码无效（示例：24 或 255.255.255.0）".to_string();
        return;
    };
    let gw_raw = net_state.inputs[2].trim();
    if !gw_raw.is_empty() && gw_raw.parse::<Ipv4Addr>().is_err() {
        *status = "默认网关格式无效（示例：10.0.2.2）".to_string();
        return;
    }
    let dns_raw = net_state.inputs[3].trim();
    if !dns_raw.is_empty() && dns_raw.parse::<Ipv4Addr>().is_err() {
        *status = "DNS 服务器地址格式无效（示例：10.0.2.3）".to_string();
        return;
    }

    let next_serial = host.config().net_apply_serial.wrapping_add(1);
    host.set_config_property("network", "/address", Value::String(addr_raw.to_string()));
    host.set_config_property("network", "/prefixlen", Value::Int(i64::from(prefixlen)));
    host.set_config_property("network", "/gateway", Value::String(gw_raw.to_string()));
    host.set_config_property("network", "/dns", Value::String(dns_raw.to_string()));
    host.set_config_property("network", "/apply-serial", Value::Int(next_serial));
    net_state.inputs[1] = prefixlen.to_string();

    match crate::netmgr::request_apply(host.config()) {
        Ok(msg) => *status = msg,
        Err(err) => *status = err,
    }
    net_state.refresh();
}

fn scrollbar_track_in(bounds: Rect, style: SettingsRenderStyle) -> Rect {
    let vp = scroll_viewport_in(bounds, style);
    Rect::new(
        bounds.right() - ui::SCROLLBAR_HIT_W - 2,
        vp.y + 4,
        ui::SCROLLBAR_HIT_W,
        (vp.h - 8).max(8),
    )
}

fn volume_from_slider_x(rect: Rect, x: i32) -> i32 {
    let frac = ((x - rect.x) as f32 / rect.w.max(1) as f32).clamp(0.0, 1.0);
    (frac * 100.0).round() as i32
}

/// Apply the master volume to the hardware mixer immediately. While a slider
/// drag is in progress (`persist == false`) the value is only staged locally;
/// `commit_master_volume` persists it once on release so dragging does not
/// fsync the config file and broadcast to every client per motion event.
fn apply_master_volume(host: &mut impl SettingsHost, vol: i32, persist: bool) {
    let clamped = vol.clamp(0, 100);
    if clamped != host.config().sound_master_volume {
        let value = Value::Int(i64::from(clamped));
        if persist {
            host.set_config_property("sound", "/master-volume", value);
        } else {
            host.stage_config_property("sound", "/master-volume", value);
        }
    }
    if host.config().sound_muted && clamped > 0 {
        if persist {
            host.set_config_property("sound", "/muted", Value::Bool(false));
        } else {
            host.stage_config_property("sound", "/muted", Value::Bool(false));
        }
    }
    let muted_effective = host.config().sound_muted && clamped == 0;
    crate::sound::apply_mixer_volume(clamped, muted_effective);
}

/// Persist the staged drag value (volume and the implied unmute) to `uxconfd`.
fn commit_master_volume(host: &mut impl SettingsHost) {
    let vol = host.config().sound_master_volume;
    let muted = host.config().sound_muted;
    host.set_config_property("sound", "/master-volume", Value::Int(i64::from(vol)));
    host.set_config_property("sound", "/muted", Value::Bool(muted));
}

/// Reusable System Settings UI state shared between `spui settings` and Start Screen `设置中心`.
pub struct SettingsState {
    pub page: Page,
    pub scroll: ui::SmoothScroll,
    vol_slider_rect: Option<Rect>,
    dragging_volume: bool,
    hover: Option<HitTarget>,
    hits: Vec<HitBox>,
    status: String,
    screen_size: (u16, u16),
    sound_card: String,
    last_clock_preview: String,
    thumbnails: [Option<Sprite>; 4],
    sys_info: SystemInfo,
    log_state: LogViewerState,
    net_state: NetPageState,
    last_bounds: Rect,
    last_style: SettingsRenderStyle,
    search_query: String,
}

impl SettingsState {
    pub fn new(host: &impl SettingsHost, initial_page: Option<&str>) -> Self {
        apply_local_timezone(&host.config().timezone);
        let cfg = host.config();
        Self {
            page: parse_initial_page(initial_page),
            scroll: ui::SmoothScroll::new("settings_scroll"),
            vol_slider_rect: None,
            dragging_volume: false,
            hover: None,
            hits: Vec::with_capacity(64),
            status: String::new(),
            screen_size: host.root_resolution(),
            sound_card: crate::sound::detected_card_name(),
            last_clock_preview: String::new(),
            thumbnails: load_wallpaper_thumbnails(),
            sys_info: SystemInfo::gather(),
            log_state: LogViewerState::load(),
            net_state: NetPageState::new(cfg),
            last_bounds: Rect::new(0, 0, 840, 580),
            last_style: SettingsRenderStyle::Classic,
            search_query: String::new(),
        }
    }

    pub fn set_page(&mut self, host: &impl SettingsHost, next_page: Page) {
        if self.page != next_page {
            self.page = next_page;
            self.scroll.snap_to(0.0);
        }
        self.net_state.focused = None;
        match next_page {
            Page::Display => self.screen_size = host.root_resolution(),
            Page::Network => self.net_state.refresh(),
            Page::Sound => self.sound_card = crate::sound::detected_card_name(),
            _ => {}
        }
    }

    pub fn set_search_query(&mut self, query: &str) {
        if self.search_query != query {
            self.search_query = query.to_string();
        }
    }

    pub fn is_text_input_focused(&self) -> bool {
        self.page == Page::Network && self.net_state.focused.is_some()
    }

    pub fn clear_text_focus(&mut self) {
        self.net_state.focused = None;
    }

    pub fn refresh_net_now(&mut self) -> bool {
        self.net_state.refresh();
        self.page == Page::Network
    }

    pub fn on_config_changed(&mut self, host: &impl SettingsHost) {
        let cfg = host.config();
        apply_local_timezone(&cfg.timezone);
        self.screen_size = host.root_resolution();
        self.sound_card = crate::sound::detected_card_name();
        if self.net_state.focused.is_none() {
            self.net_state.sync_from_config(cfg);
        }
        self.net_state.refresh();
    }

    /// Periodic tick for live clock preview and network status polling.
    pub fn tick(&mut self, cfg: &DesktopConfig) -> bool {
        let mut dirty = false;
        if self.page == Page::DateTime {
            let (now_str, _) = format_datetime_preview(cfg);
            if now_str != self.last_clock_preview {
                self.last_clock_preview = now_str;
                dirty = true;
            }
        } else if self.page == Page::Network
            && self.net_state.last_refresh.elapsed() >= Duration::from_millis(1800)
        {
            self.net_state.refresh();
            dirty = true;
        }
        dirty
    }

    pub fn on_wheel(&mut self, x: i32, y: i32, delta: i32) -> bool {
        let vp = scroll_viewport_in(self.last_bounds, self.last_style);
        vp.contains(x, y) && self.scroll.wheel(delta as f32 * 22.0)
    }

    pub fn on_motion(
        &mut self,
        host: &mut impl SettingsHost,
        x: i32,
        y: i32,
    ) -> (bool, CursorKind) {
        if self.dragging_volume {
            if let Some(sr) = self.vol_slider_rect {
                let vol = volume_from_slider_x(sr, x);
                let mut changed = false;
                if vol != host.config().sound_master_volume || host.config().sound_muted {
                    apply_master_volume(host, vol, false);
                    changed = true;
                }
                return (changed, CursorKind::Pointer);
            }
            return (false, CursorKind::Pointer);
        }
        let mut redraw = self.scroll.handle_motion(x, y);
        let next = if !self.scroll.is_dragging() && self.last_bounds.contains(x, y) {
            find_hit(&self.hits, x, y)
        } else {
            None
        };
        let cursor = if self.scroll.is_hovered() || self.scroll.is_dragging() {
            CursorKind::Pointer
        } else {
            match next {
                Some(HitTarget::NetInput(_)) => CursorKind::Text,
                Some(_) => CursorKind::Pointer,
                None => CursorKind::Default,
            }
        };
        if next != self.hover {
            self.hover = next;
            redraw = true;
        }
        (redraw, cursor)
    }

    pub fn on_button(
        &mut self,
        host: &mut impl SettingsHost,
        x: i32,
        y: i32,
        button: u8,
        down: bool,
    ) -> SettingsAction {
        let mut action = SettingsAction::default();
        if button == 1 && !down && self.dragging_volume {
            self.dragging_volume = false;
            commit_master_volume(host);
            crate::sound::play(host.config(), crate::sound::SystemSound::Volume);
            action.redraw = true;
            return action;
        }
        if self.scroll.handle_button(x, y, button, down) {
            action.cursor = Some(if self.scroll.is_hovered() || self.scroll.is_dragging() {
                CursorKind::Pointer
            } else {
                CursorKind::Default
            });
            action.redraw = true;
            return action;
        }
        if button == 1 && down && self.last_bounds.contains(x, y) {
            let hit = find_hit(&self.hits, x, y);
            if !matches!(hit, Some(HitTarget::NetInput(_))) && self.net_state.focused.is_some() {
                self.net_state.focused = None;
                action.redraw = true;
            }
            if matches!(hit, Some(HitTarget::SoundMasterSlider)) {
                if let Some(sr) = self.vol_slider_rect {
                    self.dragging_volume = true;
                    apply_master_volume(host, volume_from_slider_x(sr, x), false);
                    action.cursor = Some(CursorKind::Pointer);
                    action.redraw = true;
                }
            } else if let Some(target) = hit {
                self.handle_click(host, target, &mut action);
            }
        }
        action
    }

    pub fn on_key(&mut self, host: &mut impl SettingsHost, key: Key) -> bool {
        if self.page != Page::Network {
            return false;
        }
        let Some(idx) = self.net_state.focused else {
            return false;
        };
        if key.is_escape() {
            self.net_state.focused = None;
            return true;
        } else if key.is_tab() {
            self.net_state.focused = Some(if key.shift() {
                (idx + 3) % 4
            } else {
                (idx + 1) % 4
            });
            return true;
        } else if key.is_backspace() {
            self.net_state.inputs[idx].pop();
            return true;
        } else if key.ctrl_byte() == Some(21) {
            self.net_state.inputs[idx].clear();
            return true;
        } else if key.is_enter() {
            apply_static_network_settings(host, &mut self.net_state, &mut self.status);
            return true;
        } else if !key.control() && !key.alt() {
            if let Some(ch) = key.text() {
                if (ch.is_ascii_digit() || matches!(ch, '.' | '/'))
                    && self.net_state.inputs[idx].len() < 31
                {
                    self.net_state.inputs[idx].push(ch);
                    return true;
                }
            }
        }
        false
    }

    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        bounds: Rect,
        style: SettingsRenderStyle,
    ) {
        self.last_bounds = bounds;
        self.last_style = style;
        let prev_scroll = self.scroll.offset_i32();
        self.draw_pass(canvas, text, theme, cfg, bounds, style);
        if self.scroll.offset_i32() != prev_scroll {
            self.draw_pass(canvas, text, theme, cfg, bounds, style);
        }
    }
}

pub fn run(initial_page: Option<&str>) -> Result<()> {
    let mut app = App::new("系统设置", Icon::Settings, 840, 580)?;
    app.min_width = 760;
    app.min_height = 500;

    let mut state = SettingsState::new(&app, initial_page);
    let net_fd = crate::netmgr::open_event_socket();

    loop {
        if state.tick(&app.config) {
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            let bounds = app.content();
            state.draw(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &app.config,
                bounds,
                SettingsRenderStyle::Classic,
            );
            app.draw_chrome();
            app.present()?;
        }

        if app.close {
            break;
        }

        let timeout_ms = if ui::has_active_animations() {
            16
        } else if state.page == Page::DateTime && app.config.clock_seconds {
            250
        } else {
            500
        };
        let (events, net_ready) = app.pump_with_fd(net_fd, timeout_ms)?;
        if net_ready {
            if let Some(fd) = net_fd {
                let _ = crate::netmgr::drain_events(fd);
            }
            if state.refresh_net_now() {
                app.needs_redraw = true;
            }
        }
        for event in events {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } => {
                    state.screen_size = app.root_resolution();
                    app.needs_redraw = true;
                }
                UiEvent::ConfigChanged => {
                    state.on_config_changed(&app);
                    app.needs_redraw = true;
                }
                UiEvent::Key(key) => {
                    if state.on_key(&mut app, key) {
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Wheel { x, y, delta } => {
                    if state.on_wheel(x, y, delta) {
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Motion { x, y, .. } => {
                    let (redraw, cursor) = state.on_motion(&mut app, x, y);
                    app.set_content_cursor(cursor);
                    if redraw {
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x, y, button, down, ..
                } => {
                    let action = state.on_button(&mut app, x, y, button, down);
                    if let Some(c) = action.cursor {
                        app.set_content_cursor(c);
                    }
                    if action.redraw {
                        app.needs_redraw = true;
                    }
                }
            }
        }
    }

    if let Some(fd) = net_fd {
        // SAFETY: Owned Netlink event descriptor closed once on exit.
        unsafe {
            libc::close(fd);
        }
    }
    Ok(())
}

impl SettingsState {
    fn handle_click(
        &mut self,
        host: &mut impl SettingsHost,
        target: HitTarget,
        action: &mut SettingsAction,
    ) {
        self.status.clear();
        let mut play_click = true;
        match target {
            HitTarget::Nav(i) => {
                let next_page = NAV[i].0;
                self.set_page(host, next_page);
            }
            HitTarget::BackToHome => {
                action.back_to_home = true;
            }
            HitTarget::SearchJump(next_page) => {
                self.search_query.clear();
                self.set_page(host, next_page);
            }
            HitTarget::ResetPage => {
                for ch in self.page.channels() {
                    host.reset_config_channel(ch);
                }
                match self.page {
                    Page::Display => {
                        apply_display_mode(&host.config().display_resolution);
                        self.screen_size = host.root_resolution();
                    }
                    Page::DateTime => apply_local_timezone(&host.config().timezone),
                    Page::Network => {
                        self.net_state.focused = None;
                        self.net_state.sync_from_config(host.config());
                        let _ = crate::netmgr::request_apply(host.config());
                        self.net_state.refresh();
                    }
                    Page::Sound => {
                        self.sound_card = crate::sound::detected_card_name();
                        crate::sound::apply_mixer_volume(
                            host.config().sound_master_volume,
                            host.config().sound_muted,
                        );
                    }
                    _ => {}
                }
                self.status = "已恢复默认设置".to_string();
            }
            HitTarget::ThemePreset(i) => {
                let preset = THEME_PRESETS[i];
                host.apply_theme_preset(preset);
                self.status = format!("已切换为「{}」系统主题", preset.name());
            }
            HitTarget::ThemeMode(mode) => {
                host.set_config_property(
                    "appearance",
                    "/theme/mode",
                    Value::String(mode.as_str().to_string()),
                );
            }
            HitTarget::Accent(i) => {
                let (rgb, _) = ACCENT_PRESETS[i];
                host.set_config_property(
                    "appearance",
                    "/theme/accent",
                    Value::String(format_hex_rgb(rgb)),
                );
            }
            HitTarget::FontScale(i) => {
                host.set_config_property("appearance", "/font/scale", Value::Int(FONT_SCALES[i].0));
            }
            HitTarget::NetEnabled => {
                let next = !host.config().net_enabled;
                host.set_config_property("network", "/enabled", Value::Bool(next));
                match crate::netmgr::request_connect(None, next) {
                    Ok(msg) => self.status = msg,
                    Err(err) => self.status = err,
                }
                self.net_state.refresh();
            }
            HitTarget::NetMode(i) => {
                let mode = if i == 0 {
                    NetMode::Dhcp
                } else {
                    NetMode::Static
                };
                if host.config().net_mode != mode {
                    host.set_config_property(
                        "network",
                        "/mode",
                        Value::String(mode.as_str().to_string()),
                    );
                    if mode == NetMode::Dhcp {
                        self.net_state.focused = None;
                        match crate::netmgr::request_apply(host.config()) {
                            Ok(msg) => self.status = msg,
                            Err(err) => self.status = err,
                        }
                        self.net_state.refresh();
                    } else {
                        if let Some((ip, plen)) = self.net_state.status.ipv4 {
                            self.net_state.inputs[0] = ip.to_string();
                            self.net_state.inputs[1] = plen.to_string();
                        }
                        if let Some(gw) = self.net_state.status.gateway {
                            self.net_state.inputs[2] = gw.to_string();
                        }
                        if let Some(dns) = self.net_state.status.dns {
                            self.net_state.inputs[3] = dns.to_string();
                        }
                        self.net_state.focused = Some(0);
                        self.status = "编辑静态 IPv4 参数后点击「应用网络配置」".to_string();
                    }
                }
            }
            HitTarget::NetInput(i) => {
                self.net_state.focused = Some(i.min(3));
            }
            HitTarget::NetApply => {
                self.net_state.focused = None;
                apply_static_network_settings(host, &mut self.net_state, &mut self.status);
            }
            HitTarget::NetRenew => {
                match crate::netmgr::request_renew(None) {
                    Ok(msg) => self.status = msg,
                    Err(err) => self.status = err,
                }
                self.net_state.refresh();
            }
            HitTarget::NetRefresh => {
                self.net_state.refresh();
                self.status = "网络状态已刷新".to_string();
            }
            HitTarget::NetPingGateway => {
                let target = self
                    .net_state
                    .status
                    .gateway
                    .unwrap_or_else(|| Ipv4Addr::new(10, 0, 2, 2));
                match crate::netmgr::ping_once(target, 1000) {
                    Ok(rtt) => {
                        let msg = format!("网关 {target} 可达 · RTT {rtt:.2} ms");
                        self.net_state.ping_note = msg.clone();
                        self.status = msg;
                    }
                    Err(err) => {
                        self.net_state.ping_note = err.clone();
                        self.status = err;
                    }
                }
                self.net_state.refresh();
            }
            HitTarget::BackdropStyle(i) => {
                let style = [
                    BackdropStyle::Cover,
                    BackdropStyle::Contain,
                    BackdropStyle::Stretch,
                    BackdropStyle::Solid,
                ][i];
                host.set_config_property(
                    "desktop",
                    "/backdrop/style",
                    Value::String(style.as_str().to_string()),
                );
            }
            HitTarget::Wallpaper(i) => {
                host.set_config_property(
                    "desktop",
                    "/backdrop/wallpaper",
                    Value::String(WALLPAPERS[i].file.to_string()),
                );
                if host.config().backdrop_style == BackdropStyle::Solid {
                    host.set_config_property(
                        "desktop",
                        "/backdrop/style",
                        Value::String(BackdropStyle::Cover.as_str().to_string()),
                    );
                }
            }
            HitTarget::SolidColor(i) => {
                let (rgb, _) = SOLID_BG_PRESETS[i];
                host.set_config_property(
                    "desktop",
                    "/backdrop/solid-color",
                    Value::String(format_hex_rgb(rgb)),
                );
            }
            HitTarget::DesktopIconsVisible => {
                let next = !host.config().desktop_icons_visible;
                host.set_config_property("desktop", "/icons/visible", Value::Bool(next));
            }
            HitTarget::DesktopIconSize(i) => {
                host.set_config_property(
                    "desktop",
                    "/icons/size",
                    Value::Int(DESKTOP_ICON_SIZES[i].0),
                );
            }
            HitTarget::PanelPosition(i) => {
                let pos = if i == 0 {
                    PanelPosition::Bottom
                } else {
                    PanelPosition::Top
                };
                host.set_config_property(
                    "panel",
                    "/position",
                    Value::String(pos.as_str().to_string()),
                );
            }
            HitTarget::PanelSize(i) => {
                host.set_config_property("panel", "/size", Value::Int(PANEL_SIZES[i].0));
            }
            HitTarget::PanelShowPinned => {
                let next = !host.config().panel_show_pinned;
                host.set_config_property("panel", "/show-pinned", Value::Bool(next));
            }
            HitTarget::PanelStartFullscreen => {
                let next = !host.config().panel_start_fullscreen;
                host.set_config_property("panel", "/start-fullscreen", Value::Bool(next));
            }
            HitTarget::WmButtonLayout(i) => {
                let layout = if i == 0 {
                    ButtonLayout::Right
                } else {
                    ButtonLayout::Left
                };
                host.set_config_property(
                    "wm",
                    "/button-layout",
                    Value::String(layout.as_str().to_string()),
                );
            }
            HitTarget::WmDoubleClick(i) => {
                let act = [
                    DoubleClickAction::Maximize,
                    DoubleClickAction::Minimize,
                    DoubleClickAction::None,
                ][i];
                host.set_config_property(
                    "wm",
                    "/double-click-action",
                    Value::String(act.as_str().to_string()),
                );
            }
            HitTarget::WmFocusMode(i) => {
                let fm = if i == 0 {
                    FocusMode::Click
                } else {
                    FocusMode::Sloppy
                };
                host.set_config_property(
                    "wm",
                    "/focus-mode",
                    Value::String(fm.as_str().to_string()),
                );
            }
            HitTarget::WmAutoRaise => {
                let next = !host.config().wm_auto_raise;
                host.set_config_property("wm", "/auto-raise", Value::Bool(next));
            }
            HitTarget::WmPlacement(i) => {
                let pl = if i == 0 {
                    WindowPlacement::Cascade
                } else {
                    WindowPlacement::Center
                };
                host.set_config_property(
                    "wm",
                    "/placement",
                    Value::String(pl.as_str().to_string()),
                );
            }
            HitTarget::Resolution(i) => {
                let (mode_arg, _) = RESOLUTIONS[i];
                host.set_config_property(
                    "display",
                    "/resolution",
                    Value::String(mode_arg.to_string()),
                );
                apply_display_mode(mode_arg);
                self.screen_size = host.root_resolution();
            }
            HitTarget::BlankTimeout(i) => {
                host.set_config_property(
                    "display",
                    "/blank-timeout-sec",
                    Value::Int(BLANK_TIMEOUTS[i].0),
                );
            }
            HitTarget::MouseHandedness(i) => {
                host.set_config_property("input", "/mouse/left-handed", Value::Bool(i == 1));
            }
            HitTarget::MouseSpeed(i) => {
                host.set_config_property("input", "/mouse/speed", Value::Int(MOUSE_SPEEDS[i].0));
            }
            HitTarget::DoubleClickSpeed(i) => {
                host.set_config_property(
                    "input",
                    "/mouse/double-click-ms",
                    Value::Int(DOUBLE_CLICK_SPEEDS[i].0),
                );
            }
            HitTarget::NaturalScroll => {
                let next = !host.config().mouse_natural_scroll;
                host.set_config_property("input", "/mouse/natural-scroll", Value::Bool(next));
            }
            HitTarget::KeyRepeatEnabled => {
                let next = !host.config().key_repeat_enabled;
                host.set_config_property("input", "/keyboard/repeat-enabled", Value::Bool(next));
            }
            HitTarget::KeyRepeatDelay(i) => {
                host.set_config_property(
                    "input",
                    "/keyboard/repeat-delay-ms",
                    Value::Int(KEY_DELAYS[i].0),
                );
            }
            HitTarget::KeyRepeatRate(i) => {
                host.set_config_property(
                    "input",
                    "/keyboard/repeat-rate-hz",
                    Value::Int(KEY_RATES[i].0),
                );
            }
            HitTarget::TermFontSize(i) => {
                host.set_config_property(
                    "terminal",
                    "/font-size",
                    Value::Int(CODE_FONT_SIZES[i].0),
                );
            }
            HitTarget::TermCursorStyle(i) => {
                let cs = [
                    CursorStyle::Block,
                    CursorStyle::Beam,
                    CursorStyle::Underline,
                ][i];
                host.set_config_property(
                    "terminal",
                    "/cursor-style",
                    Value::String(cs.as_str().to_string()),
                );
            }
            HitTarget::TermScrollback(i) => {
                host.set_config_property(
                    "terminal",
                    "/scrollback-lines",
                    Value::Int(SCROLLBACK_PRESETS[i].0),
                );
            }
            HitTarget::FilesShowHidden => {
                let next = !host.config().files_show_hidden;
                host.set_config_property("files", "/show-hidden", Value::Bool(next));
            }
            HitTarget::FilesFoldersFirst => {
                let next = !host.config().files_folders_first;
                host.set_config_property("files", "/folders-first", Value::Bool(next));
            }
            HitTarget::FilesSingleClick => {
                let next = !host.config().files_single_click;
                host.set_config_property("files", "/single-click-open", Value::Bool(next));
            }
            HitTarget::EditorFontSize(i) => {
                host.set_config_property("editor", "/font-size", Value::Int(CODE_FONT_SIZES[i].0));
            }
            HitTarget::EditorLineNumbers => {
                let next = !host.config().editor_show_line_numbers;
                host.set_config_property("editor", "/show-line-numbers", Value::Bool(next));
            }
            HitTarget::EditorHighlightLine => {
                let next = !host.config().editor_highlight_active_line;
                host.set_config_property("editor", "/highlight-active-line", Value::Bool(next));
            }
            HitTarget::EditorTabWidth(i) => {
                host.set_config_property("editor", "/tab-width", Value::Int(TAB_WIDTHS[i].0));
            }
            HitTarget::Clock24h => {
                let next = !host.config().clock_24h;
                host.set_config_property("datetime", "/clock-24h", Value::Bool(next));
            }
            HitTarget::ClockSeconds => {
                let next = !host.config().clock_seconds;
                host.set_config_property("datetime", "/clock-seconds", Value::Bool(next));
            }
            HitTarget::ClockShowDate => {
                let next = !host.config().clock_show_date;
                host.set_config_property("datetime", "/clock-show-date", Value::Bool(next));
            }
            HitTarget::Timezone(i) => {
                let (tz_id, _) = TIMEZONES[i];
                apply_local_timezone(tz_id);
                host.set_config_property("datetime", "/timezone", Value::String(tz_id.to_string()));
            }
            HitTarget::FactoryReset => {
                let now = std::time::Instant::now();
                let confirmed = {
                    let mut armed = FACTORY_RESET_ARMED.lock().unwrap();
                    let confirm = armed.is_some_and(|at| {
                        now.duration_since(at) < std::time::Duration::from_secs(15)
                    });
                    *armed = if confirm { None } else { Some(now) };
                    confirm
                };
                if !confirmed {
                    self.status = "再次点击「重置…」确认恢复出厂设置（15 秒内有效）".to_string();
                } else {
                    match host.factory_reset() {
                        Some(Ok(())) => {
                            self.status =
                                "恢复出厂设置已登记；重启后清除本地账户并进入首次设置向导"
                                    .to_string();
                        }
                        Some(Err(error)) => {
                            self.status = format!("无法登记恢复出厂设置：{error}");
                        }
                        None => {
                            self.status = "配置服务不可用，无法登记恢复出厂设置".to_string();
                        }
                    }
                }
            }
            HitTarget::JumpToLogs => {
                self.page = Page::Logs;
                self.scroll.snap_to(0.0);
            }
            HitTarget::LogFilter(idx) => {
                self.log_state.filter = match idx {
                    1 => LogFilterType::Kernel,
                    2 => LogFilterType::Services,
                    3 => LogFilterType::ErrorsWarns,
                    _ => LogFilterType::All,
                };
                self.scroll.snap_to(0.0);
            }
            HitTarget::LogRefresh => {
                self.log_state = LogViewerState::load();
                self.status = format!("日志已刷新（共 {} 条记录）", self.log_state.entries.len());
            }
            HitTarget::LogSelect(idx) => {
                if self.log_state.selected == Some(idx) {
                    self.log_state.selected = None;
                } else {
                    self.log_state.selected = Some(idx);
                }
            }
            HitTarget::SoundMuted => {
                let next = !host.config().sound_muted;
                host.set_config_property("sound", "/muted", Value::Bool(next));
                crate::sound::apply_mixer_volume(host.config().sound_master_volume, next);
                if !next {
                    crate::sound::play(host.config(), crate::sound::SystemSound::Volume);
                }
                play_click = false;
            }
            HitTarget::SoundMasterSlider => {
                play_click = false;
            }
            HitTarget::SoundVolumePreset(i) => {
                apply_master_volume(host, VOLUME_PRESETS[i].0 as i32, true);
                crate::sound::play(host.config(), crate::sound::SystemSound::Volume);
                play_click = false;
            }
            HitTarget::SoundEnabled => {
                let next = !host.config().sound_enabled;
                host.set_config_property("sound", "/system-sounds-enabled", Value::Bool(next));
            }
            HitTarget::SoundEventStartup => {
                let next = !host.config().sound_event_startup;
                host.set_config_property("sound", "/event-startup", Value::Bool(next));
            }
            HitTarget::SoundEventUi => {
                let next = !host.config().sound_event_ui;
                host.set_config_property("sound", "/event-ui", Value::Bool(next));
            }
            HitTarget::SoundEventNotify => {
                let next = !host.config().sound_event_notify;
                host.set_config_property("sound", "/event-notify", Value::Bool(next));
            }
            HitTarget::SoundEventAlert => {
                let next = !host.config().sound_event_alert;
                host.set_config_property("sound", "/event-alert", Value::Bool(next));
            }
            HitTarget::SoundEventVolume => {
                let next = !host.config().sound_event_volume;
                host.set_config_property("sound", "/event-volume", Value::Bool(next));
            }
            HitTarget::SoundPreview(idx) => {
                let (sfx, label) = match idx {
                    0 => (crate::sound::SystemSound::Startup, "开机迎宾和弦"),
                    1 => (crate::sound::SystemSound::Click, "界面点击反馈"),
                    2 => (crate::sound::SystemSound::Notify, "系统通知提示"),
                    3 => (crate::sound::SystemSound::Alert, "警告与终端响铃"),
                    _ => (crate::sound::SystemSound::Volume, "音量调节提示"),
                };
                if crate::sound::preview(host.config(), sfx) {
                    self.status = format!("正在试听：{label}");
                } else if host.config().sound_muted || host.config().sound_master_volume <= 0 {
                    self.status = "当前处于静音或音量为 0%，无法试听".to_string();
                } else {
                    self.status = "未检测到音频输出设备或音效资源".to_string();
                }
                play_click = false;
            }
        }
        if play_click {
            crate::sound::play(host.config(), crate::sound::SystemSound::Click);
        }
        action.redraw = true;
    }
}

fn draw_section_heading(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    x: i32,
    y: i32,
    title: &str,
) -> i32 {
    text.draw(
        canvas,
        title,
        (x + 2) as f32,
        (y + 16) as f32,
        11.5,
        theme.text_muted,
        Face::Bold,
    );
    y + 24
}

fn draw_row_divider(canvas: &mut Canvas, theme: &Theme, card_x: i32, card_w: i32, y: i32) {
    canvas.fill(
        Rect::new(card_x + 16, y, (card_w - 32).max(1), 1),
        theme.window_border,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_row_labels(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    card_x: i32,
    y: i32,
    max_w: i32,
    title: &str,
    subtitle: &str,
) {
    let shown_title = text.truncate(title, max_w.max(60) as f32, 12.5, Face::Ui);
    text.draw(
        canvas,
        &shown_title,
        (card_x + 16) as f32,
        (y + 22) as f32,
        12.5,
        theme.text,
        Face::Ui,
    );
    if !subtitle.is_empty() {
        let shown_sub = text.truncate(subtitle, max_w.max(60) as f32, 11.0, Face::Ui);
        text.draw(
            canvas,
            &shown_sub,
            (card_x + 16) as f32,
            (y + 41) as f32,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_toggle_row(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    viewport: Rect,
    hits: &mut Vec<HitBox>,
    card_x: i32,
    card_w: i32,
    y: i32,
    title: &str,
    subtitle: &str,
    checked: bool,
    target: HitTarget,
    hover: Option<HitTarget>,
) {
    let sw_rect = Rect::new(card_x + card_w - 16 - 42, y + (ROW_H - 22) / 2, 42, 22);
    let label_max_w = sw_rect.x - (card_x + 16) - 16;
    draw_row_labels(canvas, text, theme, card_x, y, label_max_w, title, subtitle);
    ui::toggle_switch_with_id(
        canvas,
        theme,
        target,
        sw_rect,
        checked,
        hover == Some(target),
    );
    push_clipped_hit(hits, viewport, sw_rect, target);
}

#[allow(clippy::too_many_arguments)]
fn draw_segmented_row<F>(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    viewport: Rect,
    hits: &mut Vec<HitBox>,
    card_x: i32,
    card_w: i32,
    y: i32,
    title: &str,
    subtitle: &str,
    ctrl_w: i32,
    labels: &[&str],
    selected: usize,
    make_target: F,
    hover: Option<HitTarget>,
) where
    F: Fn(usize) -> HitTarget,
{
    let seg_rect = Rect::new(
        card_x + card_w - 16 - ctrl_w,
        y + (ROW_H - 30) / 2,
        ctrl_w,
        30,
    );
    let label_max_w = seg_rect.x - (card_x + 16) - 14;
    draw_row_labels(canvas, text, theme, card_x, y, label_max_w, title, subtitle);

    let mut hover_idx = None;
    for i in 0..labels.len() {
        let item_target = make_target(i);
        if hover == Some(item_target) {
            hover_idx = Some(i);
        }
        let item_rect = ui::segmented_item_rect(seg_rect, labels.len(), i);
        push_clipped_hit(hits, viewport, item_rect, item_target);
    }
    ui::segmented_control_with_id(
        canvas, text, theme, title, seg_rect, labels, selected, hover_idx,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_sound_event_row(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    viewport: Rect,
    hits: &mut Vec<HitBox>,
    card_x: i32,
    card_w: i32,
    y: i32,
    title: &str,
    subtitle: &str,
    checked: bool,
    toggle_target: HitTarget,
    preview_target: HitTarget,
    hover: Option<HitTarget>,
) {
    let sw_rect = Rect::new(card_x + card_w - 16 - 42, y + (ROW_H - 22) / 2, 42, 22);
    let btn_rect = Rect::new(sw_rect.x - 10 - 58, y + (ROW_H - 26) / 2, 58, 26);
    let label_max_w = btn_rect.x - (card_x + 16) - 14;
    draw_row_labels(canvas, text, theme, card_x, y, label_max_w, title, subtitle);
    ui::button(
        canvas,
        text,
        theme,
        btn_rect,
        "试听",
        None,
        false,
        hover == Some(preview_target),
    );
    push_clipped_hit(hits, viewport, btn_rect, preview_target);
    ui::toggle_switch_with_id(
        canvas,
        theme,
        toggle_target,
        sw_rect,
        checked,
        hover == Some(toggle_target),
    );
    push_clipped_hit(hits, viewport, sw_rect, toggle_target);
}

impl SettingsState {
    #[allow(clippy::too_many_arguments)]
    fn draw_pass(
        &mut self,
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        cfg: &DesktopConfig,
        bounds: Rect,
        style: SettingsRenderStyle,
    ) {
        self.hits.clear();
        self.vol_slider_rect = None;

        let scroll_y = self.scroll.offset_i32().max(0);
        let sidebar_w = sidebar_width_for(bounds, style);
        let header_h = header_height_for(style);

        let ctrl_r = theme.radius_control();
        let is_win98 = theme.is_win98();
        match style {
            SettingsRenderStyle::Classic => {
                canvas.fill(bounds, theme.window);
                canvas.fill(
                    Rect::new(bounds.x, bounds.y, sidebar_w, bounds.h),
                    theme.window_alt,
                );
                canvas.fill(
                    Rect::new(bounds.x + sidebar_w - 1, bounds.y, 1, bounds.h),
                    theme.window_border,
                );
                text.draw(
                    canvas,
                    "系统设置",
                    (bounds.x + 18) as f32,
                    (bounds.y + 24) as f32,
                    11.0,
                    theme.text_muted,
                    Face::Bold,
                );
            }
            SettingsRenderStyle::Xj380 => {
                if is_win98 {
                    canvas.fill(bounds, theme.window);
                    canvas.fill(
                        Rect::new(bounds.x, bounds.y, sidebar_w, bounds.h),
                        theme.window_alt,
                    );
                    canvas.fill(
                        Rect::new(bounds.x + sidebar_w - 1, bounds.y, 1, bounds.h),
                        theme.bevel_shadow,
                    );
                } else {
                    let pane_bg = if theme.mode == ThemeMode::Dark {
                        0x101620
                    } else {
                        0xf3f5f8
                    };
                    let nav_bg = if theme.mode == ThemeMode::Dark {
                        0x0d131d
                    } else {
                        0xe7ecf3
                    };
                    canvas.fill_alpha(bounds, pane_bg, 0.88);
                    canvas.fill_alpha(
                        Rect::new(bounds.x, bounds.y, sidebar_w, bounds.h),
                        nav_bg,
                        0.94,
                    );
                    canvas.fill(
                        Rect::new(bounds.x + sidebar_w - 1, bounds.y, 1, bounds.h),
                        theme.panel_border,
                    );
                }
                text.draw(
                    canvas,
                    "控制分类 / SETTINGS",
                    (bounds.x + 18) as f32,
                    (bounds.y + 24) as f32,
                    11.0,
                    theme.accent,
                    Face::Bold,
                );
            }
        }

        let nav_top = bounds.y + 36;
        let active_idx = NAV
            .iter()
            .position(|(id, _, _)| *id == self.page)
            .unwrap_or(0);
        if !is_win98 {
            let target_nav_y = (nav_top + active_idx as i32 * 38) as f32;
            let nav_y = ui::animate_float("settings_nav", target_nav_y, ui::MOTION_INDICATOR);
            let active_pill = Rect::new(bounds.x + 10, nav_y.round() as i32, sidebar_w - 20, 34);
            canvas.rounded_rect(active_pill, ctrl_r, theme.accent_soft, 1.0);
            canvas.fill(
                Rect::new(active_pill.x, active_pill.y + 7, 3, active_pill.h - 14),
                theme.accent,
            );
        }

        for (i, (id, label, icon)) in NAV.iter().enumerate() {
            let r = Rect::new(bounds.x + 10, nav_top + i as i32 * 38, sidebar_w - 20, 34);
            let active = self.page == *id;
            let target = HitTarget::Nav(i);
            let fg = if is_win98 {
                ui::list_item_bg(canvas, theme, r, active, self.hover == Some(target))
            } else {
                if self.hover == Some(target) && !active {
                    canvas.rounded_rect(r, ctrl_r, theme.panel_hover, 1.0);
                }
                if active { theme.text } else { theme.text_dim }
            };
            icons::draw(canvas, *icon, r.x + 10, r.y + (r.h - 18) / 2, 18, None);
            text.draw(
                canvas,
                label,
                (r.x + 36) as f32,
                (r.y + 22) as f32,
                12.5,
                fg,
                if active { Face::Bold } else { Face::Ui },
            );
            self.hits.push(HitBox { rect: r, target });
        }

        let pane_x = bounds.x + sidebar_w;
        let pane_w = (bounds.w - sidebar_w).max(320);
        let max_card_w = if style == SettingsRenderStyle::Xj380 {
            920
        } else {
            pane_w - 48
        };
        let card_w = (pane_w - 48).clamp(320, max_card_w.max(320));
        let content_x = pane_x
            + if style == SettingsRenderStyle::Xj380 && pane_w > card_w + 56 {
                (pane_w - card_w) / 2
            } else {
                24
            };

        // Fixed page header bar.
        if style == SettingsRenderStyle::Xj380 {
            let nav_icon = NAV
                .iter()
                .find_map(|(p, _, ic)| (*p == self.page).then_some(*ic))
                .unwrap_or(Icon::Settings);
            icons::draw(canvas, nav_icon, content_x, bounds.y + 15, 22, None);
            text.draw(
                canvas,
                self.page.title(),
                (content_x + 30) as f32,
                (bounds.y + 33) as f32,
                17.5,
                theme.text,
                Face::Bold,
            );
        } else {
            text.draw(
                canvas,
                self.page.title(),
                content_x as f32,
                (bounds.y + 33) as f32,
                16.5,
                theme.text,
                Face::Bold,
            );
        }

        let mut right_cursor = bounds.right() - 24;
        if style == SettingsRenderStyle::Xj380 {
            let home_rect = Rect::new(right_cursor - 96, bounds.y + 12, 96, 28);
            ui::button(
                canvas,
                text,
                theme,
                home_rect,
                "← 返回主页",
                None,
                false,
                self.hover == Some(HitTarget::BackToHome),
            );
            self.hits.push(HitBox {
                rect: home_rect,
                target: HitTarget::BackToHome,
            });
            right_cursor = home_rect.x - 10;
        }
        if !self.page.channels().is_empty() {
            let reset_rect = Rect::new(right_cursor - 84, bounds.y + 12, 84, 28);
            ui::button(
                canvas,
                text,
                theme,
                reset_rect,
                "恢复默认",
                None,
                false,
                self.hover == Some(HitTarget::ResetPage),
            );
            self.hits.push(HitBox {
                rect: reset_rect,
                target: HitTarget::ResetPage,
            });
            right_cursor = reset_rect.x - 14;
        }
        if !self.status.is_empty() {
            let max_st = (right_cursor - content_x - 180).max(80) as f32;
            let shown = text.truncate(&self.status, max_st, 11.5, Face::Ui);
            let st_w = text.measure(&shown, 11.5, Face::Ui);
            text.draw(
                canvas,
                &shown,
                right_cursor as f32 - st_w,
                (bounds.y + 31) as f32,
                11.5,
                theme.accent,
                Face::Ui,
            );
        }
        if scroll_y > 0 || style == SettingsRenderStyle::Xj380 {
            canvas.fill(
                Rect::new(pane_x, bounds.y + header_h - 1, pane_w, 1),
                theme.window_border,
            );
        }

        let viewport = scroll_viewport_in(bounds, style);
        let start_y = viewport.y + 6 - scroll_y;
        let mut end_y = start_y;

        let hover = self.hover;
        let page = self.page;
        let dragging_volume = self.dragging_volume;
        let thumbnails = &self.thumbnails;
        let screen_size = self.screen_size;
        let sound_card = self.sound_card.as_str();
        let sys_info = &self.sys_info;
        let log_state = &self.log_state;
        let net_state = &self.net_state;
        let hits = &mut self.hits;
        let vol_slider_rect = &mut self.vol_slider_rect;
        let search_matches = if style == SettingsRenderStyle::Xj380 {
            search_settings(&self.search_query)
        } else {
            Vec::new()
        };

        canvas.with_clip(viewport, |canvas| {
            let mut y = start_y;
            if !search_matches.is_empty() {
                let shown_count = search_matches.len().min(6);
                let row_h = 44;
                y = draw_section_heading(
                    canvas,
                    text,
                    theme,
                    content_x,
                    y,
                    "全局设置搜索匹配（点击快速跳转分类）",
                );
                let s_card = Rect::new(content_x, y, card_w, row_h * shown_count as i32);
                ui::card(canvas, theme, s_card);
                for (idx, item) in search_matches.iter().take(shown_count).enumerate() {
                    let ry = y + idx as i32 * row_h;
                    if idx > 0 {
                        draw_row_divider(canvas, theme, content_x, card_w, ry);
                    }
                    let row_r = Rect::new(content_x + 6, ry + 4, card_w - 12, row_h - 8);
                    let target = HitTarget::SearchJump(item.page);
                    let row_fg =
                        ui::list_item_bg(canvas, theme, row_r, hover == Some(target), false);
                    let badge_r = Rect::new(content_x + 14, ry + 11, 82, 22);
                    canvas.rounded_rect(badge_r, if is_win98 { 0 } else { 4 }, theme.accent_soft, 1.0);
                    text.draw_centered(
                        canvas,
                        item.category,
                        badge_r.x as f32 + badge_r.w as f32 * 0.5,
                        (badge_r.y + 15) as f32,
                        10.5,
                        theme.accent,
                        Face::Bold,
                    );
                    let summary = format!("{}  ·  {}", item.title, item.subtitle);
                    let shown = text.truncate(&summary, (card_w - 126) as f32, 12.0, Face::Ui);
                    text.draw(
                        canvas,
                        &shown,
                        (badge_r.right() + 12) as f32,
                        (ry + 26) as f32,
                        12.0,
                        row_fg,
                        Face::Ui,
                    );
                    push_clipped_hit(hits, viewport, row_r, target);
                }
                y += s_card.h + 14;
            }
            match page {
                Page::Appearance => {
                    y = draw_section_heading(canvas, text, theme, content_x, y, "系统主题方案");
                    let c_theme = Rect::new(content_x, y, card_w, 134);
                    ui::card(canvas, theme, c_theme);
                    let active_preset = THEME_PRESETS
                        .iter()
                        .copied()
                        .find(|p| p.id() == cfg.theme_id)
                        .unwrap_or_else(|| ThemePreset::from_style(cfg.theme_style));
                    let preset_gap = 14;
                    let preset_w = (card_w - 32 - preset_gap) / 2;
                    for (idx, preset) in THEME_PRESETS.iter().enumerate() {
                        let card_r = Rect::new(
                            content_x + 16 + idx as i32 * (preset_w + preset_gap),
                            y + 14,
                            preset_w,
                            82,
                        );
                        let selected = *preset == active_preset;
                        let target = HitTarget::ThemePreset(idx);
                        let hovered = hover == Some(target);
                        if is_win98 {
                            canvas.fill(
                                card_r,
                                if selected {
                                    theme.window_alt
                                } else if hovered {
                                    theme.panel_hover
                                } else {
                                    theme.panel
                                },
                            );
                            if selected {
                                ui::sunken_bevel(canvas, theme, card_r);
                            } else {
                                ui::raised_bevel(canvas, theme, card_r);
                            }
                        } else {
                            let fill = if selected {
                                theme.accent_soft
                            } else if hovered {
                                theme.panel_hover
                            } else {
                                theme.window_alt
                            };
                            canvas.rounded_rect(card_r, ctrl_r, fill, 1.0);
                            canvas.rounded_border(
                                card_r,
                                ctrl_r,
                                if selected {
                                    theme.accent
                                } else {
                                    theme.window_border
                                },
                                1.0,
                                if selected { 2 } else { 1 },
                            );
                        }

                        // Mini desktop + window preview.
                        let mini = Rect::new(card_r.x + 12, card_r.y + 13, 82, 56);
                        match preset {
                            ThemePreset::Spui4 => {
                                canvas.rounded_rect(mini, 4, 0x0f172a, 1.0);
                                let win_m = Rect::new(mini.x + 10, mini.y + 8, mini.w - 20, 32);
                                canvas.rounded_rect(win_m, 3, 0x1e293b, 1.0);
                                canvas.fill(
                                    Rect::new(win_m.x + 1, win_m.y + 1, win_m.w - 2, 7),
                                    0x2563eb,
                                );
                                canvas.fill(
                                    Rect::new(mini.x + 2, mini.bottom() - 9, mini.w - 4, 7),
                                    0x111827,
                                );
                                canvas.rounded_border(mini, 4, theme.panel_border, 1.0, 1);
                            }
                            ThemePreset::Win98 => {
                                canvas.fill(mini, 0x008080);
                                let win_m = Rect::new(mini.x + 8, mini.y + 7, mini.w - 16, 33);
                                canvas.fill(win_m, 0xc0c0c0);
                                canvas.bevel_1px(win_m, 0xffffff, 0x404040);
                                canvas.horizontal_gradient(
                                    Rect::new(win_m.x + 2, win_m.y + 2, win_m.w - 4, 8),
                                    0x000080,
                                    0x1084d0,
                                );
                                canvas.fill(
                                    Rect::new(mini.x + 1, mini.bottom() - 9, mini.w - 2, 8),
                                    0xc0c0c0,
                                );
                                canvas.fill(
                                    Rect::new(mini.x + 3, mini.bottom() - 7, 16, 5),
                                    0xdfdfdf,
                                );
                                canvas.bevel_1px(mini, 0x808080, 0xffffff);
                            }
                        }

                        let tx = mini.right() + 12;
                        text.draw(
                            canvas,
                            preset.name(),
                            tx as f32,
                            (card_r.y + 28) as f32,
                            13.5,
                            if selected { theme.accent } else { theme.text },
                            Face::Bold,
                        );
                        let sub_max = (card_r.right() - tx - 10).max(40) as f32;
                        let sub_shown = text.truncate(preset.subtitle(), sub_max, 11.0, Face::Ui);
                        text.draw(
                            canvas,
                            &sub_shown,
                            tx as f32,
                            (card_r.y + 47) as f32,
                            11.0,
                            theme.text_dim,
                            Face::Ui,
                        );
                        if selected {
                            text.draw(
                                canvas,
                                "● 当前启用",
                                tx as f32,
                                (card_r.y + 66) as f32,
                                10.5,
                                theme.accent,
                                Face::Bold,
                            );
                        }
                        push_clipped_hit(hits, viewport, card_r, target);
                    }
                    text.draw(
                        canvas,
                        "切换主题将立即更新桌面、任务栏、开始菜单、全部应用、登录及关机界面；深浅/强调色仍可微调",
                        (content_x + 16) as f32,
                        (y + 116) as f32,
                        11.0,
                        theme.text_muted,
                        Face::Ui,
                    );
                    y += c_theme.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "主题与色彩");
                    let c1 = Rect::new(content_x, y, card_w, 166);
                    ui::card(canvas, theme, c1);
                    text.draw(
                        canvas,
                        "外观模式",
                        (content_x + 16) as f32,
                        (y + 26) as f32,
                        12.5,
                        theme.text,
                        Face::Ui,
                    );
                    for (idx, (mode, label, bg_col, chrome_col, fg_col)) in [
                        (ThemeMode::Dark, "深色", 0x18181b, 0x242429, 0xf4f4f5),
                        (ThemeMode::Light, "浅色", 0xf4f4f6, 0xe4e4ea, 0x18181b),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let card_r = Rect::new(
                            content_x + card_w - 16 - 332 + idx as i32 * 170,
                            y + 14,
                            162,
                            66,
                        );
                        let selected = cfg.mode == mode;
                        let target = HitTarget::ThemeMode(mode);
                        let fill = if selected {
                            theme.accent_soft
                        } else if hover == Some(target) {
                            theme.panel_hover
                        } else {
                            theme.window_alt
                        };
                        if is_win98 {
                            canvas.fill(
                                card_r,
                                if selected {
                                    theme.window_alt
                                } else {
                                    theme.panel
                                },
                            );
                            if selected {
                                ui::sunken_bevel(canvas, theme, card_r);
                            } else {
                                ui::raised_bevel(canvas, theme, card_r);
                            }
                        } else {
                            canvas.rounded_rect(card_r, ctrl_r, fill, 1.0);
                            canvas.rounded_border(
                                card_r,
                                ctrl_r,
                                if selected {
                                    theme.accent
                                } else {
                                    theme.window_border
                                },
                                1.0,
                                if selected { 2 } else { 1 },
                            );
                        }
                        let mini = Rect::new(card_r.x + 12, card_r.y + 11, 56, 44);
                        let mini_r = if is_win98 { 0 } else { 4 };
                        canvas.rounded_rect(mini, mini_r, bg_col, 1.0);
                        canvas.fill(
                            Rect::new(mini.x + 1, mini.y + 1, mini.w - 2, 10),
                            chrome_col,
                        );
                        canvas.rounded_border(mini, mini_r, theme.panel_border, 1.0, 1);
                        canvas.fill(Rect::new(mini.x + 7, mini.y + 18, 22, 4), cfg.accent);
                        canvas.fill(Rect::new(mini.x + 7, mini.y + 27, 36, 3), fg_col);
                        canvas.fill(Rect::new(mini.x + 7, mini.y + 34, 28, 3), chrome_col);
                        text.draw(
                            canvas,
                            label,
                            (mini.right() + 14) as f32,
                            (card_r.y + 39) as f32,
                            12.5,
                            theme.text,
                            if selected { Face::Bold } else { Face::Ui },
                        );
                        push_clipped_hit(hits, viewport, card_r, target);
                    }

                    draw_row_divider(canvas, theme, content_x, card_w, y + 94);
                    let active_accent_name = ACCENT_PRESETS
                        .iter()
                        .find_map(|(rgb, name)| (*rgb == cfg.accent).then_some(*name))
                        .unwrap_or("自定义");
                    let accent_sub = format!("当前：{active_accent_name}");
                    draw_row_labels(
                        canvas,
                        text,
                        theme,
                        content_x,
                        y + 102,
                        200,
                        "强调色",
                        &accent_sub,
                    );
                    let sw_total_w = ACCENT_PRESETS.len() as i32 * 34 - 6;
                    let sw_start_x = content_x + card_w - 16 - sw_total_w;
                    for (i, (rgb, _)) in ACCENT_PRESETS.iter().enumerate() {
                        let sw_r = Rect::new(sw_start_x + i as i32 * 34, y + 116, 28, 28);
                        let target = HitTarget::Accent(i);
                        ui::color_swatch(
                            canvas,
                            theme,
                            sw_r,
                            *rgb,
                            cfg.accent == *rgb,
                            hover == Some(target),
                        );
                        push_clipped_hit(hits, viewport, sw_r, target);
                    }
                    y += c1.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "字体与缩放");
                    let c2 = Rect::new(content_x, y, card_w, ROW_H);
                    ui::card(canvas, theme, c2);
                    let scale_idx = nearest_preset_index(&FONT_SCALES, i64::from(cfg.font_scale));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "界面字号",
                        "调整菜单、面板与系统控件的文字大小",
                        216,
                        &["紧凑", "标准", "大号"],
                        scale_idx,
                        HitTarget::FontScale,
                        hover,
                    );
                    y += c2.h + 18;
                }

                Page::Network => {
                    let st = &net_state.status;
                    y = draw_section_heading(
                        canvas,
                        text,
                        theme,
                        content_x,
                        y,
                        "有线以太网与 NetworkManager 服务",
                    );

                    let connected = st.connected();
                    let header_h = 68;
                    let detail_row_h = 38;
                    let detail_count = if net_state.ping_note.is_empty() { 5 } else { 6 };
                    let c_status =
                        Rect::new(content_x, y, card_w, header_h + detail_count * detail_row_h);
                    ui::card(canvas, theme, c_status);

                    let badge_r = Rect::new(content_x + 16, y + 14, 40, 40);
                    let badge_bg = if connected {
                        theme.accent_soft
                    } else {
                        theme.window_alt
                    };
                    canvas.rounded_rect(badge_r, ctrl_r, badge_bg, 1.0);
                    let (net_ic, ic_tint) = if connected {
                        (Icon::NetworkWired, theme.success)
                    } else {
                        (Icon::NetworkOffline, theme.text_muted)
                    };
                    icons::draw(
                        canvas,
                        net_ic,
                        badge_r.x + 10,
                        badge_r.y + 10,
                        20,
                        Some(ic_tint),
                    );

                    let show_renew =
                        cfg.net_enabled && cfg.net_mode == NetMode::Dhcp && st.has_device;
                    let refresh_btn = Rect::new(c_status.right() - 16 - 62, y + 19, 62, 30);
                    let ping_btn = Rect::new(refresh_btn.x - 8 - 82, y + 19, 82, 30);
                    let renew_btn = Rect::new(ping_btn.x - 8 - 76, y + 19, 76, 30);

                    ui::button(
                        canvas,
                        text,
                        theme,
                        refresh_btn,
                        "刷新",
                        None,
                        false,
                        hover == Some(HitTarget::NetRefresh),
                    );
                    push_clipped_hit(hits, viewport, refresh_btn, HitTarget::NetRefresh);

                    ui::button(
                        canvas,
                        text,
                        theme,
                        ping_btn,
                        "探测网关",
                        None,
                        false,
                        hover == Some(HitTarget::NetPingGateway),
                    );
                    push_clipped_hit(hits, viewport, ping_btn, HitTarget::NetPingGateway);

                    let right_limit = if show_renew {
                        ui::button(
                            canvas,
                            text,
                            theme,
                            renew_btn,
                            "续租",
                            None,
                            false,
                            hover == Some(HitTarget::NetRenew),
                        );
                        push_clipped_hit(hits, viewport, renew_btn, HitTarget::NetRenew);
                        renew_btn.x - 12
                    } else {
                        ping_btn.x - 12
                    };

                    let title_str = if !st.has_device {
                        "未检测到有线以太网适配器".to_string()
                    } else {
                        format!("以太网 ({}) · {}", st.ifname, st.device_state.label_zh())
                    };
                    let daemon_tag = if st.daemon_running {
                        "NetworkManager.service 运行中"
                    } else {
                        "NetworkManager 未运行"
                    };
                    let sub_str = if st.mode_note.is_empty() {
                        format!(
                            "{daemon_tag} · 状态 {} ({})",
                            st.device_state.code(),
                            st.device_state.as_str()
                        )
                    } else {
                        format!("{daemon_tag} · {}", st.mode_note)
                    };
                    let text_x = badge_r.right() + 14;
                    let avail_w = (right_limit - text_x).max(80) as f32;
                    let shown_t = text.truncate(&title_str, avail_w, 13.0, Face::Bold);
                    text.draw(
                        canvas,
                        &shown_t,
                        text_x as f32,
                        (y + 30) as f32,
                        13.0,
                        theme.text,
                        Face::Bold,
                    );
                    let shown_s = text.truncate(&sub_str, avail_w, 11.0, Face::Ui);
                    text.draw(
                        canvas,
                        &shown_s,
                        text_x as f32,
                        (y + 49) as f32,
                        11.0,
                        if connected {
                            theme.text_dim
                        } else {
                            theme.text_muted
                        },
                        Face::Ui,
                    );

                    let hw_val = if st.has_device {
                        format!(
                            "{}  ·  MTU {}  ·  载波 {}",
                            st.mac,
                            st.mtu,
                            if st.carrier { "已连接" } else { "未插入" }
                        )
                    } else {
                        "--".to_string()
                    };
                    let ipv4_val = match st.ipv4 {
                        Some((ip, plen)) => match st.gateway {
                            Some(gw) => format!("{ip}/{plen}  ·  默认网关 {gw}"),
                            None => format!("{ip}/{plen}  ·  无默认网关"),
                        },
                        None => "未分配 IPv4 地址".to_string(),
                    };
                    let dns_v6_val = {
                        let dns_part = st
                            .dns
                            .map(|d| d.to_string())
                            .unwrap_or_else(|| "--".to_string());
                        let v6_part = st
                            .ipv6_ll
                            .map(|(ip6, p6)| format!("{ip6}/{p6}"))
                            .unwrap_or_else(|| "--".to_string());
                        format!("DNS {dns_part}  ·  IPv6 {v6_part}")
                    };
                    let lease_val = if st.lease_secs > 0 {
                        format!(
                            "{}  ·  租期 {}s (剩余 {}s · T1={}s / T2={}s)",
                            st.profile_name,
                            st.lease_secs,
                            st.lease_remaining_secs,
                            st.t1_secs,
                            st.t2_secs
                        )
                    } else if st.has_device {
                        format!("{}  ·  {}", st.profile_name, st.device_state.as_str())
                    } else {
                        "--".to_string()
                    };
                    let traffic_val = format!(
                        "接收 {} ({} 包)  ·  发送 {} ({} 包)",
                        crate::netmgr::NetStatus::format_bytes(st.rx_bytes),
                        st.rx_packets,
                        crate::netmgr::NetStatus::format_bytes(st.tx_bytes),
                        st.tx_packets
                    );

                    let mut rows: Vec<(&str, String, u32)> = vec![
                        ("物理链路", hw_val, theme.text),
                        ("IPv4 与路由", ipv4_val, theme.text),
                        ("DNS 与 IPv6", dns_v6_val, theme.text),
                        ("连接配置与租约", lease_val, theme.text_dim),
                        ("实时流量统计", traffic_val, theme.text_dim),
                    ];
                    if !net_state.ping_note.is_empty() {
                        let col = if net_state.ping_note.contains("可达") {
                            theme.success
                        } else {
                            theme.warning
                        };
                        rows.push(("网关连通性探测", net_state.ping_note.clone(), col));
                    }

                    for (idx, (k, v, v_col)) in rows.iter().enumerate() {
                        let ry = y + header_h + idx as i32 * detail_row_h;
                        draw_row_divider(canvas, theme, content_x, card_w, ry);
                        text.draw(
                            canvas,
                            k,
                            (content_x + 16) as f32,
                            (ry + 24) as f32,
                            11.5,
                            theme.text_muted,
                            Face::Ui,
                        );
                        let shown_v = text.truncate(v, (card_w - 154) as f32, 11.5, Face::Ui);
                        let vw = text.measure(&shown_v, 11.5, Face::Ui);
                        text.draw(
                            canvas,
                            &shown_v,
                            (content_x + card_w - 16) as f32 - vw,
                            (ry + 24) as f32,
                            11.5,
                            *v_col,
                            Face::Ui,
                        );
                    }
                    y += c_status.h + 16;

                    y = draw_section_heading(
                        canvas,
                        text,
                        theme,
                        content_x,
                        y,
                        "连接控制与 IPv4 参数",
                    );
                    let is_static = cfg.net_mode == NetMode::Static;
                    let static_extra_h = if is_static { 4 * ROW_H + 54 } else { 0 };
                    let c_cfg = Rect::new(content_x, y, card_w, ROW_H * 2 + static_extra_h);
                    ui::card(canvas, theme, c_cfg);

                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "启用有线网络",
                        "由 systemg 托管的 NetworkManager 服务自动维护链路、DHCP 与路由",
                        cfg.net_enabled,
                        HitTarget::NetEnabled,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "IPv4 配置方式",
                        "自动通过 RFC 2131 DHCPv4 获取或手动指定静态 IPv4 地址",
                        224,
                        &["自动 (DHCP)", "手动 (静态)"],
                        if is_static { 1 } else { 0 },
                        HitTarget::NetMode,
                        hover,
                    );

                    if is_static {
                        let fields: [(&str, &str, &str); 4] = [
                            ("IPv4 地址", "本机静态 IPv4 地址", "例如 10.0.2.15"),
                            (
                                "子网前缀或掩码",
                                "支持 CIDR 前缀长度或点分十进制掩码",
                                "24 或 255.255.255.0",
                            ),
                            (
                                "默认网关",
                                "下一跳路由器 IPv4 地址（可选）",
                                "例如 10.0.2.2",
                            ),
                            (
                                "首选 DNS",
                                "写入 /etc/resolv.conf 的域名服务器",
                                "例如 10.0.2.3",
                            ),
                        ];
                        let input_w = 224;
                        for (idx, (label, sub, placeholder)) in fields.iter().enumerate() {
                            let fy = y + ROW_H * (2 + idx as i32);
                            draw_row_divider(canvas, theme, content_x, card_w, fy);
                            let in_rect = Rect::new(
                                content_x + card_w - 16 - input_w,
                                fy + (ROW_H - 32) / 2,
                                input_w,
                                32,
                            );
                            let max_lw = in_rect.x - (content_x + 16) - 14;
                            draw_row_labels(canvas, text, theme, content_x, fy, max_lw, label, sub);
                            ui::text_input(
                                canvas,
                                text,
                                theme,
                                in_rect,
                                &net_state.inputs[idx],
                                placeholder,
                                net_state.focused == Some(idx),
                                None,
                            );
                            push_clipped_hit(hits, viewport, in_rect, HitTarget::NetInput(idx));
                        }

                        let action_y = y + ROW_H * 6;
                        draw_row_divider(canvas, theme, content_x, card_w, action_y);
                        text.draw(
                            canvas,
                            "按 Tab 切换输入框，按 Enter 或点击右侧按钮提交至 NetworkManager",
                            (content_x + 16) as f32,
                            (action_y + 31) as f32,
                            11.0,
                            theme.text_muted,
                            Face::Ui,
                        );
                        let apply_btn =
                            Rect::new(content_x + card_w - 16 - 118, action_y + 10, 118, 34);
                        ui::button(
                            canvas,
                            text,
                            theme,
                            apply_btn,
                            "应用网络配置",
                            None,
                            true,
                            hover == Some(HitTarget::NetApply),
                        );
                        push_clipped_hit(hits, viewport, apply_btn, HitTarget::NetApply);
                    }

                    y += c_cfg.h + 18;
                }

                Page::DesktopPanel => {
                    y = draw_section_heading(canvas, text, theme, content_x, y, "桌面背景");
                    let c1 = Rect::new(content_x, y, card_w, ROW_H + 226 + ROW_H);
                    ui::card(canvas, theme, c1);
                    let style_idx = match cfg.backdrop_style {
                        BackdropStyle::Cover => 0,
                        BackdropStyle::Contain => 1,
                        BackdropStyle::Stretch => 2,
                        BackdropStyle::Solid => 3,
                    };
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "背景填充方式",
                        "设置壁纸在屏幕上的缩放方式或使用纯色桌面",
                        264,
                        &["填充", "适应", "拉伸", "纯色"],
                        style_idx,
                        HitTarget::BackdropStyle,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);

                    let grid_y = y + ROW_H + 14;
                    let cell_w = (card_w - 32 - 12) / 2;
                    let cell_h = 92;
                    for (i, wp) in WALLPAPERS.iter().enumerate() {
                        let col = (i % 2) as i32;
                        let row = (i / 2) as i32;
                        let r = Rect::new(
                            content_x + 16 + col * (cell_w + 12),
                            grid_y + row * (cell_h + 12),
                            cell_w,
                            cell_h,
                        );
                        let selected =
                            cfg.backdrop_style != BackdropStyle::Solid && cfg.wallpaper == wp.file;
                        let target = HitTarget::Wallpaper(i);
                        let bg = if selected {
                            theme.accent_soft
                        } else if hover == Some(target) {
                            theme.panel_hover
                        } else {
                            theme.window_alt
                        };
                        canvas.rounded_rect(r, 6, bg, 1.0);
                        canvas.rounded_border(
                            r,
                            6,
                            if selected {
                                theme.accent
                            } else {
                                theme.window_border
                            },
                            1.0,
                            if selected { 2 } else { 1 },
                        );
                        let tb = Rect::new(r.x + 10, r.y + 13, THUMB_W as i32, THUMB_H as i32);
                        if let Some(sprite) = &thumbnails[i] {
                            canvas.blit_sprite(tb.x, tb.y, sprite);
                            canvas.border_1px(tb, theme.window_border);
                        } else {
                            canvas.rounded_rect(tb, 4, theme.bg_bottom, 1.0);
                        }
                        let tx = (tb.right() + 12) as f32;
                        text.draw(
                            canvas,
                            wp.label,
                            tx,
                            (r.y + 40) as f32,
                            12.5,
                            theme.text,
                            Face::Bold,
                        );
                        text.draw(
                            canvas,
                            if selected {
                                "当前壁纸"
                            } else {
                                "点击应用"
                            },
                            tx,
                            (r.y + 60) as f32,
                            11.0,
                            if selected {
                                theme.accent
                            } else {
                                theme.text_muted
                            },
                            Face::Ui,
                        );
                        push_clipped_hit(hits, viewport, r, target);
                    }

                    let solid_y = y + ROW_H + 226;
                    draw_row_divider(canvas, theme, content_x, card_w, solid_y);
                    draw_row_labels(
                        canvas,
                        text,
                        theme,
                        content_x,
                        solid_y,
                        240,
                        "背景底色",
                        "用于纯色背景与适应模式留边底色",
                    );
                    let sw_total = SOLID_BG_PRESETS.len() as i32 * 34 - 6;
                    let sw_x0 = content_x + card_w - 16 - sw_total;
                    for (i, (rgb, _)) in SOLID_BG_PRESETS.iter().enumerate() {
                        let sw_r = Rect::new(sw_x0 + i as i32 * 34, solid_y + 14, 28, 28);
                        let target = HitTarget::SolidColor(i);
                        ui::color_swatch(
                            canvas,
                            theme,
                            sw_r,
                            *rgb,
                            cfg.solid_color == *rgb,
                            hover == Some(target),
                        );
                        push_clipped_hit(hits, viewport, sw_r, target);
                    }
                    y += c1.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "桌面图标");
                    let c2 = Rect::new(content_x, y, card_w, ROW_H * 2);
                    ui::card(canvas, theme, c2);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "显示桌面图标",
                        "在桌面上展示常用应用程序的快捷方式",
                        cfg.desktop_icons_visible,
                        HitTarget::DesktopIconsVisible,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    let icon_sz_idx =
                        nearest_preset_index(&DESKTOP_ICON_SIZES, i64::from(cfg.desktop_icon_size));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "图标尺寸",
                        "调整桌面快捷方式图标大小与排列间距",
                        204,
                        &["小", "标准", "大"],
                        icon_sz_idx,
                        HitTarget::DesktopIconSize,
                        hover,
                    );
                    y += c2.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "系统面板");
                    let c3 = Rect::new(content_x, y, card_w, ROW_H * 4);
                    ui::card(canvas, theme, c3);
                    let pos_idx = if cfg.panel_position == PanelPosition::Bottom {
                        0
                    } else {
                        1
                    };
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "面板位置",
                        "将任务栏停靠在屏幕底部或屏幕顶部",
                        204,
                        &["屏幕底部", "屏幕顶部"],
                        pos_idx,
                        HitTarget::PanelPosition,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    let panel_sz_idx =
                        nearest_preset_index(&PANEL_SIZES, i64::from(cfg.panel_size));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "面板高度",
                        "调整系统面板高度与栏内按钮尺寸",
                        216,
                        &["紧凑", "标准", "宽大"],
                        panel_sz_idx,
                        HitTarget::PanelSize,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 2,
                        "显示固定应用",
                        "在开始菜单按钮右侧显示常用应用快速启动图标",
                        cfg.panel_show_pinned,
                        HitTarget::PanelShowPinned,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 3);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 3,
                        "全屏开始屏幕",
                        "开启 Windows 8 风格全屏开始屏幕与设置中心，关闭则使用经典弹出菜单",
                        cfg.panel_start_fullscreen,
                        HitTarget::PanelStartFullscreen,
                        hover,
                    );
                    y += c3.h + 18;
                }

                Page::Windows => {
                    y = draw_section_heading(canvas, text, theme, content_x, y, "标题栏");
                    let c1 = Rect::new(content_x, y, card_w, ROW_H * 2);
                    ui::card(canvas, theme, c1);
                    let btn_idx = if cfg.wm_button_layout == ButtonLayout::Right {
                        0
                    } else {
                        1
                    };
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "标题栏按钮位置",
                        "设置最小化、最大化与关闭按钮所在的窗口侧边",
                        188,
                        &["右侧", "左侧"],
                        btn_idx,
                        HitTarget::WmButtonLayout,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    let dc_idx = match cfg.wm_double_click {
                        DoubleClickAction::Maximize => 0,
                        DoubleClickAction::Minimize => 1,
                        DoubleClickAction::None => 2,
                    };
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "双击标题栏动作",
                        "双击窗口顶部标题栏空白区域时执行的操作",
                        240,
                        &["最大化", "最小化", "无操作"],
                        dc_idx,
                        HitTarget::WmDoubleClick,
                        hover,
                    );
                    y += c1.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "焦点与布局");
                    let c2 = Rect::new(content_x, y, card_w, ROW_H * 3);
                    ui::card(canvas, theme, c2);
                    let focus_idx = if cfg.wm_focus_mode == FocusMode::Click {
                        0
                    } else {
                        1
                    };
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "窗口焦点策略",
                        "点击窗口激活焦点，或随鼠标移入自动切换焦点",
                        216,
                        &["点击激活", "悬停激活"],
                        focus_idx,
                        HitTarget::WmFocusMode,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "悬停时自动置顶",
                        "在悬停激活模式下将获得焦点的窗口自动提升至顶层",
                        cfg.wm_auto_raise,
                        HitTarget::WmAutoRaise,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);
                    let place_idx = if cfg.wm_placement == WindowPlacement::Cascade {
                        0
                    } else {
                        1
                    };
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 2,
                        "新窗口默认位置",
                        "打开新应用窗口时的初始位置策略",
                        216,
                        &["自动错开", "屏幕居中"],
                        place_idx,
                        HitTarget::WmPlacement,
                        hover,
                    );
                    y += c2.h + 18;
                }

                Page::Display => {
                    let ratio = aspect_ratio_label(screen_size.0, screen_size.1);
                    let c1 = Rect::new(content_x, y + 4, card_w, 108);
                    ui::card(canvas, theme, c1);

                    // Clean monitor bezel illustration showing screen aspect ratio.
                    let mon = Rect::new(content_x + 20, c1.y + 18, 104, 64);
                    canvas.rounded_rect(mon, 5, theme.accent_soft, 1.0);
                    canvas.rounded_border(mon, 5, theme.accent, 1.0, 2);
                    canvas.fill(
                        Rect::new(mon.x + 42, mon.bottom(), 20, 6),
                        theme.window_border,
                    );
                    canvas.fill(
                        Rect::new(mon.x + 30, mon.bottom() + 6, 44, 2),
                        theme.window_border,
                    );
                    if !ratio.is_empty() {
                        text.draw_centered(
                            canvas,
                            &ratio,
                            mon.x as f32 + mon.w as f32 * 0.5,
                            mon.y as f32 + 37.0,
                            12.0,
                            theme.accent,
                            Face::Bold,
                        );
                    }

                    text.draw(
                        canvas,
                        "主显示器",
                        (mon.right() + 22) as f32,
                        (c1.y + 46) as f32,
                        14.5,
                        theme.text,
                        Face::Bold,
                    );
                    let res_summary = if ratio.is_empty() {
                        format!("当前分辨率：{} × {}", screen_size.0, screen_size.1)
                    } else {
                        format!(
                            "当前分辨率：{} × {} ({ratio})",
                            screen_size.0, screen_size.1
                        )
                    };
                    text.draw(
                        canvas,
                        &res_summary,
                        (mon.right() + 22) as f32,
                        (c1.y + 71) as f32,
                        12.0,
                        theme.text_dim,
                        Face::Ui,
                    );
                    y = c1.bottom() + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "分辨率");
                    let c2 = Rect::new(content_x, y, card_w, 118);
                    ui::card(canvas, theme, c2);
                    let active_mode = format!("{}x{}", screen_size.0, screen_size.1);
                    let btn_w = (card_w - 32 - 20) / 3;
                    for (i, (mode_arg, label)) in RESOLUTIONS.iter().enumerate() {
                        let col = (i % 3) as i32;
                        let row = (i / 3) as i32;
                        let r = Rect::new(
                            content_x + 16 + col * (btn_w + 10),
                            y + 15 + row * 48,
                            btn_w,
                            36,
                        );
                        let is_current = active_mode == *mode_arg;
                        let target = HitTarget::Resolution(i);
                        ui::button(
                            canvas,
                            text,
                            theme,
                            r,
                            label,
                            None,
                            is_current,
                            hover == Some(target),
                        );
                        push_clipped_hit(hits, viewport, r, target);
                    }
                    y += c2.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "节能");
                    let c3 = Rect::new(content_x, y, card_w, ROW_H);
                    ui::card(canvas, theme, c3);
                    let blank_idx = nearest_preset_index(
                        &BLANK_TIMEOUTS,
                        i64::from(cfg.display_blank_timeout_sec),
                    );
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "自动关闭屏幕",
                        "空闲无操作达到指定时长后关闭显示器背光",
                        280,
                        &["从不", "5 分钟", "10 分钟", "15 分钟"],
                        blank_idx,
                        HitTarget::BlankTimeout,
                        hover,
                    );
                    y += c3.h + 18;
                }

                Page::Sound => {
                    y = draw_section_heading(canvas, text, theme, content_x, y, "主音量与输出设备");
                    let c1 = Rect::new(content_x, y, card_w, ROW_H * 4);
                    ui::card(canvas, theme, c1);

                    // Row 0: Detected output device + live state badge
                    draw_row_labels(
                        canvas,
                        text,
                        theme,
                        content_x,
                        y,
                        (card_w - 200).max(160),
                        "音频输出设备",
                        sound_card,
                    );
                    let dev_badge = if cfg.sound_muted {
                        "已静音".to_string()
                    } else {
                        format!("{}% · 硬件混音已开启", cfg.sound_master_volume)
                    };
                    text.draw_right(
                        canvas,
                        &dev_badge,
                        (content_x + card_w - 16) as f32,
                        (y + 33) as f32,
                        11.5,
                        if cfg.sound_muted {
                            theme.danger
                        } else {
                            theme.accent
                        },
                        Face::Bold,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);

                    // Row 1: Mute switch
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "静音全部声音",
                        "将硬件主混音器静音并暂停所有系统提示音效",
                        cfg.sound_muted,
                        HitTarget::SoundMuted,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);

                    // Row 2: Draggable Master Volume slider + percentage readout
                    let vol_row_y = y + ROW_H * 2;
                    let slider_w = (card_w - 246).clamp(160, 290);
                    let vol_lbl_w = 46;
                    let slider_rect = Rect::new(
                        content_x + card_w - 16 - vol_lbl_w - slider_w,
                        vol_row_y + (ROW_H - 22) / 2,
                        slider_w,
                        22,
                    );
                    draw_row_labels(
                        canvas,
                        text,
                        theme,
                        content_x,
                        vol_row_y,
                        (slider_rect.x - content_x - 28).max(80),
                        "主音量",
                        "拖动滑块或点击滑轨连续调节输出音量",
                    );
                    let slider_prog = if cfg.sound_muted {
                        0.0
                    } else {
                        (cfg.sound_master_volume.clamp(0, 100) as f32) / 100.0
                    };
                    ui::slider(
                        canvas,
                        theme,
                        slider_rect,
                        slider_prog,
                        hover == Some(HitTarget::SoundMasterSlider),
                        dragging_volume,
                    );
                    *vol_slider_rect = Some(slider_rect);
                    push_clipped_hit(hits, viewport, slider_rect, HitTarget::SoundMasterSlider);
                    let vol_txt = if cfg.sound_muted {
                        "静音".to_string()
                    } else {
                        format!("{}%", cfg.sound_master_volume)
                    };
                    text.draw_right(
                        canvas,
                        &vol_txt,
                        (content_x + card_w - 16) as f32,
                        (vol_row_y + 33) as f32,
                        12.5,
                        if cfg.sound_muted {
                            theme.text_muted
                        } else {
                            theme.text
                        },
                        Face::Bold,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 3);

                    // Row 3: Quick volume presets
                    let preset_idx =
                        nearest_preset_index(&VOLUME_PRESETS, i64::from(cfg.sound_master_volume));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 3,
                        "常用音量档位",
                        "默认开机自动取消静音并设为 80% 音量",
                        228,
                        &["25%", "50%", "80%", "100%"],
                        preset_idx,
                        HitTarget::SoundVolumePreset,
                        hover,
                    );
                    y += c1.h + 16;

                    y = draw_section_heading(
                        canvas,
                        text,
                        theme,
                        content_x,
                        y,
                        "系统提示音效（现代通透风）",
                    );
                    let c2 = Rect::new(content_x, y, card_w, ROW_H * 6);
                    ui::card(canvas, theme, c2);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "启用系统提示音",
                        "控制桌面登录、交互点击、通知与终端响铃音效总开关",
                        cfg.sound_enabled,
                        HitTarget::SoundEnabled,
                        hover,
                    );

                    let event_rows = [
                        (
                            "开机进入桌面",
                            "进入桌面会话时播放柔和清透迎宾和弦",
                            cfg.sound_event_startup,
                            HitTarget::SoundEventStartup,
                            HitTarget::SoundPreview(0),
                        ),
                        (
                            "界面点击与切换",
                            "按钮按下、页面切换与窗口菜单确认触感音",
                            cfg.sound_event_ui,
                            HitTarget::SoundEventUi,
                            HitTarget::SoundPreview(1),
                        ),
                        (
                            "系统通知提示",
                            "弹出桌面 Toast 通知或状态提醒时播放提示音",
                            cfg.sound_event_notify,
                            HitTarget::SoundEventNotify,
                            HitTarget::SoundPreview(2),
                        ),
                        (
                            "警告与终端响铃",
                            "终端响铃 (BEL) 或文件保存失败警告提示音",
                            cfg.sound_event_alert,
                            HitTarget::SoundEventAlert,
                            HitTarget::SoundPreview(3),
                        ),
                        (
                            "音量调节反馈",
                            "拖动主音量滑块或状态栏滚轮调音后播放反馈音",
                            cfg.sound_event_volume,
                            HitTarget::SoundEventVolume,
                            HitTarget::SoundPreview(4),
                        ),
                    ];
                    for (i, (title, sub, checked, tog, prev)) in event_rows.into_iter().enumerate()
                    {
                        let row_y = y + ROW_H * (i as i32 + 1);
                        draw_row_divider(canvas, theme, content_x, card_w, row_y);
                        draw_sound_event_row(
                            canvas, text, theme, viewport, hits, content_x, card_w, row_y, title,
                            sub, checked, tog, prev, hover,
                        );
                    }
                    y += c2.h + 18;
                }

                Page::Input => {
                    y = draw_section_heading(canvas, text, theme, content_x, y, "鼠标");
                    let c1 = Rect::new(content_x, y, card_w, ROW_H * 4);
                    ui::card(canvas, theme, c1);
                    let hand_idx = usize::from(cfg.mouse_left_handed);
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "主鼠标按键",
                        "选择用于点击与拖拽的主要鼠标按键",
                        188,
                        &["右手", "左手"],
                        hand_idx,
                        HitTarget::MouseHandedness,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    let speed_idx = nearest_preset_index(&MOUSE_SPEEDS, i64::from(cfg.mouse_speed));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "指针移动速度",
                        "调节光标在屏幕上的移动加速度",
                        275,
                        &["慢", "较慢", "适中", "较快", "快"],
                        speed_idx,
                        HitTarget::MouseSpeed,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);
                    let dc_idx = nearest_preset_index(
                        &DOUBLE_CLICK_SPEEDS,
                        cfg.mouse_double_click_ms as i64,
                    );
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 2,
                        "双击响应速度",
                        "两次点击被识别为双击的最大时间间隔",
                        204,
                        &["快", "适中", "慢"],
                        dc_idx,
                        HitTarget::DoubleClickSpeed,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 3);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 3,
                        "自然滚动",
                        "使页面内容沿手指滑动方向移动",
                        cfg.mouse_natural_scroll,
                        HitTarget::NaturalScroll,
                        hover,
                    );
                    y += c1.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "键盘");
                    let c2 = Rect::new(content_x, y, card_w, ROW_H * 3);
                    ui::card(canvas, theme, c2);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "按键重复输入",
                        "按住字符键不放时连续输入字符",
                        cfg.key_repeat_enabled,
                        HitTarget::KeyRepeatEnabled,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    let delay_idx =
                        nearest_preset_index(&KEY_DELAYS, i64::from(cfg.key_repeat_delay_ms));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "重复触发延迟",
                        "按下按键后开始连续输入前的等待时间",
                        204,
                        &["短", "适中", "长"],
                        delay_idx,
                        HitTarget::KeyRepeatDelay,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);
                    let rate_idx =
                        nearest_preset_index(&KEY_RATES, i64::from(cfg.key_repeat_rate_hz));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 2,
                        "字符重复速度",
                        "触发连续输入后的字符重复速率",
                        204,
                        &["慢", "适中", "快"],
                        rate_idx,
                        HitTarget::KeyRepeatRate,
                        hover,
                    );
                    y += c2.h + 18;
                }

                Page::Applications => {
                    y = draw_section_heading(canvas, text, theme, content_x, y, "终端");
                    let c1 = Rect::new(content_x, y, card_w, ROW_H * 3);
                    ui::card(canvas, theme, c1);
                    let tf_idx =
                        nearest_preset_index(&CODE_FONT_SIZES, i64::from(cfg.term_font_size));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "字体大小",
                        "终端仿真器等宽字体的字号 (px)",
                        220,
                        &["12", "14", "16", "18"],
                        tf_idx,
                        HitTarget::TermFontSize,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    let cs_idx = match cfg.term_cursor_style {
                        CursorStyle::Block => 0,
                        CursorStyle::Beam => 1,
                        CursorStyle::Underline => 2,
                    };
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "光标样式",
                        "命令行输入光标的显示形状",
                        228,
                        &["块状", "竖线", "下划线"],
                        cs_idx,
                        HitTarget::TermCursorStyle,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);
                    let sb_idx =
                        nearest_preset_index(&SCROLLBACK_PRESETS, cfg.term_scrollback_lines as i64);
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 2,
                        "回滚行数",
                        "终端缓冲区保留的最大历史输出行数",
                        246,
                        &["500 行", "1000 行", "3000 行"],
                        sb_idx,
                        HitTarget::TermScrollback,
                        hover,
                    );
                    y += c1.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "文件管理器");
                    let c2 = Rect::new(content_x, y, card_w, ROW_H * 3);
                    ui::card(canvas, theme, c2);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "显示隐藏文件",
                        "在目录视图中展示以点号 (.) 开头的文件与目录",
                        cfg.files_show_hidden,
                        HitTarget::FilesShowHidden,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "文件夹排在文件前",
                        "浏览目录时始终将文件夹分组排列在普通文件上方",
                        cfg.files_folders_first,
                        HitTarget::FilesFoldersFirst,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 2,
                        "单击直接打开项目",
                        "单击即可进入文件夹或打开文件，关闭则使用双击打开",
                        cfg.files_single_click,
                        HitTarget::FilesSingleClick,
                        hover,
                    );
                    y += c2.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "文本编辑器");
                    let c3 = Rect::new(content_x, y, card_w, ROW_H * 4);
                    ui::card(canvas, theme, c3);
                    let ef_idx =
                        nearest_preset_index(&CODE_FONT_SIZES, i64::from(cfg.editor_font_size));
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "正文字号",
                        "代码与文本编辑区的等宽字体大小 (px)",
                        220,
                        &["12", "14", "16", "18"],
                        ef_idx,
                        HitTarget::EditorFontSize,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "显示行号栏",
                        "在文档编辑区左侧显示行号标尺",
                        cfg.editor_show_line_numbers,
                        HitTarget::EditorLineNumbers,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 2,
                        "高亮当前行",
                        "突出显示光标当前所在的文本行背景",
                        cfg.editor_highlight_active_line,
                        HitTarget::EditorHighlightLine,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 3);
                    let tab_idx = nearest_preset_index(&TAB_WIDTHS, cfg.editor_tab_width as i64);
                    draw_segmented_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 3,
                        "Tab 缩进宽度",
                        "按下 Tab 键时插入的空格数",
                        234,
                        &["2 空格", "4 空格", "8 空格"],
                        tab_idx,
                        HitTarget::EditorTabWidth,
                        hover,
                    );
                    y += c3.h + 18;
                }

                Page::DateTime => {
                    let (time_str, date_str) = format_datetime_preview(cfg);
                    let hero = Rect::new(content_x, y + 4, card_w, 86);
                    ui::card(canvas, theme, hero);
                    text.draw(
                        canvas,
                        &time_str,
                        (content_x + 20) as f32,
                        (hero.y + 39) as f32,
                        22.0,
                        theme.text,
                        Face::Bold,
                    );
                    text.draw(
                        canvas,
                        &date_str,
                        (content_x + 20) as f32,
                        (hero.y + 65) as f32,
                        12.0,
                        theme.text_dim,
                        Face::Ui,
                    );
                    y = hero.bottom() + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "时钟格式");
                    let c1 = Rect::new(content_x, y, card_w, ROW_H * 3);
                    ui::card(canvas, theme, c1);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y,
                        "24 小时制",
                        "关闭后使用上午 / 下午 12 小时制显示时间",
                        cfg.clock_24h,
                        HitTarget::Clock24h,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H,
                        "显示秒数",
                        "在面板时钟中每秒更新并显示秒数",
                        cfg.clock_seconds,
                        HitTarget::ClockSeconds,
                        hover,
                    );
                    draw_row_divider(canvas, theme, content_x, card_w, y + ROW_H * 2);
                    draw_toggle_row(
                        canvas,
                        text,
                        theme,
                        viewport,
                        hits,
                        content_x,
                        card_w,
                        y + ROW_H * 2,
                        "面板显示日期",
                        "在桌面面板右侧时钟旁同时显示月日与星期",
                        cfg.clock_show_date,
                        HitTarget::ClockShowDate,
                        hover,
                    );
                    y += c1.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "时区");
                    let tz_row_h = 44;
                    let c2 = Rect::new(content_x, y, card_w, tz_row_h * TIMEZONES.len() as i32);
                    ui::card(canvas, theme, c2);
                    for (i, (tz_id, tz_name)) in TIMEZONES.iter().enumerate() {
                        let ry = y + i as i32 * tz_row_h;
                        if i > 0 {
                            draw_row_divider(canvas, theme, content_x, card_w, ry);
                        }
                        let row_r = Rect::new(content_x + 6, ry + 4, card_w - 12, tz_row_h - 8);
                        let selected = cfg.timezone == *tz_id;
                        let target = HitTarget::Timezone(i);
                        let fg = ui::list_item_bg(canvas, theme, row_r, selected, hover == Some(target));
                        text.draw(
                            canvas,
                            tz_name,
                            (content_x + 18) as f32,
                            (ry + 27) as f32,
                            12.5,
                            if selected && !is_win98 { theme.accent } else { fg },
                            if selected { Face::Bold } else { Face::Ui },
                        );
                        let id_w = text.measure(tz_id, 11.5, Face::Ui);
                        text.draw(
                            canvas,
                            tz_id,
                            (content_x + card_w - 20) as f32 - id_w,
                            (ry + 27) as f32,
                            11.5,
                            if selected {
                                if is_win98 { fg } else { theme.accent }
                            } else {
                                theme.text_muted
                            },
                            Face::Ui,
                        );
                        push_clipped_hit(hits, viewport, row_r, target);
                    }
                    y += c2.h + 18;
                }

                Page::Logs => {
                    let hero = Rect::new(content_x, y + 4, card_w, 92);
                    ui::card(canvas, theme, hero);
                    text.draw(
                        canvas,
                        "启动与系统日志",
                        (content_x + 20) as f32,
                        (hero.y + 34) as f32,
                        18.0,
                        theme.text,
                        Face::Bold,
                    );
                    text.draw(
                        canvas,
                        "内核引导加载、硬件探测及系统服务运行日志",
                        (content_x + 20) as f32,
                        (hero.y + 56) as f32,
                        11.5,
                        theme.text_dim,
                        Face::Ui,
                    );

                    let up_label = format!("系统运行时间: {}", log_state.uptime_str);
                    let up_w = text.measure(&up_label, 11.0, Face::Ui);
                    text.draw(
                        canvas,
                        &up_label,
                        (content_x + card_w - 20) as f32 - up_w,
                        (hero.y + 34) as f32,
                        11.0,
                        theme.text_muted,
                        Face::Ui,
                    );

                    let stats_y = hero.y + 70;
                    let stat_items: [(&str, usize, u32); 4] = [
                        ("全部日志", log_state.total_count, theme.text),
                        ("内核消息", log_state.kernel_count, theme.accent),
                        ("系统服务", log_state.service_count, theme.success),
                        (
                            "异常/警告",
                            log_state.error_count + log_state.warn_count,
                            if log_state.error_count > 0 {
                                theme.danger
                            } else {
                                theme.warning
                            },
                        ),
                    ];
                    let mut sx = content_x + 20;
                    for (slabel, scnt, scolor) in stat_items {
                        let stext = format!("{}: {}", slabel, scnt);
                        text.draw(
                            canvas,
                            &stext,
                            sx as f32,
                            stats_y as f32,
                            11.0,
                            scolor,
                            Face::Bold,
                        );
                        sx += (text.measure(&stext, 11.0, Face::Bold) as i32) + 24;
                    }
                    y = hero.bottom() + 14;

                    let act_h = 46;
                    let c_act = Rect::new(content_x, y, card_w, act_h);
                    ui::card(canvas, theme, c_act);

                    let filter_tabs = ["全部日志", "内核消息", "系统服务", "异常与告警"];
                    let filter_w = 92;
                    for (f_idx, flabel) in filter_tabs.iter().enumerate() {
                        let btn_r = Rect::new(
                            content_x + 8 + f_idx as i32 * (filter_w + 4),
                            y + 7,
                            filter_w,
                            32,
                        );
                        let is_active = match log_state.filter {
                            LogFilterType::All => f_idx == 0,
                            LogFilterType::Kernel => f_idx == 1,
                            LogFilterType::Services => f_idx == 2,
                            LogFilterType::ErrorsWarns => f_idx == 3,
                        };
                        let target = HitTarget::LogFilter(f_idx);
                        if is_active {
                            canvas.rounded_rect(btn_r, ctrl_r, theme.accent_soft, 1.0);
                            canvas.fill(
                                Rect::new(btn_r.x + 12, btn_r.bottom() - 2, btn_r.w - 24, 2),
                                theme.accent,
                            );
                        } else if hover == Some(target) {
                            canvas.rounded_rect(btn_r, ctrl_r, theme.panel_hover, 1.0);
                        }
                        let lw = text.measure(
                            flabel,
                            11.5,
                            if is_active { Face::Bold } else { Face::Ui },
                        );
                        text.draw(
                            canvas,
                            flabel,
                            btn_r.x as f32 + (btn_r.w as f32 - lw) * 0.5,
                            (btn_r.y + 20) as f32,
                            11.5,
                            if is_active {
                                theme.accent
                            } else {
                                theme.text_dim
                            },
                            if is_active { Face::Bold } else { Face::Ui },
                        );
                        push_clipped_hit(hits, viewport, btn_r, target);
                    }

                    let ref_btn = Rect::new(c_act.right() - 96, y + 7, 88, 32);
                    ui::button(
                        canvas,
                        text,
                        theme,
                        ref_btn,
                        "刷新",
                        Some(Icon::Refresh),
                        false,
                        hover == Some(HitTarget::LogRefresh),
                    );
                    push_clipped_hit(hits, viewport, ref_btn, HitTarget::LogRefresh);
                    y += c_act.h + 14;

                    if let Some(sel_idx) = log_state.selected {
                        if let Some(entry) = log_state.entries.get(sel_idx) {
                            let sec = entry.mono_us / 1000000;
                            let mic = entry.mono_us % 1000000;
                            let prio_label = match entry.prio {
                                0..=3 => "错误 (Error)",
                                4 => "警告 (Warning)",
                                5 | 6 => "信息 (Info)",
                                _ => "调试 (Debug)",
                            };
                            let d_card = Rect::new(content_x, y, card_w, 96);
                            ui::card(canvas, theme, d_card);
                            canvas.rounded_rect(
                                Rect::new(d_card.x + 2, d_card.y + 2, 4, d_card.h - 4),
                                2,
                                if entry.prio <= 3 {
                                    theme.danger
                                } else if entry.prio == 4 {
                                    theme.warning
                                } else {
                                    theme.accent
                                },
                                1.0,
                            );
                            let meta_str = format!(
                                "时间戳: [{:5}.{:06}s]   源: [{}]   级别: {}",
                                sec, mic, entry.unit, prio_label
                            );
                            text.draw(
                                canvas,
                                &meta_str,
                                (content_x + 16) as f32,
                                (d_card.y + 26) as f32,
                                11.5,
                                theme.accent,
                                Face::Bold,
                            );
                            text.draw(
                                canvas,
                                "详细消息内容:",
                                (content_x + 16) as f32,
                                (d_card.y + 48) as f32,
                                11.0,
                                theme.text_dim,
                                Face::Ui,
                            );
                            let msg_shown = text.truncate(
                                &entry.message,
                                (card_w - 36) as f32,
                                11.5,
                                Face::Mono,
                            );
                            text.draw(
                                canvas,
                                &msg_shown,
                                (content_x + 16) as f32,
                                (d_card.y + 72) as f32,
                                11.5,
                                theme.text,
                                Face::Mono,
                            );
                            y += d_card.h + 14;
                        }
                    }

                    let filtered: Vec<(usize, &SystemLogEntry)> = log_state
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| match log_state.filter {
                            LogFilterType::All => true,
                            LogFilterType::Kernel => e.unit == "kernel",
                            LogFilterType::Services => e.unit != "kernel",
                            LogFilterType::ErrorsWarns => e.prio <= 4,
                        })
                        .collect();

                    let row_h = 28;
                    let table_h = (filtered.len() as i32 * row_h).max(row_h);
                    let c_table = Rect::new(content_x, y, card_w, table_h + 12);
                    ui::card(canvas, theme, c_table);

                    if filtered.is_empty() {
                        text.draw(
                            canvas,
                            "暂无符合条件的日志记录",
                            (content_x + card_w / 2) as f32 - 70.0,
                            (y + 26) as f32,
                            12.0,
                            theme.text_muted,
                            Face::Ui,
                        );
                    } else {
                        for (list_idx, (orig_idx, entry)) in filtered.iter().enumerate() {
                            let ry = y + 6 + list_idx as i32 * row_h;
                            let row_r = Rect::new(content_x + 4, ry, card_w - 8, row_h);
                            let is_selected = log_state.selected == Some(*orig_idx);
                            let target = HitTarget::LogSelect(*orig_idx);

                            if is_selected {
                                canvas.rounded_rect(row_r, ctrl_r, theme.accent_soft, 1.0);
                            } else if hover == Some(target) {
                                canvas.rounded_rect(row_r, ctrl_r, theme.panel_hover, 1.0);
                            }

                            let (badge_txt, badge_bg, badge_fg) = match entry.prio {
                                0..=3 => ("ERR", theme.danger, 0xffffff),
                                4 => ("WARN", theme.warning, 0x18181b),
                                5 | 6 => ("INFO", theme.accent_soft, theme.accent),
                                _ => ("DBG", theme.panel_border, theme.text_muted),
                            };
                            let b_rect = Rect::new(content_x + 10, ry + 5, 38, 18);
                            canvas.rounded_rect(b_rect, 3, badge_bg, 1.0);
                            let bw = text.measure(badge_txt, 9.5, Face::Bold);
                            text.draw(
                                canvas,
                                badge_txt,
                                b_rect.x as f32 + (b_rect.w as f32 - bw) * 0.5,
                                (b_rect.y + 13) as f32,
                                9.5,
                                badge_fg,
                                Face::Bold,
                            );

                            let sec = entry.mono_us / 1000000;
                            let mic = entry.mono_us % 1000000;
                            let ts_str = format!("[{:5}.{:06}]", sec, mic);
                            text.draw(
                                canvas,
                                &ts_str,
                                (content_x + 54) as f32,
                                (ry + 19) as f32,
                                10.5,
                                theme.text_muted,
                                Face::Mono,
                            );

                            let tag_str = format!("[{}]", entry.tag);
                            let tag_shown = text.truncate(&tag_str, 90.0, 10.5, Face::Bold);
                            text.draw(
                                canvas,
                                &tag_shown,
                                (content_x + 160) as f32,
                                (ry + 19) as f32,
                                10.5,
                                if is_selected {
                                    theme.accent
                                } else {
                                    theme.text_dim
                                },
                                Face::Bold,
                            );

                            let msg_x = content_x + 258;
                            let max_msg_w = (card_w - 270).max(60) as f32;
                            let msg_clean = entry.message.replace('\n', " ");
                            let msg_shown = text.truncate(&msg_clean, max_msg_w, 11.0, Face::Ui);
                            text.draw(
                                canvas,
                                &msg_shown,
                                msg_x as f32,
                                (ry + 19) as f32,
                                11.0,
                                if is_selected {
                                    theme.text
                                } else {
                                    theme.text_dim
                                },
                                Face::Ui,
                            );

                            push_clipped_hit(hits, viewport, row_r, target);
                        }
                    }
                    y += c_table.h + 18;
                }

                Page::About => {
                    let hero = Rect::new(content_x, y + 4, card_w, 98);
                    ui::card(canvas, theme, hero);
                    icons::draw(
                        canvas,
                        Icon::SpaceLogo,
                        content_x + 20,
                        hero.y + 21,
                        56,
                        None,
                    );
                    text.draw(
                        canvas,
                        "SpaceOS 9",
                        (content_x + 92) as f32,
                        (hero.y + 45) as f32,
                        19.0,
                        theme.text,
                        Face::Bold,
                    );
                    text.draw(
                        canvas,
                        "ViudiraTech 桌面操作系统",
                        (content_x + 92) as f32,
                        (hero.y + 70) as f32,
                        12.5,
                        theme.text_dim,
                        Face::Ui,
                    );
                    y = hero.bottom() + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "硬件信息");
                    let hw_rows: [(&str, &str); 4] = [
                        ("设备名称", &sys_info.hostname),
                        ("处理器", &sys_info.cpu),
                        ("内存容量", &sys_info.memory),
                        ("磁盘容量", &sys_info.disk),
                    ];
                    let info_row_h = 42;
                    let c_hw = Rect::new(content_x, y, card_w, info_row_h * hw_rows.len() as i32);
                    ui::card(canvas, theme, c_hw);
                    for (idx, (k, v)) in hw_rows.iter().enumerate() {
                        let ry = y + idx as i32 * info_row_h;
                        if idx > 0 {
                            draw_row_divider(canvas, theme, content_x, card_w, ry);
                        }
                        text.draw(
                            canvas,
                            k,
                            (content_x + 16) as f32,
                            (ry + 26) as f32,
                            12.0,
                            theme.text_dim,
                            Face::Ui,
                        );
                        let shown_v = text.truncate(v, (card_w - 150) as f32, 12.0, Face::Ui);
                        let vw = text.measure(&shown_v, 12.0, Face::Ui);
                        text.draw(
                            canvas,
                            &shown_v,
                            (content_x + card_w - 18) as f32 - vw,
                            (ry + 26) as f32,
                            12.0,
                            theme.text,
                            Face::Ui,
                        );
                    }
                    y += c_hw.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "软件环境");
                    let sw_rows: [(&str, &str); 3] = [
                        ("操作系统", &sys_info.os_name),
                        ("桌面环境", "SPUI 4.0"),
                        ("内核版本", &sys_info.kernel_arch),
                    ];
                    let c_sw = Rect::new(content_x, y, card_w, info_row_h * sw_rows.len() as i32);
                    ui::card(canvas, theme, c_sw);
                    for (idx, (k, v)) in sw_rows.iter().enumerate() {
                        let ry = y + idx as i32 * info_row_h;
                        if idx > 0 {
                            draw_row_divider(canvas, theme, content_x, card_w, ry);
                        }
                        text.draw(
                            canvas,
                            k,
                            (content_x + 16) as f32,
                            (ry + 26) as f32,
                            12.0,
                            theme.text_dim,
                            Face::Ui,
                        );
                        let shown_v = text.truncate(v, (card_w - 150) as f32, 12.0, Face::Ui);
                        let vw = text.measure(&shown_v, 12.0, Face::Ui);
                        text.draw(
                            canvas,
                            &shown_v,
                            (content_x + card_w - 18) as f32 - vw,
                            (ry + 26) as f32,
                            12.0,
                            theme.text,
                            Face::Ui,
                        );
                    }
                    y += c_sw.h + 18;

                    y = draw_section_heading(
                        canvas,
                        text,
                        theme,
                        content_x,
                        y,
                        "系统诊断与启动日志",
                    );
                    let c_diag = Rect::new(content_x, y, card_w, 76);
                    ui::card(canvas, theme, c_diag);
                    text.draw(
                        canvas,
                        "系统启动与运行日志",
                        (content_x + 16) as f32,
                        (c_diag.y + 28) as f32,
                        13.0,
                        theme.text,
                        Face::Bold,
                    );
                    text.draw(
                        canvas,
                        "查看内核引导、硬件探测以及系统服务初始化的完整时间戳日志。",
                        (content_x + 16) as f32,
                        (c_diag.y + 50) as f32,
                        11.0,
                        theme.text_muted,
                        Face::Ui,
                    );
                    let logs_btn = Rect::new(c_diag.right() - 132, c_diag.y + 21, 112, 34);
                    ui::button(
                        canvas,
                        text,
                        theme,
                        logs_btn,
                        "查看日志…",
                        None,
                        false,
                        hover == Some(HitTarget::JumpToLogs),
                    );
                    push_clipped_hit(hits, viewport, logs_btn, HitTarget::JumpToLogs);
                    y += c_diag.h + 16;

                    y = draw_section_heading(canvas, text, theme, content_x, y, "系统维护");
                    let c_reset = Rect::new(content_x, y, card_w, 92);
                    ui::card(canvas, theme, c_reset);
                    text.draw(
                        canvas,
                        "恢复出厂设置",
                        (content_x + 16) as f32,
                        (c_reset.y + 30) as f32,
                        13.0,
                        theme.danger,
                        Face::Bold,
                    );
                    text.draw(
                        canvas,
                        "删除本地账户、主目录和个性化配置；重启后进入首次设置向导。",
                        (content_x + 16) as f32,
                        (c_reset.y + 52) as f32,
                        10.5,
                        theme.text_muted,
                        Face::Ui,
                    );
                    text.draw(
                        canvas,
                        "需要 wheel 管理员确认；不会影响内核、系统文件或已安装软件。",
                        (content_x + 16) as f32,
                        (c_reset.y + 70) as f32,
                        9.5,
                        theme.text_muted,
                        Face::Ui,
                    );
                    let reset_btn = Rect::new(c_reset.right() - 132, c_reset.y + 30, 112, 34);
                    ui::button(
                        canvas,
                        text,
                        theme,
                        reset_btn,
                        "重置…",
                        None,
                        false,
                        hover == Some(HitTarget::FactoryReset),
                    );
                    push_clipped_hit(hits, viewport, reset_btn, HitTarget::FactoryReset);
                    y += c_reset.h + 18;
                }
            }
            end_y = y;
        });

        let total_h = (end_y - start_y + 6).max(viewport.h);
        self.scroll.draw_v(
            canvas,
            theme,
            scrollbar_track_in(bounds, style),
            viewport.h as f32,
            total_h as f32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_index_covers_all_eleven_categories() {
        for (page, title, _) in NAV {
            assert!(
                SETTING_ITEMS.iter().any(|item| item.page == page),
                "missing searchable settings entry for page {title}"
            );
        }
        let hits = search_settings("全屏");
        assert!(hits.iter().any(|item| item.page == Page::DesktopPanel));
        let net_hits = search_settings("DHCP");
        assert!(net_hits.iter().any(|item| item.page == Page::Network));
    }

    #[test]
    fn parse_initial_page_handles_slugs() {
        assert_eq!(parse_initial_page(Some("network")), Page::Network);
        assert_eq!(parse_initial_page(Some("audio")), Page::Sound);
        assert_eq!(parse_initial_page(Some("logs")), Page::Logs);
        assert_eq!(parse_initial_page(Some("about")), Page::About);
        assert_eq!(parse_initial_page(None), Page::Appearance);
    }
}
