//
//       user/editor/src/draw.rs
//       SpaceEditor rendering: Ribbon, Ruler, Document Outline, A4 Canvas & Modals
//       Zero theme/skin checks live here; all chrome follows Theme tokens & spui_ui.
//

use crate::app::{
    Action, EditorState, FocusedField, Hits, ModalKind, RibbonTab, PAGE_GAP,
};
use crate::doc::{Align, BlockKind, MarginPreset, Orientation, PageSize};
use crate::io_text::DocFormat;
use crate::layout::{DocLayout, LinePrefix, PageItem};
use spui_ui::draw::{mix, Canvas, Rect};
use spui_ui::icons::{self, Icon};
use spui_ui::text::{Face, TextRenderer};
use spui_ui::theme::Theme;
use spui_ui::ui;
use std::time::Instant;

const QUICK_TAB_H: i32 = 34;
const RIBBON_H: i32 = 54;
const FIND_H: i32 = 38;
const RULER_H: i32 = 22;
const STATUS_H: i32 = 26;
const OUTLINE_W: i32 = 208;
const SCROLLBAR_W: i32 = 10;

const TEXT_COLORS: [u32; 6] = [0x1e293b, 0x1d4ed8, 0xdc2626, 0x15803d, 0x7c3aed, 0xb45309];
const HIGHLIGHT_COLORS: [Option<u32>; 5] = [
    None,
    Some(0xfef08a),
    Some(0xbbf7d0),
    Some(0xbfdbfe),
    Some(0xfecdd3),
];

fn top_chrome_height(state: &EditorState) -> i32 {
    QUICK_TAB_H + RIBBON_H + if state.find_open { FIND_H } else { 0 }
}

pub fn canvas_viewport_rect(state: &EditorState, content: Rect) -> Rect {
    let top_h = top_chrome_height(state);
    let left_w = if state.show_outline && content.w >= 760 {
        OUTLINE_W
    } else {
        0
    };
    let ruler_h = if state.show_ruler { RULER_H } else { 0 };
    Rect::new(
        content.x + left_w,
        content.y + top_h + ruler_h,
        (content.w - left_w - SCROLLBAR_W).max(160),
        (content.h - top_h - ruler_h - STATUS_H).max(120),
    )
}

pub fn screen_to_page_local(
    state: &EditorState,
    content: Rect,
    sx: i32,
    sy: i32,
) -> Option<(usize, f32, f32)> {
    let vp = canvas_viewport_rect(state, content);
    if !vp.contains(sx, sy) {
        return None;
    }
    let layout = state.layout.as_ref()?;
    let scroll_y = state.scroll.offset_i32();
    let doc_y = sy - vp.y + scroll_y;

    for page in &layout.pages {
        let pw = (page.width as f32 * state.zoom).round() as i32;
        let ph = (page.height as f32 * state.zoom).round() as i32;
        let px = vp.x + ((vp.w - pw).max(24)) / 2;
        let py = layout.page_top_y(page.page_index, state.zoom, PAGE_GAP);
        if doc_y >= py - PAGE_GAP / 2 && doc_y <= py + ph + PAGE_GAP / 2 {
            let lx = ((sx - px) as f32 / state.zoom).clamp(0.0, page.width as f32);
            let ly = ((doc_y - py) as f32 / state.zoom).clamp(0.0, page.height as f32);
            return Some((page.page_index, lx, ly));
        }
    }
    layout.pages.last().map(|page| {
        let pw = (page.width as f32 * state.zoom).round() as i32;
        let px = vp.x + ((vp.w - pw).max(24)) / 2;
        let lx = ((sx - px) as f32 / state.zoom).clamp(0.0, page.width as f32);
        (page.page_index, lx, page.height as f32 - page.margin_bottom as f32)
    })
}

pub fn draw_editor(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut EditorState,
    content: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) -> Option<Rect> {
    canvas.fill(content, mix(theme.window_alt, theme.panel_border, 0.22));

    let quick_rect = Rect::new(content.x, content.y, content.w, QUICK_TAB_H);
    let ribbon_rect = Rect::new(content.x, quick_rect.bottom(), content.w, RIBBON_H);
    draw_quick_and_tabs(canvas, text, theme, state, quick_rect, prev_hits, hits);
    draw_ribbon_strip(canvas, text, theme, state, ribbon_rect, prev_hits, hits);

    let mut ime_spot = None;
    let mut after_top = ribbon_rect.bottom();
    if state.find_open {
        let find_rect = Rect::new(content.x, after_top, content.w, FIND_H);
        if let Some(spot) = draw_find_bar(canvas, text, theme, state, find_rect, prev_hits, hits) {
            ime_spot = Some(spot);
        }
        after_top = find_rect.bottom();
    }

    let body_h = (content.bottom() - after_top - STATUS_H).max(120);
    let mut work_x = content.x;
    if state.show_outline && content.w >= 760 {
        let outline_rect = Rect::new(content.x, after_top, OUTLINE_W, body_h);
        draw_outline_pane(canvas, text, theme, state, outline_rect, prev_hits, hits);
        work_x = outline_rect.right();
    }

    let work_w = (content.right() - work_x).max(160);
    if state.show_ruler {
        let ruler_rect = Rect::new(work_x, after_top, work_w - SCROLLBAR_W, RULER_H);
        draw_ruler(canvas, text, theme, state, ruler_rect);
    }

    let vp = canvas_viewport_rect(state, content);
    state.canvas_viewport = vp;

    let sb_rect = Rect::new(vp.right(), vp.y, SCROLLBAR_W, vp.h);
    let total_h = state
        .layout
        .as_ref()
        .map_or(vp.h, |l| l.total_height_scaled(state.zoom, PAGE_GAP))
        .max(vp.h);
    state.scroll.set_bounds(vp.h as f32, total_h as f32, sb_rect);

    if let Some(spot) = draw_document_pages(canvas, text, theme, state, vp, prev_hits) {
        if state.focused == FocusedField::Canvas {
            ime_spot = Some(spot);
        }
    }

    let offset = state.scroll.offset_i32();
    ui::scrollbar_v_animated(
        canvas,
        theme,
        "editor_main_scrollbar",
        sb_rect,
        total_h as f32,
        vp.h as f32,
        offset as f32,
        state.scroll.is_hovered(),
        state.scroll.is_dragging(),
        state.scroll.is_animating(),
    );

    let status_rect = Rect::new(content.x, content.bottom() - STATUS_H, content.w, STATUS_H);
    draw_status_bar(canvas, text, theme, state, status_rect, prev_hits, hits);

    if state.font_menu_open {
        draw_font_dropdown_menu(canvas, text, theme, state, content, prev_hits, hits);
    }

    if state.modal.is_some() {
        if let Some(spot) = draw_modal(canvas, text, theme, state, content, prev_hits, hits) {
            ime_spot = Some(spot);
        }
    }

    ime_spot
}

fn draw_quick_and_tabs(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &EditorState,
    rect: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    canvas.fill(rect, theme.window_alt);
    canvas.fill(Rect::new(rect.x, rect.bottom() - 1, rect.w, 1), theme.panel_border);
    let ptr = state.last_pointer;
    let mut x = rect.x + 10;

    let quick_cmds: [(&str, Action, bool); 6] = [
        ("新建", Action::NewDoc, false),
        ("打开", Action::OpenModal(ModalKind::OpenFile), false),
        ("保存", Action::Save, state.doc.dirty),
        ("另存", Action::OpenModal(ModalKind::SaveAs), false),
        ("撤销", Action::Undo, false),
        ("重做", Action::Redo, false),
    ];
    for (label, act, primary) in quick_cmds {
        let w = text.measure(label, 11.5, Face::Ui) as i32 + 16;
        let r = Rect::new(x, rect.y + 5, w, 24);
        let hov = prev_hits.hover_rect(ptr.0, ptr.1, r);
        ui::button(canvas, text, theme, r, label, None, primary, hov);
        hits.push(r, act);
        x += w + 5;
    }

    x += 8;
    canvas.fill(Rect::new(x, rect.y + 7, 1, rect.h - 14), theme.panel_border);
    x += 12;

    for tab in [RibbonTab::Home, RibbonTab::Insert, RibbonTab::Layout, RibbonTab::View] {
        let label = tab.label();
        let w = text.measure(label, 12.5, Face::Ui) as i32 + 28;
        let r = Rect::new(x, rect.y + 4, w, 28);
        let active = state.active_tab == tab;
        let hov = prev_hits.hover_rect(ptr.0, ptr.1, r);
        let color = ui::tab_item(canvas, theme, r, active, hov, false, None);
        let tw = text.measure(label, 12.5, Face::Ui);
        text.draw(
            canvas,
            label,
            (r.x + r.w / 2) as f32 - tw * 0.5,
            (r.y + 19) as f32,
            12.5,
            color,
            if active { Face::Bold } else { Face::Ui },
        );
        hits.push(r, Action::SelectTab(tab));
        x += w + 4;
    }

    let find_r = Rect::new(rect.right() - 88, rect.y + 5, 76, 24);
    let hov_f = prev_hits.hover_rect(ptr.0, ptr.1, find_r);
    ui::button(
        canvas,
        text,
        theme,
        find_r,
        "查找替换",
        Some(Icon::Search),
        state.find_open,
        hov_f,
    );
    hits.push(find_r, Action::ToggleFind(true));
}

fn chip_button(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    active: bool,
    hovered: bool,
    face: Face,
) {
    let bg = if active {
        theme.accent
    } else if hovered {
        theme.panel_hover
    } else {
        theme.panel
    };
    let fg = if active { 0xffffff } else { theme.text };
    let r = theme.radius_control();
    canvas.rounded_rect(rect, r, bg, 1.0);
    canvas.rounded_border(
        rect,
        r,
        if active { theme.accent } else { theme.panel_border },
        0.75,
        1,
    );
    let shown = text.truncate(label, (rect.w - 6) as f32, 11.5, face);
    let tw = text.measure(&shown, 11.5, face);
    text.draw(
        canvas,
        &shown,
        (rect.x + rect.w / 2) as f32 - tw * 0.5,
        (rect.y + 18) as f32,
        11.5,
        fg,
        face,
    );
}

fn draw_ribbon_strip(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &EditorState,
    rect: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    canvas.fill(rect, theme.panel);
    canvas.fill(Rect::new(rect.x, rect.bottom() - 1, rect.w, 1), theme.panel_border);
    let ptr = state.last_pointer;
    let y = rect.y + 13;
    let bh = 28;
    let mut x = rect.x + 12;

    match state.active_tab {
        RibbonTab::Home => {
            let cur_kind = state.doc.current_block_kind();
            let style_defs: [(&str, BlockKind, i32); 4] = [
                ("正文", BlockKind::Paragraph, 42),
                ("H1", BlockKind::Heading(1), 32),
                ("H2", BlockKind::Heading(2), 32),
                ("H3", BlockKind::Heading(3), 32),
            ];
            for (lbl, k, w) in style_defs {
                let r = Rect::new(x, y, w, bh);
                let active = cur_kind == k;
                chip_button(canvas, text, theme, r, lbl, active, prev_hits.hover_rect(ptr.0, ptr.1, r), Face::Bold);
                hits.push(r, Action::SetBlockKind(k));
                x += w + 4;
            }

            x += 4;
            canvas.fill(Rect::new(x, rect.y + 10, 1, rect.h - 20), theme.panel_border);
            x += 8;

            // Dynamic uxconfd font family picker dropdown + font size +/-
            let font_r = Rect::new(x, y, 136, bh);
            let font_hov = prev_hits.hover_rect(ptr.0, ptr.1, font_r);
            let font_bg = if state.font_menu_open {
                mix(theme.window_high, theme.accent, 0.14)
            } else if font_hov {
                theme.panel_hover
            } else {
                theme.window_high
            };
            let font_brd = if state.font_menu_open || font_hov {
                theme.accent
            } else {
                theme.panel_border
            };
            let r = theme.radius_control();
            canvas.rounded_rect(font_r, r, font_bg, 1.0);
            canvas.rounded_border(font_r, r, font_brd, 0.85, 1);
            let active_font = state.active_font_display_name();
            let shown_font = text.truncate(&active_font, (font_r.w - 24) as f32, 11.5, Face::Ui);
            text.draw(
                canvas,
                &shown_font,
                (font_r.x + 8) as f32,
                (font_r.y + 18) as f32,
                11.5,
                theme.text,
                Face::Ui,
            );
            let arrow_c = if state.font_menu_open {
                theme.accent
            } else {
                theme.text_muted
            };
            let ax = font_r.right() - 12;
            let ay = font_r.y + font_r.h / 2 - 1;
            for d in 0..=3 {
                canvas.fill(Rect::new(ax - d, ay + 2 - d, d * 2 + 1, 1), arrow_c);
            }
            hits.push(font_r, Action::ToggleFontMenu(font_r));
            x += 140;

            let sz_down = Rect::new(x, y, 22, bh);
            chip_button(canvas, text, theme, sz_down, "A-", false, prev_hits.hover_rect(ptr.0, ptr.1, sz_down), Face::Ui);
            hits.push(sz_down, Action::SizeDelta(-1.5));
            x += 24;

            let sz_lbl = format!("{:.0}", state.doc.active_style.size_pt);
            let sw = text.measure(&sz_lbl, 11.5, Face::Bold);
            text.draw(canvas, &sz_lbl, x as f32 + 12.0 - sw * 0.5, (y + 18) as f32, 11.5, theme.text, Face::Bold);
            x += 26;

            let sz_up = Rect::new(x, y, 22, bh);
            chip_button(canvas, text, theme, sz_up, "A+", false, prev_hits.hover_rect(ptr.0, ptr.1, sz_up), Face::Ui);
            hits.push(sz_up, Action::SizeDelta(1.5));
            x += 28;

            canvas.fill(Rect::new(x, rect.y + 10, 1, rect.h - 20), theme.panel_border);
            x += 8;

            // B / I / U / S
            let fmt_btns: [(&str, bool, Face, Action); 4] = [
                ("B", state.doc.active_style.bold, Face::Bold, Action::ToggleBold),
                ("I", state.doc.active_style.italic, Face::Italic, Action::ToggleItalic),
                ("U", state.doc.active_style.underline, Face::Ui, Action::ToggleUnderline),
                ("S", state.doc.active_style.strikethrough, Face::Ui, Action::ToggleStrike),
            ];
            for (lbl, on, face, act) in fmt_btns {
                let r = Rect::new(x, y, 26, bh);
                chip_button(canvas, text, theme, r, lbl, on, prev_hits.hover_rect(ptr.0, ptr.1, r), face);
                hits.push(r, act);
                x += 29;
            }

            x += 5;
            canvas.fill(Rect::new(x, rect.y + 10, 1, rect.h - 20), theme.panel_border);
            x += 8;

            // Color swatches
            for col in TEXT_COLORS {
                let r = Rect::new(x, y + 4, 20, 20);
                let active = state.doc.active_style.color == col;
                canvas.rounded_rect(r, 4, col, 1.0);
                if active || prev_hits.hover_rect(ptr.0, ptr.1, r) {
                    canvas.rounded_border(r, 4, theme.text, 0.9, if active { 2 } else { 1 });
                }
                hits.push(r, Action::SetTextColor(col));
                x += 23;
            }

            x += 4;
            for hl in HIGHLIGHT_COLORS {
                let r = Rect::new(x, y + 4, 20, 20);
                let active = state.doc.active_style.highlight == hl;
                if let Some(bg) = hl {
                    canvas.rounded_rect(r, 4, bg, 1.0);
                } else {
                    canvas.rounded_rect(r, 4, theme.window_alt, 1.0);
                    canvas.line(r.x + 4, r.bottom() - 4, r.right() - 4, r.y + 4, theme.text_muted, 0.9);
                }
                canvas.rounded_border(r, 4, if active { theme.accent } else { theme.panel_border }, 0.9, if active { 2 } else { 1 });
                hits.push(r, Action::SetHighlight(hl));
                x += 23;
            }

            x += 4;
            canvas.fill(Rect::new(x, rect.y + 10, 1, rect.h - 20), theme.panel_border);
            x += 8;

            let cur_align = state.doc.current_align();
            for (lbl, al) in [("左", Align::Left), ("中", Align::Center), ("右", Align::Right)] {
                if x + 28 > rect.right() - 8 {
                    break;
                }
                let r = Rect::new(x, y, 26, bh);
                chip_button(canvas, text, theme, r, lbl, cur_align == al, prev_hits.hover_rect(ptr.0, ptr.1, r), Face::Ui);
                hits.push(r, Action::SetAlign(al));
                x += 29;
            }

            for (lbl, kind) in [
                ("•", BlockKind::BulletList),
                ("1.", BlockKind::NumberedList),
                ("”", BlockKind::Quote),
            ] {
                if x + 28 > rect.right() - 8 {
                    break;
                }
                let r = Rect::new(x, y, 26, bh);
                let active = cur_kind == kind;
                chip_button(canvas, text, theme, r, lbl, active, prev_hits.hover_rect(ptr.0, ptr.1, r), Face::Bold);
                hits.push(r, Action::SetBlockKind(kind));
                x += 29;
            }
        }
        RibbonTab::Insert => {
            let btns: [(&str, Action, bool); 8] = [
                ("快速表格 3×3", Action::InsertQuickTable(3, 3), true),
                ("自定义表格...", Action::OpenModal(ModalKind::InsertTable), false),
                ("表格+行", Action::TableAddRow, false),
                ("表格+列", Action::TableAddCol, false),
                ("表格-行", Action::TableDelRow, false),
                ("插入图片...", Action::OpenModal(ModalKind::InsertImage), false),
                ("水平分割线", Action::InsertDivider, false),
                ("载入示例报告", Action::OpenDemo, false),
            ];
            for (lbl, act, primary) in btns {
                let w = text.measure(lbl, 11.5, Face::Ui) as i32 + 22;
                let r = Rect::new(x, y, w, bh);
                ui::button(canvas, text, theme, r, lbl, None, primary, prev_hits.hover_rect(ptr.0, ptr.1, r));
                hits.push(r, act);
                x += w + 8;
            }
        }
        RibbonTab::Layout => {
            for sz in [PageSize::A4, PageSize::Letter] {
                let r = Rect::new(x, y, 60, bh);
                chip_button(canvas, text, theme, r, sz.label(), state.doc.setup.size == sz, prev_hits.hover_rect(ptr.0, ptr.1, r), Face::Bold);
                hits.push(r, Action::SetPageSize(sz));
                x += 64;
            }
            let orient_lbl = if state.doc.setup.orientation == Orientation::Portrait {
                "纵向页面"
            } else {
                "横向页面"
            };
            let or_r = Rect::new(x, y, 76, bh);
            chip_button(canvas, text, theme, or_r, orient_lbl, false, prev_hits.hover_rect(ptr.0, ptr.1, or_r), Face::Ui);
            hits.push(or_r, Action::ToggleOrientation);
            x += 84;

            for preset in [MarginPreset::Normal, MarginPreset::Narrow, MarginPreset::Wide] {
                let lbl = format!("边距:{}", preset.label());
                let w = text.measure(&lbl, 11.5, Face::Ui) as i32 + 16;
                let r = Rect::new(x, y, w, bh);
                chip_button(canvas, text, theme, r, &lbl, state.doc.setup.margin_preset == preset, prev_hits.hover_rect(ptr.0, ptr.1, r), Face::Ui);
                hits.push(r, Action::SetMarginPreset(preset));
                x += w + 6;
            }

            for sp in [1.15f32, 1.35, 1.65] {
                let lbl = format!("行距{sp:.2}");
                let r = Rect::new(x, y, 64, bh);
                chip_button(canvas, text, theme, r, &lbl, false, prev_hits.hover_rect(ptr.0, ptr.1, r), Face::Ui);
                hits.push(r, Action::SetLineSpacing(sp));
                x += 68;
            }

            let hf_r = Rect::new(x, y, 92, bh);
            ui::button(canvas, text, theme, hf_r, "页眉/页脚...", None, false, prev_hits.hover_rect(ptr.0, ptr.1, hf_r));
            hits.push(hf_r, Action::OpenModal(ModalKind::HeaderFooter));
            x += 98;

            let pn_r = Rect::new(x, y, 68, bh);
            chip_button(canvas, text, theme, pn_r, "页码显示", state.doc.setup.show_page_numbers, prev_hits.hover_rect(ptr.0, ptr.1, pn_r), Face::Ui);
            hits.push(pn_r, Action::TogglePageNumbers);
        }
        RibbonTab::View => {
            let out_r = Rect::new(x, y, 88, bh);
            chip_button(canvas, text, theme, out_r, "文档大纲栏", state.show_outline, prev_hits.hover_rect(ptr.0, ptr.1, out_r), Face::Ui);
            hits.push(out_r, Action::ToggleOutline);
            x += 94;

            let rul_r = Rect::new(x, y, 76, bh);
            chip_button(canvas, text, theme, rul_r, "标尺刻度", state.show_ruler, prev_hits.hover_rect(ptr.0, ptr.1, rul_r), Face::Ui);
            hits.push(rul_r, Action::ToggleRuler);
            x += 84;

            for z in [0.75f32, 1.0, 1.25, 1.5] {
                let lbl = format!("{:.0}%", z * 100.0);
                let r = Rect::new(x, y, 54, bh);
                chip_button(canvas, text, theme, r, &lbl, (state.zoom - z).abs() < 0.04, prev_hits.hover_rect(ptr.0, ptr.1, r), Face::Bold);
                hits.push(r, Action::SetZoom(z));
                x += 58;
            }
            let fit_r = Rect::new(x, y, 74, bh);
            chip_button(canvas, text, theme, fit_r, "适应宽度", false, prev_hits.hover_rect(ptr.0, ptr.1, fit_r), Face::Ui);
            hits.push(fit_r, Action::FitWidth);
        }
    }
}

fn draw_find_bar(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &EditorState,
    rect: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) -> Option<Rect> {
    canvas.fill(rect, theme.window_alt);
    canvas.fill(Rect::new(rect.x, rect.bottom() - 1, rect.w, 1), theme.panel_border);
    let ptr = state.last_pointer;
    let mut spot = None;
    let mut x = rect.x + 12;

    let fq_rect = Rect::new(x, rect.y + 5, 220, 28);
    let fq_focused = state.focused == FocusedField::FindQuery;
    let fq_spot = ui::text_input_ime(
        canvas,
        text,
        theme,
        fq_rect,
        &state.find_query,
        if fq_focused { &state.preedit } else { "" },
        "查找文档内容...",
        fq_focused,
        Some(Icon::Search),
    );
    if fq_focused {
        spot = Some(fq_spot);
    }
    hits.push(fq_rect, Action::FocusField(FocusedField::FindQuery));
    x += 228;

    let count_lbl = if state.find_query.is_empty() {
        "0 项".into()
    } else if state.find_matches.is_empty() {
        "无匹配".into()
    } else {
        format!("{}/{}", state.find_index + 1, state.find_matches.len())
    };
    text.draw(canvas, &count_lbl, x as f32, (rect.y + 23) as f32, 11.5, theme.text_muted, Face::Ui);
    x += 54;

    for (lbl, act) in [("上一个", Action::FindPrev), ("下一个", Action::FindNext)] {
        let r = Rect::new(x, rect.y + 6, 56, 26);
        ui::button(canvas, text, theme, r, lbl, None, false, prev_hits.hover_rect(ptr.0, ptr.1, r));
        hits.push(r, act);
        x += 62;
    }

    if state.find_replace_open {
        let rep_rect = Rect::new(x + 4, rect.y + 5, 190, 28);
        let rep_focused = state.focused == FocusedField::ReplaceWith;
        let rep_spot = ui::text_input_ime(
            canvas,
            text,
            theme,
            rep_rect,
            &state.replace_text,
            if rep_focused { &state.preedit } else { "" },
            "替换为...",
            rep_focused,
            None,
        );
        if rep_focused {
            spot = Some(rep_spot);
        }
        hits.push(rep_rect, Action::FocusField(FocusedField::ReplaceWith));
        x += 200;

        for (lbl, act) in [("替换", Action::ReplaceOne), ("全部替换", Action::ReplaceAll)] {
            let w = text.measure(lbl, 11.5, Face::Ui) as i32 + 18;
            let r = Rect::new(x, rect.y + 6, w, 26);
            ui::button(canvas, text, theme, r, lbl, None, false, prev_hits.hover_rect(ptr.0, ptr.1, r));
            hits.push(r, act);
            x += w + 6;
        }
    }

    let close_r = Rect::new(rect.right() - 34, rect.y + 6, 26, 26);
    ui::button(canvas, text, theme, close_r, "×", None, false, prev_hits.hover_rect(ptr.0, ptr.1, close_r));
    hits.push(close_r, Action::ToggleFind(state.find_replace_open));
    spot
}

fn draw_outline_pane(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &EditorState,
    rect: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    ui::sidebar_surface(canvas, theme, rect);
    let ptr = state.last_pointer;
    text.draw(canvas, "文档大纲", (rect.x + 14) as f32, (rect.y + 24) as f32, 13.0, theme.text, Face::Bold);
    canvas.fill(Rect::new(rect.x + 12, rect.y + 34, rect.w - 24, 1), theme.panel_border);

    let items = state.doc.outline_headings();
    if items.is_empty() {
        text.draw(canvas, "暂无标题段落", (rect.x + 14) as f32, (rect.y + 62) as f32, 11.5, theme.text_muted, Face::Ui);
        return;
    }

    let cur_blk = state.doc.caret().block_idx();
    let list_clip = Rect::new(rect.x + 6, rect.y + 40, rect.w - 12, rect.h - 46);
    let mut y = list_clip.y;
    canvas.with_clip(list_clip, |canvas| {
        for item in items {
            if y > list_clip.bottom() {
                break;
            }
            let row = Rect::new(list_clip.x, y, list_clip.w, 28);
            let active = cur_blk == item.block;
            let hov = prev_hits.hover_rect(ptr.0, ptr.1, row);
            if active || hov {
                ui::list_item_bg(canvas, theme, row, active, hov);
            }
            let indent = (item.level.saturating_sub(1) as i32) * 12;
            let face = if item.level == 1 { Face::Bold } else { Face::Ui };
            let shown = text.truncate(&item.title, (row.w - 14 - indent) as f32, 11.5, face);
            text.draw(
                canvas,
                &shown,
                (row.x + 8 + indent) as f32,
                (row.y + 18) as f32,
                11.5,
                if active { theme.accent } else { theme.text },
                face,
            );
            hits.push_clipped(row, list_clip, Action::JumpOutline(item.block));
            y += 30;
        }
    });
}

fn draw_ruler(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &EditorState,
    rect: Rect,
) {
    canvas.fill(rect, theme.panel);
    canvas.fill(Rect::new(rect.x, rect.bottom() - 1, rect.w, 1), theme.panel_border);
    let Some(layout) = &state.layout else {
        return;
    };
    let Some(p) = layout.pages.first() else {
        return;
    };
    let pw = (p.width as f32 * state.zoom).round() as i32;
    let vp_w = (rect.w).max(24);
    let px = rect.x + ((vp_w - pw).max(24)) / 2;
    let body_l = px + (p.margin_left as f32 * state.zoom).round() as i32;
    let body_r = px + pw - (p.margin_right as f32 * state.zoom).round() as i32;

    canvas.with_clip(rect, |canvas| {
        canvas.fill(Rect::new(px, rect.y + 3, pw, rect.h - 6), theme.window_alt);
        canvas.fill(Rect::new(body_l, rect.y + 3, (body_r - body_l).max(1), rect.h - 6), theme.panel);
        let px_per_cm = (96.0 / 2.54) * state.zoom;
        let max_cm = ((pw - p.margin_left - p.margin_right) as f32 / (96.0 / 2.54)).ceil() as i32 + 2;
        for cm in 0..=max_cm {
            let tx = body_l + (cm as f32 * px_per_cm).round() as i32;
            if tx > rect.right() {
                break;
            }
            if tx >= rect.x {
                canvas.fill(Rect::new(tx, rect.bottom() - 9, 1, 8), theme.text_muted);
                let num = format!("{cm}");
                text.draw(canvas, &num, (tx + 3) as f32, (rect.y + 13) as f32, 9.5, theme.text_muted, Face::Ui);
            }
            let half = tx + (px_per_cm * 0.5).round() as i32;
            if half < rect.right() && half >= rect.x {
                canvas.fill(Rect::new(half, rect.bottom() - 5, 1, 4), theme.panel_border);
            }
        }
    });
}

fn draw_document_pages(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut EditorState,
    vp: Rect,
    _prev_hits: &Hits,
) -> Option<Rect> {
    let layout = match state.layout.clone() {
        Some(l) => l,
        None => DocLayout::build(&state.doc, text),
    };
    let scroll_y = state.scroll.offset_i32();
    let zoom = state.zoom;
    let mut caret_screen_spot = None;
    let caret_geom = layout.caret_geom(&state.doc, state.doc.caret(), text);

    let total_pages = layout.pages.len().max(1);
    let setup = state.doc.setup.clone();
    let selection = state.doc.selection;
    let find_matches = state.find_matches.clone();
    let find_active = state.find_open;
    let find_index = state.find_index;
    let caret_elapsed = state.caret_blink_at.elapsed().as_millis();
    let caret_visible = (caret_elapsed % 1060) < 620;

    canvas.with_clip(vp, |canvas| {
        for page in &layout.pages {
            let pw = (page.width as f32 * zoom).round() as i32;
            let ph = (page.height as f32 * zoom).round() as i32;
            let px = vp.x + ((vp.w - pw).max(24)) / 2;
            let py = vp.y + layout.page_top_y(page.page_index, zoom, PAGE_GAP) - scroll_y;
            let page_rect = Rect::new(px, py, pw, ph);
            if page_rect.bottom() < vp.y - 20 || page_rect.y > vp.bottom() + 20 {
                continue;
            }

            // Paper drop shadow & crisp white sheet
            canvas.fill_alpha(Rect::new(px + 3, py + 4, pw, ph), 0x0f172a, 0.16);
            canvas.fill_alpha(Rect::new(px + 1, py + 2, pw, ph), 0x0f172a, 0.10);
            canvas.fill(page_rect, 0xffffff);
            canvas.rounded_border(page_rect, 1, 0xcbd5e1, 0.85, 1);

            // Subtle corner margin crop marks
            let ml = px + (page.margin_left as f32 * zoom).round() as i32;
            let mr = px + pw - (page.margin_right as f32 * zoom).round() as i32;
            let mt = py + (page.margin_top as f32 * zoom).round() as i32;
            let mb = py + ph - (page.margin_bottom as f32 * zoom).round() as i32;
            let mc = 0xcbd5e1;
            for (cx, cy, dx, dy) in [
                (ml, mt, -8, -8),
                (mr, mt, 8, -8),
                (ml, mb, -8, 8),
                (mr, mb, 8, 8),
            ] {
                let x0 = cx.min(cx + dx);
                canvas.fill_alpha(Rect::new(x0, cy, dx.abs(), 1), mc, 0.75);
                let y0 = cy.min(cy + dy);
                canvas.fill_alpha(Rect::new(cx, y0, 1, dy.abs()), mc, 0.75);
            }

            // Header & Footer
            let hf_px = (10.5 * zoom).clamp(7.0, 18.0);
            if !setup.header.is_empty() {
                let tw = text.measure(&setup.header, hf_px, Face::Ui);
                text.draw(
                    canvas,
                    &setup.header,
                    px as f32 + (pw as f32 - tw) * 0.5,
                    (mt - (14.0 * zoom) as i32) as f32,
                    hf_px,
                    0x64748b,
                    Face::Ui,
                );
            }
            let footer_str = match (setup.footer.is_empty(), setup.show_page_numbers) {
                (false, true) => format!("{}   ·   第 {} / {} 页", setup.footer, page.page_index + 1, total_pages),
                (true, true) => format!("第 {} / {} 页", page.page_index + 1, total_pages),
                (false, false) => setup.footer.clone(),
                (true, false) => String::new(),
            };
            if !footer_str.is_empty() {
                let tw = text.measure(&footer_str, hf_px, Face::Ui);
                text.draw(
                    canvas,
                    &footer_str,
                    px as f32 + (pw as f32 - tw) * 0.5,
                    (mb + (24.0 * zoom) as i32) as f32,
                    hf_px,
                    0x64748b,
                    Face::Ui,
                );
            }

            // Find highlights
            if find_active && !find_matches.is_empty() {
                for (m_idx, m) in find_matches.iter().enumerate() {
                    let sel = crate::doc::Selection {
                        anchor: m.start,
                        focus: m.end,
                    };
                    for sr in layout.selection_rects(&state.doc, sel, page.page_index, text) {
                        let dr = Rect::new(
                            px + (sr.x as f32 * zoom).round() as i32,
                            py + (sr.y as f32 * zoom).round() as i32,
                            (sr.w as f32 * zoom).round().max(3.0) as i32,
                            (sr.h as f32 * zoom).round().max(12.0) as i32,
                        );
                        let color = if m_idx == find_index { 0xf59e0b } else { 0xfde047 };
                        canvas.fill_alpha(dr, color, if m_idx == find_index { 0.48 } else { 0.30 });
                    }
                }
            }

            // Selection highlights
            for sr in layout.selection_rects(&state.doc, selection, page.page_index, text) {
                let dr = Rect::new(
                    px + (sr.x as f32 * zoom).round() as i32,
                    py + (sr.y as f32 * zoom).round() as i32,
                    (sr.w as f32 * zoom).round().max(4.0) as i32,
                    (sr.h as f32 * zoom).round().max(12.0) as i32,
                );
                canvas.fill_alpha(dr, theme.accent, 0.24);
            }

            // Page items
            for item in &page.items {
                match item {
                    PageItem::ParagraphLine {
                        kind_tag,
                        line,
                        quote_bar,
                        ..
                    } => {
                        if *quote_bar {
                            let bx = px + ((line.x_left - 12.0) * zoom).round() as i32;
                            let by = py + (line.y * zoom).round() as i32;
                            let bh = (line.height * zoom).round().max(14.0) as i32;
                            canvas.fill(Rect::new(bx, by + 2, 3, bh - 4), 0x3b82f6);
                        }
                        match kind_tag {
                            LinePrefix::Bullet => {
                                let bx = px + ((line.x_left - 14.0) * zoom).round() as i32;
                                let by = py + ((line.y + line.baseline - 5.5) * zoom).round() as i32;
                                canvas.rounded_rect(Rect::new(bx, by, 5, 5), 2, 0x334155, 1.0);
                            }
                            LinePrefix::Numbered(n) => {
                                let nx = px as f32 + (line.x_left - 20.0) * zoom;
                                let ny = py as f32 + (line.y + line.baseline) * zoom;
                                let np = (14.5 * zoom).clamp(9.0, 22.0);
                                text.draw(canvas, &format!("{n}."), nx, ny, np, 0x334155, Face::Bold);
                            }
                            LinePrefix::None => {}
                        }
                        draw_layout_line(canvas, text, line, px, py, zoom);
                    }
                    PageItem::Divider { x, y, w, h, .. } => {
                        let dx = px + (*x * zoom).round() as i32;
                        let dy = py + ((*y + *h * 0.5) * zoom).round() as i32;
                        let dw = (*w * zoom).round() as i32;
                        canvas.fill(Rect::new(dx, dy, dw, 1), 0xcbd5e1);
                    }
                    PageItem::TableRow(tr) => {
                        for cell in &tr.cells {
                            let cr = Rect::new(
                                px + (cell.x * zoom).round() as i32,
                                py + (tr.y * zoom).round() as i32,
                                (cell.width * zoom).round() as i32,
                                (tr.height * zoom).round() as i32,
                            );
                            if let Some(bg) = cell.shading {
                                canvas.fill(cr, bg);
                            }
                            for line in &cell.lines {
                                draw_layout_line(canvas, text, line, px, py, zoom);
                            }
                            canvas.fill(Rect::new(cr.x, cr.y, cr.w, 1), 0xcbd5e1);
                            canvas.fill(Rect::new(cr.x, cr.y, 1, cr.h), 0xcbd5e1);
                            canvas.fill(Rect::new(cr.x, cr.bottom() - 1, cr.w, 1), 0xcbd5e1);
                            canvas.fill(Rect::new(cr.right() - 1, cr.y, 1, cr.h), 0xcbd5e1);
                        }
                    }
                    PageItem::Image {
                        block,
                        x,
                        y,
                        w,
                        h,
                        caption_y,
                        caption,
                    } => {
                        let dr = Rect::new(
                            px + (*x * zoom).round() as i32,
                            py + (*y * zoom).round() as i32,
                            (*w * zoom).round().max(16.0) as i32,
                            (*h * zoom).round().max(16.0) as i32,
                        );
                        if let Some(BlockKind::Image(img)) = state.doc.blocks.get(*block).map(|b| &b.kind) {
                            let id = img.id;
                            let bytes = img.bytes.clone();
                            if let Some(sprite) = state.decode_cached_image(id, &bytes) {
                                let scaled = sprite.scale_bilinear(dr.w as u32, dr.h as u32);
                                canvas.blit_sprite(dr.x, dr.y, &scaled);
                            }
                        }
                        canvas.rounded_border(dr, 2, 0xcbd5e1, 0.85, 1);
                        if !caption.trim().is_empty() {
                            let cpx = (11.5 * zoom).clamp(8.0, 20.0);
                            let cw = text.measure(caption, cpx, Face::Italic);
                            text.draw(
                                canvas,
                                caption,
                                dr.x as f32 + (dr.w as f32 - cw) * 0.5,
                                py as f32 + *caption_y * zoom,
                                cpx,
                                0x475569,
                                Face::Italic,
                            );
                        }
                    }
                }
            }

            // Caret + IME preedit on active page
            if page.page_index == caret_geom.page_index {
                let cx = px + (caret_geom.x * zoom).round() as i32;
                let cy = py + (caret_geom.y * zoom).round() as i32;
                let ch = (caret_geom.height * zoom).round().max(16.0) as i32;
                if state.focused == FocusedField::Canvas && !state.preedit.is_empty() {
                    let pre_px = (state.doc.active_style.px() * zoom).clamp(9.0, 48.0);
                    let pw_pre = text.measure(&state.preedit, pre_px, Face::Ui).ceil() as i32 + 4;
                    canvas.fill(Rect::new(cx, cy + 1, pw_pre, ch - 2), 0xeff6ff);
                    text.draw(
                        canvas,
                        &state.preedit,
                        (cx + 2) as f32,
                        cy as f32 + ch as f32 * 0.78,
                        pre_px,
                        theme.accent,
                        Face::Ui,
                    );
                    canvas.fill(Rect::new(cx, cy + ch - 2, pw_pre, 2), theme.accent);
                }
                if state.focused == FocusedField::Canvas && caret_visible && state.modal.is_none() {
                    canvas.fill(Rect::new(cx, cy + 2, 2, (ch - 4).max(12)), theme.accent);
                }
                caret_screen_spot = Some(Rect::new(cx, cy, 2, ch));
            }
        }
    });

    caret_screen_spot
}

fn draw_layout_line(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    line: &crate::layout::LayoutLine,
    px: i32,
    py: i32,
    zoom: f32,
) {
    let base_y = py as f32 + (line.y + line.baseline) * zoom;
    for sp in &line.spans {
        let sx = px as f32 + sp.x * zoom;
        let sw = sp.width * zoom;
        let sp_px = sp.style.px() * zoom;
        if let Some(bg) = sp.style.highlight {
            canvas.fill(
                Rect::new(
                    sx.round() as i32,
                    py + (line.y * zoom).round() as i32 + 2,
                    sw.round().max(2.0) as i32,
                    (line.height * zoom).round().max(12.0) as i32 - 4,
                ),
                bg,
            );
        }
        text.draw(
            canvas,
            &sp.text,
            sx,
            base_y,
            sp_px,
            sp.style.color,
            sp.style.face(),
        );
        if sp.style.underline {
            let uy = (base_y + 2.0 * zoom).round() as i32;
            canvas.fill(Rect::new(sx.round() as i32, uy, sw.round().max(2.0) as i32, 1), sp.style.color);
        }
        if sp.style.strikethrough {
            let sy = (base_y - sp_px * 0.30).round() as i32;
            canvas.fill(Rect::new(sx.round() as i32, sy, sw.round().max(2.0) as i32, 1), sp.style.color);
        }
    }
}

fn draw_status_bar(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &EditorState,
    rect: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    ui::status_bar_surface(canvas, theme, rect);
    let ptr = state.last_pointer;
    let stats = state.doc.stats();
    let pages = state.layout.as_ref().map_or(1, |l| l.pages.len()).max(1);
    let cur_page = state
        .layout
        .as_ref()
        .map_or(1, |l| l.caret_geom(&state.doc, state.doc.caret(), text).page_index + 1);
    let kind_lbl = state.doc.current_block_kind().label();

    let active_font = state.active_font_display_name();
    let info = format!(
        "第 {cur_page} / {pages} 页  ·  {} 词  ·  {} 字符  ·  {kind_lbl} ({:.0}pt {active_font})",
        stats.words,
        stats.chars_no_spaces,
        state.doc.active_style.size_pt,
    );
    text.draw(canvas, &info, (rect.x + 14) as f32, (rect.y + 17) as f32, 11.0, theme.text_muted, Face::Ui);

    let zoom_out = Rect::new(rect.right() - 122, rect.y + 3, 24, 20);
    let zoom_lbl = Rect::new(rect.right() - 94, rect.y + 3, 52, 20);
    let zoom_in = Rect::new(rect.right() - 38, rect.y + 3, 24, 20);

    chip_button(canvas, text, theme, zoom_out, "-", false, prev_hits.hover_rect(ptr.0, ptr.1, zoom_out), Face::Bold);
    hits.push(zoom_out, Action::SetZoom((state.zoom - 0.15).max(0.6)));
    let ztext = format!("{:.0}%", state.zoom * 100.0);
    chip_button(canvas, text, theme, zoom_lbl, &ztext, false, prev_hits.hover_rect(ptr.0, ptr.1, zoom_lbl), Face::Ui);
    hits.push(zoom_lbl, Action::SetZoom(1.0));
    chip_button(canvas, text, theme, zoom_in, "+", false, prev_hits.hover_rect(ptr.0, ptr.1, zoom_in), Face::Bold);
    hits.push(zoom_in, Action::SetZoom((state.zoom + 0.15).min(1.6)));

    let info_end = rect.x + 14 + text.measure(&info, 11.0, Face::Ui) as i32 + 24;
    if info_end + 80 < zoom_out.x {
        let shown = text.truncate(&state.status, (zoom_out.x - info_end - 16) as f32, 11.0, Face::Ui);
        text.draw(canvas, &shown, info_end as f32, (rect.y + 17) as f32, 11.0, theme.text, Face::Ui);
    }
}

fn draw_font_dropdown_menu(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &EditorState,
    content: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    use crate::doc::FontFamily;

    // Click anywhere outside to close the font dropdown menu.
    hits.push(content, Action::CloseFontMenu);

    let ptr = state.last_pointer;
    let anchor = state.font_menu_anchor;
    let max_visible_fonts = state.available_fonts.len().clamp(1, 8);
    let menu_w = 248;
    let menu_h = 104 + (max_visible_fonts as i32 + 1) * 28;
    let mx = anchor.x.clamp(content.x + 8, (content.right() - menu_w - 8).max(content.x + 8));
    let my = (anchor.bottom() + 4).min((content.bottom() - menu_h - 8).max(content.y + 8));
    let menu = Rect::new(mx, my, menu_w, menu_h);

    let rad = theme.radius_control().max(8);
    canvas.rounded_drop_shadow(menu, rad, 16, 4, 0.24);
    canvas.rounded_rect(menu, rad, theme.window_high, 1.0);
    canvas.rounded_border(menu, rad, theme.window_border, 0.95, 1);
    hits.push(menu, Action::FocusField(state.focused));

    text.draw(
        canvas,
        "字体角色 (uxconfd 通道)",
        (menu.x + 12) as f32,
        (menu.y + 20) as f32,
        10.5,
        theme.text_muted,
        Face::Bold,
    );

    let roles = [
        (FontFamily::Serif, "文档正文"),
        (FontFamily::Sans, "无衬线"),
        (FontFamily::Mono, "等宽代码"),
    ];
    let pill_w = (menu.w - 24 - 8) / 3;
    for (idx, (role, label)) in roles.into_iter().enumerate() {
        let rx = menu.x + 12 + idx as i32 * (pill_w + 4);
        let r = Rect::new(rx, menu.y + 26, pill_w, 24);
        let active = state.doc.active_style.font == role;
        let hov = prev_hits.hover_rect(ptr.0, ptr.1, r);
        chip_button(canvas, text, theme, r, label, active, hov, Face::Bold);
        hits.push(r, Action::SelectFontRole(role));
    }

    canvas.fill(
        Rect::new(menu.x + 10, menu.y + 58, menu.w - 20, 1),
        mix(theme.panel_border, theme.window_high, 0.35),
    );

    let prop_name = match state.doc.active_style.font {
        FontFamily::Serif => "font:/document-family",
        FontFamily::Sans => "font:/ui-family",
        FontFamily::Mono => "font:/monospace-family",
    };
    text.draw(
        canvas,
        &format!("系统已安装字体 ({prop_name})"),
        (menu.x + 12) as f32,
        (menu.y + 75) as f32,
        10.5,
        theme.text_muted,
        Face::Bold,
    );

    let current_cfg = state.uxconf_font_for_role(state.doc.active_style.font);
    let mut iy = menu.y + 82;

    // Row 0: System default ("")
    let def_label = match state.default_scanned_family_for_role(state.doc.active_style.font) {
        Some(name) => format!("系统默认 ({name})"),
        None => "系统默认".to_string(),
    };
    let def_row = Rect::new(menu.x + 6, iy, menu.w - 12, 26);
    let def_selected = current_cfg.is_empty();
    let def_hov = prev_hits.hover_rect(ptr.0, ptr.1, def_row);
    if def_selected || def_hov {
        canvas.rounded_rect(
            def_row,
            6,
            if def_selected {
                mix(theme.window_high, theme.accent, 0.16)
            } else {
                theme.panel_hover
            },
            1.0,
        );
    }
    let shown_def = text.truncate(&def_label, (def_row.w - 24) as f32, 11.5, Face::Ui);
    text.draw(
        canvas,
        &shown_def,
        (def_row.x + 10) as f32,
        (def_row.y + 17) as f32,
        11.5,
        if def_selected { theme.accent } else { theme.text },
        if def_selected { Face::Bold } else { Face::Ui },
    );
    hits.push(def_row, Action::SelectSystemFont(String::new()));
    iy += 28;

    for family in state.available_fonts.iter().take(max_visible_fonts) {
        let row = Rect::new(menu.x + 6, iy, menu.w - 12, 26);
        let selected = current_cfg == family.as_str();
        let hov = prev_hits.hover_rect(ptr.0, ptr.1, row);
        if selected || hov {
            canvas.rounded_rect(
                row,
                6,
                if selected {
                    mix(theme.window_high, theme.accent, 0.16)
                } else {
                    theme.panel_hover
                },
                1.0,
            );
        }
        let shown = text.truncate(family, (row.w - 24) as f32, 11.5, Face::Ui);
        text.draw(
            canvas,
            &shown,
            (row.x + 10) as f32,
            (row.y + 17) as f32,
            11.5,
            if selected { theme.accent } else { theme.text },
            if selected { Face::Bold } else { Face::Ui },
        );
        hits.push(row, Action::SelectSystemFont(family.clone()));
        iy += 28;
    }
}

fn draw_modal(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &EditorState,
    content: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) -> Option<Rect> {
    let Some(m) = state.modal.as_ref() else {
        return None;
    };
    canvas.fill_alpha(content, 0x0f172a, 0.56);
    let ptr = state.last_pointer;
    let mut ime = None;

    match m.kind {
        ModalKind::InsertTable => {
            let dlg = Rect::new(content.x + (content.w - 340) / 2, content.y + (content.h - 210) / 2, 340, 210);
            ui::popup_surface(canvas, theme, dlg, 10);
            text.draw(canvas, "插入表格", (dlg.x + 20) as f32, (dlg.y + 30) as f32, 15.0, theme.text, Face::Bold);
            text.draw(canvas, &format!("行数: {}", m.table_rows), (dlg.x + 24) as f32, (dlg.y + 78) as f32, 13.0, theme.text, Face::Ui);
            text.draw(canvas, &format!("列数: {}", m.table_cols), (dlg.x + 24) as f32, (dlg.y + 118) as f32, 13.0, theme.text, Face::Ui);

            for (y_off, dr, dc) in [(58, 1, 0), (58, -1, 0), (98, 0, 1), (98, 0, -1)] {
                let is_plus = dr > 0 || dc > 0;
                let r = Rect::new(dlg.x + if is_plus { 190 } else { 150 }, dlg.y + y_off, 32, 26);
                ui::button(canvas, text, theme, r, if is_plus { "+" } else { "-" }, None, false, prev_hits.hover_rect(ptr.0, ptr.1, r));
                hits.push(r, Action::ModalTableDelta(dr, dc));
            }
            let cancel = Rect::new(dlg.right() - 184, dlg.bottom() - 44, 78, 30);
            let ok = Rect::new(dlg.right() - 96, dlg.bottom() - 44, 78, 30);
            ui::button(canvas, text, theme, cancel, "取消", None, false, prev_hits.hover_rect(ptr.0, ptr.1, cancel));
            ui::button(canvas, text, theme, ok, "插入", None, true, prev_hits.hover_rect(ptr.0, ptr.1, ok));
            hits.push(cancel, Action::CloseModal);
            hits.push(ok, Action::ModalConfirm);
        }
        ModalKind::HeaderFooter => {
            let dlg = Rect::new(content.x + (content.w - 460) / 2, content.y + (content.h - 240) / 2, 460, 240);
            ui::popup_surface(canvas, theme, dlg, 10);
            text.draw(canvas, "页眉与页脚设置", (dlg.x + 20) as f32, (dlg.y + 30) as f32, 15.0, theme.text, Face::Bold);
            text.draw(canvas, "页眉文字", (dlg.x + 20) as f32, (dlg.y + 62) as f32, 12.0, theme.text_muted, Face::Ui);
            let hr = Rect::new(dlg.x + 20, dlg.y + 68, dlg.w - 40, 32);
            let hf = state.focused == FocusedField::HeaderInput;
            let hs = ui::text_input_ime(canvas, text, theme, hr, &m.header_text, if hf { &state.preedit } else { "" }, "输入页眉内容...", hf, None);
            if hf {
                ime = Some(hs);
            }
            hits.push(hr, Action::FocusField(FocusedField::HeaderInput));

            text.draw(canvas, "页脚文字", (dlg.x + 20) as f32, (dlg.y + 124) as f32, 12.0, theme.text_muted, Face::Ui);
            let fr = Rect::new(dlg.x + 20, dlg.y + 130, dlg.w - 40, 32);
            let ff = state.focused == FocusedField::FooterInput;
            let fs = ui::text_input_ime(canvas, text, theme, fr, &m.footer_text, if ff { &state.preedit } else { "" }, "输入页脚内容...", ff, None);
            if ff {
                ime = Some(fs);
            }
            hits.push(fr, Action::FocusField(FocusedField::FooterInput));

            let cancel = Rect::new(dlg.right() - 184, dlg.bottom() - 44, 78, 30);
            let ok = Rect::new(dlg.right() - 96, dlg.bottom() - 44, 78, 30);
            ui::button(canvas, text, theme, cancel, "取消", None, false, prev_hits.hover_rect(ptr.0, ptr.1, cancel));
            ui::button(canvas, text, theme, ok, "应用", None, true, prev_hits.hover_rect(ptr.0, ptr.1, ok));
            hits.push(cancel, Action::CloseModal);
            hits.push(ok, Action::ModalConfirm);
        }
        ModalKind::OpenFile | ModalKind::SaveAs | ModalKind::InsertImage => {
            let dlg = Rect::new(content.x + (content.w - 560) / 2, content.y + (content.h - 420) / 2, 560, 420);
            ui::popup_surface(canvas, theme, dlg, 10);
            let title = match m.kind {
                ModalKind::OpenFile => "打开文档 (.docx / .md / .html / .txt)",
                ModalKind::SaveAs => "保存 / 导出文档",
                ModalKind::InsertImage => "插入本地图片 (.png / .jpg)",
                _ => "",
            };
            text.draw(canvas, title, (dlg.x + 20) as f32, (dlg.y + 30) as f32, 15.0, theme.text, Face::Bold);
            let dir_s = text.truncate(&m.dir.display().to_string(), (dlg.w - 40) as f32, 11.5, Face::Mono);
            text.draw(canvas, &dir_s, (dlg.x + 20) as f32, (dlg.y + 52) as f32, 11.5, theme.text_muted, Face::Mono);

            let list_r = Rect::new(dlg.x + 20, dlg.y + 64, dlg.w - 40, 224);
            ui::table_body_bg(canvas, theme, list_r);
            canvas.rounded_border(list_r, 6, theme.panel_border, 0.85, 1);

            let visible_rows = 8usize;
            canvas.with_clip(list_r, |canvas| {
                for (slot, (idx, item)) in m
                    .entries
                    .iter()
                    .enumerate()
                    .skip(m.scroll_offset)
                    .take(visible_rows)
                    .enumerate()
                {
                    let row = Rect::new(list_r.x + 4, list_r.y + 4 + slot as i32 * 27, list_r.w - 8, 25);
                    let sel = m.selected == Some(idx);
                    let hov = prev_hits.hover_rect(ptr.0, ptr.1, row);
                    if sel || hov {
                        ui::list_item_bg(canvas, theme, row, sel, hov);
                    }
                    icons::draw(
                        canvas,
                        if item.is_dir { Icon::Folder } else { icons::Icon::for_file(&item.path, false) },
                        row.x + 6,
                        row.y + 4,
                        16,
                        None,
                    );
                    let lbl = text.truncate(&item.name, (row.w - 36) as f32, 12.0, Face::Ui);
                    text.draw(canvas, &lbl, (row.x + 28) as f32, (row.y + 17) as f32, 12.0, theme.text, Face::Ui);
                    hits.push_clipped(
                        row,
                        list_r,
                        if item.is_dir || m.selected == Some(idx) {
                            Action::ModalOpenEntry(idx)
                        } else {
                            Action::ModalSelectEntry(idx)
                        },
                    );
                }
            });

            if m.kind == ModalKind::SaveAs {
                let mut fx = dlg.x + 20;
                for (fmt, lbl) in [
                    (DocFormat::Docx, ".docx 文档"),
                    (DocFormat::Markdown, ".md Markdown"),
                    (DocFormat::Html, ".html 网页"),
                    (DocFormat::Text, ".txt 纯文本"),
                ] {
                    let fr = Rect::new(fx, dlg.y + 298, 108, 26);
                    chip_button(canvas, text, theme, fr, lbl, m.format == fmt, prev_hits.hover_rect(ptr.0, ptr.1, fr), Face::Ui);
                    hits.push(fr, Action::ModalFormat(fmt));
                    fx += 114;
                }
            }

            let name_r = Rect::new(dlg.x + 20, dlg.y + 332, dlg.w - 40, 32);
            let nf = state.focused == FocusedField::DialogName;
            let ns = ui::text_input_ime(canvas, text, theme, name_r, &m.filename, if nf { &state.preedit } else { "" }, "输入文件名...", nf, None);
            if nf {
                ime = Some(ns);
            }
            hits.push(name_r, Action::FocusField(FocusedField::DialogName));

            let cancel = Rect::new(dlg.right() - 184, dlg.bottom() - 44, 78, 30);
            let ok = Rect::new(dlg.right() - 96, dlg.bottom() - 44, 78, 30);
            let ok_lbl = match m.kind {
                ModalKind::SaveAs => "保存",
                ModalKind::InsertImage => "插入",
                _ => "打开",
            };
            ui::button(canvas, text, theme, cancel, "取消", None, false, prev_hits.hover_rect(ptr.0, ptr.1, cancel));
            ui::button(canvas, text, theme, ok, ok_lbl, None, true, prev_hits.hover_rect(ptr.0, ptr.1, ok));
            hits.push(cancel, Action::CloseModal);
            hits.push(ok, Action::ModalConfirm);
        }
    }

    let _ = (Instant::now, mix);
    ime
}

#[cfg(test)]
mod tests {
    #[test]
    fn editor_crate_has_zero_skin_coupling() {
        let app_src = include_str!("app.rs");
        let draw_src = include_str!("draw.rs");
        let forbidden = [
            concat!("is_", "win98"),
            concat!("is_", "vortex"),
            concat!("theme.", "skin"),
            concat!("draw_", "aero"),
        ];
        for bad in forbidden {
            assert!(!app_src.contains(bad), "app.rs must not mention {bad}");
            assert!(!draw_src.contains(bad), "draw.rs must not mention {bad}");
        }
    }
}
