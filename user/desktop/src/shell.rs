/*
 *
 *       user/desktop/src/shell.rs
 *       SPUI 4.0 desktop shell: WM, backdrop, shortcuts, configurable panel & start menu
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The shell acts as the X11 window manager and renders the desktop backdrop
//! (cover / contain / stretch / solid), shortcut icons, right-click context
//! menu, top- or bottom-docked panel, XShape-clipped Start Menu (with live
//! search) and notification toasts, synchronized live with `uxconfd`.

use crate::compositor::{
    attach_window_pixmap, blit_scaled_window, blit_window_pixels_split, capture_dirty_windows,
    capture_one_window, composite_shell_layer, compute_thumb_popup, detach_window_resources,
    draw_aero_peek_sheet, draw_alt_tab_hud, draw_flip3d_scene, draw_thumb_preview, lerp_rect,
    paint_shell_glass_region, scaled_rect_around_center, window_damage_bounds, AltTabState,
    AnimKind, ClosingGhost, CompWindow, Compositor, Flip3dState, SnapZone, SwitcherEntry,
    WindowAnim,
};
use crate::ddm::wire::PowerAction;
use crate::draw::{mix, BlurScratch, Canvas, Rect, RectRegion, Sprite};
use crate::icons::{self, Icon};
use crate::start_screen::{ShellConfigHost, StartAction, StartScreenState};
use spime::client::SpimeClient;
use spime::proto::DaemonReply;
use crate::text::{Face, TextGlow, TextRenderer};
use crate::theme::{Theme, VORTEX_TITLEBAR_HEIGHT};
use crate::ui;
use crate::vortex::{
    draw_space_start_orb, draw_vortex_frame_chrome_cached, smootherstep, AERO_BLUR_RADIUS,
};
use crate::x11::{CursorKind, Key, Result, X};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uxconf::client::{UxConfClient, UxConfWatch};
use uxconf::{
    BackdropStyle, CompositorQuality, DesktopConfig, FocusMode, PanelPosition, WindowPlacement,
};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigWindow, ConfigureNotifyEvent, ConfigureWindowAux,
    ConnectionExt, EventMask, ModMask, NotifyDetail, NotifyMode, PropMode, StackMode, Window,
};
use x11rb::protocol::Event;

extern "C" {
    fn tzset();
}

const MENU_WIDTH: i32 = 380;
const MENU_HEIGHT: i32 = 530;
const VORTEX_MENU_WIDTH: i32 = 448;
const VORTEX_MENU_HEIGHT: i32 = 536;
const CTX_WIDTH: i32 = 192;
const CTX_HEIGHT: i32 = 148;
const TOAST_WIDTH: i32 = 324;
const TOAST_HEIGHT: i32 = 74;
const VOL_POPUP_WIDTH: i32 = 286;
const VOL_POPUP_HEIGHT: i32 = 138;
const VOLUME_PERSIST_DEBOUNCE: Duration = Duration::from_millis(250);

struct StartPlace {
    label: &'static str,
    icon: Icon,
    mode: &'static str,
    arg: Option<&'static str>,
}

const VORTEX_PLACES: [StartPlace; 7] = [
    StartPlace {
        label: "我的文档",
        icon: Icon::Files,
        mode: "files",
        arg: None,
    },
    StartPlace {
        label: "图片相册",
        icon: Icon::ImageViewer,
        mode: "view",
        arg: None,
    },
    StartPlace {
        label: "音乐播放",
        icon: Icon::Music,
        mode: "music",
        arg: None,
    },
    StartPlace {
        label: "计算机",
        icon: Icon::Files,
        mode: "files",
        arg: Some("/"),
    },
    StartPlace {
        label: "控制面板",
        icon: Icon::Settings,
        mode: "settings",
        arg: None,
    },
    StartPlace {
        label: "帮助和支持",
        icon: Icon::Help,
        mode: "help",
        arg: None,
    },
    StartPlace {
        label: "终端控制台",
        icon: Icon::Terminal,
        mode: "term",
        arg: None,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCategory {
    System,
    Productivity,
    Media,
    Games,
}

pub struct AppEntry {
    pub name: &'static str,
    pub subtitle: &'static str,
    pub keywords: &'static str,
    pub icon: Icon,
    pub mode: &'static str,
    pub category: AppCategory,
}

pub const APPS: [AppEntry; 11] = [
    AppEntry {
        name: "文件管理器",
        subtitle: "浏览目录与系统文件",
        keywords: "files folder directory dolphin 文件 目录",
        icon: Icon::Files,
        mode: "files",
        category: AppCategory::Productivity,
    },
    AppEntry {
        name: "终端",
        subtitle: "交互式命令行会话",
        keywords: "term terminal shell sh bash console 终端 命令行",
        icon: Icon::Terminal,
        mode: "term",
        category: AppCategory::System,
    },
    AppEntry {
        name: "系统监视器",
        subtitle: "CPU、内存、进程性能监控与管理",
        keywords: "monitor taskmgr top htop task manager 监视器 性能 进程 任务管理器",
        icon: Icon::Monitor,
        mode: "monitor",
        category: AppCategory::System,
    },
    AppEntry {
        name: "文本编辑器",
        subtitle: "查看与编辑文本文档",
        keywords: "edit editor text notepad kwrite 文本 编辑器",
        icon: Icon::Editor,
        mode: "edit",
        category: AppCategory::Productivity,
    },
    AppEntry {
        name: "计算器",
        subtitle: "标准四则运算与即时数值计算",
        keywords: "calc calculator math 计算器 计算 数学",
        icon: Icon::Calculator,
        mode: "calc",
        category: AppCategory::Productivity,
    },
    AppEntry {
        name: "扫雷",
        subtitle: "经典 XP 扫雷 · AI 约束推演与概率预测",
        keywords: "mines minesweeper game ai xp 扫雷 排雷 游戏 益智",
        icon: Icon::Minesweeper,
        mode: "mines",
        category: AppCategory::Games,
    },
    AppEntry {
        name: "看图工具",
        subtitle: "图像浏览、缩放与壁纸设定",
        keywords: "view image viewer photo picture 看图 图片 图像 相册",
        icon: Icon::ImageViewer,
        mode: "view",
        category: AppCategory::Media,
    },
    AppEntry {
        name: "音乐播放器",
        subtitle: "播放示例 MP3 曲目",
        keywords: "music mp3 audio player media 音乐 播放器 音频 歌曲",
        icon: Icon::Music,
        mode: "music",
        category: AppCategory::Media,
    },
    AppEntry {
        name: "系统设置",
        subtitle: "外观、网络、桌面、输入设备与系统偏好",
        keywords:
            "settings network ip dhcp ethernet display wallpaper theme 外观 网络 壁纸 分辨率 设置",
        icon: Icon::Settings,
        mode: "settings",
        category: AppCategory::System,
    },
    AppEntry {
        name: "帮助手册",
        subtitle: "SpaceOS-9 桌面使用与快捷键指南",
        keywords: "help manual guide doc 帮助 手册 快捷键 文档 指南",
        icon: Icon::Help,
        mode: "help",
        category: AppCategory::System,
    },
    AppEntry {
        name: "Space 浏览器",
        subtitle: "网页浏览与 QuickJS 交互引擎",
        keywords: "browser web html internet js space 浏览器 网页 上网",
        icon: Icon::Browser,
        mode: "browser",
        category: AppCategory::Productivity,
    },
];

/// Curated taskbar quick-launch apps; all applications remain in Start Menu & Desktop.
const PINNED_MODES: [&str; 4] = ["files", "term", "edit", "settings"];

fn pinned_app_entry(slot: usize) -> Option<&'static AppEntry> {
    let mode = *PINNED_MODES.get(slot)?;
    APPS.iter().find(|app| app.mode == mode)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Hit {
    None,
    DesktopIcon(usize),
    Start,
    Pinned(usize),
    Window(usize),
    ImeTray,
    Network,
    Volume,
    Bell,
    ShowDesktop,
    VolMute,
    VolSlider,
    VolSfx,
    VolSettings,
    MenuRow(usize),
    MenuPlace(usize),
    Logout,
    Reboot,
    PowerOff,
    ConfirmPowerOk,
    ConfirmPowerCancel,
    ContextRow(usize),
}

struct Managed {
    window: Window,
    title: String,
    icon: Icon,
    minimized: bool,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

struct Toast {
    title: String,
    body: String,
    deadline: Instant,
}

/// Tracked external `override_redirect` popup/menu/IME candidate window.
struct OverrideWin {
    window: Window,
    rect: Rect,
    cw: CompWindow,
}

/// Per-window geometry and visibility plan computed during single-pass occlusion culling.
#[derive(Default)]
struct WinCompPlan {
    window: Window,
    draw_rect: Rect,
    cull_bounds: Rect,
    opaque_rect: Rect,
    glass_strips: [Rect; 4],
    glass_count: usize,
    is_max: bool,
    is_anim: bool,
    peeking_out: bool,
    has_vortex_glass: bool,
    active: bool,
    glass_top: i32,
    eff_op: u8,
    cw_pw: usize,
    cw_ph: usize,
    cw_depth: u8,
    win_vis: Vec<Rect>,
    glass_vis: Vec<(Rect, bool)>,
}

impl WinCompPlan {
    fn reset(&mut self) {
        self.glass_count = 0;
        self.win_vis.clear();
        self.glass_vis.clear();
    }
}

#[derive(Clone, Copy)]
struct WmDrag {
    window: Window,
    mode: u32,
    orig_x: i32,
    orig_y: i32,
    orig_w: i32,
    orig_h: i32,
    press_rx: i32,
    press_ry: i32,
    min_w: i32,
    min_h: i32,
    pending_x: i32,
    pending_y: i32,
    pending_w: i32,
    pending_h: i32,
    has_pending: bool,
}

pub struct Shell {
    x: X,
    text: TextRenderer,
    config: DesktopConfig,
    ux_client: Option<UxConfClient>,
    watch: Option<UxConfWatch>,
    theme: Theme,
    comp: Compositor,
    comps: HashMap<Window, CompWindow>,
    z_stack: Vec<Window>,
    override_wins: Vec<OverrideWin>,
    wallpaper_raw: Option<Sprite>,
    wallpaper_scaled: Vec<u32>,
    wallpaper_file: String,
    desktop: Window,
    desktop_canvas: Canvas,
    /// Server-side copy of the desktop canvas used as the desktop window's
    /// background, so Xorg repaints uncovered areas itself during window moves.
    desktop_bg: u32,
    selected_desktop_icon: Option<usize>,
    taskbar: Window,
    taskbar_canvas: Canvas,
    thumb_window: Window,
    menu: Window,
    menu_canvas: Canvas,
    menu_open: bool,
    menu_query: String,
    menu_ime_preedit: String,
    menu_ime_focused: bool,
    menu_ime_spot: Option<(i32, i32, i32)>,
    menu_confirm_power: Option<PowerAction>,
    start_screen: StartScreenState,
    ctx_menu: Window,
    ctx_canvas: Canvas,
    ctx_open: bool,
    ctx_is_ime: bool,
    ctx_pos: (i16, i16),
    ime_client: SpimeClient,
    ime_chinese: bool,
    ime_active: bool,
    comp_shm_pending_sync: bool,
    vol_popup: Window,
    vol_canvas: Canvas,
    vol_open: bool,
    vol_dragging: bool,
    vol_persist: VolumePersistState,
    pointer_grab: Option<Window>,
    kbd_grabbed: bool,
    wm_drag: Option<WmDrag>,
    toast_window: Window,
    toast_canvas: Canvas,
    toast: Option<Toast>,
    windows: Vec<Managed>,
    active: Option<Window>,
    all_minimized_backup: Vec<Window>,
    hover: Hit,
    clock_tick: (i32, i32, i32),
    net_fd: Option<i32>,
    net_status: crate::netmgr::NetStatus,
    last_net_refresh: Instant,
    last_launch: Option<(&'static str, Instant)>,
    last_composite: Instant,
    theme_stamp: Option<std::time::SystemTime>,
    dirty_rects_buf: Vec<Rect>,
    comp_plan_buf: Vec<WinCompPlan>,
    capture_keep_buf: Vec<Window>,
    running: bool,
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.release_popup_grab();
        if self.kbd_grabbed {
            self.x.ungrab_keyboard();
            self.kbd_grabbed = false;
        }
        self.x.ungrab_pointer();
        for (_, mut cw) in self.comps.drain() {
            detach_window_resources(&self.x, &mut cw);
        }
        for mut ov in self.override_wins.drain(..) {
            detach_window_resources(&self.x, &mut ov.cw);
        }
        self.comp.shutdown(&self.x);
        self.flush_volume_persist();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SoundSnapshot {
    master_volume: i32,
    muted: bool,
    enabled: bool,
}

impl SoundSnapshot {
    fn from_config(cfg: &DesktopConfig) -> Self {
        Self {
            master_volume: cfg.sound_master_volume,
            muted: cfg.sound_muted,
            enabled: cfg.sound_enabled,
        }
    }

    fn apply_to_config(self, cfg: &mut DesktopConfig) {
        cfg.sound_master_volume = self.master_volume;
        cfg.sound_muted = self.muted;
        cfg.sound_enabled = self.enabled;
    }
}

/// Coalesces burst clicks, wheel scrolls, and slider drags into a single
/// debounced `uxconfd` persist so the shell UI thread never blocks while a
/// popup pointer grab is held.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VolumePersistState {
    persisted: SoundSnapshot,
    dirty: bool,
    deadline: Option<Instant>,
}

impl VolumePersistState {
    fn new(initial: SoundSnapshot) -> Self {
        Self {
            persisted: initial,
            dirty: false,
            deadline: None,
        }
    }

    fn stage(&mut self, current: SoundSnapshot, now: Instant) {
        if current == self.persisted {
            self.dirty = false;
            self.deadline = None;
        } else {
            self.dirty = true;
            self.deadline = Some(now + VOLUME_PERSIST_DEBOUNCE);
        }
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn timeout_ms_until(&self, now: Instant) -> Option<i32> {
        if !self.dirty {
            return None;
        }
        self.deadline.map(|dl| {
            dl.saturating_duration_since(now)
                .as_millis()
                .clamp(1, i32::MAX as u128) as i32
        })
    }

    fn is_due(&self, now: Instant) -> bool {
        self.dirty && self.deadline.is_some_and(|dl| now >= dl)
    }

    fn take_pending_changes(
        &mut self,
        current: SoundSnapshot,
    ) -> Option<Vec<(&'static str, uxconf::Value)>> {
        if !self.dirty && current == self.persisted {
            self.deadline = None;
            return None;
        }
        let mut props = Vec::with_capacity(3);
        if current.master_volume != self.persisted.master_volume {
            props.push((
                "/master-volume",
                uxconf::Value::Int(i64::from(current.master_volume)),
            ));
        }
        if current.muted != self.persisted.muted {
            props.push(("/muted", uxconf::Value::Bool(current.muted)));
        }
        if current.enabled != self.persisted.enabled {
            props.push((
                "/system-sounds-enabled",
                uxconf::Value::Bool(current.enabled),
            ));
        }
        self.persisted = current;
        self.dirty = false;
        self.deadline = None;
        if props.is_empty() {
            None
        } else {
            Some(props)
        }
    }

    fn sync_from_external(&mut self, external: SoundSnapshot) {
        if !self.dirty {
            self.persisted = external;
        }
    }
}

fn transition_popup_grab(current: Option<Window>, target: Window, grabbed: bool) -> Option<Window> {
    let _ = current;
    if grabbed {
        Some(target)
    } else {
        None
    }
}

fn popup_contains_point(width: i32, height: i32, x: i32, y: i32) -> bool {
    Rect::new(0, 0, width, height).contains(x, y)
}

fn taskbar_local_point(
    screen_w: i32,
    taskbar_y: i32,
    panel_h: i32,
    root_x: i32,
    root_y: i32,
) -> Option<(i32, i32)> {
    if root_x < 0 || root_x >= screen_w || root_y < taskbar_y || root_y >= taskbar_y + panel_h {
        return None;
    }
    Some((root_x, root_y - taskbar_y))
}

pub fn session_identity() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_string());
    let host = fs::read_to_string("/etc/hostname")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "spaceos".to_string());
    format!("{user}@{host}")
}

pub fn now_local() -> (i32, i32, i32, i32, i32, i32, i32) {
    // SAFETY: `localtime_r` writes into stack-owned zeroed `libc::tm` with no shared state.
    unsafe {
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
    }
}

fn apply_timezone(tz: &str) {
    if tz.is_empty() || tz.contains("..") || tz.starts_with('/') {
        return;
    }
    let zone_path = format!("/usr/share/zoneinfo/{tz}");
    if Path::new(&zone_path).exists() {
        let _ = fs::remove_file("/etc/localtime");
        let _ = std::os::unix::fs::symlink(&zone_path, "/etc/localtime");
    }
    let posix_tz = uxconf::posix_tz_for_zone(tz);
    let _ = fs::write("/etc/TZ", format!("{posix_tz}\n"));
    std::env::set_var("TZ", &posix_tz);
    // SAFETY: `tzset` refreshes libc's timezone state from `TZ` / `/etc/localtime` in this single-threaded process.
    unsafe {
        tzset();
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

fn draw_etched_sep(canvas: &mut Canvas, theme: &Theme, x: i32, y: i32, h: i32) {
    if theme.is_win98() {
        canvas.fill(Rect::new(x, y, 1, h), theme.bevel_shadow);
        canvas.fill(Rect::new(x + 1, y, 1, h), theme.bevel_hi);
    } else {
        canvas.fill(Rect::new(x, y, 1, h), theme.panel_border);
    }
}

fn draw_etched_hsep(canvas: &mut Canvas, theme: &Theme, x: i32, y: i32, w: i32) {
    if theme.is_win98() {
        canvas.fill(Rect::new(x, y, w, 1), theme.bevel_shadow);
        canvas.fill(Rect::new(x, y + 1, w, 1), theme.bevel_hi);
    } else {
        canvas.fill(Rect::new(x, y, w, 1), theme.panel_border);
    }
}

fn menu_row_rect(index: usize) -> Rect {
    Rect::new(12, 58 + index as i32 * 52, MENU_WIDTH - 24, 48)
}

fn vortex_menu_row_rect(index: usize) -> Rect {
    Rect::new(14, 16 + index as i32 * 54, 244, 50)
}

fn vortex_menu_search_rect() -> Rect {
    Rect::new(16, VORTEX_MENU_HEIGHT - 54, 240, 34)
}

fn vortex_menu_place_rect(index: usize) -> Rect {
    Rect::new(274, 82 + index as i32 * 38, VORTEX_MENU_WIDTH - 288, 34)
}

fn vortex_menu_footer_rects() -> (Rect, Rect, Rect, Rect, Rect) {
    let power_off = Rect::new(274, VORTEX_MENU_HEIGHT - 48, 76, 30);
    let reboot = Rect::new(354, VORTEX_MENU_HEIGHT - 48, 42, 30);
    let logout = Rect::new(398, VORTEX_MENU_HEIGHT - 48, 42, 30);
    let confirm_cancel = Rect::new(274, VORTEX_MENU_HEIGHT - 48, 78, 30);
    let confirm_ok = Rect::new(358, VORTEX_MENU_HEIGHT - 48, 82, 30);
    (logout, reboot, power_off, confirm_cancel, confirm_ok)
}

fn custom_menu_row_rect(sm: &crate::theme::StartMenuSkin, index: usize) -> Rect {
    let body_top = sm.header_height + 8;
    let left_w = (sm.width / 2 + 6).clamp(180, sm.width - 140);
    let extra_gap = if index >= 2 { 8 } else { 0 };
    Rect::new(
        8,
        body_top + index as i32 * 44 + extra_gap,
        left_w - 14,
        42,
    )
}

fn custom_menu_search_rect(sm: &crate::theme::StartMenuSkin) -> Rect {
    let left_w = (sm.width / 2 + 6).clamp(180, sm.width - 140);
    Rect::new(10, sm.height - sm.footer_height - 40, left_w - 18, 28)
}

fn custom_menu_place_rect(sm: &crate::theme::StartMenuSkin, index: usize) -> Rect {
    let body_top = sm.header_height + 10;
    let left_w = (sm.width / 2 + 6).clamp(180, sm.width - 140);
    let right_x = 2 + left_w + 6;
    let right_w = (sm.width - right_x - 8).max(120);
    let extra_gap = if index >= 3 { 10 } else { 0 };
    Rect::new(
        right_x,
        body_top + index as i32 * 38 + extra_gap,
        right_w,
        34,
    )
}

fn custom_menu_footer_rects(
    sm: &crate::theme::StartMenuSkin,
) -> (Rect, Rect, Rect, Rect, Rect) {
    let fy = sm.height - sm.footer_height + (sm.footer_height - 28) / 2;
    let power_off = Rect::new(sm.width - 108, fy, 98, 28);
    let reboot = Rect::new(sm.width - 182, fy, 68, 28);
    let logout = Rect::new(sm.width - 256, fy, 68, 28);
    let confirm_cancel = Rect::new(sm.width - 170, fy, 74, 28);
    let confirm_ok = Rect::new(sm.width - 88, fy, 78, 28);
    (logout, reboot, power_off, confirm_cancel, confirm_ok)
}

fn compact_footer_rects() -> (Rect, Rect, Rect, Rect, Rect) {
    let logout = Rect::new(MENU_WIDTH - 222, MENU_HEIGHT - 42, 66, 30);
    let reboot = Rect::new(MENU_WIDTH - 150, MENU_HEIGHT - 42, 66, 30);
    let power_off = Rect::new(MENU_WIDTH - 78, MENU_HEIGHT - 42, 66, 30);
    let confirm_cancel = Rect::new(MENU_WIDTH - 148, MENU_HEIGHT - 42, 64, 30);
    let confirm_ok = Rect::new(MENU_WIDTH - 78, MENU_HEIGHT - 42, 66, 30);
    (logout, reboot, power_off, confirm_cancel, confirm_ok)
}

fn ctx_row_rect(index: usize) -> Rect {
    let y = match index {
        0 => 6,
        1 => 38,
        2 => 70,
        _ => 110,
    };
    Rect::new(6, y, CTX_WIDTH - 12, 30)
}

fn vol_popup_mute_rect() -> Rect {
    Rect::new(14, 43, 30, 30)
}

fn vol_popup_slider_rect() -> Rect {
    Rect::new(52, 47, VOL_POPUP_WIDTH - 68, 22)
}

fn vol_popup_sfx_rect() -> Rect {
    Rect::new(14, 95, 134, 28)
}

fn vol_popup_settings_rect() -> Rect {
    Rect::new(VOL_POPUP_WIDTH - 110, 95, 96, 28)
}

fn vol_from_slider_x(rect: Rect, x: i32) -> i32 {
    let frac = ((x - rect.x) as f32 / rect.w.max(1) as f32).clamp(0.0, 1.0);
    (frac * 100.0).round() as i32
}

fn infer_window_icon(title: &str) -> Icon {
    let lower = title.to_ascii_lowercase();
    if title.contains("终端") || lower.contains("term") {
        Icon::Terminal
    } else if title.contains("文件") || lower.contains("file") {
        Icon::Files
    } else if title.contains("编辑") || lower.contains("edit") {
        Icon::Editor
    } else if title.contains("监视器") || lower.contains("monitor") || title.contains("任务") {
        Icon::Monitor
    } else if title.contains("计算器") || lower.contains("calc") {
        Icon::Calculator
    } else if title.contains("扫雷") || lower.contains("mine") {
        Icon::Minesweeper
    } else if title.contains("看图") || lower.contains("view") || lower.contains("image") {
        Icon::ImageViewer
    } else if title.contains("音乐")
        || title.contains("播放器")
        || lower.contains("music")
        || lower.contains("audio")
        || lower.contains("mp3")
    {
        Icon::Music
    } else if title.contains("浏览器") || lower.contains("browser") {
        Icon::Browser
    } else if title.contains("设置") || lower.contains("setting") {
        Icon::Settings
    } else if title.contains("帮助") || lower.contains("help") || title.contains("手册") {
        Icon::Help
    } else if title.contains("通知") || lower.contains("notify") {
        Icon::Notify
    } else if title.contains("登录") || title.contains("账户") {
        Icon::Login
    } else {
        Icon::SpaceLogo
    }
}

fn next_ime_scheme(cur: &str) -> &'static str {
    match cur {
        "full" => "xiaohe",
        "xiaohe" => "ziranma",
        "ziranma" => "microsoft",
        "microsoft" => "sogou",
        "sogou" => "abc",
        _ => "full",
    }
}

struct TaskbarLayout {
    start: Rect,
    start_sep_x: i32,
    pinned_sep_x: i32,
    win_start_x: i32,
    ime: Rect,
    net: Rect,
    volume: Rect,
    bell: Rect,
    clock_sep_x: i32,
    show_desktop: Rect,
}

impl TaskbarLayout {
    #[allow(dead_code)]
    fn compute(screen_w: i32, panel_h: i32, tray_w: i32, show_pinned: bool) -> Self {
        Self::compute_styled(screen_w, panel_h, tray_w, show_pinned, false)
    }

    fn compute_styled(
        screen_w: i32,
        panel_h: i32,
        tray_w: i32,
        show_pinned: bool,
        is_vortex: bool,
    ) -> Self {
        Self::compute_with_start(
            screen_w,
            panel_h,
            tray_w,
            show_pinned,
            is_vortex,
            None,
        )
    }

    fn compute_for_theme(
        theme: &Theme,
        screen_w: i32,
        panel_h: i32,
        tray_w: i32,
        show_pinned: bool,
    ) -> Self {
        let custom_start = theme.skin().map(|s| {
            let sb = &s.start_button;
            if sb.full_height {
                Rect::new(sb.margin_left, 0, sb.width, panel_h)
            } else {
                let bh = (panel_h - 8).clamp(24, 42);
                Rect::new(sb.margin_left.max(4), (panel_h - bh) / 2, sb.width, bh)
            }
        });
        Self::compute_with_start(
            screen_w,
            panel_h,
            tray_w,
            show_pinned,
            theme.is_vortexglass(),
            custom_start,
        )
    }

    fn compute_with_start(
        screen_w: i32,
        panel_h: i32,
        tray_w: i32,
        show_pinned: bool,
        is_vortex: bool,
        custom_start: Option<Rect>,
    ) -> Self {
        let start = if let Some(sr) = custom_start {
            sr
        } else if is_vortex {
            let orb = (panel_h - 4).clamp(32, 46);
            Rect::new(6, (panel_h - orb) / 2, orb.max(42), orb)
        } else {
            let start_bh = (panel_h - 10).clamp(26, 42);
            Rect::new(6, (panel_h - start_bh) / 2, 86, start_bh)
        };
        let start_sep_x = start.right() + 6;
        let pinned_start_x = start_sep_x + 6;
        let pinned_sep_x = pinned_start_x + (PINNED_MODES.len() as i32) * 36 + 2;
        let win_start_x = if show_pinned {
            pinned_sep_x + 8
        } else {
            start_sep_x + 8
        };
        let tray_bh = (panel_h - 12).clamp(24, 38);
        let tray_y = (panel_h - tray_bh) / 2;
        let peek_w = if is_vortex { 16 } else { 0 };
        let clock_sep_x = screen_w - tray_w - peek_w;
        let bell = Rect::new(clock_sep_x - 40, tray_y, 32, tray_bh);
        let volume = Rect::new(bell.x - 36, tray_y, 32, tray_bh);
        let net = Rect::new(volume.x - 36, tray_y, 32, tray_bh);
        let ime = Rect::new(net.x - 34, tray_y, 28, tray_bh);
        let show_desktop = if is_vortex {
            Rect::new(screen_w - 14, 0, 14, panel_h)
        } else {
            Rect::new(screen_w, 0, 0, 0)
        };
        Self {
            start,
            start_sep_x,
            pinned_sep_x,
            win_start_x,
            ime,
            net,
            volume,
            bell,
            clock_sep_x,
            show_desktop,
        }
    }

    fn pinned_rect(&self, panel_h: i32, slot: usize) -> Rect {
        let bh = (panel_h - 12).clamp(24, 38);
        Rect::new(
            self.start_sep_x + 6 + slot as i32 * 36,
            (panel_h - bh) / 2,
            32,
            bh,
        )
    }

    fn window_button_rect(&self, panel_h: i32, count: usize, index: usize) -> Rect {
        let bh = (panel_h - 12).clamp(24, 40);
        let available = (self.ime.x - 10 - self.win_start_x).max(80);
        let count = count.clamp(1, 6) as i32;
        let btn_w = ((available - (count - 1) * 4) / count).clamp(28, 160);
        Rect::new(
            self.win_start_x + index as i32 * (btn_w + 4),
            (panel_h - bh) / 2,
            btn_w,
            bh,
        )
    }
}

impl Shell {
    pub fn run() -> Result<()> {
        let startup = Instant::now();
        eprintln!("spui: shell start");
        // Map the CJK fonts before Xorg grabs DRM/VT; the kernel currently
        // deadlocks when a large file-backed mapping races Xorg startup.
        let mut text =
            TextRenderer::load().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        eprintln!("spui: fonts mapped at {} ms", startup.elapsed().as_millis());
        let _ = fs::write("/tmp/spui-loaded", b"1");
        let x = X::connect()?;
        eprintln!(
            "spui: X connected {}x{} depth={} shm={} shape={}",
            x.width, x.height, x.depth, x.has_shm, x.has_shape
        );

        let mut ux_client = UxConfClient::connect().ok();
        let config = ux_client
            .as_mut()
            .map(UxConfClient::load_desktop_config)
            .unwrap_or_default();
        let watch = UxConfWatch::subscribe(None).ok();
        let theme = Theme::from_config(&config);
        text.set_ui_scale(theme.ui_scale());
        apply_timezone(&config.timezone);

        x.set_cursor(x.root, CursorKind::Default)?;

        let root_mask = EventMask::SUBSTRUCTURE_REDIRECT
            | EventMask::SUBSTRUCTURE_NOTIFY
            | EventMask::PROPERTY_CHANGE
            | EventMask::STRUCTURE_NOTIFY;
        x.conn
            .change_window_attributes(
                x.root,
                &ChangeWindowAttributesAux::new().event_mask(root_mask),
            )?
            .check()?;

        let desktop = x.generate_id()?;
        x.create_window(
            desktop,
            x.root,
            0,
            0,
            x.width,
            x.height,
            true,
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION
                | EventMask::KEY_PRESS
                | EventMask::KEY_RELEASE,
            theme.bg_top,
        )?;
        x.set_wm_type(desktop, x.atoms.net_wm_type_desktop)?;
        x.map(desktop)?;
        x.lower(desktop)?;

        let panel_h = config.panel_size.clamp(34, 60);
        let taskbar_y = if config.panel_position == PanelPosition::Top {
            0
        } else {
            i32::from(x.height) - panel_h
        };
        let taskbar = x.generate_id()?;
        x.create_window(
            taskbar,
            x.root,
            0,
            taskbar_y as i16,
            x.width,
            panel_h as u16,
            true,
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION
                | EventMask::LEAVE_WINDOW
                | EventMask::KEY_PRESS,
            theme.panel,
        )?;
        x.set_wm_type(taskbar, x.atoms.net_wm_type_dock)?;
        x.map(taskbar)?;

        let thumb_window = x.generate_id()?;
        x.create_window(
            thumb_window,
            x.root,
            0,
            0,
            1,
            1,
            true,
            EventMask::ENTER_WINDOW
                | EventMask::LEAVE_WINDOW
                | EventMask::POINTER_MOTION
                | EventMask::BUTTON_PRESS,
            theme.panel,
        )?;
        x.set_wm_type(thumb_window, x.atoms.net_wm_type_dialog)?;

        let (base_mw, base_mh) = if theme.is_vortexglass() {
            (VORTEX_MENU_WIDTH, VORTEX_MENU_HEIGHT)
        } else {
            (MENU_WIDTH, MENU_HEIGHT)
        };
        let popup_menu_y = if config.panel_position == PanelPosition::Top {
            panel_h + 6
        } else {
            (taskbar_y - base_mh - 6).max(8)
        };
        let (menu_x, menu_y, menu_w, menu_h) = if config.panel_start_fullscreen {
            (0i16, 0i16, x.width, x.height)
        } else {
            (6i16, popup_menu_y as i16, base_mw as u16, base_mh as u16)
        };
        let menu = x.generate_id()?;
        x.create_window(
            menu,
            x.root,
            menu_x,
            menu_y,
            menu_w,
            menu_h,
            true,
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION
                | EventMask::LEAVE_WINDOW
                | EventMask::KEY_PRESS
                | EventMask::KEY_RELEASE
                | EventMask::FOCUS_CHANGE
                | EventMask::STRUCTURE_NOTIFY,
            theme.panel,
        )?;
        x.set_wm_type(menu, x.atoms.net_wm_type_dialog)?;
        if !config.panel_start_fullscreen {
            x.set_rounded_shape(
                menu,
                base_mw as u16,
                base_mh as u16,
                theme.radius_popup(),
            )?;
        }

        // Bind global Start Screen, Alt+Tab, and Win+Tab Flip 3D shortcuts.
        x.grab_global_key(0xffeb, ModMask::from(0u16));
        x.grab_global_key(0xffec, ModMask::from(0u16));
        x.grab_global_key(0xff1b, ModMask::CONTROL);
        x.grab_global_key(0xff09, ModMask::M1);
        x.grab_global_key(0xff09, ModMask::M1 | ModMask::SHIFT);
        x.grab_global_key(0xff09, ModMask::M4);
        x.grab_global_key(0xff09, ModMask::M4 | ModMask::SHIFT);

        let ctx_menu = x.generate_id()?;
        x.create_window(
            ctx_menu,
            x.root,
            0,
            0,
            CTX_WIDTH as u16,
            CTX_HEIGHT as u16,
            true,
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::POINTER_MOTION
                | EventMask::LEAVE_WINDOW
                | EventMask::KEY_PRESS
                | EventMask::FOCUS_CHANGE
                | EventMask::STRUCTURE_NOTIFY,
            theme.panel,
        )?;
        x.set_wm_type(ctx_menu, x.atoms.net_wm_type_dialog)?;
        x.set_rounded_shape(
            ctx_menu,
            CTX_WIDTH as u16,
            CTX_HEIGHT as u16,
            theme.radius_popup(),
        )?;

        let vol_popup = x.generate_id()?;
        x.create_window(
            vol_popup,
            x.root,
            0,
            0,
            VOL_POPUP_WIDTH as u16,
            VOL_POPUP_HEIGHT as u16,
            true,
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION
                | EventMask::LEAVE_WINDOW
                | EventMask::KEY_PRESS
                | EventMask::FOCUS_CHANGE
                | EventMask::STRUCTURE_NOTIFY,
            theme.panel,
        )?;
        x.set_wm_type(vol_popup, x.atoms.net_wm_type_dialog)?;
        x.set_rounded_shape(
            vol_popup,
            VOL_POPUP_WIDTH as u16,
            VOL_POPUP_HEIGHT as u16,
            theme.radius_popup(),
        )?;

        let toast_y = if config.panel_position == PanelPosition::Top {
            panel_h + 12
        } else {
            12
        };
        let toast_window = x.generate_id()?;
        x.create_window(
            toast_window,
            x.root,
            (i32::from(x.width) - TOAST_WIDTH - 12) as i16,
            toast_y as i16,
            TOAST_WIDTH as u16,
            TOAST_HEIGHT as u16,
            true,
            EventMask::EXPOSURE | EventMask::BUTTON_PRESS | EventMask::POINTER_MOTION,
            theme.panel,
        )?;
        x.set_wm_type(toast_window, x.atoms.net_wm_type_dialog)?;
        x.set_rounded_shape(
            toast_window,
            TOAST_WIDTH as u16,
            TOAST_HEIGHT as u16,
            theme.radius_popup(),
        )?;

        let desktop_canvas = x.create_canvas(x.width as usize, x.height as usize, theme.bg_top);
        let taskbar_canvas = x.create_canvas(x.width as usize, panel_h as usize, theme.panel);
        let menu_canvas = x.create_canvas(menu_w as usize, menu_h as usize, theme.panel);
        let ctx_canvas = x.create_canvas(CTX_WIDTH as usize, CTX_HEIGHT as usize, theme.panel);
        let vol_canvas = x.create_canvas(
            VOL_POPUP_WIDTH as usize,
            VOL_POPUP_HEIGHT as usize,
            theme.panel,
        );
        let toast_canvas =
            x.create_canvas(TOAST_WIDTH as usize, TOAST_HEIGHT as usize, theme.panel);

        let net_fd = crate::netmgr::open_event_socket();
        let net_status = crate::netmgr::query_status();
        let vol_persist = VolumePersistState::new(SoundSnapshot::from_config(&config));
        let comp = Compositor::new(&x, &theme);
        eprintln!(
            "spui: compositor active={} (composite={} damage={} xfixes={})",
            comp.active, x.has_composite, x.has_damage, x.has_xfixes
        );

        let ime_chinese = x.get_spime_status().map(|(c, _)| c).unwrap_or(true);
        let ime_active = config.ime_enabled;
        let mut shell = Self {
            x,
            text,
            config,
            ux_client,
            watch,
            theme,
            comp,
            comps: HashMap::new(),
            z_stack: Vec::new(),
            override_wins: Vec::new(),
            wallpaper_raw: None,
            wallpaper_scaled: Vec::new(),
            wallpaper_file: String::new(),
            desktop,
            desktop_canvas,
            desktop_bg: 0,
            selected_desktop_icon: None,
            taskbar,
            taskbar_canvas,
            thumb_window,
            menu,
            menu_canvas,
            menu_open: false,
            menu_query: String::new(),
            menu_ime_preedit: String::new(),
            menu_ime_focused: false,
            menu_ime_spot: None,
            menu_confirm_power: None,
            start_screen: StartScreenState::new(),
            ctx_menu,
            ctx_canvas,
            ctx_open: false,
            ctx_is_ime: false,
            ctx_pos: (0, 0),
            ime_client: SpimeClient::new(menu),
            ime_chinese,
            ime_active,
            comp_shm_pending_sync: false,
            vol_popup,
            vol_canvas,
            vol_open: false,
            vol_dragging: false,
            vol_persist,
            pointer_grab: None,
            kbd_grabbed: false,
            wm_drag: None,
            toast_window,
            toast_canvas,
            toast: None,
            windows: Vec::new(),
            active: None,
            all_minimized_backup: Vec::new(),
            hover: Hit::None,
            clock_tick: (-1, -1, -1),
            net_fd,
            net_status,
            last_net_refresh: Instant::now(),
            last_launch: None,
            last_composite: Instant::now() - Duration::from_millis(32),
            theme_stamp: crate::theme::theme_reload_stamp(),
            dirty_rects_buf: Vec::with_capacity(16),
            comp_plan_buf: Vec::with_capacity(8),
            capture_keep_buf: Vec::with_capacity(16),
            running: true,
        };
        shell.sync_argb_canvases();
        shell.claim_wm()?;
        shell.sync_initial_override_windows();
        shell.apply_x11_settings(None);
        crate::sound::apply_mixer_volume(
            shell.config.sound_master_volume,
            shell.config.sound_muted,
        );
        shell.reload_wallpaper(true);
        shell.draw_desktop();
        shell.present_desktop()?;
        shell.update_clock();
        shell.draw_taskbar();
        shell.present_taskbar()?;
        shell.composite_frame()?;
        crate::sound::play(&shell.config, crate::sound::SystemSound::Startup);
        eprintln!(
            "spui: desktop ready at {} ms",
            startup.elapsed().as_millis()
        );
        shell.event_loop()
    }

    fn menu_dims(&self) -> (i32, i32) {
        if self.config.panel_start_fullscreen {
            (i32::from(self.x.width), i32::from(self.x.height))
        } else if let Some(ref sm) = self.theme.skin().and_then(|s| s.start_menu.as_ref()) {
            (sm.width, sm.height)
        } else if self.theme.is_vortexglass() {
            (VORTEX_MENU_WIDTH, VORTEX_MENU_HEIGHT)
        } else {
            (MENU_WIDTH, MENU_HEIGHT)
        }
    }

    fn menu_x(&self) -> i32 {
        if self.config.panel_start_fullscreen {
            0
        } else if let Some(ref sm) = self.theme.skin().and_then(|s| s.start_menu.as_ref()) {
            sm.offset_x
        } else {
            6
        }
    }

    fn sync_argb_canvases(&mut self) {
        let argb = self.comp.active && self.theme.is_vortexglass();
        self.taskbar_canvas.set_argb(argb);
        self.menu_canvas
            .set_argb(argb && !self.config.panel_start_fullscreen);
        self.ctx_canvas.set_argb(argb);
        self.vol_canvas.set_argb(argb);
        self.toast_canvas.set_argb(argb);
    }

    fn panel_height(&self) -> i32 {
        self.theme.panel_height(&self.config)
    }

    fn taskbar_y(&self) -> i32 {
        if self.config.panel_position == PanelPosition::Top {
            0
        } else {
            i32::from(self.x.height) - self.panel_height()
        }
    }

    fn menu_y(&self) -> i32 {
        let (_, mh) = self.menu_dims();
        let gap = self
            .theme
            .skin()
            .and_then(|s| s.start_menu.as_ref())
            .map_or(6, |sm| sm.gap_y);
        if self.config.panel_position == PanelPosition::Top {
            self.panel_height() + gap
        } else {
            (self.taskbar_y() - mh - gap).max(0)
        }
    }

    fn sync_menu_window(&mut self) -> Result<()> {
        let (mx, my, mw, mh, rounded) = if self.config.panel_start_fullscreen {
            (0i16, 0i16, self.x.width, self.x.height, false)
        } else {
            let (vw, vh) = self.menu_dims();
            (
                self.menu_x() as i16,
                self.menu_y() as i16,
                vw as u16,
                vh as u16,
                true,
            )
        };
        self.x.move_resize(self.menu, mx, my, mw, mh)?;
        if rounded {
            self.x
                .set_rounded_shape(self.menu, mw, mh, self.theme.radius_popup())?;
        } else {
            self.x.clear_shape(self.menu)?;
        }
        if self.menu_canvas.width != mw as usize || self.menu_canvas.height != mh as usize {
            self.x.release_canvas(&self.menu_canvas);
            self.menu_canvas = self
                .x
                .create_canvas(mw as usize, mh as usize, self.theme.panel);
            self.start_screen.invalidate_backdrop();
        }
        self.sync_argb_canvases();
        Ok(())
    }

    fn handle_start_action(
        &mut self,
        action: StartAction,
        prev_cfg: Option<DesktopConfig>,
    ) -> Result<()> {
        let cfg_updated = if let Some(prev) = prev_cfg {
            self.apply_live_config_diff(prev)?;
            true
        } else {
            false
        };
        match action {
            StartAction::None => {}
            StartAction::Redraw => {
                if !cfg_updated && self.menu_open {
                    self.draw_menu();
                    self.present_menu()?;
                }
            }
            StartAction::Close => {
                self.close_menu()?;
            }
            StartAction::Launch(mode) => {
                self.close_menu()?;
                self.launch(mode);
            }
            StartAction::Logout => {
                self.close_menu()?;
                // DDM owns the session. Exiting the shell tears down the
                // session cleanly and returns to the greeter.
                std::process::exit(0);
            }
            StartAction::Power(action) => {
                self.execute_power_action(action)?;
            }
            StartAction::ToggleMute => {
                self.toggle_mute()?;
                if self.menu_open {
                    self.draw_menu();
                    self.present_menu()?;
                }
            }
        }
        Ok(())
    }

    fn execute_power_action(&mut self, action: PowerAction) -> Result<()> {
        if let Err(err) = crate::apps::session_ui::send_power_request(action) {
            self.close_menu()?;
            let label = match action {
                PowerAction::Reboot => "重启失败",
                PowerAction::PowerOff => "关机失败",
            };
            self.show_toast(label, &err.to_string());
        }
        Ok(())
    }

    fn toast_y(&self) -> i32 {
        if self.config.panel_position == PanelPosition::Top {
            self.panel_height() + 12
        } else {
            12
        }
    }

    fn tray_width(&self) -> i32 {
        if self.config.clock_seconds || !self.config.clock_24h {
            136
        } else {
            108
        }
    }

    fn desktop_icon_rect(&self, index: usize) -> Rect {
        let icon_sz = self.config.desktop_icon_size.clamp(32, 48);
        let top_base = if self.config.panel_position == PanelPosition::Top {
            self.panel_height() + 12
        } else {
            14
        };
        let bottom_limit = if self.config.panel_position == PanelPosition::Bottom {
            i32::from(self.x.height) - self.panel_height() - 14
        } else {
            i32::from(self.x.height) - 14
        };
        let cell_w = (icon_sz + 44).max(84);
        let cell_h = icon_sz + 42;
        let spacing = 6;
        let per_col = ((bottom_limit - top_base) / (cell_h + spacing)).max(1) as usize;
        let col = index / per_col;
        let row = index % per_col;
        Rect::new(
            14 + col as i32 * (cell_w + 10),
            top_base + row as i32 * (cell_h + spacing),
            cell_w,
            cell_h,
        )
    }

    fn taskbar_layout(&self) -> TaskbarLayout {
        TaskbarLayout::compute_for_theme(
            &self.theme,
            i32::from(self.x.width),
            self.panel_height(),
            self.tray_width(),
            self.config.panel_show_pinned,
        )
    }

    fn show_desktop_rect(&self) -> Rect {
        self.taskbar_layout().show_desktop
    }

    fn start_button_rect(&self) -> Rect {
        self.taskbar_layout().start
    }

    fn pinned_rect(&self, index: usize) -> Rect {
        self.taskbar_layout()
            .pinned_rect(self.panel_height(), index)
    }

    fn apply_x11_settings(&self, prev: Option<&DesktopConfig>) {
        if prev.is_none_or(|p| p.mouse_left_handed != self.config.mouse_left_handed) {
            self.x.apply_pointer_mapping(self.config.mouse_left_handed);
        }
        if prev.is_none_or(|p| p.mouse_speed != self.config.mouse_speed) {
            self.x.apply_pointer_speed(self.config.mouse_speed);
        }
        if prev.is_none_or(|p| {
            p.key_repeat_enabled != self.config.key_repeat_enabled
                || p.key_repeat_delay_ms != self.config.key_repeat_delay_ms
                || p.key_repeat_rate_hz != self.config.key_repeat_rate_hz
        }) {
            self.x.apply_keyboard_repeat(
                self.config.key_repeat_enabled,
                self.config.key_repeat_delay_ms,
                self.config.key_repeat_rate_hz,
            );
        }
        if prev.is_none_or(|p| p.display_blank_timeout_sec != self.config.display_blank_timeout_sec)
        {
            self.x
                .apply_screensaver_timeout(self.config.display_blank_timeout_sec);
        }
        let current_mode = format!("{}x{}", self.x.width, self.x.height);
        let res_changed = match prev {
            Some(p) => p.display_resolution != self.config.display_resolution,
            None => self.config.display_resolution != current_mode,
        };
        if res_changed {
            apply_display_mode(&self.config.display_resolution);
        }
    }

    fn apply_live_config_diff(&mut self, prev: DesktopConfig) -> Result<()> {
        self.theme = Theme::from_config(&self.config);
        self.text.set_ui_scale(self.theme.ui_scale());

        if prev.timezone != self.config.timezone {
            apply_timezone(&self.config.timezone);
            self.clock_tick = (-1, -1, -1);
        }

        self.apply_x11_settings(Some(&prev));

        let prev_panel_h = Theme::from_config(&prev).panel_height(&prev);
        let ph = self.panel_height();
        let panel_geom_changed = prev.panel_position != self.config.panel_position
            || prev.panel_size != self.config.panel_size
            || prev_panel_h != ph;
        let theme_style_changed = prev.theme_style != self.config.theme_style;
        if theme_style_changed {
            let radius = self.theme.radius_popup();
            self.x
                .set_rounded_shape(self.ctx_menu, CTX_WIDTH as u16, CTX_HEIGHT as u16, radius)?;
            self.x.set_rounded_shape(
                self.vol_popup,
                VOL_POPUP_WIDTH as u16,
                VOL_POPUP_HEIGHT as u16,
                radius,
            )?;
            self.x.set_rounded_shape(
                self.toast_window,
                TOAST_WIDTH as u16,
                TOAST_HEIGHT as u16,
                radius,
            )?;
        }
        if panel_geom_changed {
            self.x.move_resize(
                self.taskbar,
                0,
                self.taskbar_y() as i16,
                self.x.width,
                ph as u16,
            )?;
            self.x.move_resize(
                self.toast_window,
                (i32::from(self.x.width) - TOAST_WIDTH - 12) as i16,
                self.toast_y() as i16,
                TOAST_WIDTH as u16,
                TOAST_HEIGHT as u16,
            )?;
            if self.taskbar_canvas.height != ph as usize {
                self.x.release_canvas(&self.taskbar_canvas);
                self.taskbar_canvas =
                    self.x
                        .create_canvas(self.x.width as usize, ph as usize, self.theme.panel);
            }
        }
        if panel_geom_changed
            || theme_style_changed
            || prev.panel_start_fullscreen != self.config.panel_start_fullscreen
        {
            self.sync_menu_window()?;
            if self.menu_open && prev.panel_start_fullscreen != self.config.panel_start_fullscreen {
                if self.config.panel_start_fullscreen {
                    if self.pointer_grab == Some(self.menu) {
                        self.release_popup_grab();
                    }
                } else {
                    let _ = self.acquire_popup_grab(self.menu);
                }
            }
        }

        let wp_file_changed = prev.wallpaper != self.config.wallpaper;
        let backdrop_changed = wp_file_changed
            || prev.backdrop_style != self.config.backdrop_style
            || prev.solid_color != self.config.solid_color;
        if backdrop_changed {
            self.reload_wallpaper(wp_file_changed);
        }
        let desktop_redraw_needed = backdrop_changed
            || theme_style_changed
            || panel_geom_changed
            || prev.mode != self.config.mode
            || prev.accent != self.config.accent
            || prev.font_scale != self.config.font_scale
            || prev.desktop_icons_visible != self.config.desktop_icons_visible
            || prev.desktop_icon_size != self.config.desktop_icon_size;
        {
            let host = ShellConfigHost::new(&self.x, &mut self.config, &mut self.ux_client);
            self.start_screen.on_config_changed(&host);
        }

        if desktop_redraw_needed {
            self.sync_argb_canvases();
            self.draw_desktop();
            self.present_desktop()?;
        }
        self.sync_argb_canvases();
        self.draw_taskbar();
        self.present_taskbar()?;
        if self.menu_open {
            self.draw_menu();
            self.present_menu()?;
        }
        if self.ctx_open {
            self.draw_ctx();
            self.present_ctx()?;
        }
        if prev.sound_master_volume != self.config.sound_master_volume
            || prev.sound_muted != self.config.sound_muted
        {
            crate::sound::apply_mixer_volume(
                self.config.sound_master_volume,
                self.config.sound_muted,
            );
        }
        if self.vol_open {
            if panel_geom_changed {
                let (vx, vy) = self.vol_popup_pos();
                self.x.move_resize(
                    self.vol_popup,
                    vx,
                    vy,
                    VOL_POPUP_WIDTH as u16,
                    VOL_POPUP_HEIGHT as u16,
                )?;
            }
            self.draw_vol_popup();
            self.present_vol_popup()?;
        }
        if prev.wm_compositor_quality != self.config.wm_compositor_quality
            || theme_style_changed
            || desktop_redraw_needed
        {
            self.comp.invalidate_desktop_blur();
        }
        self.comp.mark_full_dirty();
        self.composite_frame()?;
        if self.toast.is_some() {
            self.draw_toast();
            self.present_toast()?;
        }
        if desktop_redraw_needed
            || panel_geom_changed
            || self.menu_open
            || self.ctx_open
            || self.vol_open
        {
            self.x.raise(self.taskbar)?;
            if self.menu_open {
                self.x.raise(self.menu)?;
            }
            if self.ctx_open {
                self.x.raise(self.ctx_menu)?;
            }
            if self.vol_open {
                self.x.raise(self.vol_popup)?;
            }
        }
        Ok(())
    }

    fn reload_wallpaper(&mut self, force_decode: bool) {
        if self.config.backdrop_style == BackdropStyle::Solid {
            self.wallpaper_scaled.clear();
            return;
        }
        if force_decode
            || self.wallpaper_raw.is_none()
            || self.wallpaper_file != self.config.wallpaper
        {
            self.wallpaper_file = self.config.wallpaper.clone();
            let path = self.config.wallpaper_path();
            self.wallpaper_raw = icons::decode_jpeg(Path::new(&path));
        }
        let w = self.x.width as usize;
        let h = self.x.height as usize;
        if let Some(raw) = &self.wallpaper_raw {
            self.wallpaper_scaled = match self.config.backdrop_style {
                BackdropStyle::Cover => raw.scale_cover_opaque(w, h),
                BackdropStyle::Contain => raw.scale_contain_opaque(w, h, self.config.solid_color),
                BackdropStyle::Stretch => raw.scale_stretch_opaque(w, h),
                BackdropStyle::Solid => Vec::new(),
            };
        } else {
            self.wallpaper_scaled.clear();
        }
    }

    fn claim_wm(&self) -> Result<()> {
        self.x.conn.change_property(
            PropMode::REPLACE,
            self.x.root,
            self.x.atoms.net_supporting_wm_check,
            AtomEnum::WINDOW,
            32,
            1,
            &self.taskbar.to_ne_bytes(),
        )?;
        self.x.conn.change_property(
            PropMode::REPLACE,
            self.taskbar,
            self.x.atoms.net_supporting_wm_check,
            AtomEnum::WINDOW,
            32,
            1,
            &self.taskbar.to_ne_bytes(),
        )?;
        let supported = [
            self.x.atoms.net_wm_name,
            self.x.atoms.net_wm_type,
            self.x.atoms.net_client_list,
            self.x.atoms.net_active_window,
            self.x.atoms.net_wm_state,
            self.x.atoms.net_wm_state_hidden,
            self.x.atoms.net_supporting_wm_check,
        ];
        let mut bytes = Vec::with_capacity(supported.len() * 4);
        for atom in supported {
            bytes.extend_from_slice(&atom.to_ne_bytes());
        }
        self.x.conn.change_property(
            PropMode::REPLACE,
            self.x.root,
            self.x.atoms.net_supported,
            AtomEnum::ATOM,
            32,
            supported.len() as u32,
            &bytes,
        )?;
        self.x.flush()?;
        Ok(())
    }

    fn update_clock(&mut self) -> bool {
        let (_, _, _, hour, minute, sec, _) = now_local();
        let stamp = (
            hour,
            minute,
            if self.config.clock_seconds { sec } else { -1 },
        );
        if stamp != self.clock_tick {
            self.clock_tick = stamp;
            true
        } else {
            false
        }
    }

    fn reload_theme_pack(&mut self) -> Result<()> {
        let prev = self.config.clone();
        if let Some(missing) = crate::theme::fallback_missing_theme(&mut self.config) {
            eprintln!(
                "theme '{missing}' is no longer installed; falling back to the default theme"
            );
        }
        if self.config != prev {
            self.apply_live_config_diff(prev)?;
        } else {
            self.theme = Theme::from_config(&self.config);
            self.reload_wallpaper(true);
            self.comp.mark_full_dirty();
        }
        Ok(())
    }

    fn poll_uxconf(&mut self) -> Result<()> {
        let prev = self.config.clone();
        let (mut changed, events) = self
            .watch
            .as_mut()
            .map_or((false, Vec::new()), |w| w.poll_into(&mut self.config));
        if !events.is_empty() {
            if self.vol_dragging || self.vol_persist.is_dirty() {
                SoundSnapshot::from_config(&prev).apply_to_config(&mut self.config);
                changed = self.config != prev;
            } else {
                self.vol_persist
                    .sync_from_external(SoundSnapshot::from_config(&self.config));
            }
        }
        if changed {
            self.apply_live_config_diff(prev)?;
        }
        Ok(())
    }

    fn refresh_net_status(&mut self) -> bool {
        let next = crate::netmgr::query_status();
        self.last_net_refresh = Instant::now();
        let visual_changed = next.panel_label() != self.net_status.panel_label()
            || next.connected() != self.net_status.connected()
            || next.up != self.net_status.up
            || next.has_device != self.net_status.has_device;
        self.net_status = next;
        visual_changed
    }

    fn event_loop(&mut self) -> Result<()> {
        const FRAME_BUDGET: Duration = Duration::from_millis(16);
        let mut comp_animating = false;
        while self.running {
            let reload_stamp = crate::theme::theme_reload_stamp();
            if reload_stamp != self.theme_stamp {
                self.theme_stamp = reload_stamp;
                self.reload_theme_pack()?;
            }
            self.poll_uxconf()?;

            let mut net_dirty = false;
            let mut net_event = false;
            if let Some(fd) = self.net_fd {
                if crate::netmgr::drain_events(fd) {
                    net_event = true;
                    net_dirty |= self.refresh_net_status();
                }
            }

            let mut raw = Vec::new();
            while let Some(event) = self.x.poll()? {
                raw.push(event);
            }
            let was_animating = ui::has_active_animations() || comp_animating;
            if raw.is_empty() {
                let ux_fd = self.watch.as_ref().map(UxConfWatch::as_raw_fd);
                let mut timeout = if was_animating {
                    16
                } else if self.config.clock_seconds {
                    200
                } else {
                    500
                };
                if !self.vol_dragging {
                    if let Some(persist_ms) = self.vol_persist.timeout_ms_until(Instant::now()) {
                        timeout = timeout.min(persist_ms);
                    }
                }
                if self.comp.thumb_hover_start.is_some() || self.comp.thumb_leave_at.is_some() {
                    timeout = timeout.min(25);
                }
                if self.comp.active {
                    let now_idle = Instant::now();
                    if self.comp.is_dirty() || self.wm_drag.as_ref().is_some_and(|d| d.has_pending) {
                        let budget = if self.wm_drag.is_some() {
                            Duration::from_millis(12)
                        } else {
                            FRAME_BUDGET
                        };
                        let rem = budget
                            .saturating_sub(now_idle.duration_since(self.last_composite))
                            .as_millis()
                            .clamp(1, 16) as i32;
                        timeout = timeout.min(rem);
                    }
                    for cw in self.comps.values() {
                        if let Some(dl) = cw.resize_wait_until {
                            let rem = dl.saturating_duration_since(now_idle).as_millis().clamp(1, 85) as i32;
                            timeout = timeout.min(rem);
                        }
                    }
                }
                let (_, ux_ready, net_ready) = self.x.wait_fds(ux_fd, self.net_fd, timeout);
                if ux_ready {
                    self.poll_uxconf()?;
                }
                if net_ready {
                    if let Some(fd) = self.net_fd {
                        let _ = crate::netmgr::drain_events(fd);
                    }
                    net_event = true;
                    net_dirty |= self.refresh_net_status();
                }
                while let Some(event) = self.x.poll()? {
                    raw.push(event);
                }
            }

            if self.last_net_refresh.elapsed() >= Duration::from_secs(3) {
                net_dirty |= self.refresh_net_status();
            }

            // Coalesce burst MotionNotify, ConfigureNotify & geometry ConfigureRequest events cleanly.
            let mut events: Vec<Event> = Vec::with_capacity(raw.len());
            for ev in raw {
                match &ev {
                    Event::MotionNotify(curr) => {
                        if let Some(slot) = events
                            .iter_mut()
                            .rev()
                            .take_while(|p| {
                                matches!(
                                    p,
                                    Event::MotionNotify(_)
                                        | Event::ConfigureNotify(_)
                                        | Event::DamageNotify(_)
                                        | Event::ConfigureRequest(_)
                                )
                            })
                            .find_map(|p| match p {
                                Event::MotionNotify(prev) if prev.event == curr.event => Some(p),
                                _ => None,
                            })
                        {
                            *slot = ev;
                            continue;
                        }
                    }
                    Event::ConfigureNotify(curr) => {
                        if let Some(slot) = events
                            .iter_mut()
                            .rev()
                            .take_while(|p| {
                                matches!(
                                    p,
                                    Event::MotionNotify(_)
                                        | Event::ConfigureNotify(_)
                                        | Event::DamageNotify(_)
                                        | Event::ConfigureRequest(_)
                                )
                            })
                            .find_map(|p| match p {
                                Event::ConfigureNotify(prev) if prev.window == curr.window => {
                                    Some(p)
                                }
                                _ => None,
                            })
                        {
                            *slot = ev;
                            continue;
                        }
                    }
                    Event::ConfigureRequest(curr)
                        if u16::from(curr.value_mask) & u16::from(ConfigWindow::STACK_MODE) == 0 =>
                    {
                        if let Some(prev) = events
                            .iter_mut()
                            .rev()
                            .take_while(|p| match p {
                                Event::MotionNotify(_)
                                | Event::ConfigureNotify(_)
                                | Event::DamageNotify(_) => true,
                                Event::ConfigureRequest(cr) => {
                                    u16::from(cr.value_mask) & u16::from(ConfigWindow::STACK_MODE)
                                        == 0
                                }
                                _ => false,
                            })
                            .find_map(|p| match p {
                                Event::ConfigureRequest(prev) if prev.window == curr.window => {
                                    Some(prev)
                                }
                                _ => None,
                            })
                        {
                            let cm = u16::from(curr.value_mask);
                            if cm & u16::from(ConfigWindow::X) != 0 {
                                prev.x = curr.x;
                            }
                            if cm & u16::from(ConfigWindow::Y) != 0 {
                                prev.y = curr.y;
                            }
                            if cm & u16::from(ConfigWindow::WIDTH) != 0 {
                                prev.width = curr.width;
                            }
                            if cm & u16::from(ConfigWindow::HEIGHT) != 0 {
                                prev.height = curr.height;
                            }
                            prev.value_mask = (u16::from(prev.value_mask) | cm).into();
                            continue;
                        }
                    }
                    _ => {}
                }
                events.push(ev);
            }

            for event in events {
                self.handle(event)?;
            }

            let now = Instant::now();
            // Trigger taskbar live thumbnail card after 90ms dwell over a window button.
            if self.comp.active && self.comp.thumb.is_none() {
                if let Some((idx, ts)) = self.comp.thumb_hover_start {
                    if now.duration_since(ts).as_millis() >= 90
                        && !self.menu_open
                        && !self.vol_open
                        && !self.ctx_open
                    {
                        self.open_thumb(idx)?;
                    }
                }
            }
            if self.comp.active {
                let should_close_thumb = self.comp.thumb.as_ref().is_some_and(|t| {
                    !t.pointer_inside
                        && self
                            .comp
                            .thumb_leave_at
                            .is_some_and(|ts| now.duration_since(ts).as_millis() >= 240)
                });
                if should_close_thumb {
                    self.close_thumb()?;
                }
            }

            let start_net = if net_event {
                self.start_screen.on_net_event()
            } else {
                false
            };
            let ui_anim = ui::has_active_animations();
            if self.update_clock() || ui_anim || net_dirty {
                self.draw_taskbar();
                self.present_taskbar()?;
            }
            if self.vol_open && ui_anim {
                self.draw_vol_popup();
                self.present_vol_popup()?;
            }
            if self.menu_open {
                if self.config.panel_start_fullscreen {
                    let start_tick = self.start_screen.tick(&self.config);
                    if start_tick || start_net || ui_anim || net_dirty {
                        self.draw_menu();
                        self.present_menu()?;
                    }
                } else if ui_anim {
                    self.draw_menu();
                    self.present_menu()?;
                }
            }
            if let Some(toast) = &self.toast {
                if now >= toast.deadline {
                    self.hide_toast()?;
                }
            }
            if !self.vol_dragging && self.vol_persist.is_due(now) {
                self.flush_volume_persist();
            }

            if self.comp.active {
                let pre_anim_dirty = self.active_anim_dirty_bounds();
                let mut win_anims: Vec<Option<WindowAnim>> = self
                    .z_stack
                    .iter()
                    .map(|w| self.comps.get(w).and_then(|c| c.anim))
                    .collect();
                let start_orb_hov = self.hover == Hit::Start;
                let tick_out = self.comp.tick(
                    now,
                    self.menu_open,
                    self.ctx_open,
                    self.vol_open,
                    self.toast.is_some(),
                    start_orb_hov,
                    &mut win_anims,
                );
                comp_animating = tick_out.animating;
                for (&w, anim) in self.z_stack.iter().zip(win_anims.into_iter()) {
                    if let Some(cw) = self.comps.get_mut(&w) {
                        cw.anim = anim;
                    }
                }
                if tick_out.orb_changed {
                    self.draw_taskbar();
                    let _ = self.present_taskbar();
                }
                self.mark_tick_dirty(tick_out, pre_anim_dirty);
                let frame_elapsed = now.duration_since(self.last_composite);
                let drag_due = self.wm_drag.is_some() && frame_elapsed >= Duration::from_millis(12);
                let non_drag_due = self.wm_drag.is_none()
                    && (frame_elapsed >= Duration::from_millis(14) || tick_out.peek_or_flip_changed);
                if drag_due {
                    self.apply_pending_wm_drag()?;
                    self.composite_frame()?;
                } else if non_drag_due {
                    self.composite_frame()?;
                }
            } else {
                if self.wm_drag.as_ref().is_some_and(|d| d.has_pending) {
                    self.apply_pending_wm_drag()?;
                }
                comp_animating = false;
            }
            self.x.flush()?;
        }
        Ok(())
    }

    fn handle_screen_resize(&mut self, new_w: u16, new_h: u16) -> Result<()> {
        if new_w == 0 || new_h == 0 || (new_w == self.x.width && new_h == self.x.height) {
            return Ok(());
        }
        self.x.width = new_w;
        self.x.height = new_h;
        let ph = self.panel_height();

        self.x.move_resize(self.desktop, 0, 0, new_w, new_h)?;
        self.x
            .move_resize(self.taskbar, 0, self.taskbar_y() as i16, new_w, ph as u16)?;
        self.sync_menu_window()?;
        self.x.move_resize(
            self.toast_window,
            (i32::from(new_w) - TOAST_WIDTH - 12) as i16,
            self.toast_y() as i16,
            TOAST_WIDTH as u16,
            TOAST_HEIGHT as u16,
        )?;

        if self.desktop_bg != 0 {
            let _ = self.x.conn.free_pixmap(self.desktop_bg);
            self.desktop_bg = 0;
        }
        self.x.release_canvas(&self.desktop_canvas);
        self.x.release_canvas(&self.taskbar_canvas);
        self.desktop_canvas =
            self.x
                .create_canvas(new_w as usize, new_h as usize, self.theme.bg_top);
        self.taskbar_canvas = self
            .x
            .create_canvas(new_w as usize, ph as usize, self.theme.panel);
        self.comp.resize(&self.x, new_w, new_h, self.theme.bg_top);
        self.sync_argb_canvases();

        self.reload_wallpaper(false);
        {
            let host = ShellConfigHost::new(&self.x, &mut self.config, &mut self.ux_client);
            self.start_screen.on_config_changed(&host);
        }
        self.draw_desktop();
        self.present_desktop()?;
        self.draw_taskbar();
        self.present_taskbar()?;
        if self.menu_open {
            self.draw_menu();
            self.present_menu()?;
        }
        if self.vol_open {
            let (vx, vy) = self.vol_popup_pos();
            self.x.move_resize(
                self.vol_popup,
                vx,
                vy,
                VOL_POPUP_WIDTH as u16,
                VOL_POPUP_HEIGHT as u16,
            )?;
            self.draw_vol_popup();
            self.present_vol_popup()?;
        }
        self.raise_chrome_above_clients()?;
        self.composite_frame()?;
        Ok(())
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::ConfigureNotify(e) => {
                if e.window == self.x.root {
                    self.handle_screen_resize(e.width, e.height)?;
                } else {
                    let scr_w = i32::from(self.x.width);
                    let work_h = i32::from(self.x.height) - self.panel_height();
                    let is_vortex = self.theme.is_vortexglass();
                    let mut dirty_move = None;
                    if let Some(managed) = self.windows.iter_mut().find(|w| w.window == e.window) {
                        let old_rect = Rect::new(
                            i32::from(managed.x),
                            i32::from(managed.y),
                            i32::from(managed.width),
                            i32::from(managed.height),
                        );
                        let size_changed = managed.width != e.width || managed.height != e.height;
                        managed.x = e.x;
                        managed.y = e.y;
                        managed.width = e.width;
                        managed.height = e.height;
                        let new_rect = Rect::new(
                            i32::from(e.x),
                            i32::from(e.y),
                            i32::from(e.width),
                            i32::from(e.height),
                        );
                        if let Some(cw) = self.comps.get_mut(&e.window) {
                            if size_changed
                                || cw.pixmap_w != usize::from(e.width)
                                || cw.pixmap_h != usize::from(e.height)
                            {
                                let depth = cw.depth;
                                attach_window_pixmap(&self.x, cw, e.width, e.height, depth);
                            }
                        }
                        if self.comp.active {
                            let old_max = old_rect.w >= scr_w - 2 && old_rect.h >= work_h - 2;
                            let new_max = new_rect.w >= scr_w - 2 && new_rect.h >= work_h - 2;
                            if old_rect != new_rect {
                                dirty_move = Some(
                                    window_damage_bounds(old_rect, is_vortex, old_max)
                                        .union(window_damage_bounds(new_rect, is_vortex, new_max)),
                                );
                            }
                        }
                    } else if let Some(ov) =
                        self.override_wins.iter_mut().find(|o| o.window == e.window)
                    {
                        let old_rect = ov.rect;
                        let new_rect = Rect::new(
                            i32::from(e.x),
                            i32::from(e.y),
                            i32::from(e.width),
                            i32::from(e.height),
                        );
                        let size_changed = ov.cw.pixmap_w != usize::from(e.width)
                            || ov.cw.pixmap_h != usize::from(e.height);
                        ov.rect = new_rect;
                        if size_changed && e.width > 0 && e.height > 0 {
                            let depth = ov.cw.depth;
                            attach_window_pixmap(&self.x, &mut ov.cw, e.width, e.height, depth);
                            // Override-redirect popups never send WM present sync (cmd=22),
                            // so capture their resized pixmap immediately instead of holding
                            // stale dimensions for the 85ms managed-resize timeout.
                            ov.cw.confirm_sync_commit();
                        }
                        if self.comp.active && old_rect != new_rect {
                            dirty_move = Some(old_rect.union(new_rect).expand(20));
                        }
                    }
                    if let Some(d) = dirty_move {
                        self.comp.mark_dirty(d);
                    }
                }
            }
            Event::DamageNotify(e) => {
                if self.comp.active {
                    let local = Rect::new(
                        i32::from(e.area.x),
                        i32::from(e.area.y),
                        i32::from(e.area.width),
                        i32::from(e.area.height),
                    );
                    if let Some(cw) = self
                        .comps
                        .values_mut()
                        .find(|c| c.window == e.drawable || c.damage == e.damage)
                    {
                        cw.mark_damage(local);
                        if !cw.awaiting_sync_commit {
                            if let Some(m) = self.windows.iter().find(|w| w.window == cw.window) {
                                let scr = Rect::new(
                                    i32::from(m.x) + local.x - 16,
                                    i32::from(m.y) + local.y - 16,
                                    local.w + 32,
                                    local.h + 32,
                                );
                                self.comp.mark_dirty(scr);
                            }
                        }
                    } else if let Some(ov) = self
                        .override_wins
                        .iter_mut()
                        .find(|o| o.window == e.drawable || o.cw.damage == e.damage)
                    {
                        if ov.cw.awaiting_sync_commit {
                            ov.cw.confirm_sync_commit();
                        }
                        ov.cw.mark_damage(local);
                        self.comp.mark_dirty(Rect::new(
                            ov.rect.x + local.x - 16,
                            ov.rect.y + local.y - 16,
                            local.w + 32,
                            local.h + 32,
                        ));
                    }
                }
            }
            Event::MapRequest(e) => self.manage(e.window)?,
            Event::MapNotify(e) => {
                if self.comp.active
                    && !self.is_internal_window(e.window)
                    && !self.windows.iter().any(|w| w.window == e.window)
                {
                    if let Some((wx, wy, ww, wh, depth, or_flag, viewable)) =
                        self.x.get_window_info(e.window)
                    {
                        if (or_flag || e.override_redirect) && viewable && ww > 0 && wh > 0 {
                            self.track_override_window(
                                e.window,
                                Rect::new(i32::from(wx), i32::from(wy), i32::from(ww), i32::from(wh)),
                                depth,
                            );
                        }
                    }
                }
            }
            Event::ConfigureRequest(e) => {
                let mask = u16::from(e.value_mask);
                let mut aux = ConfigureWindowAux::new();
                if mask & u16::from(ConfigWindow::X) != 0 {
                    aux = aux.x(i32::from(e.x));
                }
                if mask & u16::from(ConfigWindow::Y) != 0 {
                    aux = aux.y(i32::from(e.y));
                }
                if mask & u16::from(ConfigWindow::WIDTH) != 0 {
                    aux = aux.width(u32::from(e.width));
                }
                if mask & u16::from(ConfigWindow::HEIGHT) != 0 {
                    aux = aux.height(u32::from(e.height));
                }
                self.x.conn.configure_window(e.window, &aux)?;
                let resized =
                    mask & (u16::from(ConfigWindow::WIDTH) | u16::from(ConfigWindow::HEIGHT)) != 0;
                let scr_w = i32::from(self.x.width);
                let work_h = i32::from(self.x.height) - self.panel_height();
                let is_vortex = self.theme.is_vortexglass();
                let mut dirty_req = None;
                let mut is_managed = false;
                if let Some(managed) = self.windows.iter_mut().find(|w| w.window == e.window) {
                    is_managed = true;
                    let old_rect = Rect::new(
                        i32::from(managed.x),
                        i32::from(managed.y),
                        i32::from(managed.width),
                        i32::from(managed.height),
                    );
                    if mask & u16::from(ConfigWindow::X) != 0 {
                        managed.x = e.x;
                    }
                    if mask & u16::from(ConfigWindow::Y) != 0 {
                        managed.y = e.y;
                    }
                    if mask & u16::from(ConfigWindow::WIDTH) != 0 {
                        managed.width = e.width;
                    }
                    if mask & u16::from(ConfigWindow::HEIGHT) != 0 {
                        managed.height = e.height;
                    }
                    let new_rect = Rect::new(
                        i32::from(managed.x),
                        i32::from(managed.y),
                        i32::from(managed.width),
                        i32::from(managed.height),
                    );
                    if self.comp.active && old_rect != new_rect {
                        let old_max = old_rect.w >= scr_w - 2 && old_rect.h >= work_h - 2;
                        let new_max = new_rect.w >= scr_w - 2 && new_rect.h >= work_h - 2;
                        dirty_req = Some(
                            window_damage_bounds(old_rect, is_vortex, old_max)
                                .union(window_damage_bounds(new_rect, is_vortex, new_max)),
                        );
                    }
                    if resized {
                        let notify = ConfigureNotifyEvent {
                            response_type: 22,
                            sequence: 0,
                            event: e.window,
                            window: e.window,
                            above_sibling: x11rb::NONE,
                            x: managed.x,
                            y: managed.y,
                            width: managed.width,
                            height: managed.height,
                            border_width: 0,
                            override_redirect: false,
                        };
                        let _ = self.x.conn.send_event(
                            false,
                            e.window,
                            EventMask::STRUCTURE_NOTIFY,
                            notify,
                        );
                    }
                }
                if let Some(d) = dirty_req {
                    self.comp.mark_dirty(d);
                }
                if is_managed
                    && (mask & u16::from(ConfigWindow::STACK_MODE) != 0)
                    && e.stack_mode == StackMode::ABOVE
                {
                    self.activate(e.window)?;
                }
            }
            Event::EnterNotify(e) => {
                if e.event == self.thumb_window {
                    let thumb_dirty = if let Some(t) = self.comp.thumb.as_mut() {
                        t.pointer_inside = true;
                        t.close_hover = t.close_rect.contains(
                            t.rect.x + i32::from(e.event_x),
                            t.rect.y + i32::from(e.event_y),
                        );
                        Some(t.rect)
                    } else {
                        None
                    };
                    if let Some(r) = thumb_dirty {
                        self.comp.thumb_leave_at = None;
                        self.comp.mark_dirty(r);
                    }
                    return Ok(());
                }
                if self.config.wm_focus_mode == FocusMode::Sloppy
                    && !self.menu_open
                    && !self.ctx_open
                    && !self.vol_open
                    && self.active != Some(e.event)
                    && self
                        .windows
                        .iter()
                        .any(|w| w.window == e.event && !w.minimized)
                {
                    let prev_active = self.active;
                    if self.config.wm_auto_raise {
                        self.x.raise(e.event)?;
                        self.raise_z(e.event);
                        self.raise_chrome_above_clients()?;
                    }
                    self.x.focus(e.event)?;
                    self.active = Some(e.event);
                    if self.comp.active {
                        if let Some(prev) = prev_active {
                            self.mark_window_dirty(prev);
                        }
                        self.mark_window_dirty(e.event);
                    }
                    self.publish_active()?;
                    self.draw_taskbar();
                    self.present_taskbar()?;
                }
            }
            Event::FocusOut(e) => {
                if e.mode == NotifyMode::NORMAL
                    && !matches!(
                        e.detail,
                        NotifyDetail::POINTER | NotifyDetail::INFERIOR | NotifyDetail::NONE
                    )
                {
                    if e.event == self.vol_popup && self.vol_open {
                        self.close_vol_popup()?;
                    } else if e.event == self.ctx_menu && self.ctx_open {
                        self.close_ctx()?;
                    } else if e.event == self.menu
                        && self.menu_open
                        && !self.config.panel_start_fullscreen
                    {
                        self.close_menu()?;
                    }
                }
            }
            Event::UnmapNotify(e) => {
                if e.window == self.vol_popup {
                    if self.vol_open {
                        self.close_vol_popup()?;
                    }
                } else if e.window == self.ctx_menu {
                    if self.ctx_open {
                        self.close_ctx()?;
                    }
                } else if e.window == self.menu {
                    if self.menu_open {
                        self.close_menu()?;
                    }
                } else if !self.remove_override_window(e.window) {
                    let is_minimized = self
                        .windows
                        .iter()
                        .find(|w| w.window == e.window)
                        .is_some_and(|w| w.minimized);
                    if !is_minimized && self.remove_window(e.window) {
                        self.draw_taskbar();
                        self.present_taskbar()?;
                    }
                }
            }
            Event::DestroyNotify(e) => {
                self.remove_override_window(e.window);
                if self.remove_window(e.window) {
                    self.draw_taskbar();
                    self.present_taskbar()?;
                }
            }
            Event::PropertyNotify(e) => {
                if e.window == self.x.root && e.atom == self.x.atoms.net_active_window {
                    let active = self.x.get_active_window();
                    let top_desync =
                        active.is_some_and(|w| self.z_stack.last().copied() != Some(w));
                    if active != self.active || top_desync {
                        let prev_active = self.active;
                        self.active = active;
                        if active.is_some() && self.vol_open {
                            self.close_vol_popup()?;
                        }
                        let target = active.filter(|window| {
                            !self.menu_open
                                && !self.vol_open
                                && self
                                    .windows
                                    .iter()
                                    .any(|w| w.window == *window && !w.minimized)
                        });
                        if let Some(window) = target {
                            self.x.raise(window)?;
                            self.raise_z(window);
                            self.raise_chrome_above_clients()?;
                            self.x.focus(window)?;
                            self.x.flush()?;
                            if self.comp.active {
                                self.mark_window_dirty(window);
                            }
                        }
                        if self.comp.active {
                            if let Some(prev) = prev_active {
                                self.mark_window_dirty(prev);
                            }
                        }
                        self.draw_taskbar();
                        self.present_taskbar()?;
                    }
                } else if e.window == self.x.root && e.atom == self.x.atoms.spime_status {
                    if let Some((cn, en)) = self.x.get_spime_status() {
                        if self.ime_chinese != cn || self.ime_active != en {
                            self.ime_chinese = cn;
                            self.ime_active = en;
                            self.draw_taskbar();
                            self.present_taskbar()?;
                        }
                    }
                } else if let Some(ov) =
                    self.override_wins.iter_mut().find(|o| o.window == e.window)
                {
                    if e.atom == self.x.atoms.spui_glass_extents {
                        ov.cw.glass_extents = self.x.get_glass_extents(e.window);
                        let dirty = ov.rect.expand(20);
                        self.comp.mark_dirty(dirty);
                    }
                } else if self.windows.iter().any(|w| w.window == e.window) {
                    let title = self.window_title(e.window);
                    let icon = self.window_icon(e.window, &title);
                    if e.atom == self.x.atoms.spui_glass_extents {
                        let ext = self.x.get_glass_extents(e.window);
                        let is_vortex = self.theme.is_vortexglass();
                        let win_rect = self
                            .windows
                            .iter()
                            .find(|w| w.window == e.window)
                            .map(|m| self.window_rect_for(m));
                        if let Some(cw) = self.comps.get_mut(&e.window) {
                            cw.glass_extents = ext;
                        }
                        if let Some((rect, is_max)) = win_rect {
                            self.comp
                                .mark_dirty(window_damage_bounds(rect, is_vortex, is_max));
                        }
                    }
                    if let Some(managed) = self.windows.iter_mut().find(|w| w.window == e.window) {
                        if managed.title != title || managed.icon != icon {
                            managed.icon = icon;
                            managed.title = title;
                            self.draw_taskbar();
                            self.present_taskbar()?;
                        }
                    }
                }
            }
            Event::Expose(e) => {
                let area = Rect::new(
                    i32::from(e.x),
                    i32::from(e.y),
                    i32::from(e.width),
                    i32::from(e.height),
                );
                if e.window == self.desktop {
                    if self.desktop_bg == 0 {
                        self.desktop_canvas.mark_dirty(area);
                        self.present_desktop()?;
                    }
                } else if e.window == self.taskbar {
                    self.taskbar_canvas.mark_dirty(area);
                    self.present_taskbar()?;
                } else if e.window == self.menu && self.menu_open {
                    self.menu_canvas.mark_dirty(area);
                    self.present_menu()?;
                } else if e.window == self.ctx_menu && self.ctx_open {
                    self.ctx_canvas.mark_dirty(area);
                    self.present_ctx()?;
                } else if e.window == self.vol_popup && self.vol_open {
                    self.vol_canvas.mark_dirty(area);
                    self.present_vol_popup()?;
                } else if e.window == self.toast_window && self.toast.is_some() {
                    self.toast_canvas.mark_dirty(area);
                    self.present_toast()?;
                }
            }
            Event::LeaveNotify(e) => {
                ui::notify_pointer_leave();
                if e.event == self.menu
                    && self.menu_open
                    && self.config.panel_start_fullscreen
                    && self.start_screen.on_leave()
                {
                    self.draw_menu();
                    self.present_menu()?;
                }
                if e.event == self.thumb_window {
                    if matches!(e.mode, NotifyMode::GRAB | NotifyMode::UNGRAB) {
                        return Ok(());
                    }
                    let mut thumb_rect = None;
                    if let Some(t) = self.comp.thumb.as_mut() {
                        t.pointer_inside = false;
                        t.close_hover = false;
                        thumb_rect = Some(t.rect);
                    }
                    if let Some(r) = thumb_rect {
                        self.comp.mark_dirty(r);
                    }
                    let over_other_btn = self
                        .taskbar_local_coords(e.root_x, e.root_y)
                        .is_some_and(|(tx, ty)| match self.taskbar_hit(tx, ty) {
                            Hit::Window(idx) => self
                                .comp
                                .thumb
                                .as_ref()
                                .is_some_and(|t| t.win_index != idx),
                            Hit::None => false,
                            _ => true,
                        });
                    if over_other_btn {
                        self.close_thumb()?;
                    } else if self.comp.thumb.is_some() {
                        self.comp.thumb_leave_at = Some(Instant::now());
                    }
                    return Ok(());
                }
                let prev = self.hover;
                if (e.event == self.taskbar
                    && matches!(
                        prev,
                        Hit::Start
                            | Hit::Pinned(_)
                            | Hit::Window(_)
                            | Hit::ImeTray
                            | Hit::Network
                            | Hit::Volume
                            | Hit::Bell
                            | Hit::ShowDesktop
                    ))
                    || (e.event == self.menu
                        && matches!(
                            prev,
                            Hit::MenuRow(_)
                                | Hit::MenuPlace(_)
                                | Hit::Logout
                                | Hit::Reboot
                                | Hit::PowerOff
                                | Hit::ConfirmPowerOk
                                | Hit::ConfirmPowerCancel
                        ))
                    || (e.event == self.ctx_menu && matches!(prev, Hit::ContextRow(_)))
                    || (e.event == self.vol_popup
                        && !self.vol_dragging
                        && matches!(
                            prev,
                            Hit::VolMute | Hit::VolSlider | Hit::VolSfx | Hit::VolSettings
                        ))
                {
                    self.hover = Hit::None;
                    if e.event == self.taskbar {
                        self.comp.taskbar_mouse_x = None;
                        self.comp.thumb_hover_start = None;
                        self.comp.show_desktop_peek = false;
                        if !matches!(e.mode, NotifyMode::GRAB | NotifyMode::UNGRAB)
                            && self.comp.thumb.is_some()
                        {
                            if self.thumb_bridge_contains(i32::from(e.root_x), i32::from(e.root_y))
                            {
                                if self.comp.thumb_leave_at.is_none() {
                                    self.comp.thumb_leave_at = Some(Instant::now());
                                }
                            } else {
                                self.close_thumb()?;
                            }
                        }
                        self.draw_taskbar();
                        self.present_taskbar()?;
                    } else if e.event == self.menu && self.menu_open {
                        self.draw_menu();
                        self.present_menu()?;
                    } else if e.event == self.ctx_menu && self.ctx_open {
                        self.draw_ctx();
                        self.present_ctx()?;
                    } else if e.event == self.vol_popup && self.vol_open {
                        self.draw_vol_popup();
                        self.present_vol_popup()?;
                    }
                }
            }
            Event::ButtonPress(e) => {
                let (x, y) = (e.event_x as i32, e.event_y as i32);
                ui::notify_pointer_button(x, y, e.detail, true);
                if self.comp.flip3d.is_some() || self.comp.alt_tab.is_some() {
                    match e.detail {
                        4 => {
                            self.cycle_switcher(-1);
                            return Ok(());
                        }
                        5 => {
                            self.cycle_switcher(1);
                            return Ok(());
                        }
                        1 => {
                            self.commit_switcher()?;
                            return Ok(());
                        }
                        3 => {
                            self.cancel_switcher()?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                if e.event == self.thumb_window {
                    if let Some(t) = &self.comp.thumb {
                        let target = t.target_window;
                        let on_close = t.close_rect.contains(t.rect.x + x, t.rect.y + y);
                        self.close_thumb()?;
                        if e.detail == 1 {
                            if on_close {
                                let _ = self.x.send_delete(target);
                                let _ = self.x.flush();
                            } else {
                                self.activate(target)?;
                            }
                        }
                    }
                } else if e.event == self.taskbar {
                    self.close_thumb()?;
                    if (e.detail == 4 || e.detail == 5) && self.vol_rect().contains(x, y) {
                        let step = if e.detail == 4 { 5 } else { -5 };
                        self.adjust_volume_step(step)?;
                    } else if (e.detail == 2 || e.detail == 3) && self.vol_rect().contains(x, y) {
                        self.toggle_mute()?;
                    } else if e.detail == 1 {
                        self.close_ctx()?;
                        self.taskbar_click(x, y)?;
                        self.draw_taskbar();
                        self.present_taskbar()?;
                    } else if e.detail == 3 && self.taskbar_hit(x, y) == Hit::ImeTray {
                        let py = if self.config.panel_position == PanelPosition::Top {
                            self.panel_height() + 4
                        } else {
                            i32::from(self.x.height) - self.panel_height() - CTX_HEIGHT - 4
                        };
                        self.open_ctx_mode(e.root_x as i32 - CTX_WIDTH / 2, py, true)?;
                    } else {
                        self.close_ctx()?;
                        self.close_vol_popup()?;
                        self.close_menu()?;
                    }
                } else if e.event == self.vol_popup && self.vol_open {
                    if !popup_contains_point(VOL_POPUP_WIDTH, VOL_POPUP_HEIGHT, x, y) {
                        if (e.detail == 4 || e.detail == 5)
                            && self
                                .taskbar_local_coords(e.root_x, e.root_y)
                                .is_some_and(|(tx, ty)| self.taskbar_hit(tx, ty) == Hit::Volume)
                        {
                            let step = if e.detail == 4 { 5 } else { -5 };
                            self.adjust_volume_step(step)?;
                        } else {
                            self.vol_dragging = false;
                            self.close_vol_popup()?;
                            if e.detail == 1 {
                                if let Some((tx, ty)) =
                                    self.taskbar_local_coords(e.root_x, e.root_y)
                                {
                                    if self.taskbar_hit(tx, ty) != Hit::Volume {
                                        ui::notify_pointer_button(tx, ty, 1, true);
                                        self.taskbar_click(tx, ty)?;
                                        self.draw_taskbar();
                                        self.present_taskbar()?;
                                    }
                                }
                            }
                        }
                    } else if e.detail == 4 || e.detail == 5 {
                        let step = if e.detail == 4 { 5 } else { -5 };
                        self.adjust_volume_step(step)?;
                    } else if e.detail == 1 {
                        self.vol_popup_click(x, y)?;
                    }
                } else if e.event == self.menu && self.menu_open {
                    if self.config.panel_start_fullscreen {
                        if e.detail == 4 || e.detail == 5 {
                            let delta = if e.detail == 4 { -1 } else { 1 };
                            if self.start_screen.on_wheel(
                                i32::from(self.x.width),
                                i32::from(self.x.height),
                                x,
                                y,
                                delta,
                            ) {
                                self.draw_menu();
                                self.present_menu()?;
                            }
                        } else if e.detail == 1 || e.detail == 3 {
                            let (action, prev_cfg) = {
                                let mut host = ShellConfigHost::new(
                                    &self.x,
                                    &mut self.config,
                                    &mut self.ux_client,
                                );
                                let action =
                                    self.start_screen.on_button(&mut host, x, y, e.detail, true);
                                (action, host.take_changed())
                            };
                            self.handle_start_action(action, prev_cfg)?;
                        }
                    } else if {
                        let (mw, mh) = self.menu_dims();
                        !popup_contains_point(mw, mh, x, y)
                    } {
                        self.close_menu()?;
                        if e.detail == 1 {
                            if let Some((tx, ty)) = self.taskbar_local_coords(e.root_x, e.root_y) {
                                if self.taskbar_hit(tx, ty) != Hit::Start {
                                    ui::notify_pointer_button(tx, ty, 1, true);
                                    self.taskbar_click(tx, ty)?;
                                    self.draw_taskbar();
                                    self.present_taskbar()?;
                                }
                            }
                        }
                    } else if e.detail == 1 {
                        self.menu_click(x, y)?;
                    }
                } else if e.event == self.ctx_menu && self.ctx_open {
                    if !popup_contains_point(CTX_WIDTH, CTX_HEIGHT, x, y) {
                        self.close_ctx()?;
                        if e.detail == 1 {
                            if let Some((tx, ty)) = self.taskbar_local_coords(e.root_x, e.root_y) {
                                ui::notify_pointer_button(tx, ty, 1, true);
                                self.taskbar_click(tx, ty)?;
                                self.draw_taskbar();
                                self.present_taskbar()?;
                            }
                        }
                    } else if e.detail == 1 {
                        self.ctx_click(x, y)?;
                    }
                } else if e.event == self.desktop {
                    self.close_menu()?;
                    self.close_vol_popup()?;
                    if e.detail == 3 {
                        self.open_ctx(i32::from(e.root_x), i32::from(e.root_y))?;
                    } else if e.detail == 1 {
                        self.close_ctx()?;
                        let hit = self.desktop_hit(x, y);
                        if let Hit::DesktopIcon(idx) = hit {
                            self.selected_desktop_icon = Some(idx);
                            self.draw_desktop();
                            self.present_desktop()?;
                            crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                            self.launch(APPS[idx].mode);
                        } else if self.selected_desktop_icon.take().is_some() {
                            self.draw_desktop();
                            self.present_desktop()?;
                        }
                    }
                } else if e.event == self.toast_window && e.detail == 1 {
                    if Rect::new(TOAST_WIDTH - 30, 8, 22, 22).contains(x, y) {
                        self.hide_toast()?;
                    }
                }
            }
            Event::ButtonRelease(e) => {
                ui::notify_pointer_button(e.event_x as i32, e.event_y as i32, e.detail, false);
                if e.detail == 1 && self.wm_drag.is_some() {
                    self.finish_wm_drag()?;
                    return Ok(());
                }
                if e.detail == 1 && self.vol_dragging {
                    self.vol_dragging = false;
                    self.stage_sound_persist();
                    crate::sound::play(&self.config, crate::sound::SystemSound::Volume);
                    if self.vol_open {
                        self.draw_vol_popup();
                        self.present_vol_popup()?;
                    }
                } else if e.event == self.menu
                    && self.menu_open
                    && self.config.panel_start_fullscreen
                    && e.detail == 1
                {
                    let (x, y) = (e.event_x as i32, e.event_y as i32);
                    let (action, prev_cfg) = {
                        let mut host =
                            ShellConfigHost::new(&self.x, &mut self.config, &mut self.ux_client);
                        let action = self.start_screen.on_button(&mut host, x, y, 1, false);
                        (action, host.take_changed())
                    };
                    self.handle_start_action(action, prev_cfg)?;
                }
            }
            Event::MotionNotify(e) => {
                let (x, y) = (e.event_x as i32, e.event_y as i32);
                let btn1_held = u16::from(e.state) & (1 << 8) != 0;
                ui::notify_pointer_motion(x, y, btn1_held);
                if self.wm_drag.is_some() {
                    if btn1_held {
                        self.update_wm_drag_motion(i32::from(e.root_x), i32::from(e.root_y));
                    } else {
                        self.finish_wm_drag()?;
                    }
                    return Ok(());
                }
                if e.event == self.desktop {
                    let hit = self.desktop_hit(x, y);
                    if hit != self.hover {
                        let changed_desktop = matches!(hit, Hit::DesktopIcon(_))
                            || matches!(self.hover, Hit::DesktopIcon(_));
                        self.hover = hit;
                        let cursor = if matches!(hit, Hit::DesktopIcon(_)) {
                            CursorKind::Pointer
                        } else {
                            CursorKind::Default
                        };
                        let _ = self.x.set_cursor(self.desktop, cursor);
                        if changed_desktop {
                            self.draw_desktop();
                            self.present_desktop()?;
                        }
                    }
                } else if e.event == self.taskbar {
                    self.comp.taskbar_mouse_x = Some(x);
                    let hit = self.taskbar_hit(x, y);
                    let need_redraw = hit != self.hover;
                    self.hover = hit;
                    if self.comp.active {
                        self.comp.show_desktop_peek = hit == Hit::ShowDesktop;
                        match hit {
                            Hit::Window(idx) => {
                                if let Some(m) = self.windows.get(idx) {
                                    let win = m.window;
                                    if self.comp.thumb.as_ref().is_some_and(|t| t.target_window == win)
                                    {
                                        self.comp.thumb_leave_at = None;
                                    } else if self
                                        .comp
                                        .thumb
                                        .as_ref()
                                        .is_some_and(|t| t.target_window != win)
                                    {
                                        self.open_thumb(idx)?;
                                    } else if self.comp.thumb.is_none()
                                        && !self.comp.thumb_hover_start.is_some_and(|(i, _)| i == idx)
                                    {
                                        self.comp.thumb_hover_start = Some((idx, Instant::now()));
                                    }
                                }
                            }
                            _ => {
                                self.comp.thumb_hover_start = None;
                                if self.comp.thumb.is_some() {
                                    self.close_thumb()?;
                                }
                            }
                        }
                        if self.theme.is_vortexglass() && need_redraw {
                            self.comp.mark_dirty(Rect::new(
                                0,
                                self.taskbar_y(),
                                i32::from(self.x.width),
                                self.panel_height(),
                            ));
                        }
                    }
                    if need_redraw {
                        self.draw_taskbar();
                        self.present_taskbar()?;
                    }
                } else if e.event == self.thumb_window {
                    let mut thumb_dirty = None;
                    if let Some(t) = self.comp.thumb.as_mut() {
                        let was_in = t.pointer_inside;
                        let was_close = t.close_hover;
                        t.pointer_inside = true;
                        t.close_hover = t.close_rect.contains(t.rect.x + x, t.rect.y + y);
                        self.comp.thumb_leave_at = None;
                        if !was_in || was_close != t.close_hover {
                            thumb_dirty = Some(t.rect);
                        }
                    }
                    if let Some(r) = thumb_dirty {
                        self.comp.mark_dirty(r);
                    }
                } else if e.event == self.vol_popup && self.vol_open {
                    if self.vol_dragging {
                        let vol = vol_from_slider_x(vol_popup_slider_rect(), x);
                        self.set_master_volume(vol, false)?;
                    } else {
                        let hit = if popup_contains_point(VOL_POPUP_WIDTH, VOL_POPUP_HEIGHT, x, y) {
                            self.vol_popup_hit(x, y)
                        } else {
                            Hit::None
                        };
                        if hit != self.hover {
                            self.hover = hit;
                            let cursor = if hit == Hit::None {
                                CursorKind::Default
                            } else {
                                CursorKind::Pointer
                            };
                            let _ = self.x.set_cursor(self.vol_popup, cursor);
                            self.draw_vol_popup();
                            self.present_vol_popup()?;
                        }
                    }
                } else if e.event == self.menu && self.menu_open {
                    if self.config.panel_start_fullscreen {
                        let (redraw, cursor, prev_cfg) = {
                            let mut host = ShellConfigHost::new(
                                &self.x,
                                &mut self.config,
                                &mut self.ux_client,
                            );
                            let (r, c) = self.start_screen.on_motion(&mut host, x, y);
                            (r, c, host.take_changed())
                        };
                        let _ = self.x.set_cursor(self.menu, cursor);
                        if let Some(prev) = prev_cfg {
                            self.apply_live_config_diff(prev)?;
                        } else if redraw {
                            self.draw_menu();
                            self.present_menu()?;
                        }
                    } else {
                        let hit = self.menu_hit(x, y);
                        if hit != self.hover {
                            self.hover = hit;
                            self.draw_menu();
                            self.present_menu()?;
                        }
                    }
                } else if e.event == self.ctx_menu && self.ctx_open {
                    let hit = self.ctx_hit(x, y);
                    if hit != self.hover {
                        self.hover = hit;
                        self.draw_ctx();
                        self.present_ctx()?;
                    }
                }
            }
            Event::KeyPress(e) => {
                let state: u16 = e.state.into();
                let key = Key {
                    code: e.detail,
                    state,
                    sym: self.x.key_symbol(e.detail, state),
                };
                if self.menu_open && self.menu_confirm_power.is_none() {
                    let wants_ime = !self.config.panel_start_fullscreen
                        || self.start_screen.wants_ime();
                    self.menu_ime_focused = wants_ime;
                    if wants_ime {
                        if let Some(reply) = self.ime_client.key_event(
                            self.menu,
                            true,
                            key.sym,
                            state,
                            u16::from(e.detail),
                        ) {
                            let consumed = reply.consumed;
                            self.apply_menu_ime_reply(reply)?;
                            if consumed {
                                return Ok(());
                            }
                        }
                    }
                }
                let is_tab = matches!(key.sym, 0xff09 | 0xfe20);
                let alt_held = (key.state & 0x0008) != 0;
                let win_held = (key.state & 0x0040) != 0;
                let dir = if key.shift() { -1 } else { 1 };
                if is_tab && win_held && !self.windows.is_empty() {
                    self.open_or_cycle_flip3d(dir)?;
                } else if is_tab && alt_held && !self.windows.is_empty() {
                    self.open_or_cycle_alt_tab(dir)?;
                } else if self.comp.flip3d.is_some() || self.comp.alt_tab.is_some() {
                    if is_tab || matches!(key.sym, 0xff53 | 0xff54) {
                        self.cycle_switcher(dir);
                    } else if matches!(key.sym, 0xff51 | 0xff52) {
                        self.cycle_switcher(-1);
                    } else if key.is_enter() {
                        self.commit_switcher()?;
                    } else if key.is_escape() {
                        self.cancel_switcher()?;
                    }
                } else if matches!(key.sym, 0xffeb | 0xffec) || (key.control() && key.is_escape()) {
                    self.toggle_menu()?;
                } else if self.ctx_open && key.is_escape() {
                    self.close_ctx()?;
                } else if self.vol_open && key.is_escape() {
                    self.close_vol_popup()?;
                } else if self.menu_open && self.config.panel_start_fullscreen {
                    let (action, prev_cfg) = {
                        let mut host =
                            ShellConfigHost::new(&self.x, &mut self.config, &mut self.ux_client);
                        let action = self.start_screen.on_key(&mut host, key);
                        (action, host.take_changed())
                    };
                    self.handle_start_action(action, prev_cfg)?;
                } else if self.menu_open {
                    if let Some(action) = self.menu_confirm_power {
                        if key.is_enter() {
                            self.menu_confirm_power = None;
                            self.execute_power_action(action)?;
                            return Ok(());
                        }
                        if key.is_escape() {
                            self.menu_confirm_power = None;
                            self.draw_menu();
                            self.present_menu()?;
                            return Ok(());
                        }
                        return Ok(());
                    }
                    if key.is_escape() {
                        self.close_menu()?;
                    } else if key.is_backspace() {
                        if self.menu_query.pop().is_some() {
                            self.draw_menu();
                            self.present_menu()?;
                        }
                    } else if key.is_enter() {
                        let filtered = self.filtered_apps();
                        if let Some(&idx) = filtered.first() {
                            let mode = APPS[idx].mode;
                            self.close_menu()?;
                            self.launch(mode);
                        }
                    } else if let Some(ch) = key.text().filter(|c| !c.is_control()) {
                        if self.menu_query.chars().count() < 24 {
                            self.menu_query.push(ch);
                            self.draw_menu();
                            self.present_menu()?;
                        }
                    }
                }
            }
            Event::KeyRelease(e) => {
                if self.menu_open && self.menu_ime_focused {
                    let state: u16 = e.state.into();
                    let sym = self.x.key_symbol(e.detail, state);
                    if let Some(reply) =
                        self.ime_client
                            .key_event(self.menu, false, sym, state, u16::from(e.detail))
                    {
                        self.apply_menu_ime_reply(reply)?;
                    }
                }
                let sym = self.x.key_symbol(e.detail, e.state.into());
                if self.comp.alt_tab.is_some() && matches!(sym, 0xffe9 | 0xffea) {
                    self.commit_switcher()?;
                } else if self.comp.flip3d.is_some() && matches!(sym, 0xffeb | 0xffec) {
                    self.commit_switcher()?;
                }
            }
            Event::ClientMessage(e) => {
                if e.type_ == self.x.atoms.spime_notify && self.menu_open {
                    for _ in 0..8 {
                        let Some(reply) = self.ime_client.poll_pending() else {
                            break;
                        };
                        let more = reply.consumed;
                        self.apply_menu_ime_reply(reply)?;
                        if !more {
                            break;
                        }
                    }
                } else if e.type_ == self.x.atoms.net_active_window {
                    let window = e.data.as_data32()[0];
                    self.activate(window)?;
                } else if e.type_ == self.x.atoms.spui_window_cmd {
                    let data = e.data.as_data32();
                    match data[0] {
                        1 => self.minimize_window(e.window)?,
                        10 => {
                            let sn = SnapZone::from_u32(data[1]);
                            let prev_sr = self.comp.snap_rect;
                            self.comp.snap_zone = sn;
                            if sn != SnapZone::None {
                                let ph = self.panel_height();
                                let wt = if self.config.panel_position == PanelPosition::Top {
                                    ph
                                } else {
                                    0
                                };
                                let wh = i32::from(self.x.height).saturating_sub(ph);
                                self.comp.snap_rect =
                                    sn.target_rect(i32::from(self.x.width), wt, wh);
                            }
                            if let Some(sr) = prev_sr.or(self.comp.snap_rect) {
                                let merged = self
                                    .comp
                                    .snap_rect
                                    .map_or(sr, |nr| sr.union(nr))
                                    .expand(20);
                                self.comp.mark_dirty(merged);
                            } else {
                                self.comp.mark_full_dirty();
                            }
                        }
                        20 => {
                            let mode = data[1];
                            let press_rx = i32::from((data[2] as u16) as i16);
                            let press_ry = i32::from((data[3] as u16) as i16);
                            self.start_wm_drag(e.window, mode, press_rx, press_ry, data[4])?;
                        }
                        22 => {
                            if self.comp.active {
                                let mut commit_rect = None;
                                if let Some(cw) = self.comps.get_mut(&e.window) {
                                    if data[1] == 0
                                        || (cw.pixmap_w == data[1] as usize
                                            && cw.pixmap_h == data[2] as usize)
                                    {
                                        cw.confirm_sync_commit();
                                        if let Some((r, is_max)) = self
                                            .windows
                                            .iter()
                                            .find(|m| m.window == e.window)
                                            .map(|m| self.window_rect_for(m))
                                        {
                                            commit_rect = Some(window_damage_bounds(
                                                r,
                                                self.theme.is_vortexglass(),
                                                is_max,
                                            ));
                                        }
                                    }
                                } else if let Some(ov) =
                                    self.override_wins.iter_mut().find(|o| o.window == e.window)
                                {
                                    if data[1] == 0
                                        || (ov.cw.pixmap_w == data[1] as usize
                                            && ov.cw.pixmap_h == data[2] as usize)
                                    {
                                        ov.cw.confirm_sync_commit();
                                        commit_rect = Some(ov.rect.expand(20));
                                    }
                                }
                                if let Some(cr) = commit_rect {
                                    self.comp.mark_dirty(cr);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn filtered_apps(&self) -> Vec<usize> {
        let q = self.menu_query.trim().to_lowercase();
        if q.is_empty() {
            return (0..APPS.len()).collect();
        }
        APPS.iter()
            .enumerate()
            .filter(|(_, app)| {
                app.name.to_lowercase().contains(&q)
                    || app.subtitle.to_lowercase().contains(&q)
                    || app.keywords.to_lowercase().contains(&q)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    fn window_title(&self, window: Window) -> String {
        let title = self.x.get_title(window).unwrap_or_default();
        if !title.is_empty() {
            return title;
        }
        "应用".to_string()
    }

    fn window_icon(&self, window: Window, title: &str) -> Icon {
        self.x
            .get_icon_name(window)
            .ok()
            .flatten()
            .and_then(|name| Icon::from_prop_name(&name))
            .unwrap_or_else(|| infer_window_icon(title))
    }

    fn manage(&mut self, window: Window) -> Result<()> {
        self.close_vol_popup()?;
        self.close_ctx()?;
        if let Some(existing) = self.windows.iter_mut().find(|w| w.window == window) {
            existing.minimized = false;
            self.x.map(window)?;
            self.activate(window)?;
            return Ok(());
        }
        let title = self.window_title(window);
        let icon = self.window_icon(window, &title);
        let geometry = self.x.conn.get_geometry(window)?.reply()?;
        // Listen for title changes and pointer entry (for focus-follows-mouse) on managed client.
        let _ = self.x.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::PROPERTY_CHANGE | EventMask::ENTER_WINDOW),
        );

        let ph = self.panel_height();
        let top_y = if self.config.panel_position == PanelPosition::Top {
            ph
        } else {
            0
        };
        let usable_h = (i32::from(self.x.height) - ph).max(200);
        let min_x = if self.config.desktop_icons_visible {
            84
        } else {
            16
        };

        let (mut wx, mut wy) = match self.config.wm_placement {
            WindowPlacement::Center => (
                (i32::from(self.x.width) - i32::from(geometry.width)) / 2,
                top_y + (usable_h - i32::from(geometry.height)) / 2,
            ),
            WindowPlacement::Cascade => {
                let index = self.windows.len() as i32;
                (
                    (i32::from(self.x.width) - i32::from(geometry.width)) / 2 - 24
                        + (index % 5) * 28,
                    top_y + (usable_h - i32::from(geometry.height)) / 2 - 16 + (index % 5) * 26,
                )
            }
        };
        wx = wx.clamp(
            min_x,
            (i32::from(self.x.width) - i32::from(geometry.width) - 12).max(12),
        );
        wy = wy.clamp(
            top_y + 10,
            (top_y + usable_h - i32::from(geometry.height) - 10).max(top_y + 4),
        );

        self.x.move_resize(
            window,
            wx as i16,
            wy as i16,
            geometry.width,
            geometry.height,
        )?;
        let glass_extents = self.x.get_glass_extents(window);
        self.x.map(window)?;
        self.x.raise(window)?;
        self.raise_z(window);
        self.raise_chrome_above_clients()?;
        self.x.focus(window)?;
        self.active = Some(window);
        self.windows.push(Managed {
            window,
            title,
            icon,
            minimized: false,
            x: wx as i16,
            y: wy as i16,
            width: geometry.width,
            height: geometry.height,
        });
        if self.comp.active {
            let mut cw = CompWindow::new(window, geometry.depth, glass_extents);
            cw.anim = Some(WindowAnim {
                kind: AnimKind::Open,
                start: Instant::now(),
                duration_ms: 200,
            });
            attach_window_pixmap(&self.x, &mut cw, geometry.width, geometry.height, geometry.depth);
            self.comps.insert(window, cw);
            let open_rect = Rect::new(
                wx,
                wy,
                i32::from(geometry.width),
                i32::from(geometry.height),
            );
            self.comp.mark_dirty(
                window_damage_bounds(open_rect, self.theme.is_vortexglass(), false).expand(24),
            );
        }
        self.publish_client_list()?;
        self.publish_active()?;
        self.draw_taskbar();
        self.present_taskbar()?;
        Ok(())
    }

    fn minimize_window(&mut self, window: Window) -> Result<()> {
        if self.wm_drag.is_some_and(|d| d.window == window) {
            self.cancel_wm_drag();
        }
        let Some(win_idx) = self.windows.iter().position(|w| w.window == window) else {
            return Ok(());
        };
        if self.windows[win_idx].minimized {
            return Ok(());
        }
        let m_rect = Rect::new(
            i32::from(self.windows[win_idx].x),
            i32::from(self.windows[win_idx].y),
            i32::from(self.windows[win_idx].width),
            i32::from(self.windows[win_idx].height),
        );
        let btn_local = self.window_button_rect(win_idx);
        let btn_screen = Rect::new(btn_local.x, self.taskbar_y() + btn_local.y, btn_local.w, btn_local.h);
        if self.comp.active {
            if let Some(cw) = self.comps.get_mut(&window) {
                if cw.has_pixels && !cw.pixels.is_empty() {
                    self.comp.ghosts.push(ClosingGhost {
                        pixels: cw.pixels.clone(),
                        pw: cw.pw,
                        ph: cw.ph,
                        from: m_rect,
                        to_taskbar: Some(btn_screen),
                        corner_r: cw.glass_extents.map_or(0, |e| e[4]),
                        start: Instant::now(),
                        duration_ms: 220,
                    });
                }
                cw.anim = None;
            }
            self.comp.mark_dirty(m_rect.union(btn_screen).expand(28));
        }
        self.windows[win_idx].minimized = true;
        self.x.unmap(window)?;
        if self.active == Some(window) {
            self.active = self
                .z_stack
                .iter()
                .rev()
                .copied()
                .find(|&zw| {
                    zw != window
                        && self
                            .windows
                            .iter()
                            .any(|w| w.window == zw && !w.minimized)
                })
                .or_else(|| {
                    self.windows
                        .iter()
                        .rev()
                        .find(|w| !w.minimized)
                        .map(|w| w.window)
                });
            if let Some(next) = self.active {
                let _ = self.x.raise(next);
                self.raise_z(next);
                let _ = self.raise_chrome_above_clients();
                let _ = self.x.focus(next);
            } else {
                let _ = self.x.focus(self.x.root);
            }
            self.publish_active()?;
        }
        self.draw_taskbar();
        self.present_taskbar()?;
        Ok(())
    }

    fn remove_window(&mut self, window: Window) -> bool {
        if self.wm_drag.is_some_and(|d| d.window == window) {
            self.cancel_wm_drag();
        }
        let pos = self.windows.iter().position(|w| w.window == window);
        let Some(idx) = pos else {
            return false;
        };
        let m = self.windows.remove(idx);
        self.z_stack.retain(|&w| w != window);
        self.all_minimized_backup.retain(|&w| w != window);
        if self
            .comp
            .thumb
            .as_ref()
            .is_some_and(|t| t.target_window == window)
        {
            let _ = self.close_thumb();
        }
        if let Some(mut cw) = self.comps.remove(&window) {
            if self.comp.active && !m.minimized && cw.has_pixels && !cw.pixels.is_empty() {
                self.comp.ghosts.push(ClosingGhost {
                    pixels: std::mem::take(&mut cw.pixels),
                    pw: cw.pw,
                    ph: cw.ph,
                    from: Rect::new(
                        i32::from(m.x),
                        i32::from(m.y),
                        i32::from(m.width),
                        i32::from(m.height),
                    ),
                    to_taskbar: None,
                    corner_r: cw.glass_extents.map_or(0, |e| e[4]),
                    start: Instant::now(),
                    duration_ms: 170,
                });
            }
            detach_window_resources(&self.x, &mut cw);
            let close_rect = Rect::new(
                i32::from(m.x),
                i32::from(m.y),
                i32::from(m.width),
                i32::from(m.height),
            );
            self.comp.mark_dirty(
                window_damage_bounds(close_rect, self.theme.is_vortexglass(), false).expand(24),
            );
        }
        if self.active == Some(window) {
            self.active = self
                .z_stack
                .iter()
                .rev()
                .copied()
                .find(|&zw| self.windows.iter().any(|w| w.window == zw && !w.minimized))
                .or_else(|| {
                    self.windows
                        .iter()
                        .rev()
                        .find(|w| !w.minimized)
                        .map(|w| w.window)
                });
            if let Some(next) = self.active {
                let _ = self.x.raise(next);
                self.raise_z(next);
                let _ = self.raise_chrome_above_clients();
                let _ = self.x.focus(next);
            }
            let _ = self.publish_active();
        }
        let _ = self.publish_client_list();
        true
    }

    fn work_area_vertical(&self) -> (i32, i32) {
        let ph = self.panel_height();
        let work_top = if self.config.panel_position == PanelPosition::Top {
            ph
        } else {
            0
        };
        let work_h = (i32::from(self.x.height) - ph).max(200);
        (work_top, work_h)
    }

    fn start_wm_drag(
        &mut self,
        window: Window,
        mode: u32,
        press_rx: i32,
        press_ry: i32,
        packed_min: u32,
    ) -> Result<()> {
        let Some((orig_x, orig_y, orig_w, orig_h)) = self
            .windows
            .iter()
            .find(|w| w.window == window && !w.minimized)
            .map(|w| (i32::from(w.x), i32::from(w.y), i32::from(w.width), i32::from(w.height)))
        else {
            return Ok(());
        };
        let Some((cur_rx, cur_ry, mask)) = self.x.query_pointer_root() else {
            return Ok(());
        };
        if mask & (1 << 8) == 0 {
            return Ok(());
        }
        self.close_menu()?;
        self.close_ctx()?;
        self.close_vol_popup()?;
        let _ = self.close_thumb();
        if self.active != Some(window) || self.z_stack.last().copied() != Some(window) {
            self.activate(window)?;
        }
        let cursor = match mode {
            1 | 5 => CursorKind::ResizeDiagF,
            3 | 7 => CursorKind::ResizeDiagB,
            2 | 6 => CursorKind::ResizeV,
            4 | 8 => CursorKind::ResizeH,
            _ => CursorKind::Default,
        };
        if self.pointer_grab.is_some() {
            self.release_popup_grab();
        }
        if !self.x.grab_pointer_cursor(self.x.root, cursor) {
            return Ok(());
        }
        self.pointer_grab = Some(self.x.root);
        let min_w = ((packed_min >> 16) as i32).max(160);
        let min_h = ((packed_min & 0xffff) as i32).max(120);
        self.wm_drag = Some(WmDrag {
            window,
            mode,
            orig_x,
            orig_y,
            orig_w,
            orig_h,
            press_rx,
            press_ry,
            min_w,
            min_h,
            pending_x: orig_x,
            pending_y: orig_y,
            pending_w: orig_w,
            pending_h: orig_h,
            has_pending: false,
        });
        if i32::from(cur_rx) != press_rx || i32::from(cur_ry) != press_ry {
            self.update_wm_drag_motion(i32::from(cur_rx), i32::from(cur_ry));
        }
        Ok(())
    }

    fn update_wm_drag_motion(&mut self, rx: i32, ry: i32) {
        let Some(mut drag) = self.wm_drag else {
            return;
        };
        let scr_w = i32::from(self.x.width);
        let (work_top, work_h) = self.work_area_vertical();
        let dx = rx - drag.press_rx;
        let dy = ry - drag.press_ry;

        if drag.mode == 0 {
            drag.pending_x = drag.orig_x + dx;
            drag.pending_y = (drag.orig_y + dy).max(work_top);
            drag.has_pending = true;
            let zone = if ry <= work_top + 6 {
                SnapZone::Maximize
            } else if rx <= 10 {
                SnapZone::Left
            } else if rx >= scr_w - 11 {
                SnapZone::Right
            } else {
                SnapZone::None
            };
            if zone != self.comp.snap_zone {
                let prev_sr = self.comp.snap_rect;
                self.comp.snap_zone = zone;
                if zone != SnapZone::None {
                    self.comp.snap_rect = zone.target_rect(scr_w, work_top, work_h);
                }
                if let Some(sr) = prev_sr.or(self.comp.snap_rect) {
                    let merged = self
                        .comp
                        .snap_rect
                        .map_or(sr, |nr| sr.union(nr))
                        .expand(20);
                    self.comp.mark_dirty(merged);
                }
            }
        } else {
            let min_w = drag.min_w;
            let min_h = drag.min_h;
            let (mut nx, mut ny, mut nw, mut nh) =
                (drag.orig_x, drag.orig_y, drag.orig_w, drag.orig_h);
            match drag.mode {
                4 => nw = (drag.orig_w + dx).max(min_w),
                6 => nh = (drag.orig_h + dy).max(min_h),
                8 => {
                    nw = (drag.orig_w - dx).max(min_w);
                    nx = drag.orig_x + (drag.orig_w - nw);
                }
                2 => {
                    nh = (drag.orig_h - dy).max(min_h);
                    ny = (drag.orig_y + (drag.orig_h - nh)).max(work_top);
                }
                5 => {
                    nw = (drag.orig_w + dx).max(min_w);
                    nh = (drag.orig_h + dy).max(min_h);
                }
                7 => {
                    nw = (drag.orig_w - dx).max(min_w);
                    nx = drag.orig_x + (drag.orig_w - nw);
                    nh = (drag.orig_h + dy).max(min_h);
                }
                3 => {
                    nw = (drag.orig_w + dx).max(min_w);
                    nh = (drag.orig_h - dy).max(min_h);
                    ny = (drag.orig_y + (drag.orig_h - nh)).max(work_top);
                }
                1 => {
                    nw = (drag.orig_w - dx).max(min_w);
                    nx = drag.orig_x + (drag.orig_w - nw);
                    nh = (drag.orig_h - dy).max(min_h);
                    ny = (drag.orig_y + (drag.orig_h - nh)).max(work_top);
                }
                _ => {}
            }
            drag.pending_x = nx;
            drag.pending_y = ny;
            drag.pending_w = nw;
            drag.pending_h = nh;
            drag.has_pending = true;
        }
        self.wm_drag = Some(drag);
    }

    fn apply_pending_wm_drag(&mut self) -> Result<()> {
        let Some(ref mut drag) = self.wm_drag else {
            return Ok(());
        };
        if !drag.has_pending {
            return Ok(());
        }
        drag.has_pending = false;
        let (win, mode, nx, ny, nw, nh) = (
            drag.window,
            drag.mode,
            drag.pending_x,
            drag.pending_y,
            drag.pending_w,
            drag.pending_h,
        );
        let scr_w = i32::from(self.x.width);
        let work_h = i32::from(self.x.height) - self.panel_height();
        let is_vortex = self.theme.is_vortexglass();
        let mut dirty_union = None;
        let mut changed = false;

        if let Some(m) = self.windows.iter_mut().find(|w| w.window == win) {
            let old_rect = Rect::new(
                i32::from(m.x),
                i32::from(m.y),
                i32::from(m.width),
                i32::from(m.height),
            );
            let target_w = if mode == 0 {
                i32::from(m.width)
            } else {
                nw.max(1)
            };
            let target_h = if mode == 0 {
                i32::from(m.height)
            } else {
                nh.max(1)
            };
            let new_rect = Rect::new(nx, ny, target_w, target_h);
            if old_rect != new_rect {
                changed = true;
                m.x = nx as i16;
                m.y = ny as i16;
                m.width = target_w as u16;
                m.height = target_h as u16;
                if self.comp.active {
                    let old_max = old_rect.w >= scr_w - 2 && old_rect.h >= work_h - 2;
                    let new_max = new_rect.w >= scr_w - 2 && new_rect.h >= work_h - 2;
                    dirty_union = Some(
                        window_damage_bounds(old_rect, is_vortex, old_max)
                            .union(window_damage_bounds(new_rect, is_vortex, new_max)),
                    );
                }
            }
        }
        if changed {
            if mode == 0 {
                self.x.move_window(win, nx as i16, ny as i16)?;
            } else {
                self.x
                    .move_resize(win, nx as i16, ny as i16, nw as u16, nh as u16)?;
            }
            if let Some(d) = dirty_union {
                self.comp.mark_dirty(d);
            }
        }
        Ok(())
    }

    fn cancel_wm_drag(&mut self) {
        if self.wm_drag.take().is_some() {
            if self.pointer_grab == Some(self.x.root) {
                self.release_popup_grab();
            }
            if self.comp.snap_zone != SnapZone::None {
                self.comp.snap_zone = SnapZone::None;
                if let Some(sr) = self.comp.snap_rect {
                    self.comp.mark_dirty(sr.expand(20));
                }
            }
        }
    }

    fn finish_wm_drag(&mut self) -> Result<()> {
        self.apply_pending_wm_drag()?;
        let Some(drag) = self.wm_drag.take() else {
            return Ok(());
        };
        if self.pointer_grab == Some(self.x.root) {
            self.release_popup_grab();
        }

        if drag.mode == 0 {
            let zone = self.comp.snap_zone;
            self.comp.snap_zone = SnapZone::None;
            if let Some(sr) = self.comp.snap_rect {
                self.comp.mark_dirty(sr.expand(20));
            }
            if zone != SnapZone::None {
                let scr_w = i32::from(self.x.width);
                let (work_top, work_h) = self.work_area_vertical();
                let (sx, sy, sw, sh, zone_code) = match zone {
                    SnapZone::Maximize => (0i16, work_top as i16, scr_w as u16, work_h as u16, 3u32),
                    SnapZone::Left => {
                        let half = (scr_w / 2).max(200);
                        (0i16, work_top as i16, half as u16, work_h as u16, 1u32)
                    }
                    SnapZone::Right => {
                        let half = (scr_w / 2).max(200);
                        (
                            half as i16,
                            work_top as i16,
                            (scr_w - half).max(200) as u16,
                            work_h as u16,
                            2u32,
                        )
                    }
                    SnapZone::None => (0, 0, 0, 0, 0),
                };
                let packed_xy =
                    (u32::from(drag.orig_x as i16 as u16) << 16) | u32::from(drag.orig_y as i16 as u16);
                let packed_wh =
                    (u32::from(drag.orig_w as u16) << 16) | u32::from(drag.orig_h as u16);
                let _ = self.x.send_window_notify_data(
                    drag.window,
                    21,
                    zone_code,
                    packed_xy,
                    packed_wh,
                    0,
                );
                let is_vortex = self.theme.is_vortexglass();
                let mut snap_dirty = None;
                if let Some(m) = self.windows.iter_mut().find(|w| w.window == drag.window) {
                    let old_rect = Rect::new(
                        i32::from(m.x),
                        i32::from(m.y),
                        i32::from(m.width),
                        i32::from(m.height),
                    );
                    m.x = sx;
                    m.y = sy;
                    m.width = sw;
                    m.height = sh;
                    let new_rect = Rect::new(i32::from(sx), i32::from(sy), i32::from(sw), i32::from(sh));
                    if self.comp.active {
                        snap_dirty = Some(
                            window_damage_bounds(old_rect, is_vortex, false)
                                .union(window_damage_bounds(new_rect, is_vortex, zone_code == 3)),
                        );
                    }
                }
                let _ = self.x.move_resize(drag.window, sx, sy, sw, sh);
                if let Some(d) = snap_dirty {
                    self.comp.mark_dirty(d);
                }
            }
        }
        if self.comp.active {
            if matches!(
                self.config.wm_compositor_quality,
                CompositorQuality::Balanced | CompositorQuality::Quality
            ) {
                if let Some((r, is_max)) = self
                    .windows
                    .iter()
                    .find(|m| m.window == drag.window)
                    .map(|m| self.window_rect_for(m))
                {
                    self.comp.mark_dirty(window_damage_bounds(
                        r,
                        self.theme.is_vortexglass(),
                        is_max,
                    ));
                }
            }
            self.composite_frame()?;
        }
        self.x.flush()
    }

    fn publish_client_list(&self) -> Result<()> {
        let mut bytes = Vec::with_capacity(self.windows.len() * 4);
        for managed in &self.windows {
            bytes.extend_from_slice(&managed.window.to_ne_bytes());
        }
        self.x.conn.change_property(
            PropMode::REPLACE,
            self.x.root,
            self.x.atoms.net_client_list,
            AtomEnum::WINDOW,
            32,
            self.windows.len() as u32,
            &bytes,
        )?;
        Ok(())
    }

    fn publish_active(&self) -> Result<()> {
        let window = self.active.unwrap_or(x11rb::NONE);
        self.x.conn.change_property(
            PropMode::REPLACE,
            self.x.root,
            self.x.atoms.net_active_window,
            AtomEnum::WINDOW,
            32,
            1,
            &window.to_ne_bytes(),
        )?;
        Ok(())
    }

    fn raise_chrome_above_clients(&self) -> Result<()> {
        self.x.raise(self.taskbar)?;
        if self.comp.thumb.is_some() {
            let _ = self.x.raise(self.thumb_window);
        }
        if self.menu_open {
            let _ = self.x.raise(self.menu);
        }
        if self.ctx_open {
            let _ = self.x.raise(self.ctx_menu);
        }
        if self.vol_open {
            let _ = self.x.raise(self.vol_popup);
        }
        if self.toast.is_some() {
            let _ = self.x.raise(self.toast_window);
        }
        for ov in &self.override_wins {
            let _ = self.x.raise(ov.window);
        }
        Ok(())
    }

    fn raise_z(&mut self, window: Window) {
        let prev_top = self.z_stack.last().copied();
        self.z_stack.retain(|&w| w != window);
        self.z_stack.push(window);
        if self.comp.active {
            let is_vortex = self.theme.is_vortexglass();
            let mut dirty: Option<Rect> = None;
            for target in [Some(window), prev_top].into_iter().flatten() {
                if let Some((r, is_max)) = self
                    .windows
                    .iter()
                    .find(|m| m.window == target)
                    .map(|m| self.window_rect_for(m))
                {
                    let b = window_damage_bounds(r, is_vortex, is_max);
                    dirty = Some(dirty.map_or(b, |p| p.union(b)));
                }
            }
            if let Some(d) = dirty {
                self.comp.mark_dirty(d);
            } else {
                self.comp.mark_full_dirty();
            }
        }
    }

    fn thumb_bridge_contains(&self, rx: i32, ry: i32) -> bool {
        let Some(t) = self.comp.thumb.as_ref() else {
            return false;
        };
        let tb_y = self.taskbar_y();
        let panel_h = self.panel_height();
        let top = t.rect.y.min(tb_y);
        let bottom = t.rect.bottom().max(tb_y + panel_h);
        let bridge = Rect::new(t.rect.x - 14, top - 6, t.rect.w + 28, bottom - top + 12);
        bridge.contains(rx, ry)
    }

    fn open_thumb(&mut self, idx: usize) -> Result<()> {
        let Some(m) = self.windows.get(idx) else {
            return Ok(());
        };
        let win = m.window;
        let btn = self.window_button_rect(idx);
        let st = compute_thumb_popup(
            btn,
            i32::from(self.x.width),
            self.taskbar_y(),
            self.config.panel_position == PanelPosition::Top,
            self.panel_height(),
            idx,
            win,
        );
        let prev_rect = self.comp.thumb.as_ref().map(|t| t.rect);
        self.x.move_resize(
            self.thumb_window,
            st.rect.x as i16,
            st.rect.y as i16,
            st.rect.w as u16,
            st.rect.h as u16,
        )?;
        self.x.map(self.thumb_window)?;
        self.x.raise(self.thumb_window)?;
        self.comp.thumb_leave_at = None;
        let dirty = prev_rect.map_or(st.rect.expand(18), |p| p.union(st.rect).expand(18));
        self.comp.thumb = Some(st);
        self.comp.mark_dirty(dirty);
        Ok(())
    }

    fn close_thumb(&mut self) -> Result<()> {
        self.comp.thumb_hover_start = None;
        self.comp.thumb_leave_at = None;
        if let Some(prev) = self.comp.thumb.take() {
            self.x.unmap(self.thumb_window)?;
            self.comp.mark_dirty(prev.rect.expand(18));
        }
        Ok(())
    }

    fn acquire_switcher_grabs(&mut self) {
        if !self.kbd_grabbed {
            self.kbd_grabbed = self.x.grab_keyboard(self.desktop);
        }
        self.acquire_popup_grab(self.desktop);
    }

    fn release_switcher_grabs(&mut self) {
        if self.kbd_grabbed {
            self.x.ungrab_keyboard();
            self.kbd_grabbed = false;
        }
        if self.pointer_grab == Some(self.desktop) {
            self.release_popup_grab();
        }
    }

    fn open_or_cycle_alt_tab(&mut self, dir: i32) -> Result<()> {
        if self.windows.is_empty() {
            return Ok(());
        }
        self.close_menu()?;
        self.close_ctx()?;
        self.close_vol_popup()?;
        let _ = self.close_thumb();
        self.comp.flip3d = None;
        if self.comp.alt_tab.is_none() {
            let start = self
                .active
                .and_then(|a| self.windows.iter().position(|w| w.window == a))
                .map(|i| (i + 1) % self.windows.len())
                .unwrap_or(0);
            self.comp.alt_tab = Some(AltTabState {
                selected: start,
                opened: Instant::now(),
            });
            self.acquire_switcher_grabs();
        } else {
            self.cycle_switcher(dir);
        }
        self.comp.mark_full_dirty();
        Ok(())
    }

    fn open_or_cycle_flip3d(&mut self, dir: i32) -> Result<()> {
        if self.windows.is_empty() {
            return Ok(());
        }
        self.close_menu()?;
        self.close_ctx()?;
        self.close_vol_popup()?;
        let _ = self.close_thumb();
        self.comp.alt_tab = None;
        if self.comp.flip3d.is_none() {
            let next = if self.windows.len() > 1 { 1 } else { 0 };
            self.comp.flip3d = Some(Flip3dState {
                selected: next,
                scroll: 0.0,
                target_scroll: next as f32,
                opened: Instant::now(),
            });
            self.acquire_switcher_grabs();
        } else {
            self.cycle_switcher(dir);
        }
        self.comp.mark_full_dirty();
        Ok(())
    }

    fn cycle_switcher(&mut self, dir: i32) {
        let count = self.windows.len();
        if count == 0 {
            return;
        }
        if let Some(st) = self.comp.alt_tab.as_mut() {
            st.selected = ((st.selected as i32 + dir).rem_euclid(count as i32)) as usize;
        } else if let Some(st) = self.comp.flip3d.as_mut() {
            st.target_scroll += dir as f32;
            st.selected = ((st.target_scroll.round() as i32).rem_euclid(count as i32)) as usize;
        }
        self.comp.mark_full_dirty();
    }

    fn commit_switcher(&mut self) -> Result<()> {
        let sel = if let Some(st) = self.comp.alt_tab.take() {
            Some(st.selected)
        } else {
            self.comp.flip3d.take().map(|st| st.selected)
        };
        self.release_switcher_grabs();
        self.comp.mark_full_dirty();
        if let Some(idx) = sel {
            if let Some(win) = self.windows.get(idx).map(|m| m.window) {
                self.activate(win)?;
            }
        }
        Ok(())
    }

    fn cancel_switcher(&mut self) -> Result<()> {
        self.comp.alt_tab = None;
        self.comp.flip3d = None;
        self.release_switcher_grabs();
        self.comp.mark_full_dirty();
        Ok(())
    }

    fn toggle_show_desktop(&mut self) -> Result<()> {
        let any_visible = self.windows.iter().any(|w| !w.minimized);
        if any_visible {
            self.all_minimized_backup.clear();
            let list: Vec<Window> = self
                .windows
                .iter()
                .filter(|w| !w.minimized)
                .map(|w| w.window)
                .collect();
            for win in list {
                self.all_minimized_backup.push(win);
                self.minimize_window(win)?;
            }
        } else if !self.all_minimized_backup.is_empty() {
            let restore = std::mem::take(&mut self.all_minimized_backup);
            for win in restore {
                self.activate(win)?;
            }
        }
        Ok(())
    }

    fn activate(&mut self, window: Window) -> Result<()> {
        self.close_vol_popup()?;
        self.close_ctx()?;
        let Some(win_idx) = self.windows.iter().position(|w| w.window == window) else {
            return Ok(());
        };
        let was_min = self.windows[win_idx].minimized;
        if was_min {
            self.windows[win_idx].minimized = false;
            self.x.map(window)?;
            if self.comp.active {
                let btn_local = self.window_button_rect(win_idx);
                let btn_screen =
                    Rect::new(btn_local.x, self.taskbar_y() + btn_local.y, btn_local.w, btn_local.h);
                if let Some(cw) = self.comps.get_mut(&window) {
                    let mw = self.windows[win_idx].width;
                    let mh = self.windows[win_idx].height;
                    let d = cw.depth;
                    attach_window_pixmap(&self.x, cw, mw, mh, d);
                    cw.anim = Some(WindowAnim {
                        kind: AnimKind::Restore {
                            taskbar_btn: btn_screen,
                        },
                        start: Instant::now(),
                        duration_ms: 220,
                    });
                }
            }
        }
        let prev_active = self.active;
        self.x.raise(window)?;
        self.raise_z(window);
        self.raise_chrome_above_clients()?;
        self.x.focus(window)?;
        self.active = Some(window);
        if self.comp.active {
            if let Some(prev) = prev_active.filter(|&p| p != window) {
                self.mark_window_dirty(prev);
            }
            self.mark_window_dirty(window);
        }
        self.publish_active()?;
        self.draw_taskbar();
        self.present_taskbar()?;
        Ok(())
    }

    fn desktop_hit(&self, x: i32, y: i32) -> Hit {
        if !self.config.desktop_icons_visible {
            return Hit::None;
        }
        for idx in 0..APPS.len() {
            if self.desktop_icon_rect(idx).contains(x, y) {
                return Hit::DesktopIcon(idx);
            }
        }
        Hit::None
    }

    fn window_button_rect(&self, index: usize) -> Rect {
        self.taskbar_layout()
            .window_button_rect(self.panel_height(), self.windows.len(), index)
    }

    fn ime_rect(&self) -> Rect {
        self.taskbar_layout().ime
    }

    fn net_rect(&self) -> Rect {
        self.taskbar_layout().net
    }

    fn vol_rect(&self) -> Rect {
        self.taskbar_layout().volume
    }

    fn bell_rect(&self) -> Rect {
        self.taskbar_layout().bell
    }

    fn taskbar_hit(&self, x: i32, y: i32) -> Hit {
        if self.start_button_rect().contains(x, y) {
            return Hit::Start;
        }
        if self.config.panel_show_pinned {
            for i in 0..PINNED_MODES.len() {
                if self.pinned_rect(i).contains(x, y) {
                    return Hit::Pinned(i);
                }
            }
        }
        for index in 0..self.windows.len().min(6) {
            if self.window_button_rect(index).contains(x, y) {
                return Hit::Window(index);
            }
        }
        if self.ime_rect().contains(x, y) {
            return Hit::ImeTray;
        }
        if self.net_rect().contains(x, y) {
            return Hit::Network;
        }
        if self.vol_rect().contains(x, y) {
            return Hit::Volume;
        }
        if self.bell_rect().contains(x, y) {
            return Hit::Bell;
        }
        if self.theme.is_vortexglass() && self.show_desktop_rect().contains(x, y) {
            return Hit::ShowDesktop;
        }
        Hit::None
    }

    fn taskbar_click(&mut self, x: i32, y: i32) -> Result<()> {
        match self.taskbar_hit(x, y) {
            Hit::Start => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.toggle_menu()
            }
            Hit::Pinned(idx) => {
                self.close_menu()?;
                self.close_vol_popup()?;
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                if let Some(app) = pinned_app_entry(idx) {
                    self.launch(app.mode);
                }
                Ok(())
            }
            Hit::Window(index) => {
                self.close_menu()?;
                self.close_vol_popup()?;
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                if let Some(managed) = self.windows.get(index) {
                    let window = managed.window;
                    let is_min = managed.minimized;
                    if !is_min
                        && self.active == Some(window)
                        && self.z_stack.last().copied() == Some(window)
                    {
                        self.minimize_window(window)?;
                    } else {
                        self.activate(window)?;
                    }
                }
                Ok(())
            }
            Hit::ShowDesktop => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.toggle_show_desktop()
            }
            Hit::ImeTray => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.close_menu()?;
                self.close_vol_popup()?;
                self.toggle_ime_from_tray();
                Ok(())
            }
            Hit::Network => {
                self.close_menu()?;
                self.close_vol_popup()?;
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                let _ = self.refresh_net_status();
                let st = &self.net_status;
                if let Some((ip, prefix)) = st.ipv4.filter(|_| st.up && st.carrier) {
                    let gw = st
                        .gateway
                        .map(|g| g.to_string())
                        .unwrap_or_else(|| "无".to_string());
                    let rx = crate::netmgr::NetStatus::format_bytes(st.rx_bytes);
                    let tx = crate::netmgr::NetStatus::format_bytes(st.tx_bytes);
                    let title = format!("有线网络 · {} (已连接)", st.ifname);
                    let body = format!("{ip}/{prefix} · 网关 {gw} · ↓{rx} ↑{tx}");
                    self.show_toast(&title, &body);
                } else {
                    let title = if st.has_device {
                        format!("有线网络 · {}", st.ifname)
                    } else {
                        "有线网络".to_string()
                    };
                    let body = format!("当前状态：{} · 点击配置网络", st.panel_label());
                    self.show_toast(&title, &body);
                }
                self.launch_arg("settings", "network");
                Ok(())
            }
            Hit::Volume => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.toggle_vol_popup()
            }
            Hit::Bell => {
                self.close_menu()?;
                self.close_vol_popup()?;
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.show_toast("系统通知", "当前没有未读系统通知");
                Ok(())
            }
            _ => {
                self.close_menu()?;
                self.close_vol_popup()?;
                Ok(())
            }
        }
    }

    fn acquire_popup_grab(&mut self, window: Window) -> bool {
        if self.pointer_grab == Some(window) {
            return true;
        }
        if self.pointer_grab.is_some() {
            self.release_popup_grab();
        }
        let grabbed = self.x.grab_pointer(window);
        self.pointer_grab = transition_popup_grab(self.pointer_grab, window, grabbed);
        grabbed
    }

    fn release_popup_grab(&mut self) {
        if self.pointer_grab.take().is_some() {
            self.x.ungrab_pointer();
        }
    }

    fn persist_config_prop(&mut self, channel: &str, prop: &str, val: uxconf::Value) -> bool {
        let _ = self.config.apply_property(channel, prop, &val);
        if self.ux_client.is_none() {
            self.ux_client = UxConfClient::connect().ok();
        }
        let Some(client) = self.ux_client.as_mut() else {
            return false;
        };
        client.set(channel, prop, val).is_ok()
    }

    fn persist_sound_prop(&mut self, prop: &str, val: uxconf::Value) -> bool {
        self.persist_config_prop("sound", prop, val)
    }

    fn toggle_ime_from_tray(&mut self) {
        if !self.config.ime_enabled {
            self.persist_config_prop("input", "/ime/enabled", uxconf::Value::Bool(true));
            self.ime_active = true;
            self.ime_chinese = self.ime_client.set_chinese_mode(true).unwrap_or(true);
            return;
        }
        let next = self.ime_client.toggle_mode().unwrap_or(!self.ime_chinese);
        self.ime_chinese = next;
    }

    fn sync_menu_ime_spot(&mut self, caret: Rect) {
        if !self.menu_open {
            return;
        }
        let (mx, my) = if self.config.panel_start_fullscreen {
            (0, 0)
        } else {
            (self.menu_x(), self.menu_y())
        };
        let rx = mx + caret.x;
        let ry = my + caret.y;
        let h = caret.h.max(1);
        if self.menu_ime_spot == Some((rx, ry, h)) {
            return;
        }
        self.menu_ime_spot = Some((rx, ry, h));
        self.ime_client.set_spot(rx, ry, h);
    }

    fn apply_menu_ime_reply(&mut self, reply: DaemonReply) -> Result<()> {
        if self.ime_chinese != reply.chinese_mode {
            self.ime_chinese = reply.chinese_mode;
            self.draw_taskbar();
            self.present_taskbar()?;
        }
        if !self.menu_open {
            return Ok(());
        }
        if self.config.panel_start_fullscreen {
            let mut dirty = self.start_screen.on_preedit(&reply.preedit.text);
            if !reply.commit_text.is_empty() {
                let action = self.start_screen.on_commit(&reply.commit_text);
                if matches!(action, StartAction::Redraw) {
                    dirty = true;
                }
            }
            if dirty {
                self.draw_menu();
                self.present_menu()?;
            }
        } else {
            let mut dirty = false;
            if reply.preedit.text != self.menu_ime_preedit {
                self.menu_ime_preedit = reply.preedit.text;
                dirty = true;
            }
            if !reply.commit_text.is_empty() {
                for ch in reply.commit_text.chars() {
                    if !ch.is_control() && self.menu_query.chars().count() < 24 {
                        self.menu_query.push(ch);
                        dirty = true;
                    }
                }
            }
            if dirty {
                if !self.menu_ime_preedit.is_empty() {
                    if self.pointer_grab == Some(self.menu) {
                        self.release_popup_grab();
                    }
                } else if self.menu_open && !self.config.panel_start_fullscreen {
                    let _ = self.acquire_popup_grab(self.menu);
                }
                self.hover = Hit::None;
                self.draw_menu();
                self.present_menu()?;
            }
        }
        Ok(())
    }

    fn stage_sound_persist(&mut self) {
        self.vol_persist
            .stage(SoundSnapshot::from_config(&self.config), Instant::now());
    }

    /// Persist staged volume/mute/sfx changes at most once per debounce burst or popup close.
    fn flush_volume_persist(&mut self) {
        let current = SoundSnapshot::from_config(&self.config);
        let Some(props) = self.vol_persist.take_pending_changes(current) else {
            return;
        };
        for (prop, val) in props {
            if !self.persist_sound_prop(prop, val) {
                break;
            }
        }
    }

    fn set_master_volume(&mut self, vol: i32, play_sfx: bool) -> Result<()> {
        let clamped = vol.clamp(0, 100);
        let prev_vol = self.config.sound_master_volume;
        let prev_muted = self.config.sound_muted;
        if clamped != prev_vol {
            let value = uxconf::Value::Int(i64::from(clamped));
            let _ = self
                .config
                .apply_property("sound", "/master-volume", &value);
        }
        if self.config.sound_muted && clamped > 0 {
            let _ = self
                .config
                .apply_property("sound", "/muted", &uxconf::Value::Bool(false));
        }
        if clamped != prev_vol || self.config.sound_muted != prev_muted {
            self.stage_sound_persist();
        }
        crate::sound::apply_mixer_volume(clamped, self.config.sound_muted && clamped == 0);
        if play_sfx {
            crate::sound::play(&self.config, crate::sound::SystemSound::Volume);
        }
        if clamped != prev_vol || self.config.sound_muted != prev_muted {
            self.draw_taskbar();
            self.present_taskbar()?;
        }
        if self.vol_open {
            self.draw_vol_popup();
            self.present_vol_popup()?;
        }
        Ok(())
    }

    fn adjust_volume_step(&mut self, delta_pct: i32) -> Result<()> {
        let next = (self.config.sound_master_volume + delta_pct).clamp(0, 100);
        self.set_master_volume(next, true)
    }

    fn toggle_mute(&mut self) -> Result<()> {
        let next = !self.config.sound_muted;
        let _ = self
            .config
            .apply_property("sound", "/muted", &uxconf::Value::Bool(next));
        self.stage_sound_persist();
        crate::sound::apply_mixer_volume(self.config.sound_master_volume, next);
        if !next {
            crate::sound::play(&self.config, crate::sound::SystemSound::Volume);
        }
        self.draw_taskbar();
        self.present_taskbar()?;
        if self.vol_open {
            self.draw_vol_popup();
            self.present_vol_popup()?;
        }
        Ok(())
    }

    fn taskbar_local_coords(&self, root_x: i16, root_y: i16) -> Option<(i32, i32)> {
        taskbar_local_point(
            i32::from(self.x.width),
            self.taskbar_y(),
            self.panel_height(),
            i32::from(root_x),
            i32::from(root_y),
        )
    }

    fn vol_popup_pos(&self) -> (i16, i16) {
        let vr = self.vol_rect();
        let x = (vr.right() + 12 - VOL_POPUP_WIDTH)
            .clamp(8, (i32::from(self.x.width) - VOL_POPUP_WIDTH - 8).max(8));
        let y = if self.config.panel_position == PanelPosition::Top {
            self.panel_height() + 6
        } else {
            (self.taskbar_y() - VOL_POPUP_HEIGHT - 6).max(8)
        };
        (x as i16, y as i16)
    }

    fn open_vol_popup(&mut self) -> Result<()> {
        self.close_menu()?;
        self.close_ctx()?;
        let (vx, vy) = self.vol_popup_pos();
        self.x.move_resize(
            self.vol_popup,
            vx,
            vy,
            VOL_POPUP_WIDTH as u16,
            VOL_POPUP_HEIGHT as u16,
        )?;
        self.vol_open = true;
        self.vol_dragging = false;
        self.hover = Hit::None;
        self.draw_vol_popup();
        self.draw_taskbar();
        self.x.map(self.vol_popup)?;
        self.x.raise(self.vol_popup)?;
        self.present_vol_popup()?;
        self.present_taskbar()?;
        let _ = self.x.focus(self.vol_popup);
        let _ = self.acquire_popup_grab(self.vol_popup);
        self.comp.mark_full_dirty();
        Ok(())
    }

    fn close_vol_popup(&mut self) -> Result<()> {
        if self.vol_open {
            // Release pointer grab and unmap visually BEFORE flushing uxconfd I/O so
            // popup dismissal is instantaneous and never bricks input on IPC stall.
            self.release_popup_grab();
            self.vol_open = false;
            self.vol_dragging = false;
            if matches!(
                self.hover,
                Hit::VolMute | Hit::VolSlider | Hit::VolSfx | Hit::VolSettings
            ) {
                self.hover = Hit::None;
            }
            self.x.unmap(self.vol_popup)?;
            let target = self.active.unwrap_or(self.x.root);
            let _ = self.x.focus(target);
            self.draw_taskbar();
            self.present_taskbar()?;
            self.comp.mark_full_dirty();
            let _ = self.x.flush();
            self.flush_volume_persist();
        }
        Ok(())
    }

    fn toggle_vol_popup(&mut self) -> Result<()> {
        if self.vol_open {
            self.close_vol_popup()
        } else {
            self.open_vol_popup()
        }
    }

    fn vol_popup_hit(&self, x: i32, y: i32) -> Hit {
        if vol_popup_mute_rect().contains(x, y) {
            Hit::VolMute
        } else if vol_popup_slider_rect().contains(x, y) {
            Hit::VolSlider
        } else if vol_popup_sfx_rect().contains(x, y) {
            Hit::VolSfx
        } else if vol_popup_settings_rect().contains(x, y) {
            Hit::VolSettings
        } else {
            Hit::None
        }
    }

    fn vol_popup_click(&mut self, x: i32, y: i32) -> Result<()> {
        match self.vol_popup_hit(x, y) {
            Hit::VolMute => self.toggle_mute(),
            Hit::VolSlider => {
                self.vol_dragging = true;
                let vol = vol_from_slider_x(vol_popup_slider_rect(), x);
                self.set_master_volume(vol, false)
            }
            Hit::VolSfx => {
                let next = !self.config.sound_enabled;
                let _ = self.config.apply_property(
                    "sound",
                    "/system-sounds-enabled",
                    &uxconf::Value::Bool(next),
                );
                self.stage_sound_persist();
                if next {
                    crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                }
                self.draw_vol_popup();
                self.present_vol_popup()
            }
            Hit::VolSettings => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.close_vol_popup()?;
                self.launch_arg("settings", "sound");
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn menu_hit(&self, x: i32, y: i32) -> Hit {
        if let Some(ref sm) = self.theme.skin().and_then(|s| s.start_menu.as_ref()) {
            if sm.layout == crate::theme::StartMenuLayoutKind::TwoColumn {
                let (logout, reboot, power_off, confirm_cancel, confirm_ok) =
                    custom_menu_footer_rects(sm);
                if self.menu_confirm_power.is_some() {
                    if confirm_cancel.contains(x, y) {
                        return Hit::ConfirmPowerCancel;
                    }
                    if confirm_ok.contains(x, y) {
                        return Hit::ConfirmPowerOk;
                    }
                    return Hit::None;
                }
                let filtered = self.filtered_apps();
                for (slot, &app_idx) in filtered.iter().take(7).enumerate() {
                    if custom_menu_row_rect(sm, slot).contains(x, y) {
                        return Hit::MenuRow(app_idx);
                    }
                }
                for (i, _) in VORTEX_PLACES.iter().enumerate() {
                    if custom_menu_place_rect(sm, i).contains(x, y) {
                        return Hit::MenuPlace(i);
                    }
                }
                if logout.contains(x, y) {
                    return Hit::Logout;
                }
                if reboot.contains(x, y) {
                    return Hit::Reboot;
                }
                if power_off.contains(x, y) {
                    return Hit::PowerOff;
                }
                return Hit::None;
            }
        }
        let is_vortex = self.theme.is_vortexglass();
        let (logout, reboot, power_off, confirm_cancel, confirm_ok) = if is_vortex {
            vortex_menu_footer_rects()
        } else {
            compact_footer_rects()
        };
        if self.menu_confirm_power.is_some() {
            if confirm_cancel.contains(x, y) {
                return Hit::ConfirmPowerCancel;
            }
            if confirm_ok.contains(x, y) {
                return Hit::ConfirmPowerOk;
            }
            return Hit::None;
        }
        let filtered = self.filtered_apps();
        if is_vortex {
            for (slot, &app_idx) in filtered.iter().take(8).enumerate() {
                if vortex_menu_row_rect(slot).contains(x, y) {
                    return Hit::MenuRow(app_idx);
                }
            }
            for (i, _) in VORTEX_PLACES.iter().enumerate() {
                if vortex_menu_place_rect(i).contains(x, y) {
                    return Hit::MenuPlace(i);
                }
            }
        } else {
            for (slot, &app_idx) in filtered.iter().enumerate() {
                if menu_row_rect(slot).contains(x, y) {
                    return Hit::MenuRow(app_idx);
                }
            }
        }
        if logout.contains(x, y) {
            return Hit::Logout;
        }
        if reboot.contains(x, y) {
            return Hit::Reboot;
        }
        if power_off.contains(x, y) {
            return Hit::PowerOff;
        }
        Hit::None
    }

    fn menu_click(&mut self, x: i32, y: i32) -> Result<()> {
        match self.menu_hit(x, y) {
            Hit::MenuRow(app_idx) => {
                let mode = APPS[app_idx].mode;
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.close_menu()?;
                self.launch(mode);
                Ok(())
            }
            Hit::MenuPlace(idx) => {
                if let Some(p) = VORTEX_PLACES.get(idx) {
                    crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                    self.close_menu()?;
                    match p.arg {
                        Some(a) => self.launch_arg(p.mode, a),
                        None => self.launch(p.mode),
                    }
                }
                Ok(())
            }
            Hit::Logout => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.close_menu()?;
                // DDM owns the session.  Exiting the shell makes the daemon
                // tear down the whole process group and show the greeter again.
                std::process::exit(0);
            }
            Hit::Reboot => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.menu_confirm_power = Some(PowerAction::Reboot);
                self.draw_menu();
                self.present_menu()
            }
            Hit::PowerOff => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.menu_confirm_power = Some(PowerAction::PowerOff);
                self.draw_menu();
                self.present_menu()
            }
            Hit::ConfirmPowerOk => {
                if let Some(action) = self.menu_confirm_power.take() {
                    crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                    self.execute_power_action(action)?;
                }
                Ok(())
            }
            Hit::ConfirmPowerCancel => {
                crate::sound::play(&self.config, crate::sound::SystemSound::Click);
                self.menu_confirm_power = None;
                self.draw_menu();
                self.present_menu()
            }
            _ => Ok(()),
        }
    }

    fn open_ctx(&mut self, root_x: i32, root_y: i32) -> Result<()> {
        self.open_ctx_mode(root_x, root_y, false)
    }

    fn open_ctx_mode(&mut self, root_x: i32, root_y: i32, is_ime: bool) -> Result<()> {
        self.close_menu()?;
        self.close_vol_popup()?;
        let ph = self.panel_height();
        let min_y = if self.config.panel_position == PanelPosition::Top {
            ph + 4
        } else {
            4
        };
        let max_y = if self.config.panel_position == PanelPosition::Top {
            (i32::from(self.x.height) - CTX_HEIGHT - 4).max(min_y)
        } else {
            (i32::from(self.x.height) - ph - CTX_HEIGHT - 4).max(min_y)
        };
        let max_x = (i32::from(self.x.width) - CTX_WIDTH - 4).max(4);
        let cx = root_x.clamp(4, max_x) as i16;
        let cy = root_y.clamp(min_y, max_y) as i16;
        self.ctx_is_ime = is_ime;
        self.ctx_open = true;
        self.ctx_pos = (cx, cy);
        self.hover = Hit::None;
        self.x
            .move_resize(self.ctx_menu, cx, cy, CTX_WIDTH as u16, CTX_HEIGHT as u16)?;
        self.draw_ctx();
        self.x.map(self.ctx_menu)?;
        self.x.raise(self.ctx_menu)?;
        self.present_ctx()?;
        let _ = self.x.focus(self.ctx_menu);
        let _ = self.acquire_popup_grab(self.ctx_menu);
        self.comp.mark_full_dirty();
        Ok(())
    }

    fn close_ctx(&mut self) -> Result<()> {
        if self.ctx_open {
            self.release_popup_grab();
            self.ctx_open = false;
            self.x.unmap(self.ctx_menu)?;
            let target = self.active.unwrap_or(self.x.root);
            let _ = self.x.focus(target);
            self.comp.mark_full_dirty();
            self.x.flush()?;
        }
        Ok(())
    }

    fn ctx_hit(&self, x: i32, y: i32) -> Hit {
        for idx in 0..4 {
            if ctx_row_rect(idx).contains(x, y) {
                return Hit::ContextRow(idx);
            }
        }
        Hit::None
    }

    fn ctx_click(&mut self, x: i32, y: i32) -> Result<()> {
        let hit = self.ctx_hit(x, y);
        let is_ime = self.ctx_is_ime;
        self.close_ctx()?;
        if hit != Hit::None {
            crate::sound::play(&self.config, crate::sound::SystemSound::Click);
        }
        if is_ime {
            match hit {
                Hit::ContextRow(0) => {
                    self.toggle_ime_from_tray();
                    self.draw_taskbar();
                    self.present_taskbar()?;
                }
                Hit::ContextRow(1) => {
                    let next = next_ime_scheme(&self.config.ime_scheme);
                    self.persist_config_prop(
                        "input",
                        "/ime/scheme",
                        uxconf::Value::String(next.to_string()),
                    );
                }
                Hit::ContextRow(2) => {
                    let next = if self.config.ime_layout == "vertical" {
                        "horizontal"
                    } else {
                        "vertical"
                    };
                    self.persist_config_prop(
                        "input",
                        "/ime/layout",
                        uxconf::Value::String(next.to_string()),
                    );
                }
                Hit::ContextRow(3) => self.launch_arg("settings", "input"),
                _ => {}
            }
            return Ok(());
        }
        match hit {
            Hit::ContextRow(0) => self.launch("term"),
            Hit::ContextRow(1) => self.launch("files"),
            Hit::ContextRow(2) => {
                self.reload_wallpaper(true);
                self.draw_desktop();
                self.present_desktop()?;
            }
            Hit::ContextRow(3) => self.launch("settings"),
            _ => {}
        }
        Ok(())
    }

    fn toggle_menu(&mut self) -> Result<()> {
        if self.menu_open {
            self.close_menu()
        } else {
            self.close_ctx()?;
            self.close_vol_popup()?;
            self.menu_open = true;
            self.menu_query.clear();
            self.menu_ime_preedit.clear();
            self.menu_ime_spot = None;
            self.hover = Hit::None;
            self.sync_menu_window()?;
            if self.config.panel_start_fullscreen {
                self.start_screen.on_open();
            }
            self.draw_menu();
            self.draw_taskbar();
            self.x.map(self.menu)?;
            self.x.raise(self.menu)?;
            self.present_menu()?;
            self.present_taskbar()?;
            self.x.focus(self.menu)?;
            self.menu_ime_focused = !self.config.panel_start_fullscreen || self.start_screen.wants_ime();
            if self.menu_ime_focused {
                self.ime_client.focus_in(self.menu);
                if let Some((rx, ry, h)) = self.menu_ime_spot {
                    self.ime_client.set_spot(rx, ry, h);
                }
            }
            if !self.config.panel_start_fullscreen {
                let _ = self.acquire_popup_grab(self.menu);
            }
            self.comp.mark_full_dirty();
            Ok(())
        }
    }

    fn close_menu(&mut self) -> Result<()> {
        if self.menu_open {
            if self.menu_ime_focused {
                self.menu_ime_focused = false;
                self.ime_client.focus_out(self.menu);
            }
            self.menu_ime_preedit.clear();
            self.menu_ime_spot = None;
            if self.pointer_grab == Some(self.menu) {
                self.release_popup_grab();
            }
            self.menu_open = false;
            self.menu_query.clear();
            self.menu_confirm_power = None;
            self.x.unmap(self.menu)?;
            let target = self.active.unwrap_or(self.x.root);
            let _ = self.x.focus(target);
            self.draw_taskbar();
            self.present_taskbar()?;
            self.comp.mark_full_dirty();
        }
        Ok(())
    }

    fn launch(&mut self, mode: &'static str) {
        self.launch_inner(mode, None);
    }

    fn launch_arg(&mut self, mode: &'static str, arg: &str) {
        self.launch_inner(mode, Some(arg));
    }

    fn launch_inner(&mut self, mode: &'static str, extra_arg: Option<&str>) {
        let now = Instant::now();
        if let Some((prev_mode, ts)) = self.last_launch {
            if prev_mode == mode && now.duration_since(ts) < Duration::from_millis(350) {
                return;
            }
        }
        self.last_launch = Some((mode, now));
        let mut cmd = if mode == "browser" {
            let sibling = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("space-browser")))
                .filter(|p| p.is_file());
            if let Some(bin) = sibling {
                Command::new(bin)
            } else {
                Command::new("/usr/bin/space-browser")
            }
        } else {
            let Ok(path) = std::env::current_exe() else {
                return;
            };
            let mut c = Command::new(path);
            c.arg(mode);
            c
        };
        if let Some(arg) = extra_arg {
            cmd.arg(arg);
        }
        let _ = cmd.spawn();
    }

    fn show_toast(&mut self, title: &str, body: &str) {
        self.toast = Some(Toast {
            title: title.to_string(),
            body: body.to_string(),
            deadline: Instant::now() + Duration::from_secs(4),
        });
        crate::sound::play(&self.config, crate::sound::SystemSound::Notify);
        self.draw_toast();
        let _ = self.x.map(self.toast_window);
        let _ = self.x.raise(self.toast_window);
        let _ = self.present_toast();
        self.comp.mark_full_dirty();
    }

    fn hide_toast(&mut self) -> Result<()> {
        self.toast = None;
        self.x.unmap(self.toast_window)?;
        self.comp.mark_full_dirty();
        self.x.flush()?;
        Ok(())
    }

    fn draw_desktop(&mut self) {
        let w = self.desktop_canvas.width as i32;
        let h = self.desktop_canvas.height as i32;
        if self.config.backdrop_style == BackdropStyle::Solid {
            self.desktop_canvas.clear(self.config.solid_color);
        } else if self.wallpaper_scaled.len() == (w * h) as usize {
            self.desktop_canvas.copy_background(&self.wallpaper_scaled);
        } else {
            self.desktop_canvas.vertical_gradient(
                Rect::new(0, 0, w, h),
                self.theme.bg_top,
                self.theme.bg_bottom,
            );
        }

        if !self.config.desktop_icons_visible {
            return;
        }

        let icon_sz = self.config.desktop_icon_size.clamp(32, 48) as u32;
        for (idx, app) in APPS.iter().enumerate() {
            let cell = self.desktop_icon_rect(idx);
            let hovered = self.hover == Hit::DesktopIcon(idx);
            let selected = self.selected_desktop_icon == Some(idx);
            if selected || hovered {
                if self.theme.is_win98() {
                    self.desktop_canvas.fill(
                        cell,
                        if selected {
                            self.theme.selection_bg
                        } else {
                            crate::draw::mix(self.config.solid_color, self.theme.selection_bg, 0.35)
                        },
                    );
                    if selected {
                        self.desktop_canvas.border_1px(cell, 0xffff80);
                    }
                } else {
                    self.desktop_canvas.rounded_rect(
                        cell,
                        6,
                        0xffffff,
                        if selected { 0.22 } else { 0.13 },
                    );
                    self.desktop_canvas.rounded_border(
                        cell,
                        6,
                        0xffffff,
                        if selected { 0.42 } else { 0.22 },
                        1,
                    );
                }
            }
            icons::draw(
                &mut self.desktop_canvas,
                app.icon,
                cell.x + (cell.w - icon_sz as i32) / 2,
                cell.y + 8,
                icon_sz,
                None,
            );
            let cx = cell.x as f32 + cell.w as f32 * 0.5;
            let by = (cell.y + icon_sz as i32 + 28) as f32;
            self.text.draw_centered(
                &mut self.desktop_canvas,
                app.name,
                cx + 1.0,
                by + 1.0,
                11.5,
                0x000000,
                Face::Ui,
            );
            self.text.draw_centered(
                &mut self.desktop_canvas,
                app.name,
                cx,
                by,
                11.5,
                0xffffff,
                Face::Ui,
            );
        }
    }

    fn superbar_glow_tint(icon: Icon, fallback_accent: u32) -> u32 {
        match icon {
            Icon::Files | Icon::Browser => 0x56a8f5,
            Icon::Terminal => 0x62c4e8,
            Icon::Monitor | Icon::Minesweeper => 0x4fd18b,
            Icon::Editor => 0xf2a74b,
            Icon::Music => 0xe86b86,
            Icon::Calculator => 0x7ab8e8,
            Icon::ImageViewer => 0x9b7ee8,
            _ => fallback_accent,
        }
    }

    fn draw_taskbar(&mut self) {
        let w = self.taskbar_canvas.width as i32;
        let h = self.panel_height();
        let show_pinned = self.config.panel_show_pinned;
        let is_vortex = self.theme.is_vortexglass();
        let skin_opt = self.theme.skin();
        let layout =
            TaskbarLayout::compute_for_theme(&self.theme, w, h, self.tray_width(), show_pinned);
        let start = layout.start;
        let pinned_rects: [Rect; PINNED_MODES.len()] =
            std::array::from_fn(|i| layout.pinned_rect(h, i));
        let win_count = self.windows.len().min(6);
        let win_rects: Vec<Rect> = (0..win_count)
            .map(|i| layout.window_button_rect(h, win_count, i))
            .collect();
        let ime = layout.ime;
        let net = layout.net;
        let vol = layout.volume;
        let bell = layout.bell;
        let is_win98 = self.theme.is_win98();
        let ctrl_r = self.theme.radius_control();
        let border_y = if self.config.panel_position == PanelPosition::Top {
            h - 1
        } else {
            0
        };

        let canvas = &mut self.taskbar_canvas;
        if let Some(skin) = skin_opt {
            skin.taskbar.bg.paint(canvas, Rect::new(0, 0, w, h), [0; 4]);
        } else if is_vortex {
            if canvas.is_argb() {
                canvas.clear_transparent(Rect::new(0, 0, w, h), self.theme.panel);
            } else {
                canvas.fill_gradient_v(
                    Rect::new(0, 0, w, h),
                    mix(self.theme.panel, self.theme.accent, 0.22),
                    mix(self.theme.panel, 0x0b1624, 0.25),
                    1.0,
                    1.0,
                );
            }
            let rim_y = if self.config.panel_position == PanelPosition::Top {
                h - 1
            } else {
                0
            };
            let hi_y = if self.config.panel_position == PanelPosition::Top {
                h - 2
            } else {
                1
            };
            canvas.fill_alpha(Rect::new(0, rim_y, w, 1), 0x0a1420, 0.55);
            canvas.fill_alpha(Rect::new(0, hi_y, w, 1), 0xffffff, 0.42);
        } else {
            canvas.fill(Rect::new(0, 0, w, h), self.theme.panel);
            if is_win98 {
                if self.config.panel_position == PanelPosition::Top {
                    canvas.fill(Rect::new(0, h - 2, w, 1), self.theme.bevel_shadow);
                    canvas.fill(Rect::new(0, h - 1, w, 1), self.theme.bevel_dark);
                } else {
                    canvas.fill(Rect::new(0, 0, w, 1), self.theme.bevel_light);
                    canvas.fill(Rect::new(0, 1, w, 1), self.theme.bevel_hi);
                }
            } else {
                canvas.fill(Rect::new(0, border_y, w, 1), self.theme.panel_border);
            }
        }

        // Start button (Declarative skin, SpaceOS Starlight Crystal Orb in VortexGlass, or pill/bevel).
        let start_pressed = self.menu_open || ui::is_rect_pressed(start);
        let start_active = start_pressed || self.hover == Hit::Start;
        let start_t = ui::animate_bool(start, start_active, ui::MOTION_HOVER);
        let mut start_shift = 0;
        if let Some(skin) = skin_opt {
            let sb = &skin.start_button;
            let sb_surf = if start_pressed {
                start_shift = 1;
                &sb.active
            } else if self.hover == Hit::Start {
                &sb.hover
            } else {
                &sb.normal
            };
            sb_surf.paint(canvas, start, sb.radii);
            if sb_surf.sprite.is_none() {
                if let Some(ref ic_sp) = sb.icon {
                    let iy = start.y + (start.h - ic_sp.height as i32) / 2 + start_shift;
                    canvas.blit_sprite(start.x + 10 + start_shift, iy, ic_sp);
                } else {
                    icons::draw(
                        canvas,
                        Icon::SpaceLogo,
                        start.x + 10 + start_shift,
                        start.y + (start.h - 20) / 2 + start_shift,
                        20,
                        None,
                    );
                }
                let tx = (start.x + 35 + start_shift) as f32;
                let ty = start.y as f32 + start.h as f32 * 0.5 + 4.8 + start_shift as f32;
                if let Some(sh_col) = sb.text_shadow {
                    self.text.draw(
                        canvas,
                        &sb.label,
                        tx + 1.0,
                        ty + 1.0,
                        13.5,
                        sh_col,
                        Face::Bold,
                    );
                }
                self.text
                    .draw(canvas, &sb.label, tx, ty, 13.5, sb.text_color, Face::Bold);
            }
        } else if is_vortex {
            let radius = (start.w.min(start.h) as f32 * 0.48).max(12.0);
            draw_space_start_orb(
                canvas,
                start.x as f32 + start.w as f32 * 0.5,
                start.y as f32 + start.h as f32 * 0.5,
                radius,
                self.theme.accent,
                start_t,
                if start_pressed { 1.0 } else { 0.0 },
                self.comp.start_orb_phase,
                self.theme.is_dark(),
            );
        } else {
            if is_win98 {
                canvas.fill(
                    start,
                    if start_pressed {
                        mix(self.theme.panel, self.theme.bevel_shadow, 0.12)
                    } else if self.hover == Hit::Start {
                        self.theme.panel_hover
                    } else {
                        self.theme.panel
                    },
                );
                if start_pressed {
                    ui::sunken_bevel(canvas, &self.theme, start);
                    if start.w >= 14 && start.h >= 12 {
                        canvas.dotted_border_1px(start.inset(4), self.theme.text);
                    }
                    start_shift = 1;
                } else {
                    ui::raised_bevel(canvas, &self.theme, start);
                }
            } else if start_t > 0.001 {
                canvas.rounded_rect(
                    start,
                    ctrl_r,
                    if start_pressed {
                        self.theme.accent_soft
                    } else {
                        self.theme.panel_hover
                    },
                    start_t,
                );
                canvas.rounded_border(
                    start,
                    ctrl_r,
                    if start_pressed {
                        self.theme.accent
                    } else {
                        self.theme.panel_border
                    },
                    start_t,
                    1,
                );
            }
            icons::draw(
                canvas,
                Icon::SpaceLogo,
                start.x + 9 + start_shift,
                start.y + (start.h - 22) / 2 + start_shift,
                22,
                None,
            );
            self.text.draw(
                canvas,
                "开始",
                (start.x + 38 + start_shift) as f32,
                start.y as f32 + start.h as f32 * 0.5 + 4.8 + start_shift as f32,
                13.0,
                self.theme.text,
                Face::Bold,
            );
        }

        let sep_h = (h - 14).clamp(16, 26);
        let sep_y = (h - sep_h) / 2;
        if skin_opt.is_some() || is_vortex {
            canvas.fill_alpha(Rect::new(layout.start_sep_x, sep_y, 1, sep_h), 0x0a1420, 0.30);
            canvas.fill_alpha(Rect::new(layout.start_sep_x + 1, sep_y, 1, sep_h), 0xffffff, 0.38);
        } else {
            draw_etched_sep(canvas, &self.theme, layout.start_sep_x, sep_y, sep_h);
        }

        // Quick-launch curated core apps (optional).
        if show_pinned {
            for (i, &rect) in pinned_rects.iter().enumerate() {
                let Some(app) = pinned_app_entry(i) else {
                    continue;
                };
                let pin_pressed = ui::is_rect_pressed(rect);
                let pin_shift = i32::from((is_win98 || skin_opt.is_some()) && pin_pressed);
                let hov_t = ui::animate_bool(rect, self.hover == Hit::Pinned(i), ui::MOTION_HOVER);
                if let Some(skin) = skin_opt {
                    if pin_pressed {
                        skin.taskbar
                            .button_pressed
                            .paint(canvas, rect, [skin.taskbar.button_radius; 4]);
                    } else if self.hover == Hit::Pinned(i) {
                        skin.taskbar
                            .button_hover
                            .paint(canvas, rect, [skin.taskbar.button_radius; 4]);
                    }
                } else if is_win98 {
                    if pin_pressed {
                        canvas.fill(
                            rect,
                            mix(self.theme.panel, self.theme.bevel_shadow, 0.14),
                        );
                        canvas.bevel_1px(rect, self.theme.bevel_shadow, self.theme.bevel_light);
                    } else if self.hover == Hit::Pinned(i) {
                        canvas.fill(rect, self.theme.panel_hover);
                        canvas.bevel_1px(rect, self.theme.bevel_light, self.theme.bevel_shadow);
                    }
                } else if is_vortex && (hov_t > 0.001 || pin_pressed) {
                    let t = hov_t.max(f32::from(u8::from(pin_pressed)));
                    let glow = Self::superbar_glow_tint(app.icon, self.theme.accent);
                    canvas.rounded_rect(rect, ctrl_r, 0xffffff, 0.28 * t);
                    canvas.with_clip(rect, |c| {
                        c.radial_glow(
                            (rect.x + rect.w / 2) as f32,
                            (rect.bottom() - 2) as f32,
                            rect.w as f32 * 0.9,
                            glow,
                            0.48 * t,
                        );
                    });
                    canvas.rounded_border(rect, ctrl_r, 0xffffff, 0.45 * t, 1);
                } else if hov_t > 0.001 || pin_pressed {
                    canvas.rounded_rect(
                        rect,
                        ctrl_r,
                        self.theme.panel_hover,
                        hov_t.max(f32::from(u8::from(pin_pressed))),
                    );
                }
                icons::draw(
                    canvas,
                    app.icon,
                    rect.x + (rect.w - 20) / 2 + pin_shift,
                    rect.y + (rect.h - 20) / 2 + pin_shift,
                    20,
                    None,
                );
            }
            if skin_opt.is_some() || is_vortex {
                canvas.fill_alpha(Rect::new(layout.pinned_sep_x, sep_y, 1, sep_h), 0x0a1420, 0.30);
                canvas.fill_alpha(Rect::new(layout.pinned_sep_x + 1, sep_y, 1, sep_h), 0xffffff, 0.38);
            } else {
                draw_etched_sep(canvas, &self.theme, layout.pinned_sep_x, sep_y, sep_h);
            }
        }

        // Running window buttons.
        for (index, &rect) in win_rects.iter().enumerate() {
            let managed = &self.windows[index];
            let is_active = !managed.minimized && self.active == Some(managed.window);
            let is_pressed = ui::is_rect_pressed(rect);
            let is_sunken = is_active || is_pressed;
            let is_hover = self.hover == Hit::Window(index);
            let hov_t = ui::animate_bool(rect, is_hover, ui::MOTION_HOVER);
            let mut win_shift = 0;
            if let Some(skin) = skin_opt {
                let tb = &skin.taskbar;
                let btn_surf = if is_pressed {
                    win_shift = 1;
                    &tb.button_pressed
                } else if is_active {
                    win_shift = 1;
                    &tb.button_active
                } else if is_hover {
                    &tb.button_hover
                } else {
                    &tb.button_normal
                };
                btn_surf.paint(canvas, rect, [tb.button_radius; 4]);
            } else if is_win98 {
                if is_sunken {
                    if is_active {
                        canvas.dither_checkerboard(rect, self.theme.panel, self.theme.bevel_light);
                    } else {
                        canvas.fill(
                            rect,
                            mix(self.theme.panel, self.theme.bevel_shadow, 0.14),
                        );
                    }
                    ui::sunken_bevel(canvas, &self.theme, rect);
                    win_shift = 1;
                } else {
                    canvas.fill(
                        rect,
                        if is_hover {
                            self.theme.panel_hover
                        } else {
                            self.theme.panel
                        },
                    );
                    ui::raised_bevel(canvas, &self.theme, rect);
                }
            } else if is_vortex {
                let glow = Self::superbar_glow_tint(managed.icon, self.theme.accent);
                let top_h = (rect.h / 2).max(4);
                let base_a = if is_active {
                    0.40
                } else if !managed.minimized {
                    0.18 + 0.18 * hov_t
                } else {
                    0.08 + 0.14 * hov_t
                };
                canvas.rounded_rect(
                    rect,
                    ctrl_r,
                    if is_active { 0xffffff } else { glow },
                    base_a,
                );
                canvas.fill_gradient_v(
                    Rect::new(rect.x + 2, rect.y + 1, (rect.w - 4).max(1), top_h),
                    0xffffff,
                    0xffffff,
                    if is_active { 0.36 } else { 0.18 + 0.14 * hov_t },
                    0.04,
                );
                if hov_t > 0.01 {
                    let mx = self
                        .comp
                        .taskbar_mouse_x
                        .filter(|_| is_hover)
                        .unwrap_or(rect.x + rect.w / 2)
                        .clamp(rect.x, rect.right());
                    canvas.with_clip(rect, |c| {
                        c.radial_glow(
                            mx as f32,
                            (rect.bottom() - 2) as f32,
                            (rect.h as f32 * 1.65).max(32.0),
                            glow,
                            0.60 * hov_t,
                        );
                    });
                }
                let rim_col = if is_active { 0xe8f5ff } else { 0xffffff };
                let rim_a = if is_active { 0.80 } else { 0.32 + 0.36 * hov_t };
                canvas.rounded_border(rect, ctrl_r, rim_col, rim_a, 1);
                if is_active {
                    canvas.rounded_rect(
                        Rect::new(rect.x + 8, rect.bottom() - 3, (rect.w - 16).max(6), 2),
                        1,
                        mix(glow, 0xffffff, 0.35),
                        0.95,
                    );
                }
            } else {
                let bg = if is_active {
                    self.theme.panel_high
                } else if hov_t > 0.001 {
                    mix(self.theme.panel, self.theme.panel_hover, hov_t)
                } else {
                    self.theme.panel
                };
                canvas.rounded_rect(rect, ctrl_r, bg, 1.0);
                if is_active || hov_t > 0.001 {
                    let border_alpha = if is_active { 1.0 } else { hov_t };
                    canvas.rounded_border(rect, ctrl_r, self.theme.panel_border, border_alpha, 1);
                }
                if is_active {
                    canvas.rounded_rect(
                        Rect::new(rect.x + 10, rect.bottom() - 3, rect.w - 20, 2),
                        1,
                        self.theme.accent,
                        1.0,
                    );
                } else if !managed.minimized {
                    canvas.rounded_rect(
                        Rect::new(rect.x + rect.w / 2 - 6, rect.bottom() - 3, 12, 2),
                        1,
                        self.theme.text_muted,
                        1.0,
                    );
                }
            }
            icons::draw(
                canvas,
                managed.icon,
                rect.x + 8 + win_shift,
                rect.y + (rect.h - 18) / 2 + win_shift,
                18,
                None,
            );
            let max_w = (rect.w - 38).max(20) as f32;
            let label = self.text.truncate(&managed.title, max_w, 12.0, Face::Ui);
            let fg = if let Some(skin) = skin_opt {
                if is_active {
                    skin.taskbar.button_active_text
                } else if managed.minimized {
                    skin.taskbar.button_minimized_text
                } else {
                    skin.taskbar.button_text
                }
            } else if is_vortex {
                if self.theme.is_dark() {
                    if managed.minimized { 0xbccad8 } else { 0xf5f9fd }
                } else if is_active {
                    0x0c1828
                } else if managed.minimized {
                    0x2a3b4e
                } else {
                    0x101c2b
                }
            } else if is_active {
                self.theme.text
            } else if managed.minimized {
                self.theme.text_muted
            } else {
                self.theme.text_dim
            };
            let face = if (is_win98 || is_vortex || skin_opt.is_some()) && is_active {
                Face::Bold
            } else {
                Face::Ui
            };
            let label_x = (rect.x + 31 + win_shift) as f32;
            let label_y = rect.y as f32 + rect.h as f32 * 0.5 + 4.5 + win_shift as f32;
            if is_vortex {
                let glow = TextGlow::aero_compact(self.theme.is_dark(), is_active);
                canvas.with_clip(rect.inset(1), |c| {
                    self.text.draw_glow(c, &label, label_x, label_y, 12.0, face, glow);
                });
            }
            self.text.draw(canvas, &label, label_x, label_y, 12.0, fg, face);
        }

        // Right tray: IME toggle, wired network status, speaker volume icon, bell & clock.
        if let Some(ref tray_surf) = skin_opt.and_then(|s| s.taskbar.tray_bg.as_ref()) {
            let tray_x = (ime.x - 8).max(0);
            let tray_rect = Rect::new(tray_x, 0, w - tray_x, h);
            tray_surf.paint(canvas, tray_rect, [0; 4]);
        } else if is_win98 {
            let well = Rect::new(ime.x - 4, 4, w - (ime.x - 4) - 4, h - 8);
            canvas.bevel_1px(well, self.theme.bevel_shadow, self.theme.bevel_hi);
        }
        let tray_fg_override = skin_opt.map(|s| (s.taskbar.tray_text, s.taskbar.tray_muted_text));
        let ime_pressed = (self.ctx_open && self.ctx_is_ime) || ui::is_rect_pressed(ime);
        let ime_shift = i32::from(is_win98 && ime_pressed);
        let ime_hov = ui::animate_bool(ime, self.hover == Hit::ImeTray, ui::MOTION_HOVER);
        let ime_on = self.config.ime_enabled && self.ime_active;
        let ime_lbl = if !ime_on {
            "--"
        } else if self.ime_chinese {
            "中"
        } else {
            "英"
        };
        if is_win98 {
            if ime_pressed {
                canvas.bevel_1px(ime, self.theme.bevel_shadow, self.theme.bevel_light);
            } else if self.hover == Hit::ImeTray {
                canvas.bevel_1px(ime, self.theme.bevel_hi, self.theme.bevel_shadow);
            }
        } else {
            if ime_on && self.ime_chinese {
                canvas.rounded_rect(
                    ime,
                    ctrl_r,
                    if is_vortex || tray_fg_override.is_some() {
                        0xffffff
                    } else {
                        self.theme.accent
                    },
                    if is_vortex || tray_fg_override.is_some() {
                        0.22 + 0.16 * ime_hov
                    } else {
                        0.16 + 0.10 * ime_hov
                    },
                );
            } else if ime_hov > 0.001 {
                canvas.rounded_rect(
                    ime,
                    ctrl_r,
                    if is_vortex || tray_fg_override.is_some() {
                        0xffffff
                    } else {
                        self.theme.panel_hover
                    },
                    if is_vortex || tray_fg_override.is_some() {
                        0.26 * ime_hov
                    } else {
                        ime_hov
                    },
                );
            }
            if (is_vortex || tray_fg_override.is_some())
                && (ime_hov > 0.001 || (ime_on && self.ime_chinese))
            {
                canvas.rounded_border(ime, ctrl_r, 0xffffff, 0.35, 1);
            }
        }
        let ime_fg = if let Some((t_fg, t_muted)) = tray_fg_override {
            if !ime_on { t_muted } else { t_fg }
        } else if !ime_on {
            self.theme.text_muted
        } else if is_vortex {
            if self.theme.is_dark() {
                0xf5f9fd
            } else {
                0x0e1d30
            }
        } else if self.ime_chinese {
            self.theme.accent
        } else {
            self.theme.text_dim
        };
        if is_vortex {
            let glow = TextGlow::aero_compact(self.theme.is_dark(), self.ime_chinese);
            canvas.with_clip(ime.inset(1), |c| {
                self.text.draw_glow(
                    c,
                    ime_lbl,
                    (ime.x + ime.w / 2) as f32,
                    ime.y as f32 + ime.h as f32 * 0.5 + 4.5 + ime_shift as f32,
                    12.0,
                    Face::Bold,
                    glow,
                );
            });
        }
        self.text.draw_centered(
            canvas,
            ime_lbl,
            (ime.x + ime.w / 2 + ime_shift) as f32,
            ime.y as f32 + ime.h as f32 * 0.5 + 4.5 + ime_shift as f32,
            12.0,
            ime_fg,
            if self.ime_chinese {
                Face::Bold
            } else {
                Face::Ui
            },
        );
        let net_pressed = ui::is_rect_pressed(net);
        let net_shift = i32::from(is_win98 && net_pressed);
        let net_hov = ui::animate_bool(net, self.hover == Hit::Network, ui::MOTION_HOVER);
        let net_connected = self.net_status.connected();
        if is_win98 {
            if net_pressed {
                canvas.bevel_1px(net, self.theme.bevel_shadow, self.theme.bevel_light);
            } else if self.hover == Hit::Network {
                canvas.bevel_1px(net, self.theme.bevel_hi, self.theme.bevel_shadow);
            }
        } else if net_hov > 0.001 {
            canvas.rounded_rect(
                net,
                ctrl_r,
                if is_vortex || tray_fg_override.is_some() {
                    0xffffff
                } else {
                    self.theme.panel_hover
                },
                if is_vortex || tray_fg_override.is_some() {
                    0.26 * net_hov
                } else {
                    net_hov
                },
            );
        }
        let (net_icon, net_tint) = if let Some((t_fg, t_muted)) = tray_fg_override {
            if net_connected {
                (Icon::NetworkWired, t_fg)
            } else if self.net_status.has_device && self.net_status.up {
                (Icon::NetworkWired, 0xffe066)
            } else {
                (Icon::NetworkOffline, t_muted)
            }
        } else if net_connected {
            (Icon::NetworkWired, self.theme.text)
        } else if self.net_status.has_device && self.net_status.up {
            (Icon::NetworkWired, self.theme.warning)
        } else {
            (Icon::NetworkOffline, self.theme.text_muted)
        };
        icons::draw(
            canvas,
            net_icon,
            net.x + (net.w - 18) / 2 + net_shift,
            net.y + (net.h - 18) / 2 + net_shift,
            18,
            Some(net_tint),
        );

        let vol_pressed = self.vol_open || ui::is_rect_pressed(vol);
        let vol_shift = i32::from(is_win98 && vol_pressed);
        let vol_active = vol_pressed || self.hover == Hit::Volume;
        let vol_hov = ui::animate_bool(vol, vol_active, ui::MOTION_HOVER);
        if is_win98 {
            if vol_pressed {
                canvas.bevel_1px(vol, self.theme.bevel_shadow, self.theme.bevel_hi);
            } else if self.hover == Hit::Volume {
                canvas.bevel_1px(vol, self.theme.bevel_hi, self.theme.bevel_shadow);
            }
        } else if vol_hov > 0.001 {
            canvas.rounded_rect(
                vol,
                ctrl_r,
                if self.vol_open && tray_fg_override.is_none() {
                    self.theme.accent_soft
                } else if is_vortex || tray_fg_override.is_some() {
                    0xffffff
                } else {
                    self.theme.panel_hover
                },
                if (is_vortex || tray_fg_override.is_some()) && !self.vol_open {
                    0.26 * vol_hov
                } else if tray_fg_override.is_some() {
                    0.34
                } else {
                    vol_hov
                },
            );
        }
        let vol_icon = Icon::for_volume(self.config.sound_master_volume, self.config.sound_muted);
        let vol_tint = if let Some((t_fg, t_muted)) = tray_fg_override {
            if self.config.sound_muted || self.config.sound_master_volume <= 0 {
                t_muted
            } else {
                t_fg
            }
        } else if self.config.sound_muted || self.config.sound_master_volume <= 0 {
            self.theme.text_muted
        } else if self.vol_open {
            self.theme.accent
        } else {
            self.theme.text
        };
        icons::draw(
            canvas,
            vol_icon,
            vol.x + (vol.w - 18) / 2 + vol_shift,
            vol.y + (vol.h - 18) / 2 + vol_shift,
            18,
            Some(vol_tint),
        );

        let bell_pressed = ui::is_rect_pressed(bell);
        let bell_shift = i32::from(is_win98 && bell_pressed);
        let bell_hov = ui::animate_bool(bell, self.hover == Hit::Bell, ui::MOTION_HOVER);
        if is_win98 {
            if bell_pressed {
                canvas.bevel_1px(bell, self.theme.bevel_shadow, self.theme.bevel_light);
            } else if self.hover == Hit::Bell {
                canvas.bevel_1px(bell, self.theme.bevel_hi, self.theme.bevel_shadow);
            }
        } else if bell_hov > 0.001 {
            canvas.rounded_rect(
                bell,
                ctrl_r,
                if is_vortex || tray_fg_override.is_some() {
                    0xffffff
                } else {
                    self.theme.panel_hover
                },
                if is_vortex || tray_fg_override.is_some() {
                    0.26 * bell_hov
                } else {
                    bell_hov
                },
            );
        }
        icons::draw(
            canvas,
            if self.toast.is_some() {
                Icon::BellActive
            } else {
                Icon::Bell
            },
            bell.x + (bell.w - 18) / 2 + bell_shift,
            bell.y + (bell.h - 18) / 2 + bell_shift,
            18,
            Some(if let Some((t_fg, t_muted)) = tray_fg_override {
                if self.toast.is_some() { 0xffe066 } else { t_fg.max(t_muted) }
            } else if self.toast.is_some() {
                self.theme.accent
            } else {
                self.theme.text_dim
            }),
        );
        if !is_win98 {
            if is_vortex || tray_fg_override.is_some() {
                canvas.fill_alpha(Rect::new(layout.clock_sep_x, sep_y, 1, sep_h), 0x0a1420, 0.25);
                canvas.fill_alpha(Rect::new(layout.clock_sep_x + 1, sep_y, 1, sep_h), 0xffffff, 0.36);
            } else {
                canvas.fill(
                    Rect::new(layout.clock_sep_x, sep_y, 1, sep_h),
                    self.theme.panel_border,
                );
            }
        }

        let (year, month, day, hour, minute, sec, weekday) = now_local();
        let time_str = if self.config.clock_24h {
            if self.config.clock_seconds {
                format!("{hour:02}:{minute:02}:{sec:02}")
            } else {
                format!("{hour:02}:{minute:02}")
            }
        } else {
            let period = if hour < 12 { "上午" } else { "下午" };
            let h12 = match hour % 12 {
                0 => 12,
                v => v,
            };
            if self.config.clock_seconds {
                format!("{period} {h12}:{minute:02}:{sec:02}")
            } else {
                format!("{period} {h12}:{minute:02}")
            }
        };

        let right_x = if is_vortex {
            (w - 22) as f32
        } else {
            (w - 14) as f32
        };
        let clock_fg = if let Some((t_fg, _)) = tray_fg_override {
            t_fg
        } else if is_vortex {
            if self.theme.is_dark() { 0xf4f8fc } else { 0x0d1826 }
        } else {
            self.theme.text
        };
        let date_fg = if let Some((_, t_muted)) = tray_fg_override {
            t_muted
        } else if is_vortex {
            if self.theme.is_dark() { 0xc0d0e0 } else { 0x1e2c3e }
        } else {
            self.theme.text_muted
        };
        let clock_clip = Rect::new(
            layout.clock_sep_x + 2,
            1,
            (layout.show_desktop.x - layout.clock_sep_x - 3).max(1),
            (h - 2).max(1),
        );
        let clock_glow = TextGlow::aero_compact(self.theme.is_dark(), true);
        if self.config.clock_show_date {
            let weeks = ["日", "一", "二", "三", "四", "五", "六"];
            let mid = h as f32 * 0.5;
            let date_str = format!(
                "{year}/{month:02}/{day:02} 周{}",
                weeks[(weekday.max(0) as usize).min(6)]
            );
            if is_vortex {
                canvas.with_clip(clock_clip, |c| {
                    self.text.draw_right_with_glow(
                        c, &time_str, right_x, mid - 2.0, 12.5, clock_fg, Face::Bold, clock_glow,
                    );
                    self.text.draw_right_with_glow(
                        c, &date_str, right_x, mid + 13.0, 10.0, date_fg, Face::Ui, clock_glow,
                    );
                });
            } else {
                self.text
                    .draw_right(canvas, &time_str, right_x, mid - 2.0, 12.5, clock_fg, Face::Bold);
                self.text
                    .draw_right(canvas, &date_str, right_x, mid + 13.0, 10.0, date_fg, Face::Ui);
            }
        } else if is_vortex {
            canvas.with_clip(clock_clip, |c| {
                self.text.draw_right_with_glow(
                    c,
                    &time_str,
                    right_x,
                    h as f32 * 0.5 + 4.8,
                    13.0,
                    clock_fg,
                    Face::Bold,
                    clock_glow,
                );
            });
        } else {
            self.text.draw_right(
                canvas,
                &time_str,
                right_x,
                h as f32 * 0.5 + 4.8,
                13.0,
                clock_fg,
                Face::Bold,
            );
        }

        // Far-right Aero Show Desktop crystal sliver
        if is_vortex && layout.show_desktop.w > 0 {
            let sd = layout.show_desktop;
            let sd_hov = ui::animate_bool(sd, self.hover == Hit::ShowDesktop, ui::MOTION_HOVER);
            canvas.fill_alpha(sd, 0xffffff, 0.14 + 0.28 * sd_hov);
            canvas.fill_alpha(Rect::new(sd.x, 1, 1, h - 2), 0x0a1420, 0.40);
            canvas.fill_alpha(Rect::new(sd.x + 1, 1, 1, h - 2), 0xffffff, 0.45 + 0.30 * sd_hov);
        }
    }

    fn draw_menu_vortex(&mut self) {
        let filtered = self.filtered_apps();
        let canvas = &mut self.menu_canvas;
        let (w, h) = (VORTEX_MENU_WIDTH, VORTEX_MENU_HEIGHT);
        let r = self.theme.radius_popup();
        if canvas.is_argb() {
            canvas.clear_transparent(Rect::new(0, 0, w, h), self.theme.panel);
        } else {
            canvas.clear(self.theme.panel);
        }
        let outer = Rect::new(0, 0, w, h);
        let (frame_tint, frame_a) = self.theme.glass_tint(true);
        canvas.rounded_rect(outer, r, frame_tint, (frame_a as f32 / 255.0).max(0.45));
        canvas.rounded_border(outer, r, 0x0f1a28, 0.72, 1);
        canvas.rounded_border(outer.inset(1), (r - 1).max(1), 0xffffff, 0.56, 1);

        // Left pane: frosted crisp app list + recessed Win7 Search box.
        let left_pane = Rect::new(10, 10, 252, h - 20);
        let left_bg = if self.theme.is_dark() { 0x162230 } else { 0xfafdff };
        canvas.rounded_rect(left_pane, 5, left_bg, 0.94);
        canvas.rounded_border(
            left_pane,
            5,
            if self.theme.is_dark() { 0x3a526c } else { 0x8aa2ba },
            0.78,
            1,
        );

        if filtered.is_empty() {
            self.text.draw_centered(
                canvas,
                "未找到匹配的应用",
                left_pane.x as f32 + left_pane.w as f32 * 0.5,
                180.0,
                12.5,
                self.theme.text_muted,
                Face::Ui,
            );
        } else {
            for (slot, &app_idx) in filtered.iter().take(8).enumerate() {
                let app = &APPS[app_idx];
                let row = vortex_menu_row_rect(slot);
                let hovered = self.hover == Hit::MenuRow(app_idx)
                    || (slot == 0 && !self.menu_query.is_empty());
                let fg = ui::list_item_bg(canvas, &self.theme, row, hovered, false);
                icons::draw(canvas, app.icon, row.x + 8, row.y + 9, 32, None);
                self.text.draw(
                    canvas,
                    app.name,
                    (row.x + 48) as f32,
                    (row.y + 22) as f32,
                    12.5,
                    fg,
                    Face::Bold,
                );
                self.text.draw(
                    canvas,
                    app.subtitle,
                    (row.x + 48) as f32,
                    (row.y + 39) as f32,
                    10.5,
                    self.theme.text_muted,
                    Face::Ui,
                );
            }
        }

        let search_r = vortex_menu_search_rect();
        let search_caret = ui::text_input_ime(
            canvas,
            &mut self.text,
            &self.theme,
            search_r,
            &self.menu_query,
            &self.menu_ime_preedit,
            "搜索程序和文件...",
            true,
            Some(Icon::Search),
        );

        // Right smoky Aero column: user emblem + place links + split Win7 power bar.
        let right_fg = if self.theme.is_dark() { 0xf4f8fc } else { 0x0d1a2a };
        let avatar_box = Rect::new(280, 14, 48, 48);
        canvas.rounded_rect(avatar_box, 6, 0xffffff, 0.35);
        canvas.rounded_border(avatar_box, 6, 0x0a1624, 0.45, 1);
        canvas.rounded_border(avatar_box.inset(1), 5, 0xffffff, 0.72, 1);
        icons::draw(canvas, Icon::SpaceLogo, avatar_box.x + 8, avatar_box.y + 8, 32, None);

        let identity = session_identity();
        let shown_id = self.text.truncate(&identity, 96.0, 13.0, Face::Bold);
        let dark = self.theme.is_dark();
        self.text.draw_with_glow(
            canvas,
            &shown_id,
            338.0,
            38.0,
            13.0,
            right_fg,
            Face::Bold,
            TextGlow::aero_compact(dark, true),
        );
        self.text.draw_with_glow(
            canvas,
            "SpaceOS 9",
            338.0,
            54.0,
            10.5,
            if dark { 0xb0c4d8 } else { 0x26384c },
            Face::Ui,
            TextGlow::aero_compact(dark, false),
        );
        canvas.fill_alpha(Rect::new(274, 72, w - 288, 1), 0xffffff, 0.35);

        for (i, place) in VORTEX_PLACES.iter().enumerate() {
            if i == 4 {
                canvas.fill_alpha(Rect::new(274, 82 + 4 * 38 - 2, w - 288, 1), 0xffffff, 0.30);
            }
            let pr = vortex_menu_place_rect(i);
            let hovered = self.hover == Hit::MenuPlace(i);
            if hovered {
                canvas.rounded_rect(pr, 4, 0xffffff, 0.34);
                canvas.rounded_border(pr, 4, self.theme.aero_hover_border(), 0.72, 1);
            }
            icons::draw(canvas, place.icon, pr.x + 8, pr.y + 7, 20, None);
            self.text.draw_with_glow(
                canvas,
                place.label,
                (pr.x + 34) as f32,
                (pr.y + 22) as f32,
                12.0,
                right_fg,
                if hovered { Face::Bold } else { Face::Ui },
                TextGlow::aero_compact(dark, hovered),
            );
        }

        let (logout, reboot, power_off, confirm_cancel, confirm_ok) = vortex_menu_footer_rects();
        if let Some(action) = self.menu_confirm_power {
            let (prompt, ok_label, ok_icon) = match action {
                PowerAction::Reboot => ("确认重启？", "重启", Icon::Refresh),
                PowerAction::PowerOff => ("确认关机？", "关机", Icon::Shutdown),
            };
            self.text.draw_with_glow(
                canvas,
                prompt,
                276.0,
                (h - 60) as f32,
                12.0,
                right_fg,
                Face::Bold,
                TextGlow::aero_compact(dark, true),
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                confirm_cancel,
                "取消",
                None,
                false,
                self.hover == Hit::ConfirmPowerCancel,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                confirm_ok,
                ok_label,
                Some(ok_icon),
                true,
                self.hover == Hit::ConfirmPowerOk,
            );
        } else {
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                power_off,
                "关机",
                Some(Icon::Shutdown),
                false,
                self.hover == Hit::PowerOff,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                reboot,
                "重启",
                None,
                false,
                self.hover == Hit::Reboot,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                logout,
                "注销",
                None,
                false,
                self.hover == Hit::Logout,
            );
        }
        if canvas.is_argb() {
            canvas.mask_rounded_corners(r);
        }
        self.sync_menu_ime_spot(search_caret);
    }

    fn draw_menu_custom_two_column(&mut self, sm: &'static crate::theme::StartMenuSkin) {
        let filtered = self.filtered_apps();
        let canvas = &mut self.menu_canvas;
        let (w, h) = (sm.width, sm.height);
        let top_r = sm.top_radius;
        let bot_r = sm.bottom_radius;
        canvas.clear(sm.frame_border);

        // 1. Top User Banner
        let header_r = Rect::new(0, 0, w, sm.header_height);
        sm.header_bg.paint(canvas, header_r, [top_r, top_r, 0, 0]);
        if sm.header_top_sheen {
            canvas.fill_alpha(Rect::new(4, 1, (w - 8).max(1), 2), 0xffffff, 0.42);
            canvas.fill_alpha(Rect::new(6, 3, (w - 12).max(1), 1), 0xffffff, 0.18);
        }
        let avatar_box = Rect::new(10, (sm.header_height - 44) / 2, 44, 44);
        canvas.rounded_rect(avatar_box, 4, 0xffffff, 0.96);
        canvas.rounded_border(avatar_box, 4, sm.avatar_border, 0.95, 2);
        icons::draw(
            canvas,
            Icon::SpaceLogo,
            avatar_box.x + 6,
            avatar_box.y + 6,
            32,
            None,
        );
        let identity = session_identity();
        let shown_id = self.text.truncate(&identity, (w - 80) as f32, 14.5, Face::Bold);
        let id_y = (sm.header_height / 2 + 5) as f32;
        if let Some(sh) = sm.header_text_shadow {
            self.text
                .draw(canvas, &shown_id, 63.0, id_y + 1.0, 14.5, sh, Face::Bold);
        }
        self.text.draw(
            canvas,
            &shown_id,
            62.0,
            id_y,
            14.5,
            sm.header_text,
            Face::Bold,
        );

        // 2. Two-column body (Left programs + Right system places)
        let body_top = sm.header_height;
        let body_h = (h - sm.footer_height - body_top).max(40);
        let left_w = (w / 2 + 6).clamp(180, w - 140);
        let left_pane = Rect::new(2, body_top, left_w, body_h);
        let right_pane = Rect::new(2 + left_w, body_top, (w - 4 - left_w).max(40), body_h);
        canvas.fill(left_pane, sm.left_bg);
        canvas.fill(right_pane, sm.right_bg);
        canvas.fill(
            Rect::new(right_pane.x, body_top, 1, body_h),
            sm.right_border_left,
        );

        // Center-fading 2px orange highlight divider under the header banner
        if let Some(orange) = sm.orange_divider {
            let mid = (w / 2).max(1);
            for x in 2..(w - 2) {
                let dist = (x - mid).abs() as f32 / mid as f32;
                let a = (1.0 - dist).clamp(0.0, 1.0);
                if a > 0.01 {
                    canvas.fill_alpha(Rect::new(x, body_top, 1, 2), orange, a * 0.92);
                }
            }
        }

        // Left column: pinned + frequent applications
        if filtered.is_empty() {
            self.text.draw_centered(
                canvas,
                "未找到匹配的应用",
                left_pane.x as f32 + left_pane.w as f32 * 0.5,
                (body_top + 140) as f32,
                12.0,
                sm.left_subtext,
                Face::Ui,
            );
        } else {
            for (slot, &app_idx) in filtered.iter().take(7).enumerate() {
                if slot == 2 {
                    let sep_y = custom_menu_row_rect(sm, slot).y - 5;
                    let l_mid = left_pane.x + left_pane.w / 2;
                    for x in (left_pane.x + 10)..(left_pane.right() - 10) {
                        let dist = (x - l_mid).abs() as f32 / (left_pane.w as f32 * 0.5);
                        let a = (1.0 - dist).clamp(0.0, 1.0) * 0.22;
                        canvas.fill_alpha(Rect::new(x, sep_y, 1, 1), 0x000000, a);
                    }
                }
                let app = &APPS[app_idx];
                let row = custom_menu_row_rect(sm, slot);
                let hovered = self.hover == Hit::MenuRow(app_idx)
                    || (slot == 0 && !self.menu_query.is_empty());
                if hovered {
                    canvas.fill(row, sm.item_hover_bg);
                }
                let title_fg = if hovered {
                    sm.item_hover_text
                } else {
                    sm.left_text
                };
                let sub_fg = if hovered {
                    sm.item_hover_text
                } else {
                    sm.left_subtext
                };
                icons::draw(canvas, app.icon, row.x + 6, row.y + 6, 30, None);
                self.text.draw(
                    canvas,
                    app.name,
                    (row.x + 42) as f32,
                    (row.y + 18) as f32,
                    12.0,
                    title_fg,
                    if slot < 2 { Face::Bold } else { Face::Ui },
                );
                let max_sub = (row.w - 46).max(30) as f32;
                let sub_shown = self.text.truncate(app.subtitle, max_sub, 10.0, Face::Ui);
                self.text.draw(
                    canvas,
                    &sub_shown,
                    (row.x + 42) as f32,
                    (row.y + 33) as f32,
                    10.0,
                    sub_fg,
                    Face::Ui,
                );
            }
        }

        // Bottom of Left Column: All Programs badge + compact search input
        let search_r = custom_menu_search_rect(sm);
        let ap_sep_y = search_r.y - 26;
        let l_mid = left_pane.x + left_pane.w / 2;
        for x in (left_pane.x + 10)..(left_pane.right() - 10) {
            let dist = (x - l_mid).abs() as f32 / (left_pane.w as f32 * 0.5);
            let a = (1.0 - dist).clamp(0.0, 1.0) * 0.25;
            canvas.fill_alpha(Rect::new(x, ap_sep_y, 1, 1), 0x000000, a);
        }
        self.text.draw(
            canvas,
            &sm.all_programs_label,
            (left_pane.x + 38) as f32,
            (search_r.y - 8) as f32,
            11.5,
            sm.left_text,
            Face::Bold,
        );
        // Green triangle arrow next to "所有程序"
        let ax = left_pane.x + 114;
        let ay = search_r.y - 17;
        for dy in 0..9 {
            let span = (5 - (dy - 4_i32).abs()).max(1);
            canvas.fill(Rect::new(ax, ay + dy, span, 1), sm.all_programs_arrow);
        }

        let search_caret = ui::text_input_ime(
            canvas,
            &mut self.text,
            &self.theme,
            search_r,
            &self.menu_query,
            &self.menu_ime_preedit,
            "搜索程序...",
            true,
            Some(Icon::Search),
        );

        // Right column: Places & System Tools
        for (i, place) in VORTEX_PLACES.iter().enumerate() {
            if i == 3 {
                let sep_y = custom_menu_place_rect(sm, i).y - 6;
                let r_mid = right_pane.x + right_pane.w / 2;
                for x in (right_pane.x + 10)..(right_pane.right() - 10) {
                    let dist = (x - r_mid).abs() as f32 / (right_pane.w as f32 * 0.5);
                    let a = (1.0 - dist).clamp(0.0, 1.0) * 0.65;
                    canvas.fill_alpha(Rect::new(x, sep_y, 1, 1), sm.right_border_left, a);
                }
            }
            let pr = custom_menu_place_rect(sm, i);
            let hovered = self.hover == Hit::MenuPlace(i);
            if hovered {
                canvas.fill(pr, sm.item_hover_bg);
            }
            let fg = if hovered {
                sm.item_hover_text
            } else {
                sm.right_text
            };
            icons::draw(canvas, place.icon, pr.x + 6, pr.y + 6, 22, None);
            self.text.draw(
                canvas,
                place.label,
                (pr.x + 34) as f32,
                (pr.y + 21) as f32,
                11.5,
                fg,
                if i < 3 { Face::Bold } else { Face::Ui },
            );
        }

        // 3. Bottom Footer Bar with Iconic Square Logout / Reboot / Turn Off Computer Badges
        let footer_r = Rect::new(0, h - sm.footer_height, w, sm.footer_height);
        sm.footer_bg.paint(canvas, footer_r, [0, 0, bot_r, bot_r]);
        let (logout, reboot, power_off, confirm_cancel, confirm_ok) =
            custom_menu_footer_rects(sm);
        if let Some(action) = self.menu_confirm_power {
            let (prompt, ok_label, ok_icon) = match action {
                PowerAction::Reboot => ("确认重新启动计算机？", "重启", Icon::Refresh),
                PowerAction::PowerOff => ("确认关闭计算机？", "关机", Icon::Shutdown),
            };
            self.text.draw(
                canvas,
                prompt,
                14.0,
                (footer_r.y + 26) as f32,
                12.0,
                sm.footer_text,
                Face::Bold,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                confirm_cancel,
                "取消",
                None,
                false,
                self.hover == Hit::ConfirmPowerCancel,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                confirm_ok,
                ok_label,
                Some(ok_icon),
                true,
                self.hover == Hit::ConfirmPowerOk,
            );
        } else {
            for (btn_r, badge_surf, icon, label, hit_kind) in [
                (logout, &sm.logout_badge, Icon::Logout, "注销", Hit::Logout),
                (reboot, &sm.reboot_badge, Icon::Refresh, "重启", Hit::Reboot),
                (
                    power_off,
                    &sm.shutdown_badge,
                    Icon::Shutdown,
                    "关闭计算机",
                    Hit::PowerOff,
                ),
            ] {
                let hov = self.hover == hit_kind;
                if hov {
                    canvas.rounded_rect(btn_r, 3, 0x103896, 0.55);
                }
                let badge = Rect::new(btn_r.x + 3, btn_r.y + (btn_r.h - 22) / 2, 22, 22);
                badge_surf.paint(canvas, badge, [3; 4]);
                icons::draw(canvas, icon, badge.x + 3, badge.y + 3, 16, Some(0xffffff));
                self.text.draw(
                    canvas,
                    label,
                    (badge.right() + 5) as f32,
                    (btn_r.y + 19) as f32,
                    11.5,
                    sm.footer_text,
                    Face::Ui,
                );
            }
        }

        // Outer Frame Border
        canvas.rounded_border_corners(
            Rect::new(0, 0, w, h),
            [top_r, top_r, bot_r, bot_r],
            sm.frame_border,
            1.0,
            2,
        );
        self.sync_menu_ime_spot(search_caret);
    }

    fn draw_menu(&mut self) {
        if self.config.panel_start_fullscreen {
            let host = ShellConfigHost::new(&self.x, &mut self.config, &mut self.ux_client);
            self.start_screen.draw(
                &mut self.menu_canvas,
                &mut self.text,
                &self.theme,
                &host,
                &self.net_status,
                &self.wallpaper_scaled,
                self.windows.len(),
            );
            let caret = self
                .start_screen
                .ime_spot(i32::from(self.x.width), i32::from(self.x.height));
            self.sync_menu_ime_spot(caret);
            return;
        }
        if let Some(ref sm) = self.theme.skin().and_then(|s| s.start_menu.as_ref()) {
            if sm.layout == crate::theme::StartMenuLayoutKind::TwoColumn {
                self.draw_menu_custom_two_column(sm);
                return;
            }
        }
        if self.theme.is_vortexglass() {
            self.draw_menu_vortex();
            return;
        }
        let filtered = self.filtered_apps();
        let canvas = &mut self.menu_canvas;
        let (w, h) = (MENU_WIDTH, MENU_HEIGHT);
        canvas.clear(self.theme.panel);
        ui::popup_surface(
            canvas,
            &self.theme,
            Rect::new(0, 0, w, h),
            self.theme.radius_popup(),
        );

        let search_caret = ui::text_input_ime(
            canvas,
            &mut self.text,
            &self.theme,
            Rect::new(14, 14, w - 28, 34),
            &self.menu_query,
            &self.menu_ime_preedit,
            "搜索应用...",
            true,
            Some(Icon::Search),
        );

        if filtered.is_empty() {
            self.text.draw_centered(
                canvas,
                "未找到匹配的应用",
                w as f32 * 0.5,
                160.0,
                12.5,
                self.theme.text_muted,
                Face::Ui,
            );
        } else {
            let is_win98 = self.theme.is_win98();
            for (slot, &app_idx) in filtered.iter().enumerate() {
                let app = &APPS[app_idx];
                let row = menu_row_rect(slot);
                let hovered = self.hover == Hit::MenuRow(app_idx)
                    || (slot == 0 && !self.menu_query.is_empty());
                let fg = ui::list_item_bg(canvas, &self.theme, row, hovered, false);
                let sub_fg = if hovered && is_win98 {
                    self.theme.selection_text
                } else {
                    self.theme.text_muted
                };
                icons::draw(canvas, app.icon, row.x + 10, row.y + 8, 32, None);
                self.text.draw(
                    canvas,
                    app.name,
                    (row.x + 52) as f32,
                    (row.y + 20) as f32,
                    13.0,
                    fg,
                    Face::Bold,
                );
                self.text.draw(
                    canvas,
                    app.subtitle,
                    (row.x + 52) as f32,
                    (row.y + 37) as f32,
                    11.0,
                    sub_fg,
                    Face::Ui,
                );
            }
        }

        // Footer bar.
        let (logout, reboot, power_off, confirm_cancel, confirm_ok) = compact_footer_rects();
        canvas.fill(Rect::new(3, h - 54, w - 6, 51), self.theme.window_alt);
        draw_etched_hsep(canvas, &self.theme, 3, h - 54, w - 6);
        if let Some(action) = self.menu_confirm_power {
            let (prompt, ok_label, ok_icon) = match action {
                PowerAction::Reboot => ("确认重新启动计算机？", "重启", Icon::Refresh),
                PowerAction::PowerOff => ("确认关闭计算机？", "关机", Icon::Shutdown),
            };
            self.text.draw(
                canvas,
                prompt,
                16.0,
                (h - 22) as f32,
                12.5,
                self.theme.text,
                Face::Bold,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                confirm_cancel,
                "取消",
                None,
                false,
                self.hover == Hit::ConfirmPowerCancel,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                confirm_ok,
                ok_label,
                Some(ok_icon),
                true,
                self.hover == Hit::ConfirmPowerOk,
            );
        } else {
            icons::draw(canvas, Icon::SpaceLogo, 16, h - 38, 22, None);
            let identity = session_identity();
            let shown_id = self.text.truncate(&identity, 102.0, 12.0, Face::Ui);
            self.text.draw(
                canvas,
                &shown_id,
                44.0,
                (h - 22) as f32,
                12.0,
                self.theme.text,
                Face::Ui,
            );

            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                logout,
                "注销",
                Some(Icon::Logout),
                false,
                self.hover == Hit::Logout,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                reboot,
                "重启",
                Some(Icon::Refresh),
                false,
                self.hover == Hit::Reboot,
            );
            ui::button(
                canvas,
                &mut self.text,
                &self.theme,
                power_off,
                "关机",
                Some(Icon::Shutdown),
                false,
                self.hover == Hit::PowerOff,
            );
        }
        self.sync_menu_ime_spot(search_caret);
    }

    fn draw_ctx(&mut self) {
        let canvas = &mut self.ctx_canvas;
        let full = Rect::new(0, 0, CTX_WIDTH, CTX_HEIGHT);
        let r = self.theme.radius_popup();
        draw_vortex_popup_shell(canvas, &self.theme, full, r, true);

        let items = if self.ctx_is_ime {
            let mode_lbl = if !self.config.ime_enabled {
                "启用中文输入法"
            } else if self.ime_chinese {
                "切换为 英文模式"
            } else {
                "切换为 中文模式"
            };
            let scheme_lbl = match self.config.ime_scheme.as_str() {
                "xiaohe" => "方案: 小鹤双拼",
                "ziranma" => "方案: 自然码",
                "microsoft" => "方案: 微软双拼",
                "sogou" => "方案: 搜狗双拼",
                "abc" => "方案: 智能ABC",
                _ => "方案: 全拼",
            };
            let layout_lbl = if self.config.ime_layout == "vertical" {
                "候选栏: 竖排 -> 横排"
            } else {
                "候选栏: 横排 -> 竖排"
            };
            [
                (mode_lbl, Icon::InputDevices),
                (scheme_lbl, Icon::Editor),
                (layout_lbl, Icon::Panel),
                ("输入法设置...", Icon::Settings),
            ]
        } else {
            [
                ("在此打开终端", Icon::Terminal),
                ("打开文件管理器", Icon::Files),
                ("刷新桌面", Icon::Refresh),
                ("桌面与外观设置", Icon::Settings),
            ]
        };
        draw_etched_hsep(canvas, &self.theme, 10, 104, CTX_WIDTH - 20);
        for (idx, (label, ic)) in items.iter().enumerate() {
            let row = ctx_row_rect(idx);
            let hovered = self.hover == Hit::ContextRow(idx);
            let fg = ui::list_item_bg(canvas, &self.theme, row, hovered, false);
            let tint = if ic.is_symbolic() {
                Some(if hovered && self.theme.is_win98() {
                    fg
                } else {
                    self.theme.text_dim
                })
            } else {
                None
            };
            icons::draw(canvas, *ic, row.x + 8, row.y + 7, 16, tint);
            self.text.draw(
                canvas,
                label,
                (row.x + 32) as f32,
                (row.y + 20) as f32,
                12.0,
                fg,
                Face::Ui,
            );
        }
        if canvas.is_argb() {
            canvas.mask_rounded_corners(r);
        }
    }

    fn draw_toast(&mut self) {
        let canvas = &mut self.toast_canvas;
        let (w, h) = (TOAST_WIDTH, TOAST_HEIGHT);
        let r = self.theme.radius_popup();
        draw_vortex_popup_shell(canvas, &self.theme, Rect::new(0, 0, w, h), r, true);
        icons::draw(canvas, Icon::Notify, 14, (h - 28) / 2, 28, None);
        if let Some(toast) = &self.toast {
            self.text.draw(
                canvas,
                &toast.title,
                52.0,
                29.0,
                13.0,
                self.theme.text,
                Face::Bold,
            );
            let clipped = self
                .text
                .truncate(&toast.body, (w - 72) as f32, 11.5, Face::Ui);
            self.text.draw(
                canvas,
                &clipped,
                52.0,
                50.0,
                11.5,
                self.theme.text_dim,
                Face::Ui,
            );
        }
        let cx = w - 22;
        let cy = 14;
        canvas.line(cx, cy, cx + 7, cy + 7, self.theme.text_muted, 1.0);
        canvas.line(cx + 7, cy, cx, cy + 7, self.theme.text_muted, 1.0);
        if canvas.is_argb() {
            canvas.mask_rounded_corners(r);
        }
    }

    fn present_desktop(&mut self) -> Result<()> {
        let dirty = self.desktop_canvas.take_dirty();
        let area = if dirty.is_empty() {
            self.desktop_canvas.bounds()
        } else {
            dirty
        };
        let area = area.intersect(self.desktop_canvas.bounds());
        if area.is_empty() {
            return Ok(());
        }
        if self.comp.active {
            self.comp.invalidate_desktop_blur();
            self.comp.mark_dirty(area);
            return self.x.flush();
        }
        let full = self.desktop_bg == 0;
        if full {
            let id = self.x.generate_id()?;
            self.x.conn.create_pixmap(
                self.x.depth,
                id,
                self.x.root,
                self.desktop_canvas.width as u16,
                self.desktop_canvas.height as u16,
            )?;
            self.desktop_bg = id;
        }
        let upload = if full {
            self.desktop_canvas.bounds()
        } else {
            area
        };
        self.x
            .put_canvas(self.desktop_bg, &self.desktop_canvas, upload)?;
        self.x.conn.change_window_attributes(
            self.desktop,
            &ChangeWindowAttributesAux::new().background_pixmap(self.desktop_bg),
        )?;
        self.x.conn.clear_area(
            false,
            self.desktop,
            area.x as i16,
            area.y as i16,
            area.w as u16,
            area.h as u16,
        )?;
        self.x.flush()
    }

    fn present_taskbar(&mut self) -> Result<()> {
        let dirty = self.taskbar_canvas.take_dirty();
        let area = if dirty.is_empty() {
            self.taskbar_canvas.bounds()
        } else {
            dirty
        };
        if self.comp.active {
            self.comp.mark_dirty(Rect::new(
                area.x,
                self.taskbar_y() + area.y,
                area.w,
                area.h,
            ));
            return Ok(());
        }
        self.x
            .put_canvas(self.taskbar, &self.taskbar_canvas, area)?;
        self.x.flush()
    }

    fn present_menu(&mut self) -> Result<()> {
        let area = self.menu_canvas.bounds();
        self.menu_canvas.take_dirty();
        if self.comp.active {
            let r = if self.config.panel_start_fullscreen {
                area
            } else {
                Rect::new(self.menu_x(), self.menu_y(), area.w, area.h).expand(16)
            };
            self.comp.mark_dirty(r);
            return Ok(());
        }
        self.x.put_canvas(self.menu, &self.menu_canvas, area)?;
        self.x.flush()
    }

    fn present_ctx(&mut self) -> Result<()> {
        let area = self.ctx_canvas.bounds();
        self.ctx_canvas.take_dirty();
        if self.comp.active {
            let (cx, cy) = self.ctx_pos;
            self.comp
                .mark_dirty(Rect::new(i32::from(cx), i32::from(cy), area.w, area.h).expand(16));
            return Ok(());
        }
        self.x.put_canvas(self.ctx_menu, &self.ctx_canvas, area)?;
        self.x.flush()
    }

    fn present_toast(&mut self) -> Result<()> {
        let area = self.toast_canvas.bounds();
        self.toast_canvas.take_dirty();
        if self.comp.active {
            let tx = i32::from(self.x.width) - TOAST_WIDTH - 12;
            self.comp
                .mark_dirty(Rect::new(tx, self.toast_y(), area.w, area.h).expand(28));
            return Ok(());
        }
        self.x
            .put_canvas(self.toast_window, &self.toast_canvas, area)?;
        self.x.flush()
    }

    fn draw_vol_popup(&mut self) {
        let canvas = &mut self.vol_canvas;
        let w = VOL_POPUP_WIDTH;
        let h = VOL_POPUP_HEIGHT;
        let is_win98 = self.theme.is_win98();
        let ctrl_r = self.theme.radius_control();
        let r = self.theme.radius_popup();
        draw_vortex_popup_shell(canvas, &self.theme, Rect::new(0, 0, w, h), r, true);

        let muted = self.config.sound_muted;
        let vol = self.config.sound_master_volume.clamp(0, 100);

        self.text.draw(
            canvas,
            "音频输出 · 主音量",
            16.0,
            28.0,
            12.5,
            self.theme.text,
            Face::Bold,
        );
        let pct_txt = if muted {
            "已静音".to_string()
        } else {
            format!("{vol}%")
        };
        self.text.draw_right(
            canvas,
            &pct_txt,
            (w - 16) as f32,
            28.0,
            12.5,
            if muted {
                self.theme.danger
            } else {
                self.theme.accent
            },
            Face::Bold,
        );

        let mute_r = vol_popup_mute_rect();
        let mute_hov = self.hover == Hit::VolMute;
        let mute_pressed = muted || ui::is_rect_pressed(mute_r);
        let mute_shift = i32::from(is_win98 && mute_pressed);
        if is_win98 {
            canvas.fill(
                mute_r,
                if mute_pressed {
                    self.theme.window_alt
                } else if mute_hov {
                    self.theme.panel_hover
                } else {
                    self.theme.panel
                },
            );
            if mute_pressed {
                ui::sunken_bevel(canvas, &self.theme, mute_r);
            } else {
                ui::raised_bevel(canvas, &self.theme, mute_r);
            }
        } else if muted || mute_hov {
            canvas.rounded_rect(
                mute_r,
                ctrl_r,
                if muted {
                    crate::draw::mix(self.theme.panel_hover, self.theme.danger, 0.20)
                } else {
                    self.theme.panel_hover
                },
                1.0,
            );
        }
        icons::draw(
            canvas,
            Icon::for_volume(vol, muted),
            mute_r.x + (mute_r.w - 18) / 2 + mute_shift,
            mute_r.y + (mute_r.h - 18) / 2 + mute_shift,
            18,
            Some(if muted {
                self.theme.text_muted
            } else {
                self.theme.text
            }),
        );

        let slider_r = vol_popup_slider_rect();
        let progress = if muted { 0.0 } else { vol as f32 / 100.0 };
        ui::slider(
            canvas,
            &self.theme,
            slider_r,
            progress,
            self.hover == Hit::VolSlider,
            self.vol_dragging,
        );

        draw_etched_hsep(canvas, &self.theme, 14, 84, w - 28);

        let sfx_r = vol_popup_sfx_rect();
        if self.hover == Hit::VolSfx {
            canvas.rounded_rect(sfx_r, ctrl_r, self.theme.panel_hover, 0.7);
        }
        self.text.draw(
            canvas,
            "提示音效",
            (sfx_r.x + 6) as f32,
            (sfx_r.y + 19) as f32,
            12.0,
            self.theme.text_dim,
            Face::Ui,
        );
        let sw_r = Rect::new(sfx_r.right() - 44, sfx_r.y + 3, 40, 22);
        ui::toggle_switch_with_id(
            canvas,
            &self.theme,
            Hit::VolSfx,
            sw_r,
            self.config.sound_enabled,
            self.hover == Hit::VolSfx,
        );

        let cfg_r = vol_popup_settings_rect();
        ui::button(
            canvas,
            &mut self.text,
            &self.theme,
            cfg_r,
            "声音设置 ›",
            None,
            false,
            self.hover == Hit::VolSettings,
        );
        if canvas.is_argb() {
            canvas.mask_rounded_corners(self.theme.radius_popup());
        }
    }

    fn present_vol_popup(&mut self) -> Result<()> {
        let area = self.vol_canvas.bounds();
        self.vol_canvas.take_dirty();
        if self.comp.active {
            let (vx, vy) = self.vol_popup_pos();
            self.comp
                .mark_dirty(Rect::new(i32::from(vx), i32::from(vy), area.w, area.h).expand(16));
            return Ok(());
        }
        self.x.put_canvas(self.vol_popup, &self.vol_canvas, area)?;
        self.x.flush()
    }

    fn active_anim_dirty_bounds(&self) -> Option<Rect> {
        let is_vortex = self.theme.is_vortexglass();
        let mut anim_dirty: Option<Rect> = None;
        for &w in &self.z_stack {
            if self.comps.get(&w).is_some_and(|cw| cw.anim.is_some()) {
                if let Some((r, is_max)) = self
                    .windows
                    .iter()
                    .find(|m| m.window == w)
                    .map(|m| self.window_rect_for(m))
                {
                    let b = window_damage_bounds(r, is_vortex, is_max).expand(28);
                    anim_dirty = Some(anim_dirty.map_or(b, |p| p.union(b)));
                }
            }
        }
        for g in &self.comp.ghosts {
            let target = g.to_taskbar.unwrap_or(g.from);
            let b = g.from.union(target).expand(28);
            anim_dirty = Some(anim_dirty.map_or(b, |p| p.union(b)));
        }
        anim_dirty
    }

    fn mark_tick_dirty(
        &mut self,
        tick_out: crate::compositor::TickOutcome,
        pre_anim_dirty: Option<Rect>,
    ) {
        if tick_out.peek_or_flip_changed {
            self.comp.mark_full_dirty();
            return;
        }
        if tick_out.snap_changed {
            if let Some(sr) = self.comp.snap_rect {
                self.comp.mark_dirty(sr.expand(16));
            }
        }
        if tick_out.menu_changed {
            if self.config.panel_start_fullscreen {
                self.comp.mark_full_dirty();
            } else {
                self.comp.mark_dirty(
                    Rect::new(
                        self.menu_x(),
                        self.menu_y(),
                        self.menu_canvas.width as i32,
                        self.menu_canvas.height as i32,
                    )
                    .expand(24),
                );
            }
        }
        if tick_out.ctx_changed {
            let (cx, cy) = self.ctx_pos;
            self.comp.mark_dirty(
                Rect::new(i32::from(cx), i32::from(cy), CTX_WIDTH, CTX_HEIGHT).expand(20),
            );
        }
        if tick_out.vol_changed {
            let (vx, vy) = self.vol_popup_pos();
            self.comp.mark_dirty(
                Rect::new(i32::from(vx), i32::from(vy), VOL_POPUP_WIDTH, VOL_POPUP_HEIGHT)
                    .expand(20),
            );
        }
        if tick_out.toast_changed {
            let tx = i32::from(self.x.width) - TOAST_WIDTH - 12;
            self.comp.mark_dirty(
                Rect::new(tx, self.toast_y(), TOAST_WIDTH + 32, TOAST_HEIGHT).expand(24),
            );
        }
        if tick_out.win_anims_or_ghosts {
            if let Some(b) = pre_anim_dirty {
                self.comp.mark_dirty(b);
            }
            if let Some(b) = self.active_anim_dirty_bounds() {
                self.comp.mark_dirty(b);
            }
        }
    }

    fn window_glass_top(&self, cw: &CompWindow, m_h: i32) -> i32 {
        if !self.theme.is_vortexglass() || cw.depth != 32 {
            0
        } else {
            cw.glass_extents
                .map(|e| e[0])
                .unwrap_or(VORTEX_TITLEBAR_HEIGHT)
                .clamp(0, m_h)
        }
    }

    fn window_rect_for(&self, m: &Managed) -> (Rect, bool) {
        let scr_w = i32::from(self.x.width);
        let work_h = i32::from(self.x.height) - self.panel_height();
        let rect = Rect::new(
            i32::from(m.x),
            i32::from(m.y),
            i32::from(m.width),
            i32::from(m.height),
        );
        let is_max = rect.w >= scr_w - 2 && rect.h >= work_h - 2;
        (rect, is_max)
    }

    fn is_internal_window(&self, w: Window) -> bool {
        w == self.x.root
            || w == self.desktop
            || w == self.taskbar
            || w == self.thumb_window
            || w == self.menu
            || w == self.ctx_menu
            || w == self.vol_popup
            || w == self.toast_window
            || self.comp.overlay == Some(w)
    }

    fn mark_window_dirty(&mut self, window: Window) {
        if !self.comp.active {
            return;
        }
        if let Some((rect, is_max)) = self
            .windows
            .iter()
            .find(|m| m.window == window && !m.minimized)
            .map(|m| self.window_rect_for(m))
        {
            self.comp.mark_dirty(
                window_damage_bounds(rect, self.theme.is_vortexglass(), is_max).expand(4),
            );
        }
    }

    fn track_override_window(&mut self, window: Window, rect: Rect, depth: u8) {
        if !self.comp.active
            || self.is_internal_window(window)
            || self.windows.iter().any(|w| w.window == window)
            || rect.is_empty()
        {
            return;
        }
        self.remove_override_window(window);
        let _ = self.x.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        );
        let glass_extents = self.x.get_glass_extents(window);
        let mut cw = CompWindow::new(window, depth, glass_extents);
        attach_window_pixmap(&self.x, &mut cw, rect.w as u16, rect.h as u16, depth);
        self.override_wins.push(OverrideWin { window, rect, cw });
        self.comp.mark_dirty(rect.expand(24));
    }

    fn remove_override_window(&mut self, window: Window) -> bool {
        let Some(pos) = self.override_wins.iter().position(|o| o.window == window) else {
            return false;
        };
        let mut ov = self.override_wins.remove(pos);
        detach_window_resources(&self.x, &mut ov.cw);
        if self.comp.active {
            self.comp.mark_dirty(ov.rect.expand(24));
        }
        true
    }

    fn sync_initial_override_windows(&mut self) {
        if !self.comp.active {
            return;
        }
        for (w, wx, wy, ww, wh, depth) in self.x.query_override_children() {
            self.track_override_window(
                w,
                Rect::new(i32::from(wx), i32::from(wy), i32::from(ww), i32::from(wh)),
                depth,
            );
        }
    }

    fn composite_frame(&mut self) -> Result<()> {
        if !self.comp.active {
            return Ok(());
        }
        let Some(overlay) = self.comp.overlay else {
            return Ok(());
        };
        let now = Instant::now();
        let is_vortex = self.theme.is_vortexglass();
        let scr = Rect::new(0, 0, i32::from(self.x.width), i32::from(self.x.height));

        // Only capture occluded background windows when actually exposed or previewed.
        let capture_all = self.comp.alt_tab.is_some()
            || self.comp.flip3d.is_some()
            || self.comp.peek_factor > 0.01;
        self.capture_keep_buf.clear();
        if !capture_all {
            let mut exposed = std::mem::take(&mut self.comp.need_region);
            exposed.reset_to_slice(&[scr]);
            for &w in self.z_stack.iter().rev() {
                let Some(m) = self.windows.iter().find(|m| m.window == w && !m.minimized) else {
                    continue;
                };
                let Some(cw) = self.comps.get(&w) else {
                    continue;
                };
                let is_thumb = self
                    .comp
                    .thumb
                    .as_ref()
                    .is_some_and(|t| t.target_window == w);
                let (rect, is_max) = self.window_rect_for(m);
                let bounds = if cw.anim.is_some() {
                    rect.expand(24)
                } else {
                    window_damage_bounds(rect, is_vortex, is_max)
                };
                if !cw.has_pixels || is_thumb || exposed.intersects_rect(bounds.expand(AERO_BLUR_RADIUS)) {
                    self.capture_keep_buf.push(w);
                }
                if cw.has_pixels
                    && cw.anim.is_none()
                    && cw.pw == usize::from(m.width)
                    && cw.ph == usize::from(m.height)
                {
                    let glass_top = self.window_glass_top(cw, i32::from(m.height));
                    let is_popup = cw.glass_extents.is_some_and(|e| e[1] == -1);
                    if !is_popup && (cw.depth != 32 || (is_vortex && glass_top > 0)) {
                        let opaque_rect = Rect::new(
                            i32::from(m.x),
                            i32::from(m.y) + glass_top,
                            i32::from(m.width),
                            (i32::from(m.height) - glass_top).max(0),
                        );
                        exposed.subtract_rect(opaque_rect);
                    }
                }
            }
            self.comp.need_region = exposed;
        }

        let captured = {
            let keep = &self.capture_keep_buf;
            capture_dirty_windows(
                &self.x,
                self.comp.shm_read.as_ref(),
                self.comps.values_mut(),
                now,
                |cw| capture_all || keep.contains(&cw.window),
            )
        };
        for (win, local_area, first_capture) in captured {
            if let Some((rect, is_max)) = self
                .windows
                .iter()
                .find(|m| m.window == win)
                .map(|m| self.window_rect_for(m))
            {
                let dmg = if first_capture {
                    window_damage_bounds(rect, is_vortex, is_max)
                } else {
                    Rect::new(
                        rect.x + local_area.x - AERO_BLUR_RADIUS,
                        rect.y + local_area.y - AERO_BLUR_RADIUS,
                        local_area.w + AERO_BLUR_RADIUS * 2,
                        local_area.h + AERO_BLUR_RADIUS * 2,
                    )
                };
                self.comp.mark_dirty(dmg);
            }
        }

        for ov in &mut self.override_wins {
            if let Some((local_area, first_capture)) =
                capture_one_window(&self.x, self.comp.shm_read.as_ref(), &mut ov.cw, now)
            {
                let dmg = if first_capture {
                    ov.rect.expand(24)
                } else {
                    Rect::new(
                        ov.rect.x + local_area.x - AERO_BLUR_RADIUS,
                        ov.rect.y + local_area.y - AERO_BLUR_RADIUS,
                        local_area.w + AERO_BLUR_RADIUS * 2,
                        local_area.h + AERO_BLUR_RADIUS * 2,
                    )
                };
                self.comp.mark_dirty(dmg);
            }
        }

        if !self.comp.is_dirty() || scr.is_empty() {
            return Ok(());
        }

        let mut dirty_rects = std::mem::take(&mut self.dirty_rects_buf);
        self.comp.dirty.take_into(scr, &mut dirty_rects);
        if dirty_rects.is_empty() {
            self.dirty_rects_buf = dirty_rects;
            return Ok(());
        }
        let Some(mut dst) = self.comp.canvas.take() else {
            self.dirty_rects_buf = dirty_rects;
            return Ok(());
        };

        let (live_blur, with_sheen) = match self.config.wm_compositor_quality {
            CompositorQuality::Quality => (true, true),
            CompositorQuality::Balanced => (self.wm_drag.is_none(), true),
            CompositorQuality::Fast => (false, false),
        };
        if is_vortex || self.comp.flip3d.is_some() {
            self.comp.ensure_desktop_blur(&self.desktop_canvas);
        }
        if self.comp_shm_pending_sync {
            if let Err(err) = self.x.sync() {
                self.comp.canvas = Some(dst);
                dirty_rects.clear();
                self.dirty_rects_buf = dirty_rects;
                return Err(err);
            }
            self.comp_shm_pending_sync = false;
        }

        for &dirty_area in &dirty_rects {
            if !self.try_fast_top_opaque_blit(&mut dst, dirty_area) {
                if self.comp.flip3d.is_some() {
                    dst.with_clip(dirty_area, |d| {
                        d.copy_rect_from(
                            self.desktop_canvas.pixels(),
                            self.desktop_canvas.width,
                            dirty_area,
                        );
                        let entries = build_switcher_entries(&self.windows, &self.comps);
                        if let Some(ref flip) = self.comp.flip3d {
                            draw_flip3d_scene(
                                d,
                                &self.comp.desktop_blur_cache,
                                &mut self.text,
                                &self.theme,
                                &entries,
                                flip,
                                now,
                            );
                        }
                    });
                    self.composite_taskbar(&mut dst, dirty_area, live_blur, with_sheen);
                    self.composite_override_windows(&mut dst, dirty_area, live_blur);
                } else {
                    self.composite_scene_occlusion_culled(
                        &mut dst,
                        dirty_area,
                        now,
                        live_blur,
                        with_sheen,
                    );
                    dst.with_clip(dirty_area, |d| {
                        self.composite_ghosts(d, now);
                        self.composite_snap_preview(d);
                    });
                    self.composite_taskbar(&mut dst, dirty_area, live_blur, with_sheen);
                    self.composite_popups(&mut dst, dirty_area);
                    self.composite_override_windows(&mut dst, dirty_area, live_blur);
                    if let Some(selected) = self.comp.alt_tab.as_ref().map(|a| a.selected) {
                        let entries = build_switcher_entries(&self.windows, &self.comps);
                        dst.with_clip(dirty_area, |d| {
                            draw_alt_tab_hud(
                                d,
                                &mut self.text,
                                &self.theme,
                                &mut self.comp.blur_scratch,
                                &entries,
                                selected,
                            );
                        });
                    }
                }
            }
            if let Err(err) = self.x.put_canvas_unsynced(overlay, &dst, dirty_area) {
                self.comp.canvas = Some(dst);
                dirty_rects.clear();
                self.dirty_rects_buf = dirty_rects;
                return Err(err);
            }
        }

        if dst.shm_seg().is_some() {
            self.comp_shm_pending_sync = true;
        }
        let flush_res = self.x.flush();
        self.comp.canvas = Some(dst);
        dirty_rects.clear();
        self.dirty_rects_buf = dirty_rects;
        self.last_composite = Instant::now();
        flush_res
    }

    fn try_fast_top_opaque_blit(&self, dst: &mut Canvas, dirty: Rect) -> bool {
        if self.comp.flip3d.is_some()
            || self.comp.alt_tab.is_some()
            || self.comp.snap_alpha > 0.01
            || !self.comp.ghosts.is_empty()
            || self.comp.thumb.is_some()
            || self.comp.peek_factor > 0.01
            || self.menu_open
            || self.ctx_open
            || self.vol_open
            || self.comp.menu_anim > 0.01
            || self.comp.ctx_anim > 0.01
            || self.comp.vol_anim > 0.01
            || self.comp.toast_anim > 0.01
            || self
                .override_wins
                .iter()
                .any(|o| o.cw.has_pixels && !o.rect.expand(20).intersect(dirty).is_empty())
        {
            return false;
        }
        let Some(&top_w) = self.z_stack.last() else {
            return false;
        };
        let Some(m) = self.windows.iter().find(|w| w.window == top_w && !w.minimized) else {
            return false;
        };
        let Some(cw) = self.comps.get(&top_w).filter(|c| {
            c.has_pixels
                && c.anim.is_none()
                && c.pw == usize::from(m.width)
                && c.ph == usize::from(m.height)
                && c.glass_extents.is_none_or(|e| e[1] != -1)
        }) else {
            return false;
        };
        let tb = Rect::new(
            0,
            self.taskbar_y(),
            i32::from(self.x.width),
            self.panel_height(),
        );
        if !dirty.intersect(tb).is_empty() {
            return false;
        }
        let glass_top = self.window_glass_top(cw, i32::from(m.height));
        if cw.depth == 32 && glass_top == 0 {
            return false;
        }
        let opaque_rect = Rect::new(
            i32::from(m.x),
            i32::from(m.y) + glass_top,
            i32::from(m.width),
            (i32::from(m.height) - glass_top).max(0),
        );
        if opaque_rect.is_empty()
            || dirty.x < opaque_rect.x
            || dirty.y < opaque_rect.y
            || dirty.right() > opaque_rect.right()
            || dirty.bottom() > opaque_rect.bottom()
        {
            return false;
        }
        dst.with_clip(dirty, |c| {
            blit_window_pixels_split(
                c,
                &cw.pixels,
                cw.pw,
                cw.ph,
                i32::from(m.x),
                i32::from(m.y),
                255,
                0,
                cw.depth == 32,
                glass_top,
            );
        });
        true
    }

    fn composite_scene_occlusion_culled(
        &mut self,
        dst: &mut Canvas,
        dirty_area: Rect,
        now: Instant,
        live_blur: bool,
        with_sheen: bool,
    ) {
        let peek_factor = self.comp.peek_factor;
        let peek_target = self
            .comp
            .thumb
            .as_ref()
            .filter(|t| t.pointer_inside)
            .map(|t| t.target_window);
        let is_vortex = self.theme.is_vortexglass();
        let scr_w = i32::from(self.x.width);

        // Reuse plan slots without per-frame allocation.
        let mut plans = std::mem::take(&mut self.comp_plan_buf);
        let mut used = 0usize;
        for &w in &self.z_stack {
            let Some((base_rect, is_max)) = self
                .windows
                .iter()
                .find(|m| m.window == w && !m.minimized)
                .map(|m| self.window_rect_for(m))
            else {
                continue;
            };
            let Some(cw) = self.comps.get(&w).filter(|c| c.has_pixels) else {
                continue;
            };
            let (draw_rect, base_op, is_anim) = match cw.anim {
                Some(a) => {
                    let t = smootherstep(a.progress(now));
                    match a.kind {
                        AnimKind::Open => {
                            let scale = 0.90 + 0.10 * t;
                            let dy = ((1.0 - t) * 14.0).round() as i32;
                            let op = (t * 255.0).round().clamp(0.0, 255.0) as u8;
                            (scaled_rect_around_center(base_rect, scale, dy), op, true)
                        }
                        AnimKind::Restore { taskbar_btn } => {
                            let op = (70.0 + 185.0 * t).round().clamp(0.0, 255.0) as u8;
                            (lerp_rect(taskbar_btn, base_rect, t), op, true)
                        }
                    }
                }
                None => (base_rect, 255, false),
            };
            let cull_bounds = if is_anim {
                draw_rect.expand(16)
            } else {
                window_damage_bounds(draw_rect, is_vortex, is_max)
            };
            let peeking_out = peek_factor > 0.01 && peek_target != Some(w);
            let eff_op = if peeking_out {
                ((base_op as f32) * (1.0 - peek_factor))
                    .round()
                    .clamp(0.0, 255.0) as u8
            } else {
                base_op
            };
            let glass_top = if is_vortex && cw.depth == 32 {
                cw.glass_extents
                    .map(|e| e[0])
                    .unwrap_or(VORTEX_TITLEBAR_HEIGHT)
                    .clamp(1, draw_rect.h)
            } else {
                0
            };
            let has_vortex_glass = is_vortex && !is_anim && cw.depth == 32 && eff_op == 255;
            let same_size = draw_rect.w == cw.pw as i32 && draw_rect.h == cw.ph as i32;
            let is_frameless_popup = cw.glass_extents.is_some_and(|e| e[1] == -1);
            let is_opaque = !is_anim
                && !peeking_out
                && same_size
                && eff_op == 255
                && !is_frameless_popup
                && (cw.depth != 32 || (is_vortex && glass_top > 0));
            let opaque_rect = if is_opaque {
                Rect::new(
                    draw_rect.x,
                    draw_rect.y + glass_top,
                    draw_rect.w,
                    (draw_rect.h - glass_top).max(0),
                )
            } else {
                Rect::default()
            };

            if used >= plans.len() {
                plans.push(WinCompPlan::default());
            }
            let p = &mut plans[used];
            p.reset();
            p.window = w;
            p.draw_rect = draw_rect;
            p.cull_bounds = cull_bounds;
            p.opaque_rect = opaque_rect;
            p.is_max = is_max;
            p.is_anim = is_anim;
            p.peeking_out = peeking_out;
            p.has_vortex_glass = has_vortex_glass;
            p.active = self.active == Some(w);
            p.glass_top = glass_top;
            p.eff_op = eff_op;
            p.cw_pw = cw.pw;
            p.cw_ph = cw.ph;
            p.cw_depth = cw.depth;
            if has_vortex_glass {
                let glass_x = if is_max { 0 } else { draw_rect.x - 4 };
                let glass_w = if is_max { scr_w } else { draw_rect.w + 8 };
                if glass_top > 1 {
                    p.glass_strips[p.glass_count] =
                        Rect::new(glass_x, draw_rect.y + 1, glass_w, glass_top - 1);
                    p.glass_count += 1;
                }
                if !is_max {
                    let client_y = draw_rect.y + glass_top;
                    let client_h = (draw_rect.h - glass_top).max(0);
                    if client_h > 0 {
                        p.glass_strips[p.glass_count] =
                            Rect::new(draw_rect.x - 4, client_y, 3, client_h);
                        p.glass_count += 1;
                        p.glass_strips[p.glass_count] =
                            Rect::new(draw_rect.right() + 1, client_y, 3, client_h);
                        p.glass_count += 1;
                    }
                    p.glass_strips[p.glass_count] =
                        Rect::new(glass_x, draw_rect.bottom(), glass_w, 4);
                    p.glass_count += 1;
                }
            }
            used += 1;
        }

        // Single-pass front-to-back exact occlusion culling + blur-halo preservation.
        plan_window_occlusion(
            dirty_area,
            live_blur,
            self.wm_drag.is_some(),
            &mut plans[..used],
            &mut self.comp.desktop_need,
            &mut self.comp.strip_scratch,
        );

        // Step 1: Copy ONLY exposed desktop wallpaper regions into `dst`.
        for &dr in &self.comp.desktop_need.rects {
            dst.with_clip(dr, |d| {
                d.copy_rect_from(
                    self.desktop_canvas.pixels(),
                    self.desktop_canvas.width,
                    dr,
                );
            });
        }

        // Step 2: Back-to-front draw of only visible window regions + non-recursive local glass blur.
        let mut ft = self.comp.frost_tmp.take();
        for p in &plans[..used] {
            if p.win_vis.is_empty() {
                continue;
            }
            if p.peeking_out {
                for &vr in &p.win_vis {
                    dst.with_clip(vr, |d| {
                        draw_aero_peek_sheet(d, p.draw_rect, self.theme.accent, peek_factor);
                    });
                }
                if p.eff_op == 0 {
                    continue;
                }
            }

            if p.has_vortex_glass {
                for &(gr, has_lower) in &p.glass_vis {
                    frost_clip_from_dst(
                        dst,
                        ft.as_mut(),
                        &mut self.comp.blur_scratch,
                        &self.comp.desktop_blur_cache,
                        self.desktop_canvas.width,
                        gr,
                        dirty_area,
                        has_lower,
                        0,
                    );
                }
                let mut fc = self.comps.get_mut(&p.window).and_then(|c| c.frame_cache.take());
                for &vr in &p.win_vis {
                    dst.with_clip(vr, |d| {
                        draw_vortex_frame_chrome_cached(
                            d,
                            p.draw_rect,
                            p.glass_top,
                            &self.theme,
                            p.active,
                            p.is_max,
                            with_sheen,
                            &mut fc,
                        );
                    });
                }
                if let Some(cw) = self.comps.get_mut(&p.window) {
                    cw.frame_cache = fc;
                }
            } else if !p.is_anim && !p.is_max && p.eff_op > 128 {
                for &vr in &p.win_vis {
                    dst.with_clip(vr, |d| {
                        d.rounded_drop_shadow(
                            p.draw_rect,
                            self.theme.radius_window(),
                            if p.active { 10 } else { 6 },
                            2,
                            if p.active { 0.28 } else { 0.15 },
                        );
                    });
                }
            }

            if let Some(cw) = self.comps.get(&p.window) {
                let split_y = if is_vortex && !p.is_anim && p.cw_depth == 32 {
                    p.glass_top
                } else {
                    0
                };
                for &vr in &p.win_vis {
                    dst.with_clip(vr, |d| {
                        if p.is_anim
                            || p.draw_rect.w != p.cw_pw as i32
                            || p.draw_rect.h != p.cw_ph as i32
                        {
                            blit_scaled_window(
                                d,
                                &cw.pixels,
                                p.cw_pw,
                                p.cw_ph,
                                p.draw_rect,
                                p.eff_op,
                            );
                        } else {
                            blit_window_pixels_split(
                                d,
                                &cw.pixels,
                                p.cw_pw,
                                p.cw_ph,
                                p.draw_rect.x,
                                p.draw_rect.y,
                                p.eff_op,
                                0,
                                p.cw_depth == 32,
                                split_y,
                            );
                        }
                    });
                }
            }
        }
        self.comp.frost_tmp = ft;
        self.comp_plan_buf = plans;
    }

    fn composite_ghosts(&self, dst: &mut Canvas, now: Instant) {
        for ghost in &self.comp.ghosts {
            let t = smootherstep(ghost.progress(now));
            if let Some(tb) = ghost.to_taskbar {
                let target = lerp_rect(ghost.from, tb, t);
                let op = ((1.0 - t * 0.78) * 255.0).round().clamp(0.0, 255.0) as u8;
                blit_scaled_window(dst, &ghost.pixels, ghost.pw, ghost.ph, target, op);
            } else {
                let scale = 1.0 - 0.10 * t;
                let dy = (t * 12.0).round() as i32;
                let target = scaled_rect_around_center(ghost.from, scale, dy);
                let op = ((1.0 - t) * 255.0).round().clamp(0.0, 255.0) as u8;
                blit_scaled_window(dst, &ghost.pixels, ghost.pw, ghost.ph, target, op);
            }
        }
    }

    fn composite_snap_preview(&self, dst: &mut Canvas) {
        let a = self.comp.snap_alpha;
        if a <= 0.01 {
            return;
        }
        let Some(sr) = self.comp.snap_rect else {
            return;
        };
        dst.rounded_rect(sr, 6, self.theme.accent, 0.22 * a);
        dst.rounded_border(sr, 6, 0xffffff, 0.86 * a, 2);
        if sr.w > 8 && sr.h > 8 {
            dst.rounded_border(sr.inset(2), 4, self.theme.accent, 0.65 * a, 1);
        }
    }

    fn composite_taskbar(
        &mut self,
        dst: &mut Canvas,
        dirty_area: Rect,
        live_blur: bool,
        with_sheen: bool,
    ) {
        let tb = Rect::new(
            0,
            self.taskbar_y(),
            i32::from(self.x.width),
            self.panel_height(),
        );
        let tb_dirty = tb.intersect(dirty_area);
        if tb_dirty.is_empty() {
            return;
        }
        if self.theme.is_vortexglass() {
            let has_overlap = if self.comp.flip3d.is_some() {
                true
            } else if !live_blur {
                false
            } else {
                let tb_ex = tb_dirty
                    .expand(AERO_BLUR_RADIUS)
                    .intersect(dirty_area)
                    .intersect(dst.bounds());
                self.comp.snap_alpha > 0.01
                    || !self.comp.ghosts.is_empty()
                    || self.z_stack.iter().any(|&w| {
                        let Some(m) = self.windows.iter().find(|m| m.window == w && !m.minimized)
                        else {
                            return false;
                        };
                        if !self.comps.get(&w).is_some_and(|c| c.has_pixels) {
                            return false;
                        }
                        let (r, is_max) = self.window_rect_for(m);
                        !window_damage_bounds(r, true, is_max).intersect(tb_ex).is_empty()
                    })
            };
            let mut ft = self.comp.frost_tmp.take();
            frost_clip_from_dst(
                dst,
                ft.as_mut(),
                &mut self.comp.blur_scratch,
                &self.comp.desktop_blur_cache,
                self.desktop_canvas.width,
                tb_dirty,
                dirty_area,
                has_overlap,
                0,
            );
            self.comp.frost_tmp = ft;
            dst.with_clip(tb_dirty, |d| {
                if with_sheen {
                    crate::compositor::finish_shell_glass_overlay(d, tb, &self.theme, 130);
                } else {
                    let (tint, tint_a) = self.theme.glass_tint(true);
                    d.fill_u8(tb, tint, tint_a);
                }
            });
        }
        dst.with_clip(tb_dirty, |d| {
            composite_shell_layer(
                d,
                &self.taskbar_canvas,
                0,
                tb.y,
                255,
                self.taskbar_canvas.is_argb(),
            );
        });
    }

    fn composite_override_windows(
        &mut self,
        dst: &mut Canvas,
        dirty_area: Rect,
        live_blur: bool,
    ) {
        if self.override_wins.is_empty() {
            return;
        }
        let is_vortex = self.theme.is_vortexglass();
        let mut ft = self.comp.frost_tmp.take();
        for ov in &self.override_wins {
            if !ov.cw.has_pixels || ov.cw.pixels.is_empty() {
                continue;
            }
            let r = Rect::new(
                ov.rect.x,
                ov.rect.y,
                (ov.cw.pw as i32).min(ov.rect.w),
                (ov.cw.ph as i32).min(ov.rect.h),
            );
            if r.is_empty() || r.expand(20).intersect(dirty_area).is_empty() {
                continue;
            }
            let glass_popup =
                is_vortex && ov.cw.depth == 32 && ov.cw.glass_extents.is_some_and(|e| e[1] == -1);
            if glass_popup {
                let corner_r = ov.cw.glass_extents.map_or(6, |e| e[4].clamp(0, 16));
                let clip = r.intersect(dirty_area).intersect(dst.bounds());
                if !clip.is_empty() {
                    if live_blur {
                        let ex = clip
                            .expand(AERO_BLUR_RADIUS)
                            .intersect(dirty_area)
                            .intersect(dst.bounds());
                        if let Some(ft_ref) = ft.as_mut() {
                            ft_ref.with_clip(ex, |f| {
                                f.copy_rect_from(dst.pixels(), dst.width, ex);
                                f.blur_rect(ex, AERO_BLUR_RADIUS, &mut self.comp.blur_scratch);
                            });
                            dst.with_clip(dirty_area, |d| {
                                d.rounded_drop_shadow(r, corner_r, 10, 3, 0.34);
                            });
                            copy_rounded_from_buf(dst, ft_ref.pixels(), ft_ref.width, r, clip, corner_r);
                        }
                    } else {
                        dst.with_clip(dirty_area, |d| {
                            d.rounded_drop_shadow(r, corner_r, 10, 3, 0.34);
                        });
                        if self.comp.desktop_blur_cache.len() >= dst.width * dst.height {
                            copy_rounded_from_buf(
                                dst,
                                &self.comp.desktop_blur_cache,
                                self.desktop_canvas.width,
                                r,
                                clip,
                                corner_r,
                            );
                        }
                    }
                }
            } else if ov.cw.depth != 32 {
                dst.with_clip(dirty_area, |d| {
                    d.rounded_drop_shadow(r, 0, 8, 2, 0.25);
                });
            }
            dst.with_clip(dirty_area, |d| {
                blit_window_pixels_split(
                    d,
                    &ov.cw.pixels,
                    ov.cw.pw,
                    ov.cw.ph,
                    r.x,
                    r.y,
                    255,
                    0,
                    ov.cw.depth == 32,
                    0,
                );
            });
        }
        self.comp.frost_tmp = ft;
    }

    fn composite_popups(&mut self, dst: &mut Canvas, dirty_area: Rect) {
        let panel_top = self.config.panel_position == PanelPosition::Top;
        let is_vortex = self.theme.is_vortexglass();

        if let Some(thumb) = self.comp.thumb.take() {
            if !thumb.rect.expand(18).intersect(dirty_area).is_empty() {
                if let Some(m) = self
                    .windows
                    .iter()
                    .find(|w| w.window == thumb.target_window)
                {
                    let cw = self.comps.get(&m.window).filter(|c| c.has_pixels);
                    dst.with_clip(dirty_area, |d| {
                        draw_thumb_preview(
                            d,
                            &mut self.text,
                            &self.theme,
                            &mut self.comp.blur_scratch,
                            &thumb,
                            &m.title,
                            m.icon,
                            cw.map(|c| c.pixels.as_slice()),
                            cw.map_or(0, |c| c.pw),
                            cw.map_or(0, |c| c.ph),
                        );
                    });
                }
            }
            self.comp.thumb = Some(thumb);
        }

        if self.comp.menu_anim > 0.01 {
            let t = smootherstep(self.comp.menu_anim);
            let fullscreen = self.config.panel_start_fullscreen;
            let (mx, base_y) = if fullscreen {
                (0, 0)
            } else {
                (self.menu_x(), self.menu_y())
            };
            let slide = if fullscreen {
                0
            } else {
                ((1.0 - t) * if panel_top { -12.0 } else { 12.0 }).round() as i32
            };
            let mr = Rect::new(
                mx,
                base_y + slide,
                self.menu_canvas.width as i32,
                self.menu_canvas.height as i32,
            );
            if fullscreen || !mr.expand(18).intersect(dirty_area).is_empty() {
                dst.with_clip(dirty_area, |d| {
                    if is_vortex && !fullscreen {
                        d.rounded_drop_shadow(mr, self.theme.radius_popup(), 12, 3, 0.36 * t);
                        paint_shell_glass_region(
                            d,
                            mr,
                            &self.theme,
                            &mut self.comp.blur_scratch,
                            110,
                        );
                    }
                    composite_shell_layer(
                        d,
                        &self.menu_canvas,
                        mr.x,
                        mr.y,
                        (t * 255.0).round().clamp(0.0, 255.0) as u8,
                        self.menu_canvas.is_argb(),
                    );
                });
            }
        }

        if self.comp.vol_anim > 0.01 {
            let t = smootherstep(self.comp.vol_anim);
            let (vx, vy) = self.vol_popup_pos();
            let slide = ((1.0 - t) * if panel_top { -10.0 } else { 10.0 }).round() as i32;
            let vr = Rect::new(
                i32::from(vx),
                i32::from(vy) + slide,
                VOL_POPUP_WIDTH,
                VOL_POPUP_HEIGHT,
            );
            if !vr.expand(16).intersect(dirty_area).is_empty() {
                dst.with_clip(dirty_area, |d| {
                    if is_vortex {
                        d.rounded_drop_shadow(vr, self.theme.radius_popup(), 10, 3, 0.34 * t);
                        paint_shell_glass_region(
                            d,
                            vr,
                            &self.theme,
                            &mut self.comp.blur_scratch,
                            95,
                        );
                    }
                    composite_shell_layer(
                        d,
                        &self.vol_canvas,
                        vr.x,
                        vr.y,
                        (t * 255.0).round().clamp(0.0, 255.0) as u8,
                        self.vol_canvas.is_argb(),
                    );
                });
            }
        }

        if self.comp.ctx_anim > 0.01 {
            let t = smootherstep(self.comp.ctx_anim);
            let (cx, cy) = self.ctx_pos;
            let slide = ((1.0 - t) * 6.0).round() as i32;
            let cr = Rect::new(i32::from(cx), i32::from(cy) + slide, CTX_WIDTH, CTX_HEIGHT);
            if !cr.expand(16).intersect(dirty_area).is_empty() {
                dst.with_clip(dirty_area, |d| {
                    if is_vortex {
                        d.rounded_drop_shadow(cr, self.theme.radius_popup(), 10, 3, 0.34 * t);
                        paint_shell_glass_region(
                            d,
                            cr,
                            &self.theme,
                            &mut self.comp.blur_scratch,
                            85,
                        );
                    }
                    composite_shell_layer(
                        d,
                        &self.ctx_canvas,
                        cr.x,
                        cr.y,
                        (t * 255.0).round().clamp(0.0, 255.0) as u8,
                        self.ctx_canvas.is_argb(),
                    );
                });
            }
        }

        if self.comp.toast_anim > 0.01 {
            let t = smootherstep(self.comp.toast_anim);
            let slide_x = ((1.0 - t) * 28.0).round() as i32;
            let tr = Rect::new(
                i32::from(self.x.width) - TOAST_WIDTH - 12 + slide_x,
                self.toast_y(),
                TOAST_WIDTH,
                TOAST_HEIGHT,
            );
            if !tr.expand(16).intersect(dirty_area).is_empty() {
                dst.with_clip(dirty_area, |d| {
                    if is_vortex {
                        d.rounded_drop_shadow(tr, self.theme.radius_popup(), 10, 3, 0.34 * t);
                        paint_shell_glass_region(
                            d,
                            tr,
                            &self.theme,
                            &mut self.comp.blur_scratch,
                            95,
                        );
                    }
                    composite_shell_layer(
                        d,
                        &self.toast_canvas,
                        tr.x,
                        tr.y,
                        (t * 255.0).round().clamp(0.0, 255.0) as u8,
                        self.toast_canvas.is_argb(),
                    );
                });
            }
        }
    }

}

fn build_switcher_entries<'a>(
    windows: &'a [Managed],
    comps: &'a HashMap<Window, CompWindow>,
) -> Vec<SwitcherEntry<'a>> {
    windows
        .iter()
        .map(|m| {
            let cw = comps.get(&m.window).filter(|c| c.has_pixels);
            SwitcherEntry {
                title: &m.title,
                icon: m.icon,
                pixels: cw.map(|c| c.pixels.as_slice()),
                pw: cw.map_or(0, |c| c.pw),
                ph: cw.map_or(0, |c| c.ph),
                minimized: m.minimized,
            }
        })
        .collect()
}

/// Front-to-back single-pass occlusion subtraction preserving blur halos under glass strips.
fn plan_window_occlusion(
    dirty_area: Rect,
    live_blur: bool,
    drag_active: bool,
    plans: &mut [WinCompPlan],
    need: &mut RectRegion,
    strip_scratch: &mut Vec<Rect>,
) {
    need.reset_to_slice(&[dirty_area]);
    for i in (0..plans.len()).rev() {
        plans[i].win_vis.clear();
        plans[i].glass_vis.clear();
        if need.is_empty() || !need.intersects_rect(plans[i].cull_bounds) {
            continue;
        }
        need.intersect_rect_into(plans[i].cull_bounds, &mut plans[i].win_vis);
        if plans[i].has_vortex_glass {
            for s_idx in 0..plans[i].glass_count {
                let strip = plans[i].glass_strips[s_idx];
                need.intersect_rect_into(strip, strip_scratch);
                for &gr in strip_scratch.iter() {
                    let ex = gr.expand(AERO_BLUR_RADIUS).intersect(dirty_area);
                    // During active drags, keep live blur on the main titlebar strip (s_idx == 0)
                    // and reuse static desktop blur on thin side/bottom frame borders until drop.
                    let has_lower = live_blur
                        && !(drag_active && s_idx > 0)
                        && (0..i).any(|j| !plans[j].cull_bounds.intersect(ex).is_empty());
                    plans[i].glass_vis.push((gr, has_lower));
                }
            }
        }
        let opaque = plans[i].opaque_rect;
        if !opaque.is_empty() {
            need.subtract_rect(opaque);
        }
        for &(gr, has_lower) in &plans[i].glass_vis {
            if has_lower {
                let ex = gr.expand(AERO_BLUR_RADIUS).intersect(dirty_area);
                need.add_rect(ex);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn frost_clip_from_dst(
    dst: &mut Canvas,
    ft: Option<&mut Canvas>,
    blur_scratch: &mut BlurScratch,
    desktop_blur_cache: &[u32],
    desktop_stride: usize,
    clip: Rect,
    dirty_area: Rect,
    has_lower_overlap: bool,
    corner_r: i32,
) {
    let clip = clip.intersect(dirty_area).intersect(dst.bounds());
    if clip.is_empty() {
        return;
    }
    if !has_lower_overlap && desktop_blur_cache.len() >= dst.width * dst.height {
        if corner_r <= 1 {
            dst.with_clip(clip, |d| {
                d.copy_rect_from(desktop_blur_cache, desktop_stride, clip);
            });
        } else {
            copy_rounded_from_buf(dst, desktop_blur_cache, desktop_stride, clip, clip, corner_r);
        }
        return;
    }
    let Some(ft) = ft else {
        return;
    };
    let ex = clip
        .expand(AERO_BLUR_RADIUS)
        .intersect(dirty_area)
        .intersect(dst.bounds());
    if ex.is_empty() {
        return;
    }
    ft.with_clip(ex, |f| {
        f.copy_rect_from(dst.pixels(), dst.width, ex);
        f.blur_rect(ex, AERO_BLUR_RADIUS, blur_scratch);
    });
    if corner_r <= 1 {
        dst.with_clip(clip, |d| {
            d.copy_rect_from(ft.pixels(), ft.width, clip);
        });
    } else {
        copy_rounded_from_buf(dst, ft.pixels(), ft.width, clip, clip, corner_r);
    }
}

fn copy_rounded_from_buf(
    dst: &mut Canvas,
    src: &[u32],
    src_stride: usize,
    round_rect: Rect,
    clip: Rect,
    corner_r: i32,
) {
    let clip = clip.intersect(round_rect).intersect(dst.bounds());
    if clip.is_empty() {
        return;
    }
    let rad = corner_r.min(round_rect.w / 2).min(round_rect.h / 2);
    if rad <= 1 {
        dst.with_clip(clip, |d| {
            d.copy_rect_from(src, src_stride, clip);
        });
        return;
    }
    dst.mark_dirty(clip);
    let dst_stride = dst.width;
    let out = dst.pixels_mut();
    let r2 = (rad * rad) as f32;
    for py in clip.y..clip.bottom() {
        let dy = if py < round_rect.y + rad {
            (round_rect.y + rad) - py
        } else if py >= round_rect.bottom() - rad {
            py - (round_rect.bottom() - rad - 1)
        } else {
            0
        };
        let inset = if dy > 0 {
            let d2 = (dy * dy) as f32;
            let dx = (r2 - d2).max(0.0).sqrt().floor() as i32;
            (rad - dx).max(0)
        } else {
            0
        };
        let rx0 = (round_rect.x + inset).max(clip.x);
        let rx1 = (round_rect.right() - inset).min(clip.right());
        if rx1 > rx0 {
            let d_off = py as usize * dst_stride + rx0 as usize;
            let s_off = py as usize * src_stride + rx0 as usize;
            let n = (rx1 - rx0) as usize;
            out[d_off..d_off + n].copy_from_slice(&src[s_off..s_off + n]);
        }
    }
}

fn draw_vortex_popup_shell(
    canvas: &mut Canvas,
    theme: &Theme,
    full: Rect,
    r: i32,
    frosted_inset: bool,
) {
    if canvas.is_argb() {
        canvas.clear_transparent(full, theme.panel);
        let (tint, ta) = theme.glass_tint(true);
        canvas.rounded_rect(full, r, tint, (ta as f32 / 255.0).max(0.48));
        canvas.rounded_border(full, r, 0x0f1a28, 0.72, 1);
        canvas.rounded_border(full.inset(1), (r - 1).max(1), 0xffffff, 0.56, 1);
        if frosted_inset && full.w > 12 && full.h > 12 {
            let inner_bg = if theme.is_dark() { 0x182434 } else { 0xf9fcff };
            canvas.rounded_rect(full.inset(5), 4, inner_bg, 0.90);
        }
    } else {
        canvas.clear(theme.panel);
        ui::popup_surface(canvas, theme, full, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_window_icon_matches_all_apps() {
        assert_eq!(infer_window_icon("终端"), Icon::Terminal);
        assert_eq!(infer_window_icon("文件管理器"), Icon::Files);
        assert_eq!(infer_window_icon("文本编辑器"), Icon::Editor);
        assert_eq!(infer_window_icon("系统监视器"), Icon::Monitor);
        assert_eq!(infer_window_icon("计算器"), Icon::Calculator);
        assert_eq!(infer_window_icon("扫雷"), Icon::Minesweeper);
        assert_eq!(infer_window_icon("看图工具"), Icon::ImageViewer);
        assert_eq!(infer_window_icon("音乐播放器"), Icon::Music);
        assert_eq!(infer_window_icon("系统设置"), Icon::Settings);
        assert_eq!(infer_window_icon("Space 浏览器"), Icon::Browser);
        assert_eq!(infer_window_icon("帮助手册"), Icon::Help);
        assert_eq!(infer_window_icon("系统通知"), Icon::Notify);
        assert_eq!(infer_window_icon("会话账户"), Icon::Login);
        assert_eq!(infer_window_icon("Unknown App"), Icon::SpaceLogo);
    }

    #[test]
    fn icon_prop_name_roundtrips() {
        for app in APPS {
            let prop = app.icon.prop_name();
            assert_eq!(Icon::from_prop_name(prop), Some(app.icon));
        }
        assert_eq!(
            Icon::from_prop_name(Icon::SpaceLogo.prop_name()),
            Some(Icon::SpaceLogo)
        );
    }

    #[test]
    fn pinned_modes_resolve_cleanly() {
        assert!(PINNED_MODES.len() <= 4);
        for slot in 0..PINNED_MODES.len() {
            let entry = pinned_app_entry(slot).expect("pinned mode must exist in APPS");
            assert_eq!(entry.mode, PINNED_MODES[slot]);
        }
    }

    #[test]
    fn taskbar_geometry_has_no_overlaps() {
        for screen_w in [800, 1024, 1280, 1920] {
            for panel_h in [34, 38, 46, 56] {
                for tray_w in [108, 136] {
                    for show_pinned in [true, false] {
                        let layout = TaskbarLayout::compute(screen_w, panel_h, tray_w, show_pinned);
                        assert!(layout.start.right() < layout.start_sep_x);
                        if show_pinned {
                            for slot in 0..PINNED_MODES.len() {
                                let r = layout.pinned_rect(panel_h, slot);
                                assert!(r.x > layout.start_sep_x);
                                assert!(r.right() < layout.pinned_sep_x);
                                if slot + 1 < PINNED_MODES.len() {
                                    let next = layout.pinned_rect(panel_h, slot + 1);
                                    assert!(r.right() < next.x);
                                }
                            }
                            assert!(layout.win_start_x > layout.pinned_sep_x);
                        } else {
                            assert!(layout.win_start_x > layout.start_sep_x);
                        }

                        assert!(layout.ime.right() < layout.net.x);
                        assert!(layout.net.right() < layout.volume.x);
                        assert!(layout.volume.right() < layout.bell.x);
                        assert!(layout.bell.right() < layout.clock_sep_x);

                        for count in [1, 3, 6] {
                            for idx in 0..count {
                                let wr = layout.window_button_rect(panel_h, count, idx);
                                assert!(wr.x >= layout.win_start_x);
                                assert!(wr.right() <= layout.ime.x);
                                if idx + 1 < count {
                                    let next = layout.window_button_rect(panel_h, count, idx + 1);
                                    assert!(wr.right() < next.x);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn volume_popup_controls_fit_without_overlap() {
        let mute_r = vol_popup_mute_rect();
        let slider_r = vol_popup_slider_rect();
        let sfx_r = vol_popup_sfx_rect();
        let cfg_r = vol_popup_settings_rect();
        assert!(mute_r.right() < slider_r.x);
        assert!(slider_r.right() < VOL_POPUP_WIDTH);
        assert!(sfx_r.right() < cfg_r.x);
        assert!(cfg_r.right() < VOL_POPUP_WIDTH);
        assert!(cfg_r.bottom() < VOL_POPUP_HEIGHT);
        assert_eq!(vol_from_slider_x(slider_r, slider_r.x - 10), 0);
        assert_eq!(vol_from_slider_x(slider_r, slider_r.right() + 10), 100);
        assert_eq!(vol_from_slider_x(slider_r, slider_r.x + slider_r.w / 2), 50);
    }

    #[test]
    fn popup_bounds_and_taskbar_passthrough_work() {
        assert!(popup_contains_point(
            VOL_POPUP_WIDTH,
            VOL_POPUP_HEIGHT,
            0,
            0
        ));
        assert!(popup_contains_point(
            VOL_POPUP_WIDTH,
            VOL_POPUP_HEIGHT,
            VOL_POPUP_WIDTH - 1,
            VOL_POPUP_HEIGHT - 1
        ));
        assert!(!popup_contains_point(
            VOL_POPUP_WIDTH,
            VOL_POPUP_HEIGHT,
            -1,
            10
        ));
        assert!(!popup_contains_point(
            VOL_POPUP_WIDTH,
            VOL_POPUP_HEIGHT,
            10,
            VOL_POPUP_HEIGHT
        ));
        assert_eq!(
            taskbar_local_point(1280, 754, 46, 1200, 770),
            Some((1200, 16))
        );
        assert_eq!(taskbar_local_point(1280, 754, 46, 1200, 750), None);
    }

    #[test]
    fn volume_persist_coalesces_burst_clicks_and_ignores_stale_external_sync() {
        let t0 = Instant::now();
        let initial = SoundSnapshot {
            master_volume: 60,
            muted: false,
            enabled: true,
        };
        let mut st = VolumePersistState::new(initial);
        assert!(!st.is_dirty());
        assert_eq!(st.timeout_ms_until(t0), None);

        // Rapid clicks / wheel steps extend debounce and stay dirty without blocking I/O.
        st.stage(
            SoundSnapshot {
                master_volume: 65,
                muted: false,
                enabled: true,
            },
            t0,
        );
        st.stage(
            SoundSnapshot {
                master_volume: 75,
                muted: true,
                enabled: true,
            },
            t0 + Duration::from_millis(40),
        );
        assert!(st.is_dirty());
        assert!(!st.is_due(t0 + Duration::from_millis(280)));
        assert!(st.is_due(t0 + Duration::from_millis(290)));

        // Stale external broadcast while dirty must not erase our persisted baseline.
        st.sync_from_external(SoundSnapshot {
            master_volume: 65,
            muted: false,
            enabled: true,
        });

        let changes = st
            .take_pending_changes(SoundSnapshot {
                master_volume: 75,
                muted: true,
                enabled: true,
            })
            .expect("must emit changed props");
        assert_eq!(
            changes,
            vec![
                ("/master-volume", uxconf::Value::Int(75)),
                ("/muted", uxconf::Value::Bool(true)),
            ]
        );
        assert!(!st.is_dirty());

        // Toggling back to persisted value before debounce cancels write cleanly.
        let t1 = t0 + Duration::from_millis(300);
        st.stage(
            SoundSnapshot {
                master_volume: 75,
                muted: false,
                enabled: true,
            },
            t1,
        );
        assert!(st.is_dirty());
        st.stage(
            SoundSnapshot {
                master_volume: 75,
                muted: true,
                enabled: true,
            },
            t1 + Duration::from_millis(20),
        );
        assert!(!st.is_dirty());
        assert_eq!(
            st.take_pending_changes(SoundSnapshot {
                master_volume: 75,
                muted: true,
                enabled: true,
            }),
            None
        );
    }

    #[test]
    fn popup_grab_transition_clears_on_failure() {
        assert_eq!(transition_popup_grab(None, 42, true), Some(42));
        assert_eq!(transition_popup_grab(Some(42), 99, true), Some(99));
        assert_eq!(transition_popup_grab(Some(42), 99, false), None);
    }

    #[test]
    fn vortex_taskbar_and_menu_rects_have_no_overlaps() {
        for screen_w in [800, 1024, 1280, 1920] {
            for panel_h in [40, 44, 52] {
                for show_pinned in [true, false] {
                    let layout =
                        TaskbarLayout::compute_styled(screen_w, panel_h, 108, show_pinned, true);
                    assert!(layout.show_desktop.w >= 12);
                    assert_eq!(layout.show_desktop.right(), screen_w);
                    assert!(layout.clock_sep_x < layout.show_desktop.x);
                    assert!(layout.start.right() < layout.start_sep_x);
                    let last_win = layout.window_button_rect(panel_h, 4, 3);
                    assert!(last_win.right() <= layout.ime.x);
                }
            }
        }

        let search = vortex_menu_search_rect();
        for idx in 0..8 {
            let r = vortex_menu_row_rect(idx);
            assert!(r.bottom() <= search.y);
            assert!(r.right() < 274);
            if idx > 0 {
                assert!(vortex_menu_row_rect(idx - 1).bottom() <= r.y);
            }
        }
        let (logout, reboot, power_off, confirm_cancel, confirm_ok) = vortex_menu_footer_rects();
        for idx in 0..VORTEX_PLACES.len() {
            let p = vortex_menu_place_rect(idx);
            assert!(p.x >= 274);
            assert!(p.right() < VORTEX_MENU_WIDTH);
            assert!(p.bottom() < power_off.y);
        }
        assert!(power_off.right() < reboot.x);
        assert!(reboot.right() < logout.x);
        assert!(logout.right() <= VORTEX_MENU_WIDTH);
        assert!(confirm_cancel.right() < confirm_ok.x);
        assert!(confirm_ok.right() <= VORTEX_MENU_WIDTH);
    }

    #[test]
    fn occlusion_plan_skips_buried_body_and_preserves_glass_blur_halo() {
        let dirty = Rect::new(0, 0, 1280, 800);
        // Bottom window at (100, 100, 600, 400), top window overlapping at (200, 180, 600, 400).
        let w0_rect = Rect::new(100, 100, 600, 400);
        let w1_rect = Rect::new(200, 180, 600, 400);
        let glass_top = 30;

        let mut p0 = WinCompPlan {
            window: 1,
            draw_rect: w0_rect,
            cull_bounds: window_damage_bounds(w0_rect, true, false),
            opaque_rect: Rect::new(100, 130, 600, 370),
            has_vortex_glass: true,
            glass_top,
            glass_count: 1,
            glass_strips: [
                Rect::new(96, 101, 608, 29),
                Rect::default(),
                Rect::default(),
                Rect::default(),
            ],
            ..WinCompPlan::default()
        };
        let mut p1 = WinCompPlan {
            window: 2,
            draw_rect: w1_rect,
            cull_bounds: window_damage_bounds(w1_rect, true, false),
            opaque_rect: Rect::new(200, 210, 600, 370),
            has_vortex_glass: true,
            glass_top,
            glass_count: 1,
            glass_strips: [
                Rect::new(196, 181, 608, 29),
                Rect::default(),
                Rect::default(),
                Rect::default(),
            ],
            ..WinCompPlan::default()
        };

        let mut plans = [p0, p1];
        let mut desktop_need = RectRegion::new();
        let mut scratch = Vec::new();
        plan_window_occlusion(dirty, true, false, &mut plans, &mut desktop_need, &mut scratch);

        // Top window glass overlaps bottom window -> needs live dynamic blur (`has_lower = true`).
        assert_eq!(plans[1].glass_vis.len(), 1);
        assert!(plans[1].glass_vis[0].1);

        // Bottom window has NO lower window -> fast desktop_blur_cache (`has_lower = false`).
        assert_eq!(plans[0].glass_vis.len(), 1);
        assert!(!plans[0].glass_vis[0].1);

        // Bottom window's deeply buried interior (e.g. (350, 350)) is NOT in win_vis.
        let buried = Rect::new(350, 350, 100, 100);
        assert!(plans[0]
            .win_vis
            .iter()
            .all(|r| r.intersect(buried).is_empty()));

        // Wait: the 8px halo band right under w1's glass strip (y=210..218) IS kept in w0.win_vis
        // so w1's blur kernel gets valid lower-window pixels without overdrawing (350, 350)!
        let halo_probe = Rect::new(300, 212, 20, 4);
        assert!(plans[0]
            .win_vis
            .iter()
            .any(|r| !r.intersect(halo_probe).is_empty()));

        // Buried desktop wallpaper pixels behind w0/w1 are skipped cleanly.
        assert!(desktop_need
            .rects
            .iter()
            .all(|r| r.intersect(buried).is_empty()));

        // When top window maximizes to full screen work area, bottom window is completely skipped!
        p0 = WinCompPlan {
            opaque_rect: Rect::new(100, 130, 600, 370),
            cull_bounds: w0_rect,
            ..WinCompPlan::default()
        };
        p1 = WinCompPlan {
            opaque_rect: Rect::new(0, 0, 1280, 756),
            cull_bounds: Rect::new(0, 0, 1280, 756),
            ..WinCompPlan::default()
        };
        let mut max_plans = [p0, p1];
        plan_window_occlusion(
            Rect::new(0, 0, 1280, 756),
            true,
            false,
            &mut max_plans,
            &mut desktop_need,
            &mut scratch,
        );
        assert!(max_plans[0].win_vis.is_empty());
        assert!(desktop_need.is_empty());
    }

    #[test]
    fn copy_rounded_from_buf_clips_corners_cleanly() {
        let mut dst = Canvas::new(40, 40, 0x111111);
        let src = vec![0xabcdefu32; 40 * 40];
        let r = Rect::new(5, 5, 30, 30);
        copy_rounded_from_buf(&mut dst, &src, 40, r, r, 8);
        // Very top-left corner pixel of bounding rect stays untouched background; center copied.
        assert_eq!(dst.pixels()[5 * 40 + 5], 0x111111);
        assert_eq!(dst.pixels()[20 * 40 + 20], 0xabcdef);
    }
}
