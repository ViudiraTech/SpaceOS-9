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

use crate::ddm::wire::PowerAction;
use crate::draw::{Canvas, Rect, Sprite};
use crate::icons::{self, Icon};
use crate::start_screen::{ShellConfigHost, StartAction, StartScreenState};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, RADIUS_CONTROL, RADIUS_POPUP};
use crate::ui;
use crate::x11::{CursorKind, Key, Result, X};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uxconf::client::{UxConfClient, UxConfWatch};
use uxconf::{BackdropStyle, DesktopConfig, FocusMode, PanelPosition, WindowPlacement};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigWindow, ConfigureNotifyEvent, ConfigureWindowAux,
    ConnectionExt, EventMask, ModMask, PropMode, Window,
};
use x11rb::protocol::Event;

extern "C" {
    fn tzset();
}

const MENU_WIDTH: i32 = 380;
const MENU_HEIGHT: i32 = 530;
const CTX_WIDTH: i32 = 192;
const CTX_HEIGHT: i32 = 148;
const TOAST_WIDTH: i32 = 324;
const TOAST_HEIGHT: i32 = 74;
const VOL_POPUP_WIDTH: i32 = 286;
const VOL_POPUP_HEIGHT: i32 = 138;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCategory {
    System,
    Productivity,
    Media,
}

pub struct AppEntry {
    pub name: &'static str,
    pub subtitle: &'static str,
    pub keywords: &'static str,
    pub icon: Icon,
    pub mode: &'static str,
    pub category: AppCategory,
}

pub const APPS: [AppEntry; 9] = [
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
    Network,
    Volume,
    Bell,
    VolMute,
    VolSlider,
    VolSfx,
    VolSettings,
    MenuRow(usize),
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

pub struct Shell {
    x: X,
    text: TextRenderer,
    config: DesktopConfig,
    ux_client: Option<UxConfClient>,
    watch: Option<UxConfWatch>,
    theme: Theme,
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
    menu: Window,
    menu_canvas: Canvas,
    menu_open: bool,
    menu_query: String,
    menu_confirm_power: Option<PowerAction>,
    start_screen: StartScreenState,
    ctx_menu: Window,
    ctx_canvas: Canvas,
    ctx_open: bool,
    vol_popup: Window,
    vol_canvas: Canvas,
    vol_open: bool,
    vol_dragging: bool,
    toast_window: Window,
    toast_canvas: Canvas,
    toast: Option<Toast>,
    windows: Vec<Managed>,
    active: Option<Window>,
    hover: Hit,
    clock_tick: (i32, i32, i32),
    net_fd: Option<i32>,
    net_status: crate::netmgr::NetStatus,
    last_net_refresh: Instant,
    last_launch: Option<(&'static str, Instant)>,
    running: bool,
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

fn menu_row_rect(index: usize) -> Rect {
    Rect::new(12, 58 + index as i32 * 52, MENU_WIDTH - 24, 48)
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
    } else if title.contains("看图") || lower.contains("view") || lower.contains("image") {
        Icon::ImageViewer
    } else if title.contains("音乐")
        || title.contains("播放器")
        || lower.contains("music")
        || lower.contains("audio")
        || lower.contains("mp3")
    {
        Icon::Music
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

struct TaskbarLayout {
    start: Rect,
    start_sep_x: i32,
    pinned_sep_x: i32,
    win_start_x: i32,
    net: Rect,
    volume: Rect,
    bell: Rect,
    clock_sep_x: i32,
}

impl TaskbarLayout {
    fn compute(screen_w: i32, panel_h: i32, tray_w: i32, show_pinned: bool) -> Self {
        let start_bh = (panel_h - 10).clamp(28, 42);
        let start = Rect::new(6, (panel_h - start_bh) / 2, 86, start_bh);
        let start_sep_x = start.right() + 6;
        let pinned_start_x = start_sep_x + 6;
        let pinned_sep_x = pinned_start_x + (PINNED_MODES.len() as i32) * 36 + 2;
        let win_start_x = if show_pinned {
            pinned_sep_x + 8
        } else {
            start_sep_x + 8
        };
        let tray_bh = (panel_h - 12).clamp(26, 38);
        let tray_y = (panel_h - tray_bh) / 2;
        let clock_sep_x = screen_w - tray_w;
        let bell = Rect::new(clock_sep_x - 40, tray_y, 32, tray_bh);
        let volume = Rect::new(bell.x - 36, tray_y, 32, tray_bh);
        let net = Rect::new(volume.x - 36, tray_y, 32, tray_bh);
        Self {
            start,
            start_sep_x,
            pinned_sep_x,
            win_start_x,
            net,
            volume,
            bell,
            clock_sep_x,
        }
    }

    fn pinned_rect(&self, panel_h: i32, slot: usize) -> Rect {
        let bh = (panel_h - 12).clamp(26, 38);
        Rect::new(
            self.start_sep_x + 6 + slot as i32 * 36,
            (panel_h - bh) / 2,
            32,
            bh,
        )
    }

    fn window_button_rect(&self, panel_h: i32, count: usize, index: usize) -> Rect {
        let bh = (panel_h - 12).clamp(26, 40);
        let available = (self.net.x - 10 - self.win_start_x).max(80);
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
                | EventMask::POINTER_MOTION
                | EventMask::KEY_PRESS,
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

        let popup_menu_y = if config.panel_position == PanelPosition::Top {
            panel_h + 6
        } else {
            (taskbar_y - MENU_HEIGHT - 6).max(8)
        };
        let (menu_x, menu_y, menu_w, menu_h) = if config.panel_start_fullscreen {
            (0i16, 0i16, x.width, x.height)
        } else {
            (
                6i16,
                popup_menu_y as i16,
                MENU_WIDTH as u16,
                MENU_HEIGHT as u16,
            )
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
                | EventMask::KEY_PRESS,
            theme.panel,
        )?;
        x.set_wm_type(menu, x.atoms.net_wm_type_dialog)?;
        if !config.panel_start_fullscreen {
            x.set_rounded_shape(menu, MENU_WIDTH as u16, MENU_HEIGHT as u16, RADIUS_POPUP)?;
        }

        // Bind global Start Screen shortcuts (Super_L, Super_R, Ctrl+Escape).
        x.grab_global_key(0xffeb, ModMask::from(0u16));
        x.grab_global_key(0xffec, ModMask::from(0u16));
        x.grab_global_key(0xff1b, ModMask::CONTROL);

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
                | EventMask::KEY_PRESS,
            theme.panel,
        )?;
        x.set_wm_type(ctx_menu, x.atoms.net_wm_type_dialog)?;
        x.set_rounded_shape(ctx_menu, CTX_WIDTH as u16, CTX_HEIGHT as u16, RADIUS_POPUP)?;

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
                | EventMask::KEY_PRESS,
            theme.panel,
        )?;
        x.set_wm_type(vol_popup, x.atoms.net_wm_type_dialog)?;
        x.set_rounded_shape(
            vol_popup,
            VOL_POPUP_WIDTH as u16,
            VOL_POPUP_HEIGHT as u16,
            RADIUS_POPUP,
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
            RADIUS_POPUP,
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

        let mut shell = Self {
            x,
            text,
            config,
            ux_client,
            watch,
            theme,
            wallpaper_raw: None,
            wallpaper_scaled: Vec::new(),
            wallpaper_file: String::new(),
            desktop,
            desktop_canvas,
            desktop_bg: 0,
            selected_desktop_icon: None,
            taskbar,
            taskbar_canvas,
            menu,
            menu_canvas,
            menu_open: false,
            menu_query: String::new(),
            menu_confirm_power: None,
            start_screen: StartScreenState::new(),
            ctx_menu,
            ctx_canvas,
            ctx_open: false,
            vol_popup,
            vol_canvas,
            vol_open: false,
            vol_dragging: false,
            toast_window,
            toast_canvas,
            toast: None,
            windows: Vec::new(),
            active: None,
            hover: Hit::None,
            clock_tick: (-1, -1, -1),
            net_fd,
            net_status,
            last_net_refresh: Instant::now(),
            last_launch: None,
            running: true,
        };
        shell.claim_wm()?;
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
        crate::sound::play(&shell.config, crate::sound::SystemSound::Startup);
        eprintln!(
            "spui: desktop ready at {} ms",
            startup.elapsed().as_millis()
        );
        shell.event_loop()
    }

    fn panel_height(&self) -> i32 {
        self.config.panel_size.clamp(34, 60)
    }

    fn taskbar_y(&self) -> i32 {
        if self.config.panel_position == PanelPosition::Top {
            0
        } else {
            i32::from(self.x.height) - self.panel_height()
        }
    }

    fn menu_y(&self) -> i32 {
        if self.config.panel_position == PanelPosition::Top {
            self.panel_height() + 6
        } else {
            (self.taskbar_y() - MENU_HEIGHT - 6).max(8)
        }
    }

    fn sync_menu_window(&mut self) -> Result<()> {
        let (mx, my, mw, mh, rounded) = if self.config.panel_start_fullscreen {
            (0i16, 0i16, self.x.width, self.x.height, false)
        } else {
            (
                6i16,
                self.menu_y() as i16,
                MENU_WIDTH as u16,
                MENU_HEIGHT as u16,
                true,
            )
        };
        self.x.move_resize(self.menu, mx, my, mw, mh)?;
        if rounded {
            self.x.set_rounded_shape(self.menu, mw, mh, RADIUS_POPUP)?;
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
        TaskbarLayout::compute(
            i32::from(self.x.width),
            self.panel_height(),
            self.tray_width(),
            self.config.panel_show_pinned,
        )
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

        let panel_geom_changed = prev.panel_position != self.config.panel_position
            || prev.panel_size != self.config.panel_size;
        if panel_geom_changed {
            let ph = self.panel_height();
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
        if panel_geom_changed || prev.panel_start_fullscreen != self.config.panel_start_fullscreen {
            self.sync_menu_window()?;
        }

        let wp_file_changed = prev.wallpaper != self.config.wallpaper;
        let backdrop_changed = wp_file_changed
            || prev.backdrop_style != self.config.backdrop_style
            || prev.solid_color != self.config.solid_color;
        if backdrop_changed {
            self.reload_wallpaper(wp_file_changed);
        }
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
        if self.toast.is_some() {
            self.draw_toast();
            self.present_toast()?;
        }
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

    fn poll_uxconf(&mut self) -> Result<()> {
        let prev = self.config.clone();
        let changed = self
            .watch
            .as_mut()
            .is_some_and(|w| w.poll_into(&mut self.config).0);
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
        while self.running {
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
            if raw.is_empty() {
                let ux_fd = self.watch.as_ref().map(UxConfWatch::as_raw_fd);
                let anim_active = ui::has_active_animations();
                let timeout = if anim_active {
                    16
                } else if self.config.clock_seconds {
                    200
                } else {
                    500
                };
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

            // Coalesce burst MotionNotify events on the same shell window.
            let mut events: Vec<Event> = Vec::with_capacity(raw.len());
            for ev in raw {
                if let (Some(Event::MotionNotify(prev)), Event::MotionNotify(curr)) =
                    (events.last(), &ev)
                {
                    if prev.event == curr.event {
                        *events.last_mut().unwrap() = ev;
                        continue;
                    }
                }
                events.push(ev);
            }

            for event in events {
                self.handle(event)?;
            }

            let start_net = if net_event {
                self.start_screen.on_net_event()
            } else {
                false
            };
            let anim_active = ui::has_active_animations();
            if self.update_clock() || anim_active || net_dirty {
                self.draw_taskbar();
                self.present_taskbar()?;
            }
            if self.vol_open && anim_active {
                self.draw_vol_popup();
                self.present_vol_popup()?;
            }
            if self.menu_open && self.config.panel_start_fullscreen {
                let start_tick = self.start_screen.tick(&self.config);
                if start_tick || start_net || anim_active || net_dirty {
                    self.draw_menu();
                    self.present_menu()?;
                }
            }
            if let Some(toast) = &self.toast {
                if Instant::now() >= toast.deadline {
                    self.hide_toast()?;
                }
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
        self.x.raise(self.taskbar)?;
        if self.menu_open {
            self.x.raise(self.menu)?;
        }
        Ok(())
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::ConfigureNotify(e) if e.window == self.x.root => {
                self.handle_screen_resize(e.width, e.height)?;
            }
            Event::MapRequest(e) => self.manage(e.window)?,
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
                if let Some(managed) = self.windows.iter_mut().find(|w| w.window == e.window) {
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
                    if resized {
                        let notify = ConfigureNotifyEvent {
                            response_type: 22,
                            sequence: 0,
                            event: e.window,
                            window: e.window,
                            above_sibling: x11rb::NONE,
                            x: e.x,
                            y: e.y,
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
            }
            Event::EnterNotify(e) => {
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
                    if self.config.wm_auto_raise {
                        self.x.raise(e.event)?;
                        self.x.raise(self.taskbar)?;
                        if self.toast.is_some() {
                            let _ = self.x.raise(self.toast_window);
                        }
                    }
                    self.x.focus(e.event)?;
                    self.active = Some(e.event);
                    self.publish_active()?;
                    self.draw_taskbar();
                    self.present_taskbar()?;
                }
            }
            Event::UnmapNotify(e) => {
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
            Event::DestroyNotify(e) => {
                if self.remove_window(e.window) {
                    self.draw_taskbar();
                    self.present_taskbar()?;
                }
            }
            Event::PropertyNotify(e) => {
                if e.window == self.x.root && e.atom == self.x.atoms.net_active_window {
                    let active = self.x.get_active_window();
                    if active != self.active {
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
                            self.x.raise(self.taskbar)?;
                            if self.toast.is_some() {
                                let _ = self.x.raise(self.toast_window);
                            }
                            self.x.focus(window)?;
                            self.x.flush()?;
                        }
                        self.draw_taskbar();
                        self.present_taskbar()?;
                    }
                } else if self.windows.iter().any(|w| w.window == e.window) {
                    let title = self.window_title(e.window);
                    let icon = self.window_icon(e.window, &title);
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
                if e.event == self.menu
                    && self.menu_open
                    && self.config.panel_start_fullscreen
                    && self.start_screen.on_leave()
                {
                    self.draw_menu();
                    self.present_menu()?;
                }
                let prev = self.hover;
                if (e.event == self.taskbar
                    && matches!(
                        prev,
                        Hit::Start
                            | Hit::Pinned(_)
                            | Hit::Window(_)
                            | Hit::Network
                            | Hit::Volume
                            | Hit::Bell
                    ))
                    || (e.event == self.menu
                        && matches!(
                            prev,
                            Hit::MenuRow(_)
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
                if e.event == self.taskbar {
                    if (e.detail == 4 || e.detail == 5) && self.vol_rect().contains(x, y) {
                        let step = if e.detail == 4 { 5 } else { -5 };
                        self.adjust_volume_step(step)?;
                    } else if (e.detail == 2 || e.detail == 3) && self.vol_rect().contains(x, y) {
                        self.toggle_mute()?;
                    } else if e.detail == 1 {
                        self.close_ctx()?;
                        self.taskbar_click(x, y)?;
                    }
                } else if e.event == self.vol_popup && self.vol_open {
                    if e.detail == 4 || e.detail == 5 {
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
                    } else if e.detail == 1 {
                        self.menu_click(x, y)?;
                    }
                } else if e.event == self.ctx_menu && e.detail == 1 {
                    self.ctx_click(x, y)?;
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
                if e.detail == 1 && self.vol_dragging {
                    self.vol_dragging = false;
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
                    let hit = self.taskbar_hit(x, y);
                    if hit != self.hover {
                        self.hover = hit;
                        self.draw_taskbar();
                        self.present_taskbar()?;
                    }
                } else if e.event == self.vol_popup && self.vol_open {
                    if self.vol_dragging {
                        let vol = vol_from_slider_x(vol_popup_slider_rect(), x);
                        self.set_master_volume(vol, false)?;
                    } else {
                        let hit = self.vol_popup_hit(x, y);
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
                let key = Key {
                    code: e.detail,
                    state: e.state.into(),
                    sym: self.x.key_symbol(e.detail, e.state.into()),
                };
                if matches!(key.sym, 0xffeb | 0xffec) || (key.control() && key.is_escape()) {
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
            Event::ClientMessage(e) => {
                if e.type_ == self.x.atoms.net_active_window {
                    let window = e.data.as_data32()[0];
                    self.activate(window)?;
                } else if e.type_ == self.x.atoms.spui_window_cmd {
                    let cmd = e.data.as_data32()[0];
                    if cmd == 1 {
                        self.minimize_window(e.window)?;
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
        self.x.map(window)?;
        self.x.raise(window)?;
        self.x.raise(self.taskbar)?;
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
        self.publish_client_list()?;
        self.publish_active()?;
        self.draw_taskbar();
        self.present_taskbar()?;
        Ok(())
    }

    fn minimize_window(&mut self, window: Window) -> Result<()> {
        let Some(managed) = self.windows.iter_mut().find(|w| w.window == window) else {
            return Ok(());
        };
        if managed.minimized {
            return Ok(());
        };
        managed.minimized = true;
        self.x.unmap(window)?;
        if self.active == Some(window) {
            self.active = self
                .windows
                .iter()
                .rev()
                .find(|w| !w.minimized)
                .map(|w| w.window);
            if let Some(next) = self.active {
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
        let before = self.windows.len();
        self.windows.retain(|w| w.window != window);
        if self.windows.len() == before {
            return false;
        }
        if self.active == Some(window) {
            self.active = self
                .windows
                .iter()
                .rev()
                .find(|w| !w.minimized)
                .map(|w| w.window);
            if let Some(next) = self.active {
                let _ = self.x.focus(next);
            }
            let _ = self.publish_active();
        }
        let _ = self.publish_client_list();
        true
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

    fn activate(&mut self, window: Window) -> Result<()> {
        let Some(managed) = self.windows.iter_mut().find(|w| w.window == window) else {
            return Ok(());
        };
        if managed.minimized {
            managed.minimized = false;
            self.x.map(window)?;
        }
        self.x.raise(window)?;
        self.x.raise(self.taskbar)?;
        if self.toast.is_some() {
            let _ = self.x.raise(self.toast_window);
        }
        self.x.focus(window)?;
        self.active = Some(window);
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
        if self.net_rect().contains(x, y) {
            return Hit::Network;
        }
        if self.vol_rect().contains(x, y) {
            return Hit::Volume;
        }
        if self.bell_rect().contains(x, y) {
            return Hit::Bell;
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
                    if !is_min && self.active == Some(window) {
                        self.minimize_window(window)?;
                    } else {
                        self.activate(window)?;
                    }
                }
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

    fn set_sound_prop(&mut self, prop: &str, val: uxconf::Value) {
        let _ = self.config.apply_property("sound", prop, &val);
        if self.ux_client.is_none() {
            self.ux_client = UxConfClient::connect().ok();
        }
        if let Some(client) = self.ux_client.as_mut() {
            if client.set("sound", prop, val.clone()).is_err() {
                self.ux_client = UxConfClient::connect().ok();
                if let Some(c) = self.ux_client.as_mut() {
                    let _ = c.set("sound", prop, val);
                }
            }
        }
    }

    fn set_master_volume(&mut self, vol: i32, play_sfx: bool) -> Result<()> {
        let clamped = vol.clamp(0, 100);
        let prev_vol = self.config.sound_master_volume;
        let prev_muted = self.config.sound_muted;
        if clamped != prev_vol {
            self.set_sound_prop("/master-volume", uxconf::Value::Int(i64::from(clamped)));
        }
        if self.config.sound_muted && clamped > 0 {
            self.set_sound_prop("/muted", uxconf::Value::Bool(false));
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
        self.set_sound_prop("/muted", uxconf::Value::Bool(next));
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
        self.x.focus(self.vol_popup)?;
        self.present_vol_popup()?;
        self.present_taskbar()?;
        Ok(())
    }

    fn close_vol_popup(&mut self) -> Result<()> {
        if self.vol_open {
            self.vol_open = false;
            self.vol_dragging = false;
            if matches!(
                self.hover,
                Hit::VolMute | Hit::VolSlider | Hit::VolSfx | Hit::VolSettings
            ) {
                self.hover = Hit::None;
            }
            self.x.unmap(self.vol_popup)?;
            if let Some(active) = self.active {
                let _ = self.x.focus(active);
            }
            self.draw_taskbar();
            self.present_taskbar()?;
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
                self.set_sound_prop("/system-sounds-enabled", uxconf::Value::Bool(next));
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
        let (logout, reboot, power_off, confirm_cancel, confirm_ok) = compact_footer_rects();
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
        for (slot, &app_idx) in filtered.iter().enumerate() {
            if menu_row_rect(slot).contains(x, y) {
                return Hit::MenuRow(app_idx);
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
        self.ctx_open = true;
        self.hover = Hit::None;
        self.x
            .move_resize(self.ctx_menu, cx, cy, CTX_WIDTH as u16, CTX_HEIGHT as u16)?;
        self.draw_ctx();
        self.x.map(self.ctx_menu)?;
        self.x.raise(self.ctx_menu)?;
        self.present_ctx()?;
        Ok(())
    }

    fn close_ctx(&mut self) -> Result<()> {
        if self.ctx_open {
            self.ctx_open = false;
            self.x.unmap(self.ctx_menu)?;
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
        self.close_ctx()?;
        if hit != Hit::None {
            crate::sound::play(&self.config, crate::sound::SystemSound::Click);
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
            self.hover = Hit::None;
            self.sync_menu_window()?;
            if self.config.panel_start_fullscreen {
                self.start_screen.on_open();
            }
            self.draw_menu();
            self.draw_taskbar();
            self.x.map(self.menu)?;
            self.x.raise(self.menu)?;
            self.x.focus(self.menu)?;
            self.present_menu()?;
            self.present_taskbar()?;
            Ok(())
        }
    }

    fn close_menu(&mut self) -> Result<()> {
        if self.menu_open {
            self.menu_open = false;
            self.menu_query.clear();
            self.menu_confirm_power = None;
            self.x.unmap(self.menu)?;
            if let Some(active) = self.active {
                let _ = self.x.focus(active);
            }
            self.draw_taskbar();
            self.present_taskbar()?;
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
        let Ok(path) = std::env::current_exe() else {
            return;
        };
        let mut cmd = Command::new(path);
        cmd.arg(mode);
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
    }

    fn hide_toast(&mut self) -> Result<()> {
        self.toast = None;
        self.x.unmap(self.toast_window)?;
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

    fn draw_taskbar(&mut self) {
        let w = self.taskbar_canvas.width as i32;
        let h = self.panel_height();
        let show_pinned = self.config.panel_show_pinned;
        let layout = TaskbarLayout::compute(w, h, self.tray_width(), show_pinned);
        let start = layout.start;
        let pinned_rects: [Rect; PINNED_MODES.len()] =
            std::array::from_fn(|i| layout.pinned_rect(h, i));
        let win_count = self.windows.len().min(6);
        let win_rects: Vec<Rect> = (0..win_count)
            .map(|i| layout.window_button_rect(h, win_count, i))
            .collect();
        let net = layout.net;
        let vol = layout.volume;
        let bell = layout.bell;
        let border_y = if self.config.panel_position == PanelPosition::Top {
            h - 1
        } else {
            0
        };

        let canvas = &mut self.taskbar_canvas;
        canvas.fill(Rect::new(0, 0, w, h), self.theme.panel);
        canvas.fill(Rect::new(0, border_y, w, 1), self.theme.panel_border);

        // Start button using official SpaceOS9.png logo.
        let start_active = self.menu_open || self.hover == Hit::Start;
        let start_t = ui::animate_bool(start, start_active, ui::MOTION_HOVER);
        if start_t > 0.001 {
            canvas.rounded_rect(
                start,
                RADIUS_CONTROL,
                if self.menu_open {
                    self.theme.accent_soft
                } else {
                    self.theme.panel_hover
                },
                start_t,
            );
            canvas.rounded_border(
                start,
                RADIUS_CONTROL,
                if self.menu_open {
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
            start.x + 9,
            start.y + (start.h - 22) / 2,
            22,
            None,
        );
        self.text.draw(
            canvas,
            "开始",
            (start.x + 38) as f32,
            start.y as f32 + start.h as f32 * 0.5 + 4.8,
            13.0,
            self.theme.text,
            Face::Bold,
        );

        let sep_h = (h - 20).clamp(16, 26);
        let sep_y = (h - sep_h) / 2;
        canvas.fill(
            Rect::new(layout.start_sep_x, sep_y, 1, sep_h),
            self.theme.panel_border,
        );

        // Quick-launch curated core apps (optional).
        if show_pinned {
            for (i, &rect) in pinned_rects.iter().enumerate() {
                let Some(app) = pinned_app_entry(i) else {
                    continue;
                };
                let hov_t = ui::animate_bool(rect, self.hover == Hit::Pinned(i), ui::MOTION_HOVER);
                if hov_t > 0.001 {
                    canvas.rounded_rect(rect, RADIUS_CONTROL, self.theme.panel_hover, hov_t);
                }
                icons::draw(
                    canvas,
                    app.icon,
                    rect.x + (rect.w - 20) / 2,
                    rect.y + (rect.h - 20) / 2,
                    20,
                    None,
                );
            }
            canvas.fill(
                Rect::new(layout.pinned_sep_x, sep_y, 1, sep_h),
                self.theme.panel_border,
            );
        }

        // Running window buttons.
        for (index, &rect) in win_rects.iter().enumerate() {
            let managed = &self.windows[index];
            let is_active = !managed.minimized && self.active == Some(managed.window);
            let is_hover = self.hover == Hit::Window(index);
            let hov_t = ui::animate_bool(rect, is_hover, ui::MOTION_HOVER);
            let bg = if is_active {
                self.theme.panel_high
            } else if hov_t > 0.001 {
                crate::draw::mix(self.theme.panel, self.theme.panel_hover, hov_t)
            } else {
                self.theme.panel
            };
            canvas.rounded_rect(rect, RADIUS_CONTROL, bg, 1.0);
            if is_active || hov_t > 0.001 {
                let border_alpha = if is_active { 1.0 } else { hov_t };
                canvas.rounded_border(
                    rect,
                    RADIUS_CONTROL,
                    self.theme.panel_border,
                    border_alpha,
                    1,
                );
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
            icons::draw(
                canvas,
                managed.icon,
                rect.x + 8,
                rect.y + (rect.h - 18) / 2,
                18,
                None,
            );
            let max_w = (rect.w - 38).max(20) as f32;
            let label = self.text.truncate(&managed.title, max_w, 12.0, Face::Ui);
            let fg = if is_active {
                self.theme.text
            } else if managed.minimized {
                self.theme.text_muted
            } else {
                self.theme.text_dim
            };
            self.text.draw(
                canvas,
                &label,
                (rect.x + 31) as f32,
                rect.y as f32 + rect.h as f32 * 0.5 + 4.5,
                12.0,
                fg,
                Face::Ui,
            );
        }

        // Right tray: wired network status, speaker volume icon, notification bell & clock.
        let net_hov = ui::animate_bool(net, self.hover == Hit::Network, ui::MOTION_HOVER);
        let net_connected = self.net_status.connected();
        if net_hov > 0.001 {
            canvas.rounded_rect(net, RADIUS_CONTROL, self.theme.panel_hover, net_hov);
        }
        let (net_icon, net_tint) = if net_connected {
            (Icon::NetworkWired, self.theme.text)
        } else if self.net_status.has_device && self.net_status.up {
            (Icon::NetworkWired, self.theme.warning)
        } else {
            (Icon::NetworkOffline, self.theme.text_muted)
        };
        icons::draw(
            canvas,
            net_icon,
            net.x + (net.w - 18) / 2,
            net.y + (net.h - 18) / 2,
            18,
            Some(net_tint),
        );

        let vol_active = self.vol_open || self.hover == Hit::Volume;
        let vol_hov = ui::animate_bool(vol, vol_active, ui::MOTION_HOVER);
        if vol_hov > 0.001 {
            canvas.rounded_rect(
                vol,
                RADIUS_CONTROL,
                if self.vol_open {
                    self.theme.accent_soft
                } else {
                    self.theme.panel_hover
                },
                vol_hov,
            );
        }
        let vol_icon = Icon::for_volume(self.config.sound_master_volume, self.config.sound_muted);
        let vol_tint = if self.config.sound_muted || self.config.sound_master_volume <= 0 {
            self.theme.text_muted
        } else if self.vol_open {
            self.theme.accent
        } else {
            self.theme.text
        };
        icons::draw(
            canvas,
            vol_icon,
            vol.x + (vol.w - 18) / 2,
            vol.y + (vol.h - 18) / 2,
            18,
            Some(vol_tint),
        );

        let bell_hov = ui::animate_bool(bell, self.hover == Hit::Bell, ui::MOTION_HOVER);
        if bell_hov > 0.001 {
            canvas.rounded_rect(bell, RADIUS_CONTROL, self.theme.panel_hover, bell_hov);
        }
        icons::draw(
            canvas,
            if self.toast.is_some() {
                Icon::BellActive
            } else {
                Icon::Bell
            },
            bell.x + (bell.w - 18) / 2,
            bell.y + (bell.h - 18) / 2,
            18,
            Some(if self.toast.is_some() {
                self.theme.accent
            } else {
                self.theme.text_dim
            }),
        );
        canvas.fill(
            Rect::new(layout.clock_sep_x, sep_y, 1, sep_h),
            self.theme.panel_border,
        );

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

        let right_x = (w - 14) as f32;
        if self.config.clock_show_date {
            let weeks = ["日", "一", "二", "三", "四", "五", "六"];
            let mid = h as f32 * 0.5;
            self.text.draw_right(
                canvas,
                &time_str,
                right_x,
                mid - 2.0,
                12.5,
                self.theme.text,
                Face::Bold,
            );
            self.text.draw_right(
                canvas,
                &format!(
                    "{year}/{month:02}/{day:02} 周{}",
                    weeks[(weekday.max(0) as usize).min(6)]
                ),
                right_x,
                mid + 13.0,
                10.0,
                self.theme.text_muted,
                Face::Ui,
            );
        } else {
            self.text.draw_right(
                canvas,
                &time_str,
                right_x,
                h as f32 * 0.5 + 4.8,
                13.0,
                self.theme.text,
                Face::Bold,
            );
        }
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
            return;
        }
        let filtered = self.filtered_apps();
        let canvas = &mut self.menu_canvas;
        let (w, h) = (MENU_WIDTH, MENU_HEIGHT);
        canvas.clear(self.theme.panel);
        ui::popup_surface(canvas, &self.theme, Rect::new(0, 0, w, h), RADIUS_POPUP);

        ui::text_input(
            canvas,
            &mut self.text,
            &self.theme,
            Rect::new(14, 14, w - 28, 34),
            &self.menu_query,
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
            for (slot, &app_idx) in filtered.iter().enumerate() {
                let app = &APPS[app_idx];
                let row = menu_row_rect(slot);
                let hovered = self.hover == Hit::MenuRow(app_idx)
                    || (slot == 0 && !self.menu_query.is_empty());
                if hovered {
                    canvas.rounded_rect(row, RADIUS_CONTROL, self.theme.panel_hover, 1.0);
                }
                icons::draw(canvas, app.icon, row.x + 10, row.y + 8, 32, None);
                self.text.draw(
                    canvas,
                    app.name,
                    (row.x + 52) as f32,
                    (row.y + 20) as f32,
                    13.0,
                    self.theme.text,
                    Face::Bold,
                );
                self.text.draw(
                    canvas,
                    app.subtitle,
                    (row.x + 52) as f32,
                    (row.y + 37) as f32,
                    11.0,
                    self.theme.text_muted,
                    Face::Ui,
                );
            }
        }

        // Footer bar.
        let (logout, reboot, power_off, confirm_cancel, confirm_ok) = compact_footer_rects();
        canvas.fill(Rect::new(1, h - 54, w - 2, 53), self.theme.window_alt);
        canvas.fill(Rect::new(1, h - 54, w - 2, 1), self.theme.panel_border);
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
    }

    fn draw_ctx(&mut self) {
        let canvas = &mut self.ctx_canvas;
        canvas.clear(self.theme.panel);
        ui::popup_surface(
            canvas,
            &self.theme,
            Rect::new(0, 0, CTX_WIDTH, CTX_HEIGHT),
            RADIUS_POPUP,
        );

        let items = [
            ("在此打开终端", Icon::Terminal),
            ("打开文件管理器", Icon::Files),
            ("刷新桌面", Icon::Refresh),
            ("桌面与外观设置", Icon::Settings),
        ];
        canvas.fill(
            Rect::new(10, 104, CTX_WIDTH - 20, 1),
            self.theme.panel_border,
        );
        for (idx, (label, ic)) in items.iter().enumerate() {
            let row = ctx_row_rect(idx);
            if self.hover == Hit::ContextRow(idx) {
                canvas.rounded_rect(row, RADIUS_CONTROL, self.theme.panel_hover, 1.0);
            }
            let tint = if ic.is_symbolic() {
                Some(self.theme.text_dim)
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
                self.theme.text,
                Face::Ui,
            );
        }
    }

    fn draw_toast(&mut self) {
        let canvas = &mut self.toast_canvas;
        let (w, h) = (TOAST_WIDTH, TOAST_HEIGHT);
        canvas.clear(self.theme.panel);
        ui::popup_surface(canvas, &self.theme, Rect::new(0, 0, w, h), RADIUS_POPUP);
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
        self.x
            .put_canvas(self.taskbar, &self.taskbar_canvas, area)?;
        self.x.flush()
    }

    fn present_menu(&mut self) -> Result<()> {
        let area = self.menu_canvas.bounds();
        self.menu_canvas.take_dirty();
        self.x.put_canvas(self.menu, &self.menu_canvas, area)?;
        self.x.flush()
    }

    fn present_ctx(&mut self) -> Result<()> {
        let area = self.ctx_canvas.bounds();
        self.ctx_canvas.take_dirty();
        self.x.put_canvas(self.ctx_menu, &self.ctx_canvas, area)?;
        self.x.flush()
    }

    fn present_toast(&mut self) -> Result<()> {
        let area = self.toast_canvas.bounds();
        self.toast_canvas.take_dirty();
        self.x
            .put_canvas(self.toast_window, &self.toast_canvas, area)?;
        self.x.flush()
    }

    fn draw_vol_popup(&mut self) {
        let canvas = &mut self.vol_canvas;
        let w = VOL_POPUP_WIDTH;
        let h = VOL_POPUP_HEIGHT;
        canvas.clear(self.theme.panel);
        ui::popup_surface(canvas, &self.theme, Rect::new(0, 0, w, h), RADIUS_POPUP);

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
        if muted || mute_hov {
            canvas.rounded_rect(
                mute_r,
                RADIUS_CONTROL,
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
            mute_r.x + (mute_r.w - 18) / 2,
            mute_r.y + (mute_r.h - 18) / 2,
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

        canvas.fill(Rect::new(14, 84, w - 28, 1), self.theme.window_border);

        let sfx_r = vol_popup_sfx_rect();
        if self.hover == Hit::VolSfx {
            canvas.rounded_rect(sfx_r, RADIUS_CONTROL, self.theme.panel_hover, 0.7);
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
    }

    fn present_vol_popup(&mut self) -> Result<()> {
        let area = self.vol_canvas.bounds();
        self.vol_canvas.take_dirty();
        self.x.put_canvas(self.vol_popup, &self.vol_canvas, area)?;
        self.x.flush()
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
        assert_eq!(infer_window_icon("看图工具"), Icon::ImageViewer);
        assert_eq!(infer_window_icon("音乐播放器"), Icon::Music);
        assert_eq!(infer_window_icon("系统设置"), Icon::Settings);
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
            for panel_h in [38, 46, 56] {
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

                        assert!(layout.net.right() < layout.volume.x);
                        assert!(layout.volume.right() < layout.bell.x);
                        assert!(layout.bell.right() < layout.clock_sep_x);

                        for count in [1, 3, 6] {
                            for idx in 0..count {
                                let wr = layout.window_button_rect(panel_h, count, idx);
                                assert!(wr.x >= layout.win_start_x);
                                assert!(wr.right() <= layout.net.x);
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
}
