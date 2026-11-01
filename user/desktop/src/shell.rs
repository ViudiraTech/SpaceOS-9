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

use crate::draw::{Canvas, Rect, Sprite};
use crate::icons::{self, Icon};
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
    ConnectionExt, EventMask, PropMode, Window,
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

struct AppEntry {
    name: &'static str,
    subtitle: &'static str,
    keywords: &'static str,
    icon: Icon,
    mode: &'static str,
}

const APPS: [AppEntry; 8] = [
    AppEntry {
        name: "文件管理器",
        subtitle: "浏览目录与系统文件",
        keywords: "files folder directory dolphin 文件 目录",
        icon: Icon::Files,
        mode: "files",
    },
    AppEntry {
        name: "终端",
        subtitle: "交互式命令行会话",
        keywords: "term terminal shell sh bash console 终端 命令行",
        icon: Icon::Terminal,
        mode: "term",
    },
    AppEntry {
        name: "系统监视器",
        subtitle: "CPU、内存、进程性能监控与管理",
        keywords: "monitor taskmgr top htop task manager 监视器 性能 进程 任务管理器",
        icon: Icon::Monitor,
        mode: "monitor",
    },
    AppEntry {
        name: "文本编辑器",
        subtitle: "查看与编辑文本文档",
        keywords: "edit editor text notepad kwrite 文本 编辑器",
        icon: Icon::Editor,
        mode: "edit",
    },
    AppEntry {
        name: "计算器",
        subtitle: "标准四则运算与即时数值计算",
        keywords: "calc calculator math 计算器 计算 数学",
        icon: Icon::Calculator,
        mode: "calc",
    },
    AppEntry {
        name: "看图工具",
        subtitle: "图像浏览、缩放与壁纸设定",
        keywords: "view image viewer photo picture 看图 图片 图像 相册",
        icon: Icon::ImageViewer,
        mode: "view",
    },
    AppEntry {
        name: "系统设置",
        subtitle: "外观、桌面、输入设备与系统偏好",
        keywords: "settings display wallpaper theme 外观 壁纸 分辨率 设置",
        icon: Icon::Settings,
        mode: "settings",
    },
    AppEntry {
        name: "帮助手册",
        subtitle: "SpaceOS-9 桌面使用与快捷键指南",
        keywords: "help manual guide doc 帮助 手册 快捷键 文档 指南",
        icon: Icon::Help,
        mode: "help",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    None,
    DesktopIcon(usize),
    Start,
    Pinned(usize),
    Window(usize),
    Bell,
    MenuRow(usize),
    Logout,
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
    ctx_menu: Window,
    ctx_canvas: Canvas,
    ctx_open: bool,
    toast_window: Window,
    toast_canvas: Canvas,
    toast: Option<Toast>,
    windows: Vec<Managed>,
    active: Option<Window>,
    hover: Hit,
    clock_tick: (i32, i32, i32),
    last_launch: Option<(&'static str, Instant)>,
    running: bool,
}

fn session_identity() -> String {
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

fn now_local() -> (i32, i32, i32, i32, i32, i32, i32) {
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

fn ctx_row_rect(index: usize) -> Rect {
    let y = match index {
        0 => 6,
        1 => 38,
        2 => 70,
        _ => 110,
    };
    Rect::new(6, y, CTX_WIDTH - 12, 30)
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
    } else if title.contains("设置") || lower.contains("setting") {
        Icon::Settings
    } else if title.contains("帮助") || lower.contains("help") || title.contains("手册") {
        Icon::Help
    } else if title.contains("登录") || title.contains("账户") {
        Icon::Login
    } else {
        Icon::SpaceLogo
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

        let menu_y = if config.panel_position == PanelPosition::Top {
            panel_h + 6
        } else {
            (taskbar_y - MENU_HEIGHT - 6).max(8)
        };
        let menu = x.generate_id()?;
        x.create_window(
            menu,
            x.root,
            6,
            menu_y as i16,
            MENU_WIDTH as u16,
            MENU_HEIGHT as u16,
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
        x.set_rounded_shape(menu, MENU_WIDTH as u16, MENU_HEIGHT as u16, RADIUS_POPUP)?;

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
        let menu_canvas = x.create_canvas(MENU_WIDTH as usize, MENU_HEIGHT as usize, theme.panel);
        let ctx_canvas = x.create_canvas(CTX_WIDTH as usize, CTX_HEIGHT as usize, theme.panel);
        let toast_canvas =
            x.create_canvas(TOAST_WIDTH as usize, TOAST_HEIGHT as usize, theme.panel);

        let mut shell = Self {
            x,
            text,
            config,
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
            ctx_menu,
            ctx_canvas,
            ctx_open: false,
            toast_window,
            toast_canvas,
            toast: None,
            windows: Vec::new(),
            active: None,
            hover: Hit::None,
            clock_tick: (-1, -1, -1),
            last_launch: None,
            running: true,
        };
        shell.claim_wm()?;
        shell.apply_x11_settings(None);
        shell.reload_wallpaper(true);
        shell.draw_desktop();
        shell.present_desktop()?;
        shell.update_clock();
        shell.draw_taskbar();
        shell.present_taskbar()?;
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

    fn start_button_rect(&self) -> Rect {
        let ph = self.panel_height();
        let bh = (ph - 10).clamp(28, 42);
        Rect::new(6, (ph - bh) / 2, 86, bh)
    }

    fn pinned_rect(&self, index: usize) -> Rect {
        let ph = self.panel_height();
        let bh = (ph - 10).clamp(28, 42);
        Rect::new(102 + index as i32 * 38, (ph - bh) / 2, 34, bh)
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
                self.menu,
                6,
                self.menu_y() as i16,
                MENU_WIDTH as u16,
                MENU_HEIGHT as u16,
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

        let wp_file_changed = prev.wallpaper != self.config.wallpaper;
        let backdrop_changed = wp_file_changed
            || prev.backdrop_style != self.config.backdrop_style
            || prev.solid_color != self.config.solid_color;
        if backdrop_changed {
            self.reload_wallpaper(wp_file_changed);
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
        if self.toast.is_some() {
            self.draw_toast();
            self.present_toast()?;
        }
        self.x.raise(self.taskbar)?;
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

    fn event_loop(&mut self) -> Result<()> {
        while self.running {
            self.poll_uxconf()?;

            let mut raw = Vec::new();
            while let Some(event) = self.x.poll()? {
                raw.push(event);
            }
            if raw.is_empty() {
                let ux_fd = self.watch.as_ref().map(UxConfWatch::as_raw_fd);
                let timeout = if self.config.clock_seconds { 200 } else { 500 };
                let (_, ux_ready, _) = self.x.wait_fds(ux_fd, None, timeout);
                if ux_ready {
                    self.poll_uxconf()?;
                }
                while let Some(event) = self.x.poll()? {
                    raw.push(event);
                }
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

            if self.update_clock() {
                self.draw_taskbar();
                self.present_taskbar()?;
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
        self.x.move_resize(
            self.menu,
            6,
            self.menu_y() as i16,
            MENU_WIDTH as u16,
            MENU_HEIGHT as u16,
        )?;
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
        self.draw_desktop();
        self.present_desktop()?;
        self.draw_taskbar();
        self.present_taskbar()?;
        self.x.raise(self.taskbar)?;
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
                        let target = active.filter(|window| {
                            !self.menu_open
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
                    if let Some(managed) = self.windows.iter_mut().find(|w| w.window == e.window) {
                        if managed.title != title {
                            managed.icon = infer_window_icon(&title);
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
                } else if e.window == self.toast_window && self.toast.is_some() {
                    self.toast_canvas.mark_dirty(area);
                    self.present_toast()?;
                }
            }
            Event::LeaveNotify(e) => {
                let prev = self.hover;
                if (e.event == self.taskbar
                    && matches!(
                        prev,
                        Hit::Start | Hit::Pinned(_) | Hit::Window(_) | Hit::Bell
                    ))
                    || (e.event == self.menu && matches!(prev, Hit::MenuRow(_) | Hit::Logout))
                    || (e.event == self.ctx_menu && matches!(prev, Hit::ContextRow(_)))
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
                    }
                }
            }
            Event::ButtonPress(e) => {
                let (x, y) = (e.event_x as i32, e.event_y as i32);
                if e.event == self.taskbar && e.detail == 1 {
                    self.close_ctx()?;
                    self.taskbar_click(x, y)?;
                } else if e.event == self.menu && e.detail == 1 {
                    self.menu_click(x, y)?;
                } else if e.event == self.ctx_menu && e.detail == 1 {
                    self.ctx_click(x, y)?;
                } else if e.event == self.desktop {
                    if self.menu_open {
                        self.close_menu()?;
                    }
                    if e.detail == 3 {
                        self.open_ctx(i32::from(e.root_x), i32::from(e.root_y))?;
                    } else if e.detail == 1 {
                        self.close_ctx()?;
                        let hit = self.desktop_hit(x, y);
                        if let Hit::DesktopIcon(idx) = hit {
                            self.selected_desktop_icon = Some(idx);
                            self.draw_desktop();
                            self.present_desktop()?;
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
                } else if e.event == self.menu && self.menu_open {
                    let hit = self.menu_hit(x, y);
                    if hit != self.hover {
                        self.hover = hit;
                        self.draw_menu();
                        self.present_menu()?;
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
                if self.ctx_open && key.is_escape() {
                    self.close_ctx()?;
                } else if self.menu_open {
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

    fn manage(&mut self, window: Window) -> Result<()> {
        if let Some(existing) = self.windows.iter_mut().find(|w| w.window == window) {
            existing.minimized = false;
            self.x.map(window)?;
            self.activate(window)?;
            return Ok(());
        }
        let title = self.window_title(window);
        let icon = infer_window_icon(&title);
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
        let ph = self.panel_height();
        let bh = (ph - 12).clamp(26, 40);
        let start_x = if self.config.panel_show_pinned {
            102 + (APPS.len() as i32) * 38 + 10
        } else {
            106
        };
        let right_reserve = self.tray_width() + 56;
        let available = (i32::from(self.x.width) - start_x - right_reserve).max(120);
        let count = self.windows.len().clamp(1, 6) as i32;
        let btn_w = ((available - (count - 1) * 4) / count).clamp(80, 160);
        Rect::new(
            start_x + index as i32 * (btn_w + 4),
            (ph - bh) / 2,
            btn_w,
            bh,
        )
    }

    fn bell_rect(&self) -> Rect {
        let w = i32::from(self.x.width);
        let ph = self.panel_height();
        let bh = (ph - 12).clamp(26, 38);
        Rect::new(w - self.tray_width() - 40, (ph - bh) / 2, 34, bh)
    }

    fn taskbar_hit(&self, x: i32, y: i32) -> Hit {
        if self.start_button_rect().contains(x, y) {
            return Hit::Start;
        }
        if self.config.panel_show_pinned {
            for i in 0..APPS.len() {
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
        if self.bell_rect().contains(x, y) {
            return Hit::Bell;
        }
        Hit::None
    }

    fn taskbar_click(&mut self, x: i32, y: i32) -> Result<()> {
        match self.taskbar_hit(x, y) {
            Hit::Start => self.toggle_menu(),
            Hit::Pinned(idx) => {
                self.close_menu()?;
                self.launch(APPS[idx].mode);
                Ok(())
            }
            Hit::Window(index) => {
                self.close_menu()?;
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
            Hit::Bell => {
                self.close_menu()?;
                self.show_toast("系统通知", "当前没有未读系统通知");
                Ok(())
            }
            _ => {
                self.close_menu()?;
                Ok(())
            }
        }
    }

    fn menu_hit(&self, x: i32, y: i32) -> Hit {
        let filtered = self.filtered_apps();
        for (slot, &app_idx) in filtered.iter().enumerate() {
            if menu_row_rect(slot).contains(x, y) {
                return Hit::MenuRow(app_idx);
            }
        }
        if Rect::new(MENU_WIDTH - 96, MENU_HEIGHT - 42, 82, 30).contains(x, y) {
            return Hit::Logout;
        }
        Hit::None
    }

    fn menu_click(&mut self, x: i32, y: i32) -> Result<()> {
        match self.menu_hit(x, y) {
            Hit::MenuRow(app_idx) => {
                let mode = APPS[app_idx].mode;
                self.close_menu()?;
                self.launch(mode);
                Ok(())
            }
            Hit::Logout => {
                self.close_menu()?;
                // DDM owns the session.  Exiting the shell makes the daemon
                // tear down the whole process group and show the greeter again.
                std::process::exit(0);
            }
            _ => Ok(()),
        }
    }

    fn open_ctx(&mut self, root_x: i32, root_y: i32) -> Result<()> {
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
            self.menu_open = true;
            self.menu_query.clear();
            self.hover = Hit::None;
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
        let _ = Command::new(path).arg(mode).spawn();
    }

    fn show_toast(&mut self, title: &str, body: &str) {
        self.toast = Some(Toast {
            title: title.to_string(),
            body: body.to_string(),
            deadline: Instant::now() + Duration::from_secs(4),
        });
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
        let start = self.start_button_rect();
        let show_pinned = self.config.panel_show_pinned;
        let pinned_rects: [Rect; 8] = std::array::from_fn(|i| self.pinned_rect(i));
        let win_count = self.windows.len().min(6);
        let win_rects: Vec<Rect> = (0..win_count).map(|i| self.window_button_rect(i)).collect();
        let bell = self.bell_rect();
        let tray_w = self.tray_width();
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
        if start_active {
            canvas.rounded_rect(
                start,
                RADIUS_CONTROL,
                if self.menu_open {
                    self.theme.accent_soft
                } else {
                    self.theme.panel_hover
                },
                1.0,
            );
            canvas.rounded_border(
                start,
                RADIUS_CONTROL,
                if self.menu_open {
                    self.theme.accent
                } else {
                    self.theme.panel_border
                },
                1.0,
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
        canvas.fill(Rect::new(97, sep_y, 1, sep_h), self.theme.panel_border);

        // Quick-launch pinned icons (optional).
        if show_pinned {
            for (i, app) in APPS.iter().enumerate() {
                let rect = pinned_rects[i];
                if self.hover == Hit::Pinned(i) {
                    canvas.rounded_rect(rect, RADIUS_CONTROL, self.theme.panel_hover, 1.0);
                }
                icons::draw(
                    canvas,
                    app.icon,
                    rect.x + 6,
                    rect.y + (rect.h - 22) / 2,
                    22,
                    None,
                );
            }
            canvas.fill(Rect::new(257, sep_y, 1, sep_h), self.theme.panel_border);
        }

        // Running window buttons.
        for (index, &rect) in win_rects.iter().enumerate() {
            let managed = &self.windows[index];
            let is_active = !managed.minimized && self.active == Some(managed.window);
            let is_hover = self.hover == Hit::Window(index);
            let bg = if is_active {
                self.theme.panel_high
            } else if is_hover {
                self.theme.panel_hover
            } else {
                self.theme.panel
            };
            canvas.rounded_rect(rect, RADIUS_CONTROL, bg, 1.0);
            if is_active || is_hover {
                canvas.rounded_border(rect, RADIUS_CONTROL, self.theme.panel_border, 1.0, 1);
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
                rect.x + 9,
                rect.y + (rect.h - 16) / 2,
                16,
                None,
            );
            let max_w = (rect.w - 36).max(24) as f32;
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
                (rect.x + 30) as f32,
                rect.y as f32 + rect.h as f32 * 0.5 + 4.5,
                12.0,
                fg,
                Face::Ui,
            );
        }

        // Right tray: notification bell & configurable clock.
        if self.hover == Hit::Bell {
            canvas.rounded_rect(bell, RADIUS_CONTROL, self.theme.panel_hover, 1.0);
        }
        icons::draw(
            canvas,
            if self.toast.is_some() {
                Icon::BellActive
            } else {
                Icon::Bell
            },
            bell.x + (bell.w - 16) / 2,
            bell.y + (bell.h - 16) / 2,
            16,
            Some(if self.toast.is_some() {
                self.theme.accent
            } else {
                self.theme.text_dim
            }),
        );
        canvas.fill(
            Rect::new(w - tray_w, sep_y, 1, sep_h),
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
        canvas.fill(Rect::new(1, h - 54, w - 2, 53), self.theme.window_alt);
        canvas.fill(Rect::new(1, h - 54, w - 2, 1), self.theme.panel_border);
        icons::draw(canvas, Icon::SpaceLogo, 16, h - 38, 22, None);
        let identity = session_identity();
        self.text.draw(
            canvas,
            &identity,
            46.0,
            (h - 22) as f32,
            12.5,
            self.theme.text,
            Face::Ui,
        );

        let logout = Rect::new(w - 96, h - 42, 82, 30);
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
}
