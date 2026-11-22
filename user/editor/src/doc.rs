//
//       user/editor/src/doc.rs
//       Structured word-processor document model, rich editing, tables, images & undo/redo
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use crc32fast::hash as crc32;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use spui_ui::icons;
use spui_ui::text::Face;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Logical 96-DPI conversion factor: 1 mm = 96 / 25.4 px.
pub const PX_PER_MM: f32 = 96.0 / 25.4;
/// Logical 96-DPI typographic point to px: 1 pt = 96 / 72 = 4/3 px.
pub const PX_PER_PT: f32 = 96.0 / 72.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    A4,
    Letter,
}

impl PageSize {
    pub fn label(self) -> &'static str {
        match self {
            Self::A4 => "A4",
            Self::Letter => "Letter",
        }
    }

    /// Portrait logical dimensions at 96 DPI: A4 794x1123, Letter 816x1056.
    pub fn portrait_px(self) -> (i32, i32) {
        match self {
            Self::A4 => (794, 1123),
            Self::Letter => (816, 1056),
        }
    }

    pub fn dimensions_twips(self) -> (u32, u32) {
        match self {
            Self::A4 => (11906, 16838),
            Self::Letter => (12240, 15840),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarginPreset {
    Normal,
    Narrow,
    Wide,
}

impl MarginPreset {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "标准",
            Self::Narrow => "窄距",
            Self::Wide => "宽距",
        }
    }

    pub fn margins_mm(self) -> PageMargins {
        match self {
            Self::Normal => PageMargins {
                top_mm: 22.0,
                bottom_mm: 22.0,
                left_mm: 24.0,
                right_mm: 24.0,
            },
            Self::Narrow => PageMargins {
                top_mm: 12.7,
                bottom_mm: 12.7,
                left_mm: 12.7,
                right_mm: 12.7,
            },
            Self::Wide => PageMargins {
                top_mm: 25.4,
                bottom_mm: 25.4,
                left_mm: 36.0,
                right_mm: 36.0,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageMargins {
    pub top_mm: f32,
    pub bottom_mm: f32,
    pub left_mm: f32,
    pub right_mm: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageSetup {
    pub size: PageSize,
    pub orientation: Orientation,
    pub margins: PageMargins,
    pub margin_preset: MarginPreset,
    pub header: String,
    pub footer: String,
    pub show_page_numbers: bool,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            size: PageSize::A4,
            orientation: Orientation::Portrait,
            margins: MarginPreset::Normal.margins_mm(),
            margin_preset: MarginPreset::Normal,
            header: String::new(),
            footer: String::new(),
            show_page_numbers: true,
        }
    }
}

impl PageSetup {
    pub fn page_size_px(&self) -> (i32, i32) {
        let (pw, ph) = self.size.portrait_px();
        match self.orientation {
            Orientation::Portrait => (pw, ph),
            Orientation::Landscape => (ph, pw),
        }
    }

    /// Returns `(top, right, bottom, left)` margins in logical 96-DPI pixels.
    pub fn margins_px(&self) -> (i32, i32, i32, i32) {
        (
            (self.margins.top_mm * PX_PER_MM).round() as i32,
            (self.margins.right_mm * PX_PER_MM).round() as i32,
            (self.margins.bottom_mm * PX_PER_MM).round() as i32,
            (self.margins.left_mm * PX_PER_MM).round() as i32,
        )
    }

    pub fn body_width_px(&self) -> i32 {
        let (pw, _) = self.page_size_px();
        let (_, r, _, l) = self.margins_px();
        (pw - l - r).max(160)
    }

    pub fn body_height_px(&self) -> i32 {
        let (_, ph) = self.page_size_px();
        let (t, _, b, _) = self.margins_px();
        (ph - t - b).max(160)
    }

    pub fn set_margin_preset(&mut self, preset: MarginPreset) {
        self.margin_preset = preset;
        self.margins = preset.margins_mm();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontFamily {
    Sans,
    Serif,
    Mono,
}

impl FontFamily {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sans => "思源黑体",
            Self::Serif => "思源宋体",
            Self::Mono => "等宽字体",
        }
    }

    pub fn docx_name(self) -> &'static str {
        match self {
            Self::Sans => "Noto Sans CJK SC",
            Self::Serif => "Noto Serif CJK SC",
            Self::Mono => "Noto Sans Mono CJK SC",
        }
    }

    pub fn from_docx_name(name: &str) -> Self {
        let lower = name.to_ascii_lowercase();
        if lower.contains("mono")
            || lower.contains("consolas")
            || lower.contains("courier")
            || lower.contains("code")
        {
            Self::Mono
        } else if lower.contains("serif")
            || lower.contains("song")
            || lower.contains("ming")
            || lower.contains("times")
            || lower.contains("georgia")
            || name.contains("宋体")
            || name.contains("仿宋")
            || name.contains("楷")
        {
            Self::Serif
        } else {
            Self::Sans
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub font: FontFamily,
    pub size_pt: f32,
    pub color: u32,
    pub highlight: Option<u32>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            // Body text follows `font:/document-family` (the Serif role), so
            // the configured document font applies without extra plumbing.
            font: FontFamily::Serif,
            size_pt: 11.5,
            color: 0x1e293b,
            highlight: None,
        }
    }
}

impl TextStyle {
    pub fn face(&self) -> Face {
        match (self.font, self.bold, self.italic) {
            (FontFamily::Mono, _, _) => Face::Mono,
            (FontFamily::Serif, false, false) => Face::Serif,
            (FontFamily::Serif, true, false) => Face::SerifBold,
            (FontFamily::Serif, false, true) => Face::SerifItalic,
            (FontFamily::Serif, true, true) => Face::SerifBoldItalic,
            (FontFamily::Sans, false, false) => Face::Ui,
            (FontFamily::Sans, true, false) => Face::Bold,
            (FontFamily::Sans, false, true) => Face::Italic,
            (FontFamily::Sans, true, true) => Face::BoldItalic,
        }
    }

    pub fn px(&self) -> f32 {
        (self.size_pt * PX_PER_PT).clamp(8.0, 96.0)
    }

    pub fn eq_exact(&self, other: &Self) -> bool {
        self.bold == other.bold
            && self.italic == other.italic
            && self.underline == other.underline
            && self.strikethrough == other.strikethrough
            && self.font == other.font
            && (self.size_pt - other.size_pt).abs() < 0.05
            && self.color == other.color
            && self.highlight == other.highlight
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub style: TextStyle,
}

impl Run {
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

pub fn normalize_runs(runs: &mut Vec<Run>, default_style: TextStyle) {
    let mut out: Vec<Run> = Vec::with_capacity(runs.len());
    for run in runs.drain(..) {
        if run.text.is_empty() {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if last.style.eq_exact(&run.style) {
                last.text.push_str(&run.text);
                continue;
            }
        }
        out.push(run);
    }
    if out.is_empty() {
        out.push(Run::new("", default_style));
    }
    *runs = out;
}

pub fn runs_plain_text(runs: &[Run]) -> String {
    let total: usize = runs.iter().map(|r| r.text.len()).sum();
    let mut out = String::with_capacity(total);
    for r in runs {
        out.push_str(&r.text);
    }
    out
}

pub fn runs_char_len(runs: &[Run]) -> usize {
    runs.iter().map(|r| r.text.chars().count()).sum()
}

pub fn style_at_offset(runs: &[Run], offset: usize, fallback: TextStyle) -> TextStyle {
    let mut seen = 0usize;
    for r in runs {
        let len = r.text.chars().count();
        if len == 0 {
            continue;
        }
        if offset <= seen + len {
            return r.style;
        }
        seen += len;
    }
    runs.last().map(|r| r.style).unwrap_or(fallback)
}

pub fn slice_runs(runs: &[Run], start: usize, end: usize) -> Vec<Run> {
    if start >= end {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    for r in runs {
        let rlen = r.text.chars().count();
        let rstart = pos;
        let rend = pos + rlen;
        pos = rend;
        if rend <= start || rstart >= end {
            continue;
        }
        let local_s = start.saturating_sub(rstart);
        let local_e = (end - rstart).min(rlen);
        let b0 = char_to_byte(&r.text, local_s);
        let b1 = char_to_byte(&r.text, local_e);
        if b0 < b1 {
            out.push(Run::new(&r.text[b0..b1], r.style));
        }
    }
    out
}

pub fn insert_text_in_runs(
    runs: &mut Vec<Run>,
    offset: usize,
    text: &str,
    style: TextStyle,
) {
    if text.is_empty() {
        return;
    }
    let mut pos = 0usize;
    let mut inserted = false;
    for idx in 0..runs.len() {
        let rlen = runs[idx].text.chars().count();
        if offset <= pos + rlen {
            let local = offset - pos;
            if runs[idx].style.eq_exact(&style) {
                let b = char_to_byte(&runs[idx].text, local);
                runs[idx].text.insert_str(b, text);
            } else if local == 0 {
                runs.insert(idx, Run::new(text, style));
            } else if local >= rlen {
                runs.insert(idx + 1, Run::new(text, style));
            } else {
                let b = char_to_byte(&runs[idx].text, local);
                let tail = runs[idx].text.split_off(b);
                let orig_style = runs[idx].style;
                runs.insert(idx + 1, Run::new(text, style));
                runs.insert(idx + 2, Run::new(tail, orig_style));
            }
            inserted = true;
            break;
        }
        pos += rlen;
    }
    if !inserted {
        runs.push(Run::new(text, style));
    }
    normalize_runs(runs, style);
}

pub fn remove_range_in_runs(
    runs: &mut Vec<Run>,
    start: usize,
    end: usize,
    fallback: TextStyle,
) {
    if start >= end {
        return;
    }
    let prefix = slice_runs(runs, 0, start);
    let total = runs_char_len(runs);
    let suffix = slice_runs(runs, end, total);
    runs.clear();
    runs.extend(prefix);
    runs.extend(suffix);
    normalize_runs(runs, fallback);
}

pub fn apply_style_in_runs(
    runs: &mut Vec<Run>,
    start: usize,
    end: usize,
    fallback: TextStyle,
    mut f: impl FnMut(&mut TextStyle),
) {
    if start >= end {
        return;
    }
    let total = runs_char_len(runs);
    let prefix = slice_runs(runs, 0, start);
    let mut middle = slice_runs(runs, start, end);
    let suffix = slice_runs(runs, end, total);
    for r in &mut middle {
        f(&mut r.style);
    }
    runs.clear();
    runs.extend(prefix);
    runs.extend(middle);
    runs.extend(suffix);
    normalize_runs(runs, fallback);
}

pub fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(b, _)| b)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableCell {
    pub runs: Vec<Run>,
    pub align: Align,
    pub shading: Option<u32>,
}

impl TableCell {
    pub fn with_text(text: &str, style: TextStyle, align: Align, shading: Option<u32>) -> Self {
        Self {
            runs: vec![Run::new(text, style)],
            align,
            shading,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableBlock {
    pub rows: Vec<Vec<TableCell>>,
    pub col_widths: Vec<f32>,
    pub header_row: bool,
}

impl TableBlock {
    pub fn empty(rows: usize, cols: usize, body_width: f32) -> Self {
        let rows = rows.clamp(1, 30);
        let cols = cols.clamp(1, 12);
        let col_w = (body_width / cols as f32).max(48.0);
        let mut grid = Vec::with_capacity(rows);
        for r in 0..rows {
            let mut row = Vec::with_capacity(cols);
            for _c in 0..cols {
                let mut st = TextStyle {
                    size_pt: 10.5,
                    ..TextStyle::default()
                };
                let shading = if r == 0 {
                    st.bold = true;
                    Some(0xeff6ff)
                } else {
                    None
                };
                row.push(TableCell::with_text("", st, Align::Left, shading));
            }
            grid.push(row);
        }
        Self {
            rows: grid,
            col_widths: vec![col_w; cols],
            header_row: true,
        }
    }

    pub fn col_count(&self) -> usize {
        self.rows.first().map_or(0, Vec::len)
    }

    pub fn scaled_col_widths(&self, max_width: f32) -> Vec<f32> {
        let cols = self.col_count().max(1);
        let raw: Vec<f32> = if self.col_widths.len() == cols {
            self.col_widths.iter().map(|w| w.max(36.0)).collect()
        } else {
            vec![(max_width / cols as f32).max(36.0); cols]
        };
        let sum: f32 = raw.iter().sum();
        if sum <= 0.0 || sum > max_width {
            let ratio = max_width / sum.max(1.0);
            raw.into_iter().map(|w| (w * ratio).max(28.0)).collect()
        } else {
            raw
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageBlock {
    pub id: u64,
    pub filename: String,
    pub mime: String,
    pub bytes: Arc<Vec<u8>>,
    pub intrinsic_w: u32,
    pub intrinsic_h: u32,
    pub display_w: u32,
    pub caption: String,
    pub align: Align,
}

impl ImageBlock {
    pub fn display_size(&self, max_body_w: i32) -> (i32, i32) {
        let w = (self.display_w as i32).min(max_body_w).clamp(64, max_body_w.max(64));
        let ih = self.intrinsic_h.max(1) as f32;
        let iw = self.intrinsic_w.max(1) as f32;
        let h = ((w as f32) * (ih / iw)).round().clamp(32.0, 560.0) as i32;
        (w, h)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    BulletList,
    NumberedList,
    Quote,
    Divider,
    Table(TableBlock),
    Image(ImageBlock),
}

impl BlockKind {
    pub fn default_style(&self) -> TextStyle {
        match self {
            Self::Heading(1) => TextStyle {
                bold: true,
                size_pt: 20.0,
                color: 0x0f172a,
                ..TextStyle::default()
            },
            Self::Heading(2) => TextStyle {
                bold: true,
                size_pt: 15.0,
                color: 0x1e3a8a,
                ..TextStyle::default()
            },
            Self::Heading(3) => TextStyle {
                bold: true,
                size_pt: 12.5,
                color: 0x334155,
                ..TextStyle::default()
            },
            Self::Quote => TextStyle {
                italic: true,
                size_pt: 11.0,
                color: 0x475569,
                ..TextStyle::default()
            },
            _ => TextStyle::default(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Paragraph => "正文",
            Self::Heading(1) => "标题 1",
            Self::Heading(2) => "标题 2",
            Self::Heading(_) => "标题 3",
            Self::BulletList => "项目符号列表",
            Self::NumberedList => "编号列表",
            Self::Quote => "引用块",
            Self::Divider => "分割线",
            Self::Table(_) => "表格",
            Self::Image(_) => "图像",
        }
    }

    pub fn is_text_like(&self) -> bool {
        matches!(
            self,
            Self::Paragraph
                | Self::Heading(_)
                | Self::BulletList
                | Self::NumberedList
                | Self::Quote
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    pub runs: Vec<Run>,
    pub align: Align,
    pub line_spacing: f32,
    pub space_after_pt: f32,
}

impl Block {
    pub fn paragraph(runs: Vec<Run>) -> Self {
        Self {
            kind: BlockKind::Paragraph,
            runs,
            align: Align::Left,
            line_spacing: 1.35,
            space_after_pt: 6.0,
        }
    }

    pub fn heading(level: u8, text: &str) -> Self {
        let level = level.clamp(1, 3);
        let kind = BlockKind::Heading(level);
        Self {
            runs: vec![Run::new(text, kind.default_style())],
            kind,
            align: Align::Left,
            line_spacing: 1.25,
            space_after_pt: match level {
                1 => 10.0,
                2 => 8.0,
                _ => 6.0,
            },
        }
    }

    pub fn with_kind(kind: BlockKind, runs: Vec<Run>) -> Self {
        let space_after_pt = match kind {
            BlockKind::Heading(1) => 10.0,
            BlockKind::Heading(2) => 8.0,
            BlockKind::BulletList | BlockKind::NumberedList => 3.5,
            BlockKind::Table(_) | BlockKind::Image(_) => 10.0,
            _ => 6.0,
        };
        Self {
            kind,
            runs,
            align: Align::Left,
            line_spacing: 1.35,
            space_after_pt,
        }
    }

    pub fn is_empty_text(&self) -> bool {
        self.kind.is_text_like() && runs_plain_text(&self.runs).is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocPos {
    Text {
        block: usize,
        offset: usize,
    },
    TableCell {
        block: usize,
        row: usize,
        col: usize,
        offset: usize,
    },
    Object {
        block: usize,
        after: bool,
    },
}

impl DocPos {
    pub fn block_idx(self) -> usize {
        match self {
            Self::Text { block, .. }
            | Self::TableCell { block, .. }
            | Self::Object { block, .. } => block,
        }
    }
}

impl Ord for DocPos {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let rank = |p: &DocPos| -> (usize, usize, usize, usize) {
            match *p {
                DocPos::Text { block, offset } => (block, 0, 0, offset),
                DocPos::TableCell {
                    block,
                    row,
                    col,
                    offset,
                } => (block, row, col, offset),
                DocPos::Object { block, after } => (block, 0, 0, usize::from(after)),
            }
        };
        rank(self).cmp(&rank(other))
    }
}

impl PartialOrd for DocPos {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: DocPos,
    pub focus: DocPos,
}

impl Selection {
    pub fn collapsed(pos: DocPos) -> Self {
        Self {
            anchor: pos,
            focus: pos,
        }
    }

    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.focus
    }

    pub fn ordered(&self) -> (DocPos, DocPos) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub block: usize,
    pub level: u8,
    pub title: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocStats {
    pub words: usize,
    pub chars_with_spaces: usize,
    pub chars_no_spaces: usize,
    pub paragraphs: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: DocPos,
    pub end: DocPos,
}

#[derive(Clone)]
struct Snapshot {
    blocks: Vec<Block>,
    setup: PageSetup,
    selection: Selection,
    active_style: TextStyle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CoalesceKind {
    Insert,
    Delete,
    Other,
}

pub struct UndoStack {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_kind: CoalesceKind,
    last_time: Instant,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoStack {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            last_kind: CoalesceKind::Other,
            last_time: Instant::now() - Duration::from_secs(10),
        }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.last_kind = CoalesceKind::Other;
    }

    fn record_with_snap(&mut self, snap: Snapshot, kind: CoalesceKind) {
        let now = Instant::now();
        let coalesce = kind != CoalesceKind::Other
            && kind == self.last_kind
            && now.saturating_duration_since(self.last_time) < Duration::from_millis(650)
            && !self.undo.is_empty();
        self.last_kind = kind;
        self.last_time = now;
        self.redo.clear();
        if coalesce {
            return;
        }
        self.undo.push(snap);
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }
}

pub struct Document {
    pub path: Option<PathBuf>,
    pub setup: PageSetup,
    pub blocks: Vec<Block>,
    pub selection: Selection,
    pub active_style: TextStyle,
    pub dirty: bool,
    pub undo: UndoStack,
    pub next_image_id: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self::empty()
    }
}

impl Document {
    pub fn empty() -> Self {
        let style = TextStyle::default();
        Self {
            path: None,
            setup: PageSetup::default(),
            blocks: vec![Block::paragraph(vec![Run::new("", style)])],
            selection: Selection::collapsed(DocPos::Text {
                block: 0,
                offset: 0,
            }),
            active_style: style,
            dirty: false,
            undo: UndoStack::new(),
            next_image_id: 1,
        }
    }

    pub fn from_parts(setup: PageSetup, mut blocks: Vec<Block>) -> Self {
        if blocks.is_empty() {
            blocks.push(Block::paragraph(vec![Run::new("", TextStyle::default())]));
        }
        let next_image_id = blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::Image(img) => Some(img.id + 1),
                _ => None,
            })
            .max()
            .unwrap_or(1);
        let first_pos = Self::first_pos_in_block(&blocks, 0);
        let style = blocks
            .first()
            .and_then(|b| b.runs.first().map(|r| r.style))
            .unwrap_or_default();
        Self {
            path: None,
            setup,
            blocks,
            selection: Selection::collapsed(first_pos),
            active_style: style,
            dirty: false,
            undo: UndoStack::new(),
            next_image_id,
        }
    }

    /// Rich built-in Chinese+English technical specification document with headings,
    /// multiple font families, inline styles, lists, styled data table and an embedded PNG chart.
    pub fn demo_document() -> Self {
        let mut doc = Self::empty();
        doc.setup.header = "SpaceOS-9 架构与文档排版系统技术白皮书".to_string();
        doc.setup.footer = "SpaceEditor 原生字处理引擎 · 示例文档".to_string();
        doc.setup.show_page_numbers = true;

        let chart_png = generate_demo_chart_png();
        let body_w = doc.setup.body_width_px() as f32;

        let mut tbl = TableBlock::empty(4, 4, body_w);
        let headers = ["子系统模块", "核心实现语言", "内存占用", "当前状态"];
        let rows = [
            ["SPUI 4.0 合成窗口管理器", "Rust (musl 静态链接)", "18.4 MiB", "已稳定发布"],
            ["Space 商店与 SPK 事务引擎", "Rust + Ed25519 签名", "11.2 MiB", "已部署软件源"],
            ["SpaceEditor 分页字处理", "Rust + OOXML (.docx)", "14.6 MiB", "实时 A4 所见即所得"],
        ];
        for (c, h) in headers.iter().enumerate() {
            tbl.rows[0][c] = TableCell::with_text(
                h,
                TextStyle {
                    bold: true,
                    size_pt: 10.5,
                    color: 0x1e3a8a,
                    ..TextStyle::default()
                },
                Align::Left,
                Some(0xeff6ff),
            );
        }
        for (r, row_vals) in rows.iter().enumerate() {
            for (c, val) in row_vals.iter().enumerate() {
                tbl.rows[r + 1][c] = TableCell::with_text(
                    val,
                    TextStyle {
                        size_pt: 10.0,
                        font: if c == 1 {
                            FontFamily::Mono
                        } else {
                            FontFamily::Sans
                        },
                        color: if c == 3 { 0x15803d } else { 0x1e293b },
                        bold: c == 3,
                        ..TextStyle::default()
                    },
                    Align::Left,
                    if r % 2 == 1 { Some(0xf8fafc) } else { None },
                );
            }
        }
        tbl.col_widths = vec![body_w * 0.34, body_w * 0.28, body_w * 0.16, body_w * 0.22];

        doc.blocks = vec![
            Block::heading(1, "SpaceOS-9 文档排版与办公套件设计规范"),
            Block::paragraph(vec![
                Run::new(
                    "本文档演示 ",
                    TextStyle::default(),
                ),
                Run::new(
                    "SpaceEditor",
                    TextStyle {
                        bold: true,
                        color: 0x1d4ed8,
                        ..TextStyle::default()
                    },
                ),
                Run::new(
                    " 的 A4 所见即所得分页排版能力，包括多级标题、混合字体（黑体 / ",
                    TextStyle::default(),
                ),
                Run::new(
                    "思源宋体",
                    TextStyle {
                        font: FontFamily::Serif,
                        bold: true,
                        ..TextStyle::default()
                    },
                ),
                Run::new(" / ", TextStyle::default()),
                Run::new(
                    "Monospace",
                    TextStyle {
                        font: FontFamily::Mono,
                        highlight: Some(0xfef08a),
                        ..TextStyle::default()
                    },
                ),
                Run::new(
                    "）、富文本装饰、项目列表、原生表格与嵌入式位图图像。",
                    TextStyle::default(),
                ),
            ]),
            Block::heading(2, "1. 核心设计目标与特性"),
            Block::with_kind(
                BlockKind::BulletList,
                vec![
                    Run::new(
                        "纯静态 Rust 架构：",
                        TextStyle {
                            bold: true,
                            ..TextStyle::default()
                        },
                    ),
                    Run::new(
                        "不依赖任何外部 C 库，直接渲染到 SPUI SHM 画布并自适应深浅桌面主题。",
                        TextStyle::default(),
                    ),
                ],
            ),
            Block::with_kind(
                BlockKind::BulletList,
                vec![
                    Run::new(
                        "标准 OOXML 兼容：",
                        TextStyle {
                            bold: true,
                            ..TextStyle::default()
                        },
                    ),
                    Run::new(
                        "支持 ",
                        TextStyle::default(),
                    ),
                    Run::new(
                        ".docx",
                        TextStyle {
                            font: FontFamily::Mono,
                            bold: true,
                            color: 0x0f766e,
                            ..TextStyle::default()
                        },
                    ),
                    Run::new(
                        " 双向读写以及 Markdown / HTML / 纯文本导入导出，包含表格与嵌入图片。",
                        TextStyle::default(),
                    ),
                ],
            ),
            Block::with_kind(
                BlockKind::BulletList,
                vec![
                    Run::new(
                        "所见即所得页视图：",
                        TextStyle {
                            bold: true,
                            ..TextStyle::default()
                        },
                    ),
                    Run::new(
                        "支持 A4 / Letter 纸张、标准/窄/宽页边距、页眉页脚、自动页码与视图缩放。",
                        TextStyle::default(),
                    ),
                ],
            ),
            Block::with_kind(
                BlockKind::Quote,
                vec![Run::new(
                    "“好的文档工具应当让文字本身成为焦点，同时保证版面逻辑、字体度量与跨平台文件格式的高度可预测。”",
                    BlockKind::Quote.default_style(),
                )],
            ),
            Block::heading(2, "2. 系统子模块性能概览"),
            Block::with_kind(BlockKind::Table(tbl), Vec::new()),
            Block::heading(3, "2.1 排版渲染性能柱状图"),
            Block::paragraph(vec![Run::new(
                "下图为 SpaceOS 桌面核心组件在 1280×800 分辨率下的冷启动首帧时间对比（内置 PNG 图表）：",
                TextStyle {
                    font: FontFamily::Serif,
                    size_pt: 11.0,
                    ..TextStyle::default()
                },
            )]),
            Block::with_kind(
                BlockKind::Image(ImageBlock {
                    id: 1,
                    filename: "perf-chart.png".into(),
                    mime: "image/png".into(),
                    bytes: Arc::new(chart_png),
                    intrinsic_w: 440,
                    intrinsic_h: 180,
                    display_w: 440,
                    caption: "图 1 · SpaceOS-9 原生组件帧耗时与冷启动对比 (ms)".into(),
                    align: Align::Center,
                }),
                Vec::new(),
            ),
            Block::with_kind(BlockKind::Divider, Vec::new()),
            Block::heading(2, "3. 快速上手与快捷键"),
            Block::with_kind(
                BlockKind::NumberedList,
                vec![
                    Run::new(
                        "格式控制：",
                        TextStyle {
                            bold: true,
                            ..TextStyle::default()
                        },
                    ),
                    Run::new(
                        "使用 Ctrl+B 加粗、Ctrl+I 斜体、Ctrl+U 下划线，或通过上方功能区切换标题与配色。",
                        TextStyle::default(),
                    ),
                ],
            ),
            Block::with_kind(
                BlockKind::NumberedList,
                vec![
                    Run::new(
                        "查找与替换：",
                        TextStyle {
                            bold: true,
                            ..TextStyle::default()
                        },
                    ),
                    Run::new(
                        "按下 Ctrl+F 或 Ctrl+H 打开文档内查找替换栏，实时高亮并定位全部匹配项。",
                        TextStyle::default(),
                    ),
                ],
            ),
            Block::with_kind(
                BlockKind::NumberedList,
                vec![
                    Run::new(
                        "表格与导出：",
                        TextStyle {
                            bold: true,
                            ..TextStyle::default()
                        },
                    ),
                    Run::new(
                        "在表格内按 Tab 快速切换单元格；通过 Ctrl+S 导出为通用 .docx 文档。",
                        TextStyle::default(),
                    ),
                ],
            ),
        ];
        doc.next_image_id = 2;
        doc.selection = Selection::collapsed(DocPos::Text {
            block: 1,
            offset: 0,
        });
        doc.active_style = TextStyle::default();
        doc.dirty = false;
        doc.undo.clear();
        doc
    }

    fn record_undo(&mut self, kind: CoalesceKind) {
        let snap = self.snapshot();
        self.undo.record_with_snap(snap, kind);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            blocks: self.blocks.clone(),
            setup: self.setup.clone(),
            selection: self.selection,
            active_style: self.active_style,
        }
    }

    fn restore(&mut self, snap: Snapshot) {
        self.blocks = snap.blocks;
        self.setup = snap.setup;
        self.selection = snap.selection;
        self.active_style = snap.active_style;
        self.dirty = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undo.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.undo.pop() else {
            return false;
        };
        let cur = self.snapshot();
        self.undo.redo.push(cur);
        self.undo.last_kind = CoalesceKind::Other;
        self.restore(prev);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.undo.redo.pop() else {
            return false;
        };
        let cur = self.snapshot();
        self.undo.undo.push(cur);
        self.undo.last_kind = CoalesceKind::Other;
        self.restore(next);
        true
    }

    pub fn caret(&self) -> DocPos {
        self.selection.focus
    }

    pub fn first_pos_in_block(blocks: &[Block], block: usize) -> DocPos {
        match blocks.get(block).map(|b| &b.kind) {
            Some(BlockKind::Table(t)) if !t.rows.is_empty() && !t.rows[0].is_empty() => {
                DocPos::TableCell {
                    block,
                    row: 0,
                    col: 0,
                    offset: 0,
                }
            }
            Some(BlockKind::Image(_) | BlockKind::Divider) => DocPos::Object { block, after: true },
            _ => DocPos::Text { block, offset: 0 },
        }
    }

    pub fn last_pos_in_block(blocks: &[Block], block: usize) -> DocPos {
        match blocks.get(block).map(|b| &b.kind) {
            Some(BlockKind::Table(t)) if !t.rows.is_empty() && !t.rows[0].is_empty() => {
                let r = t.rows.len() - 1;
                let c = t.rows[r].len() - 1;
                let off = runs_char_len(&t.rows[r][c].runs);
                DocPos::TableCell {
                    block,
                    row: r,
                    col: c,
                    offset: off,
                }
            }
            Some(BlockKind::Image(_) | BlockKind::Divider) => DocPos::Object { block, after: true },
            Some(_) => DocPos::Text {
                block,
                offset: runs_char_len(&blocks[block].runs),
            },
            None => DocPos::Text {
                block: 0,
                offset: 0,
            },
        }
    }

    pub fn clamp_pos(&self, pos: DocPos) -> DocPos {
        if self.blocks.is_empty() {
            return DocPos::Text {
                block: 0,
                offset: 0,
            };
        }
        let block = pos.block_idx().min(self.blocks.len() - 1);
        match (&self.blocks[block].kind, pos) {
            (BlockKind::Table(tbl), DocPos::TableCell { row, col, offset, .. }) => {
                if tbl.rows.is_empty() || tbl.rows[0].is_empty() {
                    return DocPos::Object { block, after: true };
                }
                let r = row.min(tbl.rows.len() - 1);
                let c = col.min(tbl.rows[r].len() - 1);
                let off = offset.min(runs_char_len(&tbl.rows[r][c].runs));
                DocPos::TableCell {
                    block,
                    row: r,
                    col: c,
                    offset: off,
                }
            }
            (BlockKind::Table(tbl), _) => {
                if !tbl.rows.is_empty() && !tbl.rows[0].is_empty() {
                    DocPos::TableCell {
                        block,
                        row: 0,
                        col: 0,
                        offset: 0,
                    }
                } else {
                    DocPos::Object { block, after: true }
                }
            }
            (BlockKind::Image(_) | BlockKind::Divider, DocPos::Object { after, .. }) => {
                DocPos::Object { block, after }
            }
            (BlockKind::Image(_) | BlockKind::Divider, _) => DocPos::Object { block, after: true },
            (_, DocPos::Text { offset, .. }) => DocPos::Text {
                block,
                offset: offset.min(runs_char_len(&self.blocks[block].runs)),
            },
            _ => DocPos::Text { block, offset: 0 },
        }
    }

    pub fn sync_active_style(&mut self) {
        let pos = self.clamp_pos(self.caret());
        self.active_style = match pos {
            DocPos::Text { block, offset } => {
                let b = &self.blocks[block];
                style_at_offset(&b.runs, offset, b.kind.default_style())
            }
            DocPos::TableCell {
                block,
                row,
                col,
                offset,
            } => {
                if let BlockKind::Table(tbl) = &self.blocks[block].kind {
                    style_at_offset(&tbl.rows[row][col].runs, offset, TextStyle::default())
                } else {
                    TextStyle::default()
                }
            }
            DocPos::Object { .. } => TextStyle::default(),
        };
    }

    pub fn set_caret(&mut self, pos: DocPos, extend_selection: bool) {
        let pos = self.clamp_pos(pos);
        if extend_selection {
            self.selection.focus = pos;
        } else {
            self.selection = Selection::collapsed(pos);
        }
        self.sync_active_style();
    }

    pub fn select_all(&mut self) {
        if self.blocks.is_empty() {
            return;
        }
        let anchor = Self::first_pos_in_block(&self.blocks, 0);
        let focus = Self::last_pos_in_block(&self.blocks, self.blocks.len() - 1);
        self.selection = Selection { anchor, focus };
        self.sync_active_style();
    }

    pub fn selected_text(&self) -> String {
        if self.selection.is_collapsed() {
            return String::new();
        }
        let (start, end) = self.selection.ordered();
        match (start, end) {
            (
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
            ) if b1 == b2 && r1 == r2 && c1 == c2 => {
                if let Some(BlockKind::Table(tbl)) = self.blocks.get(b1).map(|b| &b.kind) {
                    return runs_plain_text(&slice_runs(&tbl.rows[r1][c1].runs, o1, o2));
                }
                String::new()
            }
            _ => {
                let mut parts = Vec::new();
                let sb = start.block_idx();
                let eb = end.block_idx().min(self.blocks.len().saturating_sub(1));
                for b in sb..=eb {
                    let blk = &self.blocks[b];
                    if blk.kind.is_text_like() {
                        let s_off = match start {
                            DocPos::Text { block, offset } if block == b => offset,
                            _ => 0,
                        };
                        let total = runs_char_len(&blk.runs);
                        let e_off = match end {
                            DocPos::Text { block, offset } if block == b => offset,
                            _ => total,
                        };
                        if s_off <= e_off {
                            parts.push(runs_plain_text(&slice_runs(&blk.runs, s_off, e_off)));
                        }
                    } else if let BlockKind::Table(tbl) = &blk.kind {
                        for row in &tbl.rows {
                            let row_txt: Vec<String> =
                                row.iter().map(|c| runs_plain_text(&c.runs)).collect();
                            parts.push(row_txt.join("\t"));
                        }
                    }
                }
                parts.join("\n")
            }
        }
    }

    pub fn delete_selection_no_undo(&mut self) -> bool {
        if self.selection.is_collapsed() {
            return false;
        }
        let (start, end) = self.selection.ordered();
        match (start, end) {
            (
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
            ) if b1 == b2 && r1 == r2 && c1 == c2 => {
                if let Some(BlockKind::Table(tbl)) = self.blocks.get_mut(b1).map(|b| &mut b.kind) {
                    let fb = style_at_offset(&tbl.rows[r1][c1].runs, o1, TextStyle::default());
                    remove_range_in_runs(&mut tbl.rows[r1][c1].runs, o1, o2, fb);
                }
                self.selection = Selection::collapsed(DocPos::TableCell {
                    block: b1,
                    row: r1,
                    col: c1,
                    offset: o1,
                });
                self.dirty = true;
                return true;
            }
            (
                DocPos::Text {
                    block: b1,
                    offset: o1,
                },
                DocPos::Text {
                    block: b2,
                    offset: o2,
                },
            ) if b1 == b2 => {
                let fb = self.blocks[b1].kind.default_style();
                remove_range_in_runs(&mut self.blocks[b1].runs, o1, o2, fb);
                self.selection = Selection::collapsed(DocPos::Text {
                    block: b1,
                    offset: o1,
                });
                self.dirty = true;
                return true;
            }
            _ => {}
        }

        let sb = start.block_idx();
        let eb = end.block_idx().min(self.blocks.len().saturating_sub(1));
        if sb == eb {
            self.selection = Selection::collapsed(start);
            return false;
        }

        let first_is_text = self.blocks[sb].kind.is_text_like();
        let last_is_text = self.blocks[eb].kind.is_text_like();
        match (first_is_text, last_is_text) {
            (true, true) => {
                let o1 = match start {
                    DocPos::Text { offset, .. } => offset,
                    _ => 0,
                };
                let o2 = match end {
                    DocPos::Text { offset, .. } => offset,
                    _ => runs_char_len(&self.blocks[eb].runs),
                };
                let fb = self.blocks[sb].kind.default_style();
                let total_end = runs_char_len(&self.blocks[eb].runs);
                let tail = slice_runs(&self.blocks[eb].runs, o2, total_end);
                let total_start = runs_char_len(&self.blocks[sb].runs);
                remove_range_in_runs(&mut self.blocks[sb].runs, o1, total_start, fb);
                self.blocks[sb].runs.extend(tail);
                normalize_runs(&mut self.blocks[sb].runs, fb);
                self.blocks.drain(sb + 1..=eb);
                self.selection = Selection::collapsed(DocPos::Text {
                    block: sb,
                    offset: o1,
                });
            }
            _ => {
                // Trim start/end blocks or remove middle blocks.
                let remove_start = if first_is_text {
                    let o1 = match start {
                        DocPos::Text { offset, .. } => offset,
                        _ => 0,
                    };
                    let fb = self.blocks[sb].kind.default_style();
                    let len = runs_char_len(&self.blocks[sb].runs);
                    remove_range_in_runs(&mut self.blocks[sb].runs, o1, len, fb);
                    sb + 1
                } else {
                    sb
                };
                if last_is_text {
                    let o2 = match end {
                        DocPos::Text { offset, .. } => offset,
                        _ => 0,
                    };
                    let fb = self.blocks[eb].kind.default_style();
                    remove_range_in_runs(&mut self.blocks[eb].runs, 0, o2, fb);
                }
                let remove_end = if last_is_text { eb } else { eb + 1 };
                if remove_start < remove_end {
                    self.blocks.drain(remove_start..remove_end);
                }
                if self.blocks.is_empty() {
                    self.blocks.push(Block::paragraph(vec![Run::new("", TextStyle::default())]));
                }
                self.selection = Selection::collapsed(self.clamp_pos(start));
            }
        }
        self.dirty = true;
        true
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.record_undo(CoalesceKind::Insert);
        self.delete_selection_no_undo();

        // Handle multi-line pasted text cleanly.
        if text.contains('\n') {
            let clean = text.replace("\r\n", "\n");
            let mut first = true;
            for part in clean.split('\n') {
                if !first {
                    self.insert_newline_no_undo();
                }
                first = false;
                if !part.is_empty() {
                    self.insert_single_line_text(part);
                }
            }
            self.dirty = true;
            return;
        }
        self.insert_single_line_text(text);
        self.dirty = true;
    }

    fn insert_single_line_text(&mut self, text: &str) {
        let style = self.active_style;
        let pos = self.clamp_pos(self.caret());
        match pos {
            DocPos::Text { block, offset } => {
                insert_text_in_runs(&mut self.blocks[block].runs, offset, text, style);
                let new_off = offset + text.chars().count();
                self.selection = Selection::collapsed(DocPos::Text {
                    block,
                    offset: new_off,
                });
            }
            DocPos::TableCell {
                block,
                row,
                col,
                offset,
            } => {
                if let BlockKind::Table(tbl) = &mut self.blocks[block].kind {
                    insert_text_in_runs(&mut tbl.rows[row][col].runs, offset, text, style);
                    let new_off = offset + text.chars().count();
                    self.selection = Selection::collapsed(DocPos::TableCell {
                        block,
                        row,
                        col,
                        offset: new_off,
                    });
                }
            }
            DocPos::Object { block, after } => {
                let insert_at = if after { block + 1 } else { block };
                let p = Block::paragraph(vec![Run::new(text, style)]);
                self.blocks.insert(insert_at, p);
                self.selection = Selection::collapsed(DocPos::Text {
                    block: insert_at,
                    offset: text.chars().count(),
                });
            }
        }
    }

    pub fn insert_newline(&mut self) {
        self.record_undo(CoalesceKind::Other);
        self.delete_selection_no_undo();
        self.insert_newline_no_undo();
        self.dirty = true;
    }

    fn insert_newline_no_undo(&mut self) {
        let pos = self.clamp_pos(self.caret());
        match pos {
            DocPos::TableCell {
                block,
                row,
                col,
                offset,
            } => {
                if let BlockKind::Table(tbl) = &mut self.blocks[block].kind {
                    if row + 1 < tbl.rows.len() {
                        self.selection = Selection::collapsed(DocPos::TableCell {
                            block,
                            row: row + 1,
                            col,
                            offset: runs_char_len(&tbl.rows[row + 1][col].runs),
                        });
                    } else {
                        insert_text_in_runs(&mut tbl.rows[row][col].runs, offset, " ", self.active_style);
                        self.selection = Selection::collapsed(DocPos::TableCell {
                            block,
                            row,
                            col,
                            offset: offset + 1,
                        });
                    }
                }
            }
            DocPos::Object { block, after } => {
                let idx = if after { block + 1 } else { block };
                self.blocks.insert(idx, Block::paragraph(vec![Run::new("", TextStyle::default())]));
                self.selection = Selection::collapsed(DocPos::Text {
                    block: idx,
                    offset: 0,
                });
                self.sync_active_style();
            }
            DocPos::Text { block, offset } => {
                // Pressing Enter on an empty Bullet/Numbered/Quote block converts it to Paragraph.
                if self.blocks[block].is_empty_text()
                    && matches!(
                        self.blocks[block].kind,
                        BlockKind::BulletList | BlockKind::NumberedList | BlockKind::Quote
                    )
                {
                    self.blocks[block].kind = BlockKind::Paragraph;
                    let def = TextStyle::default();
                    self.blocks[block].runs = vec![Run::new("", def)];
                    self.active_style = def;
                    self.selection = Selection::collapsed(DocPos::Text { block, offset: 0 });
                    return;
                }
                let fb = self.blocks[block].kind.default_style();
                let total = runs_char_len(&self.blocks[block].runs);
                let tail = slice_runs(&self.blocks[block].runs, offset, total);
                remove_range_in_runs(&mut self.blocks[block].runs, offset, total, fb);

                let next_kind = match self.blocks[block].kind {
                    BlockKind::Heading(_) | BlockKind::Quote => BlockKind::Paragraph,
                    BlockKind::BulletList => BlockKind::BulletList,
                    BlockKind::NumberedList => BlockKind::NumberedList,
                    _ => BlockKind::Paragraph,
                };
                let next_style = if matches!(self.blocks[block].kind, BlockKind::Heading(_)) {
                    TextStyle::default()
                } else {
                    self.active_style
                };
                let mut next_runs = if tail.is_empty() {
                    vec![Run::new("", next_style)]
                } else {
                    tail
                };
                normalize_runs(&mut next_runs, next_style);
                let mut new_blk = Block::with_kind(next_kind, next_runs);
                new_blk.align = self.blocks[block].align;
                new_blk.line_spacing = self.blocks[block].line_spacing;
                self.blocks.insert(block + 1, new_blk);
                self.selection = Selection::collapsed(DocPos::Text {
                    block: block + 1,
                    offset: 0,
                });
                self.active_style = next_style;
            }
        }
    }

    pub fn backspace(&mut self) {
        self.record_undo(CoalesceKind::Delete);
        if self.delete_selection_no_undo() {
            self.sync_active_style();
            return;
        }
        let pos = self.clamp_pos(self.caret());
        match pos {
            DocPos::Text { block, offset } => {
                if offset > 0 {
                    let fb = self.blocks[block].kind.default_style();
                    remove_range_in_runs(&mut self.blocks[block].runs, offset - 1, offset, fb);
                    self.selection = Selection::collapsed(DocPos::Text {
                        block,
                        offset: offset - 1,
                    });
                    self.dirty = true;
                } else if matches!(
                    self.blocks[block].kind,
                    BlockKind::BulletList | BlockKind::NumberedList | BlockKind::Quote
                ) {
                    self.blocks[block].kind = BlockKind::Paragraph;
                    self.dirty = true;
                } else if block > 0 {
                    if self.blocks[block - 1].kind.is_text_like() {
                        let prev_len = runs_char_len(&self.blocks[block - 1].runs);
                        let cur = self.blocks.remove(block);
                        let fb = self.blocks[block - 1].kind.default_style();
                        self.blocks[block - 1].runs.extend(cur.runs);
                        normalize_runs(&mut self.blocks[block - 1].runs, fb);
                        self.selection = Selection::collapsed(DocPos::Text {
                            block: block - 1,
                            offset: prev_len,
                        });
                        self.dirty = true;
                    } else if matches!(
                        self.blocks[block - 1].kind,
                        BlockKind::Divider | BlockKind::Image(_)
                    ) {
                        self.blocks.remove(block - 1);
                        self.selection = Selection::collapsed(DocPos::Text {
                            block: block - 1,
                            offset: 0,
                        });
                        self.dirty = true;
                    }
                }
            }
            DocPos::TableCell {
                block,
                row,
                col,
                offset,
            } => {
                if offset > 0 {
                    if let BlockKind::Table(tbl) = &mut self.blocks[block].kind {
                        let fb = style_at_offset(&tbl.rows[row][col].runs, offset, TextStyle::default());
                        remove_range_in_runs(&mut tbl.rows[row][col].runs, offset - 1, offset, fb);
                        self.selection = Selection::collapsed(DocPos::TableCell {
                            block,
                            row,
                            col,
                            offset: offset - 1,
                        });
                        self.dirty = true;
                    }
                }
            }
            DocPos::Object { block, .. } => {
                if self.blocks.len() > 1 {
                    self.blocks.remove(block);
                    let target = block.saturating_sub(1);
                    self.selection = Selection::collapsed(Self::last_pos_in_block(&self.blocks, target));
                } else {
                    self.blocks[0] = Block::paragraph(vec![Run::new("", TextStyle::default())]);
                    self.selection = Selection::collapsed(DocPos::Text {
                        block: 0,
                        offset: 0,
                    });
                }
                self.dirty = true;
            }
        }
        self.sync_active_style();
    }

    pub fn delete_forward(&mut self) {
        self.record_undo(CoalesceKind::Delete);
        if self.delete_selection_no_undo() {
            self.sync_active_style();
            return;
        }
        let pos = self.clamp_pos(self.caret());
        match pos {
            DocPos::Text { block, offset } => {
                let len = runs_char_len(&self.blocks[block].runs);
                if offset < len {
                    let fb = self.blocks[block].kind.default_style();
                    remove_range_in_runs(&mut self.blocks[block].runs, offset, offset + 1, fb);
                    self.dirty = true;
                } else if block + 1 < self.blocks.len() && self.blocks[block + 1].kind.is_text_like() {
                    let next = self.blocks.remove(block + 1);
                    let fb = self.blocks[block].kind.default_style();
                    self.blocks[block].runs.extend(next.runs);
                    normalize_runs(&mut self.blocks[block].runs, fb);
                    self.dirty = true;
                }
            }
            DocPos::TableCell {
                block,
                row,
                col,
                offset,
            } => {
                if let BlockKind::Table(tbl) = &mut self.blocks[block].kind {
                    let len = runs_char_len(&tbl.rows[row][col].runs);
                    if offset < len {
                        let fb = style_at_offset(&tbl.rows[row][col].runs, offset, TextStyle::default());
                        remove_range_in_runs(&mut tbl.rows[row][col].runs, offset, offset + 1, fb);
                        self.dirty = true;
                    }
                }
            }
            DocPos::Object { block, .. } => {
                self.backspace();
                let _ = block;
            }
        }
        self.sync_active_style();
    }

    pub fn apply_style_change(&mut self, mut f: impl FnMut(&mut TextStyle)) {
        self.record_undo(CoalesceKind::Other);
        f(&mut self.active_style);
        if self.selection.is_collapsed() {
            if let DocPos::Text { block, .. } = self.caret() {
                if self.blocks[block].is_empty_text() {
                    self.blocks[block].runs = vec![Run::new("", self.active_style)];
                }
            }
            return;
        }
        let (start, end) = self.selection.ordered();
        match (start, end) {
            (
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
            ) if b1 == b2 => {
                if let BlockKind::Table(tbl) = &mut self.blocks[b1].kind {
                    if r1 == r2 && c1 == c2 {
                        let fb = style_at_offset(&tbl.rows[r1][c1].runs, o1, TextStyle::default());
                        apply_style_in_runs(&mut tbl.rows[r1][c1].runs, o1, o2, fb, &mut f);
                    } else {
                        for r in r1.min(r2)..=r1.max(r2).min(tbl.rows.len() - 1) {
                            for c in c1.min(c2)..=c1.max(c2).min(tbl.rows[r].len() - 1) {
                                let len = runs_char_len(&tbl.rows[r][c].runs);
                                apply_style_in_runs(
                                    &mut tbl.rows[r][c].runs,
                                    0,
                                    len,
                                    TextStyle::default(),
                                    &mut f,
                                );
                            }
                        }
                    }
                    self.dirty = true;
                }
            }
            _ => {
                let sb = start.block_idx();
                let eb = end.block_idx().min(self.blocks.len() - 1);
                for b in sb..=eb {
                    if !self.blocks[b].kind.is_text_like() {
                        continue;
                    }
                    let total = runs_char_len(&self.blocks[b].runs);
                    let s_off = match start {
                        DocPos::Text { block, offset } if block == b => offset,
                        _ => 0,
                    };
                    let e_off = match end {
                        DocPos::Text { block, offset } if block == b => offset,
                        _ => total,
                    };
                    let fb = self.blocks[b].kind.default_style();
                    apply_style_in_runs(&mut self.blocks[b].runs, s_off, e_off, fb, &mut f);
                    self.dirty = true;
                }
            }
        }
    }

    pub fn set_block_kind(&mut self, kind: BlockKind) {
        self.record_undo(CoalesceKind::Other);
        let (start, end) = self.selection.ordered();
        let sb = start.block_idx();
        let eb = end.block_idx().min(self.blocks.len() - 1);
        for b in sb..=eb {
            if !self.blocks[b].kind.is_text_like() {
                continue;
            }
            let same_list = matches!(
                (&self.blocks[b].kind, &kind),
                (BlockKind::BulletList, BlockKind::BulletList)
                    | (BlockKind::NumberedList, BlockKind::NumberedList)
                    | (BlockKind::Quote, BlockKind::Quote)
            );
            let target_kind = if same_list {
                BlockKind::Paragraph
            } else {
                kind.clone()
            };
            let plain = runs_plain_text(&self.blocks[b].runs);
            let is_heading_target = matches!(target_kind, BlockKind::Heading(_))
                || matches!(self.blocks[b].kind, BlockKind::Heading(_));
            if is_heading_target {
                let st = target_kind.default_style();
                self.blocks[b].runs = vec![Run::new(plain, st)];
                self.active_style = st;
            }
            self.blocks[b].kind = target_kind;
            self.dirty = true;
        }
        self.sync_active_style();
    }

    pub fn set_align(&mut self, align: Align) {
        self.record_undo(CoalesceKind::Other);
        let (start, end) = self.selection.ordered();
        let sb = start.block_idx();
        let eb = end.block_idx().min(self.blocks.len() - 1);
        for b in sb..=eb {
            if let DocPos::TableCell { row, col, .. } = start {
                if let BlockKind::Table(tbl) = &mut self.blocks[b].kind {
                    if let Some(cell) = tbl.rows.get_mut(row).and_then(|r| r.get_mut(col)) {
                        cell.align = align;
                        self.dirty = true;
                        continue;
                    }
                }
            }
            if let BlockKind::Image(img) = &mut self.blocks[b].kind {
                img.align = align;
            } else {
                self.blocks[b].align = align;
            }
            self.dirty = true;
        }
    }

    pub fn set_line_spacing(&mut self, spacing: f32) {
        self.record_undo(CoalesceKind::Other);
        let (start, end) = self.selection.ordered();
        let sb = start.block_idx();
        let eb = end.block_idx().min(self.blocks.len() - 1);
        for b in sb..=eb {
            self.blocks[b].line_spacing = spacing.clamp(1.0, 2.5);
        }
        self.dirty = true;
    }

    pub fn current_block_kind(&self) -> BlockKind {
        let idx = self.caret().block_idx().min(self.blocks.len().saturating_sub(1));
        self.blocks
            .get(idx)
            .map(|b| b.kind.clone())
            .unwrap_or(BlockKind::Paragraph)
    }

    pub fn current_align(&self) -> Align {
        let pos = self.clamp_pos(self.caret());
        match pos {
            DocPos::TableCell {
                block, row, col, ..
            } => {
                if let BlockKind::Table(tbl) = &self.blocks[block].kind {
                    return tbl.rows[row][col].align;
                }
                Align::Left
            }
            _ => match self.blocks.get(pos.block_idx()) {
                Some(b) => match &b.kind {
                    BlockKind::Image(img) => img.align,
                    _ => b.align,
                },
                None => Align::Left,
            },
        }
    }

    pub fn insert_table(&mut self, rows: usize, cols: usize) {
        self.record_undo(CoalesceKind::Other);
        self.delete_selection_no_undo();
        let body_w = self.setup.body_width_px() as f32;
        let tbl = TableBlock::empty(rows, cols, body_w);
        let after_idx = (self.caret().block_idx() + 1).min(self.blocks.len());
        self.blocks
            .insert(after_idx, Block::with_kind(BlockKind::Table(tbl), Vec::new()));
        // Keep a trailing paragraph after table so cursor can always exit cleanly.
        if after_idx + 1 >= self.blocks.len() {
            self.blocks
                .push(Block::paragraph(vec![Run::new("", TextStyle::default())]));
        }
        self.selection = Selection::collapsed(DocPos::TableCell {
            block: after_idx,
            row: 0,
            col: 0,
            offset: 0,
        });
        self.dirty = true;
    }

    pub fn table_add_row(&mut self) -> bool {
        let DocPos::TableCell { block, row, col, .. } = self.caret() else {
            return false;
        };
        self.record_undo(CoalesceKind::Other);
        if let Some(BlockKind::Table(tbl)) = self.blocks.get_mut(block).map(|b| &mut b.kind) {
            let cols = tbl.col_count().max(1);
            let new_row = (0..cols)
                .map(|_| TableCell::with_text("", TextStyle::default(), Align::Left, None))
                .collect();
            let ins = (row + 1).min(tbl.rows.len());
            tbl.rows.insert(ins, new_row);
            self.selection = Selection::collapsed(DocPos::TableCell {
                block,
                row: ins,
                col: col.min(cols - 1),
                offset: 0,
            });
            self.dirty = true;
            return true;
        }
        false
    }

    pub fn table_del_row(&mut self) -> bool {
        let DocPos::TableCell { block, row, col, .. } = self.caret() else {
            return false;
        };
        self.record_undo(CoalesceKind::Other);
        if let Some(BlockKind::Table(tbl)) = self.blocks.get_mut(block).map(|b| &mut b.kind) {
            if tbl.rows.len() <= 1 {
                return false;
            }
            tbl.rows.remove(row.min(tbl.rows.len() - 1));
            let nr = row.min(tbl.rows.len() - 1);
            let nc = col.min(tbl.rows[nr].len().saturating_sub(1));
            self.selection = Selection::collapsed(DocPos::TableCell {
                block,
                row: nr,
                col: nc,
                offset: 0,
            });
            self.dirty = true;
            return true;
        }
        false
    }

    pub fn table_add_col(&mut self) -> bool {
        let DocPos::TableCell { block, row, col, .. } = self.caret() else {
            return false;
        };
        self.record_undo(CoalesceKind::Other);
        let max_w = self.setup.body_width_px() as f32;
        if let Some(BlockKind::Table(tbl)) = self.blocks.get_mut(block).map(|b| &mut b.kind) {
            if tbl.col_count() >= 12 {
                return false;
            }
            let ins_col = (col + 1).min(tbl.col_count());
            for (r_idx, r) in tbl.rows.iter_mut().enumerate() {
                let mut st = TextStyle {
                    size_pt: 10.5,
                    ..TextStyle::default()
                };
                let shading = if r_idx == 0 {
                    st.bold = true;
                    Some(0xeff6ff)
                } else {
                    None
                };
                r.insert(ins_col, TableCell::with_text("", st, Align::Left, shading));
            }
            let cols = tbl.col_count().max(1);
            tbl.col_widths = vec![(max_w / cols as f32).max(40.0); cols];
            self.selection = Selection::collapsed(DocPos::TableCell {
                block,
                row,
                col: ins_col,
                offset: 0,
            });
            self.dirty = true;
            return true;
        }
        false
    }

    pub fn table_step_cell(&mut self, forward: bool) -> bool {
        let DocPos::TableCell {
            block,
            mut row,
            mut col,
            ..
        } = self.caret()
        else {
            return false;
        };
        let Some(BlockKind::Table(tbl)) = self.blocks.get(block).map(|b| &b.kind) else {
            return false;
        };
        let rows = tbl.rows.len();
        let cols = tbl.col_count();
        if rows == 0 || cols == 0 {
            return false;
        }
        if forward {
            if col + 1 < cols {
                col += 1;
            } else if row + 1 < rows {
                row += 1;
                col = 0;
            } else {
                // Exit table to next block (creating one if needed) or append a row? Keep clean: move to next block.
                if block + 1 < self.blocks.len() {
                    self.set_caret(Self::first_pos_in_block(&self.blocks, block + 1), false);
                    return true;
                }
                return self.table_add_row();
            }
        } else if col > 0 {
            col -= 1;
        } else if row > 0 {
            row -= 1;
            col = cols - 1;
        } else if block > 0 {
            self.set_caret(Self::last_pos_in_block(&self.blocks, block - 1), false);
            return true;
        }
        let off = runs_char_len(&tbl.rows[row][col].runs);
        self.set_caret(
            DocPos::TableCell {
                block,
                row,
                col,
                offset: off,
            },
            false,
        );
        true
    }

    pub fn insert_image_bytes(&mut self, filename: &str, bytes: Vec<u8>) -> Result<(), String> {
        let sprite = icons::decode_image_bytes(&bytes)
            .ok_or_else(|| "无法解码图像：仅支持有效的 PNG 或 JPEG 文件".to_string())?;
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else {
            "image/jpeg"
        };
        self.record_undo(CoalesceKind::Other);
        self.delete_selection_no_undo();
        let body_w = self.setup.body_width_px().max(120) as u32;
        let display_w = sprite.width.clamp(64, body_w);
        let id = self.next_image_id;
        self.next_image_id += 1;
        let img = ImageBlock {
            id,
            filename: filename.to_string(),
            mime: mime.to_string(),
            bytes: Arc::new(bytes),
            intrinsic_w: sprite.width.max(1),
            intrinsic_h: sprite.height.max(1),
            display_w,
            caption: String::new(),
            align: Align::Center,
        };
        let insert_at = (self.caret().block_idx() + 1).min(self.blocks.len());
        self.blocks
            .insert(insert_at, Block::with_kind(BlockKind::Image(img), Vec::new()));
        if insert_at + 1 >= self.blocks.len() {
            self.blocks
                .push(Block::paragraph(vec![Run::new("", TextStyle::default())]));
        }
        self.selection = Selection::collapsed(DocPos::Object {
            block: insert_at,
            after: true,
        });
        self.dirty = true;
        Ok(())
    }

    pub fn insert_divider(&mut self) {
        self.record_undo(CoalesceKind::Other);
        self.delete_selection_no_undo();
        let insert_at = (self.caret().block_idx() + 1).min(self.blocks.len());
        self.blocks
            .insert(insert_at, Block::with_kind(BlockKind::Divider, Vec::new()));
        self.blocks.insert(
            insert_at + 1,
            Block::paragraph(vec![Run::new("", TextStyle::default())]),
        );
        self.selection = Selection::collapsed(DocPos::Text {
            block: insert_at + 1,
            offset: 0,
        });
        self.dirty = true;
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend && !self.selection.is_collapsed() {
            let (s, _) = self.selection.ordered();
            self.set_caret(s, false);
            return;
        }
        let pos = self.clamp_pos(self.caret());
        let next = match pos {
            DocPos::Text { block, offset } => {
                if offset > 0 {
                    DocPos::Text {
                        block,
                        offset: offset - 1,
                    }
                } else if block > 0 {
                    Self::last_pos_in_block(&self.blocks, block - 1)
                } else {
                    pos
                }
            }
            DocPos::TableCell {
                block,
                row,
                col,
                offset,
            } => {
                if offset > 0 {
                    DocPos::TableCell {
                        block,
                        row,
                        col,
                        offset: offset - 1,
                    }
                } else if col > 0 {
                    let prev_col = col - 1;
                    let off = if let BlockKind::Table(t) = &self.blocks[block].kind {
                        runs_char_len(&t.rows[row][prev_col].runs)
                    } else {
                        0
                    };
                    DocPos::TableCell {
                        block,
                        row,
                        col: prev_col,
                        offset: off,
                    }
                } else if row > 0 {
                    let prev_row = row - 1;
                    let last_col = if let BlockKind::Table(t) = &self.blocks[block].kind {
                        t.rows[prev_row].len().saturating_sub(1)
                    } else {
                        0
                    };
                    let off = if let BlockKind::Table(t) = &self.blocks[block].kind {
                        runs_char_len(&t.rows[prev_row][last_col].runs)
                    } else {
                        0
                    };
                    DocPos::TableCell {
                        block,
                        row: prev_row,
                        col: last_col,
                        offset: off,
                    }
                } else if block > 0 {
                    Self::last_pos_in_block(&self.blocks, block - 1)
                } else {
                    pos
                }
            }
            DocPos::Object { block, after } => {
                if after {
                    DocPos::Object {
                        block,
                        after: false,
                    }
                } else if block > 0 {
                    Self::last_pos_in_block(&self.blocks, block - 1)
                } else {
                    pos
                }
            }
        };
        self.set_caret(next, extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend && !self.selection.is_collapsed() {
            let (_, e) = self.selection.ordered();
            self.set_caret(e, false);
            return;
        }
        let pos = self.clamp_pos(self.caret());
        let next = match pos {
            DocPos::Text { block, offset } => {
                let len = runs_char_len(&self.blocks[block].runs);
                if offset < len {
                    DocPos::Text {
                        block,
                        offset: offset + 1,
                    }
                } else if block + 1 < self.blocks.len() {
                    Self::first_pos_in_block(&self.blocks, block + 1)
                } else {
                    pos
                }
            }
            DocPos::TableCell {
                block,
                row,
                col,
                offset,
            } => {
                if let BlockKind::Table(t) = &self.blocks[block].kind {
                    let len = runs_char_len(&t.rows[row][col].runs);
                    if offset < len {
                        DocPos::TableCell {
                            block,
                            row,
                            col,
                            offset: offset + 1,
                        }
                    } else if col + 1 < t.rows[row].len() {
                        DocPos::TableCell {
                            block,
                            row,
                            col: col + 1,
                            offset: 0,
                        }
                    } else if row + 1 < t.rows.len() {
                        DocPos::TableCell {
                            block,
                            row: row + 1,
                            col: 0,
                            offset: 0,
                        }
                    } else if block + 1 < self.blocks.len() {
                        Self::first_pos_in_block(&self.blocks, block + 1)
                    } else {
                        pos
                    }
                } else {
                    pos
                }
            }
            DocPos::Object { block, after } => {
                if !after {
                    DocPos::Object { block, after: true }
                } else if block + 1 < self.blocks.len() {
                    Self::first_pos_in_block(&self.blocks, block + 1)
                } else {
                    pos
                }
            }
        };
        self.set_caret(next, extend);
    }

    pub fn find_matches(&self, query: &str, case_sensitive: bool) -> Vec<SearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let needle_chars: Vec<char> = if case_sensitive {
            query.chars().collect()
        } else {
            query.to_lowercase().chars().collect()
        };
        let nlen = needle_chars.len();
        if nlen == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let search_buf = |hay: &str, out: &mut Vec<(usize, usize)>| {
            let chars: Vec<char> = if case_sensitive {
                hay.chars().collect()
            } else {
                hay.to_lowercase().chars().collect()
            };
            if chars.len() < nlen {
                return;
            }
            let mut i = 0;
            while i + nlen <= chars.len() {
                if chars[i..i + nlen] == needle_chars[..] {
                    out.push((i, i + nlen));
                    i += nlen;
                } else {
                    i += 1;
                }
            }
        };
        let mut spans = Vec::new();
        for (b, blk) in self.blocks.iter().enumerate() {
            if blk.kind.is_text_like() {
                spans.clear();
                search_buf(&runs_plain_text(&blk.runs), &mut spans);
                for &(s, e) in &spans {
                    out.push(SearchMatch {
                        start: DocPos::Text {
                            block: b,
                            offset: s,
                        },
                        end: DocPos::Text {
                            block: b,
                            offset: e,
                        },
                    });
                }
            } else if let BlockKind::Table(tbl) = &blk.kind {
                for (r, row) in tbl.rows.iter().enumerate() {
                    for (c, cell) in row.iter().enumerate() {
                        spans.clear();
                        search_buf(&runs_plain_text(&cell.runs), &mut spans);
                        for &(s, e) in &spans {
                            out.push(SearchMatch {
                                start: DocPos::TableCell {
                                    block: b,
                                    row: r,
                                    col: c,
                                    offset: s,
                                },
                                end: DocPos::TableCell {
                                    block: b,
                                    row: r,
                                    col: c,
                                    offset: e,
                                },
                            });
                        }
                    }
                }
            }
        }
        out
    }

    pub fn replace_match(&mut self, m: &SearchMatch, replacement: &str) -> bool {
        self.record_undo(CoalesceKind::Other);
        match (m.start, m.end) {
            (
                DocPos::Text {
                    block: b,
                    offset: s,
                },
                DocPos::Text { offset: e, .. },
            ) => {
                if let Some(blk) = self.blocks.get_mut(b) {
                    let fb = blk.kind.default_style();
                    let st = style_at_offset(&blk.runs, s, fb);
                    remove_range_in_runs(&mut blk.runs, s, e, fb);
                    insert_text_in_runs(&mut blk.runs, s, replacement, st);
                    let end_off = s + replacement.chars().count();
                    self.selection = Selection {
                        anchor: DocPos::Text {
                            block: b,
                            offset: s,
                        },
                        focus: DocPos::Text {
                            block: b,
                            offset: end_off,
                        },
                    };
                    self.dirty = true;
                    return true;
                }
            }
            (
                DocPos::TableCell {
                    block: b,
                    row: r,
                    col: c,
                    offset: s,
                },
                DocPos::TableCell { offset: e, .. },
            ) => {
                if let Some(BlockKind::Table(tbl)) = self.blocks.get_mut(b).map(|x| &mut x.kind) {
                    let cell = &mut tbl.rows[r][c];
                    let st = style_at_offset(&cell.runs, s, TextStyle::default());
                    remove_range_in_runs(&mut cell.runs, s, e, st);
                    insert_text_in_runs(&mut cell.runs, s, replacement, st);
                    let end_off = s + replacement.chars().count();
                    self.selection = Selection {
                        anchor: DocPos::TableCell {
                            block: b,
                            row: r,
                            col: c,
                            offset: s,
                        },
                        focus: DocPos::TableCell {
                            block: b,
                            row: r,
                            col: c,
                            offset: end_off,
                        },
                    };
                    self.dirty = true;
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    pub fn replace_all(&mut self, query: &str, replacement: &str, case_sensitive: bool) -> usize {
        let matches = self.find_matches(query, case_sensitive);
        if matches.is_empty() {
            return 0;
        }
        self.record_undo(CoalesceKind::Other);
        let count = matches.len();
        // Replace backwards so earlier offsets remain valid.
        for m in matches.into_iter().rev() {
            match (m.start, m.end) {
                (
                    DocPos::Text {
                        block: b,
                        offset: s,
                    },
                    DocPos::Text { offset: e, .. },
                ) => {
                    let fb = self.blocks[b].kind.default_style();
                    let st = style_at_offset(&self.blocks[b].runs, s, fb);
                    remove_range_in_runs(&mut self.blocks[b].runs, s, e, fb);
                    insert_text_in_runs(&mut self.blocks[b].runs, s, replacement, st);
                }
                (
                    DocPos::TableCell {
                        block: b,
                        row: r,
                        col: c,
                        offset: s,
                    },
                    DocPos::TableCell { offset: e, .. },
                ) => {
                    if let BlockKind::Table(tbl) = &mut self.blocks[b].kind {
                        let cell = &mut tbl.rows[r][c];
                        let st = style_at_offset(&cell.runs, s, TextStyle::default());
                        remove_range_in_runs(&mut cell.runs, s, e, st);
                        insert_text_in_runs(&mut cell.runs, s, replacement, st);
                    }
                }
                _ => {}
            }
        }
        self.dirty = true;
        count
    }

    pub fn outline_headings(&self) -> Vec<OutlineItem> {
        let mut out = Vec::new();
        for (idx, blk) in self.blocks.iter().enumerate() {
            if let BlockKind::Heading(level) = blk.kind {
                let raw = runs_plain_text(&blk.runs);
                let title = if raw.trim().is_empty() {
                    format!("（空标题 {level}）")
                } else {
                    raw.trim().to_string()
                };
                out.push(OutlineItem {
                    block: idx,
                    level,
                    title,
                });
            }
        }
        out
    }

    pub fn stats(&self) -> DocStats {
        let mut s = DocStats::default();
        let mut count_str = |text: &str| {
            let mut in_ascii_word = false;
            for ch in text.chars() {
                s.chars_with_spaces += 1;
                if !ch.is_whitespace() {
                    s.chars_no_spaces += 1;
                }
                if is_cjk(ch) {
                    if in_ascii_word {
                        s.words += 1;
                        in_ascii_word = false;
                    }
                    s.words += 1;
                } else if ch.is_alphanumeric() || ch == '_' {
                    in_ascii_word = true;
                } else if in_ascii_word {
                    s.words += 1;
                    in_ascii_word = false;
                }
            }
            if in_ascii_word {
                s.words += 1;
            }
        };
        for blk in &self.blocks {
            if blk.kind.is_text_like() {
                s.paragraphs += 1;
                count_str(&runs_plain_text(&blk.runs));
            } else if let BlockKind::Table(tbl) = &blk.kind {
                for row in &tbl.rows {
                    for cell in row {
                        count_str(&runs_plain_text(&cell.runs));
                    }
                }
            }
        }
        s
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x3040..=0x30FF | 0xAC00..=0xD7AF
    )
}

/// Generate a crisp 440x180 pure-Rust PNG bar chart used inside the built-in demo document.
pub fn generate_demo_chart_png() -> Vec<u8> {
    let w = 440usize;
    let h = 180usize;
    let mut rgb = vec![248u8; w * h * 3];
    // Soft white background card (#f8fafc -> rgb 248,250,252)
    for px in rgb.chunks_exact_mut(3) {
        px[0] = 248;
        px[1] = 250;
        px[2] = 252;
    }
    let set_px = |buf: &mut [u8], x: usize, y: usize, r: u8, g: u8, b: u8| {
        if x < w && y < h {
            let off = (y * w + x) * 3;
            buf[off] = r;
            buf[off + 1] = g;
            buf[off + 2] = b;
        }
    };
    let fill_rect = |buf: &mut [u8], x0: usize, y0: usize, rw: usize, rh: usize, c: (u8, u8, u8)| {
        for yy in y0..(y0 + rh).min(h) {
            for xx in x0..(x0 + rw).min(w) {
                set_px(buf, xx, yy, c.0, c.1, c.2);
            }
        }
    };
    // Card border & horizontal grid lines
    fill_rect(&mut rgb, 0, 0, w, 1, (203, 213, 225));
    fill_rect(&mut rgb, 0, h - 1, w, 1, (203, 213, 225));
    fill_rect(&mut rgb, 0, 0, 1, h, (203, 213, 225));
    fill_rect(&mut rgb, w - 1, 0, 1, h, (203, 213, 225));
    for gy in [36usize, 68, 100, 132, 156] {
        fill_rect(&mut rgb, 24, gy, w - 48, 1, (226, 232, 240));
    }
    // Draw 5 colorful bars representing component timings.
    let bars = [
        (46usize, 44usize, 108usize, (37, 99, 235)),
        (122, 44, 92, (14, 165, 233)),
        (198, 44, 78, (16, 185, 129)),
        (274, 44, 64, (99, 102, 241)),
        (350, 44, 52, (245, 158, 11)),
    ];
    for (bx, bw, bh, c) in bars {
        let top = 156usize.saturating_sub(bh);
        fill_rect(&mut rgb, bx, top, bw, bh, c);
        // Subtle top highlight stripe
        fill_rect(
            &mut rgb,
            bx,
            top,
            bw,
            4.min(bh),
            (
                c.0.saturating_add(40),
                c.1.saturating_add(40),
                c.2.saturating_add(40),
            ),
        );
    }
    encode_rgb_png(w as u32, h as u32, &rgb)
}

fn encode_rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(height as usize * (1 + width as usize * 3));
    let row_bytes = width as usize * 3;
    for y in 0..height as usize {
        raw.push(0); // filter type 0 (None)
        raw.extend_from_slice(&rgb[y * row_bytes..(y + 1) * row_bytes]);
    }
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = enc.write_all(&raw);
    let idat = enc.finish().unwrap_or_default();

    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // color type 2 = RGB
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    write_png_chunk(&mut out, b"IHDR", &ihdr);
    write_png_chunk(&mut out, b"IDAT", &idat);
    write_png_chunk(&mut out, b"IEND", &[]);
    out
}

fn write_png_chunk(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let crc_start = out.len();
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    let crc = crc32(&out[crc_start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_document_contains_chart_table_and_headings() {
        let doc = Document::demo_document();
        assert!(!doc.outline_headings().is_empty());
        assert!(doc.blocks.iter().any(|b| matches!(b.kind, BlockKind::Table(_))));
        let img = doc
            .blocks
            .iter()
            .find_map(|b| match &b.kind {
                BlockKind::Image(img) => Some(img),
                _ => None,
            })
            .expect("demo must embed PNG chart");
        let sprite = icons::decode_image_bytes(&img.bytes).expect("demo PNG decodes cleanly");
        assert_eq!((sprite.width, sprite.height), (440, 180));
        assert!(doc.stats().words > 50);
    }

    #[test]
    fn editing_undo_redo_and_formatting_roundtrip() {
        let mut doc = Document::empty();
        doc.insert_text("Hello SpaceOS");
        assert_eq!(runs_plain_text(&doc.blocks[0].runs), "Hello SpaceOS");

        // Select "SpaceOS" and apply bold + red color
        doc.selection = Selection {
            anchor: DocPos::Text {
                block: 0,
                offset: 6,
            },
            focus: DocPos::Text {
                block: 0,
                offset: 13,
            },
        };
        assert_eq!(doc.selected_text(), "SpaceOS");
        doc.apply_style_change(|st| {
            st.bold = true;
            st.color = 0xdc2626;
        });
        assert_eq!(doc.blocks[0].runs.len(), 2);
        assert!(doc.blocks[0].runs[1].style.bold);
        assert_eq!(doc.blocks[0].runs[1].style.color, 0xdc2626);

        // Undo reverts bold/color change
        assert!(doc.undo());
        assert_eq!(doc.blocks[0].runs.len(), 1);
        assert!(!doc.blocks[0].runs[0].style.bold);

        // Redo restores it
        assert!(doc.redo());
        assert_eq!(doc.blocks[0].runs.len(), 2);
        assert!(doc.blocks[0].runs[1].style.bold);

        // Find and replace
        let matches = doc.find_matches("spaceos", false);
        assert_eq!(matches.len(), 1);
        let replaced = doc.replace_all("spaceos", "SpaceOS-9", false);
        assert_eq!(replaced, 1);
        assert_eq!(runs_plain_text(&doc.blocks[0].runs), "Hello SpaceOS-9");
    }

    #[test]
    fn table_and_selection_operations() {
        let mut doc = Document::empty();
        doc.insert_table(2, 2);
        doc.insert_text("Cell A");
        assert!(doc.table_step_cell(true));
        doc.insert_text("Cell B");
        assert!(doc.table_add_row());
        assert!(doc.table_add_col());
        if let BlockKind::Table(t) = &doc.blocks[1].kind {
            assert_eq!(t.rows.len(), 3);
            assert_eq!(t.col_count(), 3);
            assert_eq!(runs_plain_text(&t.rows[0][0].runs), "Cell A");
            assert_eq!(runs_plain_text(&t.rows[0][1].runs), "Cell B");
        } else {
            panic!("expected table");
        }
    }
}
