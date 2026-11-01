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
use crate::theme::{Theme, TITLEBAR_HEIGHT};
use crate::ui::{self, CaptionHit};
use crate::x11::{CursorKind, Key, Result, X};
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
}

#[derive(Clone, Copy)]
enum DragState {
    Move {
        orig_x: i16,
        orig_y: i16,
        press_rx: i16,
        press_ry: i16,
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
    pub text: TextRenderer,
    pub config: DesktopConfig,
    pub uxconf: Option<UxConfClient>,
    watch: Option<UxConfWatch>,
    pub theme: Theme,
    pub title: String,
    pub active: bool,
    pub maximized: bool,
    pub needs_redraw: bool,
    pub close: bool,
    drawn: bool,
    saved_geom: Option<(i16, i16, u16, u16)>,
    drag: Option<DragState>,
    caption_hover: CaptionHit,
    current_cursor: CursorKind,
    content_cursor: CursorKind,
    last_title_click: Option<Instant>,
}

impl App {
    pub fn new(title: &str, icon: Icon, width: u16, height: u16) -> Result<Self> {
        let x = X::connect()?;
        let mut uxconf = UxConfClient::connect().ok();
        let config = uxconf
            .as_mut()
            .map(UxConfClient::load_desktop_config)
            .unwrap_or_default();
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
        x.create_window(win, x.root, 0, 0, width, height, false, mask, theme.window)?;
        x.set_title(win, title)?;
        x.set_class(win, "spui", "Spui")?;
        x.set_wm_type(win, x.atoms.net_wm_type_normal)?;
        x.set_pid(win, std::process::id())?;
        x.set_protocols(win)?;
        x.map(win)?;
        x.flush()?;

        let mut text =
            TextRenderer::load().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        text.set_ui_scale(theme.ui_scale());
        let canvas = x.create_canvas(width as usize, height as usize, theme.window);
        // Keep a server-side copy so Xorg can repaint exposed areas itself
        // while another window is dragged across this one.
        let backing = x.create_backing(win, width, height)?;
        x.put_canvas(backing, &canvas, canvas.bounds())?;
        x.set_background_pixmap(win, backing)?;
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
            text,
            config,
            uxconf,
            watch,
            theme,
            title: title.to_string(),
            active: true,
            maximized: false,
            needs_redraw: true,
            close: false,
            drawn: false,
            saved_geom: None,
            drag: None,
            caption_hover: CaptionHit::None,
            current_cursor: CursorKind::Default,
            content_cursor: CursorKind::Default,
            last_title_click: None,
        })
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

    fn sync_config_visuals(&mut self) {
        self.theme = Theme::from_config(&self.config);
        self.text.set_ui_scale(self.theme.ui_scale());
        if self.maximized {
            if let Ok(cookie) = self.x.conn.get_geometry(self.x.root) {
                if let Ok(root_geom) = cookie.reply() {
                    let panel_h = self.config.panel_size.clamp(34, 60) as u16;
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
        self.needs_redraw = true;
    }

    pub fn content(&self) -> Rect {
        Rect::new(
            1,
            TITLEBAR_HEIGHT,
            (self.width as i32 - 2).max(1),
            (self.height as i32 - TITLEBAR_HEIGHT - 1).max(1),
        )
    }

    pub fn set_content_cursor(&mut self, kind: CursorKind) {
        self.content_cursor = kind;
    }

    fn apply_cursor(&mut self, kind: CursorKind) {
        if kind != self.current_cursor {
            self.current_cursor = kind;
            let _ = self.x.set_cursor(self.win, kind);
            let _ = self.x.flush();
        }
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
            if let Some((sx, sy, sw, sh)) = self.saved_geom.take() {
                self.x.move_resize(self.win, sx, sy, sw, sh)?;
            }
        } else {
            let geom = self.x.conn.get_geometry(self.win)?.reply()?;
            let root_geom = self.x.conn.get_geometry(self.x.root)?.reply()?;
            self.saved_geom = Some((geom.x, geom.y, self.width, self.height));
            self.maximized = true;
            let panel_h = self.config.panel_size.clamp(34, 60) as u16;
            let max_y = if self.config.panel_position == PanelPosition::Top {
                panel_h as i16
            } else {
                0
            };
            let max_h = root_geom.height.saturating_sub(panel_h).max(200);
            self.x
                .move_resize(self.win, 0, max_y, root_geom.width, max_h)?;
        }
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
            let (_, ux_ready, ext) = self.x.wait_fds(ux_fd, extra_fd, timeout_ms);
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

        // Coalesce consecutive MotionNotify events on our window.
        let mut coalesced: Vec<Event> = Vec::with_capacity(raw.len());
        for ev in raw {
            if matches!(ev, Event::MotionNotify(_))
                && matches!(coalesced.last(), Some(Event::MotionNotify(_)))
            {
                *coalesced.last_mut().unwrap() = ev;
            } else {
                coalesced.push(ev);
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
                    if self.drawn {
                        let area = Rect::new(
                            i32::from(e.x),
                            i32::from(e.y),
                            i32::from(e.width),
                            i32::from(e.height),
                        );
                        self.x.put_canvas(self.win, &self.canvas, area)?;
                        self.x.flush()?;
                    } else {
                        self.needs_redraw = true;
                    }
                    events.push(UiEvent::Expose);
                }
            }
            Event::ConfigureNotify(e) if e.window == self.win => {
                if e.width != self.width || e.height != self.height {
                    self.width = e.width;
                    self.height = e.height;
                    self.x.release_canvas(&self.canvas);
                    self.canvas = self.x.create_canvas(
                        e.width as usize,
                        e.height as usize,
                        self.theme.window,
                    );
                    // Replace the backing store in lockstep with the canvas so
                    // exposes during the resize also come from the new pixmap.
                    let old_backing = self.backing;
                    self.backing = self.x.create_backing(self.win, e.width, e.height)?;
                    self.x
                        .put_canvas(self.backing, &self.canvas, self.canvas.bounds())?;
                    self.x.set_background_pixmap(self.win, self.backing)?;
                    let _ = self.x.free_pixmap(old_backing);
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
            }
            Event::FocusOut(e) if e.event == self.win => {
                if self.active {
                    self.active = false;
                    self.needs_redraw = true;
                }
            }
            Event::LeaveNotify(e) if e.event == self.win => {
                if self.caption_hover != CaptionHit::None {
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
                }
            }
            Event::KeyPress(e) => {
                let key = Key {
                    code: e.detail,
                    state: e.state.into(),
                    sym: self.x.key_symbol(e.detail, e.state.into()),
                };
                events.push(UiEvent::Key(key));
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

                if e.detail == 1 {
                    let cap = ui::hit_caption(self.width as i32, x, y, self.buttons_left());
                    if cap != CaptionHit::None {
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
                        return Ok(());
                    }
                    if let Some(dir) = self.hit_resize(x, y) {
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
                    if y < TITLEBAR_HEIGHT {
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
                        if !self.maximized {
                            let geom = self.x.conn.get_geometry(self.win)?.reply()?;
                            let min_y = if self.config.panel_position == PanelPosition::Top {
                                self.config.panel_size.clamp(34, 60) as i16
                            } else {
                                0
                            };
                            self.drag = Some(DragState::Move {
                                orig_x: geom.x,
                                orig_y: geom.y.max(min_y),
                                press_rx: e.root_x,
                                press_ry: e.root_y,
                            });
                        }
                        return Ok(());
                    }
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
                if self.drag.take().is_some() {
                    self.apply_cursor(CursorKind::Default);
                }
                events.push(UiEvent::Button {
                    x: e.event_x as i32,
                    y: e.event_y as i32,
                    button: e.detail,
                    down: false,
                    state: e.state.into(),
                });
            }
            Event::MotionNotify(e) if e.event == self.win => {
                let (x, y) = (e.event_x as i32, e.event_y as i32);
                let btn1_held = u16::from(e.state) & (1 << 8) != 0;
                if let Some(drag) = self.drag {
                    if btn1_held {
                        let min_y = if self.config.panel_position == PanelPosition::Top {
                            self.config.panel_size.clamp(34, 60)
                        } else {
                            0
                        };
                        match drag {
                            DragState::Move {
                                orig_x,
                                orig_y,
                                press_rx,
                                press_ry,
                            } => {
                                let nx = orig_x.saturating_add(e.root_x.saturating_sub(press_rx));
                                let ny = orig_y
                                    .saturating_add(e.root_y.saturating_sub(press_ry))
                                    .max(min_y as i16);
                                self.x.move_window(self.win, nx, ny)?;
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
                    }
                }

                let cap = ui::hit_caption(self.width as i32, x, y, self.buttons_left());
                if cap != self.caption_hover {
                    self.caption_hover = cap;
                    self.needs_redraw = true;
                }

                let desired_cursor = if cap != CaptionHit::None {
                    CursorKind::Default
                } else if let Some(dir) = self.hit_resize(x, y) {
                    dir.cursor()
                } else if y < TITLEBAR_HEIGHT {
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
        let dirty = self.canvas.take_dirty();
        let area = if dirty.is_empty() {
            Rect::new(0, 0, self.width as i32, self.height as i32)
        } else {
            dirty
        };
        if self.backing != 0 {
            // Update the server-side copy, then let Xorg blit it to the window.
            // The window keeps a valid backing store for later exposes.
            self.x.put_canvas(self.backing, &self.canvas, area)?;
            self.x.clear_area(self.win, area)?;
        } else {
            self.x.put_canvas(self.win, &self.canvas, area)?;
        }
        self.x.flush()?;
        self.drawn = true;
        self.needs_redraw = false;
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
