/*
 *
 *       user/desktop/src/compositor.rs
 *       VortexGlass 4.0 X11 software compositor, DWM glass blur, tweens, Flip 3D & Live Thumbnails
 *
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#![allow(dead_code)]

use crate::draw::{
    lerp_f32, mix, premultiply_u8, scale_premul, src_over_premul, src_over_u8, BlurScratch, Canvas,
    Rect, RectRegion,
};
use crate::icons::{self, Icon};
use crate::text::{Face, TextGlow, TextRenderer};
use crate::theme::Theme;
use crate::vortex::{
    blit_aero_sheen, draw_glass_chrome_overlay, smootherstep, VortexFrameCache, AERO_BLUR_RADIUS,
};
use crate::x11::{ShmReadBuf, X};
use std::time::{Duration, Instant};
use x11rb::protocol::xproto::Window;

/// Retained per-window Composite backing pixmap, XDamage handle, and premultiplied snapshot.
pub struct CompWindow {
    pub window: Window,
    pub damage: u32,
    pub pixmap: u32,
    pub pixmap_w: usize,
    pub pixmap_h: usize,
    pub depth: u8,
    /// DWM extended client glass `[top, bottom, left, right, corner_r]` if SPUI app.
    pub glass_extents: Option<[i32; 5]>,
    pub pixels: Vec<u32>,
    pub pw: usize,
    pub ph: usize,
    pub has_pixels: bool,
    pub needs_capture: bool,
    pub dirty_bounds: Option<Rect>,
    pub resize_wait_until: Option<Instant>,
    pub awaiting_sync_commit: bool,
    pub frame_cache: Option<VortexFrameCache>,
    pub anim: Option<WindowAnim>,
}

impl CompWindow {
    pub fn new(window: Window, depth: u8, glass_extents: Option<[i32; 5]>) -> Self {
        Self {
            window,
            damage: 0,
            pixmap: 0,
            pixmap_w: 0,
            pixmap_h: 0,
            depth,
            glass_extents,
            pixels: Vec::new(),
            pw: 0,
            ph: 0,
            has_pixels: false,
            needs_capture: true,
            dirty_bounds: None,
            resize_wait_until: None,
            awaiting_sync_commit: false,
            frame_cache: None,
            anim: None,
        }
    }

    pub fn ensure_buffer(&mut self, w: usize, h: usize) {
        if self.pw != w || self.ph != h || self.pixels.len() != w * h {
            self.pw = w;
            self.ph = h;
            self.pixels.resize(w * h, 0);
            self.has_pixels = false;
            self.needs_capture = true;
            self.dirty_bounds = None;
        }
    }

    pub fn mark_damage(&mut self, rect: Rect) {
        if self.awaiting_sync_commit {
            // Delay capture until the client commits its first drawn resize frame (cmd=22) or timeout.
            self.dirty_bounds = Some(match self.dirty_bounds {
                Some(prev) => prev.union(rect),
                None => rect,
            });
            return;
        }
        self.needs_capture = true;
        self.dirty_bounds = Some(match self.dirty_bounds {
            Some(prev) => prev.union(rect),
            None => rect,
        });
    }

    pub fn confirm_sync_commit(&mut self) {
        self.awaiting_sync_commit = false;
        self.resize_wait_until = None;
        self.needs_capture = true;
        if self.pixmap_w > 0 && self.pixmap_h > 0 {
            self.dirty_bounds = Some(Rect::new(0, 0, self.pixmap_w as i32, self.pixmap_h as i32));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimKind {
    Open,
    Restore { taskbar_btn: Rect },
}

#[derive(Clone, Copy, Debug)]
pub struct WindowAnim {
    pub kind: AnimKind,
    pub start: Instant,
    pub duration_ms: u32,
}

impl WindowAnim {
    pub fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.start).as_secs_f32() * 1000.0;
        (elapsed / self.duration_ms.max(1) as f32).clamp(0.0, 1.0)
    }

    pub fn is_done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

/// Snapshot kept alive after a window closes or minimizes so its zoom/fly tween finishes cleanly.
pub struct ClosingGhost {
    pub pixels: Vec<u32>,
    pub pw: usize,
    pub ph: usize,
    pub from: Rect,
    pub to_taskbar: Option<Rect>,
    pub corner_r: i32,
    pub start: Instant,
    pub duration_ms: u32,
}

impl ClosingGhost {
    pub fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.start).as_secs_f32() * 1000.0;
        (elapsed / self.duration_ms.max(1) as f32).clamp(0.0, 1.0)
    }
}

/// Aero Snap drag target zone (`0=none`, `1=left`, `2=right`, `3=maximize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapZone {
    None,
    Left,
    Right,
    Maximize,
}

impl SnapZone {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Left,
            2 => Self::Right,
            3 => Self::Maximize,
            _ => Self::None,
        }
    }

    pub fn target_rect(self, scr_w: i32, work_top: i32, work_h: i32) -> Option<Rect> {
        let pad = 6;
        match self {
            Self::None => None,
            Self::Left => Some(Rect::new(
                pad,
                work_top + pad,
                (scr_w / 2 - pad * 2).max(160),
                (work_h - pad * 2).max(160),
            )),
            Self::Right => {
                let half = scr_w / 2;
                Some(Rect::new(
                    half + pad,
                    work_top + pad,
                    (scr_w - half - pad * 2).max(160),
                    (work_h - pad * 2).max(160),
                ))
            }
            Self::Maximize => Some(Rect::new(
                pad,
                work_top + pad,
                (scr_w - pad * 2).max(200),
                (work_h - pad * 2).max(200),
            )),
        }
    }
}

pub struct AltTabState {
    pub selected: usize,
    pub opened: Instant,
}

pub struct Flip3dState {
    pub selected: usize,
    pub scroll: f32,
    pub target_scroll: f32,
    pub opened: Instant,
}

pub struct ThumbPopupState {
    pub win_index: usize,
    pub target_window: Window,
    pub rect: Rect,
    pub close_rect: Rect,
    pub img_rect: Rect,
    pub pointer_inside: bool,
    pub close_hover: bool,
    pub opened: Instant,
}

/// Compute outer damage/culling bounds for a window rectangle (`wm_window_bound` from Uinxed-Kernel).
pub fn window_damage_bounds(win_rect: Rect, is_vortex: bool, is_max: bool) -> Rect {
    if is_max || win_rect.is_empty() {
        win_rect
    } else if is_vortex {
        Rect::new(
            win_rect.x - crate::vortex::AERO_DAMAGE_SIDE,
            win_rect.y - crate::vortex::AERO_DAMAGE_TOP,
            win_rect.w + crate::vortex::AERO_DAMAGE_SIDE * 2,
            win_rect.h + crate::vortex::AERO_DAMAGE_TOP + crate::vortex::AERO_DAMAGE_BOTTOM,
        )
    } else {
        win_rect.expand(12)
    }
}

/// Granular per-frame animation invalidation flags returned by `Compositor::tick`.
#[derive(Default, Clone, Copy, Debug)]
pub struct TickOutcome {
    pub animating: bool,
    pub win_anims_or_ghosts: bool,
    pub snap_changed: bool,
    pub peek_or_flip_changed: bool,
    pub menu_changed: bool,
    pub ctx_changed: bool,
    pub vol_changed: bool,
    pub toast_changed: bool,
    pub orb_changed: bool,
}

/// Bounded disjoint dirty-rectangle tracker preventing localized updates from ballooning to full-screen redraws.
#[derive(Default, Clone, Debug)]
pub struct DirtyRegion {
    pub rects: Vec<Rect>,
    pub full: bool,
}

impl DirtyRegion {
    const MAX_RECTS: usize = 6;
    const MERGE_PAD: i32 = 32;

    pub fn mark_full(&mut self) {
        self.full = true;
        self.rects.clear();
    }

    pub fn clear(&mut self) {
        self.full = false;
        self.rects.clear();
    }

    pub fn is_empty(&self) -> bool {
        !self.full && self.rects.is_empty()
    }

    pub fn mark(&mut self, rect: Rect, bounds: Rect) {
        if self.full || rect.is_empty() || bounds.is_empty() {
            return;
        }
        let mut r = rect.intersect(bounds);
        if r.is_empty() {
            return;
        }
        let screen_area = i64::from(bounds.w) * i64::from(bounds.h);
        if i64::from(r.w) * i64::from(r.h) * 4 >= screen_area * 3 {
            self.mark_full();
            return;
        }
        loop {
            let probe = r.expand(Self::MERGE_PAD);
            let mut merged_any = false;
            let mut i = 0;
            while i < self.rects.len() {
                if !self.rects[i].intersect(probe).is_empty() {
                    r = r.union(self.rects.swap_remove(i));
                    merged_any = true;
                } else {
                    i += 1;
                }
            }
            if !merged_any {
                break;
            }
        }
        r = r.intersect(bounds);
        if i64::from(r.w) * i64::from(r.h) * 4 >= screen_area * 3 {
            self.mark_full();
            return;
        }
        if self.rects.len() < Self::MAX_RECTS {
            self.rects.push(r);
        } else {
            let mut best_idx = 0usize;
            let mut best_waste = i64::MAX;
            for (idx, &ex) in self.rects.iter().enumerate() {
                let u = ex.union(r);
                let waste = i64::from(u.w) * i64::from(u.h) - i64::from(ex.w) * i64::from(ex.h);
                if waste < best_waste {
                    best_waste = waste;
                    best_idx = idx;
                }
            }
            self.rects[best_idx] = self.rects[best_idx].union(r).intersect(bounds);
        }
    }

    pub fn take_into(&mut self, bounds: Rect, out: &mut Vec<Rect>) {
        out.clear();
        if bounds.is_empty() {
            self.clear();
            return;
        }
        if self.full {
            out.push(bounds);
        } else {
            for r in self.rects.drain(..) {
                let clipped = r.intersect(bounds);
                if !clipped.is_empty() {
                    out.push(clipped);
                }
            }
        }
        self.full = false;
    }
}

/// Software compositor state owned by `Shell`.
pub struct Compositor {
    pub active: bool,
    pub overlay: Option<Window>,
    pub canvas: Option<Canvas>,
    pub frost_tmp: Option<Canvas>,
    pub shm_read: Option<ShmReadBuf>,
    pub dirty: DirtyRegion,
    pub bounds: Rect,
    pub blur_scratch: BlurScratch,
    pub desktop_blur_cache: Vec<u32>,
    pub desktop_blur_size: (usize, usize),
    pub desktop_blur_valid: bool,
    pub desktop_serial: u64,
    pub need_region: RectRegion,
    pub desktop_need: RectRegion,
    pub strip_scratch: Vec<Rect>,
    pub ghosts: Vec<ClosingGhost>,
    pub snap_zone: SnapZone,
    pub snap_rect: Option<Rect>,
    pub snap_alpha: f32,
    pub alt_tab: Option<AltTabState>,
    pub flip3d: Option<Flip3dState>,
    pub thumb: Option<ThumbPopupState>,
    pub thumb_hover_start: Option<(usize, Instant)>,
    pub thumb_leave_at: Option<Instant>,
    pub taskbar_mouse_x: Option<i32>,
    pub show_desktop_peek: bool,
    pub peek_factor: f32,
    pub start_orb_phase: f32,
    pub menu_anim: f32,
    pub ctx_anim: f32,
    pub vol_anim: f32,
    pub toast_anim: f32,
    pub last_tick: Instant,
}

impl Compositor {
    pub fn new(x: &X, theme: &Theme) -> Self {
        let overlay = x.enable_compositor().ok().flatten();
        let active = overlay.is_some();
        let (canvas, frost_tmp, shm_read) = if active {
            let mut c = x.create_canvas(x.width as usize, x.height as usize, theme.bg_top);
            c.set_argb(false);
            let frost = Canvas::new(x.width as usize, x.height as usize, theme.bg_top);
            let read_buf = x.create_shm_read_buffer(x.width as usize, x.height as usize);
            (Some(c), Some(frost), read_buf)
        } else {
            (None, None, None)
        };
        let mut dirty = DirtyRegion::default();
        dirty.mark_full();
        Self {
            active,
            overlay,
            canvas,
            frost_tmp,
            shm_read,
            dirty,
            bounds: Rect::new(0, 0, i32::from(x.width), i32::from(x.height)),
            blur_scratch: BlurScratch::default(),
            desktop_blur_cache: Vec::new(),
            desktop_blur_size: (0, 0),
            desktop_blur_valid: false,
            desktop_serial: 1,
            need_region: RectRegion::new(),
            desktop_need: RectRegion::new(),
            strip_scratch: Vec::with_capacity(16),
            ghosts: Vec::new(),
            snap_zone: SnapZone::None,
            snap_rect: None,
            snap_alpha: 0.0,
            alt_tab: None,
            flip3d: None,
            thumb: None,
            thumb_hover_start: None,
            thumb_leave_at: None,
            taskbar_mouse_x: None,
            show_desktop_peek: false,
            peek_factor: 0.0,
            start_orb_phase: 0.0,
            menu_anim: 0.0,
            ctx_anim: 0.0,
            vol_anim: 0.0,
            toast_anim: 0.0,
            last_tick: Instant::now(),
        }
    }

    pub fn shutdown(&mut self, x: &X) {
        if let Some(c) = self.canvas.take() {
            x.release_canvas(&c);
        }
        self.frost_tmp = None;
        if let Some(buf) = self.shm_read.take() {
            x.release_shm_read_buffer(&buf);
        }
        if let Some(ov) = self.overlay.take() {
            x.disable_compositor(Some(ov));
        }
        self.active = false;
    }

    pub fn resize(&mut self, x: &X, w: u16, h: u16, bg: u32) {
        if !self.active {
            return;
        }
        if let Some(ov) = self.overlay {
            let _ = x.move_resize(ov, 0, 0, w, h);
        }
        if let Some(old) = self.canvas.take() {
            x.release_canvas(&old);
        }
        let mut next = x.create_canvas(w as usize, h as usize, bg);
        next.set_argb(false);
        self.canvas = Some(next);
        self.frost_tmp = Some(Canvas::new(w as usize, h as usize, bg));
        self.bounds = Rect::new(0, 0, i32::from(w), i32::from(h));

        if let Some(old_buf) = self.shm_read.take() {
            x.release_shm_read_buffer(&old_buf);
        }
        self.shm_read = x.create_shm_read_buffer(w as usize, h as usize);
        self.desktop_blur_valid = false;
        self.desktop_serial = self.desktop_serial.wrapping_add(1);
        self.mark_full_dirty();
    }

    pub fn mark_full_dirty(&mut self) {
        self.dirty.mark_full();
    }

    pub fn mark_dirty(&mut self, rect: Rect) {
        self.dirty.mark(rect, self.bounds);
    }

    pub fn invalidate_desktop_blur(&mut self) {
        self.desktop_blur_valid = false;
        self.desktop_serial = self.desktop_serial.wrapping_add(1);
        self.mark_full_dirty();
    }

    /// Recompute cached full-screen blurred desktop wallpaper only when desktop content changed.
    pub fn ensure_desktop_blur(&mut self, desktop: &Canvas) {
        let w = desktop.width;
        let h = desktop.height;
        if self.desktop_blur_valid && self.desktop_blur_size == (w, h) {
            return;
        }
        self.desktop_blur_size = (w, h);
        self.desktop_blur_cache.resize(w * h, 0);
        self.desktop_blur_cache.copy_from_slice(desktop.pixels());
        crate::draw::blur_rect_fast(
            &mut self.desktop_blur_cache,
            w,
            h,
            Rect::new(0, 0, w as i32, h as i32),
            crate::vortex::AERO_BLUR_RADIUS,
            &mut self.blur_scratch,
        );
        self.desktop_blur_valid = true;
    }

    /// Advance animation clocks and return granular invalidation flags.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        now: Instant,
        menu_open: bool,
        ctx_open: bool,
        vol_open: bool,
        toast_open: bool,
        start_orb_hovered: bool,
        window_anims: &mut [Option<WindowAnim>],
    ) -> TickOutcome {
        let dt = now
            .duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_tick = now;

        let mut out = TickOutcome::default();

        // Window open/restore animations
        for slot in window_anims.iter_mut() {
            if let Some(a) = slot {
                out.win_anims_or_ghosts = true;
                out.animating = true;
                if a.is_done(now) {
                    *slot = None;
                }
            }
        }

        // Closing / minimizing ghosts
        if !self.ghosts.is_empty() {
            self.ghosts.retain(|g| g.progress(now) < 1.0);
            out.win_anims_or_ghosts = true;
            out.animating = true;
        }

        // Snap ghost preview fade
        let snap_target = if self.snap_zone != SnapZone::None {
            1.0
        } else {
            0.0
        };
        if step_towards(&mut self.snap_alpha, snap_target, dt * 11.0) {
            out.snap_changed = true;
            out.animating = true;
        }

        // Aero Peek fade (hovered thumbnail OR right-edge show desktop)
        let peek_on =
            self.show_desktop_peek || self.thumb.as_ref().is_some_and(|t| t.pointer_inside);
        let peek_target = if peek_on { 1.0 } else { 0.0 };
        if step_towards(&mut self.peek_factor, peek_target, dt * 10.0) {
            out.peek_or_flip_changed = true;
            out.animating = true;
        }

        // Popup open tweens
        if step_towards(
            &mut self.menu_anim,
            if menu_open { 1.0 } else { 0.0 },
            dt * 14.0,
        ) {
            out.menu_changed = true;
            out.animating = true;
        }
        if step_towards(
            &mut self.ctx_anim,
            if ctx_open { 1.0 } else { 0.0 },
            dt * 16.0,
        ) {
            out.ctx_changed = true;
            out.animating = true;
        }
        if step_towards(
            &mut self.vol_anim,
            if vol_open { 1.0 } else { 0.0 },
            dt * 14.0,
        ) {
            out.vol_changed = true;
            out.animating = true;
        }
        if step_towards(
            &mut self.toast_anim,
            if toast_open { 1.0 } else { 0.0 },
            dt * 12.0,
        ) {
            out.toast_changed = true;
            out.animating = true;
        }

        // Flip 3D smooth stack inertia
        if let Some(f) = self.flip3d.as_mut() {
            if step_towards(&mut f.scroll, f.target_scroll, dt * 14.0)
                || now.duration_since(f.opened).as_millis() < 240
            {
                out.peek_or_flip_changed = true;
                out.animating = true;
            }
        }

        // Start Orb gentle pulse when menu open or hovered
        let orb_target = if menu_open {
            1.0
        } else if start_orb_hovered {
            0.7
        } else {
            0.0
        };
        if step_towards(&mut self.start_orb_phase, orb_target, dt * 8.0) {
            out.orb_changed = true;
            out.animating = true;
        }

        // Live thumbnail delay trigger (90ms hover dwell on a taskbar button)
        if let Some((_, ts)) = self.thumb_hover_start {
            if self.thumb.is_none() && now.duration_since(ts).as_millis() < 110 {
                out.animating = true;
            }
        }

        out
    }

    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }
}

fn step_towards(val: &mut f32, target: f32, max_delta: f32) -> bool {
    let diff = target - *val;
    if diff.abs() <= 0.004 {
        let changed = (*val - target).abs() > f32::EPSILON;
        *val = target;
        changed
    } else {
        let factor = 1.0 - (-max_delta).exp();
        *val += diff * factor.max(0.08);
        if (target - *val).abs() <= 0.004 {
            *val = target;
        }
        true
    }
}

/// Bind `XComposite` pixmap and `XDamage` listener to a managed window after map/resize.
pub fn attach_window_pixmap(x: &X, cw: &mut CompWindow, w: u16, h: u16, depth: u8) {
    if !x.has_composite {
        return;
    }
    if cw.pixmap != 0 {
        let _ = x.free_pixmap(cw.pixmap);
        cw.pixmap = 0;
    }
    if cw.damage == 0 && x.has_damage {
        if let Ok(dmg) = x.create_damage(cw.window) {
            cw.damage = dmg;
        }
    }
    cw.depth = depth;
    let pw = usize::from(w.max(1));
    let ph = usize::from(h.max(1));
    cw.pixmap_w = pw;
    cw.pixmap_h = ph;
    if let Ok(px) = x.name_window_pixmap(cw.window) {
        cw.pixmap = px;
    }
    if !cw.has_pixels {
        cw.ensure_buffer(pw, ph);
        cw.needs_capture = cw.pixmap != 0;
        cw.dirty_bounds = Some(Rect::new(0, 0, pw as i32, ph as i32));
        cw.resize_wait_until = None;
        cw.awaiting_sync_commit = false;
    } else if cw.pw != pw || cw.ph != ph {
        // Hold off reading the new backing pixmap until the client commits `present()` (cmd=22)
        // (or the short safety deadline elapses) so resize never flashes a cleared blank frame.
        cw.needs_capture = false;
        cw.dirty_bounds = None;
        cw.awaiting_sync_commit = true;
        cw.resize_wait_until = Some(Instant::now() + Duration::from_millis(85));
    }
}

pub fn detach_window_resources(x: &X, cw: &mut CompWindow) {
    if cw.damage != 0 {
        x.destroy_damage(cw.damage);
        cw.damage = 0;
    }
    if cw.pixmap != 0 {
        let _ = x.free_pixmap(cw.pixmap);
        cw.pixmap = 0;
    }
}

/// Pull pending `XDamage` dirty rectangles from one off-screen window pixmap into `CompWindow::pixels`.
pub fn capture_one_window(
    x: &X,
    shm: Option<&ShmReadBuf>,
    cw: &mut CompWindow,
    now: Instant,
) -> Option<(Rect, bool)> {
    if !cw.needs_capture && cw.resize_wait_until.is_some_and(|dl| now >= dl) {
        cw.awaiting_sync_commit = false;
        cw.resize_wait_until = None;
        if cw.pixmap_w > 0 && cw.pixmap_h > 0 {
            cw.needs_capture = true;
            cw.dirty_bounds = Some(Rect::new(0, 0, cw.pixmap_w as i32, cw.pixmap_h as i32));
        }
    }
    if !cw.needs_capture {
        return None;
    }
    if cw.pixmap == 0 {
        if let Ok(px) = x.name_window_pixmap(cw.window) {
            cw.pixmap = px;
        } else {
            return None;
        }
    }
    let target_w = if cw.pixmap_w > 0 { cw.pixmap_w } else { cw.pw };
    let target_h = if cw.pixmap_h > 0 { cw.pixmap_h } else { cw.ph };
    if target_w == 0 || target_h == 0 {
        return None;
    }
    x.damage_subtract(cw.damage);
    let bounds = Rect::new(0, 0, target_w as i32, target_h as i32);
    let resizing = cw.pw != target_w || cw.ph != target_h || cw.pixels.len() != target_w * target_h;
    let area = if resizing {
        cw.dirty_bounds = None;
        bounds
    } else {
        cw.dirty_bounds
            .take()
            .map(|r| r.intersect(bounds))
            .unwrap_or(bounds)
    };
    cw.needs_capture = false;
    cw.awaiting_sync_commit = false;
    cw.resize_wait_until = None;
    if area.is_empty() {
        return None;
    }
    let first_capture = !cw.has_pixels || resizing;
    let ok = if resizing {
        let mut next = vec![0u32; target_w * target_h];
        let read_ok = x.read_pixmap_rect(
            cw.pixmap,
            shm,
            0,
            0,
            target_w as u16,
            target_h as u16,
            cw.depth,
            &mut next,
            target_w,
        );
        if read_ok {
            cw.pixels = next;
            cw.pw = target_w;
            cw.ph = target_h;
        }
        read_ok
    } else {
        x.read_pixmap_rect(
            cw.pixmap,
            shm,
            area.x as i16,
            area.y as i16,
            area.w as u16,
            area.h as u16,
            cw.depth,
            &mut cw.pixels,
            cw.pw,
        )
    };
    if ok {
        cw.has_pixels = true;
        Some((area, first_capture))
    } else {
        None
    }
}

/// Pull pending `XDamage` dirty rectangles from visible/eligible off-screen window pixmaps.
pub fn capture_dirty_windows<'a, I, F>(
    x: &X,
    shm: Option<&ShmReadBuf>,
    comps: I,
    now: Instant,
    should_capture: F,
) -> Vec<(Window, Rect, bool)>
where
    I: IntoIterator<Item = &'a mut CompWindow>,
    F: Fn(&CompWindow) -> bool,
{
    let mut updated = Vec::new();
    for cw in comps {
        if !should_capture(cw) {
            if cw.needs_capture {
                // Force a full read once this occluded window becomes exposed again so
                // XDamage BoundingBox throttling cannot drop interim interior updates.
                cw.dirty_bounds = None;
            }
            continue;
        }
        let win = cw.window;
        if let Some((area, first_capture)) = capture_one_window(x, shm, cw, now) {
            updated.push((win, area, first_capture));
        }
    }
    updated
}

/// Composite an opaque-or-ARGB source bitmap onto `dst` with optional `(opacity, glass_top, corner_r)` fast path.
#[allow(clippy::too_many_arguments)]
pub fn blit_window_pixels(
    dst: &mut Canvas,
    src: &[u32],
    sw: usize,
    sh: usize,
    dx: i32,
    dy: i32,
    opacity: u8,
    corner_r: i32,
    is_argb_source: bool,
) {
    blit_window_pixels_split(dst, src, sw, sh, dx, dy, opacity, corner_r, is_argb_source, 0);
}

/// Composite a window snapshot onto `dst`, splitting `[0..glass_top]` (ARGB blend) from
/// `[glass_top..sh]` (SIMD row `copy_from_slice`) when the client body is known opaque.
#[allow(clippy::too_many_arguments)]
pub fn blit_window_pixels_split(
    dst: &mut Canvas,
    src: &[u32],
    sw: usize,
    sh: usize,
    dx: i32,
    dy: i32,
    opacity: u8,
    corner_r: i32,
    is_argb_source: bool,
    opaque_split_y: i32,
) {
    if sw == 0 || sh == 0 || opacity == 0 || src.len() < sw * sh {
        return;
    }
    let target = Rect::new(dx, dy, sw as i32, sh as i32);
    let clip = dst.clip_rect(target);
    if clip.is_empty() {
        return;
    }
    dst.mark_dirty(clip);
    let stride = dst.width;
    let out = dst.pixels_mut();
    let full_opaque = !is_argb_source && opacity == 255 && corner_r <= 0;
    let split_row = if is_argb_source && opacity == 255 && corner_r <= 0 && opaque_split_y > 0 {
        opaque_split_y as usize
    } else {
        usize::MAX
    };

    for py in clip.y..clip.bottom() {
        let sy = (py - dy) as usize;
        let sx0 = (clip.x - dx) as usize;
        let count = clip.w as usize;
        let srow = &src[sy * sw + sx0..sy * sw + sx0 + count];
        let drow = &mut out[py as usize * stride + clip.x as usize
            ..py as usize * stride + clip.x as usize + count];

        if full_opaque || sy >= split_row {
            drow.copy_from_slice(srow);
        } else if opacity == 255 {
            for (d, &s) in drow.iter_mut().zip(srow.iter()) {
                let a = (s >> 24) & 0xff;
                if a == 255 {
                    *d = s & 0x00ff_ffff;
                } else if a != 0 {
                    *d = src_over_premul(0xff00_0000 | *d, s) & 0x00ff_ffff;
                }
            }
        } else {
            for (d, &s) in drow.iter_mut().zip(srow.iter()) {
                let spx = scale_premul(if is_argb_source { s } else { 0xff00_0000 | s }, opacity);
                if (spx >> 24) != 0 {
                    *d = src_over_premul(0xff00_0000 | *d, spx) & 0x00ff_ffff;
                }
            }
        }
    }
}

/// Fast scaled + faded premultiplied image blit for DWM open/close/minimize tweens & live thumbnails.
#[allow(clippy::too_many_arguments)]
pub fn blit_scaled_window(
    dst: &mut Canvas,
    src: &[u32],
    sw: usize,
    sh: usize,
    target: Rect,
    opacity: u8,
) {
    if sw == 0 || sh == 0 || src.len() < sw * sh || target.w <= 0 || target.h <= 0 || opacity == 0 {
        return;
    }
    let clip = dst.clip_rect(target);
    if clip.is_empty() {
        return;
    }
    dst.mark_dirty(clip);
    let stride = dst.width;
    let out = dst.pixels_mut();
    let tw = target.w as usize;
    let th = target.h as usize;

    for py in clip.y..clip.bottom() {
        let ty = (py - target.y) as usize;
        let sy = ((ty * sh) / th).min(sh - 1);
        let srow = &src[sy * sw..(sy + 1) * sw];
        let drow = &mut out[py as usize * stride..py as usize * stride + stride];
        for px in clip.x..clip.right() {
            let tx = (px - target.x) as usize;
            let sx = ((tx * sw) / tw).min(sw - 1);
            let mut spx = srow[sx];
            if opacity < 255 {
                spx = scale_premul(spx, opacity);
            }
            if (spx >> 24) != 0 {
                let slot = &mut drow[px as usize];
                *slot = src_over_premul(0xff00_0000 | *slot, spx) & 0x00ff_ffff;
            }
        }
    }
}

/// Interpolate a window rectangle centered around `center` for DWM open/close zoom tweens.
pub fn scaled_rect_around_center(rect: Rect, scale: f32, dy: i32) -> Rect {
    let w = (rect.w as f32 * scale).round().max(1.0) as i32;
    let h = (rect.h as f32 * scale).round().max(1.0) as i32;
    let x = rect.x + (rect.w - w) / 2;
    let y = rect.y + (rect.h - h) / 2 + dy;
    Rect::new(x, y, w, h)
}

/// Linearly interpolate between two `Rect`s for Minimize/Restore taskbar button fly tweens.
pub fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    let t = t.clamp(0.0, 1.0);
    Rect::new(
        lerp_f32(a.x as f32, b.x as f32, t).round() as i32,
        lerp_f32(a.y as f32, b.y as f32, t).round() as i32,
        lerp_f32(a.w as f32, b.w as f32, t).round().max(8.0) as i32,
        lerp_f32(a.h as f32, b.h as f32, t).round().max(8.0) as i32,
    )
}

/// Render a crystal glass Aero Peek wireframe outline of a window when hidden during peek.
pub fn draw_aero_peek_sheet(dst: &mut Canvas, rect: Rect, accent: u32, opacity: f32) {
    if rect.is_empty() || opacity <= 0.01 {
        return;
    }
    dst.rounded_rect(rect, 6, mix(0xffffff, accent, 0.25), 0.10 * opacity);
    dst.rounded_border(rect, 6, 0xffffff, 0.55 * opacity, 1);
    if rect.w > 4 && rect.h > 4 {
        dst.rounded_border(
            rect.inset(1),
            5,
            mix(accent, 0x000000, 0.3),
            0.35 * opacity,
            1,
        );
    }
}

/// Paint DWM extended-glass backdrop (`blur` + `reflection.png` sheen + color tint) behind a window band.
#[allow(clippy::too_many_arguments)]
pub fn paint_aero_backdrop_band(
    dst: &mut Canvas,
    band: Rect,
    theme: &Theme,
    active: bool,
    blur_scratch: &mut BlurScratch,
) {
    if band.is_empty() {
        return;
    }
    let scr = Rect::new(0, 0, dst.width as i32, dst.height as i32);
    let clip = band.intersect(scr);
    if clip.is_empty() {
        return;
    }
    dst.blur_rect(clip, AERO_BLUR_RADIUS, blur_scratch);
    draw_glass_chrome_overlay(dst, clip, 0, theme, active, clip.x, clip.y, None);
}

/// Apply glass colorization tint + `reflection.png` sheen over an already-blurred glass region.
pub fn finish_shell_glass_overlay(dst: &mut Canvas, rect: Rect, theme: &Theme, sheen_opacity: u8) {
    if rect.is_empty() {
        return;
    }
    let (tint, tint_a) = theme.glass_tint(true);
    dst.fill_u8(rect, tint, tint_a);
    blit_aero_sheen(
        dst,
        rect,
        rect.x,
        rect.y,
        None,
        sheen_opacity as f32 / 255.0,
    );
}

/// Paint Aero glass panel surface behind Taskbar or Shell popup windows (`menu`, `vol`, `ctx`, `toast`).
pub fn paint_shell_glass_region(
    dst: &mut Canvas,
    rect: Rect,
    theme: &Theme,
    blur_scratch: &mut BlurScratch,
    sheen_opacity: u8,
) {
    if rect.is_empty() {
        return;
    }
    dst.blur_rect(rect, AERO_BLUR_RADIUS, blur_scratch);
    finish_shell_glass_overlay(dst, rect, theme, sheen_opacity);
}

/// Composite an in-process shell `Canvas` (carrying premultiplied ARGB or opaque pixels) over `dst`.
pub fn composite_shell_layer(
    dst: &mut Canvas,
    src: &Canvas,
    dx: i32,
    dy: i32,
    opacity: u8,
    argb_blend: bool,
) {
    if opacity == 0 {
        return;
    }
    let sw = src.width;
    let sh = src.height;
    let target = Rect::new(dx, dy, sw as i32, sh as i32);
    let clip = dst.clip_rect(target);
    if clip.is_empty() {
        return;
    }
    dst.mark_dirty(clip);
    let stride = dst.width;
    let d_buf = dst.pixels_mut();
    let s_buf = src.pixels();
    for py in clip.y..clip.bottom() {
        let sy = (py - dy) as usize;
        let sx0 = (clip.x - dx) as usize;
        let cnt = clip.w as usize;
        let srow = &s_buf[sy * sw + sx0..sy * sw + sx0 + cnt];
        let drow = &mut d_buf
            [py as usize * stride + clip.x as usize..py as usize * stride + clip.x as usize + cnt];
        for (d, &s) in drow.iter_mut().zip(srow.iter()) {
            let spx = if argb_blend {
                if opacity < 255 {
                    scale_premul(s, opacity)
                } else {
                    s
                }
            } else {
                let p = premultiply_u8(s & 0x00ff_ffff, opacity);
                if opacity == 255 {
                    0xff00_0000 | (s & 0x00ff_ffff)
                } else {
                    p
                }
            };
            let a = spx >> 24;
            if a == 255 {
                *d = spx & 0x00ff_ffff;
            } else if a != 0 {
                *d = src_over_premul(0xff00_0000 | *d, spx) & 0x00ff_ffff;
            }
        }
    }
}

/// Draw trapezoidal Y-axis-tilted perspective card (Windows 7 Flip 3D core projection).
#[allow(clippy::too_many_arguments)]
pub fn blit_perspective_card(
    dst: &mut Canvas,
    src: &[u32],
    sw: usize,
    sh: usize,
    x0: i32,
    y0_top: i32,
    h_left: i32,
    card_w: i32,
    h_right: i32,
    opacity: u8,
    highlight_color: Option<u32>,
) {
    if card_w <= 2 || h_left <= 2 || h_right <= 2 || opacity == 0 {
        return;
    }
    let cy0 = y0_top as f32 + h_left as f32 * 0.5;
    let inv_w = 1.0 / card_w as f32;
    let dst_w = dst.width as i32;
    let dst_h = dst.height as i32;
    let min_x = x0.max(0);
    let max_x = (x0 + card_w).min(dst_w);
    if min_x >= max_x {
        return;
    }
    dst.mark_dirty(Rect::new(
        min_x,
        (y0_top - 4).max(0),
        max_x - min_x,
        (h_left.max(h_right) + 8).min(dst_h),
    ));

    let stride = dst.width;
    let out = dst.pixels_mut();
    let has_img = sw > 0 && sh > 0 && src.len() >= sw * sh;

    // Soft cast shadow beneath slanted card
    for px in min_x..max_x {
        let u = (px - x0) as f32 * inv_w;
        let col_h = lerp_f32(h_left as f32, h_right as f32, u);
        let sy_bot = (cy0 + col_h * 0.5) as i32;
        for sh_off in 1..=8 {
            let py = sy_bot + sh_off;
            if py >= 0 && py < dst_h {
                let a = ((1.0 - sh_off as f32 / 9.0) * 68.0 * (opacity as f32 / 255.0)) as u8;
                let slot = &mut out[py as usize * stride + px as usize];
                *slot = src_over_u8(*slot, 0x0000_00, a) & 0x00ff_ffff;
            }
        }
    }

    for px in min_x..max_x {
        let u = (px - x0) as f32 * inv_w;
        let col_h = lerp_f32(h_left as f32, h_right as f32, u);
        let col_top = cy0 - col_h * 0.5;
        let col_bot = cy0 + col_h * 0.5;
        let py0 = (col_top.floor() as i32).max(0);
        let py1 = (col_bot.ceil() as i32).min(dst_h);
        if py0 >= py1 {
            continue;
        }
        // Perspective-correct horizontal texture coordinate approximation for YAW tilt
        let depth_w = lerp_f32(1.0, h_right as f32 / h_left as f32, u);
        let u_persp = ((u / (u + (1.0 - u) * depth_w.max(0.1))) * (sw.saturating_sub(1) as f32))
            .clamp(0.0, sw.saturating_sub(1) as f32) as usize;
        let inv_col_h = 1.0 / col_h.max(1.0);
        // Subtle 3D shading toward the recessed right edge
        let shade = (255.0 - u * 36.0) as u8;

        for py in py0..py1 {
            let v = ((py as f32 - col_top) * inv_col_h).clamp(0.0, 1.0);
            let dst_idx = py as usize * stride + px as usize;
            let border_edge = px - x0 < 2
                || (x0 + card_w) - px <= 2
                || (py as f32 - col_top) < 2.0
                || (col_bot - py as f32) < 2.0;

            if border_edge {
                let bcol = highlight_color.unwrap_or(0xc8e4ff);
                let ba = ((opacity as u16 * if highlight_color.is_some() { 235 } else { 150 })
                    / 255) as u8;
                out[dst_idx] = src_over_u8(out[dst_idx], bcol, ba) & 0x00ff_ffff;
                continue;
            }

            let mut px_color = if has_img {
                let sy = ((v * (sh.saturating_sub(1) as f32)) as usize).min(sh - 1);
                let raw = src[sy * sw + u_persp];
                let a = (raw >> 24) & 0xff;
                if a == 0 {
                    premultiply_u8(0x1e2c3e, 195)
                } else if a < 255 {
                    let bg = mix(0x284664, 0x162436, v);
                    src_over_premul(0xff00_0000 | bg, raw)
                } else {
                    raw
                }
            } else {
                0xff00_0000 | mix(0x2d4e70, 0x182a3e, v)
            };

            px_color = scale_premul(0xff00_0000 | (px_color & 0x00ff_ffff), shade) | 0xff00_0000;
            let spx = if opacity < 255 {
                scale_premul(px_color, opacity)
            } else {
                px_color
            };
            if (spx >> 24) != 0 {
                out[dst_idx] = src_over_premul(0xff00_0000 | out[dst_idx], spx) & 0x00ff_ffff;
            }
        }
    }
}

/// Entry descriptor passed into `draw_alt_tab_hud` / `draw_flip3d_scene`.
pub struct SwitcherEntry<'a> {
    pub title: &'a str,
    pub icon: Icon,
    pub pixels: Option<&'a [u32]>,
    pub pw: usize,
    pub ph: usize,
    pub minimized: bool,
}

/// Render the centered Windows 7 Aero Glass Alt+Tab switcher HUD onto `dst`.
pub fn draw_alt_tab_hud(
    dst: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    blur_scratch: &mut BlurScratch,
    entries: &[SwitcherEntry<'_>],
    selected: usize,
) {
    if entries.is_empty() {
        return;
    }
    let sw = dst.width as i32;
    let sh = dst.height as i32;
    let cols = entries.len().clamp(1, 6);
    let rows = entries.len().div_ceil(cols);
    let cell_w = 152;
    let cell_h = 114;
    let pad = 16;
    let hud_w = (cols as i32 * cell_w + pad * 2).min(sw - 32);
    let hud_h = (rows as i32 * cell_h + pad * 2 + 34).min(sh - 48);
    let hud = Rect::new((sw - hud_w) / 2, (sh - hud_h) / 2, hud_w, hud_h);

    dst.rounded_drop_shadow(hud, 10, 18, 6, 0.42);
    paint_shell_glass_region(dst, hud, theme, blur_scratch, 130);
    dst.rounded_border(hud, 10, theme.aero_hover_border(), 0.72, 1);
    dst.rounded_border(hud.inset(1), 9, 0xffffff, 0.40, 1);

    for (idx, ent) in entries.iter().enumerate() {
        let col = (idx % cols) as i32;
        let row = (idx / cols) as i32;
        let cell = Rect::new(
            hud.x + pad + col * cell_w,
            hud.y + pad + row * cell_h,
            cell_w - 8,
            cell_h - 8,
        );
        let is_sel = idx == selected;
        if is_sel {
            dst.rounded_rect(cell, 7, 0xffffff, 0.34);
            dst.rounded_border(cell, 7, theme.accent, 0.92, 2);
            dst.rounded_border(cell.inset(1), 6, 0xffffff, 0.60, 1);
        } else {
            dst.rounded_rect(cell, 6, 0x000000, 0.14);
        }

        let thumb_r = Rect::new(cell.x + 8, cell.y + 8, cell.w - 16, cell.h - 32);
        if let Some(px) = ent
            .pixels
            .filter(|p| !p.is_empty() && ent.pw > 0 && ent.ph > 0)
        {
            let (tw, th) = fit_aspect(ent.pw as i32, ent.ph as i32, thumb_r.w, thumb_r.h);
            let fit = Rect::new(
                thumb_r.x + (thumb_r.w - tw) / 2,
                thumb_r.y + (thumb_r.h - th) / 2,
                tw,
                th,
            );
            blit_scaled_window(
                dst,
                px,
                ent.pw,
                ent.ph,
                fit,
                if ent.minimized { 185 } else { 255 },
            );
        } else {
            dst.rounded_rect(thumb_r, 4, mix(theme.panel, theme.accent, 0.12), 0.75);
            icons::draw(
                dst,
                ent.icon,
                thumb_r.x + (thumb_r.w - 32) / 2,
                thumb_r.y + (thumb_r.h - 32) / 2,
                32,
                None,
            );
        }

        let fg = if theme.is_dark() { 0xffffff } else { 0x101824 };
        icons::draw(dst, ent.icon, cell.x + 8, cell.bottom() - 22, 16, None);
        let label = text.truncate(ent.title, (cell.w - 32) as f32, 11.5, Face::Ui);
        text.draw(
            dst,
            &label,
            (cell.x + 28) as f32,
            (cell.bottom() - 9) as f32,
            11.5,
            fg,
            if is_sel { Face::Bold } else { Face::Ui },
        );
    }

    if let Some(sel_ent) = entries.get(selected) {
        let fg = if theme.is_dark() { 0xf4f8fc } else { 0x0b1626 };
        let shown = text.truncate(sel_ent.title, (hud.w - 48) as f32, 13.0, Face::Bold);
        text.draw_centered_with_glow(
            dst,
            &shown,
            hud.x as f32 + hud.w as f32 * 0.5,
            (hud.bottom() - 15) as f32,
            13.0,
            fg,
            Face::Bold,
            TextGlow::aero_compact(theme.is_dark(), true),
        );
    }
}

/// Render the Windows 7 Flip 3D cascading perspective window switcher over dimmed glass desktop.
pub fn draw_flip3d_scene(
    dst: &mut Canvas,
    desktop_blurred: &[u32],
    text: &mut TextRenderer,
    theme: &Theme,
    entries: &[SwitcherEntry<'_>],
    state: &Flip3dState,
    now: Instant,
) {
    let sw = dst.width as i32;
    let sh = dst.height as i32;
    let open_t =
        smootherstep((now.duration_since(state.opened).as_secs_f32() / 0.22).clamp(0.0, 1.0));
    if desktop_blurred.len() == dst.pixels().len() {
        let out = dst.pixels_mut();
        for (d, &b) in out.iter_mut().zip(desktop_blurred.iter()) {
            *d = mix(*d, mix(b, 0x06101c, 0.45), open_t * 0.88);
        }
        dst.mark_all_dirty();
    } else {
        dst.fill_alpha(Rect::new(0, 0, sw, sh), 0x07101c, 0.55 * open_t);
    }

    if entries.is_empty() {
        return;
    }

    let n = entries.len();
    let max_vis = n.min(7);
    let mut order: Vec<(usize, f32)> = (0..n)
        .map(|i| {
            let mut rel = (i as f32 - state.scroll).rem_euclid(n as f32);
            if rel > n as f32 - 0.5 && (i as f32 - state.scroll).abs() < 1.0 {
                rel -= n as f32;
            }
            (i, rel)
        })
        .collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));

    let base_w = (sw as f32 * 0.44).clamp(320.0, 620.0);
    let base_h = (sh as f32 * 0.54).clamp(220.0, 440.0);

    for (idx, rel) in order {
        if rel < -0.5 || rel >= max_vis as f32 {
            continue;
        }
        let ent = &entries[idx];
        let depth = rel.max(0.0);
        let scale = (1.0 - depth * 0.115).clamp(0.32, 1.0) * lerp_f32(0.84, 1.0, open_t);
        let cw = (base_w * scale) as i32;
        let h_left = (base_h * scale) as i32;
        let h_right = (h_left as f32 * 0.74) as i32;
        let cx = sw as f32 * (0.58 - depth * 0.095);
        let cy = sh as f32 * (0.52 - depth * 0.042);
        let x0 = (cx - cw as f32 * 0.5) as i32;
        let y0 = (cy - h_left as f32 * 0.5) as i32;
        let opacity = (lerp_f32(
            255.0,
            120.0,
            (depth / max_vis.max(1) as f32).clamp(0.0, 1.0),
        ) * open_t)
            .clamp(0.0, 255.0) as u8;
        let hi = if depth < 0.35 {
            Some(theme.accent)
        } else {
            None
        };
        blit_perspective_card(
            dst,
            ent.pixels.unwrap_or(&[]),
            ent.pw,
            ent.ph,
            x0,
            y0,
            h_left,
            cw,
            h_right,
            opacity,
            hi,
        );
    }

    if let Some(front) = entries.get(state.selected) {
        let badge = Rect::new(sw / 2 - 200, sh - 72, 400, 38);
        dst.rounded_rect(badge, 8, 0x0b1624, 0.72 * open_t);
        dst.rounded_border(badge, 8, 0xa8d8ff, 0.45 * open_t, 1);
        icons::draw(dst, front.icon, badge.x + 12, badge.y + 7, 24, None);
        let title = text.truncate(front.title, 336.0, 13.5, Face::Bold);
        text.draw(
            dst,
            &title,
            (badge.x + 44) as f32,
            (badge.y + 24) as f32,
            13.5,
            0xf4f8fc,
            Face::Bold,
        );
    }
}

/// Paint the floating Taskbar Live Thumbnail Preview popup onto `dst`.
pub fn draw_thumb_preview(
    dst: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    blur_scratch: &mut BlurScratch,
    thumb: &ThumbPopupState,
    title: &str,
    icon: Icon,
    pixels: Option<&[u32]>,
    pw: usize,
    ph: usize,
) {
    let r = thumb.rect;
    dst.rounded_drop_shadow(r, 8, 14, 4, 0.38);
    paint_shell_glass_region(dst, r, theme, blur_scratch, 135);
    dst.rounded_border(r, 8, theme.aero_hover_border(), 0.78, 1);
    dst.rounded_border(r.inset(1), 7, 0xffffff, 0.46, 1);

    icons::draw(dst, icon, r.x + 10, r.y + 8, 16, None);
    let fg = if theme.is_dark() { 0xf4f8fc } else { 0x0f1a28 };
    let shown = text.truncate(title, (r.w - 66) as f32, 11.5, Face::Bold);
    let glow = TextGlow::aero_compact(theme.is_dark(), true);
    let title_clip = Rect::new(r.x + 28, r.y + 1, (r.w - 60).max(1), 28);
    dst.with_clip(title_clip, |c| {
        text.draw_glow(c, &shown, (r.x + 32) as f32, (r.y + 20) as f32, 11.5, Face::Bold, glow);
    });
    text.draw(
        dst,
        &shown,
        (r.x + 32) as f32,
        (r.y + 20) as f32,
        11.5,
        fg,
        Face::Bold,
    );

    // Close [X] pill inside thumbnail top-right
    let cr = thumb.close_rect;
    if thumb.close_hover {
        dst.rounded_rect(cr, 4, 0xe84848, 0.92);
    } else {
        dst.rounded_rect(cr, 4, 0x000000, 0.18);
    }
    let x_col = if thumb.close_hover { 0xffffff } else { fg };
    dst.line(
        cr.x + 5,
        cr.y + 5,
        cr.right() - 6,
        cr.bottom() - 6,
        x_col,
        1.0,
    );
    dst.line(
        cr.right() - 6,
        cr.y + 5,
        cr.x + 5,
        cr.bottom() - 6,
        x_col,
        1.0,
    );

    let ir = thumb.img_rect;
    dst.rounded_rect(ir, 4, 0x0a1420, 0.25);
    if let Some(px) = pixels.filter(|p| !p.is_empty() && pw > 0 && ph > 0) {
        let (tw, th) = fit_aspect(pw as i32, ph as i32, ir.w - 4, ir.h - 4);
        let fit = Rect::new(ir.x + (ir.w - tw) / 2, ir.y + (ir.h - th) / 2, tw, th);
        blit_scaled_window(dst, px, pw, ph, fit, 255);
    } else {
        icons::draw(
            dst,
            icon,
            ir.x + (ir.w - 36) / 2,
            ir.y + (ir.h - 36) / 2,
            36,
            None,
        );
    }
    dst.rounded_border(ir, 4, 0xffffff, 0.32, 1);
}

pub fn compute_thumb_popup(
    btn: Rect,
    scr_w: i32,
    taskbar_y: i32,
    panel_top: bool,
    panel_h: i32,
    win_index: usize,
    target_window: Window,
) -> ThumbPopupState {
    let tw = 212;
    let th = 152;
    let x = (btn.x + btn.w / 2 - tw / 2).clamp(8, (scr_w - tw - 8).max(8));
    let y = if panel_top {
        panel_h + 8
    } else {
        (taskbar_y - th - 8).max(8)
    };
    let rect = Rect::new(x, y, tw, th);
    let close_rect = Rect::new(rect.right() - 26, rect.y + 6, 18, 18);
    let img_rect = Rect::new(rect.x + 10, rect.y + 30, rect.w - 20, rect.h - 40);
    ThumbPopupState {
        win_index,
        target_window,
        rect,
        close_rect,
        img_rect,
        pointer_inside: false,
        close_hover: false,
        opened: Instant::now(),
    }
}

pub fn fit_aspect(sw: i32, sh: i32, max_w: i32, max_h: i32) -> (i32, i32) {
    if sw <= 0 || sh <= 0 || max_w <= 0 || max_h <= 0 {
        return (max_w.max(1), max_h.max(1));
    }
    let scale = (max_w as f32 / sw as f32).min(max_h as f32 / sh as f32);
    (
        ((sw as f32 * scale).round() as i32).max(8),
        ((sh as f32 * scale).round() as i32).max(8),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_zones_stay_within_work_area() {
        let l = SnapZone::Left.target_rect(1280, 0, 756).unwrap();
        let r = SnapZone::Right.target_rect(1280, 0, 756).unwrap();
        let m = SnapZone::Maximize.target_rect(1280, 44, 720).unwrap();
        assert!(l.right() < r.x);
        assert!(l.w > 500 && r.w > 500);
        assert_eq!(m.y, 50);
    }

    #[test]
    fn perspective_card_renders_visible_pixels() {
        let mut c = Canvas::new(320, 240, 0x101824);
        let src = vec![0xff48_90d8; 64 * 48];
        blit_perspective_card(
            &mut c,
            &src,
            64,
            48,
            80,
            40,
            120,
            140,
            90,
            255,
            Some(0x5898d8),
        );
        assert_ne!(c.pixels()[100 * 320 + 150], 0x101824);
    }

    #[test]
    fn scaled_rect_preserves_center() {
        let orig = Rect::new(100, 100, 200, 100);
        let half = scaled_rect_around_center(orig, 0.5, 0);
        assert_eq!(half, Rect::new(150, 125, 100, 50));
    }

    #[test]
    fn dirty_region_keeps_disjoint_rects_and_merges_neighbors() {
        let screen = Rect::new(0, 0, 1280, 800);
        let mut reg = DirtyRegion::default();
        reg.mark(Rect::new(20, 20, 60, 40), screen);
        reg.mark(Rect::new(1100, 740, 140, 44), screen);
        assert_eq!(reg.rects.len(), 2);

        // Adjacent rect within MERGE_PAD coalesces with top-left rect only.
        reg.mark(Rect::new(88, 25, 40, 30), screen);
        assert_eq!(reg.rects.len(), 2);

        let mut out = Vec::new();
        reg.take_into(screen, &mut out);
        assert_eq!(out.len(), 2);
        assert!(reg.is_empty());
    }
}
