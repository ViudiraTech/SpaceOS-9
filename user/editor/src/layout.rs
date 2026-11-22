//
//       user/editor/src/layout.rs
//       Real-time WYSIWYG paginated A4/Letter layout engine and hit testing
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use crate::doc::{
    runs_char_len, style_at_offset, Align, BlockKind, DocPos, Document, Selection, TextStyle,
};
use spui_ui::draw::Rect;
use spui_ui::text::TextRenderer;

#[derive(Clone, Debug)]
pub struct LayoutSpan {
    pub x: f32,
    pub width: f32,
    pub text: String,
    pub style: TextStyle,
    pub char_start: usize,
    pub char_end: usize,
}

#[derive(Clone, Debug)]
pub struct LayoutLine {
    pub y: f32,
    pub height: f32,
    pub baseline: f32,
    pub x_left: f32,
    pub width: f32,
    pub spans: Vec<LayoutSpan>,
    pub char_start: usize,
    pub char_end: usize,
}

#[derive(Clone, Debug)]
pub struct LayoutTableCell {
    pub col: usize,
    pub x: f32,
    pub width: f32,
    pub shading: Option<u32>,
    pub lines: Vec<LayoutLine>,
}

#[derive(Clone, Debug)]
pub struct LayoutTableRow {
    pub block: usize,
    pub row: usize,
    pub y: f32,
    pub height: f32,
    pub table_x: f32,
    pub table_w: f32,
    pub cells: Vec<LayoutTableCell>,
}

#[derive(Clone, Debug)]
pub enum PageItem {
    ParagraphLine {
        block: usize,
        kind_tag: LinePrefix,
        line: LayoutLine,
        quote_bar: bool,
    },
    Divider {
        block: usize,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    TableRow(LayoutTableRow),
    Image {
        block: usize,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        caption_y: f32,
        caption: String,
    },
}

#[derive(Clone, Debug)]
pub enum LinePrefix {
    None,
    Bullet,
    Numbered(usize),
}

#[derive(Clone, Debug)]
pub struct PageLayout {
    pub page_index: usize,
    pub width: i32,
    pub height: i32,
    pub margin_top: i32,
    pub margin_right: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub items: Vec<PageItem>,
}

#[derive(Clone, Debug)]
pub struct CaretGeom {
    pub page_index: usize,
    pub x: f32,
    pub y: f32,
    pub height: f32,
}

#[derive(Clone, Debug)]
pub struct DocLayout {
    pub pages: Vec<PageLayout>,
    pub block_first_page_y: Vec<(usize, f32)>,
}

impl DocLayout {
    pub fn total_height_scaled(&self, zoom: f32, page_gap: i32) -> i32 {
        if self.pages.is_empty() {
            return 200;
        }
        let mut h = page_gap;
        for p in &self.pages {
            h += (p.height as f32 * zoom).round() as i32 + page_gap;
        }
        h
    }

    pub fn page_top_y(&self, page_index: usize, zoom: f32, page_gap: i32) -> i32 {
        let mut y = page_gap;
        for p in self.pages.iter().take(page_index) {
            y += (p.height as f32 * zoom).round() as i32 + page_gap;
        }
        y
    }

    pub fn block_scroll_target(&self, block: usize, zoom: f32, page_gap: i32) -> f32 {
        let Some(&(p_idx, local_y)) = self.block_first_page_y.get(block) else {
            return 0.0;
        };
        let p_top = self.page_top_y(p_idx, zoom, page_gap) as f32;
        (p_top + local_y * zoom - 16.0).max(0.0)
    }

    pub fn build(doc: &Document, text: &mut TextRenderer) -> Self {
        let (pw, ph) = doc.setup.page_size_px();
        let (mt, mr, mb, ml) = doc.setup.margins_px();
        let body_x = ml as f32;
        let body_w = (pw - ml - mr).max(160) as f32;
        let max_y = (ph - mb).max(mt + 120) as f32;

        let mut pages = vec![PageLayout {
            page_index: 0,
            width: pw,
            height: ph,
            margin_top: mt,
            margin_right: mr,
            margin_bottom: mb,
            margin_left: ml,
            items: Vec::new(),
        }];
        let mut cur_page = 0usize;
        let mut y = mt as f32;
        let mut num_counter = 0usize;
        let mut block_first_page_y = vec![(0usize, mt as f32); doc.blocks.len()];

        let push_new_page = |pages: &mut Vec<PageLayout>, cur_page: &mut usize, y: &mut f32| {
            let next_idx = pages.len();
            pages.push(PageLayout {
                page_index: next_idx,
                width: pw,
                height: ph,
                margin_top: mt,
                margin_right: mr,
                margin_bottom: mb,
                margin_left: ml,
                items: Vec::new(),
            });
            *cur_page = next_idx;
            *y = mt as f32;
        };

        for (b_idx, blk) in doc.blocks.iter().enumerate() {
            if !matches!(blk.kind, BlockKind::NumberedList) {
                num_counter = 0;
            }
            block_first_page_y[b_idx] = (cur_page, y);

            match &blk.kind {
                BlockKind::Divider => {
                    let h = 16.0;
                    if y + h > max_y && !pages[cur_page].items.is_empty() {
                        push_new_page(&mut pages, &mut cur_page, &mut y);
                        block_first_page_y[b_idx] = (cur_page, y);
                    }
                    pages[cur_page].items.push(PageItem::Divider {
                        block: b_idx,
                        x: body_x,
                        y,
                        w: body_w,
                        h,
                    });
                    y += h + 4.0;
                }
                BlockKind::Image(img) => {
                    let (dw, dh) = img.display_size(body_w as i32);
                    let cap_h = if img.caption.trim().is_empty() { 0.0 } else { 22.0 };
                    let total_h = dh as f32 + cap_h + 12.0;
                    if y + total_h > max_y && !pages[cur_page].items.is_empty() {
                        push_new_page(&mut pages, &mut cur_page, &mut y);
                        block_first_page_y[b_idx] = (cur_page, y);
                    }
                    let ix = match img.align {
                        Align::Center => body_x + (body_w - dw as f32) * 0.5,
                        Align::Right => body_x + (body_w - dw as f32),
                        _ => body_x,
                    };
                    pages[cur_page].items.push(PageItem::Image {
                        block: b_idx,
                        x: ix,
                        y,
                        w: dw as f32,
                        h: dh as f32,
                        caption_y: y + dh as f32 + 16.0,
                        caption: img.caption.clone(),
                    });
                    y += total_h;
                }
                BlockKind::Table(tbl) => {
                    let col_ws = tbl.scaled_col_widths(body_w);
                    let table_w: f32 = col_ws.iter().sum();
                    for (r_idx, row) in tbl.rows.iter().enumerate() {
                        let pad_x = 7.0f32;
                        let pad_y = 6.0f32;
                        let mut cell_lines_vec = Vec::with_capacity(row.len());
                        let mut row_h = 26.0f32;
                        for (c_idx, cell) in row.iter().enumerate() {
                            let cw = col_ws.get(c_idx).copied().unwrap_or(100.0);
                            let inner_w = (cw - pad_x * 2.0).max(20.0);
                            let lines = wrap_runs(
                                &cell.runs,
                                text,
                                0.0,
                                0.0,
                                inner_w,
                                cell.align,
                                1.25,
                                TextStyle { size_pt: 10.5, ..TextStyle::default() },
                            );
                            let ch = lines.iter().map(|l| l.height).sum::<f32>() + pad_y * 2.0;
                            row_h = row_h.max(ch);
                            cell_lines_vec.push((cw, cell.shading, lines));
                        }
                        if y + row_h > max_y && !pages[cur_page].items.is_empty() {
                            push_new_page(&mut pages, &mut cur_page, &mut y);
                            if r_idx == 0 {
                                block_first_page_y[b_idx] = (cur_page, y);
                            }
                        }
                        let mut cx = body_x;
                        let mut built_cells = Vec::with_capacity(cell_lines_vec.len());
                        for (c_idx, (cw, shading, mut lines)) in cell_lines_vec.into_iter().enumerate() {
                            let mut ly = y + pad_y;
                            for l in &mut lines {
                                l.x_left += cx + pad_x;
                                l.y = ly;
                                for sp in &mut l.spans {
                                    sp.x += cx + pad_x;
                                }
                                ly += l.height;
                            }
                            built_cells.push(LayoutTableCell {
                                col: c_idx,
                                x: cx,
                                width: cw,
                                shading,
                                lines,
                            });
                            cx += cw;
                        }
                        pages[cur_page].items.push(PageItem::TableRow(LayoutTableRow {
                            block: b_idx,
                            row: r_idx,
                            y,
                            height: row_h,
                            table_x: body_x,
                            table_w,
                            cells: built_cells,
                        }));
                        y += row_h;
                    }
                    y += 10.0;
                }
                _ => {
                    let indent = match blk.kind {
                        BlockKind::BulletList | BlockKind::NumberedList => 24.0,
                        BlockKind::Quote => 18.0,
                        _ => 0.0,
                    };
                    if matches!(blk.kind, BlockKind::NumberedList) {
                        num_counter += 1;
                    }
                    let prefix_num = num_counter;
                    let wrap_x = body_x + indent;
                    let wrap_w = (body_w - indent).max(80.0);
                    let default_st = blk.kind.default_style();
                    let lines = wrap_runs(
                        &blk.runs,
                        text,
                        wrap_x,
                        y,
                        wrap_w,
                        blk.align,
                        blk.line_spacing,
                        default_st,
                    );
                    for (l_idx, mut line) in lines.into_iter().enumerate() {
                        if y + line.height > max_y && !pages[cur_page].items.is_empty() {
                            push_new_page(&mut pages, &mut cur_page, &mut y);
                            if l_idx == 0 {
                                block_first_page_y[b_idx] = (cur_page, y);
                            }
                        }
                        line.y = y;
                        let kind_tag = if l_idx == 0 {
                            match blk.kind {
                                BlockKind::BulletList => LinePrefix::Bullet,
                                BlockKind::NumberedList => LinePrefix::Numbered(prefix_num),
                                _ => LinePrefix::None,
                            }
                        } else {
                            LinePrefix::None
                        };
                        pages[cur_page].items.push(PageItem::ParagraphLine {
                            block: b_idx,
                            kind_tag,
                            quote_bar: matches!(blk.kind, BlockKind::Quote),
                            line: line.clone(),
                        });
                        y += line.height;
                    }
                    y += blk.space_after_pt * (96.0 / 72.0);
                }
            }
        }

        Self {
            pages,
            block_first_page_y,
        }
    }

    pub fn caret_geom(&self, doc: &Document, pos: DocPos, text: &mut TextRenderer) -> CaretGeom {
        let pos = doc.clamp_pos(pos);
        match pos {
            DocPos::Text { block, offset } => {
                let mut best: Option<CaretGeom> = None;
                for p in &self.pages {
                    for item in &p.items {
                        if let PageItem::ParagraphLine { block: b, line, .. } = item {
                            if *b == block {
                                if offset >= line.char_start && offset <= line.char_end {
                                    let x = x_at_offset_in_line(line, offset, text);
                                    best = Some(CaretGeom {
                                        page_index: p.page_index,
                                        x,
                                        y: line.y,
                                        height: line.height,
                                    });
                                } else if best.is_none() {
                                    let x = x_at_offset_in_line(line, line.char_end, text);
                                    best = Some(CaretGeom {
                                        page_index: p.page_index,
                                        x,
                                        y: line.y,
                                        height: line.height,
                                    });
                                }
                            }
                        }
                    }
                }
                best.unwrap_or(CaretGeom {
                    page_index: 0,
                    x: self.pages[0].margin_left as f32,
                    y: self.pages[0].margin_top as f32,
                    height: 20.0,
                })
            }
            DocPos::TableCell {
                block,
                row,
                col,
                offset,
            } => {
                for p in &self.pages {
                    for item in &p.items {
                        if let PageItem::TableRow(tr) = item {
                            if tr.block == block && tr.row == row {
                                if let Some(cell) = tr.cells.get(col) {
                                    for line in &cell.lines {
                                        if offset >= line.char_start && offset <= line.char_end {
                                            let x = x_at_offset_in_line(line, offset, text);
                                            return CaretGeom {
                                                page_index: p.page_index,
                                                x,
                                                y: line.y,
                                                height: line.height,
                                            };
                                        }
                                    }
                                    if let Some(last) = cell.lines.last() {
                                        return CaretGeom {
                                            page_index: p.page_index,
                                            x: x_at_offset_in_line(last, last.char_end, text),
                                            y: last.y,
                                            height: last.height,
                                        };
                                    }
                                }
                            }
                        }
                    }
                }
                CaretGeom {
                    page_index: 0,
                    x: self.pages[0].margin_left as f32,
                    y: self.pages[0].margin_top as f32,
                    height: 20.0,
                }
            }
            DocPos::Object { block, after } => {
                for p in &self.pages {
                    for item in &p.items {
                        match item {
                            PageItem::Image {
                                block: b,
                                x,
                                y,
                                w,
                                h,
                                ..
                            } if *b == block => {
                                return CaretGeom {
                                    page_index: p.page_index,
                                    x: if after { x + w } else { *x },
                                    y: *y,
                                    height: *h,
                                };
                            }
                            PageItem::Divider {
                                block: b,
                                x,
                                y,
                                w,
                                h,
                            } if *b == block => {
                                return CaretGeom {
                                    page_index: p.page_index,
                                    x: if after { x + w } else { *x },
                                    y: *y,
                                    height: *h,
                                };
                            }
                            _ => {}
                        }
                    }
                }
                CaretGeom {
                    page_index: 0,
                    x: self.pages[0].margin_left as f32,
                    y: self.pages[0].margin_top as f32,
                    height: 20.0,
                }
            }
        }
    }

    pub fn hit_test(
        &self,
        doc: &Document,
        page_index: usize,
        lx: f32,
        ly: f32,
        text: &mut TextRenderer,
    ) -> DocPos {
        let Some(page) = self.pages.get(page_index) else {
            return Document::first_pos_in_block(&doc.blocks, 0);
        };
        if page.items.is_empty() {
            return Document::first_pos_in_block(&doc.blocks, 0);
        }

        // First check direct hit within vertical item bounds.
        let mut best_pos: Option<(f32, DocPos)> = None;
        let consider = |best: &mut Option<(f32, DocPos)>, dist: f32, pos: DocPos| {
            if best.as_ref().is_none_or(|(d, _)| dist < *d) {
                *best = Some((dist, pos));
            }
        };

        for item in &page.items {
            match item {
                PageItem::ParagraphLine { block, line, .. } => {
                    let dy = if ly < line.y {
                        line.y - ly
                    } else if ly > line.y + line.height {
                        ly - (line.y + line.height)
                    } else {
                        0.0
                    };
                    if dy < 18.0 {
                        let off = offset_at_x_in_line(line, lx, text);
                        let dx = if lx < line.x_left {
                            (line.x_left - lx) * 0.05
                        } else if lx > line.x_left + line.width {
                            (lx - (line.x_left + line.width)) * 0.05
                        } else {
                            0.0
                        };
                        consider(&mut best_pos, dy + dx, DocPos::Text { block: *block, offset: off });
                    } else {
                        consider(
                            &mut best_pos,
                            dy + 10.0,
                            DocPos::Text {
                                block: *block,
                                offset: if ly < line.y {
                                    line.char_start
                                } else {
                                    line.char_end
                                },
                            },
                        );
                    }
                }
                PageItem::TableRow(tr) => {
                    let dy = if ly < tr.y {
                        tr.y - ly
                    } else if ly > tr.y + tr.height {
                        ly - (tr.y + tr.height)
                    } else {
                        0.0
                    };
                    for cell in &tr.cells {
                        if lx >= cell.x && lx <= cell.x + cell.width && dy == 0.0 {
                            let mut cell_best: Option<(f32, usize)> = None;
                            for line in &cell.lines {
                                let ldy = if ly < line.y {
                                    line.y - ly
                                } else if ly > line.y + line.height {
                                    ly - (line.y + line.height)
                                } else {
                                    0.0
                                };
                                let off = offset_at_x_in_line(line, lx, text);
                                if cell_best.as_ref().is_none_or(|(d, _)| ldy < *d) {
                                    cell_best = Some((ldy, off));
                                }
                            }
                            return DocPos::TableCell {
                                block: tr.block,
                                row: tr.row,
                                col: cell.col,
                                offset: cell_best.map_or(0, |(_, o)| o),
                            };
                        }
                    }
                    let nearest_col = tr
                        .cells
                        .iter()
                        .min_by_key(|c| ((lx - (c.x + c.width * 0.5)).abs() * 10.0) as i32)
                        .map_or(0, |c| c.col);
                    consider(
                        &mut best_pos,
                        dy + 2.0,
                        DocPos::TableCell {
                            block: tr.block,
                            row: tr.row,
                            col: nearest_col,
                            offset: 0,
                        },
                    );
                }
                PageItem::Image { block, x, y, w, h, .. } => {
                    if ly >= *y && ly <= *y + *h && lx >= *x && lx <= *x + *w {
                        return DocPos::Object {
                            block: *block,
                            after: lx > *x + *w * 0.5,
                        };
                    }
                    let dy = if ly < *y { *y - ly } else { (ly - (*y + *h)).max(0.0) };
                    consider(
                        &mut best_pos,
                        dy + 4.0,
                        DocPos::Object {
                            block: *block,
                            after: true,
                        },
                    );
                }
                PageItem::Divider { block, y, h, .. } => {
                    let dy = if ly < *y { *y - ly } else { (ly - (*y + *h)).max(0.0) };
                    consider(
                        &mut best_pos,
                        dy + 4.0,
                        DocPos::Object {
                            block: *block,
                            after: true,
                        },
                    );
                }
            }
        }

        best_pos
            .map(|(_, pos)| pos)
            .unwrap_or_else(|| Document::first_pos_in_block(&doc.blocks, 0))
    }

    pub fn move_vertical(&self, doc: &Document, up: bool, text: &mut TextRenderer) -> DocPos {
        let cur = self.caret_geom(doc, doc.caret(), text);
        let step = if up { -cur.height * 0.85 } else { cur.height * 1.15 };
        let target_y = cur.y + cur.height * 0.4 + step;
        if target_y < 0.0 && cur.page_index > 0 {
            let prev = &self.pages[cur.page_index - 1];
            return self.hit_test(doc, prev.page_index, cur.x, prev.height as f32 - prev.margin_bottom as f32, text);
        }
        let ph = self.pages[cur.page_index].height as f32;
        if target_y > ph && cur.page_index + 1 < self.pages.len() {
            let next = &self.pages[cur.page_index + 1];
            return self.hit_test(doc, next.page_index, cur.x, next.margin_top as f32, text);
        }
        self.hit_test(doc, cur.page_index, cur.x, target_y, text)
    }

    pub fn selection_rects(
        &self,
        doc: &Document,
        sel: Selection,
        page_index: usize,
        text: &mut TextRenderer,
    ) -> Vec<Rect> {
        if sel.is_collapsed() {
            return Vec::new();
        }
        let Some(page) = self.pages.get(page_index) else {
            return Vec::new();
        };
        let (start, end) = sel.ordered();
        let mut rects = Vec::new();

        for item in &page.items {
            match item {
                PageItem::ParagraphLine { block, line, .. } => {
                    let sb = start.block_idx();
                    let eb = end.block_idx();
                    if *block < sb || *block > eb {
                        continue;
                    }
                    let line_len = line.char_end - line.char_start;
                    let total_blk = runs_char_len(&doc.blocks[*block].runs);
                    let s_off = match start {
                        DocPos::Text { block: b, offset } if b == *block => offset,
                        _ => 0,
                    };
                    let e_off = match end {
                        DocPos::Text { block: b, offset } if b == *block => offset,
                        _ => total_blk,
                    };
                    let cs = s_off.max(line.char_start);
                    let ce = e_off.min(line.char_end);
                    if cs < ce || (line_len == 0 && *block > sb && *block < eb) {
                        let x0 = x_at_offset_in_line(line, cs, text);
                        let x1 = if cs == ce {
                            x0 + 6.0
                        } else {
                            x_at_offset_in_line(line, ce, text)
                        };
                        rects.push(Rect::new(
                            x0.round() as i32,
                            line.y.round() as i32,
                            (x1 - x0).round().max(4.0) as i32,
                            line.height.round().max(12.0) as i32,
                        ));
                    }
                }
                PageItem::TableRow(tr) => {
                    if let (
                        DocPos::TableCell {
                            block: b1,
                            row: r1,
                            col: c1,
                            offset: o1,
                        },
                        DocPos::TableCell {
                            block: b2,
                            row: r2,
                            col: c2,
                            offset: o2,
                        },
                    ) = (start, end)
                    {
                        if b1 == b2 && b1 == tr.block && r1 == tr.row && r2 == tr.row && c1 == c2 {
                            if let Some(cell) = tr.cells.get(c1) {
                                for line in &cell.lines {
                                    let cs = o1.max(line.char_start);
                                    let ce = o2.min(line.char_end);
                                    if cs < ce {
                                        let x0 = x_at_offset_in_line(line, cs, text);
                                        let x1 = x_at_offset_in_line(line, ce, text);
                                        rects.push(Rect::new(
                                            x0.round() as i32,
                                            line.y.round() as i32,
                                            (x1 - x0).round().max(4.0) as i32,
                                            line.height.round().max(12.0) as i32,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        rects
    }
}

pub fn wrap_runs(
    runs: &[crate::doc::Run],
    text: &mut TextRenderer,
    x_left: f32,
    start_y: f32,
    max_width: f32,
    align: Align,
    line_spacing: f32,
    default_style: TextStyle,
) -> Vec<LayoutLine> {
    let default_px = default_style.px();
    let default_h = (default_px * line_spacing).max(16.0);
    if runs.iter().all(|r| r.text.is_empty()) {
        return vec![LayoutLine {
            y: start_y,
            height: default_h,
            baseline: default_h * 0.78,
            x_left,
            width: 0.0,
            spans: Vec::new(),
            char_start: 0,
            char_end: 0,
        }];
    }

    // Flatten into measured character units grouped back into style runs per line.
    struct TokenChar {
        ch: char,
        adv: f32,
        style: TextStyle,
        global_off: usize,
    }
    let mut chars_vec = Vec::new();
    let mut g_off = 0usize;
    for r in runs {
        let px = r.style.px();
        let face = r.style.face();
        for ch in r.text.chars() {
            let adv = text.glyph(face, ch, px).advance;
            chars_vec.push(TokenChar {
                ch,
                adv,
                style: r.style,
                global_off: g_off,
            });
            g_off += 1;
        }
    }

    let mut lines = Vec::new();
    let mut idx = 0usize;
    let mut y = start_y;

    while idx < chars_vec.len() {
        let line_start_idx = idx;
        let mut line_w = 0.0f32;
        let mut last_break: Option<(usize, f32)> = None;

        while idx < chars_vec.len() {
            let tc = &chars_vec[idx];
            if line_w + tc.adv > max_width && idx > line_start_idx {
                if let Some((brk, brk_w)) = last_break {
                    idx = brk + 1;
                    line_w = brk_w;
                }
                break;
            }
            line_w += tc.adv;
            if tc.ch == ' ' {
                last_break = Some((idx, line_w));
            }
            idx += 1;
        }
        if idx == line_start_idx && idx < chars_vec.len() {
            line_w += chars_vec[idx].adv;
            idx += 1;
        }

        let slice = &chars_vec[line_start_idx..idx];
        let max_px = slice
            .iter()
            .map(|c| c.style.px())
            .fold(default_px, f32::max);
        let line_h = (max_px * line_spacing).max(16.0);
        let baseline = line_h * 0.78;

        let x_offset = match align {
            Align::Center => ((max_width - line_w) * 0.5).max(0.0),
            Align::Right => (max_width - line_w).max(0.0),
            Align::Left | Align::Justify => 0.0,
        };
        let mut pen_x = x_left + x_offset;
        let mut spans: Vec<LayoutSpan> = Vec::new();
        for tc in slice {
            if let Some(last) = spans.last_mut() {
                if last.style.eq_exact(&tc.style) {
                    last.text.push(tc.ch);
                    last.width += tc.adv;
                    last.char_end = tc.global_off + 1;
                    pen_x += tc.adv;
                    continue;
                }
            }
            spans.push(LayoutSpan {
                x: pen_x,
                width: tc.adv,
                text: tc.ch.to_string(),
                style: tc.style,
                char_start: tc.global_off,
                char_end: tc.global_off + 1,
            });
            pen_x += tc.adv;
        }

        let cs = slice.first().map_or(0, |c| c.global_off);
        let ce = slice.last().map_or(cs, |c| c.global_off + 1);
        lines.push(LayoutLine {
            y,
            height: line_h,
            baseline,
            x_left: x_left + x_offset,
            width: line_w,
            spans,
            char_start: cs,
            char_end: ce,
        });
        y += line_h;
    }

    lines
}

pub fn x_at_offset_in_line(line: &LayoutLine, offset: usize, text: &mut TextRenderer) -> f32 {
    if offset <= line.char_start || line.spans.is_empty() {
        return line.x_left;
    }
    if offset >= line.char_end {
        return line.x_left + line.width;
    }
    for sp in &line.spans {
        if offset <= sp.char_start {
            return sp.x;
        }
        if offset < sp.char_end {
            let local_chars = offset - sp.char_start;
            let px = sp.style.px();
            let face = sp.style.face();
            let mut w = 0.0f32;
            for ch in sp.text.chars().take(local_chars) {
                w += text.glyph(face, ch, px).advance;
            }
            return sp.x + w;
        }
    }
    line.x_left + line.width
}

pub fn offset_at_x_in_line(line: &LayoutLine, target_x: f32, text: &mut TextRenderer) -> usize {
    if target_x <= line.x_left || line.spans.is_empty() {
        return line.char_start;
    }
    if target_x >= line.x_left + line.width {
        return line.char_end;
    }
    for sp in &line.spans {
        if target_x <= sp.x {
            return sp.char_start;
        }
        if target_x < sp.x + sp.width {
            let px = sp.style.px();
            let face = sp.style.face();
            let mut pen = sp.x;
            for (idx, ch) in sp.text.chars().enumerate() {
                let adv = text.glyph(face, ch, px).advance;
                if target_x < pen + adv * 0.5 {
                    return sp.char_start + idx;
                }
                pen += adv;
            }
            return sp.char_end;
        }
    }
    line.char_end
}

pub fn current_line_style(doc: &Document) -> TextStyle {
    match doc.caret() {
        DocPos::Text { block, offset } => doc
            .blocks
            .get(block)
            .map(|b| style_at_offset(&b.runs, offset, b.kind.default_style()))
            .unwrap_or_default(),
        _ => doc.active_style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paginated_layout_breaks_pages_and_hit_tests_accurately() {
        if std::env::var_os("SPUI_SYSROOT").is_none() {
            let candidate = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/rootfs/stage");
            if candidate.is_dir() {
                std::env::set_var("SPUI_SYSROOT", candidate);
            }
        }
        let mut text = TextRenderer::load().expect("load test fonts");
        let mut doc = Document::demo_document();
        // Append extra paragraphs to guarantee multi-page layout.
        for i in 0..18 {
            doc.blocks.push(crate::doc::Block::paragraph(vec![crate::doc::Run::new(
                format!("第 {} 扩展测试段落：验证多页 A4 分页布局与跨页光标命中精度。", i + 1),
                TextStyle::default(),
            )]));
        }
        let layout = DocLayout::build(&doc, &mut text);
        assert!(layout.pages.len() >= 2, "expected multi-page document, got {} page(s)", layout.pages.len());

        // Hit-testing on page 0 at the heading coordinate should hit block 0.
        let p0 = &layout.pages[0];
        let pos = layout.hit_test(&doc, 0, p0.margin_left as f32 + 12.0, p0.margin_top as f32 + 8.0, &mut text);
        assert_eq!(pos.block_idx(), 0);

        let geom = layout.caret_geom(&doc, pos, &mut text);
        assert_eq!(geom.page_index, 0);
        assert!(geom.x >= p0.margin_left as f32);
    }
}
