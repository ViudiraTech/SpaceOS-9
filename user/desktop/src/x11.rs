/*
 *
 *       user/desktop/src/x11.rs
 *       X11 connection, MIT-SHM upload, XRender Breeze cursors, XShape and input
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Synchronous X11 client layer on top of `x11rb`. Supports zero-copy `MIT-SHM`
//! canvas buffers, 32-bit ARGB KDE Breeze XRender cursors, `SHAPE` rounded-rect
//! window masks, X11 pointer/keyboard/screensaver controls and `poll(2)` multiplexing.

use crate::draw::{Canvas, Rect};
use std::fs;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Command, Stdio};
use x11rb::connection::Connection;
use x11rb::protocol::composite::{self, ConnectionExt as CompositeExt};
use x11rb::protocol::damage::{self, ConnectionExt as DamageExt};
use x11rb::protocol::render::{ConnectionExt as RenderExt, PictType, Pictformat};
use x11rb::protocol::shape::{self, ConnectionExt as ShapeExt};
use x11rb::protocol::shm::ConnectionExt as ShmExt;
use x11rb::protocol::xfixes::ConnectionExt as XFixesExt;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, AutoRepeatMode, Blanking, ChangeKeyboardControlAux, ChangeWindowAttributesAux,
    ClientMessageEvent, Colormap, ColormapAlloc, ConfigureWindowAux, ConnectionExt, CreateGCAux,
    CreateWindowAux, Cursor, EventMask, Exposures, Gcontext, GrabMode, ImageFormat, InputFocus,
    ModMask, PropMode, Rectangle, StackMode, Visualid, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperExt;
use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::COPY_FROM_PARENT;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CursorKind {
    Default,
    Pointer,
    Text,
    ResizeH,
    ResizeV,
    ResizeDiagF,
    ResizeDiagB,
    Move,
}

impl CursorKind {
    fn index(self) -> usize {
        match self {
            Self::Default => 0,
            Self::Pointer => 1,
            Self::Text => 2,
            Self::ResizeH => 3,
            Self::ResizeV => 4,
            Self::ResizeDiagF => 5,
            Self::ResizeDiagB => 6,
            Self::Move => 7,
        }
    }

    fn xcursor_names(self) -> &'static [&'static str] {
        match self {
            Self::Default => &["left_ptr", "default"],
            Self::Pointer => &["pointing_hand", "pointer", "hand2"],
            Self::Text => &["ibeam", "xterm", "text"],
            Self::ResizeH => &["size_hor", "sb_h_double_arrow"],
            Self::ResizeV => &["size_ver", "sb_v_double_arrow"],
            Self::ResizeDiagF => &["size_fdiag", "top_left_corner"],
            Self::ResizeDiagB => &["size_bdiag", "top_right_corner"],
            Self::Move => &["fleur", "move"],
        }
    }
}

const ALL_CURSORS: [CursorKind; 8] = [
    CursorKind::Default,
    CursorKind::Pointer,
    CursorKind::Text,
    CursorKind::ResizeH,
    CursorKind::ResizeV,
    CursorKind::ResizeDiagF,
    CursorKind::ResizeDiagB,
    CursorKind::Move,
];

pub struct Atoms {
    pub wm_protocols: Atom,
    pub wm_delete: Atom,
    pub net_wm_name: Atom,
    pub utf8: Atom,
    pub net_wm_type: Atom,
    pub net_wm_type_normal: Atom,
    pub net_wm_type_dock: Atom,
    pub net_wm_type_dialog: Atom,
    pub net_wm_type_desktop: Atom,
    pub net_wm_state: Atom,
    pub net_wm_state_hidden: Atom,
    pub net_client_list: Atom,
    pub net_active_window: Atom,
    pub net_supporting_wm_check: Atom,
    pub net_supported: Atom,
    pub net_wm_pid: Atom,
    pub wm_class: Atom,
    pub spui_window_cmd: Atom,
    pub spui_icon: Atom,
    pub spui_glass_extents: Atom,
    pub spui_compositor: Atom,
    pub spime_status: Atom,
    pub spime_notify: Atom,
}

/// Reusable MIT-SHM readback buffer for incremental `XDamage` window captures.
pub struct ShmReadBuf {
    pub seg: u32,
    ptr: *mut u32,
    pub cap_pixels: usize,
}

// SAFETY: `ptr` points to an exclusive SysV SHM mapping owned by this `ShmReadBuf`.
unsafe impl Send for ShmReadBuf {}

impl Drop for ShmReadBuf {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                libc::shmdt(self.ptr as *const libc::c_void);
            }
        }
    }
}

pub struct X {
    pub conn: RustConnection,
    pub screen: usize,
    pub root: Window,
    pub width: u16,
    pub height: u16,
    pub depth: u8,
    pub gc: Gcontext,
    pub argb_visual: Option<Visualid>,
    pub argb_colormap: Colormap,
    pub argb_gc: Gcontext,
    pub atoms: Atoms,
    pub has_shm: bool,
    pub has_shape: bool,
    pub has_composite: bool,
    pub has_damage: bool,
    pub has_xfixes: bool,
    cursors: [Cursor; 8],
    min_keycode: u8,
    keysyms_per: u8,
    keysyms: Vec<u32>,
    max_request: usize,
}

impl X {
    pub fn connect() -> Result<Self> {
        let mut attempts = 0;
        let (conn, screen) = loop {
            match x11rb::connect(None) {
                Ok(connection) => break connection,
                Err(error) => {
                    attempts += 1;
                    if attempts >= 100 {
                        return Err(error.into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        };
        let screen_info = conn.setup().roots.get(screen).ok_or("no X screen")?.clone();
        let root = screen_info.root;
        let width = screen_info.width_in_pixels;
        let height = screen_info.height_in_pixels;
        let depth = screen_info.root_depth;
        let min_keycode = conn.setup().min_keycode;
        let max_keycode = conn.setup().max_keycode;
        let mapping = conn
            .get_keyboard_mapping(min_keycode, max_keycode - min_keycode + 1)?
            .reply()?;
        let atoms = Atoms::intern(&conn)?;
        let gc = conn.generate_id()?;
        conn.create_gc(gc, root, &Default::default())?;
        let max_request = usize::from(conn.setup().maximum_request_length) * 4;
        let has_shm = conn
            .shm_query_version()
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        let has_shape = conn
            .shape_query_version()
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        let has_composite = conn
            .composite_query_version(0, 4)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        let has_damage = conn
            .damage_query_version(1, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        let has_xfixes = conn
            .xfixes_query_version(5, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        let (argb_visual, argb_colormap, argb_gc) = init_argb_visual(&conn, screen, &screen_info);
        let cursors = load_cursors(&conn, root);

        Ok(Self {
            conn,
            screen,
            root,
            width,
            height,
            depth,
            gc,
            argb_visual,
            argb_colormap,
            argb_gc,
            atoms,
            has_shm,
            has_shape,
            has_composite,
            has_damage,
            has_xfixes,
            cursors,
            min_keycode,
            keysyms_per: mapping.keysyms_per_keycode,
            keysyms: mapping.keysyms,
            max_request,
        })
    }

    pub fn generate_id(&self) -> Result<u32> {
        Ok(self.conn.generate_id()?)
    }

    pub fn set_cursor(&self, window: Window, kind: CursorKind) -> Result<()> {
        let cid = self.cursors[kind.index()];
        if cid != 0 {
            self.conn
                .change_window_attributes(window, &ChangeWindowAttributesAux::new().cursor(cid))?;
        }
        Ok(())
    }

    /// Allocate an `MIT-SHM` backed `Canvas`, falling back to heap if unavailable.
    pub fn create_canvas(&self, width: usize, height: usize, background: u32) -> Canvas {
        let w = width.max(1);
        let h = height.max(1);
        if self.has_shm {
            let byte_len = w * h * 4;
            unsafe {
                let shmid = libc::shmget(libc::IPC_PRIVATE, byte_len, libc::IPC_CREAT | 0o600);
                if shmid >= 0 {
                    let raw = libc::shmat(shmid, std::ptr::null(), 0);
                    if raw != (-1isize as *mut libc::c_void) && !raw.is_null() {
                        if let Ok(seg) = self.conn.generate_id() {
                            let attached = self
                                .conn
                                .shm_attach(seg, shmid as u32, false)
                                .ok()
                                .and_then(|cookie| cookie.check().ok())
                                .is_some();
                            let _ = libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
                            if attached {
                                return Canvas::from_shm(w, h, raw as *mut u32, seg, background);
                            }
                        }
                        libc::shmdt(raw);
                    }
                    let _ = libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
                }
            }
        }
        Canvas::new(w, h, background)
    }

    /// Detach an existing canvas's X11 SHM segment before replacing it on resize.
    pub fn release_canvas(&self, canvas: &Canvas) {
        if let Some(seg) = canvas.shm_seg() {
            let _ = self.conn.shm_detach(seg);
        }
    }

    /// Apply a 1-bit rounded-rectangle XShape bounding & clip mask to `window`.
    pub fn set_rounded_shape(
        &self,
        window: Window,
        width: u16,
        height: u16,
        radius: i32,
    ) -> Result<()> {
        if !self.has_shape || width == 0 || height == 0 {
            return Ok(());
        }
        if radius <= 0 {
            return self.clear_shape(window);
        }
        let r = radius
            .max(0)
            .min(i32::from(width) / 2)
            .min(i32::from(height) / 2);
        let pixmap = self.conn.generate_id()?;
        let gc0 = self.conn.generate_id()?;
        let gc1 = self.conn.generate_id()?;
        self.conn.create_pixmap(1, pixmap, window, width, height)?;
        self.conn
            .create_gc(gc0, pixmap, &CreateGCAux::new().foreground(0))?;
        self.conn
            .create_gc(gc1, pixmap, &CreateGCAux::new().foreground(1))?;
        self.conn.poly_fill_rectangle(
            pixmap,
            gc0,
            &[Rectangle {
                x: 0,
                y: 0,
                width,
                height,
            }],
        )?;

        let mut rects = Vec::with_capacity((r as usize) * 2 + 1);
        let mid_h = i32::from(height) - r * 2;
        if mid_h > 0 {
            rects.push(Rectangle {
                x: 0,
                y: r as i16,
                width,
                height: mid_h as u16,
            });
        }
        let rf = r as f32;
        for row in 0..r {
            let dy = rf - (row as f32 + 0.5);
            let dx = (rf * rf - dy * dy).max(0.0).sqrt();
            let inset = (rf - dx).round().clamp(0.0, rf) as i16;
            let span_w = (i32::from(width) - i32::from(inset) * 2).max(1) as u16;
            rects.push(Rectangle {
                x: inset,
                y: row as i16,
                width: span_w,
                height: 1,
            });
            rects.push(Rectangle {
                x: inset,
                y: (i32::from(height) - 1 - row) as i16,
                width: span_w,
                height: 1,
            });
        }
        self.conn.poly_fill_rectangle(pixmap, gc1, &rects)?;
        self.conn
            .shape_mask(shape::SO::SET, shape::SK::BOUNDING, window, 0, 0, pixmap)?;
        self.conn
            .shape_mask(shape::SO::SET, shape::SK::CLIP, window, 0, 0, pixmap)?;
        self.conn.free_gc(gc0)?;
        self.conn.free_gc(gc1)?;
        self.conn.free_pixmap(pixmap)?;
        Ok(())
    }

    /// Remove any custom XShape bounding/clip mask so `window` covers its full rectangle.
    pub fn clear_shape(&self, window: Window) -> Result<()> {
        if !self.has_shape {
            return Ok(());
        }
        self.conn
            .shape_mask(shape::SO::SET, shape::SK::BOUNDING, window, 0, 0, 0u32)?;
        self.conn
            .shape_mask(shape::SO::SET, shape::SK::CLIP, window, 0, 0, 0u32)?;
        Ok(())
    }

    /// Create an InputOutput window preferring a 32-bit ARGB TrueColor visual when
    /// available so compositor glass/corner transparency works seamlessly, returning
    /// `(window_id, is_argb32)`.
    #[allow(clippy::too_many_arguments)]
    pub fn create_argb_window(
        &self,
        id: Window,
        parent: Window,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        override_redirect: bool,
        mask: EventMask,
        fallback_bg: u32,
    ) -> Result<(Window, bool)> {
        if let Some(visual) = self.argb_visual {
            let cid = self.cursors[CursorKind::Default.index()];
            let mut aux = CreateWindowAux::new()
                .background_pixel(0)
                .border_pixel(0)
                .colormap(self.argb_colormap)
                .event_mask(mask)
                .override_redirect(u32::from(override_redirect));
            if cid != 0 {
                aux = aux.cursor(cid);
            }
            if self
                .conn
                .create_window(
                    32,
                    id,
                    parent,
                    x,
                    y,
                    width,
                    height,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    visual,
                    &aux,
                )
                .is_ok()
            {
                return Ok((id, true));
            }
        }
        self.create_window(
            id,
            parent,
            x,
            y,
            width,
            height,
            override_redirect,
            mask,
            fallback_bg,
        )?;
        Ok((id, false))
    }

    pub fn create_window(
        &self,
        id: Window,
        parent: Window,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        override_redirect: bool,
        mask: EventMask,
        background: u32,
    ) -> Result<()> {
        let cid = self.cursors[CursorKind::Default.index()];
        let mut aux = CreateWindowAux::new()
            .background_pixel(background)
            .event_mask(mask)
            .override_redirect(u32::from(override_redirect))
            .border_pixel(0);
        if cid != 0 {
            aux = aux.cursor(cid);
        }
        self.conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            id,
            parent,
            x,
            y,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &aux,
        )?;
        Ok(())
    }

    pub fn map(&self, window: Window) -> Result<()> {
        self.conn.map_window(window)?;
        Ok(())
    }

    pub fn unmap(&self, window: Window) -> Result<()> {
        self.conn.unmap_window(window)?;
        Ok(())
    }

    pub fn destroy(&self, window: Window) -> Result<()> {
        self.conn.destroy_window(window)?;
        Ok(())
    }

    pub fn raise(&self, window: Window) -> Result<()> {
        self.conn.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;
        Ok(())
    }

    pub fn lower(&self, window: Window) -> Result<()> {
        self.conn.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
        )?;
        Ok(())
    }

    pub fn move_resize(&self, window: Window, x: i16, y: i16, w: u16, h: u16) -> Result<()> {
        self.conn.configure_window(
            window,
            &ConfigureWindowAux::new()
                .x(x as i32)
                .y(y as i32)
                .width(w as u32)
                .height(h as u32),
        )?;
        Ok(())
    }

    /// Move a window without touching its size, so dragging does not send a
    /// redundant resize request on every pointer sample.
    pub fn move_window(&self, window: Window, x: i16, y: i16) -> Result<()> {
        self.conn
            .configure_window(window, &ConfigureWindowAux::new().x(x as i32).y(y as i32))?;
        Ok(())
    }

    /// Create an off-screen pixmap matching `window`'s size and root depth.
    pub fn create_backing(&self, window: Window, width: u16, height: u16) -> Result<u32> {
        self.create_backing_depth(window, self.depth, width, height)
    }

    /// Create an off-screen pixmap with an explicit bit `depth`.
    pub fn create_backing_depth(
        &self,
        window: Window,
        depth: u8,
        width: u16,
        height: u16,
    ) -> Result<u32> {
        let pixmap = self.conn.generate_id()?;
        self.conn
            .create_pixmap(depth, pixmap, window, width.max(1), height.max(1))?;
        Ok(pixmap)
    }

    /// Make `pixmap` the window's background. Xorg then repaints exposed
    /// regions from it on its own, without an Expose round-trip to the client.
    pub fn set_background_pixmap(&self, window: Window, pixmap: u32) -> Result<()> {
        self.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
        )?;
        Ok(())
    }

    /// Repaint `area` from the window's background pixmap (server-side copy).
    pub fn clear_area(&self, window: Window, area: Rect) -> Result<()> {
        if area.is_empty() {
            return Ok(());
        }
        self.conn.clear_area(
            false,
            window,
            area.x as i16,
            area.y as i16,
            area.w as u16,
            area.h as u16,
        )?;
        Ok(())
    }

    pub fn free_pixmap(&self, pixmap: u32) -> Result<()> {
        if pixmap != 0 {
            self.conn.free_pixmap(pixmap)?;
        }
        Ok(())
    }

    pub fn focus(&self, window: Window) -> Result<()> {
        self.conn
            .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)?;
        Ok(())
    }

    /// Acquire an async pointer grab scoped to `window` so popup menus/sliders
    /// reliably catch outside dismissal clicks and off-window drag releases.
    pub fn grab_pointer(&self, window: Window) -> bool {
        self.grab_pointer_with_cursor(window, 0)
    }

    /// Acquire an async pointer grab scoped to `window` displaying `cursor`.
    pub fn grab_pointer_cursor(&self, window: Window, cursor: CursorKind) -> bool {
        let cid = self.cursors[cursor.index()];
        self.grab_pointer_with_cursor(window, cid)
    }

    fn grab_pointer_with_cursor(&self, window: Window, cursor: Cursor) -> bool {
        let mask = EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::ENTER_WINDOW
            | EventMask::LEAVE_WINDOW;
        let Ok(cookie) = self.conn.grab_pointer(
            false,
            window,
            mask,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            cursor,
            x11rb::CURRENT_TIME,
        ) else {
            return false;
        };
        let _ = self.flush();
        cookie
            .reply()
            .is_ok_and(|r| r.status == x11rb::protocol::xproto::GrabStatus::SUCCESS)
    }

    /// Query current root pointer coordinates and button/modifier mask.
    pub fn query_pointer_root(&self) -> Option<(i16, i16, u16)> {
        let reply = self.conn.query_pointer(self.root).ok()?.reply().ok()?;
        Some((reply.root_x, reply.root_y, u16::from(reply.mask)))
    }

    /// Release any active pointer grab held by this X11 connection.
    pub fn ungrab_pointer(&self) {
        let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
        let _ = self.flush();
    }

    /// Acquire an async keyboard grab scoped to `window` (used by Alt+Tab and Flip 3D).
    pub fn grab_keyboard(&self, window: Window) -> bool {
        let Ok(cookie) = self.conn.grab_keyboard(
            false,
            window,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        ) else {
            return false;
        };
        let _ = self.flush();
        cookie
            .reply()
            .is_ok_and(|r| r.status == x11rb::protocol::xproto::GrabStatus::SUCCESS)
    }

    /// Release any active keyboard grab held by this X11 connection.
    pub fn ungrab_keyboard(&self) {
        let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        let _ = self.flush();
    }

    pub fn set_active_window(&self, window: Window) -> Result<()> {
        self.conn.change_property(
            PropMode::REPLACE,
            self.root,
            self.atoms.net_active_window,
            AtomEnum::WINDOW,
            32,
            1,
            &window.to_ne_bytes(),
        )?;
        Ok(())
    }

    pub fn set_title(&self, window: Window, title: &str) -> Result<()> {
        self.conn.change_property(
            PropMode::REPLACE,
            window,
            self.atoms.net_wm_name,
            self.atoms.utf8,
            8,
            title.len() as u32,
            title.as_bytes(),
        )?;
        self.conn.change_property(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            8,
            title.len() as u32,
            title.as_bytes(),
        )?;
        Ok(())
    }

    /// Attach an icon identifier (`_SPUI_ICON`) so the panel always displays the exact app icon.
    pub fn set_icon_name(&self, window: Window, name: &str) -> Result<()> {
        self.conn.change_property(
            PropMode::REPLACE,
            window,
            self.atoms.spui_icon,
            self.atoms.utf8,
            8,
            name.len() as u32,
            name.as_bytes(),
        )?;
        Ok(())
    }

    /// Publish DWM extended-glass margins `[top, bottom, left, right, corner_r]` in
    /// `_SPUI_GLASS_EXTENTS` so the compositor blurs behind transparent client chrome.
    pub fn set_glass_extents(
        &self,
        window: Window,
        top: i32,
        bottom: i32,
        left: i32,
        right: i32,
        corner_r: i32,
    ) -> Result<()> {
        let vals = [top, bottom, left, right, corner_r];
        let mut bytes = [0u8; 20];
        for (i, v) in vals.into_iter().enumerate() {
            bytes[i * 4..(i + 1) * 4].copy_from_slice(&(v as u32).to_ne_bytes());
        }
        self.conn.change_property(
            PropMode::REPLACE,
            window,
            self.atoms.spui_glass_extents,
            AtomEnum::CARDINAL,
            32,
            5,
            &bytes,
        )?;
        Ok(())
    }

    /// Read `_SPUI_GLASS_EXTENTS` (`[top, bottom, left, right, corner_r]`) if set.
    pub fn get_glass_extents(&self, window: Window) -> Option<[i32; 5]> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms.spui_glass_extents,
                AtomEnum::CARDINAL,
                0,
                5,
            )
            .ok()?
            .reply()
            .ok()?;
        if reply.value.len() < 20 {
            return None;
        }
        let mut out = [0i32; 5];
        for (i, dst) in out.iter_mut().enumerate() {
            let chunk: [u8; 4] = reply.value[i * 4..(i + 1) * 4].try_into().ok()?;
            *dst = u32::from_ne_bytes(chunk) as i32;
        }
        Some(out)
    }

    /// Enable manual subwindow redirection and obtain the pass-through Composite Overlay Window.
    pub fn enable_compositor(&self) -> Result<Option<Window>> {
        if !self.has_composite || !self.has_damage || !self.has_xfixes {
            return Ok(None);
        }
        self.conn
            .composite_redirect_subwindows(self.root, composite::Redirect::MANUAL)?
            .check()?;
        let overlay = self
            .conn
            .composite_get_overlay_window(self.root)?
            .reply()?
            .overlay_win;
        let reg = self.conn.generate_id()?;
        self.conn.xfixes_create_region(reg, &[])?;
        self.conn
            .xfixes_set_window_shape_region(overlay, shape::SK::INPUT, 0, 0, reg)?;
        self.conn.xfixes_destroy_region(reg)?;
        self.set_compositor_active(true);
        self.conn.flush()?;
        Ok(Some(overlay))
    }

    /// Release Composite Overlay Window on compositor shutdown.
    pub fn disable_compositor(&self, overlay: Option<Window>) {
        if overlay.is_some() && self.has_composite {
            self.set_compositor_active(false);
            let _ = self.conn.composite_release_overlay_window(self.root);
            let _ = self
                .conn
                .composite_unredirect_subwindows(self.root, composite::Redirect::MANUAL);
            let _ = self.flush();
        }
    }

    /// Advertise compositor availability on the root window (`_SPUI_COMPOSITOR`).
    pub fn set_compositor_active(&self, active: bool) {
        if active {
            let val = 1u32.to_ne_bytes();
            let _ = self.conn.change_property(
                PropMode::REPLACE,
                self.root,
                self.atoms.spui_compositor,
                AtomEnum::CARDINAL,
                32,
                1,
                &val,
            );
        } else {
            let _ = self
                .conn
                .delete_property(self.root, self.atoms.spui_compositor);
        }
    }

    /// Query whether `spui shell`'s software compositor currently owns redirection.
    pub fn is_compositor_active(&self) -> bool {
        self.conn
            .get_property(
                false,
                self.root,
                self.atoms.spui_compositor,
                AtomEnum::CARDINAL,
                0,
                1,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.value.len() >= 4 && u32::from_ne_bytes([r.value[0], r.value[1], r.value[2], r.value[3]]) != 0)
    }

    /// Read `(chinese_mode, ime_enabled)` published by `spimed` on `_SPIME_STATUS`.
    pub fn get_spime_status(&self) -> Option<(bool, bool)> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms.spime_status,
                AtomEnum::CARDINAL,
                0,
                2,
            )
            .ok()?
            .reply()
            .ok()?;
        let mut iter = reply.value32()?;
        let ch = iter.next()? != 0;
        let en = iter.next().unwrap_or(1) != 0;
        Some((ch, en))
    }

    /// Publish `(chinese_mode, ime_enabled)` on `root` so the Shell taskbar syncs instantly.
    pub fn set_spime_status(&self, chinese_mode: bool, enabled: bool) {
        let vals = [u32::from(chinese_mode), u32::from(enabled)];
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&vals[0].to_ne_bytes());
        bytes[4..8].copy_from_slice(&vals[1].to_ne_bytes());
        let _ = self.conn.change_property(
            PropMode::REPLACE,
            self.root,
            self.atoms.spime_status,
            AtomEnum::CARDINAL,
            32,
            2,
            &bytes,
        );
    }

    /// Wake an X11 SPUI window when the candidate bar completes an asynchronous click commit.
    pub fn send_spime_notify(&self, target_window: Window) {
        let ev = ClientMessageEvent {
            response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: target_window,
            type_: self.atoms.spime_notify,
            data: x11rb::protocol::xproto::ClientMessageData::from([1u32, 0, 0, 0, 0]),
        };
        let _ = self
            .conn
            .send_event(false, target_window, EventMask::NO_EVENT, ev);
    }

    pub fn name_window_pixmap(&self, window: Window) -> Result<u32> {
        let pixmap = self.conn.generate_id()?;
        self.conn
            .composite_name_window_pixmap(window, pixmap)?
            .check()?;
        Ok(pixmap)
    }

    pub fn create_damage(&self, window: Window) -> Result<u32> {
        let dmg = self.conn.generate_id()?;
        self.conn
            .damage_create(dmg, window, damage::ReportLevel::BOUNDING_BOX)?
            .check()?;
        Ok(dmg)
    }

    pub fn destroy_damage(&self, dmg: u32) {
        if dmg != 0 && self.has_damage {
            let _ = self.conn.damage_destroy(dmg);
        }
    }

    pub fn damage_subtract(&self, dmg: u32) {
        if dmg != 0 && self.has_damage {
            let _ = self.conn.damage_subtract(dmg, 0u32, 0u32);
        }
    }

    pub fn create_shm_read_buffer(&self, width: usize, height: usize) -> Option<ShmReadBuf> {
        if !self.has_shm || width == 0 || height == 0 {
            return None;
        }
        let cap_pixels = width * height;
        let byte_len = cap_pixels * 4;
        unsafe {
            let shmid = libc::shmget(libc::IPC_PRIVATE, byte_len, libc::IPC_CREAT | 0o600);
            if shmid < 0 {
                return None;
            }
            let raw = libc::shmat(shmid, std::ptr::null(), 0);
            if raw == (-1isize as *mut libc::c_void) || raw.is_null() {
                let _ = libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
                return None;
            }
            let attached = self
                .conn
                .generate_id()
                .ok()
                .and_then(|seg| {
                    let ok = self
                        .conn
                        .shm_attach(seg, shmid as u32, false)
                        .ok()
                        .and_then(|c| c.check().ok())
                        .is_some();
                    ok.then_some(seg)
                });
            let _ = libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
            if let Some(seg) = attached {
                return Some(ShmReadBuf {
                    seg,
                    ptr: raw as *mut u32,
                    cap_pixels,
                });
            }
            libc::shmdt(raw);
        }
        None
    }

    pub fn release_shm_read_buffer(&self, buf: &ShmReadBuf) {
        let _ = self.conn.shm_detach(buf.seg);
    }

    /// Read a sub-rectangle from an off-screen `pixmap` into `dst` (normalized to
    /// premultiplied `0xAARRGGBB`, forcing `alpha = 0xFF` for depth-24 sources).
    #[allow(clippy::too_many_arguments)]
    pub fn read_pixmap_rect(
        &self,
        pixmap: u32,
        shm: Option<&ShmReadBuf>,
        x: i16,
        y: i16,
        w: u16,
        h: u16,
        depth: u8,
        dst: &mut [u32],
        dst_stride: usize,
    ) -> bool {
        if pixmap == 0 || w == 0 || h == 0 {
            return false;
        }
        let wu = w as usize;
        let hu = h as usize;
        let xu = x.max(0) as usize;
        let yu = y.max(0) as usize;
        if yu + hu > dst.len() / dst_stride.max(1) || xu + wu > dst_stride {
            return false;
        }
        if let Some(s) = shm {
            if wu * hu <= s.cap_pixels {
                if let Ok(cookie) = self.conn.shm_get_image(
                    pixmap,
                    x,
                    y,
                    w,
                    h,
                    !0u32,
                    ImageFormat::Z_PIXMAP.into(),
                    s.seg,
                    0,
                ) {
                    if cookie.reply().is_ok() {
                        let src = unsafe { std::slice::from_raw_parts(s.ptr, wu * hu) };
                        for row in 0..hu {
                            let d_row = &mut dst[(yu + row) * dst_stride + xu..(yu + row) * dst_stride + xu + wu];
                            let s_row = &src[row * wu..(row + 1) * wu];
                            if depth == 32 {
                                d_row.copy_from_slice(s_row);
                            } else {
                                for (d, &p) in d_row.iter_mut().zip(s_row.iter()) {
                                    *d = 0xff00_0000 | p;
                                }
                            }
                        }
                        return true;
                    }
                }
            }
        }
        let Ok(cookie) =
            self.conn
                .get_image(ImageFormat::Z_PIXMAP, pixmap, x, y, w, h, !0u32)
        else {
            return false;
        };
        let Ok(reply) = cookie.reply() else {
            return false;
        };
        if reply.data.len() < wu * hu * 4 {
            return false;
        }
        for row in 0..hu {
            let d_row = &mut dst[(yu + row) * dst_stride + xu..(yu + row) * dst_stride + xu + wu];
            let s_off = row * wu * 4;
            for col in 0..wu {
                let p_off = s_off + col * 4;
                let px = u32::from_ne_bytes([
                    reply.data[p_off],
                    reply.data[p_off + 1],
                    reply.data[p_off + 2],
                    reply.data[p_off + 3],
                ]);
                d_row[col] = if depth == 32 {
                    px
                } else {
                    0xff00_0000 | (px & 0x00ff_ffff)
                };
            }
        }
        true
    }

    pub fn set_class(&self, window: Window, instance: &str, class: &str) -> Result<()> {
        let mut data = Vec::new();
        data.extend_from_slice(instance.as_bytes());
        data.push(0);
        data.extend_from_slice(class.as_bytes());
        data.push(0);
        self.conn.change_property(
            PropMode::REPLACE,
            window,
            self.atoms.wm_class,
            AtomEnum::STRING,
            8,
            data.len() as u32,
            &data,
        )?;
        Ok(())
    }

    pub fn set_wm_type(&self, window: Window, kind: Atom) -> Result<()> {
        self.conn.change_property(
            PropMode::REPLACE,
            window,
            self.atoms.net_wm_type,
            AtomEnum::ATOM,
            32,
            1,
            &kind.to_ne_bytes(),
        )?;
        Ok(())
    }

    pub fn set_pid(&self, window: Window, pid: u32) -> Result<()> {
        self.conn.change_property(
            PropMode::REPLACE,
            window,
            self.atoms.net_wm_pid,
            AtomEnum::CARDINAL,
            32,
            1,
            &pid.to_ne_bytes(),
        )?;
        Ok(())
    }

    pub fn set_protocols(&self, window: Window) -> Result<()> {
        self.conn.change_property(
            PropMode::REPLACE,
            window,
            self.atoms.wm_protocols,
            AtomEnum::ATOM,
            32,
            1,
            &self.atoms.wm_delete.to_ne_bytes(),
        )?;
        Ok(())
    }

    pub fn send_delete(&self, window: Window) -> Result<()> {
        let event = ClientMessageEvent::new(
            32,
            window,
            self.atoms.wm_protocols,
            [self.atoms.wm_delete, x11rb::CURRENT_TIME, 0, 0, 0],
        );
        self.conn
            .send_event(false, window, EventMask::NO_EVENT, event)?;
        Ok(())
    }

    /// Send a window-management command (`cmd`: 1 = minimize, 2 = toggle maximize) to the root WM.
    pub fn send_window_cmd(&self, window: Window, cmd: u32) -> Result<()> {
        self.send_window_cmd_data(window, cmd, 0, 0, 0, 0)
    }

    /// Send a window-management notification (`spui_window_cmd`) directly to a managed `window`.
    pub fn send_window_notify_data(
        &self,
        window: Window,
        cmd: u32,
        d1: u32,
        d2: u32,
        d3: u32,
        d4: u32,
    ) -> Result<()> {
        let event = ClientMessageEvent::new(
            32,
            window,
            self.atoms.spui_window_cmd,
            [cmd, d1, d2, d3, d4],
        );
        self.conn
            .send_event(false, window, EventMask::NO_EVENT, event)?;
        Ok(())
    }

    /// Send a window-management command with payload arguments to the root WM.
    pub fn send_window_cmd_data(
        &self,
        window: Window,
        cmd: u32,
        d1: u32,
        d2: u32,
        d3: u32,
        d4: u32,
    ) -> Result<()> {
        let event = ClientMessageEvent::new(
            32,
            window,
            self.atoms.spui_window_cmd,
            [cmd, d1, d2, d3, d4],
        );
        self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )?;
        Ok(())
    }

    /// Apply left-handed (`[3, 2, 1, ...]`) or standard (`[1, 2, 3, ...]`) X11 pointer button order.
    pub fn apply_pointer_mapping(&self, left_handed: bool) {
        let Ok(cookie) = self.conn.get_pointer_mapping() else {
            return;
        };
        let Ok(reply) = cookie.reply() else {
            return;
        };
        let mut map = reply.map;
        if map.len() >= 3 {
            for (idx, slot) in map.iter_mut().enumerate() {
                *slot = (idx + 1) as u8;
            }
            if left_handed {
                map[0] = 3;
                map[1] = 2;
                map[2] = 1;
            }
            if let Ok(set_cookie) = self.conn.set_pointer_mapping(&map) {
                let _ = set_cookie.reply();
            }
        }
    }

    /// Apply X11 pointer acceleration profile (`speed_level` in `1..=5`).
    pub fn apply_pointer_speed(&self, speed_level: i32) {
        let (num, denom, threshold) = match speed_level.clamp(1, 5) {
            1 => (1i16, 2i16, 4i16),
            2 => (3, 4, 4),
            3 => (1, 1, 4),
            4 => (3, 2, 2),
            _ => (2, 1, 1),
        };
        let _ = self
            .conn
            .change_pointer_control(num, denom, threshold, true, true);
        let _ = self.flush();
    }

    /// Configure X11 idle screen blanking timeout (`0` disables screen blanking).
    pub fn apply_screensaver_timeout(&self, seconds: i32) {
        let timeout = if seconds <= 0 {
            0i16
        } else {
            seconds.clamp(60, 7200) as i16
        };
        let _ = self
            .conn
            .set_screen_saver(timeout, 0, Blanking::PREFERRED, Exposures::ALLOWED);
        let _ = self.flush();
    }

    /// Configure X11 keyboard auto-repeat toggle and XKB repeat delay/rate via `xset`.
    pub fn apply_keyboard_repeat(&self, enabled: bool, delay_ms: i32, rate_hz: i32) {
        let mode = if enabled {
            AutoRepeatMode::ON
        } else {
            AutoRepeatMode::OFF
        };
        let _ = self
            .conn
            .change_keyboard_control(&ChangeKeyboardControlAux::new().auto_repeat_mode(mode));
        let _ = self.flush();

        if enabled && Path::new("/usr/bin/xset").exists() {
            let delay = delay_ms.clamp(150, 1000).to_string();
            let rate = rate_hz.clamp(10, 60).to_string();
            let _ = Command::new("/usr/bin/xset")
                .args(["r", "rate", &delay, &rate])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    /// Grab all keycodes mapped to `keysym` on the root window across Lock/NumLock variants.
    pub fn grab_global_key(&self, keysym: u32, modifiers: ModMask) {
        let per = self.keysyms_per.max(1) as usize;
        if per == 0 {
            return;
        }
        let mut codes = Vec::new();
        for (idx, chunk) in self.keysyms.chunks(per).enumerate() {
            if chunk.contains(&keysym) {
                if let Some(code) = self.min_keycode.checked_add(idx as u8) {
                    codes.push(code);
                }
            }
        }
        let extra = [
            ModMask::from(0u16),
            ModMask::LOCK,
            ModMask::M2,
            ModMask::LOCK | ModMask::M2,
        ];
        for code in codes {
            for lock_bits in extra {
                let _ = self.conn.grab_key(
                    true,
                    self.root,
                    modifiers | lock_bits,
                    code,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                );
            }
        }
    }

    pub fn get_active_window(&self) -> Option<Window> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?;
        if reply.value.len() >= 4 {
            let id = u32::from_ne_bytes([
                reply.value[0],
                reply.value[1],
                reply.value[2],
                reply.value[3],
            ]);
            if id != 0 {
                return Some(id);
            }
        }
        None
    }

    fn get_string_property(
        &self,
        window: Window,
        property: Atom,
        kind: Atom,
    ) -> Result<Option<String>> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, 1024)?
            .reply()?;
        if !reply.value.is_empty() {
            return Ok(Some(String::from_utf8_lossy(&reply.value).into_owned()));
        }
        Ok(None)
    }

    pub fn get_title(&self, window: Window) -> Result<String> {
        for (property, kind) in [
            (self.atoms.net_wm_name, self.atoms.utf8),
            (AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()),
        ] {
            if let Some(title) = self.get_string_property(window, property, kind)? {
                return Ok(title);
            }
        }
        Ok(String::new())
    }

    /// Read the window's `_SPUI_ICON` identifier if set.
    pub fn get_icon_name(&self, window: Window) -> Result<Option<String>> {
        self.get_string_property(window, self.atoms.spui_icon, self.atoms.utf8)
    }

    pub fn sync(&self) -> Result<()> {
        self.conn.sync()?;
        Ok(())
    }

    pub fn put_canvas(&self, window: Window, canvas: &Canvas, area: Rect) -> Result<()> {
        self.put_canvas_depth_sync(window, canvas, area, self.depth, true)
    }

    pub fn put_canvas_unsynced(&self, window: Window, canvas: &Canvas, area: Rect) -> Result<()> {
        self.put_canvas_depth_sync(window, canvas, area, self.depth, false)
    }

    pub fn put_canvas_depth(
        &self,
        window: Window,
        canvas: &Canvas,
        area: Rect,
        depth: u8,
    ) -> Result<()> {
        self.put_canvas_depth_sync(window, canvas, area, depth, true)
    }

    fn put_canvas_depth_sync(
        &self,
        window: Window,
        canvas: &Canvas,
        area: Rect,
        depth: u8,
        sync_shm: bool,
    ) -> Result<()> {
        let area = area.intersect(Rect::new(0, 0, canvas.width as i32, canvas.height as i32));
        if area.is_empty() {
            return Ok(());
        }
        let gc = if depth == 32 && self.argb_gc != 0 {
            self.argb_gc
        } else {
            self.gc
        };
        if let Some(seg) = canvas.shm_seg() {
            self.conn.shm_put_image(
                window,
                gc,
                canvas.width as u16,
                canvas.height as u16,
                area.x as u16,
                area.y as u16,
                area.w as u16,
                area.h as u16,
                area.x as i16,
                area.y as i16,
                depth,
                ImageFormat::Z_PIXMAP.into(),
                false,
                seg,
                0,
            )?;
            if sync_shm {
                // Synchronize so Xorg finishes reading the SHM buffer into ShadowFB
                // before the client mutates `canvas` on the next frame.
                self.conn.sync()?;
            }
            return Ok(());
        }

        let pixels = canvas.pixels();
        let row_bytes = area.w as usize * 4;
        // Fast path when uploading full-width horizontal strips on little-endian hosts.
        if area.x == 0 && area.w as usize == canvas.width && cfg!(target_endian = "little") {
            let rows = (self.max_request.saturating_sub(64) / row_bytes).max(1) as i32;
            let mut y = area.y;
            while y < area.bottom() {
                let height = (area.bottom() - y).min(rows);
                let start = y as usize * canvas.width;
                let len = height as usize * canvas.width;
                let slice = &pixels[start..start + len];
                let bytes = unsafe {
                    std::slice::from_raw_parts(slice.as_ptr() as *const u8, slice.len() * 4)
                };
                self.conn.put_image(
                    ImageFormat::Z_PIXMAP,
                    window,
                    gc,
                    area.w as u16,
                    height as u16,
                    0,
                    y as i16,
                    0,
                    depth,
                    bytes,
                )?;
                y += height;
            }
            return Ok(());
        }

        let rows = (self.max_request.saturating_sub(64) / row_bytes).max(1) as i32;
        let mut bytes = Vec::with_capacity(row_bytes * rows.min(area.h) as usize);
        let mut y = area.y;
        while y < area.bottom() {
            let height = (area.bottom() - y).min(rows);
            bytes.clear();
            for row in y..y + height {
                let start = row as usize * canvas.width + area.x as usize;
                let row_slice = &pixels[start..start + area.w as usize];
                let raw = unsafe {
                    std::slice::from_raw_parts(row_slice.as_ptr() as *const u8, row_slice.len() * 4)
                };
                bytes.extend_from_slice(raw);
            }
            self.conn.put_image(
                ImageFormat::Z_PIXMAP,
                window,
                gc,
                area.w as u16,
                height as u16,
                area.x as i16,
                y as i16,
                0,
                depth,
                &bytes,
            )?;
            y += height;
        }
        Ok(())
    }

    pub fn poll(&self) -> Result<Option<Event>> {
        Ok(self.conn.poll_for_event()?)
    }

    /// Block using `poll(2)` until X11, `uxconf_fd`, or `extra_fd` becomes readable.
    pub fn wait_fds(
        &self,
        uxconf_fd: Option<i32>,
        extra_fd: Option<i32>,
        timeout_ms: i32,
    ) -> (bool, bool, bool) {
        let x_fd = self.conn.stream().as_raw_fd();
        let mut fds = [libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        }; 3];
        fds[0] = libc::pollfd {
            fd: x_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let mut nfds = 1usize;
        let ux_idx = uxconf_fd.map(|fd| {
            let idx = nfds;
            fds[idx] = libc::pollfd {
                fd,
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            };
            nfds += 1;
            idx
        });
        let ext_idx = extra_fd.map(|fd| {
            let idx = nfds;
            fds[idx] = libc::pollfd {
                fd,
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            };
            nfds += 1;
            idx
        });
        // SAFETY: `fds[..nfds]` is a valid contiguous stack slice of open descriptors.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), nfds as libc::nfds_t, timeout_ms) };
        if rc <= 0 {
            return (false, false, false);
        }
        let mask = libc::POLLIN | libc::POLLHUP | libc::POLLERR;
        let x_ready = fds[0].revents & mask != 0;
        let ux_ready = ux_idx.is_some_and(|i| fds[i].revents & mask != 0);
        let ext_ready = ext_idx.is_some_and(|i| fds[i].revents & mask != 0);
        (x_ready, ux_ready, ext_ready)
    }

    /// Block using `poll(2)` until the X11 socket (or `extra_fd`, e.g. PTY master) is readable.
    #[allow(dead_code)]
    pub fn wait_readable(&self, extra_fd: Option<i32>, timeout_ms: i32) -> (bool, bool) {
        let (x_ready, _, ext_ready) = self.wait_fds(None, extra_fd, timeout_ms);
        (x_ready, ext_ready)
    }

    /// Query `(x, y, width, height, depth, override_redirect, viewable)` for any X11 window.
    pub fn get_window_info(&self, window: Window) -> Option<(i16, i16, u16, u16, u8, bool, bool)> {
        let geom_cookie = self.conn.get_geometry(window).ok()?;
        let attr_cookie = self.conn.get_window_attributes(window).ok()?;
        let geom = geom_cookie.reply().ok()?;
        let attr = attr_cookie.reply().ok()?;
        Some((
            geom.x,
            geom.y,
            geom.width,
            geom.height,
            geom.depth,
            attr.override_redirect,
            attr.map_state == x11rb::protocol::xproto::MapState::VIEWABLE,
        ))
    }

    /// Query mapped `override_redirect` direct children of `root` in stacking order.
    pub fn query_override_children(&self) -> Vec<(Window, i16, i16, u16, u16, u8)> {
        let Ok(cookie) = self.conn.query_tree(self.root) else {
            return Vec::new();
        };
        let Ok(reply) = cookie.reply() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for w in reply.children {
            if let Some((x, y, width, height, depth, or_flag, viewable)) = self.get_window_info(w) {
                if or_flag && viewable && width > 0 && height > 0 {
                    out.push((w, x, y, width, height, depth));
                }
            }
        }
        out
    }

    pub fn flush(&self) -> Result<()> {
        self.conn.flush()?;
        Ok(())
    }

    pub fn key_symbol(&self, code: u8, state: u16) -> u32 {
        let index = code.saturating_sub(self.min_keycode) as usize;
        let per = self.keysyms_per.max(1) as usize;
        let base = index * per;
        let shift = state & 1 != 0;
        let caps = state & 2 != 0;
        let normal = self.keysyms.get(base).copied().unwrap_or(0);
        let shifted = self
            .keysyms
            .get(base + 1)
            .copied()
            .filter(|&s| s != 0)
            .unwrap_or(normal);
        let is_alpha = (0x61..=0x7a).contains(&normal) || (0x41..=0x5a).contains(&normal);
        if is_alpha {
            if shift ^ caps {
                if (0x61..=0x7a).contains(&normal) {
                    normal - 32
                } else {
                    shifted
                }
            } else if (0x41..=0x5a).contains(&normal) {
                normal + 32
            } else {
                normal
            }
        } else if shift {
            shifted
        } else {
            normal
        }
    }
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Self> {
        let get = |name: &str| -> Result<Atom> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        Ok(Self {
            wm_protocols: get("WM_PROTOCOLS")?,
            wm_delete: get("WM_DELETE_WINDOW")?,
            net_wm_name: get("_NET_WM_NAME")?,
            utf8: get("UTF8_STRING")?,
            net_wm_type: get("_NET_WM_WINDOW_TYPE")?,
            net_wm_type_normal: get("_NET_WM_WINDOW_TYPE_NORMAL")?,
            net_wm_type_dock: get("_NET_WM_WINDOW_TYPE_DOCK")?,
            net_wm_type_dialog: get("_NET_WM_WINDOW_TYPE_DIALOG")?,
            net_wm_type_desktop: get("_NET_WM_WINDOW_TYPE_DESKTOP")?,
            net_wm_state: get("_NET_WM_STATE")?,
            net_wm_state_hidden: get("_NET_WM_STATE_HIDDEN")?,
            net_client_list: get("_NET_CLIENT_LIST")?,
            net_active_window: get("_NET_ACTIVE_WINDOW")?,
            net_supporting_wm_check: get("_NET_SUPPORTING_WM_CHECK")?,
            net_supported: get("_NET_SUPPORTED")?,
            net_wm_pid: get("_NET_WM_PID")?,
            wm_class: get("WM_CLASS")?,
            spui_window_cmd: get("_SPUI_WINDOW_CMD")?,
            spui_icon: get("_SPUI_ICON")?,
            spui_glass_extents: get("_SPUI_GLASS_EXTENTS")?,
            spui_compositor: get("_SPUI_COMPOSITOR")?,
            spime_status: get("_SPIME_STATUS")?,
            spime_notify: get("_SPIME_NOTIFY")?,
        })
    }
}

fn init_argb_visual(
    conn: &RustConnection,
    screen: usize,
    screen_info: &x11rb::protocol::xproto::Screen,
) -> (Option<Visualid>, Colormap, Gcontext) {
    let mut found_visual = None;
    if let Ok(cookie) = conn.render_query_pict_formats() {
        if let Ok(formats) = cookie.reply() {
            let argb_fmt = formats.formats.iter().find(|info| {
                info.type_ == PictType::DIRECT
                    && info.depth == 32
                    && info.direct.alpha_mask == 0xff
                    && info.direct.red_mask == 0xff
                    && info.direct.green_mask == 0xff
                    && info.direct.blue_mask == 0xff
                    && info.direct.alpha_shift == 24
            });
            if let (Some(fmt), Some(scr)) = (argb_fmt, formats.screens.get(screen)) {
                'outer: for d in &scr.depths {
                    if d.depth == 32 {
                        for v in &d.visuals {
                            if v.format == fmt.id {
                                found_visual = Some(v.visual);
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
    }
    if found_visual.is_none() {
        found_visual = screen_info
            .allowed_depths
            .iter()
            .find(|d| d.depth == 32)
            .and_then(|d| d.visuals.first())
            .map(|v| v.visual_id);
    }
    let Some(visual) = found_visual else {
        return (None, 0, 0);
    };
    let Ok(cmap) = conn.generate_id() else {
        return (None, 0, 0);
    };
    if conn
        .create_colormap(ColormapAlloc::NONE, cmap, screen_info.root, visual)
        .is_err()
    {
        return (None, 0, 0);
    }
    let Ok(tmp) = conn.generate_id() else {
        return (None, 0, 0);
    };
    if conn.create_pixmap(32, tmp, screen_info.root, 1, 1).is_err() {
        return (None, 0, 0);
    }
    let Ok(gc) = conn.generate_id() else {
        let _ = conn.free_pixmap(tmp);
        return (None, 0, 0);
    };
    if conn.create_gc(gc, tmp, &Default::default()).is_err() {
        let _ = conn.free_pixmap(tmp);
        return (None, 0, 0);
    }
    let _ = conn.free_pixmap(tmp);
    (Some(visual), cmap, gc)
}

struct XcursorImage {
    width: u16,
    height: u16,
    xhot: u16,
    yhot: u16,
    pixels_le: Vec<u8>,
}

fn parse_xcursor(raw: &[u8], target_nominal: u32) -> Option<XcursorImage> {
    if raw.len() < 16 || &raw[0..4] != b"Xcur" {
        return None;
    }
    let ntoc = u32::from_le_bytes(raw[12..16].try_into().ok()?) as usize;
    if raw.len() < 16 + ntoc * 12 {
        return None;
    }
    let mut best: Option<(u32, usize)> = None;
    for i in 0..ntoc {
        let off = 16 + i * 12;
        let t_type = u32::from_le_bytes(raw[off..off + 4].try_into().ok()?);
        let t_sub = u32::from_le_bytes(raw[off + 4..off + 8].try_into().ok()?);
        let t_pos = u32::from_le_bytes(raw[off + 8..off + 12].try_into().ok()?) as usize;
        if t_type != 0xfffd_0002 {
            continue;
        }
        let dist = t_sub.abs_diff(target_nominal);
        if best.is_none_or(|(bdist, _)| dist < bdist) {
            best = Some((dist, t_pos));
        }
    }
    let (_, pos) = best?;
    if raw.len() < pos + 36 {
        return None;
    }
    let w = u32::from_le_bytes(raw[pos + 16..pos + 20].try_into().ok()?);
    let h = u32::from_le_bytes(raw[pos + 20..pos + 24].try_into().ok()?);
    let xhot = u32::from_le_bytes(raw[pos + 24..pos + 28].try_into().ok()?);
    let yhot = u32::from_le_bytes(raw[pos + 28..pos + 32].try_into().ok()?);
    let byte_len = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if w == 0 || h == 0 || w > 128 || h > 128 || raw.len() < pos + 36 + byte_len {
        return None;
    }
    Some(XcursorImage {
        width: w as u16,
        height: h as u16,
        xhot: (xhot as u16).min(w as u16 - 1),
        yhot: (yhot as u16).min(h as u16 - 1),
        pixels_le: raw[pos + 36..pos + 36 + byte_len].to_vec(),
    })
}

fn find_argb32_format(conn: &RustConnection) -> Option<Pictformat> {
    let formats = conn.render_query_pict_formats().ok()?.reply().ok()?;
    for info in &formats.formats {
        if info.type_ == PictType::DIRECT
            && info.depth == 32
            && info.direct.alpha_mask == 0xff
            && info.direct.red_mask == 0xff
            && info.direct.green_mask == 0xff
            && info.direct.blue_mask == 0xff
            && info.direct.alpha_shift == 24
            && info.direct.red_shift == 16
        {
            return Some(info.id);
        }
    }
    None
}

fn fallback_arrow_image() -> XcursorImage {
    const W: usize = 24;
    const H: usize = 24;
    let mut pixels = vec![0u8; W * H * 4];
    for y in 0..17usize {
        let span = if y < 12 { y + 1 } else { (17 - y).max(2) };
        for x in 0..span {
            let border = x == 0 || x + 1 == span || y == 0 || (y == 11 && x > 4) || y == 16;
            let (b, g, r, a) = if border {
                (0xff, 0xff, 0xff, 0xff)
            } else {
                (0x1b, 0x18, 0x18, 0xff)
            };
            let idx = (y * W + x) * 4;
            pixels[idx..idx + 4].copy_from_slice(&[b, g, r, a]);
        }
    }
    XcursorImage {
        width: W as u16,
        height: H as u16,
        xhot: 1,
        yhot: 1,
        pixels_le: pixels,
    }
}

fn create_argb_cursor(
    conn: &RustConnection,
    root: Window,
    fmt: Pictformat,
    img: &XcursorImage,
) -> Option<Cursor> {
    let pixmap = conn.generate_id().ok()?;
    let gc = conn.generate_id().ok()?;
    let picture = conn.generate_id().ok()?;
    let cursor = conn.generate_id().ok()?;
    conn.create_pixmap(32, pixmap, root, img.width, img.height)
        .ok()?;
    conn.create_gc(gc, pixmap, &Default::default()).ok()?;
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        pixmap,
        gc,
        img.width,
        img.height,
        0,
        0,
        0,
        32,
        &img.pixels_le,
    )
    .ok()?;
    conn.render_create_picture(picture, pixmap, fmt, &Default::default())
        .ok()?;
    conn.render_create_cursor(cursor, picture, img.xhot, img.yhot)
        .ok()?;
    let _ = conn.render_free_picture(picture);
    let _ = conn.free_gc(gc);
    let _ = conn.free_pixmap(pixmap);
    Some(cursor)
}

fn load_cursors(conn: &RustConnection, root: Window) -> [Cursor; 8] {
    let mut out = [0u32; 8];
    let Some(fmt) = find_argb32_format(conn) else {
        return out;
    };
    let dirs = [
        "/usr/share/icons/breeze_cursors/cursors",
        "/usr/share/icons/Breeze_Light/cursors",
    ];
    let fallback = fallback_arrow_image();
    let default_cursor = create_argb_cursor(conn, root, fmt, &fallback).unwrap_or(0);

    for kind in ALL_CURSORS {
        let mut loaded = None;
        'search: for dir in dirs {
            for name in kind.xcursor_names() {
                let path = format!("{dir}/{name}");
                if let Ok(raw) = fs::read(&path) {
                    if let Some(img) = parse_xcursor(&raw, 24) {
                        if let Some(cid) = create_argb_cursor(conn, root, fmt, &img) {
                            loaded = Some(cid);
                            break 'search;
                        }
                    }
                }
            }
        }
        out[kind.index()] = loaded.unwrap_or(default_cursor);
    }
    let _ = conn.flush();
    out
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub code: u8,
    pub state: u16,
    pub sym: u32,
}

impl Key {
    pub fn shift(&self) -> bool {
        self.state & 1 != 0
    }

    pub fn control(&self) -> bool {
        self.state & 4 != 0
    }

    pub fn alt(&self) -> bool {
        self.state & 8 != 0
    }

    pub fn is_modifier(&self) -> bool {
        (0xffe1..=0xffee).contains(&self.sym) || (0xfe01..=0xfe1f).contains(&self.sym)
    }

    pub fn text(&self) -> Option<char> {
        if self.is_modifier() {
            return None;
        }
        match self.sym {
            0xff0d | 0xff8d => Some('\n'),
            0xff08 => Some('\u{8}'),
            0xff09 => Some('\t'),
            sym if (0x0020..=0x007e).contains(&sym) => char::from_u32(sym),
            sym if (0x00a0..=0x00ff).contains(&sym) => char::from_u32(sym),
            sym if (0x0100_0100..=0x0110_ffff).contains(&sym) => char::from_u32(sym - 0x0100_0000),
            _ => None,
        }
    }

    pub fn ctrl_byte(&self) -> Option<u8> {
        if !self.control() {
            return None;
        }
        let ch = self.text()?;
        if ch.is_ascii_alphabetic() {
            Some((ch.to_ascii_lowercase() as u8) - b'a' + 1)
        } else {
            match ch {
                '[' | '3' => Some(0x1b),
                '\\' | '4' => Some(0x1c),
                ']' | '5' => Some(0x1d),
                '^' | '6' => Some(0x1e),
                '_' | '7' | '/' => Some(0x1f),
                '@' | ' ' | '2' => Some(0x00),
                _ => None,
            }
        }
    }

    pub fn is_enter(&self) -> bool {
        self.sym == 0xff0d || self.sym == 0xff8d
    }

    pub fn is_backspace(&self) -> bool {
        self.sym == 0xff08
    }

    pub fn is_tab(&self) -> bool {
        self.sym == 0xff09 || self.sym == 0xfe20
    }

    pub fn is_escape(&self) -> bool {
        self.sym == 0xff1b
    }

    pub fn is_delete(&self) -> bool {
        self.sym == 0xffff
    }

    pub fn is_page_up(&self) -> bool {
        self.sym == 0xff55
    }

    pub fn is_page_down(&self) -> bool {
        self.sym == 0xff56
    }

    pub fn arrow(&self) -> Option<u8> {
        match self.sym {
            0xff51 => Some(0), // Left
            0xff52 => Some(1), // Up
            0xff53 => Some(2), // Right
            0xff54 => Some(3), // Down
            _ => None,
        }
    }

    pub fn is_home(&self) -> bool {
        self.sym == 0xff50
    }

    pub fn is_end(&self) -> bool {
        self.sym == 0xff57
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_modifier_never_maps_to_pound_sign() {
        let shift_l = Key {
            code: 50,
            state: 0,
            sym: 0xffe1,
        };
        assert!(shift_l.is_modifier());
        assert_eq!(shift_l.text(), None);

        let upper_a = Key {
            code: 38,
            state: 1,
            sym: b'A' as u32,
        };
        assert_eq!(upper_a.text(), Some('A'));
    }
}
