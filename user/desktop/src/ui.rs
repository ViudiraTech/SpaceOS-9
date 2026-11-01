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

    // Minimize button: crisp 10px horizontal bar.
    let min_rect = caption_button_rect(width, CaptionHit::Minimize, buttons_left);
    if hover == CaptionHit::Minimize {
        canvas.fill(
            Rect::new(min_rect.x, 1, min_rect.w, TITLEBAR_HEIGHT - 2),
            theme.panel_hover,
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

    // Maximize / Restore button: crisp 10x10 square or overlapping 8x8 squares.
    let max_rect = caption_button_rect(width, CaptionHit::Maximize, buttons_left);
    if hover == CaptionHit::Maximize {
        canvas.fill(
            Rect::new(max_rect.x, 1, max_rect.w, TITLEBAR_HEIGHT - 2),
            theme.panel_hover,
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

    // Close button: red hover background with crisp 10x10 'X'.
    let close_rect = caption_button_rect(width, CaptionHit::Close, buttons_left);
    let close_fg = if hover == CaptionHit::Close {
        canvas.fill(
            Rect::new(close_rect.x, 0, close_rect.w, TITLEBAR_HEIGHT - 1),
            theme.danger,
        );
        0xffffff
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
    let base = if primary {
        theme.accent
    } else {
        theme.panel_high
    };
    let bg = if hover {
        if theme.is_dark() {
            mix(base, 0xffffff, 0.10)
        } else {
            mix(base, 0x000000, 0.06)
        }
    } else {
        base
    };
    canvas.rounded_rect(rect, RADIUS_CONTROL, bg, 1.0);
    if !primary {
        canvas.rounded_border(rect, RADIUS_CONTROL, theme.window_border, 1.0, 1);
    }
    let fg = if primary { 0xffffff } else { theme.text };
    let baseline = rect.y as f32 + rect.h as f32 * 0.5 + 4.5;
    if let Some(ic) = leading_icon {
        let label_w = text.measure(label, 12.5, Face::Ui);
        let total_w = 16.0 + if label.is_empty() { 0.0 } else { 6.0 + label_w };
        let start_x = rect.x as f32 + (rect.w as f32 - total_w) * 0.5;
        icons::draw(
            canvas,
            ic,
            start_x.round() as i32,
            rect.y + (rect.h - 16) / 2,
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
            rect.x as f32 + rect.w as f32 * 0.5,
            baseline,
            12.5,
            fg,
            Face::Ui,
        );
    }
}

/// Pill-shaped boolean toggle switch (`38 × 20` recommended) with continuous animation progress `0.0..=1.0`.
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
    let start_x = rect.x + pad;
    let end_x = rect.right() - knob_d - pad;
    let knob_x = (start_x as f32 + (end_x - start_x) as f32 * p).round() as i32;
    let knob_rect = Rect::new(knob_x, rect.y + pad, knob_d, knob_d);
    canvas.rounded_rect(knob_rect, knob_d / 2, 0xffffff, 1.0);
}

/// Pill-shaped boolean toggle switch (`38 × 20` recommended).
pub fn toggle_switch(canvas: &mut Canvas, theme: &Theme, rect: Rect, checked: bool, hover: bool) {
    toggle_switch_anim(canvas, theme, rect, if checked { 1.0 } else { 0.0 }, hover);
}

/// Linear horizontal progress bar with rounded caps.
pub fn progress_bar(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    progress: f32,
    color: Option<u32>,
) {
    let radius = (rect.h / 2).max(1);
    canvas.rounded_rect(rect, radius, theme.window_alt, 1.0);
    canvas.rounded_border(rect, radius, theme.window_border, 1.0, 1);
    let p = progress.clamp(0.0, 1.0);
    let fill_w = ((rect.w as f32) * p).round() as i32;
    if fill_w > 0 {
        let fg = color.unwrap_or(theme.accent);
        let fill_rect = Rect::new(rect.x, rect.y, fill_w.min(rect.w), rect.h);
        canvas.rounded_rect(fill_rect, radius, fg, 1.0);
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

/// Multi-option segmented selector control.
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
    segmented_control_anim(canvas, text, theme, rect, labels, seg, selected, hover_idx);
}

/// Circular color swatch with active ring indicator.
pub fn color_swatch(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    color: u32,
    selected: bool,
    hover: bool,
) {
    let cx = rect.x as f32 + rect.w as f32 * 0.5;
    let cy = rect.y as f32 + rect.h as f32 * 0.5;
    let radius = (rect.w.min(rect.h) as f32 * 0.5 - 3.0).max(4.0);
    canvas.circle(cx, cy, radius, color, 1.0);
    canvas.ring(cx, cy, radius, 1.0, 0x000000, 0.18);
    if selected {
        canvas.ring(cx, cy, radius + 2.2, 1.8, theme.text, 0.92);
        canvas.circle(cx, cy, 3.2, 0xffffff, 0.98);
    } else if hover {
        canvas.ring(cx, cy, radius + 1.8, 1.4, theme.text_muted, 0.75);
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
    if hover && enabled {
        canvas.rounded_rect(rect, RADIUS_CONTROL, theme.panel_hover, 1.0);
    }
    let tint = if enabled {
        theme.text_dim
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
    canvas.rounded_rect(rect, RADIUS_CONTROL, theme.input_bg, 1.0);
    let border = if focused {
        theme.accent
    } else {
        theme.panel_border
    };
    canvas.rounded_border(rect, RADIUS_CONTROL, border, 1.0, 1);

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

pub fn scrollbar_v(
    canvas: &mut Canvas,
    theme: &Theme,
    track: Rect,
    total: usize,
    visible: usize,
    offset: usize,
) {
    if total <= visible || visible == 0 || track.h <= 8 {
        return;
    }
    canvas.fill(track, theme.scrollbar_track);
    let thumb_h = ((track.h as usize * visible) / total)
        .max(24)
        .min(track.h as usize) as i32;
    let max_offset = total - visible;
    let travel = (track.h - thumb_h).max(0);
    let thumb_y = track.y + ((offset.min(max_offset) * travel as usize) / max_offset) as i32;
    let thumb = Rect::new(track.x + 1, thumb_y, (track.w - 2).max(2), thumb_h);
    canvas.rounded_rect(thumb, 3, theme.scrollbar_thumb, 1.0);
}
