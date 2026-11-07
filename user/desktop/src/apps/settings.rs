/*
 *
 *       user/desktop/src/apps/settings.rs
 *       SPUI 4.0 System Settings: 8-category desktop & app preferences backed by uxconfd
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mature, clutter-free System Settings application connected live to `uxconfd`.
//! Organizes preferences into eight categories: Appearance, Desktop & Panel,
//! Windows, Display, Mouse & Keyboard, Applications, Date & Time, and About.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect, Sprite};
use crate::icons::{self, Icon};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, RADIUS_CONTROL, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::{CursorKind, Result};
use std::ffi::CStr;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uxconf::{
    format_hex_rgb, BackdropStyle, ButtonLayout, CursorStyle, DesktopConfig, DoubleClickAction,
    FocusMode, NetMode, PanelPosition, ThemeMode, Value, WindowPlacement, ACCENT_PRESETS,
    RESOLUTIONS, SOLID_BG_PRESETS, TIMEZONES, WALLPAPERS, WALLPAPER_DIR,
};
use x11rb::protocol::xproto::ConnectionExt;

extern "C" {
    fn tzset();
}

const SIDEBAR_W: i32 = 196;
const HEADER_H: i32 = 52;
const CONTENT_X: i32 = SIDEBAR_W + 24;
const ROW_H: i32 = 56;
const THUMB_W: usize = 112;
const THUMB_H: usize = 66;

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
const CODE_FONT_SIZES: [(i64, &str); 4] = [(12, "12"), (14, "14"), (16, "16"), (18, "18")];
const SCROLLBACK_PRESETS: [(i64, &str); 3] =
    [(500, "500 行"), (1000, "1000 行"), (3000, "3000 行")];
const TAB_WIDTHS: [(i64, &str); 3] = [(2, "2 空格"), (4, "4 空格"), (8, "8 空格")];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Appearance,
    Network,
    DesktopPanel,
    Windows,
    Display,
    Input,
    Applications,
    DateTime,
    Logs,
    About,
}

impl Page {
    const fn title(self) -> &'static str {
        match self {
            Self::Appearance => "外观",
            Self::Network => "网络",
            Self::DesktopPanel => "桌面与面板",
            Self::Windows => "窗口",
            Self::Display => "显示",
            Self::Input => "鼠标与键盘",
            Self::Applications => "应用程序",
            Self::DateTime => "日期与时间",
            Self::Logs => "启动日志",
            Self::About => "关于",
        }
    }

    const fn channels(self) -> &'static [&'static str] {
        match self {
            Self::Appearance => &["appearance"],
            Self::Network => &["network"],
            Self::DesktopPanel => &["desktop", "panel"],
            Self::Windows => &["wm"],
            Self::Display => &["display"],
            Self::Input => &["input"],
            Self::Applications => &["terminal", "files", "editor"],
            Self::DateTime => &["datetime"],
            Self::Logs => &[],
            Self::About => &[],
        }
    }
}

const NAV: [(Page, &str, Icon); 10] = [
    (Page::Appearance, "外观", Icon::Appearance),
    (Page::Network, "网络", Icon::Network),
    (Page::DesktopPanel, "桌面与面板", Icon::Panel),
    (Page::Windows, "窗口", Icon::WindowManager),
    (Page::Display, "显示", Icon::Display),
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
    WmButtonLayout(usize),
    WmDoubleClick(usize),
    WmFocusMode(usize),
    WmAutoRaise,
    WmPlacement(usize),
    Resolution(usize),
    BlankTimeout(usize),
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

fn query_root_resolution(app: &App) -> (u16, u16) {
    app.x
        .conn
        .get_geometry(app.x.root)
        .ok()
        .and_then(|c| c.reply().ok())
        .map_or((app.x.width, app.x.height), |g| (g.width, g.height))
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

fn scroll_viewport(w: i32, h: i32) -> Rect {
    Rect::new(
        SIDEBAR_W,
        TITLEBAR_HEIGHT + HEADER_H,
        (w - SIDEBAR_W - 1).max(1),
        (h - TITLEBAR_HEIGHT - HEADER_H - 1).max(1),
    )
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

fn parse_initial_page(arg: Option<&str>) -> Page {
    match arg.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("network" | "net") => Page::Network,
        Some("desktop" | "panel") => Page::DesktopPanel,
        Some("windows" | "wm") => Page::Windows,
        Some("display") => Page::Display,
        Some("input") => Page::Input,
        Some("apps" | "applications") => Page::Applications,
        Some("datetime" | "time") => Page::DateTime,
        Some("logs" | "journal") => Page::Logs,
        Some("about") => Page::About,
        _ => Page::Appearance,
    }
}

fn apply_static_network_settings(app: &mut App, net_state: &mut NetPageState, status: &mut String) {
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

    let next_serial = app.config.net_apply_serial.wrapping_add(1);
    app.set_config_property("network", "/address", Value::String(addr_raw.to_string()));
    app.set_config_property("network", "/prefixlen", Value::Int(i64::from(prefixlen)));
    app.set_config_property("network", "/gateway", Value::String(gw_raw.to_string()));
    app.set_config_property("network", "/dns", Value::String(dns_raw.to_string()));
    app.set_config_property("network", "/apply-serial", Value::Int(next_serial));
    net_state.inputs[1] = prefixlen.to_string();

    match crate::netmgr::request_apply(&app.config) {
        Ok(msg) => *status = msg,
        Err(err) => *status = err,
    }
    net_state.refresh();
}

pub fn run(initial_page: Option<&str>) -> Result<()> {
    let mut app = App::new("系统设置", Icon::Settings, 840, 580)?;
    app.min_width = 760;
    app.min_height = 500;
    apply_local_timezone(&app.config.timezone);

    let mut page = parse_initial_page(initial_page);
    let mut scroll_y: i32 = 0;
    let mut content_h: i32 = 0;
    let mut hover: Option<HitTarget> = None;
    let mut hits: Vec<HitBox> = Vec::with_capacity(64);
    let mut status = String::new();
    let mut screen_size = query_root_resolution(&app);
    let mut last_clock_preview = String::new();
    let thumbnails = load_wallpaper_thumbnails();
    let sys_info = SystemInfo::gather();
    let mut log_state = LogViewerState::load();
    let mut net_state = NetPageState::new(&app.config);
    let net_fd = crate::netmgr::open_event_socket();

    loop {
        if page == Page::DateTime {
            let (now_str, _) = format_datetime_preview(&app.config);
            if now_str != last_clock_preview {
                last_clock_preview = now_str;
                app.needs_redraw = true;
            }
        } else if page == Page::Network
            && net_state.last_refresh.elapsed() >= Duration::from_millis(1800)
        {
            net_state.refresh();
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            let vp = scroll_viewport(app.width as i32, app.height as i32);
            hits.clear();
            content_h = draw_settings(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &app.config,
                page,
                scroll_y,
                hover,
                &thumbnails,
                screen_size,
                &sys_info,
                &log_state,
                &net_state,
                &status,
                &mut hits,
            );
            let max_scroll = (content_h - vp.h).max(0);
            if scroll_y > max_scroll {
                scroll_y = max_scroll;
                hits.clear();
                content_h = draw_settings(
                    &mut app.canvas,
                    &mut app.text,
                    &app.theme,
                    &app.config,
                    page,
                    scroll_y,
                    hover,
                    &thumbnails,
                    screen_size,
                    &sys_info,
                    &log_state,
                    &net_state,
                    &status,
                    &mut hits,
                );
            }
            app.draw_chrome();
            app.present()?;
        }

        if app.close {
            break;
        }

        let timeout_ms = if page == Page::DateTime && app.config.clock_seconds {
            250
        } else {
            500
        };
        let (events, net_ready) = app.pump_with_fd(net_fd, timeout_ms)?;
        if net_ready {
            if let Some(fd) = net_fd {
                let _ = crate::netmgr::drain_events(fd);
            }
            net_state.refresh();
            if page == Page::Network {
                app.needs_redraw = true;
            }
        }
        for event in events {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } => {
                    screen_size = query_root_resolution(&app);
                    app.needs_redraw = true;
                }
                UiEvent::ConfigChanged => {
                    apply_local_timezone(&app.config.timezone);
                    screen_size = query_root_resolution(&app);
                    if net_state.focused.is_none() {
                        net_state.sync_from_config(&app.config);
                    }
                    net_state.refresh();
                    app.needs_redraw = true;
                }
                UiEvent::Key(key) if page == Page::Network => {
                    if let Some(idx) = net_state.focused {
                        if key.is_escape() {
                            net_state.focused = None;
                            app.needs_redraw = true;
                        } else if key.is_tab() {
                            net_state.focused = Some(if key.shift() {
                                (idx + 3) % 4
                            } else {
                                (idx + 1) % 4
                            });
                            app.needs_redraw = true;
                        } else if key.is_backspace() {
                            net_state.inputs[idx].pop();
                            app.needs_redraw = true;
                        } else if key.ctrl_byte() == Some(21) {
                            net_state.inputs[idx].clear();
                            app.needs_redraw = true;
                        } else if key.is_enter() {
                            apply_static_network_settings(&mut app, &mut net_state, &mut status);
                            app.needs_redraw = true;
                        } else if !key.control() && !key.alt() {
                            if let Some(ch) = key.text() {
                                if (ch.is_ascii_digit() || matches!(ch, '.' | '/'))
                                    && net_state.inputs[idx].len() < 31
                                {
                                    net_state.inputs[idx].push(ch);
                                    app.needs_redraw = true;
                                }
                            }
                        }
                    }
                }
                UiEvent::Wheel { x, y, delta } => {
                    let vp = scroll_viewport(app.width as i32, app.height as i32);
                    if vp.contains(x, y) {
                        let max_scroll = (content_h - vp.h).max(0);
                        let next_scroll = (scroll_y + delta * 18).clamp(0, max_scroll);
                        if next_scroll != scroll_y {
                            scroll_y = next_scroll;
                            app.needs_redraw = true;
                        }
                    }
                }
                UiEvent::Motion { x, y, .. } => {
                    let next = if y >= TITLEBAR_HEIGHT {
                        find_hit(&hits, x, y)
                    } else {
                        None
                    };
                    let cursor = match next {
                        Some(HitTarget::NetInput(_)) => CursorKind::Text,
                        Some(_) => CursorKind::Pointer,
                        None => CursorKind::Default,
                    };
                    app.set_content_cursor(cursor);
                    if next != hover {
                        hover = next;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x,
                    y,
                    button: 1,
                    down: true,
                    ..
                } if y >= TITLEBAR_HEIGHT => {
                    let hit = find_hit(&hits, x, y);
                    if !matches!(hit, Some(HitTarget::NetInput(_))) && net_state.focused.is_some() {
                        net_state.focused = None;
                        app.needs_redraw = true;
                    }
                    if let Some(target) = hit {
                        handle_click(
                            &mut app,
                            target,
                            &mut page,
                            &mut scroll_y,
                            &mut screen_size,
                            &mut status,
                            &mut log_state,
                            &mut net_state,
                        );
                    }
                }
                _ => {}
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

#[allow(clippy::too_many_arguments)]
fn handle_click(
    app: &mut App,
    target: HitTarget,
    page: &mut Page,
    scroll_y: &mut i32,
    screen_size: &mut (u16, u16),
    status: &mut String,
    log_state: &mut LogViewerState,
    net_state: &mut NetPageState,
) {
    status.clear();
    match target {
        HitTarget::Nav(i) => {
            let next_page = NAV[i].0;
            if *page != next_page {
                *page = next_page;
                *scroll_y = 0;
                net_state.focused = None;
                if next_page == Page::Display {
                    *screen_size = query_root_resolution(app);
                } else if next_page == Page::Network {
                    net_state.refresh();
                }
            }
        }
        HitTarget::ResetPage => {
            for ch in page.channels() {
                app.reset_config_channel(ch);
            }
            if *page == Page::Display {
                apply_display_mode(&app.config.display_resolution);
                *screen_size = query_root_resolution(app);
            } else if *page == Page::DateTime {
                apply_local_timezone(&app.config.timezone);
            } else if *page == Page::Network {
                net_state.focused = None;
                net_state.sync_from_config(&app.config);
                let _ = crate::netmgr::request_apply(&app.config);
                net_state.refresh();
            }
            *status = "已恢复默认设置".to_string();
        }
        HitTarget::ThemeMode(mode) => {
            app.set_config_property(
                "appearance",
                "/theme/mode",
                Value::String(mode.as_str().to_string()),
            );
        }
        HitTarget::Accent(i) => {
            let (rgb, _) = ACCENT_PRESETS[i];
            app.set_config_property(
                "appearance",
                "/theme/accent",
                Value::String(format_hex_rgb(rgb)),
            );
        }
        HitTarget::FontScale(i) => {
            app.set_config_property("appearance", "/font/scale", Value::Int(FONT_SCALES[i].0));
        }
        HitTarget::NetEnabled => {
            let next = !app.config.net_enabled;
            app.set_config_property("network", "/enabled", Value::Bool(next));
            match crate::netmgr::request_connect(None, next) {
                Ok(msg) => *status = msg,
                Err(err) => *status = err,
            }
            net_state.refresh();
        }
        HitTarget::NetMode(i) => {
            let mode = if i == 0 {
                NetMode::Dhcp
            } else {
                NetMode::Static
            };
            if app.config.net_mode != mode {
                app.set_config_property(
                    "network",
                    "/mode",
                    Value::String(mode.as_str().to_string()),
                );
                if mode == NetMode::Dhcp {
                    net_state.focused = None;
                    match crate::netmgr::request_apply(&app.config) {
                        Ok(msg) => *status = msg,
                        Err(err) => *status = err,
                    }
                    net_state.refresh();
                } else {
                    if let Some((ip, plen)) = net_state.status.ipv4 {
                        net_state.inputs[0] = ip.to_string();
                        net_state.inputs[1] = plen.to_string();
                    }
                    if let Some(gw) = net_state.status.gateway {
                        net_state.inputs[2] = gw.to_string();
                    }
                    if let Some(dns) = net_state.status.dns {
                        net_state.inputs[3] = dns.to_string();
                    }
                    net_state.focused = Some(0);
                    *status = "编辑静态 IPv4 参数后点击「应用网络配置」".to_string();
                }
            }
        }
        HitTarget::NetInput(i) => {
            net_state.focused = Some(i.min(3));
        }
        HitTarget::NetApply => {
            net_state.focused = None;
            apply_static_network_settings(app, net_state, status);
        }
        HitTarget::NetRenew => {
            match crate::netmgr::request_renew(None) {
                Ok(msg) => *status = msg,
                Err(err) => *status = err,
            }
            net_state.refresh();
        }
        HitTarget::NetRefresh => {
            net_state.refresh();
            *status = "网络状态已刷新".to_string();
        }
        HitTarget::NetPingGateway => {
            let target = net_state
                .status
                .gateway
                .unwrap_or_else(|| Ipv4Addr::new(10, 0, 2, 2));
            match crate::netmgr::ping_once(target, 1000) {
                Ok(rtt) => {
                    let msg = format!("网关 {target} 可达 · RTT {rtt:.2} ms");
                    net_state.ping_note = msg.clone();
                    *status = msg;
                }
                Err(err) => {
                    net_state.ping_note = err.clone();
                    *status = err;
                }
            }
            net_state.refresh();
        }
        HitTarget::BackdropStyle(i) => {
            let style = [
                BackdropStyle::Cover,
                BackdropStyle::Contain,
                BackdropStyle::Stretch,
                BackdropStyle::Solid,
            ][i];
            app.set_config_property(
                "desktop",
                "/backdrop/style",
                Value::String(style.as_str().to_string()),
            );
        }
        HitTarget::Wallpaper(i) => {
            app.set_config_property(
                "desktop",
                "/backdrop/wallpaper",
                Value::String(WALLPAPERS[i].file.to_string()),
            );
            if app.config.backdrop_style == BackdropStyle::Solid {
                app.set_config_property(
                    "desktop",
                    "/backdrop/style",
                    Value::String(BackdropStyle::Cover.as_str().to_string()),
                );
            }
        }
        HitTarget::SolidColor(i) => {
            let (rgb, _) = SOLID_BG_PRESETS[i];
            app.set_config_property(
                "desktop",
                "/backdrop/solid-color",
                Value::String(format_hex_rgb(rgb)),
            );
        }
        HitTarget::DesktopIconsVisible => {
            let next = !app.config.desktop_icons_visible;
            app.set_config_property("desktop", "/icons/visible", Value::Bool(next));
        }
        HitTarget::DesktopIconSize(i) => {
            app.set_config_property(
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
            app.set_config_property(
                "panel",
                "/position",
                Value::String(pos.as_str().to_string()),
            );
        }
        HitTarget::PanelSize(i) => {
            app.set_config_property("panel", "/size", Value::Int(PANEL_SIZES[i].0));
        }
        HitTarget::PanelShowPinned => {
            let next = !app.config.panel_show_pinned;
            app.set_config_property("panel", "/show-pinned", Value::Bool(next));
        }
        HitTarget::WmButtonLayout(i) => {
            let layout = if i == 0 {
                ButtonLayout::Right
            } else {
                ButtonLayout::Left
            };
            app.set_config_property(
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
            app.set_config_property(
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
            app.set_config_property("wm", "/focus-mode", Value::String(fm.as_str().to_string()));
        }
        HitTarget::WmAutoRaise => {
            let next = !app.config.wm_auto_raise;
            app.set_config_property("wm", "/auto-raise", Value::Bool(next));
        }
        HitTarget::WmPlacement(i) => {
            let pl = if i == 0 {
                WindowPlacement::Cascade
            } else {
                WindowPlacement::Center
            };
            app.set_config_property("wm", "/placement", Value::String(pl.as_str().to_string()));
        }
        HitTarget::Resolution(i) => {
            let (mode_arg, _) = RESOLUTIONS[i];
            app.set_config_property(
                "display",
                "/resolution",
                Value::String(mode_arg.to_string()),
            );
            apply_display_mode(mode_arg);
            *screen_size = query_root_resolution(app);
        }
        HitTarget::BlankTimeout(i) => {
            app.set_config_property(
                "display",
                "/blank-timeout-sec",
                Value::Int(BLANK_TIMEOUTS[i].0),
            );
        }
        HitTarget::MouseHandedness(i) => {
            app.set_config_property("input", "/mouse/left-handed", Value::Bool(i == 1));
        }
        HitTarget::MouseSpeed(i) => {
            app.set_config_property("input", "/mouse/speed", Value::Int(MOUSE_SPEEDS[i].0));
        }
        HitTarget::DoubleClickSpeed(i) => {
            app.set_config_property(
                "input",
                "/mouse/double-click-ms",
                Value::Int(DOUBLE_CLICK_SPEEDS[i].0),
            );
        }
        HitTarget::NaturalScroll => {
            let next = !app.config.mouse_natural_scroll;
            app.set_config_property("input", "/mouse/natural-scroll", Value::Bool(next));
        }
        HitTarget::KeyRepeatEnabled => {
            let next = !app.config.key_repeat_enabled;
            app.set_config_property("input", "/keyboard/repeat-enabled", Value::Bool(next));
        }
        HitTarget::KeyRepeatDelay(i) => {
            app.set_config_property(
                "input",
                "/keyboard/repeat-delay-ms",
                Value::Int(KEY_DELAYS[i].0),
            );
        }
        HitTarget::KeyRepeatRate(i) => {
            app.set_config_property(
                "input",
                "/keyboard/repeat-rate-hz",
                Value::Int(KEY_RATES[i].0),
            );
        }
        HitTarget::TermFontSize(i) => {
            app.set_config_property("terminal", "/font-size", Value::Int(CODE_FONT_SIZES[i].0));
        }
        HitTarget::TermCursorStyle(i) => {
            let cs = [
                CursorStyle::Block,
                CursorStyle::Beam,
                CursorStyle::Underline,
            ][i];
            app.set_config_property(
                "terminal",
                "/cursor-style",
                Value::String(cs.as_str().to_string()),
            );
        }
        HitTarget::TermScrollback(i) => {
            app.set_config_property(
                "terminal",
                "/scrollback-lines",
                Value::Int(SCROLLBACK_PRESETS[i].0),
            );
        }
        HitTarget::FilesShowHidden => {
            let next = !app.config.files_show_hidden;
            app.set_config_property("files", "/show-hidden", Value::Bool(next));
        }
        HitTarget::FilesFoldersFirst => {
            let next = !app.config.files_folders_first;
            app.set_config_property("files", "/folders-first", Value::Bool(next));
        }
        HitTarget::FilesSingleClick => {
            let next = !app.config.files_single_click;
            app.set_config_property("files", "/single-click-open", Value::Bool(next));
        }
        HitTarget::EditorFontSize(i) => {
            app.set_config_property("editor", "/font-size", Value::Int(CODE_FONT_SIZES[i].0));
        }
        HitTarget::EditorLineNumbers => {
            let next = !app.config.editor_show_line_numbers;
            app.set_config_property("editor", "/show-line-numbers", Value::Bool(next));
        }
        HitTarget::EditorHighlightLine => {
            let next = !app.config.editor_highlight_active_line;
            app.set_config_property("editor", "/highlight-active-line", Value::Bool(next));
        }
        HitTarget::EditorTabWidth(i) => {
            app.set_config_property("editor", "/tab-width", Value::Int(TAB_WIDTHS[i].0));
        }
        HitTarget::Clock24h => {
            let next = !app.config.clock_24h;
            app.set_config_property("datetime", "/clock-24h", Value::Bool(next));
        }
        HitTarget::ClockSeconds => {
            let next = !app.config.clock_seconds;
            app.set_config_property("datetime", "/clock-seconds", Value::Bool(next));
        }
        HitTarget::ClockShowDate => {
            let next = !app.config.clock_show_date;
            app.set_config_property("datetime", "/clock-show-date", Value::Bool(next));
        }
        HitTarget::Timezone(i) => {
            let (tz_id, _) = TIMEZONES[i];
            apply_local_timezone(tz_id);
            app.set_config_property("datetime", "/timezone", Value::String(tz_id.to_string()));
        }
        HitTarget::FactoryReset => {
            let now = std::time::Instant::now();
            let confirmed = {
                let mut armed = FACTORY_RESET_ARMED.lock().unwrap();
                let confirm = armed
                    .is_some_and(|at| now.duration_since(at) < std::time::Duration::from_secs(15));
                *armed = if confirm { None } else { Some(now) };
                confirm
            };
            if !confirmed {
                *status = "再次点击「重置…」确认恢复出厂设置（15 秒内有效）".to_string();
            } else {
                match app.uxconf.as_mut().map(|client| client.factory_reset()) {
                    Some(Ok(())) => {
                        *status =
                            "恢复出厂设置已登记；重启后清除本地账户并进入首次设置向导".to_string();
                    }
                    Some(Err(error)) => {
                        *status = format!("无法登记恢复出厂设置：{error}");
                    }
                    None => {
                        *status = "配置服务不可用，无法登记恢复出厂设置".to_string();
                    }
                }
            }
        }
        HitTarget::JumpToLogs => {
            *page = Page::Logs;
            *scroll_y = 0;
        }
        HitTarget::LogFilter(idx) => {
            log_state.filter = match idx {
                1 => LogFilterType::Kernel,
                2 => LogFilterType::Services,
                3 => LogFilterType::ErrorsWarns,
                _ => LogFilterType::All,
            };
            *scroll_y = 0;
        }
        HitTarget::LogRefresh => {
            *log_state = LogViewerState::load();
            *status = format!("日志已刷新（共 {} 条记录）", log_state.entries.len());
        }
        HitTarget::LogSelect(idx) => {
            if log_state.selected == Some(idx) {
                log_state.selected = None;
            } else {
                log_state.selected = Some(idx);
            }
        }
    }
    app.needs_redraw = true;
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
fn draw_settings(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    cfg: &DesktopConfig,
    page: Page,
    scroll_y: i32,
    hover: Option<HitTarget>,
    thumbnails: &[Option<Sprite>; 4],
    screen_size: (u16, u16),
    sys_info: &SystemInfo,
    log_state: &LogViewerState,
    net_state: &NetPageState,
    status: &str,
    hits: &mut Vec<HitBox>,
) -> i32 {
    let w = canvas.width as i32;
    let h = canvas.height as i32;
    canvas.clear(theme.window);

    // Left category navigation sidebar.
    let body_h = (h - TITLEBAR_HEIGHT).max(0);
    canvas.fill(
        Rect::new(0, TITLEBAR_HEIGHT, SIDEBAR_W, body_h),
        theme.window_alt,
    );
    canvas.fill(
        Rect::new(SIDEBAR_W - 1, TITLEBAR_HEIGHT, 1, body_h),
        theme.window_border,
    );
    text.draw(
        canvas,
        "系统设置",
        18.0,
        (TITLEBAR_HEIGHT + 24) as f32,
        11.0,
        theme.text_muted,
        Face::Bold,
    );

    let active_idx = NAV.iter().position(|(id, _, _)| *id == page).unwrap_or(0);
    let target_nav_y = (TITLEBAR_HEIGHT + 36 + active_idx as i32 * 38) as f32;
    let nav_y = ui::animate_float("settings_nav", target_nav_y, ui::MOTION_INDICATOR);
    let active_pill = Rect::new(10, nav_y.round() as i32, SIDEBAR_W - 20, 34);
    canvas.rounded_rect(active_pill, RADIUS_CONTROL, theme.accent_soft, 1.0);
    canvas.fill(
        Rect::new(active_pill.x, active_pill.y + 7, 3, active_pill.h - 14),
        theme.accent,
    );

    for (i, (id, label, icon)) in NAV.iter().enumerate() {
        let r = Rect::new(10, TITLEBAR_HEIGHT + 36 + i as i32 * 38, SIDEBAR_W - 20, 34);
        let active = page == *id;
        let target = HitTarget::Nav(i);
        if hover == Some(target) && !active {
            canvas.rounded_rect(r, RADIUS_CONTROL, theme.panel_hover, 1.0);
        }
        icons::draw(canvas, *icon, r.x + 10, r.y + (r.h - 18) / 2, 18, None);
        text.draw(
            canvas,
            label,
            (r.x + 36) as f32,
            (r.y + 22) as f32,
            12.5,
            if active { theme.text } else { theme.text_dim },
            if active { Face::Bold } else { Face::Ui },
        );
        hits.push(HitBox { rect: r, target });
    }

    // Fixed page header bar.
    text.draw(
        canvas,
        page.title(),
        CONTENT_X as f32,
        (TITLEBAR_HEIGHT + 33) as f32,
        16.5,
        theme.text,
        Face::Bold,
    );
    if !page.channels().is_empty() {
        let reset_rect = Rect::new(w - 24 - 84, TITLEBAR_HEIGHT + 12, 84, 28);
        ui::button(
            canvas,
            text,
            theme,
            reset_rect,
            "恢复默认",
            None,
            false,
            hover == Some(HitTarget::ResetPage),
        );
        hits.push(HitBox {
            rect: reset_rect,
            target: HitTarget::ResetPage,
        });
        if !status.is_empty() {
            let st_w = text.measure(status, 11.5, Face::Ui);
            text.draw(
                canvas,
                status,
                reset_rect.x as f32 - st_w - 14.0,
                (TITLEBAR_HEIGHT + 31) as f32,
                11.5,
                theme.accent,
                Face::Ui,
            );
        }
    } else if !status.is_empty() {
        let shown = text.truncate(status, (w - CONTENT_X - 48) as f32, 11.5, Face::Ui);
        let st_w = text.measure(&shown, 11.5, Face::Ui);
        text.draw(
            canvas,
            &shown,
            (w - 24) as f32 - st_w,
            (TITLEBAR_HEIGHT + 31) as f32,
            11.5,
            theme.accent,
            Face::Ui,
        );
    }
    if scroll_y > 0 {
        canvas.fill(
            Rect::new(SIDEBAR_W, TITLEBAR_HEIGHT + HEADER_H - 1, w - SIDEBAR_W, 1),
            theme.window_border,
        );
    }

    let viewport = scroll_viewport(w, h);
    let card_w = (w - CONTENT_X - 24).max(480);
    let start_y = viewport.y + 6 - scroll_y;
    let mut end_y = start_y;

    canvas.with_clip(viewport, |canvas| {
        let mut y = start_y;
        match page {
            Page::Appearance => {
                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "主题与色彩");
                let c1 = Rect::new(CONTENT_X, y, card_w, 166);
                ui::card(canvas, theme, c1);
                text.draw(
                    canvas,
                    "外观模式",
                    (CONTENT_X + 16) as f32,
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
                        CONTENT_X + card_w - 16 - 332 + idx as i32 * 170,
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
                    canvas.rounded_rect(card_r, RADIUS_CONTROL, fill, 1.0);
                    canvas.rounded_border(
                        card_r,
                        RADIUS_CONTROL,
                        if selected {
                            theme.accent
                        } else {
                            theme.window_border
                        },
                        1.0,
                        if selected { 2 } else { 1 },
                    );
                    let mini = Rect::new(card_r.x + 12, card_r.y + 11, 56, 44);
                    canvas.rounded_rect(mini, 4, bg_col, 1.0);
                    canvas.fill(
                        Rect::new(mini.x + 1, mini.y + 1, mini.w - 2, 10),
                        chrome_col,
                    );
                    canvas.rounded_border(mini, 4, theme.panel_border, 1.0, 1);
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

                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + 94);
                let active_accent_name = ACCENT_PRESETS
                    .iter()
                    .find_map(|(rgb, name)| (*rgb == cfg.accent).then_some(*name))
                    .unwrap_or("自定义");
                let accent_sub = format!("当前：{active_accent_name}");
                draw_row_labels(
                    canvas,
                    text,
                    theme,
                    CONTENT_X,
                    y + 102,
                    200,
                    "强调色",
                    &accent_sub,
                );
                let sw_total_w = ACCENT_PRESETS.len() as i32 * 34 - 6;
                let sw_start_x = CONTENT_X + card_w - 16 - sw_total_w;
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

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "字体与缩放");
                let c2 = Rect::new(CONTENT_X, y, card_w, ROW_H);
                ui::card(canvas, theme, c2);
                let scale_idx = nearest_preset_index(&FONT_SCALES, i64::from(cfg.font_scale));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                    CONTENT_X,
                    y,
                    "有线以太网与 NetworkManager 服务",
                );

                let connected = st.connected();
                let header_h = 68;
                let detail_row_h = 38;
                let detail_count = if net_state.ping_note.is_empty() { 5 } else { 6 };
                let c_status =
                    Rect::new(CONTENT_X, y, card_w, header_h + detail_count * detail_row_h);
                ui::card(canvas, theme, c_status);

                let badge_r = Rect::new(CONTENT_X + 16, y + 14, 40, 40);
                let badge_bg = if connected {
                    theme.accent_soft
                } else {
                    theme.window_alt
                };
                canvas.rounded_rect(badge_r, RADIUS_CONTROL, badge_bg, 1.0);
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

                let show_renew = cfg.net_enabled && cfg.net_mode == NetMode::Dhcp && st.has_device;
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
                    draw_row_divider(canvas, theme, CONTENT_X, card_w, ry);
                    text.draw(
                        canvas,
                        k,
                        (CONTENT_X + 16) as f32,
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
                        (CONTENT_X + card_w - 16) as f32 - vw,
                        (ry + 24) as f32,
                        11.5,
                        *v_col,
                        Face::Ui,
                    );
                }
                y += c_status.h + 16;

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "连接控制与 IPv4 参数");
                let is_static = cfg.net_mode == NetMode::Static;
                let static_extra_h = if is_static { 4 * ROW_H + 54 } else { 0 };
                let c_cfg = Rect::new(CONTENT_X, y, card_w, ROW_H * 2 + static_extra_h);
                ui::card(canvas, theme, c_cfg);

                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y,
                    "启用有线网络",
                    "由 systemg 托管的 NetworkManager 服务自动维护链路、DHCP 与路由",
                    cfg.net_enabled,
                    HitTarget::NetEnabled,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                        draw_row_divider(canvas, theme, CONTENT_X, card_w, fy);
                        let in_rect = Rect::new(
                            CONTENT_X + card_w - 16 - input_w,
                            fy + (ROW_H - 32) / 2,
                            input_w,
                            32,
                        );
                        let max_lw = in_rect.x - (CONTENT_X + 16) - 14;
                        draw_row_labels(canvas, text, theme, CONTENT_X, fy, max_lw, label, sub);
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
                    draw_row_divider(canvas, theme, CONTENT_X, card_w, action_y);
                    text.draw(
                        canvas,
                        "按 Tab 切换输入框，按 Enter 或点击右侧按钮提交至 NetworkManager",
                        (CONTENT_X + 16) as f32,
                        (action_y + 31) as f32,
                        11.0,
                        theme.text_muted,
                        Face::Ui,
                    );
                    let apply_btn =
                        Rect::new(CONTENT_X + card_w - 16 - 118, action_y + 10, 118, 34);
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
                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "桌面背景");
                let c1 = Rect::new(CONTENT_X, y, card_w, ROW_H + 226 + ROW_H);
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
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);

                let grid_y = y + ROW_H + 14;
                let cell_w = (card_w - 32 - 12) / 2;
                let cell_h = 92;
                for (i, wp) in WALLPAPERS.iter().enumerate() {
                    let col = (i % 2) as i32;
                    let row = (i / 2) as i32;
                    let r = Rect::new(
                        CONTENT_X + 16 + col * (cell_w + 12),
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, solid_y);
                draw_row_labels(
                    canvas,
                    text,
                    theme,
                    CONTENT_X,
                    solid_y,
                    240,
                    "背景底色",
                    "用于纯色背景与适应模式留边底色",
                );
                let sw_total = SOLID_BG_PRESETS.len() as i32 * 34 - 6;
                let sw_x0 = CONTENT_X + card_w - 16 - sw_total;
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

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "桌面图标");
                let c2 = Rect::new(CONTENT_X, y, card_w, ROW_H * 2);
                ui::card(canvas, theme, c2);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y,
                    "显示桌面图标",
                    "在桌面上展示常用应用程序的快捷方式",
                    cfg.desktop_icons_visible,
                    HitTarget::DesktopIconsVisible,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                let icon_sz_idx =
                    nearest_preset_index(&DESKTOP_ICON_SIZES, i64::from(cfg.desktop_icon_size));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "系统面板");
                let c3 = Rect::new(CONTENT_X, y, card_w, ROW_H * 3);
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
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                let panel_sz_idx = nearest_preset_index(&PANEL_SIZES, i64::from(cfg.panel_size));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 2);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H * 2,
                    "显示固定应用",
                    "在开始菜单按钮右侧显示常用应用快速启动图标",
                    cfg.panel_show_pinned,
                    HitTarget::PanelShowPinned,
                    hover,
                );
                y += c3.h + 18;
            }

            Page::Windows => {
                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "标题栏");
                let c1 = Rect::new(CONTENT_X, y, card_w, ROW_H * 2);
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
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
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
                    CONTENT_X,
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

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "焦点与布局");
                let c2 = Rect::new(CONTENT_X, y, card_w, ROW_H * 3);
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
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H,
                    "悬停时自动置顶",
                    "在悬停激活模式下将获得焦点的窗口自动提升至顶层",
                    cfg.wm_auto_raise,
                    HitTarget::WmAutoRaise,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 2);
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
                    CONTENT_X,
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
                let c1 = Rect::new(CONTENT_X, y + 4, card_w, 108);
                ui::card(canvas, theme, c1);

                // Clean monitor bezel illustration showing screen aspect ratio.
                let mon = Rect::new(CONTENT_X + 20, c1.y + 18, 104, 64);
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

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "分辨率");
                let c2 = Rect::new(CONTENT_X, y, card_w, 118);
                ui::card(canvas, theme, c2);
                let active_mode = format!("{}x{}", screen_size.0, screen_size.1);
                let btn_w = (card_w - 32 - 20) / 3;
                for (i, (mode_arg, label)) in RESOLUTIONS.iter().enumerate() {
                    let col = (i % 3) as i32;
                    let row = (i / 3) as i32;
                    let r = Rect::new(
                        CONTENT_X + 16 + col * (btn_w + 10),
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

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "节能");
                let c3 = Rect::new(CONTENT_X, y, card_w, ROW_H);
                ui::card(canvas, theme, c3);
                let blank_idx =
                    nearest_preset_index(&BLANK_TIMEOUTS, i64::from(cfg.display_blank_timeout_sec));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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

            Page::Input => {
                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "鼠标");
                let c1 = Rect::new(CONTENT_X, y, card_w, ROW_H * 4);
                ui::card(canvas, theme, c1);
                let hand_idx = usize::from(cfg.mouse_left_handed);
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                let speed_idx = nearest_preset_index(&MOUSE_SPEEDS, i64::from(cfg.mouse_speed));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 2);
                let dc_idx =
                    nearest_preset_index(&DOUBLE_CLICK_SPEEDS, cfg.mouse_double_click_ms as i64);
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 3);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H * 3,
                    "自然滚动",
                    "使页面内容沿手指滑动方向移动",
                    cfg.mouse_natural_scroll,
                    HitTarget::NaturalScroll,
                    hover,
                );
                y += c1.h + 16;

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "键盘");
                let c2 = Rect::new(CONTENT_X, y, card_w, ROW_H * 3);
                ui::card(canvas, theme, c2);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y,
                    "按键重复输入",
                    "按住字符键不放时连续输入字符",
                    cfg.key_repeat_enabled,
                    HitTarget::KeyRepeatEnabled,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                let delay_idx =
                    nearest_preset_index(&KEY_DELAYS, i64::from(cfg.key_repeat_delay_ms));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 2);
                let rate_idx = nearest_preset_index(&KEY_RATES, i64::from(cfg.key_repeat_rate_hz));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "终端");
                let c1 = Rect::new(CONTENT_X, y, card_w, ROW_H * 3);
                ui::card(canvas, theme, c1);
                let tf_idx = nearest_preset_index(&CODE_FONT_SIZES, i64::from(cfg.term_font_size));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
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
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 2);
                let sb_idx =
                    nearest_preset_index(&SCROLLBACK_PRESETS, cfg.term_scrollback_lines as i64);
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "文件管理器");
                let c2 = Rect::new(CONTENT_X, y, card_w, ROW_H * 3);
                ui::card(canvas, theme, c2);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y,
                    "显示隐藏文件",
                    "在目录视图中展示以点号 (.) 开头的文件与目录",
                    cfg.files_show_hidden,
                    HitTarget::FilesShowHidden,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H,
                    "文件夹排在文件前",
                    "浏览目录时始终将文件夹分组排列在普通文件上方",
                    cfg.files_folders_first,
                    HitTarget::FilesFoldersFirst,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 2);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H * 2,
                    "单击直接打开项目",
                    "单击即可进入文件夹或打开文件，关闭则使用双击打开",
                    cfg.files_single_click,
                    HitTarget::FilesSingleClick,
                    hover,
                );
                y += c2.h + 16;

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "文本编辑器");
                let c3 = Rect::new(CONTENT_X, y, card_w, ROW_H * 4);
                ui::card(canvas, theme, c3);
                let ef_idx =
                    nearest_preset_index(&CODE_FONT_SIZES, i64::from(cfg.editor_font_size));
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H,
                    "显示行号栏",
                    "在文档编辑区左侧显示行号标尺",
                    cfg.editor_show_line_numbers,
                    HitTarget::EditorLineNumbers,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 2);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H * 2,
                    "高亮当前行",
                    "突出显示光标当前所在的文本行背景",
                    cfg.editor_highlight_active_line,
                    HitTarget::EditorHighlightLine,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 3);
                let tab_idx = nearest_preset_index(&TAB_WIDTHS, cfg.editor_tab_width as i64);
                draw_segmented_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
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
                let hero = Rect::new(CONTENT_X, y + 4, card_w, 86);
                ui::card(canvas, theme, hero);
                text.draw(
                    canvas,
                    &time_str,
                    (CONTENT_X + 20) as f32,
                    (hero.y + 39) as f32,
                    22.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    &date_str,
                    (CONTENT_X + 20) as f32,
                    (hero.y + 65) as f32,
                    12.0,
                    theme.text_dim,
                    Face::Ui,
                );
                y = hero.bottom() + 16;

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "时钟格式");
                let c1 = Rect::new(CONTENT_X, y, card_w, ROW_H * 3);
                ui::card(canvas, theme, c1);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y,
                    "24 小时制",
                    "关闭后使用上午 / 下午 12 小时制显示时间",
                    cfg.clock_24h,
                    HitTarget::Clock24h,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H,
                    "显示秒数",
                    "在面板时钟中每秒更新并显示秒数",
                    cfg.clock_seconds,
                    HitTarget::ClockSeconds,
                    hover,
                );
                draw_row_divider(canvas, theme, CONTENT_X, card_w, y + ROW_H * 2);
                draw_toggle_row(
                    canvas,
                    text,
                    theme,
                    viewport,
                    hits,
                    CONTENT_X,
                    card_w,
                    y + ROW_H * 2,
                    "面板显示日期",
                    "在桌面面板右侧时钟旁同时显示月日与星期",
                    cfg.clock_show_date,
                    HitTarget::ClockShowDate,
                    hover,
                );
                y += c1.h + 16;

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "时区");
                let tz_row_h = 44;
                let c2 = Rect::new(CONTENT_X, y, card_w, tz_row_h * TIMEZONES.len() as i32);
                ui::card(canvas, theme, c2);
                for (i, (tz_id, tz_name)) in TIMEZONES.iter().enumerate() {
                    let ry = y + i as i32 * tz_row_h;
                    if i > 0 {
                        draw_row_divider(canvas, theme, CONTENT_X, card_w, ry);
                    }
                    let row_r = Rect::new(CONTENT_X + 6, ry + 4, card_w - 12, tz_row_h - 8);
                    let selected = cfg.timezone == *tz_id;
                    let target = HitTarget::Timezone(i);
                    if selected {
                        canvas.rounded_rect(row_r, RADIUS_CONTROL, theme.accent_soft, 1.0);
                    } else if hover == Some(target) {
                        canvas.rounded_rect(row_r, RADIUS_CONTROL, theme.panel_hover, 1.0);
                    }
                    text.draw(
                        canvas,
                        tz_name,
                        (CONTENT_X + 18) as f32,
                        (ry + 27) as f32,
                        12.5,
                        if selected { theme.accent } else { theme.text },
                        if selected { Face::Bold } else { Face::Ui },
                    );
                    let id_w = text.measure(tz_id, 11.5, Face::Ui);
                    text.draw(
                        canvas,
                        tz_id,
                        (CONTENT_X + card_w - 20) as f32 - id_w,
                        (ry + 27) as f32,
                        11.5,
                        if selected {
                            theme.accent
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
                let hero = Rect::new(CONTENT_X, y + 4, card_w, 92);
                ui::card(canvas, theme, hero);
                text.draw(
                    canvas,
                    "启动与系统日志",
                    (CONTENT_X + 20) as f32,
                    (hero.y + 34) as f32,
                    18.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    "内核引导加载、硬件探测及系统服务运行日志",
                    (CONTENT_X + 20) as f32,
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
                    (CONTENT_X + card_w - 20) as f32 - up_w,
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
                let mut sx = CONTENT_X + 20;
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
                let c_act = Rect::new(CONTENT_X, y, card_w, act_h);
                ui::card(canvas, theme, c_act);

                let filter_tabs = ["全部日志", "内核消息", "系统服务", "异常与告警"];
                let filter_w = 92;
                for (f_idx, flabel) in filter_tabs.iter().enumerate() {
                    let btn_r = Rect::new(
                        CONTENT_X + 8 + f_idx as i32 * (filter_w + 4),
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
                        canvas.rounded_rect(btn_r, RADIUS_CONTROL, theme.accent_soft, 1.0);
                        canvas.fill(
                            Rect::new(btn_r.x + 12, btn_r.bottom() - 2, btn_r.w - 24, 2),
                            theme.accent,
                        );
                    } else if hover == Some(target) {
                        canvas.rounded_rect(btn_r, RADIUS_CONTROL, theme.panel_hover, 1.0);
                    }
                    let lw =
                        text.measure(flabel, 11.5, if is_active { Face::Bold } else { Face::Ui });
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
                        let d_card = Rect::new(CONTENT_X, y, card_w, 96);
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
                            (CONTENT_X + 16) as f32,
                            (d_card.y + 26) as f32,
                            11.5,
                            theme.accent,
                            Face::Bold,
                        );
                        text.draw(
                            canvas,
                            "详细消息内容:",
                            (CONTENT_X + 16) as f32,
                            (d_card.y + 48) as f32,
                            11.0,
                            theme.text_dim,
                            Face::Ui,
                        );
                        let msg_shown =
                            text.truncate(&entry.message, (card_w - 36) as f32, 11.5, Face::Mono);
                        text.draw(
                            canvas,
                            &msg_shown,
                            (CONTENT_X + 16) as f32,
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
                let c_table = Rect::new(CONTENT_X, y, card_w, table_h + 12);
                ui::card(canvas, theme, c_table);

                if filtered.is_empty() {
                    text.draw(
                        canvas,
                        "暂无符合条件的日志记录",
                        (CONTENT_X + card_w / 2) as f32 - 70.0,
                        (y + 26) as f32,
                        12.0,
                        theme.text_muted,
                        Face::Ui,
                    );
                } else {
                    for (list_idx, (orig_idx, entry)) in filtered.iter().enumerate() {
                        let ry = y + 6 + list_idx as i32 * row_h;
                        let row_r = Rect::new(CONTENT_X + 4, ry, card_w - 8, row_h);
                        let is_selected = log_state.selected == Some(*orig_idx);
                        let target = HitTarget::LogSelect(*orig_idx);

                        if is_selected {
                            canvas.rounded_rect(row_r, RADIUS_CONTROL, theme.accent_soft, 1.0);
                        } else if hover == Some(target) {
                            canvas.rounded_rect(row_r, RADIUS_CONTROL, theme.panel_hover, 1.0);
                        }

                        let (badge_txt, badge_bg, badge_fg) = match entry.prio {
                            0..=3 => ("ERR", theme.danger, 0xffffff),
                            4 => ("WARN", theme.warning, 0x18181b),
                            5 | 6 => ("INFO", theme.accent_soft, theme.accent),
                            _ => ("DBG", theme.panel_border, theme.text_muted),
                        };
                        let b_rect = Rect::new(CONTENT_X + 10, ry + 5, 38, 18);
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
                            (CONTENT_X + 54) as f32,
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
                            (CONTENT_X + 160) as f32,
                            (ry + 19) as f32,
                            10.5,
                            if is_selected {
                                theme.accent
                            } else {
                                theme.text_dim
                            },
                            Face::Bold,
                        );

                        let msg_x = CONTENT_X + 258;
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
                let hero = Rect::new(CONTENT_X, y + 4, card_w, 98);
                ui::card(canvas, theme, hero);
                icons::draw(
                    canvas,
                    Icon::SpaceLogo,
                    CONTENT_X + 20,
                    hero.y + 21,
                    56,
                    None,
                );
                text.draw(
                    canvas,
                    "SpaceOS 9",
                    (CONTENT_X + 92) as f32,
                    (hero.y + 45) as f32,
                    19.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    "ViudiraTech 桌面操作系统",
                    (CONTENT_X + 92) as f32,
                    (hero.y + 70) as f32,
                    12.5,
                    theme.text_dim,
                    Face::Ui,
                );
                y = hero.bottom() + 16;

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "硬件信息");
                let hw_rows: [(&str, &str); 4] = [
                    ("设备名称", &sys_info.hostname),
                    ("处理器", &sys_info.cpu),
                    ("内存容量", &sys_info.memory),
                    ("磁盘容量", &sys_info.disk),
                ];
                let info_row_h = 42;
                let c_hw = Rect::new(CONTENT_X, y, card_w, info_row_h * hw_rows.len() as i32);
                ui::card(canvas, theme, c_hw);
                for (idx, (k, v)) in hw_rows.iter().enumerate() {
                    let ry = y + idx as i32 * info_row_h;
                    if idx > 0 {
                        draw_row_divider(canvas, theme, CONTENT_X, card_w, ry);
                    }
                    text.draw(
                        canvas,
                        k,
                        (CONTENT_X + 16) as f32,
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
                        (CONTENT_X + card_w - 18) as f32 - vw,
                        (ry + 26) as f32,
                        12.0,
                        theme.text,
                        Face::Ui,
                    );
                }
                y += c_hw.h + 16;

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "软件环境");
                let sw_rows: [(&str, &str); 3] = [
                    ("操作系统", &sys_info.os_name),
                    ("桌面环境", "SPUI 4.0"),
                    ("内核版本", &sys_info.kernel_arch),
                ];
                let c_sw = Rect::new(CONTENT_X, y, card_w, info_row_h * sw_rows.len() as i32);
                ui::card(canvas, theme, c_sw);
                for (idx, (k, v)) in sw_rows.iter().enumerate() {
                    let ry = y + idx as i32 * info_row_h;
                    if idx > 0 {
                        draw_row_divider(canvas, theme, CONTENT_X, card_w, ry);
                    }
                    text.draw(
                        canvas,
                        k,
                        (CONTENT_X + 16) as f32,
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
                        (CONTENT_X + card_w - 18) as f32 - vw,
                        (ry + 26) as f32,
                        12.0,
                        theme.text,
                        Face::Ui,
                    );
                }
                y += c_sw.h + 18;

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "系统诊断与启动日志");
                let c_diag = Rect::new(CONTENT_X, y, card_w, 76);
                ui::card(canvas, theme, c_diag);
                text.draw(
                    canvas,
                    "系统启动与运行日志",
                    (CONTENT_X + 16) as f32,
                    (c_diag.y + 28) as f32,
                    13.0,
                    theme.text,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    "查看内核引导、硬件探测以及系统服务初始化的完整时间戳日志。",
                    (CONTENT_X + 16) as f32,
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

                y = draw_section_heading(canvas, text, theme, CONTENT_X, y, "系统维护");
                let c_reset = Rect::new(CONTENT_X, y, card_w, 92);
                ui::card(canvas, theme, c_reset);
                text.draw(
                    canvas,
                    "恢复出厂设置",
                    (CONTENT_X + 16) as f32,
                    (c_reset.y + 30) as f32,
                    13.0,
                    theme.danger,
                    Face::Bold,
                );
                text.draw(
                    canvas,
                    "删除本地账户、主目录和个性化配置；重启后进入首次设置向导。",
                    (CONTENT_X + 16) as f32,
                    (c_reset.y + 52) as f32,
                    10.5,
                    theme.text_muted,
                    Face::Ui,
                );
                text.draw(
                    canvas,
                    "需要 wheel 管理员确认；不会影响内核、系统文件或已安装软件。",
                    (CONTENT_X + 16) as f32,
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
    if total_h > viewport.h {
        let track = Rect::new(w - 8, viewport.y + 4, 6, (viewport.h - 8).max(8));
        ui::scrollbar_v(
            canvas,
            theme,
            track,
            total_h as usize,
            viewport.h as usize,
            scroll_y.max(0) as usize,
        );
    }
    total_h
}
