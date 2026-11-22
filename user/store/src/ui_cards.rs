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

use spui_ui::draw::{Canvas, Rect};
use spui_ui::icons::{self, Icon};
use spui_ui::text::{Face, TextRenderer};
use spui_ui::theme::Theme;
use spui_ui::ui;

use crate::curated::{PackagePresentation, HeroSlide};
use crate::model::{format_bytes, StorePackage};

pub const SIDEBAR_W: i32 = 216;
pub const TOPBAR_H: i32 = 48;
pub const CARD_H: i32 = 132;

pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let av = ((a >> shift) & 0xff) as f32;
        let bv = ((b >> shift) & 0xff) as f32;
        ((av + (bv - av) * t).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Rounded gradient app icon, the visual anchor of every card.
pub fn icon_badge(canvas: &mut Canvas, icon: Icon, rect: Rect, accent: [u32; 2]) {
    if rect.w <= 2 || rect.h <= 2 {
        return;
    }
    let radius = (rect.w.min(rect.h) / 4).max(6);
    canvas.rounded_gradient(rect, radius, accent[0], accent[1], 1.0);
    // soft top highlight so the gradient reads as glass rather than a flat fill
    let gloss = Rect::new(rect.x + 2, rect.y + 2, rect.w - 4, rect.h / 2 - 2);
    canvas.rounded_rect(gloss, radius - 2, mix(accent[0], 0xffffff, 0.55), 0.18);
    let glyph = (rect.w.min(rect.h) * 55 / 100).max(12);
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
    let clipped = text.truncate(value, max_width as f32, px, face);
    text.draw(canvas, &clipped, x as f32, baseline as f32, px, color, face);
}

/// Wraps `value` into at most `lines` lines of `max_width` pixels, appending
/// an ellipsis when the text does not fit. CJK text is wrapped per character.
pub fn wrap_text(
    text: &mut TextRenderer,
    value: &str,
    max_width: f32,
    px: f32,
    face: Face,
    lines: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if ch == '\n' {
            out.push(std::mem::take(&mut current));
            if out.len() == lines {
                break;
            }
            continue;
        }
        let mut probe = current.clone();
        probe.push(ch);
        if text.measure(&probe, px, face) > max_width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            if out.len() == lines {
                break;
            }
        }
        current.push(ch);
    }
    if out.len() < lines && !current.is_empty() {
        out.push(current);
    }
    if out.len() == lines {
        if let Some(last) = out.last_mut() {
            let full = text.measure(last, px, face) > max_width;
            if full || value.chars().count() > lines * 24 {
                if let Some(trimmed) = last.clone().strip_suffix('…').map(str::to_string) {
                    *last = trimmed;
                } else {
                    last.push('…');
                }
            }
        }
    }
    out
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
    let _color = ui::action_button_surface(canvas, theme, rect, primary, hovered, pressed, true);
    if primary {
        if let Some(accent) = accent {
            canvas.rounded_rect(rect, rect.h / 2, accent, 0.38);
        }
    }
    let text_color = if primary { 0xffffff } else { theme.text };
    let width = text.measure(label, 13.0, Face::Bold);
    let x = rect.x + ((rect.w as f32 - width) / 2.0).max(6.0) as i32;
    let baseline = rect.y + (rect.h + 9) / 2;
    text.draw(canvas, label, x as f32, baseline as f32, 13.0, text_color, Face::Bold);
    rect
}

pub fn badge(canvas: &mut Canvas, _theme: &Theme, rect: Rect, _label: &str, color: u32) {
    if rect.is_empty() {
        return;
    }
    canvas.rounded_rect(rect, rect.h / 2, color, 0.18);
    canvas.rounded_border(rect, rect.h / 2, color, 0.55, 1);
}

/// Compact horizontal progress strip used inside a CTA while a job runs.
pub fn inline_progress(canvas: &mut Canvas, theme: &Theme, rect: Rect, progress: f32, color: u32) {
    canvas.rounded_rect(rect, rect.h / 2, theme.panel_border, 0.5);
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
        rect.x + 8,
        rect.y + half + 4,
        rect.w - 16,
        14.0,
        theme.text,
        Face::Bold,
    );
    draw_text_clipped(
        text,
        canvas,
        label,
        rect.x + 8,
        rect.y + rect.h - 6,
        rect.w - 16,
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
    ui::card(canvas, theme, rect);
    let cell = rect.w / items.len() as i32;
    for (index, (label, value)) in items.iter().enumerate() {
        let cell_rect = Rect::new(rect.x + cell * index as i32, rect.y, cell, rect.h);
        if index > 0 {
            canvas.line(
                cell_rect.x,
                rect.y + 10,
                cell_rect.x,
                rect.y + rect.h - 10,
                theme.panel_border,
                0.6,
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
        rect.y + 16,
        rect.w - 120,
        16.0,
        theme.text,
        Face::Bold,
    );
    if !subtitle.is_empty() {
        draw_text_clipped(
            text,
            canvas,
            subtitle,
            rect.x,
            rect.y + 34,
            rect.w,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
    }
}

/// A package tile in a grid: icon, name, subtitle, version and state.
/// Returns the CTA rect for hit-testing.
pub fn package_card(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    pkg: &StorePackage,
    pres: &PackagePresentation,
    selected: bool,
    hovered: bool,
    progress: Option<f32>,
) -> Rect {
    let hover = ui::animate_float(("store_card", rect.x, rect.y), if hovered { 1.0 } else { 0.0 }, 0.14);
    ui::selectable_card(canvas, theme, rect, selected, hovered);
    if hover > 0.01 && !selected {
        canvas.rounded_rect(rect, theme.radius_control(), theme.panel_hover, hover * 0.35);
    }
    let pad = 12;
    let badge_size = 44;
    let badge_rect = Rect::new(rect.x + pad, rect.y + pad, badge_size, badge_size);
    icon_badge(canvas, pres.icon, badge_rect, pres.accent);

    let text_x = badge_rect.x + badge_size + 12;
    let text_w = rect.x + rect.w - pad - text_x;
    draw_text_clipped(
        text,
        canvas,
        &pres.display_name,
        text_x,
        rect.y + pad + 15,
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
            rect.y + pad + 32 + index as i32 * 14,
            text_w,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
    }

    let foot_y = rect.y + rect.h - 24;
    if pkg.installed {
        badge(
            canvas,
            theme,
            Rect::new(text_x, foot_y - 10, 54, 18),
            "已安装",
            theme.success,
        );
        draw_text_clipped(
            text,
            canvas,
            "已安装",
            text_x + 9,
            foot_y + 4,
            48,
            11.0,
            theme.success,
            Face::Ui,
        );
        if pkg.upgrade_available {
            badge(
                canvas,
                theme,
                Rect::new(text_x + 60, foot_y - 10, 46, 18),
                "可更新",
                theme.warning,
            );
            draw_text_clipped(
                text,
                canvas,
                "可更新",
                text_x + 65,
                foot_y + 4,
                42,
                11.0,
                theme.warning,
                Face::Ui,
            );
        }
    } else {
        draw_text_clipped(
            text,
            canvas,
            &format!("{} · {}", pkg.display_version(), format_bytes(pkg.size)),
            text_x,
            foot_y + 4,
            text_w,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
    }

    let cta = Rect::new(rect.x + rect.w - pad - 74, foot_y - 12, 74, 26);
    if let Some(value) = progress {
        inline_progress(canvas, theme, cta, value, theme.accent);
    } else {
        let label = if pkg.upgrade_available {
            "更新"
        } else if pkg.installed {
            "打开"
        } else {
            "安装"
        };
        cta_pill(
            canvas,
            text,
            theme,
            cta,
            label,
            !pkg.installed,
            hovered,
            false,
            Some(pres.accent[0]),
        );
    }
    cta
}

/// Hero spotlight card with a large gradient wash, editorial copy and CTA.
pub fn hero_card(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    slide: &HeroSlide,
    installed: bool,
    hovered: bool,
) -> Rect {
    let radius = theme.radius_control().max(8);
    canvas.rounded_gradient(rect, radius, slide.grad_left, slide.grad_right, 1.0);
    // decorative arc: a large translucent disc behind the copy
    let disc = Rect::new(rect.x + rect.w - rect.h / 2 - 40, rect.y - rect.h / 3, rect.h, rect.h);
    canvas
        .rounded_rect(disc, rect.h / 2, mix(slide.grad_right, 0xffffff, 0.35), 0.10);
    icons::draw(
        canvas,
        slide.icon,
        rect.x + rect.w - 96,
        rect.y + rect.h / 2 - 32,
        64,
        Some(mix(slide.accent_pill, 0xffffff, 0.6)),
    );

    let text_w = rect.w - 220;
    draw_text_clipped(
        text,
        canvas,
        &slide.overline,
        rect.x + 24,
        rect.y + 30,
        text_w,
        11.0,
        mix(slide.accent_pill, 0xffffff, 0.65),
        Face::Bold,
    );
    draw_text_clipped(
        text,
        canvas,
        &slide.title,
        rect.x + 24,
        rect.y + 56,
        text_w,
        21.0,
        0xffffff,
        Face::Bold,
    );
    let lines = wrap_text(text, &slide.subtitle, text_w as f32, 12.0, Face::Ui, 2);
    for (index, line) in lines.iter().enumerate() {
        draw_text_clipped(
            text,
            canvas,
            line,
            rect.x + 24,
            rect.y + 78 + index as i32 * 16,
            text_w,
            12.0,
            mix(slide.accent_pill, 0xffffff, 0.75),
            Face::Ui,
        );
    }
    let cta = Rect::new(rect.x + 24, rect.y + rect.h - 46, 118, 30);
    let label = if installed && slide.target_package.is_some() {
        "查看详情"
    } else {
        &slide.cta_label
    };
    cta_pill(canvas, text, theme, cta, label, true, hovered, false, Some(slide.accent_pill));
    cta
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
    ui::list_item_bg(canvas, theme, rect, active, hovered);
    let fg = if active {
        theme.selection_text.max(theme.text)
    } else {
        theme.text
    };
    icons::draw(
        canvas,
        icon,
        rect.x + 12,
        rect.y + (rect.h - 16) / 2,
        16,
        Some(if active { fg } else { theme.text_muted }),
    );
    draw_text_clipped(
        text,
        canvas,
        label,
        rect.x + 38,
        rect.y + (rect.h + 9) / 2,
        rect.w - 80,
        13.0,
        fg,
        if active { Face::Bold } else { Face::Ui },
    );
    if let Some(count) = badge_count {
        if count > 0 {
            let value = count.min(99).to_string();
            let width = (value.len() as i32 * 7 + 12).max(20);
            let pill = Rect::new(rect.x + rect.w - width - 10, rect.y + (rect.h - 18) / 2, width, 18);
            badge(canvas, theme, pill, &value, theme.accent);
            let tw = text.measure(&value, 10.5, Face::Bold);
            text.draw(
                canvas,
                &value,
                (pill.x + pill.w / 2) as f32 - tw / 2.0,
                (pill.y + 13) as f32,
                10.5,
                theme.accent,
                Face::Bold,
            );
        }
    }
}

/// Dim the whole window and draw a modal card with a drop shadow.
pub fn modal_frame(canvas: &mut Canvas, theme: &Theme, screen: Rect, card: Rect) {
    canvas.fill_alpha(screen, 0x000000, 0.45);
    canvas.rounded_drop_shadow(card, theme.radius_control().max(8), 18, 4, 0.35);
    ui::popup_surface(canvas, theme, card, theme.radius_control().max(8));
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
