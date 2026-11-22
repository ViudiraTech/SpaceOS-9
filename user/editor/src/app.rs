//
//       user/editor/src/app.rs
//       SpaceEditor application state, event loop, keyboard shortcuts & mouse hit handling
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use crate::doc::{
    char_to_byte, Align, BlockKind, DocPos, Document, FontFamily, MarginPreset, Orientation,
    PageSize, SearchMatch, Selection,
};
use crate::io_text::{load_document, save_document, DocFormat};
use crate::layout::DocLayout;
use spui_ui::app::{App, UiEvent};
use spui_ui::draw::{Rect, Sprite};
use spui_ui::icons::{self, Icon};
use spui_ui::ui;
use spui_ui::x11::{CursorKind, Key};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct EditorOptions {
    pub file: Option<PathBuf>,
    pub demo: bool,
    pub tab: Option<RibbonTab>,
    pub find: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RibbonTab {
    Home,
    Insert,
    Layout,
    View,
}

impl RibbonTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "开始",
            Self::Insert => "插入",
            Self::Layout => "布局",
            Self::View => "视图",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "home" | "开始" => Some(Self::Home),
            "insert" | "插入" => Some(Self::Insert),
            "layout" | "布局" => Some(Self::Layout),
            "view" | "视图" => Some(Self::View),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusedField {
    Canvas,
    FindQuery,
    ReplaceWith,
    DialogName,
    HeaderInput,
    FooterInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalKind {
    OpenFile,
    SaveAs,
    InsertImage,
    InsertTable,
    HeaderFooter,
}

#[derive(Clone, Debug)]
pub struct FileEntryItem {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

pub struct ModalState {
    pub kind: ModalKind,
    pub dir: PathBuf,
    pub entries: Vec<FileEntryItem>,
    pub selected: Option<usize>,
    pub filename: String,
    pub format: DocFormat,
    pub table_rows: usize,
    pub table_cols: usize,
    pub header_text: String,
    pub footer_text: String,
    pub scroll_offset: usize,
}

impl ModalState {
    fn open_picker(kind: ModalKind, dir: PathBuf, default_name: &str, format: DocFormat) -> Self {
        let mut m = Self {
            kind,
            dir,
            entries: Vec::new(),
            selected: None,
            filename: default_name.to_string(),
            format,
            table_rows: 3,
            table_cols: 4,
            header_text: String::new(),
            footer_text: String::new(),
            scroll_offset: 0,
        };
        m.refresh_dir();
        m
    }

    pub fn refresh_dir(&mut self) {
        let mut out = Vec::new();
        if let Some(parent) = self.dir.parent() {
            out.push(FileEntryItem {
                name: "..（上级目录）".into(),
                path: parent.to_path_buf(),
                is_dir: true,
            });
        }
        if let Ok(read) = fs::read_dir(&self.dir) {
            let mut dirs = Vec::new();
            let mut files = Vec::new();
            for entry in read.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let is_dir = path.is_dir();
                if !is_dir {
                    let ext = path
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(str::to_ascii_lowercase);
                    let keep = match self.kind {
                        ModalKind::InsertImage => matches!(
                            ext.as_deref(),
                            Some("png" | "jpg" | "jpeg")
                        ),
                        ModalKind::OpenFile | ModalKind::SaveAs => matches!(
                            ext.as_deref(),
                            Some("docx" | "md" | "markdown" | "html" | "htm" | "txt")
                        ),
                        _ => true,
                    };
                    if !keep {
                        continue;
                    }
                }
                let item = FileEntryItem { name, path, is_dir };
                if is_dir {
                    dirs.push(item);
                } else {
                    files.push(item);
                }
            }
            dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            out.extend(dirs);
            out.extend(files);
        }
        self.entries = out;
        self.selected = None;
        self.scroll_offset = 0;
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    NewDoc,
    OpenDemo,
    OpenModal(ModalKind),
    CloseModal,
    Save,
    Undo,
    Redo,
    SelectTab(RibbonTab),
    ToggleOutline,
    ToggleRuler,
    ToggleFind(bool),
    FocusField(FocusedField),
    FindNext,
    FindPrev,
    ReplaceOne,
    ReplaceAll,
    SetBlockKind(BlockKind),
    CycleFont,
    ToggleFontMenu(Rect),
    CloseFontMenu,
    SelectFontRole(FontFamily),
    SelectSystemFont(String),
    SizeDelta(f32),
    ToggleBold,
    ToggleItalic,
    ToggleUnderline,
    ToggleStrike,
    SetTextColor(u32),
    SetHighlight(Option<u32>),
    SetAlign(Align),
    InsertQuickTable(usize, usize),
    TableAddRow,
    TableAddCol,
    TableDelRow,
    InsertDivider,
    SetPageSize(PageSize),
    ToggleOrientation,
    SetMarginPreset(MarginPreset),
    SetLineSpacing(f32),
    TogglePageNumbers,
    SetZoom(f32),
    FitWidth,
    JumpOutline(usize),
    ModalSelectEntry(usize),
    ModalOpenEntry(usize),
    ModalFormat(DocFormat),
    ModalTableDelta(i32, i32),
    ModalConfirm,
    ModalScroll(i32),
    ScrollToRatio(f32),
}

pub struct Hit {
    pub rect: Rect,
    pub raw_rect: Rect,
    pub action: Action,
}

pub struct Hits {
    pub list: Vec<Hit>,
}

impl Default for Hits {
    fn default() -> Self {
        Self::new()
    }
}

impl Hits {
    pub fn new() -> Self {
        Self { list: Vec::new() }
    }

    pub fn push(&mut self, rect: Rect, action: Action) {
        if !rect.is_empty() {
            self.list.push(Hit {
                rect,
                raw_rect: rect,
                action,
            });
        }
    }

    pub fn push_clipped(&mut self, rect: Rect, clip: Rect, action: Action) {
        let vis = rect.intersect(clip);
        if !vis.is_empty() {
            self.list.push(Hit {
                rect: vis,
                raw_rect: rect,
                action,
            });
        }
    }

    pub fn at(&self, x: i32, y: i32) -> Option<&Action> {
        self.list
            .iter()
            .rev()
            .find(|h| h.rect.contains(x, y))
            .map(|h| &h.action)
    }

    pub fn hover_rect(&self, x: i32, y: i32, rect: Rect) -> bool {
        if rect.is_empty() || !rect.contains(x, y) {
            return false;
        }
        self.list
            .iter()
            .rev()
            .find(|h| h.rect.contains(x, y))
            .is_some_and(|h| h.raw_rect == rect)
    }
}

pub const PAGE_GAP: i32 = 24;

/// Discover installed font families dynamically via `spui_ui::fonts::scan`
/// without hardcoding font names.
pub fn discover_system_font_families() -> Vec<String> {
    let sysroot = std::env::var("SPUI_SYSROOT")
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/"));
    spui_ui::fonts::scan(&sysroot)
        .into_iter()
        .map(|f| f.name)
        .collect()
}

pub struct EditorState {
    pub doc: Document,
    pub layout: Option<DocLayout>,
    pub layout_dirty: bool,
    pub active_tab: RibbonTab,
    pub show_outline: bool,
    pub show_ruler: bool,
    pub zoom: f32,
    pub scroll: ui::SmoothScroll,
    pub focused: FocusedField,
    pub preedit: String,
    pub clipboard: String,
    pub find_open: bool,
    pub find_replace_open: bool,
    pub find_query: String,
    pub replace_text: String,
    pub find_matches: Vec<SearchMatch>,
    pub find_index: usize,
    pub modal: Option<ModalState>,
    pub available_fonts: Vec<String>,
    pub uxconf_ui_font: String,
    pub uxconf_doc_font: String,
    pub uxconf_mono_font: String,
    pub font_menu_open: bool,
    pub font_menu_anchor: Rect,
    pub dragging_select: bool,
    pub last_click: Option<(Instant, (i32, i32))>,
    pub last_pointer: (i32, i32),
    pub caret_blink_at: Instant,
    pub status: String,
    pub canvas_viewport: Rect,
    pub image_cache: HashMap<u64, Sprite>,
}

impl EditorState {
    pub fn new(options: &EditorOptions) -> Self {
        let (doc, status) = if let Some(path) = &options.file {
            match load_document(path) {
                Ok(d) => (d, format!("已打开 {}", path.display())),
                Err(e) => {
                    let mut d = Document::empty();
                    d.path = Some(path.clone());
                    (d, format!("新建文档 ({e})"))
                }
            }
        } else if options.demo {
            (
                Document::demo_document(),
                "已载入 SpaceOS-9 示例文档 (.docx / 图表 / 表格)".into(),
            )
        } else {
            (
                Document::empty(),
                "就绪 · 新建 A4 文档（Ctrl+S 保存 .docx，Ctrl+B/I/U 格式）".into(),
            )
        };

        let find_query = options.find.clone().unwrap_or_default();
        let find_open = !find_query.is_empty();
        let focused = if find_open {
            FocusedField::FindQuery
        } else {
            FocusedField::Canvas
        };

        let mut state = Self {
            doc,
            layout: None,
            layout_dirty: true,
            active_tab: options.tab.unwrap_or(RibbonTab::Home),
            show_outline: true,
            show_ruler: true,
            zoom: 1.0,
            scroll: ui::SmoothScroll::new("editor_doc_scroll"),
            focused,
            preedit: String::new(),
            clipboard: String::new(),
            find_open,
            find_replace_open: find_open,
            find_query,
            replace_text: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            modal: None,
            available_fonts: discover_system_font_families(),
            uxconf_ui_font: String::new(),
            uxconf_doc_font: String::new(),
            uxconf_mono_font: String::new(),
            font_menu_open: false,
            font_menu_anchor: Rect::default(),
            dragging_select: false,
            last_click: None,
            last_pointer: (0, 0),
            caret_blink_at: Instant::now(),
            status,
            canvas_viewport: Rect::new(0, 0, 800, 500),
            image_cache: HashMap::new(),
        };
        if state.find_open {
            state.refresh_find_matches();
        }
        state
    }

    pub fn sync_uxconf_fonts(&mut self, config: &uxconf::DesktopConfig) {
        self.uxconf_ui_font = config.font_ui_family.trim().to_string();
        self.uxconf_doc_font = config.font_document_family.trim().to_string();
        self.uxconf_mono_font = config.font_mono_family.trim().to_string();
        self.available_fonts = discover_system_font_families();
    }

    pub fn uxconf_font_for_role(&self, role: FontFamily) -> &str {
        match role {
            FontFamily::Serif => self.uxconf_doc_font.as_str(),
            FontFamily::Sans => self.uxconf_ui_font.as_str(),
            FontFamily::Mono => self.uxconf_mono_font.as_str(),
        }
    }

    pub fn default_scanned_family_for_role(&self, role: FontFamily) -> Option<&str> {
        match role {
            FontFamily::Serif => self
                .available_fonts
                .iter()
                .find(|n| {
                    let l = n.to_ascii_lowercase();
                    l.contains("serif") && l.contains("cjk sc")
                })
                .or_else(|| {
                    self.available_fonts.iter().find(|n| {
                        let l = n.to_ascii_lowercase();
                        l.contains("serif") && (l.contains("cjk") || l.contains("noto"))
                    })
                })
                .or_else(|| {
                    self.available_fonts
                        .iter()
                        .find(|n| n.to_ascii_lowercase().contains("serif"))
                })
                .or_else(|| self.available_fonts.first())
                .map(String::as_str),
            FontFamily::Sans => self
                .available_fonts
                .iter()
                .find(|n| {
                    let l = n.to_ascii_lowercase();
                    l.contains("sans") && !l.contains("mono") && l.contains("cjk sc")
                })
                .or_else(|| {
                    self.available_fonts.iter().find(|n| {
                        let l = n.to_ascii_lowercase();
                        l.contains("sans") && !l.contains("mono") && (l.contains("cjk") || l.contains("noto"))
                    })
                })
                .or_else(|| {
                    self.available_fonts.iter().find(|n| {
                        let l = n.to_ascii_lowercase();
                        l.contains("sans") && !l.contains("mono")
                    })
                })
                .or_else(|| self.available_fonts.first())
                .map(String::as_str),
            FontFamily::Mono => self
                .available_fonts
                .iter()
                .find(|n| {
                    let l = n.to_ascii_lowercase();
                    l.contains("mono") && l.contains("cjk sc")
                })
                .or_else(|| {
                    self.available_fonts.iter().find(|n| {
                        let l = n.to_ascii_lowercase();
                        l.contains("mono") && (l.contains("cjk") || l.contains("noto"))
                    })
                })
                .or_else(|| {
                    self.available_fonts.iter().find(|n| {
                        let l = n.to_ascii_lowercase();
                        l.contains("mono") || l.contains("hack") || l.contains("code")
                    })
                })
                .or_else(|| self.available_fonts.first())
                .map(String::as_str),
        }
    }

    /// Resolve the active font display name dynamically from `uxconfd` and
    /// `/usr/share/fonts` (`spui_ui::fonts::scan`) without hardcoded family names.
    pub fn active_font_display_name(&self) -> String {
        let role = self.doc.active_style.font;
        let configured = self.uxconf_font_for_role(role);
        if !configured.is_empty() {
            return configured.to_string();
        }
        if let Some(scanned) = self.default_scanned_family_for_role(role) {
            return scanned.to_string();
        }
        role.label().to_string()
    }

    pub fn invalidate_layout(&mut self) {
        self.layout_dirty = true;
        if self.find_open {
            self.refresh_find_matches();
        }
    }

    pub fn ensure_layout(&mut self, text: &mut spui_ui::text::TextRenderer) {
        if self.layout_dirty || self.layout.is_none() {
            self.layout = Some(DocLayout::build(&self.doc, text));
            self.layout_dirty = false;
        }
    }

    pub fn window_title(&self) -> String {
        let name = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "未命名文档.docx".into());
        if self.doc.dirty {
            format!("• {name} — SpaceEditor")
        } else {
            format!("{name} — SpaceEditor")
        }
    }

    pub fn refresh_find_matches(&mut self) {
        self.find_matches = self.doc.find_matches(&self.find_query, false);
        if self.find_index >= self.find_matches.len() {
            self.find_index = 0;
        }
    }

    pub fn jump_to_match(&mut self, idx: usize, text: &mut spui_ui::text::TextRenderer) {
        if let Some(m) = self.find_matches.get(idx).cloned() {
            self.find_index = idx;
            self.doc.selection = Selection {
                anchor: m.start,
                focus: m.end,
            };
            self.doc.sync_active_style();
            self.ensure_caret_visible(text);
        }
    }

    pub fn ensure_caret_visible(&mut self, text: &mut spui_ui::text::TextRenderer) {
        self.ensure_layout(text);
        let Some(layout) = &self.layout else {
            return;
        };
        let geom = layout.caret_geom(&self.doc, self.doc.caret(), text);
        let page_top = layout.page_top_y(geom.page_index, self.zoom, PAGE_GAP) as f32;
        let cy = page_top + geom.y * self.zoom;
        let ch = (geom.height * self.zoom).max(16.0);
        let vp_h = self.canvas_viewport.h.max(120) as f32;
        let cur = self.scroll.target();
        if cy < cur + 16.0 {
            self.scroll.animate_to((cy - 24.0).max(0.0), ui::MOTION_SCROLL);
        } else if cy + ch > cur + vp_h - 16.0 {
            self.scroll
                .animate_to((cy + ch - vp_h + 24.0).max(0.0), ui::MOTION_SCROLL);
        }
    }

    pub fn default_docs_dir(&self) -> PathBuf {
        if let Some(p) = self.doc.path.as_ref().and_then(|p| p.parent()) {
            if p.is_dir() {
                return p.to_path_buf();
            }
        }
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            let h = PathBuf::from(home);
            let docs = h.join("Documents");
            if docs.is_dir() {
                return docs;
            }
            if h.is_dir() {
                return h;
            }
        }
        PathBuf::from("/tmp")
    }

    pub fn decode_cached_image(&mut self, id: u64, bytes: &[u8]) -> Option<&Sprite> {
        if !self.image_cache.contains_key(&id) {
            if let Some(sprite) = icons::decode_image_bytes(bytes) {
                self.image_cache.insert(id, sprite);
            }
        }
        self.image_cache.get(&id)
    }

    pub fn active_text_input_mut(&mut self) -> Option<&mut String> {
        match self.focused {
            FocusedField::FindQuery => Some(&mut self.find_query),
            FocusedField::ReplaceWith => Some(&mut self.replace_text),
            FocusedField::DialogName => self.modal.as_mut().map(|m| &mut m.filename),
            FocusedField::HeaderInput => self.modal.as_mut().map(|m| &mut m.header_text),
            FocusedField::FooterInput => self.modal.as_mut().map(|m| &mut m.footer_text),
            FocusedField::Canvas => None,
        }
    }
}

pub fn run_editor(options: EditorOptions) -> Result<(), String> {
    let mut state = EditorState::new(&options);
    let mut app = App::new(&state.window_title(), Icon::Document, 1040, 640)
        .map_err(|e| e.to_string())?;
    app.min_width = 760;
    app.min_height = 500;
    state.sync_uxconf_fonts(&app.config);

    let mut prev_hits = Hits::new();
    let mut last_title = state.window_title();

    loop {
        if app.close {
            break;
        }

        let new_title = state.window_title();
        if new_title != last_title {
            let _ = app.x.set_title(app.win, &new_title);
            app.title = new_title.clone();
            last_title = new_title;
            app.needs_redraw = true;
        }

        let content = app.content();
        state.ensure_layout(&mut app.text);
        app.set_ime_focus(true);

        if app.needs_redraw || ui::has_active_animations() || state.scroll.is_animating() {
            let mut hits = Hits::new();
            let ime_spot = {
                let App {
                    canvas, text, theme, ..
                } = &mut app;
                crate::draw::draw_editor(canvas, text, theme, &mut state, content, &prev_hits, &mut hits)
            };
            if let Some(spot) = ime_spot {
                app.set_ime_spot(spot);
            }
            let hover_cursor = if state.modal.is_none() && state.canvas_viewport.contains(state.last_pointer.0, state.last_pointer.1) {
                CursorKind::Text
            } else {
                CursorKind::Default
            };
            app.set_content_cursor(hover_cursor);
            app.draw_chrome();
            app.present().map_err(|e| e.to_string())?;
            prev_hits = hits;
        }

        let timeout = if ui::has_active_animations() || state.scroll.is_animating() {
            16
        } else {
            180
        };
        let events = app.pump_with_fd(None, timeout).map_err(|e| e.to_string())?.0;
        for ev in events {
            handle_event(&mut app, &mut state, &prev_hits, content, ev);
        }
    }
    Ok(())
}

fn handle_event(
    app: &mut App,
    state: &mut EditorState,
    hits: &Hits,
    content: Rect,
    event: UiEvent,
) {
    match event {
        UiEvent::Close => app.request_close(),
        UiEvent::ConfigChanged => {
            state.sync_uxconf_fonts(&app.config);
            state.invalidate_layout();
            app.needs_redraw = true;
        }
        UiEvent::Expose | UiEvent::Configure { .. } => {
            state.invalidate_layout();
            app.needs_redraw = true;
        }
        UiEvent::Motion { x, y, state: mask } => {
            state.last_pointer = (x, y);
            ui::notify_pointer_motion(x, y, mask & 0x100 != 0);
            if state.dragging_select && state.modal.is_none() && state.canvas_viewport.contains(x, y) {
                if let Some((p_idx, lx, ly)) = crate::draw::screen_to_page_local(state, content, x, y) {
                    state.ensure_layout(&mut app.text);
                    if let Some(layout) = &state.layout {
                        let pos = layout.hit_test(&state.doc, p_idx, lx, ly, &mut app.text);
                        state.doc.set_caret(pos, true);
                    }
                }
            }
            app.needs_redraw = true;
        }
        UiEvent::Wheel { x, y, delta } => {
            if state.modal.is_some() {
                if let Some(m) = state.modal.as_mut() {
                    let max_off = m.entries.len().saturating_sub(8);
                    if delta < 0 {
                        m.scroll_offset = (m.scroll_offset + 2).min(max_off);
                    } else if delta > 0 {
                        m.scroll_offset = m.scroll_offset.saturating_sub(2);
                    }
                    app.needs_redraw = true;
                }
                return;
            }
            let _ = (x, y);
            // Positive wheel step per SPUI SmoothScroll convention!
            let step = (delta as f32) * 42.0;
            if state.scroll.wheel(step) {
                app.needs_redraw = true;
            }
        }
        UiEvent::Button {
            x,
            y,
            button,
            down,
            state: mod_mask,
        } => {
            state.last_pointer = (x, y);
            ui::notify_pointer_button(x, y, button, down);
            let shift = (mod_mask & 1) != 0;

            if button == 1 && down {
                if state.modal.is_none()
                    && !state.font_menu_open
                    && state.canvas_viewport.contains(x, y)
                    && hits.at(x, y).is_none()
                {
                    state.focused = FocusedField::Canvas;
                    state.caret_blink_at = Instant::now();
                    if let Some((p_idx, lx, ly)) = crate::draw::screen_to_page_local(state, content, x, y) {
                        state.ensure_layout(&mut app.text);
                        if let Some(layout) = &state.layout {
                            let pos = layout.hit_test(&state.doc, p_idx, lx, ly, &mut app.text);
                            state.doc.set_caret(pos, shift);
                            state.dragging_select = true;
                        }
                    }
                    app.needs_redraw = true;
                    return;
                }
            }

            if button == 1 && !down {
                if state.dragging_select {
                    state.dragging_select = false;
                    // Double-click word/block select check
                    let now = Instant::now();
                    if let Some((prev_at, (px, py))) = state.last_click {
                        if now.saturating_duration_since(prev_at) < Duration::from_millis(360)
                            && (x - px).abs() < 6
                            && (y - py).abs() < 6
                        {
                            select_current_word_or_line(&mut state.doc);
                            state.last_click = None;
                            app.needs_redraw = true;
                            return;
                        }
                    }
                    state.last_click = Some((now, (x, y)));
                    app.needs_redraw = true;
                    return;
                }

                if let Some(action) = hits.at(x, y).cloned() {
                    perform_action(app, state, action);
                    app.needs_redraw = true;
                }
            }
        }
        UiEvent::Preedit(txt) => {
            state.preedit = txt;
            app.needs_redraw = true;
        }
        UiEvent::CommitText(txt) => {
            state.preedit.clear();
            if let Some(buf) = state.active_text_input_mut() {
                buf.push_str(&txt);
                if state.focused == FocusedField::FindQuery {
                    state.refresh_find_matches();
                }
            } else if state.modal.is_none() {
                state.doc.insert_text(&txt);
                state.invalidate_layout();
                state.ensure_caret_visible(&mut app.text);
                state.caret_blink_at = Instant::now();
            }
            app.needs_redraw = true;
        }
        UiEvent::Key(key) => {
            handle_key(app, state, key);
            app.needs_redraw = true;
        }
    }
}

fn select_current_word_or_line(doc: &mut Document) {
    if let DocPos::Text { block, offset } = doc.caret() {
        let text = crate::doc::runs_plain_text(&doc.blocks[block].runs);
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return;
        }
        let mid = offset.min(chars.len() - 1);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        if is_word(chars[mid]) && chars[mid].is_ascii() {
            let mut s = mid;
            while s > 0 && is_word(chars[s - 1]) && chars[s - 1].is_ascii() {
                s -= 1;
            }
            let mut e = mid + 1;
            while e < chars.len() && is_word(chars[e]) && chars[e].is_ascii() {
                e += 1;
            }
            doc.selection = Selection {
                anchor: DocPos::Text { block, offset: s },
                focus: DocPos::Text { block, offset: e },
            };
        } else {
            doc.selection = Selection {
                anchor: DocPos::Text { block, offset: 0 },
                focus: DocPos::Text {
                    block,
                    offset: chars.len(),
                },
            };
        }
        doc.sync_active_style();
    }
}

fn handle_key(app: &mut App, state: &mut EditorState, key: Key) {
    if state.font_menu_open && key.is_escape() {
        state.font_menu_open = false;
        return;
    }
    if state.modal.is_some() {
        if key.is_escape() {
            perform_action(app, state, Action::CloseModal);
            return;
        }
        if key.is_enter() {
            perform_action(app, state, Action::ModalConfirm);
            return;
        }
        if key.is_tab() {
            if let Some(m) = &state.modal {
                if m.kind == ModalKind::HeaderFooter {
                    state.focused = if state.focused == FocusedField::HeaderInput {
                        FocusedField::FooterInput
                    } else {
                        FocusedField::HeaderInput
                    };
                }
            }
            return;
        }
        if key.is_backspace() || key.sym == 0xff08 {
            if let Some(buf) = state.active_text_input_mut() {
                buf.pop();
            }
            return;
        }
        if !key.control() && !key.alt() {
            if let Some(ch) = key.text() {
                if !ch.is_control() {
                    if let Some(buf) = state.active_text_input_mut() {
                        buf.push(ch);
                    }
                }
            }
        }
        return;
    }

    // Global shortcuts even when Find bar focused
    if key.control() {
        match key.text() {
            Some('s') | Some('S') => {
                if key.shift() {
                    perform_action(app, state, Action::OpenModal(ModalKind::SaveAs));
                } else {
                    perform_action(app, state, Action::Save);
                }
                return;
            }
            Some('o') | Some('O') => {
                perform_action(app, state, Action::OpenModal(ModalKind::OpenFile));
                return;
            }
            Some('n') | Some('N') => {
                perform_action(app, state, Action::NewDoc);
                return;
            }
            Some('f') | Some('F') => {
                perform_action(app, state, Action::ToggleFind(false));
                return;
            }
            Some('h') | Some('H') => {
                perform_action(app, state, Action::ToggleFind(true));
                return;
            }
            Some('=') | Some('+') => {
                perform_action(app, state, Action::SetZoom((state.zoom + 0.15).min(1.6)));
                return;
            }
            Some('-') | Some('_') => {
                perform_action(app, state, Action::SetZoom((state.zoom - 0.15).max(0.6)));
                return;
            }
            Some('0') => {
                perform_action(app, state, Action::SetZoom(1.0));
                return;
            }
            _ => {}
        }
    }

    if state.focused != FocusedField::Canvas {
        if key.is_escape() {
            state.find_open = false;
            state.find_replace_open = false;
            state.focused = FocusedField::Canvas;
            state.preedit.clear();
            return;
        }
        if key.is_tab() && state.find_replace_open {
            state.focused = if state.focused == FocusedField::FindQuery {
                FocusedField::ReplaceWith
            } else {
                FocusedField::FindQuery
            };
            return;
        }
        if key.is_enter() {
            perform_action(
                app,
                state,
                if key.shift() {
                    Action::FindPrev
                } else {
                    Action::FindNext
                },
            );
            return;
        }
        if key.is_backspace() || key.sym == 0xff08 {
            if let Some(buf) = state.active_text_input_mut() {
                buf.pop();
                if state.focused == FocusedField::FindQuery {
                    state.refresh_find_matches();
                }
            }
            return;
        }
        if !key.control() && !key.alt() {
            if let Some(ch) = key.text() {
                if !ch.is_control() {
                    if let Some(buf) = state.active_text_input_mut() {
                        buf.push(ch);
                        if state.focused == FocusedField::FindQuery {
                            state.refresh_find_matches();
                        }
                    }
                }
            }
        }
        return;
    }

    if key.is_escape() {
        if state.find_open {
            state.find_open = false;
            state.find_replace_open = false;
        } else if !state.doc.selection.is_collapsed() {
            let c = state.doc.caret();
            state.doc.set_caret(c, false);
        }
        return;
    }

    if key.control() {
        match key.text() {
            Some('b') | Some('B') => {
                perform_action(app, state, Action::ToggleBold);
                return;
            }
            Some('i') | Some('I') => {
                perform_action(app, state, Action::ToggleItalic);
                return;
            }
            Some('u') | Some('U') => {
                perform_action(app, state, Action::ToggleUnderline);
                return;
            }
            Some('z') | Some('Z') => {
                perform_action(app, state, Action::Undo);
                return;
            }
            Some('y') | Some('Y') => {
                perform_action(app, state, Action::Redo);
                return;
            }
            Some('a') | Some('A') => {
                state.doc.select_all();
                return;
            }
            Some('c') | Some('C') => {
                let s = state.doc.selected_text();
                if !s.is_empty() {
                    state.clipboard = s;
                    state.status = format!("已复制 {} 个字符", state.clipboard.chars().count());
                }
                return;
            }
            Some('x') | Some('X') => {
                let s = state.doc.selected_text();
                if !s.is_empty() {
                    state.clipboard = s;
                    state.doc.backspace();
                    state.invalidate_layout();
                    state.status = "已剪切所选内容".into();
                }
                return;
            }
            Some('v') | Some('V') => {
                if !state.clipboard.is_empty() {
                    let clip = state.clipboard.clone();
                    state.doc.insert_text(&clip);
                    state.invalidate_layout();
                    state.ensure_caret_visible(&mut app.text);
                }
                return;
            }
            Some('1') => {
                perform_action(app, state, Action::SetBlockKind(BlockKind::Heading(1)));
                return;
            }
            Some('2') => {
                perform_action(app, state, Action::SetBlockKind(BlockKind::Heading(2)));
                return;
            }
            Some('3') => {
                perform_action(app, state, Action::SetBlockKind(BlockKind::Heading(3)));
                return;
            }
            _ => {}
        }
    }

    let shift = key.shift();
    state.caret_blink_at = Instant::now();
    if let Some(dir) = key.arrow() {
        match dir {
            0 => state.doc.move_left(shift),
            2 => state.doc.move_right(shift),
            1 | 3 => {
                state.ensure_layout(&mut app.text);
                if let Some(layout) = &state.layout {
                    let next = layout.move_vertical(&state.doc, dir == 1, &mut app.text);
                    state.doc.set_caret(next, shift);
                }
            }
            _ => {}
        }
        state.ensure_caret_visible(&mut app.text);
        return;
    }

    if key.is_home() {
        if let DocPos::Text { block, .. } = state.doc.caret() {
            state.doc.set_caret(DocPos::Text { block, offset: 0 }, shift);
        }
        state.ensure_caret_visible(&mut app.text);
        return;
    }
    if key.is_end() {
        if let DocPos::Text { block, .. } = state.doc.caret() {
            let len = crate::doc::runs_char_len(&state.doc.blocks[block].runs);
            state.doc.set_caret(DocPos::Text { block, offset: len }, shift);
        }
        state.ensure_caret_visible(&mut app.text);
        return;
    }
    if key.is_page_up() {
        state.scroll.wheel(320.0);
        return;
    }
    if key.is_page_down() {
        state.scroll.wheel(-320.0);
        return;
    }
    if key.is_tab() {
        if !state.doc.table_step_cell(!shift) {
            state.doc.insert_text("    ");
            state.invalidate_layout();
        }
        state.ensure_caret_visible(&mut app.text);
        return;
    }
    if key.is_enter() {
        state.doc.insert_newline();
        state.invalidate_layout();
        state.ensure_caret_visible(&mut app.text);
        return;
    }
    if key.is_backspace() || key.sym == 0xff08 {
        state.doc.backspace();
        state.invalidate_layout();
        state.ensure_caret_visible(&mut app.text);
        return;
    }
    if key.is_delete() {
        state.doc.delete_forward();
        state.invalidate_layout();
        state.ensure_caret_visible(&mut app.text);
        return;
    }
    if !key.control() && !key.alt() {
        if let Some(ch) = key.text() {
            if !ch.is_control() {
                state.doc.insert_text(&ch.to_string());
                state.invalidate_layout();
                state.ensure_caret_visible(&mut app.text);
            }
        }
    }
}

fn apply_uxconf_font_for_role(
    app: &mut App,
    state: &mut EditorState,
    role: FontFamily,
    family: String,
) {
    let prop = match role {
        FontFamily::Serif => {
            app.config.font_document_family = family.clone();
            "/document-family"
        }
        FontFamily::Sans => {
            app.config.font_ui_family = family.clone();
            "/ui-family"
        }
        FontFamily::Mono => {
            app.config.font_mono_family = family.clone();
            "/monospace-family"
        }
    };
    if let Some(client) = app.uxconf.as_mut() {
        let _ = client.set("font", prop, uxconf::Value::String(family.clone()));
    }
    let prefs = spui_ui::text::FontPrefs::from_values(
        &app.config.font_ui_family,
        &app.config.font_document_family,
        &app.config.font_mono_family,
    );
    if let Ok(mut loaded) = spui_ui::text::TextRenderer::load_with(&prefs) {
        loaded.set_ui_scale(app.theme.ui_scale());
        app.text = loaded;
    }
    state.sync_uxconf_fonts(&app.config);
    state.doc.apply_style_change(|st| st.font = role);
    state.invalidate_layout();
    state.focused = FocusedField::Canvas;
    let shown = state.active_font_display_name();
    state.status = if family.is_empty() {
        format!("已恢复默认字体：{shown} (uxconfd font:{prop})")
    } else {
        format!("已切换字体：{shown} (uxconfd font:{prop})")
    };
}

pub fn perform_action(app: &mut App, state: &mut EditorState, action: Action) {
    if !matches!(
        action,
        Action::ToggleFontMenu(_) | Action::SelectFontRole(_)
    ) {
        state.font_menu_open = false;
    }
    match action {
        Action::NewDoc => {
            state.doc = Document::empty();
            state.image_cache.clear();
            state.scroll.snap_to(0.0);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
            state.status = "已创建空白 A4 文档".into();
        }
        Action::OpenDemo => {
            state.doc = Document::demo_document();
            state.image_cache.clear();
            state.scroll.snap_to(0.0);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
            state.status = "已载入 SpaceOS-9 示例文档".into();
        }
        Action::OpenModal(kind) => {
            let dir = state.default_docs_dir();
            let default_name = state
                .doc
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "SpaceOS文档.docx".into());
            let fmt = state
                .doc
                .path
                .as_deref()
                .map(DocFormat::from_path)
                .unwrap_or(DocFormat::Docx);
            let mut modal = ModalState::open_picker(kind, dir, &default_name, fmt);
            modal.header_text = state.doc.setup.header.clone();
            modal.footer_text = state.doc.setup.footer.clone();
            state.focused = match kind {
                ModalKind::SaveAs | ModalKind::OpenFile | ModalKind::InsertImage => {
                    FocusedField::DialogName
                }
                ModalKind::HeaderFooter => FocusedField::HeaderInput,
                ModalKind::InsertTable => FocusedField::Canvas,
            };
            state.modal = Some(modal);
        }
        Action::CloseModal => {
            state.modal = None;
            state.focused = FocusedField::Canvas;
            state.preedit.clear();
        }
        Action::Save => {
            if let Some(path) = state.doc.path.clone() {
                match save_document(&state.doc, &path) {
                    Ok(()) => {
                        state.doc.dirty = false;
                        state.status = format!("已保存 {}", path.display());
                    }
                    Err(e) => state.status = format!("保存失败: {e}"),
                }
            } else {
                perform_action(app, state, Action::OpenModal(ModalKind::SaveAs));
            }
        }
        Action::Undo => {
            if state.doc.undo() {
                state.invalidate_layout();
                state.ensure_caret_visible(&mut app.text);
                state.status = "已撤销".into();
            }
        }
        Action::Redo => {
            if state.doc.redo() {
                state.invalidate_layout();
                state.ensure_caret_visible(&mut app.text);
                state.status = "已重做".into();
            }
        }
        Action::SelectTab(tab) => {
            state.active_tab = tab;
        }
        Action::ToggleOutline => {
            state.show_outline = !state.show_outline;
        }
        Action::ToggleRuler => {
            state.show_ruler = !state.show_ruler;
        }
        Action::ToggleFind(with_replace) => {
            if state.find_open && state.find_replace_open == with_replace {
                state.find_open = false;
                state.find_replace_open = false;
                state.focused = FocusedField::Canvas;
            } else {
                state.find_open = true;
                state.find_replace_open = with_replace;
                state.focused = FocusedField::FindQuery;
                state.refresh_find_matches();
            }
        }
        Action::FocusField(f) => {
            state.focused = f;
        }
        Action::FindNext => {
            state.refresh_find_matches();
            if !state.find_matches.is_empty() {
                let next = (state.find_index + 1) % state.find_matches.len();
                state.jump_to_match(next, &mut app.text);
            }
        }
        Action::FindPrev => {
            state.refresh_find_matches();
            if !state.find_matches.is_empty() {
                let prev = if state.find_index == 0 {
                    state.find_matches.len() - 1
                } else {
                    state.find_index - 1
                };
                state.jump_to_match(prev, &mut app.text);
            }
        }
        Action::ReplaceOne => {
            state.refresh_find_matches();
            if let Some(m) = state.find_matches.get(state.find_index).cloned() {
                let repl = state.replace_text.clone();
                if state.doc.replace_match(&m, &repl) {
                    state.invalidate_layout();
                    state.refresh_find_matches();
                    state.status = "已替换当前匹配项".into();
                }
            }
        }
        Action::ReplaceAll => {
            let n = state.doc.replace_all(&state.find_query.clone(), &state.replace_text.clone(), false);
            state.invalidate_layout();
            state.refresh_find_matches();
            state.status = format!("共替换 {n} 处匹配");
        }
        Action::SetBlockKind(kind) => {
            state.doc.set_block_kind(kind);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
        }
        Action::ToggleFontMenu(anchor) => {
            state.available_fonts = discover_system_font_families();
            state.font_menu_anchor = anchor;
            state.font_menu_open = !state.font_menu_open;
        }
        Action::CloseFontMenu => {
            state.font_menu_open = false;
        }
        Action::SelectFontRole(role) => {
            state.doc.apply_style_change(|st| st.font = role);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
            let shown = state.active_font_display_name();
            state.status = format!("字体角色切换为 {} ({shown})", role.label());
        }
        Action::SelectSystemFont(family) => {
            let role = state.doc.active_style.font;
            apply_uxconf_font_for_role(app, state, role, family);
        }
        Action::CycleFont => {
            state.available_fonts = discover_system_font_families();
            let role = state.doc.active_style.font;
            if state.available_fonts.is_empty() {
                let next = match role {
                    FontFamily::Sans => FontFamily::Serif,
                    FontFamily::Serif => FontFamily::Mono,
                    FontFamily::Mono => FontFamily::Sans,
                };
                state.doc.apply_style_change(|st| st.font = next);
                state.invalidate_layout();
                state.focused = FocusedField::Canvas;
                let shown = state.active_font_display_name();
                state.status = format!("字体切换为 {shown}");
            } else {
                let current = state.uxconf_font_for_role(role);
                let next_family = if current.is_empty() {
                    state.available_fonts[0].clone()
                } else {
                    match state.available_fonts.iter().position(|n| n == current) {
                        Some(idx) if idx + 1 < state.available_fonts.len() => {
                            state.available_fonts[idx + 1].clone()
                        }
                        _ => String::new(),
                    }
                };
                apply_uxconf_font_for_role(app, state, role, next_family);
            }
        }
        Action::SizeDelta(d) => {
            let cur = state.doc.active_style.size_pt;
            let next = (cur + d).clamp(8.0, 36.0);
            state.doc.apply_style_change(|st| st.size_pt = next);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
        }
        Action::ToggleBold => {
            let v = !state.doc.active_style.bold;
            state.doc.apply_style_change(|st| st.bold = v);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
        }
        Action::ToggleItalic => {
            let v = !state.doc.active_style.italic;
            state.doc.apply_style_change(|st| st.italic = v);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
        }
        Action::ToggleUnderline => {
            let v = !state.doc.active_style.underline;
            state.doc.apply_style_change(|st| st.underline = v);
            state.focused = FocusedField::Canvas;
        }
        Action::ToggleStrike => {
            let v = !state.doc.active_style.strikethrough;
            state.doc.apply_style_change(|st| st.strikethrough = v);
            state.focused = FocusedField::Canvas;
        }
        Action::SetTextColor(rgb) => {
            state.doc.apply_style_change(|st| st.color = rgb);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
        }
        Action::SetHighlight(hl) => {
            state.doc.apply_style_change(|st| st.highlight = hl);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
        }
        Action::SetAlign(a) => {
            state.doc.set_align(a);
            state.invalidate_layout();
            state.focused = FocusedField::Canvas;
        }
        Action::InsertQuickTable(r, c) => {
            state.doc.insert_table(r, c);
            state.invalidate_layout();
            state.ensure_caret_visible(&mut app.text);
            state.focused = FocusedField::Canvas;
            state.status = format!("已插入 {r}×{c} 表格（Tab 切换单元格）");
        }
        Action::TableAddRow => {
            if state.doc.table_add_row() {
                state.invalidate_layout();
                state.ensure_caret_visible(&mut app.text);
                state.status = "已增加表格行".into();
            } else {
                state.status = "请先将光标放入表格单元格内".into();
            }
        }
        Action::TableAddCol => {
            if state.doc.table_add_col() {
                state.invalidate_layout();
                state.status = "已增加表格列".into();
            } else {
                state.status = "请先将光标放入表格单元格内".into();
            }
        }
        Action::TableDelRow => {
            if state.doc.table_del_row() {
                state.invalidate_layout();
                state.status = "已删除当前表格行".into();
            }
        }
        Action::InsertDivider => {
            state.doc.insert_divider();
            state.invalidate_layout();
            state.ensure_caret_visible(&mut app.text);
            state.focused = FocusedField::Canvas;
        }
        Action::SetPageSize(sz) => {
            state.doc.setup.size = sz;
            state.doc.dirty = true;
            state.invalidate_layout();
            state.status = format!("纸张尺寸设为 {}", sz.label());
        }
        Action::ToggleOrientation => {
            state.doc.setup.orientation = match state.doc.setup.orientation {
                Orientation::Portrait => Orientation::Landscape,
                Orientation::Landscape => Orientation::Portrait,
            };
            state.doc.dirty = true;
            state.invalidate_layout();
        }
        Action::SetMarginPreset(preset) => {
            state.doc.setup.set_margin_preset(preset);
            state.doc.dirty = true;
            state.invalidate_layout();
            state.status = format!("页边距设为 {}", preset.label());
        }
        Action::SetLineSpacing(sp) => {
            state.doc.set_line_spacing(sp);
            state.invalidate_layout();
            state.status = format!("行距设为 {sp:.2}");
        }
        Action::TogglePageNumbers => {
            state.doc.setup.show_page_numbers = !state.doc.setup.show_page_numbers;
            state.doc.dirty = true;
        }
        Action::SetZoom(z) => {
            state.zoom = z.clamp(0.6, 1.6);
        }
        Action::FitWidth => {
            state.ensure_layout(&mut app.text);
            let pw = state.layout.as_ref().map_or(794.0, |l| l.pages[0].width as f32);
            let avail = (state.canvas_viewport.w - 48).max(320) as f32;
            state.zoom = (avail / pw).clamp(0.6, 1.5);
        }
        Action::JumpOutline(block) => {
            state.ensure_layout(&mut app.text);
            if let Some(layout) = &state.layout {
                let target = layout.block_scroll_target(block, state.zoom, PAGE_GAP);
                state.scroll.animate_to(target, ui::MOTION_SCROLL);
                state.doc.set_caret(DocPos::Text { block, offset: 0 }, false);
                state.focused = FocusedField::Canvas;
            }
        }
        Action::ModalSelectEntry(idx) => {
            if let Some(m) = state.modal.as_mut() {
                m.selected = Some(idx);
                if let Some(item) = m.entries.get(idx).cloned() {
                    if !item.is_dir {
                        m.filename = item.name;
                    }
                }
            }
        }
        Action::ModalOpenEntry(idx) => {
            let item = state
                .modal
                .as_ref()
                .and_then(|m| m.entries.get(idx))
                .cloned();
            if let Some(item) = item {
                if item.is_dir {
                    if let Some(m) = state.modal.as_mut() {
                        m.dir = item.path;
                        m.refresh_dir();
                    }
                } else {
                    if let Some(m) = state.modal.as_mut() {
                        m.selected = Some(idx);
                        m.filename = item.name;
                    }
                    perform_action(app, state, Action::ModalConfirm);
                }
            }
        }
        Action::ModalFormat(fmt) => {
            if let Some(m) = state.modal.as_mut() {
                m.format = fmt;
                let base = Path::new(&m.filename)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "SpaceOS文档".into());
                m.filename = format!("{base}.{}", fmt.ext());
            }
        }
        Action::ModalTableDelta(dr, dc) => {
            if let Some(m) = state.modal.as_mut() {
                m.table_rows = (m.table_rows as i32 + dr).clamp(1, 16) as usize;
                m.table_cols = (m.table_cols as i32 + dc).clamp(1, 8) as usize;
            }
        }
        Action::ModalScroll(delta) => {
            if let Some(m) = state.modal.as_mut() {
                let max_off = m.entries.len().saturating_sub(8);
                if delta > 0 {
                    m.scroll_offset = (m.scroll_offset + 2).min(max_off);
                } else {
                    m.scroll_offset = m.scroll_offset.saturating_sub(2);
                }
            }
        }
        Action::ScrollToRatio(r) => {
            let max = state.scroll.max_offset();
            state.scroll.snap_to((r.clamp(0.0, 1.0)) * max);
        }
        Action::ModalConfirm => {
            let Some(m) = state.modal.take() else {
                return;
            };
            state.focused = FocusedField::Canvas;
            state.preedit.clear();
            match m.kind {
                ModalKind::InsertTable => {
                    state.doc.insert_table(m.table_rows, m.table_cols);
                    state.invalidate_layout();
                    state.ensure_caret_visible(&mut app.text);
                    state.status = format!("已插入 {}×{} 表格", m.table_rows, m.table_cols);
                }
                ModalKind::HeaderFooter => {
                    state.doc.setup.header = m.header_text.trim().to_string();
                    state.doc.setup.footer = m.footer_text.trim().to_string();
                    state.doc.dirty = true;
                    state.status = "已更新页眉与页脚".into();
                }
                ModalKind::OpenFile => {
                    let target = resolve_modal_path(&m);
                    match load_document(&target) {
                        Ok(d) => {
                            state.doc = d;
                            state.image_cache.clear();
                            state.scroll.snap_to(0.0);
                            state.invalidate_layout();
                            state.status = format!("已打开 {}", target.display());
                        }
                        Err(e) => state.status = format!("打开失败: {e}"),
                    }
                }
                ModalKind::SaveAs => {
                    let mut target = resolve_modal_path(&m);
                    if target.extension().is_none() {
                        target.set_extension(m.format.ext());
                    }
                    match save_document(&state.doc, &target) {
                        Ok(()) => {
                            state.doc.path = Some(target.clone());
                            state.doc.dirty = false;
                            state.status = format!("已保存到 {}", target.display());
                        }
                        Err(e) => state.status = format!("保存失败: {e}"),
                    }
                }
                ModalKind::InsertImage => {
                    let target = resolve_modal_path(&m);
                    let fname = target
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "image.png".into());
                    match fs::read(&target) {
                        Ok(bytes) => {
                            if let Err(e) = state.doc.insert_image_bytes(&fname, bytes) {
                                state.status = e;
                            } else {
                                state.invalidate_layout();
                                state.ensure_caret_visible(&mut app.text);
                                state.status = format!("已插入图片 {fname}");
                            }
                        }
                        Err(e) => state.status = format!("读取图片失败: {e}"),
                    }
                }
            }
        }
    }
    let _ = char_to_byte;
}

fn resolve_modal_path(m: &ModalState) -> PathBuf {
    let trimmed = m.filename.trim();
    if trimmed.is_empty() {
        return m.dir.join(format!("SpaceOS文档.{}", m.format.ext()));
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        m.dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uxconf_font_sync_and_dynamic_display_name() {
        let mut state = EditorState::new(&EditorOptions::default());
        let mut cfg = uxconf::DesktopConfig::default();
        cfg.font_document_family = "Hack".into();
        cfg.font_ui_family = "Orbitron".into();
        cfg.font_mono_family = "Rajdhani".into();
        state.sync_uxconf_fonts(&cfg);

        // Default body style is FontFamily::Serif -> font:/document-family
        assert_eq!(state.doc.active_style.font, FontFamily::Serif);
        assert_eq!(state.active_font_display_name(), "Hack");

        state.doc.active_style.font = FontFamily::Sans;
        assert_eq!(state.active_font_display_name(), "Orbitron");

        state.doc.active_style.font = FontFamily::Mono;
        assert_eq!(state.active_font_display_name(), "Rajdhani");

        // When uxconf role is empty, falls back to discovered system fonts from /usr/share/fonts
        cfg.font_document_family.clear();
        state.sync_uxconf_fonts(&cfg);
        state.doc.active_style.font = FontFamily::Serif;
        let resolved = state.active_font_display_name();
        assert!(!resolved.is_empty());
        if !state.available_fonts.is_empty() {
            assert!(state.available_fonts.contains(&resolved));
        }
    }
}
