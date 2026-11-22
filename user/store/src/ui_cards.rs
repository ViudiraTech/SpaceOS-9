//
//       user/store/src/ui_cards.rs
//       Reusable card and chrome primitives for Space 商店
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//
// Every surface here is built from spui_ui primitives and the active theme;
// nothing inspects which skin is loaded.
//

use spui_ui::draw::{Canvas, Rect, Sprite};
use spui_ui::icons::{self, Icon};
use spui_ui::text::{Face, TextRenderer};
use spui_ui::theme::Theme;
use spui_ui::ui;

use crate::curated::{HeroSlide, PackagePresentation};
use crate::model::{format_bytes, StorePackage};

pub const SIDEBAR_W: i32 = 224;
pub const TOPBAR_H: i32 = 56;
pub const CARD_H: i32 = 152;
pub const FEATURED_CARD_H: i32 = 220;

pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let av = ((a >> shift) & 0xff) as f32;
        let bv = ((b >> shift) & 0xff) as f32;
        ((av + (bv - av) * t).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

pub fn luminance(color: u32) -> f32 {
    let r = ((color >> 16) & 0xff) as f32 / 255.0;
    let g = ((color >> 8) & 0xff) as f32 / 255.0;
    let b = (color & 0xff) as f32 / 255.0;
    0.299 * r + 0.587 * g + 0.114 * b
}

pub fn contrast_text(bg: u32) -> u32 {
    if luminance(bg) > 0.60 {
        0x0f172a
    } else {
        0xffffff
    }
}

pub fn badge_text_color(theme: &Theme, accent: u32) -> u32 {
    if luminance(theme.window_high) > 0.52 {
        mix(accent, 0x0f172a, 0.34)
    } else {
        mix(accent, 0xffffff, 0.38)
    }
}

/// Subtle 1px hairline border color for elevated cards and grouped sheets.
pub fn soft_border(theme: &Theme) -> u32 {
    mix(theme.panel_border, theme.window_high, 0.32)
}

/// Modern elevated card surface (`theme.window_high`) with soft shadow and hairline border.
pub fn elevated_card(canvas: &mut Canvas, theme: &Theme, rect: Rect, hovered: bool) {
    if rect.is_empty() {
        return;
    }
    let radius = theme.radius_control().max(10);
    canvas.rounded_drop_shadow(rect, radius, 10, 2, if hovered { 0.11 } else { 0.05 });
    let fill = if hovered {
        mix(theme.window_high, theme.panel_hover, 0.26)
    } else {
        theme.window_high
    };
    canvas.rounded_rect(rect, radius, fill, 1.0);
    let border = if hovered {
        mix(theme.accent, theme.panel_border, 0.40)
    } else {
        soft_border(theme)
    };
    canvas.rounded_border(rect, radius, border, 0.95, 1);
}

/// Outer rounded container for a Libadwaita / Mac App Store style PreferencesGroup.
pub fn group_container(canvas: &mut Canvas, theme: &Theme, rect: Rect) {
    if rect.is_empty() {
        return;
    }
    let radius = theme.radius_control().max(10);
    canvas.rounded_drop_shadow(rect, radius, 10, 2, 0.05);
    canvas.rounded_rect(rect, radius, theme.window_high, 1.0);
    canvas.rounded_border(rect, radius, soft_border(theme), 0.95, 1);
}

/// Group section header with title, item count pill, and right-aligned description.
pub fn group_section_header(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    title: &str,
    count: usize,
    subtitle: &str,
) {
    draw_text_clipped(
        text,
        canvas,
        title,
        rect.x + 2,
        rect.y + 22,
        240,
        14.0,
        theme.text,
        Face::Bold,
    );
    let title_w = text.measure(title, 14.0, Face::Bold) as i32;
    let count_str = count.to_string();
    let pill_w = (text.measure(&count_str, 11.0, Face::Bold) as i32 + 16).max(24);
    let pill = Rect::new(rect.x + title_w + 10, rect.y + 7, pill_w, 19);
    badge(canvas, text, theme, pill, &count_str, theme.accent);

    if !subtitle.is_empty() && rect.w >= 420 {
        let sub_x = pill.right() + 12;
        let avail = (rect.right() - sub_x - 4).max(40);
        draw_text_clipped(
            text,
            canvas,
            subtitle,
            sub_x,
            rect.y + 21,
            avail,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
    }
}

/// Recessed track for a modern segmented filter bar.
pub fn segmented_track(canvas: &mut Canvas, theme: &Theme, rect: Rect) {
    if rect.is_empty() {
        return;
    }
    let radius = (rect.h / 2).min(theme.radius_control().max(8));
    let bg = mix(theme.window_alt, theme.panel_border, 0.16);
    canvas.rounded_rect(rect, radius, bg, 1.0);
    canvas.rounded_border(rect, radius, soft_border(theme), 0.85, 1);
}

/// Single segment button inside a `segmented_track`.
pub fn segmented_pill(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    count: Option<usize>,
    selected: bool,
    hovered: bool,
) {
    if rect.is_empty() {
        return;
    }
    let radius = (rect.h / 2).min(theme.radius_control().max(6));
    if selected {
        canvas.rounded_drop_shadow(rect, radius, 6, 1, 0.08);
        canvas.rounded_rect(rect, radius, theme.window_high, 1.0);
        canvas.rounded_border(
            rect,
            radius,
            mix(theme.accent, theme.panel_border, 0.45),
            0.95,
            1,
        );
    } else if hovered {
        canvas.rounded_rect(
            rect,
            radius,
            mix(theme.window_high, theme.window_alt, 0.50),
            0.75,
        );
    }

    let fg = if selected {
        theme.text
    } else if hovered {
        theme.text
    } else {
        theme.text_dim
    };
    let face = if selected { Face::Bold } else { Face::Ui };

    if let Some(c) = count {
        let c_str = c.to_string();
        let chip_w = (text.measure(&c_str, 10.5, Face::Bold) as i32 + 12).max(20);
        let lw = text.measure(label, 12.0, face) as i32;
        let total_w = lw + 6 + chip_w;
        let start_x = rect.x + (rect.w - total_w) / 2;
        text.draw(
            canvas,
            label,
            start_x as f32,
            (rect.y + (rect.h + 8) / 2) as f32,
            12.0,
            fg,
            face,
        );
        let chip = Rect::new(start_x + lw + 6, rect.y + (rect.h - 17) / 2, chip_w, 17);
        let chip_bg = if selected {
            mix(theme.window_high, theme.accent, 0.16)
        } else {
            mix(theme.window_alt, theme.panel_border, 0.25)
        };
        let chip_fg = if selected {
            badge_text_color(theme, theme.accent)
        } else {
            theme.text_muted
        };
        canvas.rounded_rect(chip, 8, chip_bg, 1.0);
        let cw = text.measure(&c_str, 10.5, Face::Bold);
        text.draw(
            canvas,
            &c_str,
            (chip.x + chip.w / 2) as f32 - cw * 0.5,
            (chip.y + 12) as f32,
            10.5,
            chip_fg,
            Face::Bold,
        );
    } else {
        let lw = text.measure(label, 12.0, face);
        text.draw(
            canvas,
            label,
            (rect.x + rect.w / 2) as f32 - lw * 0.5,
            (rect.y + (rect.h + 8) / 2) as f32,
            12.0,
            fg,
            face,
        );
    }
}

/// Rounded gradient app icon, the visual anchor of every card.
pub fn icon_badge(canvas: &mut Canvas, icon: Icon, rect: Rect, accent: [u32; 2]) {
    if rect.w <= 2 || rect.h <= 2 {
        return;
    }
    let radius = (rect.w.min(rect.h) / 4).max(6);
    canvas.rounded_gradient(rect, radius, accent[0], accent[1], 1.0);
    let gloss = Rect::new(rect.x + 2, rect.y + 2, rect.w - 4, (rect.h / 2 - 2).max(2));
    canvas.rounded_rect(gloss, (radius - 2).max(2), mix(accent[0], 0xffffff, 0.55), 0.18);
    let glyph = (rect.w.min(rect.h) * 56 / 100).max(12);
    icons::draw(
        canvas,
        icon,
        rect.x + (rect.w - glyph) / 2,
        rect.y + (rect.h - glyph) / 2,
        glyph as u32,
        Some(0xffffff),
    );
}

pub fn draw_text(
    text: &mut TextRenderer,
    canvas: &mut Canvas,
    value: &str,
    x: i32,
    baseline: i32,
    px: f32,
    color: u32,
    face: Face,
) {
    text.draw(canvas, value, x as f32, baseline as f32, px, color, face);
}

pub fn draw_text_clipped(
    text: &mut TextRenderer,
    canvas: &mut Canvas,
    value: &str,
    x: i32,
    baseline: i32,
    max_width: i32,
    px: f32,
    color: u32,
    face: Face,
) {
    if max_width <= 4 {
        return;
    }
    let clipped = text.truncate(value, max_width as f32, px, face);
    text.draw(canvas, &clipped, x as f32, baseline as f32, px, color, face);
}

/// Wraps `value` into at most `lines` lines of `max_width` pixels, appending
/// an ellipsis only when the text actually overflows.
pub fn wrap_text(
    text: &mut TextRenderer,
    value: &str,
    max_width: f32,
    px: f32,
    face: Face,
    lines: usize,
) -> Vec<String> {
    if lines == 0 || max_width <= 4.0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut truncated = false;
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\n' {
            out.push(std::mem::take(&mut current));
            if out.len() == lines {
                if chars.peek().is_some() {
                    truncated = true;
                }
                break;
            }
            continue;
        }
        let mut probe = current.clone();
        probe.push(ch);
        if text.measure(&probe, px, face) > max_width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            if out.len() == lines {
                truncated = true;
                break;
            }
        }
        current.push(ch);
    }

    if out.len() < lines && (!current.is_empty() || out.is_empty()) {
        if !current.is_empty() {
            out.push(current);
        }
    } else if !current.is_empty() {
        truncated = true;
    }

    if truncated {
        if let Some(last) = out.last_mut() {
            while !last.is_empty() {
                let candidate = format!("{last}…");
                if text.measure(&candidate, px, face) <= max_width {
                    *last = candidate;
                    return out;
                }
                last.pop();
            }
            *last = "…".to_string();
        }
    }
    out
}

/// Draw an aspect-cover, rounded-corner bitmap sprite on `canvas`.
pub fn blit_sprite_cover_rounded(canvas: &mut Canvas, rect: Rect, radius: i32, sprite: &Sprite) {
    if rect.w <= 0 || rect.h <= 0 || sprite.width == 0 || sprite.height == 0 {
        return;
    }
    let dw = rect.w as usize;
    let dh = rect.h as usize;
    let mut scaled = sprite.scale_cover_opaque(dw, dh);
    let r = radius.max(0).min(rect.w / 2).min(rect.h / 2) as f32;
    let r2 = r * r;
    for py in 0..dh {
        let dy = if (py as f32) < r {
            r - (py as f32 + 0.5)
        } else if (py as f32) >= dh as f32 - r {
            (py as f32 + 0.5) - (dh as f32 - r)
        } else {
            0.0
        };
        let row_off = py * dw;
        for px in 0..dw {
            let dx = if (px as f32) < r {
                r - (px as f32 + 0.5)
            } else if (px as f32) >= dw as f32 - r {
                (px as f32 + 0.5) - (dw as f32 - r)
            } else {
                0.0
            };
            let rgb = scaled[row_off + px] & 0x00ff_ffff;
            let a = if dx > 0.0 && dy > 0.0 {
                let d2 = dx * dx + dy * dy;
                if d2 > r2 {
                    0u32
                } else {
                    255u32
                }
            } else {
                255u32
            };
            scaled[row_off + px] = (a << 24) | rgb;
        }
    }
    let cover = Sprite {
        width: rect.w as u32,
        height: rect.h as u32,
        pixels: scaled,
    };
    canvas.blit_sprite(rect.x, rect.y, &cover);
}

/// Draw a curated or procedural preview banner into `rect`.
pub fn draw_preview_artwork(
    canvas: &mut Canvas,
    rect: Rect,
    radius: i32,
    accent: [u32; 2],
    icon: Icon,
    sprite: Option<&Sprite>,
    is_theme: bool,
) {
    if rect.w < 16 || rect.h < 16 {
        return;
    }
    if let Some(sprite) = sprite {
        blit_sprite_cover_rounded(canvas, rect, radius, sprite);
        canvas.rounded_border(rect, radius, 0xffffff, 0.20, 1);
        return;
    }

    canvas.rounded_gradient(rect, radius, accent[0], accent[1], 1.0);
    canvas.with_clip(rect, |canvas| {
        // Soft ambient highlight circles clipped cleanly to `rect`.
        let orb1 = Rect::new(
            rect.x + rect.w - rect.h / 2,
            rect.y - rect.h / 4,
            rect.h,
            rect.h,
        );
        canvas.rounded_rect(orb1, rect.h / 2, mix(accent[0], 0xffffff, 0.45), 0.16);
        let orb2 = Rect::new(rect.x - rect.h / 4, rect.y + rect.h / 2, rect.h, rect.h);
        canvas.rounded_rect(orb2, rect.h / 2, mix(accent[1], 0x000000, 0.35), 0.18);

        if is_theme && rect.w >= 140 && rect.h >= 80 {
            // Miniature desktop composition with no proprietary brand marks.
            let win_w = (rect.w * 64 / 100).clamp(90, 210);
            let win_h = (rect.h * 66 / 100).clamp(52, 128);
            let wx = rect.x + (rect.w - win_w) / 2;
            let wy = rect.y + (rect.h - win_h - 10) / 2;
            let win = Rect::new(wx, wy, win_w, win_h);
            canvas.rounded_drop_shadow(win, 6, 8, 2, 0.25);
            canvas.rounded_rect(win, 6, 0xf8fafc, 0.96);
            let tbar = Rect::new(wx, wy, win_w, 16);
            canvas.rounded_rect(tbar, 6, mix(accent[0], 0x0f172a, 0.35), 1.0);
            canvas.fill(Rect::new(wx, wy + 8, win_w, 8), mix(accent[0], 0x0f172a, 0.35));
            for i in 0..3 {
                canvas.rounded_rect(
                    Rect::new(wx + 8 + i * 10, wy + 5, 6, 6),
                    3,
                    if i == 0 {
                        0xf97316
                    } else {
                        mix(accent[0], 0xffffff, 0.65)
                    },
                    0.95,
                );
            }
            let side = Rect::new(wx + 4, wy + 20, (win_w / 3).max(26), win_h - 24);
            canvas.rounded_rect(side, 3, 0xe2e8f0, 0.95);
            canvas.rounded_rect(
                Rect::new(side.x + 5, side.y + 6, side.w - 10, 10),
                3,
                accent[0],
                0.85,
            );
            let card = Rect::new(side.right() + 6, wy + 22, win_w - side.w - 14, win_h - 30);
            canvas.rounded_rect(card, 4, 0xffffff, 1.0);
            canvas.rounded_rect(
                Rect::new(card.x + 6, card.y + 6, card.w - 12, 14),
                3,
                accent[1],
                0.72,
            );
            // Bottom miniature bar
            let bar = Rect::new(rect.x + 10, rect.bottom() - 14, rect.w - 20, 10);
            canvas.rounded_rect(bar, 4, 0x0f172a, 0.42);
        } else {
            let emblem_sz = (rect.h * 52 / 100).clamp(32, 64);
            let emblem = Rect::new(
                rect.x + (rect.w - emblem_sz) / 2,
                rect.y + (rect.h - emblem_sz) / 2,
                emblem_sz,
                emblem_sz,
            );
            let rad = (emblem_sz / 4).max(8);
            canvas.rounded_rect(emblem, rad, 0xffffff, 0.16);
            canvas.rounded_border(emblem, rad, 0xffffff, 0.32, 1);
            let g = (emblem_sz * 56 / 100).max(16);
            icons::draw(
                canvas,
                icon,
                emblem.x + (emblem.w - g) / 2,
                emblem.y + (emblem.h - g) / 2,
                g as u32,
                Some(0xffffff),
            );
        }
    });
    canvas.rounded_border(rect, radius, 0xffffff, 0.20, 1);
}

/// Rounded "pill" action button. Returns the rect so callers can hit-test it.
pub fn cta_pill(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    primary: bool,
    hovered: bool,
    pressed: bool,
    accent: Option<u32>,
) -> Rect {
    if rect.is_empty() {
        return rect;
    }
    let radius = (rect.h / 2).min(theme.radius_control().max(8));
    let (bg, border, text_color) = if primary {
        let base = accent.unwrap_or(theme.accent);
        let fill = if pressed {
            mix(base, 0x000000, 0.18)
        } else if hovered {
            mix(base, 0xffffff, 0.14)
        } else {
            base
        };
        (fill, mix(fill, 0x000000, 0.20), contrast_text(fill))
    } else {
        let fill = if pressed {
            mix(theme.panel_hover, 0x000000, 0.08)
        } else if hovered {
            mix(theme.window_high, theme.panel_hover, 0.65)
        } else {
            mix(theme.window_alt, theme.window_high, 0.45)
        };
        let b = if hovered {
            mix(theme.accent, theme.panel_border, 0.40)
        } else {
            soft_border(theme)
        };
        (fill, b, theme.text)
    };

    if primary {
        canvas.rounded_gradient(
            rect,
            radius,
            mix(bg, 0xffffff, 0.08),
            mix(bg, 0x000000, 0.08),
            1.0,
        );
    } else {
        canvas.rounded_rect(rect, radius, bg, 1.0);
    }
    canvas.rounded_border(rect, radius, border, 0.92, 1);

    let max_tw = (rect.w - 12).max(8) as f32;
    let shown = text.truncate(label, max_tw, 12.0, Face::Bold);
    let width = text.measure(&shown, 12.0, Face::Bold);
    let x = rect.x as f32 + ((rect.w as f32 - width) / 2.0).max(4.0);
    let baseline = rect.y + (rect.h + 8) / 2;
    text.draw(canvas, &shown, x, baseline as f32, 12.0, text_color, Face::Bold);
    rect
}

/// Subtle destructive action pill button (e.g. for "卸载").
pub fn danger_pill(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    hovered: bool,
) -> Rect {
    if rect.is_empty() {
        return rect;
    }
    let radius = (rect.h / 2).min(theme.radius_control().max(8));
    let bg = mix(
        theme.window_high,
        theme.danger,
        if hovered { 0.20 } else { 0.10 },
    );
    let border = mix(
        theme.danger,
        theme.window_high,
        if hovered { 0.15 } else { 0.48 },
    );
    let fg = badge_text_color(theme, theme.danger);
    canvas.rounded_rect(rect, radius, bg, 1.0);
    canvas.rounded_border(rect, radius, border, 0.95, 1);

    let max_tw = (rect.w - 12).max(8) as f32;
    let shown = text.truncate(label, max_tw, 12.0, Face::Bold);
    let width = text.measure(&shown, 12.0, Face::Bold);
    let x = rect.x as f32 + ((rect.w as f32 - width) / 2.0).max(4.0);
    let baseline = rect.y + (rect.h + 8) / 2;
    text.draw(canvas, &shown, x, baseline as f32, 12.0, fg, Face::Bold);
    rect
}

/// Compact `···` overflow menu button for list rows.
pub fn more_button(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    active: bool,
    hovered: bool,
) {
    if rect.is_empty() {
        return;
    }
    let radius = (rect.h / 2).min(theme.radius_control().max(8));
    let bg = if active {
        mix(theme.window_high, theme.accent, 0.18)
    } else if hovered {
        mix(theme.window_high, theme.panel_hover, 0.65)
    } else {
        mix(theme.window_alt, theme.window_high, 0.50)
    };
    let border = if active || hovered {
        mix(theme.accent, theme.panel_border, 0.45)
    } else {
        soft_border(theme)
    };
    canvas.rounded_rect(rect, radius, bg, 1.0);
    canvas.rounded_border(rect, radius, border, 0.92, 1);

    let dot_color = if active {
        theme.accent
    } else if hovered {
        theme.text
    } else {
        theme.text_dim
    };
    let cy = rect.y + rect.h / 2 - 2;
    let cx = rect.x + rect.w / 2;
    for dx in [-7, -1, 5] {
        canvas.rounded_rect(Rect::new(cx + dx, cy, 4, 4), 2, dot_color, 1.0);
    }
}

/// Compact icon-only pill button for header bar utility actions.
pub fn toolbar_icon_button(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    icon: Icon,
    active: bool,
    hovered: bool,
    enabled: bool,
) {
    if rect.is_empty() {
        return;
    }
    let radius = (rect.h / 2).min(theme.radius_control().max(8));
    let bg = if active {
        mix(theme.window_high, theme.accent, 0.16)
    } else if hovered && enabled {
        mix(theme.window_high, theme.panel_hover, 0.65)
    } else {
        mix(theme.window_alt, theme.window_high, 0.45)
    };
    let border = if active || (hovered && enabled) {
        mix(theme.accent, theme.panel_border, 0.42)
    } else {
        soft_border(theme)
    };
    canvas.rounded_rect(rect, radius, bg, 1.0);
    canvas.rounded_border(rect, radius, border, 0.92, 1);

    let tint = if !enabled {
        theme.text_muted
    } else if active {
        theme.accent
    } else if hovered {
        theme.text
    } else {
        theme.text_dim
    };
    let glyph = 15;
    icons::draw(
        canvas,
        icon,
        rect.x + (rect.w - glyph) / 2,
        rect.y + (rect.h - glyph) / 2,
        glyph as u32,
        Some(tint),
    );
}

/// Compact clickable status chip with a small indicator dot (used in the topbar).
pub fn status_dot_chip(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    dot_color: u32,
    hovered: bool,
) {
    if rect.is_empty() {
        return;
    }
    let radius = rect.h / 2;
    let fill = if hovered {
        mix(theme.window_high, dot_color, 0.18)
    } else {
        mix(theme.window_high, dot_color, 0.10)
    };
    let border = if hovered {
        mix(dot_color, theme.panel_border, 0.35)
    } else {
        mix(dot_color, theme.window_high, 0.50)
    };
    canvas.rounded_rect(rect, radius, fill, 1.0);
    canvas.rounded_border(rect, radius, border, 0.88, 1);

    let dot_sz = 6;
    let dot_x = rect.x + 10;
    let dot_y = rect.y + (rect.h - dot_sz) / 2;
    canvas.rounded_rect(Rect::new(dot_x, dot_y, dot_sz, dot_sz), 3, dot_color, 1.0);

    let text_x = dot_x + dot_sz + 6;
    let max_tw = (rect.right() - text_x - 8).max(8);
    let shown = text.truncate(label, max_tw as f32, 11.5, Face::Bold);
    let y = rect.y + (rect.h + 8) / 2;
    text.draw(
        canvas,
        &shown,
        text_x as f32,
        y as f32,
        11.5,
        badge_text_color(theme, dot_color),
        Face::Bold,
    );
}

pub fn badge(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    color: u32,
) {
    if rect.is_empty() {
        return;
    }
    let fill = mix(theme.window_high, color, 0.14);
    canvas.rounded_rect(rect, rect.h / 2, fill, 1.0);
    canvas.rounded_border(rect, rect.h / 2, mix(color, theme.window_high, 0.42), 0.85, 1);
    let max_tw = (rect.w - 10).max(6) as f32;
    let shown = text.truncate(label, max_tw, 11.0, Face::Bold);
    let tw = text.measure(&shown, 11.0, Face::Bold);
    let x = rect.x as f32 + ((rect.w as f32 - tw) / 2.0).max(4.0);
    let y = rect.y + (rect.h + 8) / 2;
    text.draw(
        canvas,
        &shown,
        x,
        y as f32,
        11.0,
        badge_text_color(theme, color),
        Face::Bold,
    );
}

/// Compact horizontal progress strip used inside a CTA while a job runs.
pub fn inline_progress(canvas: &mut Canvas, theme: &Theme, rect: Rect, progress: f32, color: u32) {
    canvas.rounded_rect(rect, rect.h / 2, theme.panel_border, 0.55);
    let filled = ((rect.w as f32) * progress.clamp(0.0, 1.0)) as i32;
    if filled > 1 {
        let bar = Rect::new(rect.x, rect.y, filled.max(rect.h), rect.h);
        canvas.rounded_rect(bar, rect.h / 2, color, 1.0);
    }
}

pub fn avatar_stat(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    value: &str,
) {
    let half = rect.h / 2;
    draw_text_clipped(
        text,
        canvas,
        value,
        rect.x + 12,
        rect.y + half + 3,
        rect.w - 24,
        14.0,
        theme.text,
        Face::Bold,
    );
    draw_text_clipped(
        text,
        canvas,
        label,
        rect.x + 12,
        rect.y + rect.h - 8,
        rect.w - 24,
        11.0,
        theme.text_muted,
        Face::Ui,
    );
}

pub fn quick_stats(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    items: &[(&str, String)],
) {
    if items.is_empty() {
        return;
    }
    elevated_card(canvas, theme, rect, false);
    let cell = rect.w / items.len() as i32;
    for (index, (label, value)) in items.iter().enumerate() {
        let cell_rect = Rect::new(rect.x + cell * index as i32, rect.y, cell, rect.h);
        if index > 0 {
            canvas.line(
                cell_rect.x,
                rect.y + 12,
                cell_rect.x,
                rect.y + rect.h - 12,
                soft_border(theme),
                0.85,
            );
        }
        avatar_stat(canvas, text, theme, cell_rect, label, value);
    }
}

pub fn section_header(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    title: &str,
    subtitle: &str,
) {
    draw_text_clipped(
        text,
        canvas,
        title,
        rect.x,
        rect.y + 18,
        rect.w - 40,
        16.5,
        theme.text,
        Face::Bold,
    );
    if !subtitle.is_empty() {
        draw_text_clipped(
            text,
            canvas,
            subtitle,
            rect.x,
            rect.y + 36,
            rect.w,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
    }
}

/// Editorial Discover showcase card with top 16:9 preview banner + crisp metadata strip.
pub fn featured_package_card(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    pkg: &StorePackage,
    pres: &PackagePresentation,
    banner: Option<&Sprite>,
    hovered: bool,
    cta_hovered: bool,
    progress: Option<f32>,
) -> Rect {
    elevated_card(canvas, theme, rect, hovered);
    let pad = 12;
    let banner_h = 104;
    let banner_rect = Rect::new(rect.x + pad, rect.y + pad, rect.w - pad * 2, banner_h);
    draw_preview_artwork(
        canvas,
        banner_rect,
        theme.radius_control().max(8),
        pres.accent,
        pres.icon,
        banner,
        pres.is_theme,
    );

    let info_y = banner_rect.bottom() + 12;
    let badge_rect = Rect::new(rect.x + pad + 2, info_y, 40, 40);
    icon_badge(canvas, pres.icon, badge_rect, pres.accent);

    let tx = badge_rect.right() + 12;
    let tw = rect.right() - pad - tx;
    draw_text_clipped(
        text,
        canvas,
        &pres.display_name,
        tx,
        info_y + 15,
        tw,
        14.0,
        theme.text,
        Face::Bold,
    );
    draw_text_clipped(
        text,
        canvas,
        &pres.subtitle,
        tx,
        info_y + 34,
        tw,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    let cta_w = 78;
    let foot_y = rect.bottom() - 36;
    let cta = Rect::new(rect.right() - pad - cta_w, foot_y, cta_w, 26);
    let avail_w = (cta.x - rect.x - pad - 10).max(20);
    draw_card_status(canvas, text, theme, pkg, rect.x + pad + 2, foot_y + 3, avail_w);

    if let Some(value) = progress {
        inline_progress(canvas, theme, cta, value, theme.accent);
    } else {
        let label = if pkg.upgrade_available {
            "更新"
        } else if pkg.installed {
            "详情"
        } else {
            "安装"
        };
        cta_pill(
            canvas,
            text,
            theme,
            cta,
            label,
            !pkg.installed || pkg.upgrade_available,
            cta_hovered,
            false,
            Some(pres.accent[0]),
        );
    }
    cta
}

fn draw_card_status(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    pkg: &StorePackage,
    x: i32,
    y: i32,
    avail_w: i32,
) {
    if pkg.upgrade_available {
        let w = avail_w.min(60);
        badge(canvas, text, theme, Rect::new(x, y, w, 20), "可更新", theme.warning);
        if avail_w >= 118 {
            badge(
                canvas,
                text,
                theme,
                Rect::new(x + w + 6, y, 52, 20),
                "已装",
                theme.success,
            );
        }
    } else if pkg.installed {
        let w = avail_w.min(58);
        badge(canvas, text, theme, Rect::new(x, y, w, 20), "已安装", theme.success);
    } else {
        draw_text_clipped(
            text,
            canvas,
            &format!("{} · {}", pkg.display_version(), format_bytes(pkg.size)),
            x,
            y + 14,
            avail_w,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
    }
}

/// A package tile in Browse grid: spacious layout where left status never overlaps right CTA.
pub fn package_card(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    pkg: &StorePackage,
    pres: &PackagePresentation,
    _selected: bool,
    hovered: bool,
    cta_hovered: bool,
    progress: Option<f32>,
) -> Rect {
    let hover = ui::animate_float(
        ("store_card", rect.x, rect.y),
        if hovered { 1.0 } else { 0.0 },
        0.14,
    );
    elevated_card(canvas, theme, rect, hovered);
    if hover > 0.01 {
        canvas.rounded_rect(
            rect,
            theme.radius_control().max(10),
            theme.panel_hover,
            hover * 0.14,
        );
    }
    let pad = 16;
    let badge_size = 48;
    let badge_rect = Rect::new(rect.x + pad, rect.y + pad, badge_size, badge_size);
    icon_badge(canvas, pres.icon, badge_rect, pres.accent);

    let text_x = badge_rect.right() + 14;
    let text_w = rect.right() - pad - text_x;
    draw_text_clipped(
        text,
        canvas,
        &pres.display_name,
        text_x,
        rect.y + pad + 16,
        text_w,
        14.0,
        theme.text,
        Face::Bold,
    );
    for (index, line) in wrap_text(text, &pres.subtitle, text_w as f32, 11.5, Face::Ui, 2)
        .into_iter()
        .enumerate()
    {
        draw_text_clipped(
            text,
            canvas,
            &line,
            text_x,
            rect.y + pad + 35 + index as i32 * 16,
            text_w,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
    }

    let foot_y = rect.bottom() - 38;
    let cta_w = 76;
    let cta = Rect::new(rect.right() - pad - cta_w, foot_y, cta_w, 26);
    let avail_w = (cta.x - text_x - 8).max(16);
    draw_card_status(canvas, text, theme, pkg, text_x, foot_y + 3, avail_w);

    if let Some(value) = progress {
        inline_progress(canvas, theme, cta, value, theme.accent);
    } else {
        let label = if pkg.upgrade_available {
            "更新"
        } else if pkg.installed {
            "详情"
        } else {
            "安装"
        };
        cta_pill(
            canvas,
            text,
            theme,
            cta,
            label,
            !pkg.installed || pkg.upgrade_available,
            cta_hovered,
            false,
            Some(pres.accent[0]),
        );
    }
    cta
}

/// Hero spotlight card with left editorial copy + right live preview artwork + carousel controls.
/// Returns `(cta_rect, prev_arrow_rect, next_arrow_rect)`.
pub fn hero_card(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    slide: &HeroSlide,
    banner: Option<&Sprite>,
    slide_count: usize,
    installed: bool,
    cta_hovered: bool,
    prev_hovered: bool,
    next_hovered: bool,
) -> (Rect, Rect, Rect) {
    let radius = theme.radius_control().max(10);
    canvas.rounded_gradient(rect, radius, slide.grad_left, slide.grad_right, 1.0);
    canvas.rounded_border(rect, radius, 0xffffff, 0.16, 1);

    let has_showcase = rect.w >= 620;
    let preview_w = if has_showcase {
        ((rect.w * 39) / 100).clamp(240, 344)
    } else {
        0
    };
    if has_showcase {
        let preview_h = (rect.h - 32).min(preview_w * 9 / 16 + 18).max(120);
        let preview_r = Rect::new(
            rect.right() - preview_w - 22,
            rect.y + (rect.h - preview_h) / 2,
            preview_w,
            preview_h,
        );
        canvas.rounded_drop_shadow(preview_r, 10, 14, 4, 0.34);
        draw_preview_artwork(
            canvas,
            preview_r,
            10,
            [slide.grad_left, slide.grad_right],
            slide.icon,
            banner,
            banner.is_some() || slide.target_package.as_deref().is_some_and(|p| p.starts_with("theme-")),
        );
    }

    let copy_x = rect.x + 28;
    let copy_w = if has_showcase {
        (rect.w - preview_w - 72).max(180)
    } else {
        rect.w - 56
    };

    // Eyebrow pill
    let over_w = (text.measure(&slide.overline, 11.0, Face::Bold) as i32 + 20).min(copy_w);
    let over_rect = Rect::new(copy_x, rect.y + 20, over_w.max(60), 22);
    canvas.rounded_rect(over_rect, 11, 0xffffff, 0.16);
    draw_text_clipped(
        text,
        canvas,
        &slide.overline,
        over_rect.x + 10,
        over_rect.y + 15,
        over_rect.w - 20,
        11.0,
        0xffffff,
        Face::Bold,
    );

    draw_text_clipped(
        text,
        canvas,
        &slide.title,
        copy_x,
        rect.y + 72,
        copy_w,
        22.0,
        0xffffff,
        Face::Bold,
    );

    let lines = wrap_text(text, &slide.subtitle, copy_w as f32, 12.5, Face::Ui, 2);
    for (index, line) in lines.iter().enumerate() {
        draw_text_clipped(
            text,
            canvas,
            line,
            copy_x,
            rect.y + 98 + index as i32 * 18,
            copy_w,
            12.5,
            mix(slide.accent_pill, 0xffffff, 0.78),
            Face::Ui,
        );
    }

    let cta = Rect::new(copy_x, rect.bottom() - 56, 124, 32);
    let label = if installed && slide.target_package.is_some() {
        "查看详情"
    } else {
        &slide.cta_label
    };
    cta_pill(
        canvas,
        text,
        theme,
        cta,
        label,
        true,
        cta_hovered,
        false,
        Some(slide.accent_pill),
    );

    let (prev_r, next_r) = if slide_count > 1 {
        let prev = Rect::new(cta.right() + 14, cta.y + 2, 28, 28);
        let next = Rect::new(prev.right() + 8, cta.y + 2, 28, 28);
        for (r, sym, hov) in [(prev, "‹", prev_hovered), (next, "›", next_hovered)] {
            canvas.rounded_rect(r, 14, 0xffffff, if hov { 0.30 } else { 0.16 });
            canvas.rounded_border(r, 14, 0xffffff, 0.28, 1);
            let tw = text.measure(sym, 16.0, Face::Bold);
            text.draw(
                canvas,
                sym,
                (r.x + r.w / 2) as f32 - tw / 2.0,
                (r.y + 19) as f32,
                16.0,
                0xffffff,
                Face::Bold,
            );
        }
        (prev, next)
    } else {
        (Rect::default(), Rect::default())
    };

    (cta, prev_r, next_r)
}

pub fn nav_item(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    icon: Icon,
    label: &str,
    badge_count: Option<usize>,
    active: bool,
    hovered: bool,
) {
    let radius = theme.radius_control().max(8);
    if active {
        canvas.rounded_drop_shadow(rect, radius, 6, 1, 0.06);
        canvas.rounded_rect(rect, radius, theme.window_high, 1.0);
        canvas.rounded_border(
            rect,
            radius,
            mix(theme.accent, theme.panel_border, 0.42),
            0.95,
            1,
        );
        canvas.rounded_rect(
            Rect::new(rect.x + 4, rect.y + 8, 3, (rect.h - 16).max(8)),
            1,
            theme.accent,
            1.0,
        );
    } else if hovered {
        canvas.rounded_rect(
            rect,
            radius,
            mix(theme.window_high, theme.window_alt, 0.52),
            0.85,
        );
    }

    let fg = if active || hovered {
        theme.text
    } else {
        theme.text_dim
    };
    let icon_fg = if active {
        theme.accent
    } else if hovered {
        theme.text
    } else {
        theme.text_muted
    };
    icons::draw(
        canvas,
        icon,
        rect.x + 14,
        rect.y + (rect.h - 16) / 2,
        16,
        Some(icon_fg),
    );
    let right_pad = if badge_count.is_some_and(|c| c > 0) { 46 } else { 14 };
    draw_text_clipped(
        text,
        canvas,
        label,
        rect.x + 40,
        rect.y + (rect.h + 9) / 2,
        rect.w - 40 - right_pad,
        13.0,
        fg,
        if active { Face::Bold } else { Face::Ui },
    );
    if let Some(count) = badge_count {
        if count > 0 {
            let value = count.min(99).to_string();
            let width = (value.len() as i32 * 7 + 14).max(22);
            let pill = Rect::new(rect.right() - width - 10, rect.y + (rect.h - 18) / 2, width, 18);
            badge(canvas, text, theme, pill, &value, theme.accent);
        }
    }
}

/// Dim the whole window and draw a crisp 100%-opaque elevated modal card.
pub fn modal_frame(canvas: &mut Canvas, theme: &Theme, screen: Rect, card: Rect) {
    canvas.fill_alpha(screen, 0x000000, 0.56);
    let radius = theme.radius_control().max(10);
    canvas.rounded_drop_shadow(card, radius, 22, 6, 0.38);
    canvas.rounded_rect(card, radius, theme.window, 1.0);
    canvas.rounded_border(card, radius, theme.panel_border, 1.0, 1);
}

pub fn modal_title(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    title: &str,
    subtitle: &str,
) {
    draw_text_clipped(
        text,
        canvas,
        title,
        rect.x,
        rect.y + 22,
        rect.w,
        16.0,
        theme.text,
        Face::Bold,
    );
    if !subtitle.is_empty() {
        for (index, line) in wrap_text(text, subtitle, rect.w as f32, 12.0, Face::Ui, 2)
            .into_iter()
            .enumerate()
        {
            draw_text_clipped(
                text,
                canvas,
                &line,
                rect.x,
                rect.y + 42 + index as i32 * 17,
                rect.w,
                12.0,
                theme.text_muted,
                Face::Ui,
            );
        }
    }
}

pub fn empty_state(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    icon: Icon,
    title: &str,
    subtitle: &str,
) {
    let size = 48;
    icons::draw(
        canvas,
        icon,
        rect.x + (rect.w - size) / 2,
        rect.y + rect.h / 2 - 54,
        size as u32,
        Some(theme.text_muted),
    );
    let width = text.measure(title, 15.0, Face::Bold);
    text.draw(
        canvas,
        title,
        (rect.x + rect.w / 2) as f32 - width / 2.0,
        (rect.y + rect.h / 2 + 16) as f32,
        15.0,
        theme.text,
        Face::Bold,
    );
    for (index, line) in wrap_text(text, subtitle, (rect.w - 80).max(120) as f32, 12.0, Face::Ui, 2)
        .into_iter()
        .enumerate()
    {
        let width = text.measure(&line, 12.0, Face::Ui);
        text.draw(
            canvas,
            &line,
            (rect.x + rect.w / 2) as f32 - width / 2.0,
            (rect.y + rect.h / 2 + 38 + index as i32 * 17) as f32,
            12.0,
            theme.text_muted,
            Face::Ui,
        );
    }
}
