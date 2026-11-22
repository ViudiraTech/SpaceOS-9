/*
 *
 *       user/desktop/src/app.rs
 *       Application window lifecycle, 8-edge resize, SHM present and uxconfd live sync
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Every SPUI application owns one top-level window with 34px client-side
//! decorations. `App` manages `MIT-SHM` canvas resizing, Minimize / Maximize /
//! Close buttons (left- or right-aligned), configurable titlebar double-click
//! action, 4-edge + 4-corner resizing with Breeze resize cursors, and live
//! `uxconfd` configuration subscriptions.

use crate::draw::{Canvas, Rect};
use crate::icons::Icon;
use crate::text::TextRenderer;
use crate::theme::{self, Theme};
use crate::ui::{self, CaptionHit};
use crate::x11::{CursorKind, Key, Result, X};
use spime::client::SpimeClient;
use spime::proto::DaemonReply;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uxconf::client::{UxConfClient, UxConfWatch};
use uxconf::schema::channel_defaults;
use uxconf::{ButtonLayout, DesktopConfig, DoubleClickAction, PanelPosition, Value};
use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt, EventMask, Window};
use x11rb::protocol::Event;

const RESIZE_BORDER: i32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeDir {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl ResizeDir {
    fn cursor(self) -> CursorKind {
        match self {
            Self::Left | Self::Right => CursorKind::ResizeH,
            Self::Top | Self::Bottom => CursorKind::ResizeV,
            Self::TopLeft | Self::BottomRight => CursorKind::ResizeDiagF,
            Self::TopRight | Self::BottomLeft => CursorKind::ResizeDiagB,
        }
    }

    const fn cmd_mode(self) -> u32 {
        match self {
            Self::TopLeft => 1,
            Self::Top => 2,
            Self::TopRight => 3,
            Self::Right => 4,
            Self::BottomRight => 5,
            Self::Bottom => 6,
            Self::BottomLeft => 7,
            Self::Left => 8,
        }
    }
}

#[derive(Clone, Copy)]
enum DragState {
    Move {
        orig_x: i16,
        orig_y: i16,
        press_rx: i16,
        press_ry: i16,
        scr_w: i32,
        scr_h: i32,
        work_top: i32,
        work_h: i32,
        restore_pending: bool,
    },
    Resize {
        dir: ResizeDir,
        orig_x: i32,
        orig_y: i32,
        orig_w: i32,
        orig_h: i32,
        press_rx: i32,
        press_ry: i32,
    },
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum UiEvent {
    Expose,
    Configure {
        width: u16,
        height: u16,
    },
    Key(Key),
    CommitText(String),
    Preedit(String),
    Button {
        x: i32,
        y: i32,
        button: u8,
        down: bool,
        state: u16,
    },
    Wheel {
        x: i32,
        y: i32,
        delta: i32,
    },
    Motion {
        x: i32,
        y: i32,
        state: u16,
    },
    ConfigChanged,
    Close,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct AnimFloat {
    pub value: f32,
    pub target: f32,
}

pub struct App {
    pub x: X,
    pub win: Window,
    pub icon: Icon,
    pub width: u16,
    pub height: u16,
    pub min_width: u16,
    pub min_height: u16,
    pub canvas: Canvas,
    /// Server-side copy of `canvas` installed as the window background. Xorg
    /// repaints uncovered regions from it during a drag, so other applications
    /// never receive a redraw storm just because a window moved above them.
    backing: u32,
    pending_backing_sync: bool,
    is_argb: bool,
    depth: u8,
    composited: bool,
    glass_top_extra: i32,
    pub text: TextRenderer,
    pub config: DesktopConfig,
    pub uxconf: Option<UxConfClient>,
    watch: Option<UxConfWatch>,
    pub theme: Theme,
    pub title: String,
    pub active: bool,
    pub maximized: bool,
    snapped: bool,
    drag_snap_zone: u32,
    pub needs_redraw: bool,
    pub close: bool,
    drawn: bool,
    saved_geom: Option<(i16, i16, u16, u16)>,
    drag: Option<DragState>,
    caption_hover: CaptionHit,
    caption_pressed: CaptionHit,
    current_cursor: CursorKind,
    content_cursor: CursorKind,
    last_title_click: Option<Instant>,
    pub anim_active: bool,
    last_frame: Instant,
    last_dt: f32,
    #[allow(dead_code)]
    anims: HashMap<u64, AnimFloat>,
    pub ime: SpimeClient,
    pub ime_preedit: String,
    ime_focused: bool,
    ime_active_sent: bool,
    last_win_origin: Option<(i16, i16)>,
    last_ime_spot: Option<(i32, i32, i32)>,
    last_ime_spot_local: Option<Rect>,
    theme_fp: u64,
    last_theme_fp_check: Instant,
}

impl App {
    pub fn new(title: &str, icon: Icon, width: u16, height: u16) -> Result<Self> {
        let x = X::connect()?;
        let mut uxconf = UxConfClient::connect().ok();
        let mut config = uxconf
            .as_mut()
            .map(UxConfClient::load_desktop_config)
            .unwrap_or_default();
        if let Some(missing) = theme::fallback_missing_theme(&mut config) {
            eprintln!(
                "theme '{missing}' is no longer installed; falling back to the default theme"
            );
            if let Some(client) = uxconf.as_mut() {
                let _ = client.set(
                    "appearance",
                    "/theme/id",
                    Value::String(config.theme_id.clone()),
                );
                let _ = client.set(
                    "appearance",
                    "/theme/style",
                    Value::String(config.theme_style.as_str().to_string()),
                );
                let _ = client.set(
                    "appearance",
                    "/theme/mode",
                    Value::String(config.mode.as_str().to_string()),
                );
                let _ = client.set(
                    "appearance",
                    "/theme/accent",
                    Value::String(format!("#{:06x}", config.accent)),
                );
                let _ = client.set(
                    "desktop",
                    "/backdrop/wallpaper",
                    Value::String(config.wallpaper.clone()),
                );
            }
        }
        let watch = UxConfWatch::subscribe(None).ok();
        let theme = Theme::from_config(&config);

        // Listen for `_NET_ACTIVE_WINDOW` changes on the root window.
        let _ = x.conn.change_window_attributes(
            x.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        );

        let win = x.generate_id()?;
        let mask = EventMask::EXPOSURE
            | EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::STRUCTURE_NOTIFY
            | EventMask::FOCUS_CHANGE
            | EventMask::LEAVE_WINDOW;
        let (win, is_argb) =
            x.create_argb_window(win, x.root, 0, 0, width, height, false, mask, theme.window)?;
        let depth = if is_argb { 32 } else { x.depth };
        let composited = x.is_compositor_active();
        x.set_title(win, title)?;
        x.set_icon_name(win, icon.prop_name())?;
        x.set_class(win, "spui", "Spui")?;
        x.set_wm_type(win, x.atoms.net_wm_type_normal)?;
        x.set_pid(win, std::process::id())?;
        x.set_protocols(win)?;
        let init_corner = if theme.is_win98() {
            0
        } else {
            theme.radius_window()
        };
        let init_glass_top = if theme.is_vortexglass() {
            theme.titlebar_height().min(height as i32)
        } else {
            0
        };
        let _ = x.set_glass_extents(win, init_glass_top, 0, 0, 0, init_corner);
        if !is_argb && init_corner > 0 {
            let _ = x.set_rounded_shape(win, width, height, init_corner);
        }
        x.map(win)?;
        x.flush()?;

        let mut text =
            TextRenderer::load().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        text.set_ui_scale(theme.ui_scale());
        let mut canvas = x.create_canvas(width as usize, height as usize, theme.window);
        canvas.set_argb(is_argb && composited && theme.is_vortexglass());
        if canvas.is_argb() {
            canvas.clear(theme.window);
            canvas.clear_transparent(
                Rect::new(0, 0, i32::from(width), init_glass_top),
                theme.titlebar,
            );
        }
        // In composited mode XComposite already owns an off-screen backing pixmap,
        // so creating a duplicate server-side backing pixmap doubles SHM uploads.
        let backing = if composited {
            0
        } else {
            let b = x.create_backing_depth(win, depth, width, height)?;
            x.put_canvas_depth(b, &canvas, canvas.bounds(), depth)?;
            x.set_background_pixmap(win, b)?;
            b
        };
        x.flush()?;
        Ok(Self {
            x,
            win,
            icon,
            width,
            height,
            min_width: 360.min(width),
            min_height: 220.min(height),
            canvas,
            backing,
            pending_backing_sync: false,
            is_argb,
            depth,
            composited,
            glass_top_extra: 0,
            text,
            config,
            uxconf,
            watch,
            theme,
            title: title.to_string(),
            active: true,
            maximized: false,
            snapped: false,
            drag_snap_zone: 0,
            needs_redraw: true,
            close: false,
            drawn: false,
            saved_geom: None,
            drag: None,
            caption_hover: CaptionHit::None,
            caption_pressed: CaptionHit::None,
            current_cursor: CursorKind::Default,
            content_cursor: CursorKind::Default,
            last_title_click: None,
            anim_active: false,
            last_frame: Instant::now(),
            last_dt: 0.016,
            anims: HashMap::new(),
            ime: SpimeClient::new(win),
            ime_preedit: String::new(),
            ime_focused: false,
            ime_active_sent: false,
            last_win_origin: None,
            last_ime_spot: None,
            last_ime_spot_local: None,
            theme_fp: theme::installed_themes_fingerprint(),
            last_theme_fp_check: Instant::now(),
        })
    }

    pub fn update_frame_time(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.last_dt = dt.clamp(0.001, 0.100);
    }

    #[allow(dead_code)]
    pub fn anim(&mut self, id: u64, target: f32, speed: f32) -> f32 {
        let dt = self.last_dt;
        let entry = self.anims.entry(id).or_insert(AnimFloat {
            value: target,
            target,
        });
        entry.target = target;
        let factor = 1.0 - (-speed * dt).exp();
        entry.value += (entry.target - entry.value) * factor;
        if (entry.value - entry.target).abs() <= 0.002 {
            entry.value = entry.target;
        }
        let out = entry.value;
        self.anim_active = self
            .anims
            .values()
            .any(|a| (a.value - a.target).abs() > 0.002);
        out
    }

    #[allow(dead_code)]
    pub fn anim_bool(&mut self, id: u64, target: bool, speed: f32) -> f32 {
        self.anim(id, if target { 1.0 } else { 0.0 }, speed)
    }

    pub fn buttons_left(&self) -> bool {
        self.config.wm_button_layout == ButtonLayout::Left
    }

    /// Write `(channel, property, value)` to `uxconfd` and apply immediately in-process.
    pub fn set_config_property(&mut self, channel: &str, property: &str, value: Value) {
        if self.uxconf.is_none() {
            self.uxconf = UxConfClient::connect().ok();
        }
        if let Some(client) = self.uxconf.as_mut() {
            let _ = client.set(channel, property, value.clone());
        }
        if self.config.apply_property(channel, property, &value) {
            self.sync_config_visuals();
        }
    }

    /// Apply `(channel, property, value)` in-process only; pair with a later
    /// `set_config_property` to persist (used for slider-drag previews).
    pub fn stage_config_property(&mut self, channel: &str, property: &str, value: Value) {
        if self.config.apply_property(channel, property, &value) {
            self.sync_config_visuals();
        }
    }

    /// Reset an entire `channel` back to its schema defaults in `uxconfd`.
    pub fn reset_config_channel(&mut self, channel: &str) {
        if self.uxconf.is_none() {
            self.uxconf = UxConfClient::connect().ok();
        }
        if let Some(client) = self.uxconf.as_mut() {
            let _ = client.reset(channel, "/", true);
            if let Ok(entries) = client.get_all(channel, "/") {
                for (prop, val) in entries {
                    self.config.apply_property(channel, &prop, &val);
                }
                self.sync_config_visuals();
                return;
            }
        }
        for (prop, val) in channel_defaults(channel) {
            self.config.apply_property(channel, prop, &val);
        }
        self.sync_config_visuals();
    }

    #[allow(dead_code)]
    pub fn is_composited_glass(&self) -> bool {
        self.is_argb && self.composited && self.theme.is_vortexglass()
    }

    #[allow(dead_code)]
    pub fn set_glass_toolbar_height(&mut self, height: i32) {
        let clamped = height.max(0);
        if clamped != self.glass_top_extra {
            self.glass_top_extra = clamped;
            self.sync_glass_extents();
        }
    }

    fn sync_glass_extents(&self) {
        let corner_r = if self.maximized || self.snapped || self.theme.is_win98() {
            0
        } else {
            self.theme.radius_window()
        };
        let top = if self.theme.is_vortexglass() {
            (self.theme.titlebar_height() + self.glass_top_extra).min(self.height as i32)
        } else {
            0
        };
        let _ = self
            .x
            .set_glass_extents(self.win, top, 0, 0, 0, corner_r);
    }

    fn sync_shape_and_canvas_mode(&mut self) {
        let argb_glass = self.is_composited_glass();
        self.canvas.set_argb(argb_glass);
        if !self.is_argb {
            let corner_r = if self.maximized || self.snapped || self.theme.is_win98() {
                0
            } else {
                self.theme.radius_window()
            };
            let _ = self
                .x
                .set_rounded_shape(self.win, self.width, self.height, corner_r);
        }
    }

    fn sync_config_visuals(&mut self) {
        if let Some(missing) = theme::fallback_missing_theme(&mut self.config) {
            eprintln!(
                "theme '{missing}' is no longer installed; falling back to the default theme"
            );
        }
        self.theme_fp = theme::installed_themes_fingerprint();
        self.last_theme_fp_check = Instant::now();
        self.theme = Theme::from_config(&self.config);
        self.text.set_ui_scale(self.theme.ui_scale());
        if self.maximized {
            if let Ok(cookie) = self.x.conn.get_geometry(self.x.root) {
                if let Ok(root_geom) = cookie.reply() {
                    let panel_h = self.panel_height() as u16;
                    let max_y = if self.config.panel_position == PanelPosition::Top {
                        panel_h as i16
                    } else {
                        0
                    };
                    let max_h = root_geom.height.saturating_sub(panel_h).max(200);
                    let _ = self
                        .x
                        .move_resize(self.win, 0, max_y, root_geom.width, max_h);
                    let _ = self.x.flush();
                }
            }
        }
        self.sync_shape_and_canvas_mode();
        self.sync_glass_extents();
        self.needs_redraw = true;
    }

    pub fn panel_height(&self) -> i32 {
        self.theme.panel_height(&self.config)
    }

    pub fn content(&self) -> Rect {
        let bw = self.theme.window_border_width();
        let th = self.theme.titlebar_height();
        Rect::new(
            bw,
            th,
            (self.width as i32 - bw * 2).max(1),
            (self.height as i32 - th - bw).max(1),
        )
    }

    pub fn set_content_cursor(&mut self, kind: CursorKind) {
        self.content_cursor = kind;
    }

    fn win_origin(&mut self) -> (i16, i16) {
        if let Some(pos) = self.last_win_origin {
            return pos;
        }
        let pos = self
            .x
            .conn
            .translate_coordinates(self.win, self.x.root, 0, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| (r.dst_x, r.dst_y))
            .unwrap_or((0, 0));
        self.last_win_origin = Some(pos);
        pos
    }

    fn sync_ime_focus(&mut self) {
        let should_be_active = self.active && self.ime_focused;
        if should_be_active == self.ime_active_sent {
            return;
        }
        self.ime_active_sent = should_be_active;
        if should_be_active {
            self.ime.focus_in(self.win);
            if let Some(local) = self.last_ime_spot_local {
                let (ox, oy) = self.win_origin();
                let rx = i32::from(ox) + local.x;
                let ry = i32::from(oy) + local.y;
                let h = local.h.max(1);
                self.last_ime_spot = Some((rx, ry, h));
                self.ime.set_spot(rx, ry, h);
            }
        } else {
            self.ime.focus_out(self.win);
            self.last_ime_spot = None;
            if !self.ime_preedit.is_empty() {
                self.ime_preedit.clear();
                self.needs_redraw = true;
            }
        }
    }

    /// Enable or disable IME composition for the currently focused widget in this window.
    pub fn set_ime_focus(&mut self, focused: bool) {
        if self.ime_focused != focused {
            self.ime_focused = focused;
            self.sync_ime_focus();
        }
    }

    /// Update the IME candidate popup anchor using window-local pixel coordinates.
    pub fn set_ime_spot(&mut self, local_rect: Rect) {
        self.last_ime_spot_local = Some(local_rect);
        let (ox, oy) = self.win_origin();
        let rx = i32::from(ox) + local_rect.x;
        let ry = i32::from(oy) + local_rect.y;
        let h = local_rect.h.max(1);
        if self.last_ime_spot == Some((rx, ry, h)) {
            return;
        }
        self.last_ime_spot = Some((rx, ry, h));
        if self.active && self.ime_focused {
            self.ime.set_spot(rx, ry, h);
        }
    }

    /// Reset any in-progress IME preedit buffer (e.g. when clearing or switching fields).
    #[allow(dead_code)]
    pub fn reset_ime(&mut self) {
        self.ime.reset();
        if !self.ime_preedit.is_empty() {
            self.ime_preedit.clear();
            self.needs_redraw = true;
        }
    }

    fn apply_ime_reply(&mut self, reply: DaemonReply, events: &mut Vec<UiEvent>) {
        if reply.preedit.text != self.ime_preedit {
            self.ime_preedit = reply.preedit.text.clone();
            events.push(UiEvent::Preedit(reply.preedit.text));
            self.needs_redraw = true;
        }
        if !reply.commit_text.is_empty() {
            events.push(UiEvent::CommitText(reply.commit_text));
            self.needs_redraw = true;
        }
    }

    fn apply_cursor(&mut self, kind: CursorKind) {
        if kind != self.current_cursor {
            self.current_cursor = kind;
            let _ = self.x.set_cursor(self.win, kind);
            let _ = self.x.flush();
        }
    }

    /// Release X11's implicit passive button grab and hand drag/resize ownership to `Shell`.
    fn initiate_wm_drag(&self, mode: u32, root_x: i16, root_y: i16, extra: u32) {
        self.x.ungrab_pointer();
        let _ = self.x.send_window_cmd_data(
            self.win,
            20,
            mode,
            u32::from(root_x as u16),
            u32::from(root_y as u16),
            extra,
        );
        let _ = self.x.flush();
    }

    fn hit_resize(&self, x: i32, y: i32) -> Option<ResizeDir> {
        if self.maximized {
            return None;
        }
        let w = self.width as i32;
        let h = self.height as i32;
        let left = x < RESIZE_BORDER;
        let right = x >= w - RESIZE_BORDER;
        let top = y < RESIZE_BORDER;
        let bottom = y >= h - RESIZE_BORDER;
        let corner = 12;
        if (left && y < corner) || (top && x < corner) {
            Some(ResizeDir::TopLeft)
        } else if (right && y < corner) || (top && x >= w - corner) {
            Some(ResizeDir::TopRight)
        } else if (left && y >= h - corner) || (bottom && x < corner) {
            Some(ResizeDir::BottomLeft)
        } else if (right && y >= h - corner) || (bottom && x >= w - corner) {
            Some(ResizeDir::BottomRight)
        } else if left {
            Some(ResizeDir::Left)
        } else if right {
            Some(ResizeDir::Right)
        } else if top {
            Some(ResizeDir::Top)
        } else if bottom {
            Some(ResizeDir::Bottom)
        } else {
            None
        }
    }

    pub fn toggle_maximize(&mut self) -> Result<()> {
        if self.maximized {
            self.maximized = false;
            self.snapped = false;
            if let Some((sx, sy, sw, sh)) = self.saved_geom.take() {
                self.x.move_resize(self.win, sx, sy, sw, sh)?;
            }
        } else {
            let geom = self.x.conn.get_geometry(self.win)?.reply()?;
            let root_geom = self.x.conn.get_geometry(self.x.root)?.reply()?;
            if !self.snapped {
                self.saved_geom = Some((geom.x, geom.y, self.width, self.height));
            }
            self.snapped = false;
            self.maximized = true;
            let panel_h = self.panel_height() as u16;
            let max_y = if self.config.panel_position == PanelPosition::Top {
                panel_h as i16
            } else {
                0
            };
            let max_h = root_geom.height.saturating_sub(panel_h).max(200);
            self.x
                .move_resize(self.win, 0, max_y, root_geom.width, max_h)?;
        }
        self.sync_shape_and_canvas_mode();
        self.sync_glass_extents();
        self.needs_redraw = true;
        self.x.flush()?;
        Ok(())
    }

    pub fn pump(&mut self) -> Result<Vec<UiEvent>> {
        let (events, _) = self.pump_with_fd(None, 50)?;
        Ok(events)
    }

    /// Block via `poll(2)` up to `timeout_ms` for X11, `uxconfd`, or `extra_fd` activity,
    /// coalescing burst pointer motion events so redraws never fall behind input.
    pub fn pump_with_fd(
        &mut self,
        extra_fd: Option<i32>,
        timeout_ms: i32,
    ) -> Result<(Vec<UiEvent>, bool)> {
        let anim_active = ui::has_active_animations() || self.anim_active;
        let effective_timeout = if anim_active {
            // Adaptive deadline-based frame pacing: target ~60 FPS (16.6ms intervals).
            let target_frame = Duration::from_millis(16);
            let target_deadline = self.last_frame + target_frame;
            let now = Instant::now();
            if target_deadline > now {
                let rem = (target_deadline - now).as_millis() as i32;
                rem.clamp(1, 16)
            } else {
                0
            }
        } else {
            timeout_ms
        };

        let mut raw = Vec::new();
        while let Some(ev) = self.x.poll()? {
            raw.push(ev);
        }

        let mut cfg_changed = false;
        if let Some(watch) = self.watch.as_mut() {
            let (changed, _) = watch.poll_into(&mut self.config);
            if changed {
                cfg_changed = true;
            }
        }

        let ux_fd = self.watch.as_ref().map(UxConfWatch::as_raw_fd);
        let mut extra_ready = false;
        if raw.is_empty() && !cfg_changed {
            let (_, ux_ready, ext) = self.x.wait_fds(ux_fd, extra_fd, effective_timeout);
            extra_ready = ext;
            if ux_ready {
                if let Some(watch) = self.watch.as_mut() {
                    let (changed, _) = watch.poll_into(&mut self.config);
                    if changed {
                        cfg_changed = true;
                    }
                }
            }
            while let Some(ev) = self.x.poll()? {
                raw.push(ev);
            }
        } else if extra_fd.is_some() || ux_fd.is_some() {
            let (_, ux_ready, ext) = self.x.wait_fds(ux_fd, extra_fd, 0);
            extra_ready = ext;
            if ux_ready {
                if let Some(watch) = self.watch.as_mut() {
                    let (changed, _) = watch.poll_into(&mut self.config);
                    if changed {
                        cfg_changed = true;
                    }
                }
            }
        }

        // Measure true wall-clock dt after waiting, for the next frame render.
        self.update_frame_time();

        // Coalesce burst MotionNotify and ConfigureNotify events cleanly.
        let mut coalesced: Vec<Event> = Vec::with_capacity(raw.len());
        for ev in raw {
            match &ev {
                Event::MotionNotify(_)
                    if matches!(coalesced.last(), Some(Event::MotionNotify(_))) =>
                {
                    *coalesced.last_mut().unwrap() = ev;
                    continue;
                }
                Event::ConfigureNotify(curr)
                    if curr.window == self.win
                        && matches!(
                            coalesced.last(),
                            Some(Event::ConfigureNotify(prev)) if prev.window == self.win
                        ) =>
                {
                    *coalesced.last_mut().unwrap() = ev;
                    continue;
                }
                _ => {}
            }
            coalesced.push(ev);
        }

        if self.last_theme_fp_check.elapsed() >= Duration::from_secs(2) {
            self.last_theme_fp_check = Instant::now();
            let next_fp = theme::installed_themes_fingerprint();
            if next_fp != self.theme_fp {
                self.theme_fp = next_fp;
                cfg_changed = true;
            }
        }

        let mut events = Vec::new();
        if cfg_changed {
            self.sync_config_visuals();
            events.push(UiEvent::ConfigChanged);
        }
        for ev in coalesced {
            self.translate(ev, &mut events)?;
        }
        let now_animating = ui::has_active_animations() || self.anim_active;
        if anim_active || now_animating {
            self.needs_redraw = true;
        }
        Ok((events, extra_ready))
    }

    fn translate(&mut self, event: Event, events: &mut Vec<UiEvent>) -> Result<()> {
        match event {
            Event::Expose(e) if e.window == self.win => {
                // With a server-side backing pixmap Xorg has already repainted
                // the exposed rectangle before delivering this event. Handling
                // it as a UI redraw would make every extra window multiply the
                // work done during a drag.
                if self.backing == 0 {
                    if !self.drawn {
                        self.needs_redraw = true;
                        events.push(UiEvent::Expose);
                    } else if !self.composited {
                        let area = Rect::new(
                            i32::from(e.x),
                            i32::from(e.y),
                            i32::from(e.width),
                            i32::from(e.height),
                        );
                        self.x.put_canvas_depth(self.win, &self.canvas, area, self.depth)?;
                        self.x.flush()?;
                        events.push(UiEvent::Expose);
                    }
                }
            }
            Event::ConfigureNotify(e) if e.window == self.win => {
                self.last_win_origin = None;
                if self.ime_active_sent {
                    if let Some(local) = self.last_ime_spot_local {
                        self.set_ime_spot(local);
                    }
                }
                if e.width != self.width || e.height != self.height {
                    self.width = e.width;
                    self.height = e.height;
                    self.x.release_canvas(&self.canvas);
                    self.canvas = self.x.create_canvas(
                        e.width as usize,
                        e.height as usize,
                        self.theme.window,
                    );
                    self.canvas.set_argb(self.is_composited_glass());
                    // Defer replacing & uploading `backing` until `present()` finishes drawing the
                    // new-size frame so Xorg/Composite never exposes blank unrendered pixels.
                    self.pending_backing_sync = true;
                    self.sync_shape_and_canvas_mode();
                    self.sync_glass_extents();
                    self.drawn = false;
                    self.needs_redraw = true;
                    events.push(UiEvent::Configure {
                        width: e.width,
                        height: e.height,
                    });
                }
            }
            Event::FocusIn(e) if e.event == self.win => {
                if !self.active {
                    self.active = true;
                    self.needs_redraw = true;
                }
                self.sync_ime_focus();
            }
            Event::FocusOut(e) if e.event == self.win => {
                if self.active {
                    self.active = false;
                    self.needs_redraw = true;
                }
                let had_preedit = !self.ime_preedit.is_empty();
                self.sync_ime_focus();
                if had_preedit && self.ime_preedit.is_empty() {
                    events.push(UiEvent::Preedit(String::new()));
                }
            }
            Event::LeaveNotify(e) if e.event == self.win => {
                ui::notify_pointer_leave();
                if self.caption_hover != CaptionHit::None
                    || self.caption_pressed != CaptionHit::None
                {
                    self.caption_hover = CaptionHit::None;
                    self.needs_redraw = true;
                }
                if self.drag.is_none() {
                    self.apply_cursor(CursorKind::Default);
                }
            }
            Event::PropertyNotify(e) if e.window == self.x.root => {
                if e.atom == self.x.atoms.net_active_window {
                    let is_active = self.x.get_active_window() == Some(self.win);
                    if is_active != self.active {
                        self.active = is_active;
                        self.needs_redraw = true;
                    }
                } else if e.atom == self.x.atoms.spui_compositor {
                    let active = self.x.is_compositor_active();
                    if active != self.composited {
                        self.composited = active;
                        if active && self.backing != 0 {
                            let _ = self.x.conn.change_window_attributes(
                                self.win,
                                &ChangeWindowAttributesAux::new().background_pixmap(0u32),
                            );
                            let _ = self.x.free_pixmap(self.backing);
                            self.backing = 0;
                        } else if !active && self.backing == 0 {
                            if let Ok(b) = self.x.create_backing_depth(
                                self.win,
                                self.depth,
                                self.width,
                                self.height,
                            ) {
                                let _ = self.x.put_canvas_depth(
                                    b,
                                    &self.canvas,
                                    self.canvas.bounds(),
                                    self.depth,
                                );
                                let _ = self.x.set_background_pixmap(self.win, b);
                                self.backing = b;
                            }
                        }
                        self.sync_shape_and_canvas_mode();
                        self.needs_redraw = true;
                    }
                }
            }
            Event::KeyPress(e) => {
                let state: u16 = e.state.into();
                let sym = self.x.key_symbol(e.detail, state);
                if self.active && self.ime_focused {
                    if let Some(reply) =
                        self.ime
                            .key_event(self.win, true, sym, state, u16::from(e.detail))
                    {
                        let consumed = reply.consumed;
                        self.apply_ime_reply(reply, events);
                        if consumed {
                            return Ok(());
                        }
                    }
                }
                let key = Key {
                    code: e.detail,
                    state,
                    sym,
                };
                events.push(UiEvent::Key(key));
            }
            Event::KeyRelease(e) => {
                if self.active && self.ime_focused {
                    let state: u16 = e.state.into();
                    let sym = self.x.key_symbol(e.detail, state);
                    if let Some(reply) =
                        self.ime
                            .key_event(self.win, false, sym, state, u16::from(e.detail))
                    {
                        self.apply_ime_reply(reply, events);
                    }
                }
            }
            Event::ButtonPress(e) if e.event == self.win => {
                let (x, y) = (e.event_x as i32, e.event_y as i32);
                // Mouse wheel events (buttons 4 / 5), respecting natural scroll preference.
                if e.detail == 4 || e.detail == 5 {
                    let raw_delta = if e.detail == 4 { -3 } else { 3 };
                    let delta = if self.config.mouse_natural_scroll {
                        -raw_delta
                    } else {
                        raw_delta
                    };
                    events.push(UiEvent::Wheel { x, y, delta });
                    return Ok(());
                }

                let _ = self.x.focus(self.win);
                let _ = self.x.set_active_window(self.win);
                let _ = self.x.flush();
                ui::notify_pointer_button(x, y, e.detail, true);

                if e.detail == 1 {
                    let cap = ui::hit_caption_for(
                        &self.theme,
                        self.width as i32,
                        x,
                        y,
                        self.buttons_left(),
                    );
                    if cap != CaptionHit::None {
                        self.needs_redraw = true;
                        self.caption_pressed = cap;
                        self.caption_hover = cap;
                        return Ok(());
                    }
                    if let Some(dir) = self.hit_resize(x, y) {
                        if self.composited {
                            let packed_min =
                                (u32::from(self.min_width) << 16) | u32::from(self.min_height);
                            self.initiate_wm_drag(dir.cmd_mode(), e.root_x, e.root_y, packed_min);
                            return Ok(());
                        }
                        let geom = self.x.conn.get_geometry(self.win)?.reply()?;
                        self.drag = Some(DragState::Resize {
                            dir,
                            orig_x: i32::from(geom.x),
                            orig_y: i32::from(geom.y),
                            orig_w: i32::from(self.width),
                            orig_h: i32::from(self.height),
                            press_rx: i32::from(e.root_x),
                            press_ry: i32::from(e.root_y),
                        });
                        self.apply_cursor(dir.cursor());
                        return Ok(());
                    }
                    if y < self.theme.titlebar_height() {
                        let now = Instant::now();
                        let dc_limit = Duration::from_millis(self.config.mouse_double_click_ms);
                        if self.config.wm_double_click != DoubleClickAction::None
                            && self
                                .last_title_click
                                .is_some_and(|prev| now.duration_since(prev) <= dc_limit)
                        {
                            self.last_title_click = None;
                            match self.config.wm_double_click {
                                DoubleClickAction::Maximize => {
                                    self.toggle_maximize()?;
                                }
                                DoubleClickAction::Minimize => {
                                    let _ = self.x.send_window_cmd(self.win, 1);
                                    let _ = self.x.flush();
                                }
                                DoubleClickAction::None => {}
                            }
                            return Ok(());
                        }
                        self.last_title_click = Some(now);
                        if self.composited && !self.maximized && !self.snapped {
                            self.initiate_wm_drag(0, e.root_x, e.root_y, 0);
                            return Ok(());
                        }
                        let geom = self.x.conn.get_geometry(self.win)?.reply()?;
                        let root_geom = self.x.conn.get_geometry(self.x.root)?.reply()?;
                        let panel_h = self.panel_height();
                        let work_top = if self.config.panel_position == PanelPosition::Top {
                            panel_h
                        } else {
                            0
                        };
                        let work_h = i32::from(root_geom.height).saturating_sub(panel_h).max(200);
                        self.drag_snap_zone = 0;
                        self.drag = Some(DragState::Move {
                            orig_x: geom.x,
                            orig_y: geom.y.max(work_top as i16),
                            press_rx: e.root_x,
                            press_ry: e.root_y,
                            scr_w: i32::from(root_geom.width),
                            scr_h: i32::from(root_geom.height),
                            work_top,
                            work_h,
                            restore_pending: self.maximized || self.snapped,
                        });
                        return Ok(());
                    }
                    self.needs_redraw = true;
                }

                events.push(UiEvent::Button {
                    x,
                    y,
                    button: e.detail,
                    down: true,
                    state: e.state.into(),
                });
            }
            Event::ButtonRelease(e) if e.event == self.win => {
                let (x, y) = (e.event_x as i32, e.event_y as i32);
                ui::notify_pointer_button(x, y, e.detail, false);
                if e.detail == 1 {
                    let pressed_cap = self.caption_pressed;
                    self.caption_pressed = CaptionHit::None;
                    if pressed_cap != CaptionHit::None {
                        self.needs_redraw = true;
                        let cap = ui::hit_caption_for(
                            &self.theme,
                            self.width as i32,
                            x,
                            y,
                            self.buttons_left(),
                        );
                        self.caption_hover = cap;
                        if cap == pressed_cap {
                            match cap {
                                CaptionHit::Close => events.push(UiEvent::Close),
                                CaptionHit::Minimize => {
                                    let _ = self.x.send_window_cmd(self.win, 1);
                                    let _ = self.x.flush();
                                }
                                CaptionHit::Maximize => {
                                    self.toggle_maximize()?;
                                }
                                CaptionHit::None => {}
                            }
                        }
                        return Ok(());
                    }
                    self.needs_redraw = true;
                }
                if let Some(drag) = self.drag.take() {
                    self.apply_cursor(CursorKind::Default);
                    if let DragState::Move {
                        scr_w,
                        work_top,
                        work_h,
                        ..
                    } = drag
                    {
                        let zone = self.drag_snap_zone;
                        self.drag_snap_zone = 0;
                        if zone != 0 {
                            let _ = self.x.send_window_cmd_data(self.win, 10, 0, 0, 0, 0);
                            match zone {
                                3 => {
                                    if !self.maximized {
                                        let _ = self.toggle_maximize();
                                    }
                                }
                                1 | 2 => {
                                    if let Ok(cookie) = self.x.conn.get_geometry(self.win) {
                                        if let Ok(geom) = cookie.reply() {
                                            if !self.snapped && !self.maximized {
                                                self.saved_geom = Some((
                                                    geom.x,
                                                    geom.y,
                                                    self.width,
                                                    self.height,
                                                ));
                                            }
                                        }
                                    }
                                    self.maximized = false;
                                    self.snapped = true;
                                    let half = (scr_w / 2).max(200);
                                    let (sx, sw) = if zone == 1 {
                                        (0i16, half as u16)
                                    } else {
                                        (half as i16, (scr_w - half).max(200) as u16)
                                    };
                                    let _ = self.x.move_resize(
                                        self.win,
                                        sx,
                                        work_top as i16,
                                        sw,
                                        work_h as u16,
                                    );
                                    self.sync_shape_and_canvas_mode();
                                    self.sync_glass_extents();
                                    self.needs_redraw = true;
                                }
                                _ => {}
                            }
                            let _ = self.x.flush();
                        }
                    }
                }
                events.push(UiEvent::Button {
                    x,
                    y,
                    button: e.detail,
                    down: false,
                    state: e.state.into(),
                });
            }
            Event::MotionNotify(e) if e.event == self.win => {
                let (x, y) = (e.event_x as i32, e.event_y as i32);
                let btn1_held = u16::from(e.state) & (1 << 8) != 0;
                ui::notify_pointer_motion(x, y, btn1_held);
                if self.caption_pressed != CaptionHit::None && !btn1_held {
                    self.caption_pressed = CaptionHit::None;
                    self.needs_redraw = true;
                }
                if let Some(drag) = self.drag {
                    if btn1_held {
                        let min_y = if self.config.panel_position == PanelPosition::Top {
                            self.panel_height()
                        } else {
                            0
                        };
                        match drag {
                            DragState::Move {
                                mut orig_x,
                                mut orig_y,
                                press_rx,
                                press_ry,
                                scr_w,
                                scr_h,
                                work_top,
                                work_h: _,
                                restore_pending,
                            } => {
                                let dx = e.root_x.saturating_sub(press_rx);
                                let dy = e.root_y.saturating_sub(press_ry);
                                if restore_pending && (dx.abs() >= 10 || dy.abs() >= 10) {
                                    self.maximized = false;
                                    self.snapped = false;
                                    let (rw, rh) = self
                                        .saved_geom
                                        .take()
                                        .map(|(_, _, sw, sh)| (sw, sh))
                                        .unwrap_or((
                                            (scr_w * 3 / 5).max(i32::from(self.min_width)) as u16,
                                            (scr_h * 3 / 5).max(i32::from(self.min_height)) as u16,
                                        ));
                                    let new_x = (i32::from(e.root_x) - i32::from(rw) / 2)
                                        .clamp(0, (scr_w - i32::from(rw)).max(0))
                                        as i16;
                                    let new_y = (i32::from(e.root_y) - 14).max(work_top) as i16;
                                    orig_x = new_x;
                                    orig_y = new_y;
                                    self.x.move_resize(self.win, new_x, new_y, rw, rh)?;
                                    self.sync_shape_and_canvas_mode();
                                    self.sync_glass_extents();
                                    if self.composited {
                                        self.drag = None;
                                        self.initiate_wm_drag(0, e.root_x, e.root_y, 0);
                                        return Ok(());
                                    }
                                    self.drag = Some(DragState::Move {
                                        orig_x,
                                        orig_y,
                                        press_rx: e.root_x,
                                        press_ry: e.root_y,
                                        scr_w,
                                        scr_h,
                                        work_top,
                                        work_h: scr_h,
                                        restore_pending: false,
                                    });
                                } else if !restore_pending {
                                    let nx = orig_x.saturating_add(dx);
                                    let ny = orig_y
                                        .saturating_add(dy)
                                        .max(work_top as i16);
                                    self.x.move_window(self.win, nx, ny)?;
                                }
                                let zone = if i32::from(e.root_y) <= work_top + 6 {
                                    3
                                } else if e.root_x <= 10 {
                                    1
                                } else if i32::from(e.root_x) >= scr_w - 11 {
                                    2
                                } else {
                                    0
                                };
                                if zone != self.drag_snap_zone {
                                    self.drag_snap_zone = zone;
                                    let _ = self
                                        .x
                                        .send_window_cmd_data(self.win, 10, zone, 0, 0, 0);
                                }
                                self.x.flush()?;
                            }
                            DragState::Resize {
                                dir,
                                orig_x,
                                orig_y,
                                orig_w,
                                orig_h,
                                press_rx,
                                press_ry,
                            } => {
                                let dx = i32::from(e.root_x) - press_rx;
                                let dy = i32::from(e.root_y) - press_ry;
                                let min_w = i32::from(self.min_width);
                                let min_h = i32::from(self.min_height);
                                let (mut nx, mut ny, mut nw, mut nh) =
                                    (orig_x, orig_y, orig_w, orig_h);
                                match dir {
                                    ResizeDir::Right => nw = (orig_w + dx).max(min_w),
                                    ResizeDir::Bottom => nh = (orig_h + dy).max(min_h),
                                    ResizeDir::Left => {
                                        nw = (orig_w - dx).max(min_w);
                                        nx = orig_x + (orig_w - nw);
                                    }
                                    ResizeDir::Top => {
                                        nh = (orig_h - dy).max(min_h);
                                        ny = (orig_y + (orig_h - nh)).max(min_y);
                                    }
                                    ResizeDir::BottomRight => {
                                        nw = (orig_w + dx).max(min_w);
                                        nh = (orig_h + dy).max(min_h);
                                    }
                                    ResizeDir::BottomLeft => {
                                        nw = (orig_w - dx).max(min_w);
                                        nx = orig_x + (orig_w - nw);
                                        nh = (orig_h + dy).max(min_h);
                                    }
                                    ResizeDir::TopRight => {
                                        nw = (orig_w + dx).max(min_w);
                                        nh = (orig_h - dy).max(min_h);
                                        ny = (orig_y + (orig_h - nh)).max(min_y);
                                    }
                                    ResizeDir::TopLeft => {
                                        nw = (orig_w - dx).max(min_w);
                                        nx = orig_x + (orig_w - nw);
                                        nh = (orig_h - dy).max(min_h);
                                        ny = (orig_y + (orig_h - nh)).max(min_y);
                                    }
                                }
                                self.x.move_resize(
                                    self.win, nx as i16, ny as i16, nw as u16, nh as u16,
                                )?;
                                self.x.flush()?;
                            }
                        }
                        return Ok(());
                    } else {
                        self.drag = None;
                        if self.drag_snap_zone != 0 {
                            self.drag_snap_zone = 0;
                            let _ = self.x.send_window_cmd_data(self.win, 10, 0, 0, 0, 0);
                            let _ = self.x.flush();
                        }
                    }
                }

                let cap =
                    ui::hit_caption_for(&self.theme, self.width as i32, x, y, self.buttons_left());
                if cap != self.caption_hover {
                    self.caption_hover = cap;
                    self.needs_redraw = true;
                }

                let desired_cursor = if cap != CaptionHit::None {
                    CursorKind::Default
                } else if let Some(dir) = self.hit_resize(x, y) {
                    dir.cursor()
                } else if y < self.theme.titlebar_height() {
                    CursorKind::Default
                } else {
                    self.content_cursor
                };
                self.apply_cursor(desired_cursor);

                events.push(UiEvent::Motion {
                    x,
                    y,
                    state: e.state.into(),
                });
            }
            Event::ClientMessage(e) => {
                if e.type_ == self.x.atoms.wm_protocols
                    && e.data.as_data32()[0] == self.x.atoms.wm_delete
                {
                    events.push(UiEvent::Close);
                } else if e.type_ == self.x.atoms.spui_window_cmd {
                    let data = e.data.as_data32();
                    if data[0] == 21 {
                        let zone = data[1];
                        let orig_x = ((data[2] >> 16) as u16) as i16;
                        let orig_y = (data[2] & 0xffff) as u16 as i16;
                        let orig_w = (data[3] >> 16) as u16;
                        let orig_h = (data[3] & 0xffff) as u16;
                        if !self.maximized && !self.snapped && orig_w > 0 && orig_h > 0 {
                            self.saved_geom = Some((orig_x, orig_y, orig_w, orig_h));
                        }
                        self.maximized = zone == 3;
                        self.snapped = zone == 1 || zone == 2;
                        self.sync_shape_and_canvas_mode();
                        self.sync_glass_extents();
                        self.needs_redraw = true;
                    }
                } else if e.type_ == self.x.atoms.spime_notify {
                    for _ in 0..8 {
                        let Some(reply) = self.ime.poll_pending() else {
                            break;
                        };
                        let more = reply.consumed;
                        self.apply_ime_reply(reply, events);
                        if !more {
                            break;
                        }
                    }
                }
            }
            Event::DestroyNotify(e) if e.window == self.win => {
                self.close = true;
                events.push(UiEvent::Close);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn draw_chrome(&mut self) {
        let buttons_left = self.buttons_left();
        ui::titlebar(
            &mut self.canvas,
            &mut self.text,
            &self.theme,
            self.width as i32,
            self.icon,
            &self.title,
            self.active,
            self.maximized,
            buttons_left,
            self.caption_hover,
        );
        ui::window_border(&mut self.canvas, &self.theme, self.active, self.maximized);
    }

    pub fn present(&mut self) -> Result<()> {
        if !self.needs_redraw {
            return Ok(());
        }
        let full_bounds = Rect::new(0, 0, self.width as i32, self.height as i32);
        let dirty = self.canvas.take_dirty();
        let area = if self.pending_backing_sync || dirty.is_empty() {
            full_bounds
        } else {
            dirty.intersect(full_bounds)
        };
        if self.is_argb {
            let glass_top = if self.is_composited_glass() {
                (self.theme.titlebar_height() + self.glass_top_extra).min(self.height as i32)
            } else {
                0
            };
            let opaque_body = Rect::new(
                0,
                glass_top,
                self.width as i32,
                (self.height as i32 - glass_top).max(0),
            );
            self.canvas.enforce_opaque_rect(opaque_body.intersect(area));
            // Composited VortexGlass frames (`top.png`/`left.png`/`right.png`/`bottom.png`)
            // provide the outer rounded Aero border around a square client edge (like
            // Uinxed-Kernel); keep corner masking only for non-VortexGlass ARGB windows.
            let (top_r, bot_r) = if self.maximized
                || self.snapped
                || self.theme.is_win98()
                || self.is_composited_glass()
            {
                (0, 0)
            } else {
                (
                    self.theme.radius_window_top(),
                    self.theme.radius_window_bottom(),
                )
            };
            if top_r > 0 || bot_r > 0 {
                self.canvas.mask_top_bottom_corners_rect(top_r, bot_r, area);
            }
        }
        if self.backing != 0 {
            if self.pending_backing_sync {
                let new_backing =
                    self.x
                        .create_backing_depth(self.win, self.depth, self.width, self.height)?;
                self.x
                    .put_canvas_depth(new_backing, &self.canvas, full_bounds, self.depth)?;
                self.x.set_background_pixmap(self.win, new_backing)?;
                let old_backing = std::mem::replace(&mut self.backing, new_backing);
                let _ = self.x.free_pixmap(old_backing);
                self.x.clear_area(self.win, full_bounds)?;
                self.pending_backing_sync = false;
                if self.composited {
                    let _ = self.x.send_window_cmd_data(
                        self.win,
                        22,
                        u32::from(self.width),
                        u32::from(self.height),
                        0,
                        0,
                    );
                }
            } else {
                // Update the server-side copy, then let Xorg blit it to the window.
                // The window keeps a valid backing store for later exposes.
                self.x
                    .put_canvas_depth(self.backing, &self.canvas, area, self.depth)?;
                self.x.clear_area(self.win, area)?;
            }
        } else {
            self.x
                .put_canvas_depth(self.win, &self.canvas, area, self.depth)?;
            if self.pending_backing_sync {
                self.pending_backing_sync = false;
                if self.composited {
                    let _ = self.x.send_window_cmd_data(
                        self.win,
                        22,
                        u32::from(self.width),
                        u32::from(self.height),
                        0,
                        0,
                    );
                }
            }
        }
        self.x.flush()?;
        self.drawn = true;
        self.needs_redraw = false;
        self.last_frame = Instant::now();
        Ok(())
    }

    pub fn request_close(&mut self) {
        let _ = self.x.send_delete(self.win);
        self.close = true;
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.x.free_pixmap(self.backing);
        self.x.release_canvas(&self.canvas);
    }
}
