/*
 *
 *       user/desktop/src/ui.rs
 *       Native SPUI 4.0 chrome, buttons, switches, segmented controls and scrollbars
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Compact native desktop controls: 34px single-line window titlebar with
//! left- or right-aligned Minimize / Maximize-Restore / Close buttons, flat
//! 1px-bordered surfaces, buttons, switches, segmented controls, color swatches,
//! text inputs and scrollbars.

use crate::draw::{mix, Canvas, Rect};
use crate::icons::{self, Icon};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, RADIUS_CONTROL, TITLEBAR_HEIGHT};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

pub const MOTION_TOGGLE: f32 = 0.18;
pub const MOTION_INDICATOR: f32 = 0.22;
pub const MOTION_SCROLL: f32 = 0.17;
#[allow(dead_code)]
pub const MOTION_HOVER: f32 = 0.12;
pub const SCROLLBAR_HIT_W: i32 = 12;

/// Material/iOS-style decelerating curve: fast response, gentle settle.
#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WidgetId(pub u64);

impl WidgetId {
    pub fn new<T: Hash>(val: T) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        val.hash(&mut hasher);
        Self(hasher.finish())
    }
}

pub trait IntoWidgetId {
    fn into_widget_id(self) -> WidgetId;
}

impl<T: Hash> IntoWidgetId for T {
    fn into_widget_id(self) -> WidgetId {
        WidgetId::new(self)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FloatTween {
    from: f32,
    to: f32,
    start: Instant,
    duration: f32,
}

impl FloatTween {
    pub fn new(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            duration: 0.0,
        }
    }

    pub fn set_target(&mut self, target: f32, now: Instant, duration: f32) {
        if (target - self.to).abs() <= f32::EPSILON {
            return;
        }
        self.from = self.value(now);
        self.to = target;
        self.start = now;
        self.duration = duration;
    }

    pub fn value(&self, now: Instant) -> f32 {
        if self.duration <= 0.0 {
            return self.to;
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / self.duration)
            .clamp(0.0, 1.0);
        self.from + (self.to - self.from) * ease_out_cubic(t)
    }

    pub fn active(&self, now: Instant) -> bool {
        self.duration > 0.0
            && now.saturating_duration_since(self.start).as_secs_f32() < self.duration
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RectTween {
    from: [f32; 4],
    to: [f32; 4],
    start: Instant,
    duration: f32,
}

impl RectTween {
    pub fn new(rect: Rect) -> Self {
        let value = [rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32];
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            duration: 0.0,
        }
    }

    pub fn set_target(&mut self, rect: Rect, now: Instant, duration: f32) {
        let target = [rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32];
        if self.to == target {
            return;
        }
        let current = self.value(now);
        self.from = [
            current.x as f32,
            current.y as f32,
            current.w as f32,
            current.h as f32,
        ];
        self.to = target;
        self.start = now;
        self.duration = duration;
    }

    pub fn value(&self, now: Instant) -> Rect {
        if self.duration <= 0.0 {
            return rect_from_values(self.to);
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / self.duration)
            .clamp(0.0, 1.0);
        let eased = ease_out_cubic(t);
        let mut value = [0.0; 4];
        for index in 0..4 {
            value[index] = self.from[index] + (self.to[index] - self.from[index]) * eased;
        }
        rect_from_values(value)
    }

    pub fn active(&self, now: Instant) -> bool {
        self.duration > 0.0
            && now.saturating_duration_since(self.start).as_secs_f32() < self.duration
    }
}

fn rect_from_values(value: [f32; 4]) -> Rect {
    Rect::new(
        value[0].round() as i32,
        value[1].round() as i32,
        value[2].round() as i32,
        value[3].round() as i32,
    )
}

#[derive(Default)]
pub struct AnimationManager {
    toggles: HashMap<WidgetId, FloatTween>,
    rects: HashMap<WidgetId, RectTween>,
    floats: HashMap<WidgetId, FloatTween>,
    scroll_deadlines: HashMap<WidgetId, Instant>,
}

impl AnimationManager {
    pub fn animate_bool(&mut self, id: WidgetId, target: bool, duration: f32) -> f32 {
        let desired = if target { 1.0 } else { 0.0 };
        let now = Instant::now();
        let tween = self
            .toggles
            .entry(id)
            .or_insert_with(|| FloatTween::new(desired));
        tween.set_target(desired, now, duration);
        tween.value(now)
    }

    pub fn animate_rect(&mut self, id: WidgetId, target: Rect, duration: f32) -> Rect {
        let now = Instant::now();
        let tween = self
            .rects
            .entry(id)
            .or_insert_with(|| RectTween::new(target));
        tween.set_target(target, now, duration);
        tween.value(now)
    }

    pub fn animate_float(&mut self, id: WidgetId, target: f32, duration: f32) -> f32 {
        let now = Instant::now();
        let tween = self
            .floats
            .entry(id)
            .or_insert_with(|| FloatTween::new(target));
        tween.set_target(target, now, duration);
        tween.value(now)
    }

    pub fn keep_scroll_alive(&mut self, id: WidgetId, duration: f32) {
        if duration <= 0.0 {
            self.scroll_deadlines.remove(&id);
        } else {
            self.scroll_deadlines.insert(
                id,
                Instant::now() + Duration::from_secs_f32(duration + 0.02),
            );
        }
    }

    pub fn has_active(&self) -> bool {
        let now = Instant::now();
        self.toggles.values().any(|t| t.active(now))
            || self.rects.values().any(|t| t.active(now))
            || self.floats.values().any(|t| t.active(now))
            || self.scroll_deadlines.values().any(|&until| now < until)
    }
}

thread_local! {
    static ANIM_MGR: std::cell::RefCell<AnimationManager> = std::cell::RefCell::new(AnimationManager::default());
}

pub fn animate_bool(id: impl IntoWidgetId, target: bool, duration: f32) -> f32 {
    let wid = id.into_widget_id();
    ANIM_MGR.with(|m| m.borrow_mut().animate_bool(wid, target, duration))
}

pub fn animate_rect(id: impl IntoWidgetId, target: Rect, duration: f32) -> Rect {
    let wid = id.into_widget_id();
    ANIM_MGR.with(|m| m.borrow_mut().animate_rect(wid, target, duration))
}

pub fn animate_float(id: impl IntoWidgetId, target: f32, duration: f32) -> f32 {
    let wid = id.into_widget_id();
    ANIM_MGR.with(|m| m.borrow_mut().animate_float(wid, target, duration))
}

pub fn has_active_animations() -> bool {
    ANIM_MGR.with(|m| m.borrow().has_active())
}

fn keep_scroll_alive(id: WidgetId, duration: f32) {
    ANIM_MGR.with(|m| m.borrow_mut().keep_scroll_alive(id, duration));
}

pub const CAPTION_BTN_W: i32 = 42;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionHit {
    None,
    Minimize,
    Maximize,
    Close,
}

pub fn caption_button_rect(window_width: i32, kind: CaptionHit, buttons_left: bool) -> Rect {
    if buttons_left {
        match kind {
            CaptionHit::Close => Rect::new(0, 0, CAPTION_BTN_W, TITLEBAR_HEIGHT),
            CaptionHit::Minimize => Rect::new(CAPTION_BTN_W, 0, CAPTION_BTN_W, TITLEBAR_HEIGHT),
            CaptionHit::Maximize => Rect::new(CAPTION_BTN_W * 2, 0, CAPTION_BTN_W, TITLEBAR_HEIGHT),
            CaptionHit::None => Rect::default(),
        }
    } else {
        match kind {
            CaptionHit::Close => Rect::new(
                window_width - CAPTION_BTN_W,
                0,
                CAPTION_BTN_W,
                TITLEBAR_HEIGHT,
            ),
            CaptionHit::Maximize => Rect::new(
                window_width - CAPTION_BTN_W * 2,
                0,
                CAPTION_BTN_W,
                TITLEBAR_HEIGHT,
            ),
            CaptionHit::Minimize => Rect::new(
                window_width - CAPTION_BTN_W * 3,
                0,
                CAPTION_BTN_W,
                TITLEBAR_HEIGHT,
            ),
            CaptionHit::None => Rect::default(),
        }
    }
}

pub fn hit_caption(window_width: i32, x: i32, y: i32, buttons_left: bool) -> CaptionHit {
    if y < 0 || y >= TITLEBAR_HEIGHT {
        return CaptionHit::None;
    }
    for kind in [
        CaptionHit::Close,
        CaptionHit::Maximize,
        CaptionHit::Minimize,
    ] {
        if caption_button_rect(window_width, kind, buttons_left).contains(x, y) {
            return kind;
        }
    }
    CaptionHit::None
}

#[allow(clippy::too_many_arguments)]
pub fn titlebar(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    width: i32,
    app_icon: Icon,
    title: &str,
    active: bool,
    maximized: bool,
    buttons_left: bool,
    hover: CaptionHit,
) {
    let bg = if active {
        theme.titlebar
    } else {
        theme.titlebar_inactive
    };
    canvas.fill(Rect::new(0, 0, width, TITLEBAR_HEIGHT), bg);
    canvas.fill(
        Rect::new(0, TITLEBAR_HEIGHT - 1, width, 1),
        theme.window_border,
    );

    let (icon_x, title_x) = if buttons_left {
        (CAPTION_BTN_W * 3 + 10, (CAPTION_BTN_W * 3 + 33) as f32)
    } else {
        (11, 34.0)
    };
    icons::draw(
        canvas,
        app_icon,
        icon_x,
        (TITLEBAR_HEIGHT - 16) / 2,
        16,
        None,
    );

    let max_title_w = (width - CAPTION_BTN_W * 3 - 48).max(40) as f32;
    let shown = text.truncate(title, max_title_w, 12.5, Face::Ui);
    let title_color = if active { theme.text } else { theme.text_muted };
    text.draw(canvas, &shown, title_x, 22.0, 12.5, title_color, Face::Ui);

    // Minimize button: crisp 10px horizontal bar with animated hover fade.
    let min_rect = caption_button_rect(width, CaptionHit::Minimize, buttons_left);
    let min_hov = animate_bool(min_rect, hover == CaptionHit::Minimize, MOTION_HOVER);
    if min_hov > 0.001 {
        canvas.rounded_rect(
            Rect::new(min_rect.x, 1, min_rect.w, TITLEBAR_HEIGHT - 2),
            0,
            theme.panel_hover,
            min_hov,
        );
    }
    let glyph_fg = if active {
        theme.text_dim
    } else {
        theme.text_muted
    };
    canvas.fill(
        Rect::new(
            min_rect.x + (CAPTION_BTN_W - 10) / 2,
            TITLEBAR_HEIGHT / 2,
            10,
            1,
        ),
        glyph_fg,
    );

    // Maximize / Restore button: crisp 10x10 square with animated hover fade.
    let max_rect = caption_button_rect(width, CaptionHit::Maximize, buttons_left);
    let max_hov = animate_bool(max_rect, hover == CaptionHit::Maximize, MOTION_HOVER);
    if max_hov > 0.001 {
        canvas.rounded_rect(
            Rect::new(max_rect.x, 1, max_rect.w, TITLEBAR_HEIGHT - 2),
            0,
            theme.panel_hover,
            max_hov,
        );
    }
    if maximized {
        let bx = max_rect.x + (CAPTION_BTN_W - 10) / 2;
        let by = (TITLEBAR_HEIGHT - 10) / 2;
        canvas.fill(Rect::new(bx + 2, by, 8, 1), glyph_fg);
        canvas.fill(Rect::new(bx + 9, by, 1, 8), glyph_fg);
        canvas.fill(Rect::new(bx + 2, by, 1, 2), glyph_fg);
        canvas.fill(Rect::new(bx + 8, by + 7, 2, 1), glyph_fg);
        canvas.border_1px(Rect::new(bx, by + 2, 8, 8), glyph_fg);
    } else {
        let sq = Rect::new(
            max_rect.x + (CAPTION_BTN_W - 10) / 2,
            (TITLEBAR_HEIGHT - 10) / 2,
            10,
            10,
        );
        canvas.border_1px(sq, glyph_fg);
    }

    // Close button: red hover background with crisp 10x10 'X' and animated hover fade.
    let close_rect = caption_button_rect(width, CaptionHit::Close, buttons_left);
    let close_hov = animate_bool(close_rect, hover == CaptionHit::Close, MOTION_HOVER);
    if close_hov > 0.001 {
        canvas.rounded_rect(
            Rect::new(close_rect.x, 0, close_rect.w, TITLEBAR_HEIGHT - 1),
            0,
            theme.danger,
            close_hov,
        );
    }
    let close_fg = if close_hov > 0.001 {
        mix(glyph_fg, 0xffffff, close_hov)
    } else {
        glyph_fg
    };
    let cx = close_rect.x + (CAPTION_BTN_W - 10) / 2;
    let cy = (TITLEBAR_HEIGHT - 10) / 2;
    canvas.line(cx, cy, cx + 9, cy + 9, close_fg, 1.0);
    canvas.line(cx + 9, cy, cx, cy + 9, close_fg, 1.0);
}

pub fn window_border(canvas: &mut Canvas, theme: &Theme, active: bool, maximized: bool) {
    if maximized {
        return;
    }
    let color = if active {
        theme.window_border_active
    } else {
        theme.window_border
    };
    let bounds = canvas.bounds();
    canvas.border_1px(bounds, color);
}

pub fn popup_surface(canvas: &mut Canvas, theme: &Theme, rect: Rect, radius: i32) {
    canvas.rounded_rect(rect, radius, theme.panel, 1.0);
    canvas.rounded_border(rect, radius, theme.panel_border, 1.0, 1);
}

pub fn card(canvas: &mut Canvas, theme: &Theme, rect: Rect) {
    canvas.rounded_rect(rect, 6, theme.window_high, 1.0);
    canvas.rounded_border(rect, 6, theme.window_border, 1.0, 1);
}

#[allow(clippy::too_many_arguments)]
pub fn button_anim(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    leading_icon: Option<Icon>,
    primary: bool,
    hover_t: f32,
    pressed: bool,
) {
    let base = if primary {
        theme.accent
    } else {
        theme.panel_high
    };
    let hover_factor = hover_t.clamp(0.0, 1.0);
    let mut bg = if hover_factor > 0.001 {
        if theme.is_dark() {
            mix(base, 0xffffff, 0.12 * hover_factor)
        } else {
            mix(base, 0x000000, 0.08 * hover_factor)
        }
    } else {
        base
    };
    if pressed {
        bg = mix(bg, 0x000000, 0.15);
    }

    let draw_r = if pressed {
        Rect::new(rect.x, rect.y + 1, rect.w, rect.h)
    } else {
        rect
    };

    canvas.rounded_rect(draw_r, RADIUS_CONTROL, bg, 1.0);
    if !primary {
        canvas.rounded_border(draw_r, RADIUS_CONTROL, theme.window_border, 1.0, 1);
    }
    let fg = if primary { 0xffffff } else { theme.text };
    let baseline = draw_r.y as f32 + draw_r.h as f32 * 0.5 + 4.5;
    if let Some(ic) = leading_icon {
        let label_w = text.measure(label, 12.5, Face::Ui);
        let total_w = 16.0 + if label.is_empty() { 0.0 } else { 6.0 + label_w };
        let start_x = draw_r.x as f32 + (draw_r.w as f32 - total_w) * 0.5;
        icons::draw(
            canvas,
            ic,
            start_x.round() as i32,
            draw_r.y + (draw_r.h - 16) / 2,
            16,
            Some(fg),
        );
        if !label.is_empty() {
            text.draw(canvas, label, start_x + 22.0, baseline, 12.5, fg, Face::Ui);
        }
    } else {
        text.draw_centered(
            canvas,
            label,
            draw_r.x as f32 + draw_r.w as f32 * 0.5,
            baseline,
            12.5,
            fg,
            Face::Ui,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub fn button(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    leading_icon: Option<Icon>,
    primary: bool,
    hover: bool,
) {
    let hover_t = animate_bool(rect, hover, MOTION_HOVER);
    button_anim(
        canvas,
        text,
        theme,
        rect,
        label,
        leading_icon,
        primary,
        hover_t,
        false,
    );
}

#[allow(dead_code, clippy::too_many_arguments)]
pub fn button_with_id(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    id: impl IntoWidgetId,
    rect: Rect,
    label: &str,
    leading_icon: Option<Icon>,
    primary: bool,
    hover: bool,
    pressed: bool,
) {
    let hover_t = animate_bool(id, hover, MOTION_HOVER);
    button_anim(
        canvas,
        text,
        theme,
        rect,
        label,
        leading_icon,
        primary,
        hover_t,
        pressed,
    );
}

/// Pill-shaped boolean toggle switch (`38 × 20` to `42 × 22` recommended) with continuous animation progress `0.0..=1.0`.
pub fn toggle_switch_anim(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    progress: f32,
    hover: bool,
) {
    let radius = (rect.h / 2).max(1);
    let p = progress.clamp(0.0, 1.0);
    let off_color = if theme.is_dark() { 0x36363e } else { 0xd4d4dc };
    let mut base = mix(off_color, theme.accent, p);
    if hover {
        base = if theme.is_dark() {
            mix(base, 0xffffff, 0.10)
        } else {
            mix(base, 0x000000, 0.08)
        };
    }
    canvas.rounded_rect(rect, radius, base, 1.0);
    if p < 0.95 {
        let border_alpha = (1.0 - p).clamp(0.0, 1.0);
        canvas.rounded_border(rect, radius, theme.window_border, border_alpha, 1);
    }
    let pad = 2;
    let knob_d = (rect.h - pad * 2).max(6);
    // Tactile squish/stretch during motion
    let stretch = ((0.5 - (p - 0.5).abs()) * 2.0).clamp(0.0, 1.0);
    let knob_w = knob_d + (stretch * 3.0).round() as i32;

    let start_x = rect.x + pad;
    let end_x = rect.right() - knob_w - pad;
    let knob_x = (start_x as f32 + (end_x - start_x) as f32 * p).round() as i32;

    // Soft drop shadow under the knob
    let shadow_rect = Rect::new(knob_x, rect.y + pad + 1, knob_w, knob_d);
    canvas.rounded_rect(shadow_rect, knob_d / 2, 0x000000, 0.24);

    let knob_rect = Rect::new(knob_x, rect.y + pad, knob_w, knob_d);
    canvas.rounded_rect(knob_rect, knob_d / 2, 0xffffff, 1.0);
}

/// Pill-shaped boolean toggle switch (`38 × 20` recommended) with automatic smooth animation.
pub fn toggle_switch(canvas: &mut Canvas, theme: &Theme, rect: Rect, checked: bool, hover: bool) {
    let p = animate_bool(rect, checked, MOTION_TOGGLE);
    toggle_switch_anim(canvas, theme, rect, p, hover);
}

/// Pill-shaped boolean toggle switch with explicit widget ID and automatic smooth animation.
pub fn toggle_switch_with_id(
    canvas: &mut Canvas,
    theme: &Theme,
    id: impl IntoWidgetId,
    rect: Rect,
    checked: bool,
    hover: bool,
) {
    let p = animate_bool(id, checked, MOTION_TOGGLE);
    toggle_switch_anim(canvas, theme, rect, p, hover);
}

/// Linear horizontal progress bar with rounded caps and automatic smooth animation.
pub fn progress_bar(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    progress: f32,
    color: Option<u32>,
) {
    let p_target = progress.clamp(0.0, 1.0);
    let p = animate_float(rect, p_target, 0.22);
    let radius = (rect.h / 2).max(1);
    canvas.rounded_rect(rect, radius, theme.window_alt, 1.0);
    canvas.rounded_border(rect, radius, theme.window_border, 1.0, 1);
    let fill_w = ((rect.w as f32) * p).round() as i32;
    if fill_w > 0 {
        let fg = color.unwrap_or(theme.accent);
        let fill_rect = Rect::new(rect.x, rect.y, fill_w.min(rect.w), rect.h);
        canvas.rounded_rect(fill_rect, radius, fg, 1.0);
    }
}

#[allow(dead_code)]
pub fn progress_bar_with_id(
    canvas: &mut Canvas,
    theme: &Theme,
    id: impl IntoWidgetId,
    rect: Rect,
    progress: f32,
    color: Option<u32>,
) {
    let p_target = progress.clamp(0.0, 1.0);
    let p = animate_float(id, p_target, 0.22);
    let radius = (rect.h / 2).max(1);
    canvas.rounded_rect(rect, radius, theme.window_alt, 1.0);
    canvas.rounded_border(rect, radius, theme.window_border, 1.0, 1);
    let fill_w = ((rect.w as f32) * p).round() as i32;
    if fill_w > 0 {
        let fg = color.unwrap_or(theme.accent);
        let fill_rect = Rect::new(rect.x, rect.y, fill_w.min(rect.w), rect.h);
        canvas.rounded_rect(fill_rect, radius, fg, 1.0);
    }
}

/// Smooth horizontal slider with filled track, animated expandable thumb and glow.
#[allow(dead_code)]
pub fn slider(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    progress: f32,
    hover: bool,
    dragging: bool,
) {
    let p = progress.clamp(0.0, 1.0);
    let hover_t = animate_bool(rect, hover || dragging, 0.12);
    let track_h = 4;
    let track_y = rect.y + (rect.h - track_h) / 2;
    let track_rect = Rect::new(rect.x, track_y, rect.w, track_h);
    canvas.rounded_rect(track_rect, 2, theme.window_alt, 1.0);
    canvas.rounded_border(track_rect, 2, theme.window_border, 1.0, 1);

    let active_w = ((rect.w as f32) * p).round() as i32;
    if active_w > 0 {
        let active_r = Rect::new(rect.x, track_y, active_w, track_h);
        canvas.rounded_rect(active_r, 2, theme.accent, 1.0);
    }

    let thumb_d = (14.0 + if dragging { 4.0 } else { 2.0 * hover_t }).round() as i32;
    let thumb_x = (rect.x as f32 + (rect.w as f32) * p).round() as i32 - thumb_d / 2;
    let thumb_y = rect.y + (rect.h - thumb_d) / 2;
    let thumb_r = Rect::new(thumb_x, thumb_y, thumb_d, thumb_d);
    // Drop shadow
    canvas.rounded_rect(thumb_r.offset(0, 1), thumb_d / 2, 0x000000, 0.25);
    canvas.rounded_rect(thumb_r, thumb_d / 2, 0xffffff, 1.0);
    if hover_t > 0.001 {
        canvas.rounded_border(thumb_r, thumb_d / 2, theme.accent, 0.60 * hover_t, 2);
    }
}

/// Compute the hit rectangle of `index` inside a `count`-option `segmented_control`.
pub fn segmented_item_rect(rect: Rect, count: usize, index: usize) -> Rect {
    if count == 0 || index >= count {
        return Rect::default();
    }
    let inner = rect.inset(2);
    let x0 = inner.x + ((inner.w * index as i32) / count as i32);
    let x1 = if index + 1 == count {
        inner.right()
    } else {
        inner.x + ((inner.w * (index + 1) as i32) / count as i32)
    };
    Rect::new(x0, inner.y, (x1 - x0).max(1), inner.h.max(1))
}

/// Multi-option segmented selector control with animated indicator slider.
pub fn segmented_control_anim(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    labels: &[&str],
    indicator_rect: Rect,
    selected: usize,
    hover_idx: Option<usize>,
) {
    canvas.rounded_rect(rect, RADIUS_CONTROL + 1, theme.window_alt, 1.0);
    canvas.rounded_border(rect, RADIUS_CONTROL + 1, theme.window_border, 1.0, 1);
    if !indicator_rect.is_empty() {
        // Drop shadow for the sliding pill
        canvas.rounded_rect(indicator_rect.offset(0, 1), RADIUS_CONTROL, 0x000000, 0.22);
        canvas.rounded_rect(indicator_rect, RADIUS_CONTROL, theme.accent, 1.0);
    }
    let baseline = rect.y as f32 + rect.h as f32 * 0.5 + 4.3;
    for (i, label) in labels.iter().enumerate() {
        let seg = segmented_item_rect(rect, labels.len(), i);
        let is_sel = i == selected;
        if hover_idx == Some(i) && !is_sel {
            canvas.rounded_rect(seg, RADIUS_CONTROL, theme.panel_hover, 1.0);
        }
        let fg = if is_sel { 0xffffff } else { theme.text_dim };
        let face = if is_sel { Face::Bold } else { Face::Ui };
        text.draw_centered(
            canvas,
            label,
            seg.x as f32 + seg.w as f32 * 0.5,
            baseline,
            12.0,
            fg,
            face,
        );
    }
}

/// Multi-option segmented selector control with automatic smooth animation.
pub fn segmented_control(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    labels: &[&str],
    selected: usize,
    hover_idx: Option<usize>,
) {
    let seg = segmented_item_rect(rect, labels.len(), selected);
    let ind_rect = animate_rect(rect, seg, MOTION_INDICATOR);
    segmented_control_anim(
        canvas, text, theme, rect, labels, ind_rect, selected, hover_idx,
    );
}

/// Multi-option segmented selector control with explicit widget ID and automatic smooth animation.
pub fn segmented_control_with_id(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    id: impl IntoWidgetId,
    rect: Rect,
    labels: &[&str],
    selected: usize,
    hover_idx: Option<usize>,
) {
    let seg = segmented_item_rect(rect, labels.len(), selected);
    let ind_rect = animate_rect(id, seg, MOTION_INDICATOR);
    segmented_control_anim(
        canvas, text, theme, rect, labels, ind_rect, selected, hover_idx,
    );
}

/// Circular color swatch with animated active ring indicator.
pub fn color_swatch(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    color: u32,
    selected: bool,
    hover: bool,
) {
    let sel_t = animate_bool(rect, selected, 0.18);
    let hov_t = animate_bool(rect.offset(1, 1), hover, 0.12);
    let cx = rect.x as f32 + rect.w as f32 * 0.5;
    let cy = rect.y as f32 + rect.h as f32 * 0.5;
    let radius = (rect.w.min(rect.h) as f32 * 0.5 - 3.0).max(4.0);
    canvas.circle(cx, cy, radius, color, 1.0);
    canvas.ring(cx, cy, radius, 1.0, 0x000000, 0.18);
    if sel_t > 0.001 {
        canvas.ring(
            cx,
            cy,
            radius + 2.2 * sel_t,
            1.8 * sel_t,
            theme.text,
            0.92 * sel_t,
        );
        canvas.circle(cx, cy, 3.2 * sel_t, 0xffffff, 0.98 * sel_t);
    } else if hov_t > 0.001 {
        canvas.ring(
            cx,
            cy,
            radius + 1.8 * hov_t,
            1.4 * hov_t,
            theme.text_muted,
            0.75 * hov_t,
        );
    }
}

pub fn icon_button(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    icon: Icon,
    enabled: bool,
    hover: bool,
) {
    let hover_t = animate_bool(rect, hover && enabled, MOTION_HOVER);
    if hover_t > 0.001 {
        canvas.rounded_rect(rect, RADIUS_CONTROL, theme.panel_hover, hover_t);
    }
    let tint = if enabled {
        if hover_t > 0.001 {
            mix(theme.text_dim, theme.text, hover_t)
        } else {
            theme.text_dim
        }
    } else {
        theme.text_muted
    };
    icons::draw(
        canvas,
        icon,
        rect.x + (rect.w - 16) / 2,
        rect.y + (rect.h - 16) / 2,
        16,
        Some(tint),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn text_input(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    value: &str,
    placeholder: &str,
    focused: bool,
    leading_icon: Option<Icon>,
) {
    let focus_t = animate_bool(rect, focused, 0.15);
    canvas.rounded_rect(rect, RADIUS_CONTROL, theme.input_bg, 1.0);
    let border = if focus_t > 0.001 {
        mix(theme.panel_border, theme.accent, focus_t)
    } else {
        theme.panel_border
    };
    canvas.rounded_border(rect, RADIUS_CONTROL, border, 1.0, 1);
    if focus_t > 0.001 {
        canvas.rounded_border(
            rect.inset(-2),
            RADIUS_CONTROL + 2,
            theme.accent,
            0.28 * focus_t,
            2,
        );
    }

    let mut text_x = rect.x + 10;
    if let Some(ic) = leading_icon {
        icons::draw(
            canvas,
            ic,
            rect.x + 9,
            rect.y + (rect.h - 16) / 2,
            16,
            Some(theme.text_muted),
        );
        text_x = rect.x + 31;
    }

    let baseline = rect.y as f32 + rect.h as f32 * 0.5 + 4.5;
    let inner_clip = Rect::new(
        text_x,
        rect.y + 2,
        (rect.right() - text_x - 8).max(0),
        rect.h - 4,
    );
    canvas.with_clip(inner_clip, |c| {
        if value.is_empty() {
            text.draw(
                c,
                placeholder,
                text_x as f32,
                baseline,
                12.5,
                theme.text_muted,
                Face::Ui,
            );
            if focused {
                c.fill(
                    Rect::new(text_x, rect.y + (rect.h - 16) / 2, 1, 16),
                    theme.accent,
                );
            }
        } else {
            let adv = text.draw(
                c,
                value,
                text_x as f32,
                baseline,
                12.5,
                theme.text,
                Face::Ui,
            );
            if focused {
                c.fill(
                    Rect::new(
                        (text_x as f32 + adv + 1.0) as i32,
                        rect.y + (rect.h - 16) / 2,
                        1,
                        16,
                    ),
                    theme.accent,
                );
            }
        }
    });
}

/// Smooth inertial & interactive vertical scroll state shared across scrollable apps.
#[derive(Clone, Debug)]
pub struct SmoothScroll {
    id: WidgetId,
    tween: FloatTween,
    max_offset: f32,
    viewport_len: f32,
    total_len: f32,
    last_track: Rect,
    dragging: bool,
    drag_grab_off: i32,
    hovered: bool,
    last_wheel_at: Option<Instant>,
}

impl SmoothScroll {
    pub fn new(id: impl IntoWidgetId) -> Self {
        Self {
            id: id.into_widget_id(),
            tween: FloatTween::new(0.0),
            max_offset: 0.0,
            viewport_len: 1.0,
            total_len: 1.0,
            last_track: Rect::default(),
            dragging: false,
            drag_grab_off: 0,
            hovered: false,
            last_wheel_at: None,
        }
    }

    pub fn offset(&self) -> f32 {
        self.tween.value(Instant::now()).clamp(0.0, self.max_offset)
    }

    pub fn offset_i32(&self) -> i32 {
        self.offset().round() as i32
    }

    pub fn target(&self) -> f32 {
        self.tween.to.clamp(0.0, self.max_offset)
    }

    pub fn max_offset(&self) -> f32 {
        self.max_offset
    }

    pub fn is_animating(&self) -> bool {
        self.tween.active(Instant::now())
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    pub fn set_bounds(&mut self, viewport_len: f32, total_len: f32, track: Rect) {
        self.viewport_len = viewport_len.max(1.0);
        self.total_len = total_len.max(self.viewport_len);
        self.max_offset = (self.total_len - self.viewport_len).max(0.0);
        self.last_track = track;
        if self.tween.to > self.max_offset {
            self.tween.to = self.max_offset;
        }
        if self.tween.from > self.max_offset {
            self.tween.from = self.max_offset;
        }
        if self.max_offset <= 0.0 {
            self.dragging = false;
            self.hovered = false;
        }
    }

    pub fn snap_to(&mut self, offset: f32) {
        let clamped = offset.clamp(0.0, self.max_offset.max(offset.max(0.0)));
        let now = Instant::now();
        self.tween.from = clamped;
        self.tween.to = clamped;
        self.tween.start = now;
        self.tween.duration = 0.0;
        keep_scroll_alive(self.id, 0.0);
    }

    pub fn animate_to(&mut self, target: f32, duration: f32) {
        let clamped = target.clamp(0.0, self.max_offset);
        let now = Instant::now();
        self.tween.set_target(clamped, now, duration);
        keep_scroll_alive(self.id, duration);
    }

    pub fn wheel(&mut self, delta_px: f32) -> bool {
        if self.max_offset <= 0.0 || delta_px == 0.0 {
            return false;
        }
        let now = Instant::now();
        let boost = if self
            .last_wheel_at
            .is_some_and(|prev| now.saturating_duration_since(prev) < Duration::from_millis(85))
        {
            1.22
        } else {
            1.0
        };
        self.last_wheel_at = Some(now);
        let current_target = self.tween.to.clamp(0.0, self.max_offset);
        let next = (current_target + delta_px * boost).clamp(0.0, self.max_offset);
        if (next - self.offset()).abs() < 0.25 && (next - current_target).abs() < 0.25 {
            return false;
        }
        self.tween.set_target(next, now, MOTION_SCROLL);
        keep_scroll_alive(self.id, MOTION_SCROLL);
        true
    }

    fn current_thumb_rect(&self) -> Option<Rect> {
        scrollbar_thumb_rect_f32(
            self.last_track,
            self.total_len,
            self.viewport_len,
            self.offset(),
        )
    }

    pub fn handle_button(&mut self, x: i32, y: i32, button: u8, down: bool) -> bool {
        if button != 1 {
            return false;
        }
        if !down {
            if self.dragging {
                self.dragging = false;
                return true;
            }
            return false;
        }
        if self.max_offset <= 0.0 || !self.last_track.contains(x, y) {
            return false;
        }
        if let Some(thumb) = self.current_thumb_rect() {
            if y >= thumb.y && y < thumb.bottom() {
                let cur = self.offset();
                self.snap_to(cur);
                self.dragging = true;
                self.drag_grab_off = (y - thumb.y).clamp(0, thumb.h.max(1));
                return true;
            }
            let page_step = (self.viewport_len * 0.85).max(36.0);
            let dir = if y < thumb.y { -1.0 } else { 1.0 };
            self.animate_to(self.target() + dir * page_step, MOTION_SCROLL);
            return true;
        }
        false
    }

    pub fn handle_motion(&mut self, x: i32, y: i32) -> bool {
        if self.dragging {
            if let Some(thumb) = self.current_thumb_rect() {
                let travel = (self.last_track.h - thumb.h).max(1) as f32;
                let desired_y = (y - self.drag_grab_off - self.last_track.y) as f32;
                let frac = (desired_y / travel).clamp(0.0, 1.0);
                let next = frac * self.max_offset;
                if (next - self.offset()).abs() >= 0.25 {
                    self.snap_to(next);
                    return true;
                }
            }
            return true;
        }
        let hov = self.max_offset > 0.0 && self.last_track.contains(x, y);
        if hov != self.hovered {
            self.hovered = hov;
            return true;
        }
        false
    }

    pub fn draw_v(
        &mut self,
        canvas: &mut Canvas,
        theme: &Theme,
        track: Rect,
        viewport: f32,
        total: f32,
    ) {
        self.set_bounds(viewport, total, track);
        if total <= viewport || viewport <= 0.0 || track.h <= 8 {
            return;
        }
        scrollbar_v_animated(
            canvas,
            theme,
            self.id,
            track,
            total,
            viewport,
            self.offset(),
            self.hovered,
            self.dragging,
            self.is_animating(),
        );
    }
}

fn scrollbar_thumb_rect_f32(track: Rect, total: f32, visible: f32, offset: f32) -> Option<Rect> {
    if total <= visible || visible <= 0.0 || track.h <= 8 {
        return None;
    }
    let thumb_h = ((track.h as f32 * visible) / total)
        .max(28.0)
        .min(track.h as f32);
    let max_offset = (total - visible).max(1.0);
    let travel = (track.h as f32 - thumb_h).max(0.0);
    let frac = (offset.clamp(0.0, max_offset)) / max_offset;
    let thumb_y = track.y + (frac * travel).round() as i32;
    Some(Rect::new(track.x, thumb_y, track.w, thumb_h.round() as i32))
}

#[allow(clippy::too_many_arguments)]
pub fn scrollbar_v_animated(
    canvas: &mut Canvas,
    theme: &Theme,
    id: impl IntoWidgetId,
    track: Rect,
    total: f32,
    visible: f32,
    offset: f32,
    hovered: bool,
    dragging: bool,
    scrolling: bool,
) {
    let Some(base_thumb) = scrollbar_thumb_rect_f32(track, total, visible, offset) else {
        return;
    };
    let expand_t = animate_bool(id, hovered || dragging || scrolling, 0.14);
    let track_alpha = 0.20 + 0.65 * expand_t;
    let track_pill = Rect::new(
        (track.right() - (4.0 + 4.0 * expand_t).round() as i32 - 3).max(track.x),
        track.y,
        ((4.0 + 4.0 * expand_t).round() as i32 + 2).min(track.w),
        track.h,
    );
    canvas.rounded_rect(
        track_pill,
        (track_pill.w / 2).max(2),
        theme.scrollbar_track,
        track_alpha,
    );

    let thumb_w = (4.0 + (track.w - 6).clamp(3, 5) as f32 * expand_t)
        .round()
        .clamp(4.0, (track.w - 2).max(4) as f32) as i32;
    let thumb_x = track.right() - 2 - thumb_w;
    let thumb = Rect::new(thumb_x, base_thumb.y, thumb_w, base_thumb.h);
    let thumb_color = if dragging {
        mix(theme.scrollbar_thumb, theme.accent, 0.65)
    } else {
        mix(theme.scrollbar_thumb, theme.text, 0.28 * expand_t)
    };
    canvas.rounded_rect(thumb, (thumb_w / 2).max(2), thumb_color, 1.0);
}

#[allow(dead_code)]
pub fn scrollbar_v(
    canvas: &mut Canvas,
    theme: &Theme,
    track: Rect,
    total: usize,
    visible: usize,
    offset: usize,
) {
    scrollbar_v_animated(
        canvas,
        theme,
        track,
        track,
        total as f32,
        visible as f32,
        offset as f32,
        false,
        false,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ease_out_cubic() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        let mid = ease_out_cubic(0.5);
        assert!(
            mid > 0.5,
            "Ease-out cubic should accelerate early: mid={}",
            mid
        );
        assert!((mid - 0.875).abs() < 1e-4);
    }

    #[test]
    fn test_widget_id() {
        let id1 = "test_toggle".into_widget_id();
        let id2 = "test_toggle".into_widget_id();
        let id3 = "other_toggle".into_widget_id();
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);

        let r_id1 = Rect::new(10, 20, 30, 40).into_widget_id();
        let r_id2 = Rect::new(10, 20, 30, 40).into_widget_id();
        assert_eq!(r_id1, r_id2);
    }

    #[test]
    fn test_animate_bool() {
        let p0 = animate_bool("unit_test_toggle", false, 0.2);
        assert_eq!(p0, 0.0);
        let p1 = animate_bool("unit_test_toggle", true, 0.2);
        assert!(p1 >= 0.0 && p1 <= 1.0);
        assert!(has_active_animations());
    }

    #[test]
    fn test_animate_rect() {
        let r0 = Rect::new(0, 0, 100, 30);
        let r1 = Rect::new(50, 0, 100, 30);
        let out0 = animate_rect("unit_test_rect", r0, 0.2);
        assert_eq!(out0, r0);
        let out1 = animate_rect("unit_test_rect", r1, 0.2);
        assert_eq!(out1.y, 0);
        assert_eq!(out1.w, 100);
        assert_eq!(out1.h, 30);
    }

    #[test]
    fn test_smooth_scroll_wheel_track_and_drag() {
        let mut s = SmoothScroll::new("test_smooth_scroll");
        let track = Rect::new(200, 50, 12, 300);
        s.set_bounds(300.0, 1200.0, track);
        assert_eq!(s.max_offset(), 900.0);

        // Wheel clamps cleanly and triggers active animation.
        assert!(s.wheel(120.0));
        assert!(s.target() > 0.0 && s.target() <= 900.0);
        assert!(s.is_animating());

        // Over-scrolling down clamps cleanly to max_offset.
        assert!(s.wheel(5000.0));
        assert_eq!(s.target(), 900.0);
        s.snap_to(0.0);
        assert_eq!(s.offset(), 0.0);
        assert!(!s.is_animating());

        // Track click below thumb pages smoothly downward.
        assert!(s.handle_button(206, 260, 1, true));
        assert!(s.target() > 200.0);
        s.snap_to(0.0);

        // Grab thumb near top and drag halfway down -> maps 1:1 immediately.
        assert!(s.handle_button(206, 60, 1, true));
        assert!(s.is_dragging());
        let drag_to_y = 60 + 112; // ~half of travel
        assert!(s.handle_motion(160, drag_to_y)); // off-axis X remains captured
        assert!(s.offset() > 350.0 && s.offset() < 550.0);
        assert!(!s.is_animating());
        assert!(s.handle_button(160, drag_to_y, 1, false));
        assert!(!s.is_dragging());
    }
}
