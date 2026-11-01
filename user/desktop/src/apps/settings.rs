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
use crate::x11::Result;
use std::ffi::CStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use uxconf::{
    format_hex_rgb, BackdropStyle, ButtonLayout, CursorStyle, DesktopConfig, DoubleClickAction,
    FocusMode, PanelPosition, ThemeMode, Value, WindowPlacement, ACCENT_PRESETS, RESOLUTIONS,
    SOLID_BG_PRESETS, TIMEZONES, WALLPAPERS, WALLPAPER_DIR,
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
    DesktopPanel,
    Windows,
    Display,
    Input,
    Applications,
    DateTime,
    About,
}

impl Page {
    const fn title(self) -> &'static str {
        match self {
            Self::Appearance => "外观",
            Self::DesktopPanel => "桌面与面板",
            Self::Windows => "窗口",
            Self::Display => "显示",
            Self::Input => "鼠标与键盘",
            Self::Applications => "应用程序",
            Self::DateTime => "日期与时间",
            Self::About => "关于",
        }
    }

    const fn channels(self) -> &'static [&'static str] {
        match self {
            Self::Appearance => &["appearance"],
            Self::DesktopPanel => &["desktop", "panel"],
            Self::Windows => &["wm"],
            Self::Display => &["display"],
            Self::Input => &["input"],
            Self::Applications => &["terminal", "files", "editor"],
            Self::DateTime => &["datetime"],
            Self::About => &[],
        }
    }
}

const NAV: [(Page, &str, Icon); 8] = [
    (Page::Appearance, "外观", Icon::Appearance),
    (Page::DesktopPanel, "桌面与面板", Icon::Panel),
    (Page::Windows, "窗口", Icon::WindowManager),
    (Page::Display, "显示", Icon::Display),
    (Page::Input, "鼠标与键盘", Icon::InputDevices),
    (Page::Applications, "应用程序", Icon::Applications),
    (Page::DateTime, "日期与时间", Icon::DateTime),
    (Page::About, "关于", Icon::About),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HitTarget {
    Nav(usize),
    ResetPage,
    ThemeMode(ThemeMode),
    Accent(usize),
    FontScale(usize),
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

pub fn run() -> Result<()> {
    let mut app = App::new("系统设置", Icon::Settings, 840, 580)?;
    app.min_width = 760;
    app.min_height = 500;
    apply_local_timezone(&app.config.timezone);

    let mut page = Page::Appearance;
    let mut scroll_y: i32 = 0;
    let mut content_h: i32 = 0;
    let mut hover: Option<HitTarget> = None;
    let mut hits: Vec<HitBox> = Vec::with_capacity(64);
    let mut status = String::new();
    let mut screen_size = query_root_resolution(&app);
    let mut last_clock_preview = String::new();
    let thumbnails = load_wallpaper_thumbnails();
    let sys_info = SystemInfo::gather();

    loop {
        if page == Page::DateTime {
            let (now_str, _) = format_datetime_preview(&app.config);
            if now_str != last_clock_preview {
                last_clock_preview = now_str;
                app.needs_redraw = true;
            }
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
        let (events, _) = app.pump_with_fd(None, timeout_ms)?;
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
                    app.needs_redraw = true;
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
                    if let Some(target) = find_hit(&hits, x, y) {
                        handle_click(
                            &mut app,
                            target,
                            &mut page,
                            &mut scroll_y,
                            &mut screen_size,
                            &mut status,
                        );
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_click(
    app: &mut App,
    target: HitTarget,
    page: &mut Page,
    scroll_y: &mut i32,
    screen_size: &mut (u16, u16),
    status: &mut String,
) {
    status.clear();
    match target {
        HitTarget::Nav(i) => {
            let next_page = NAV[i].0;
            if *page != next_page {
                *page = next_page;
                *scroll_y = 0;
                if next_page == Page::Display {
                    *screen_size = query_root_resolution(app);
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
    let sw_rect = Rect::new(card_x + card_w - 16 - 38, y + (ROW_H - 20) / 2, 38, 20);
    let label_max_w = sw_rect.x - (card_x + 16) - 16;
    draw_row_labels(canvas, text, theme, card_x, y, label_max_w, title, subtitle);
    ui::toggle_switch(canvas, theme, sw_rect, checked, hover == Some(target));
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
    ui::segmented_control(canvas, text, theme, seg_rect, labels, selected, hover_idx);
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

    for (i, (id, label, icon)) in NAV.iter().enumerate() {
        let r = Rect::new(10, TITLEBAR_HEIGHT + 36 + i as i32 * 38, SIDEBAR_W - 20, 34);
        let active = page == *id;
        let target = HitTarget::Nav(i);
        if active {
            canvas.rounded_rect(r, RADIUS_CONTROL, theme.accent_soft, 1.0);
            canvas.fill(Rect::new(r.x, r.y + 7, 3, r.h - 14), theme.accent);
        } else if hover == Some(target) {
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
