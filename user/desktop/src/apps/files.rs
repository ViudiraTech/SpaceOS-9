/*
 *
 *       user/desktop/src/apps/files.rs
 *       SPUI 4.0 file manager with Places sidebar, Breeze icons and table scrolling
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! High-density native file browser featuring a Places sidebar, history/parent
//! toolbar, Breeze MIME/folder icons, mouse-wheel scrolling and text editor
//! / terminal integration.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::{self, Icon};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, RADIUS_CONTROL, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::Result;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const TOOLBAR_H: i32 = 40;
const SIDEBAR_W: i32 = 168;
const HEADER_H: i32 = 28;
const ROW_H: i32 = 30;
const STATUS_H: i32 = 26;
const SCROLLBAR_W: i32 = 8;

#[derive(Clone)]
struct Entry {
    name: String,
    path: PathBuf,
    directory: bool,
    size: u64,
    icon: Icon,
    kind_label: &'static str,
}

struct Place {
    label: &'static str,
    path: PathBuf,
    icon: Icon,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HoverTarget {
    None,
    Back,
    Up,
    Home,
    Refresh,
    OpenTerm,
    Place(usize),
    Row(usize),
}

struct BrowserState {
    home: PathBuf,
    places: Vec<Place>,
    current: PathBuf,
    history: Vec<PathBuf>,
    entries: Vec<Entry>,
    selected: Option<usize>,
    scroll: usize,
    hover: HoverTarget,
    status: String,
    last_row_click: Option<(usize, Instant)>,
    show_hidden: bool,
    folders_first: bool,
}

impl BrowserState {
    fn new(initial: PathBuf, home: PathBuf, show_hidden: bool, folders_first: bool) -> Self {
        let places = vec![
            Place {
                label: "主目录",
                path: home.clone(),
                icon: Icon::UserHome,
            },
            Place {
                label: "根目录 /",
                path: PathBuf::from("/"),
                icon: Icon::FolderRoot,
            },
            Place {
                label: "系统配置 /etc",
                path: PathBuf::from("/etc"),
                icon: Icon::FolderDocuments,
            },
            Place {
                label: "程序资源 /usr",
                path: PathBuf::from("/usr"),
                icon: Icon::FolderScript,
            },
            Place {
                label: "临时目录 /tmp",
                path: PathBuf::from("/tmp"),
                icon: Icon::FolderTemp,
            },
        ];
        let entries = scan(&initial, show_hidden, folders_first);
        let status = summarize_entries(&entries);
        Self {
            home,
            places,
            current: initial,
            history: Vec::new(),
            entries,
            selected: None,
            scroll: 0,
            hover: HoverTarget::None,
            status,
            last_row_click: None,
            show_hidden,
            folders_first,
        }
    }

    fn navigate_to(&mut self, target: PathBuf, record_history: bool) {
        if !target.is_dir() {
            self.status = format!("无法打开目录: {}", target.display());
            return;
        }
        if record_history && target != self.current {
            self.history.push(self.current.clone());
        }
        self.current = target;
        self.entries = scan(&self.current, self.show_hidden, self.folders_first);
        self.selected = None;
        self.scroll = 0;
        self.last_row_click = None;
        self.status = summarize_entries(&self.entries);
    }

    fn go_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.navigate_to(prev, false);
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.current.parent().map(Path::to_path_buf) {
            if parent != self.current {
                self.navigate_to(parent, true);
            }
        }
    }

    fn refresh(&mut self, visible_rows: usize) {
        self.entries = scan(&self.current, self.show_hidden, self.folders_first);
        if let Some(sel) = self.selected {
            if sel >= self.entries.len() {
                self.selected = self.entries.len().checked_sub(1);
            }
        }
        let max_scroll = self.entries.len().saturating_sub(visible_rows.max(1));
        self.scroll = self.scroll.min(max_scroll);
        self.status = format!("已刷新 · {}", summarize_entries(&self.entries));
    }

    fn apply_prefs(&mut self, show_hidden: bool, folders_first: bool, visible_rows: usize) {
        if self.show_hidden != show_hidden || self.folders_first != folders_first {
            self.show_hidden = show_hidden;
            self.folders_first = folders_first;
            self.entries = scan(&self.current, self.show_hidden, self.folders_first);
            if let Some(sel) = self.selected {
                if sel >= self.entries.len() {
                    self.selected = self.entries.len().checked_sub(1);
                }
            }
            let max_scroll = self.entries.len().saturating_sub(visible_rows.max(1));
            self.scroll = self.scroll.min(max_scroll);
            self.status = summarize_entries(&self.entries);
        }
    }

    fn ensure_selected_visible(&mut self, visible_rows: usize) {
        let Some(sel) = self.selected else { return };
        let vis = visible_rows.max(1);
        if sel < self.scroll {
            self.scroll = sel;
        } else if sel >= self.scroll + vis {
            self.scroll = sel + 1 - vis;
        }
        let max_scroll = self.entries.len().saturating_sub(vis);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn activate_entry(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index).cloned() else {
            return;
        };
        if entry.directory {
            self.navigate_to(entry.path, true);
            return;
        }
        if is_editable_text(&entry.path, entry.size) {
            match std::env::current_exe() {
                Ok(exe) => match Command::new(exe).arg("edit").arg(&entry.path).spawn() {
                    Ok(_) => self.status = format!("已在文本编辑器中打开 {}", entry.name),
                    Err(e) => self.status = format!("启动编辑器失败: {e}"),
                },
                Err(e) => self.status = format!("定位程序失败: {e}"),
            }
        } else {
            self.status = format!(
                "{} ({}) 为二进制或超大文件",
                entry.name,
                format_size(entry.size)
            );
        }
    }

    fn open_terminal_here(&mut self) {
        match std::env::current_exe() {
            Ok(exe) => match Command::new(exe).arg("term").arg(&self.current).spawn() {
                Ok(_) => self.status = format!("已在 {} 打开终端", self.current.display()),
                Err(e) => self.status = format!("启动终端失败: {e}"),
            },
            Err(e) => self.status = format!("定位程序失败: {e}"),
        }
    }
}

fn btn_back_rect() -> Rect {
    Rect::new(10, TITLEBAR_HEIGHT + 6, 28, 28)
}

fn btn_up_rect() -> Rect {
    Rect::new(42, TITLEBAR_HEIGHT + 6, 28, 28)
}

fn btn_home_rect() -> Rect {
    Rect::new(74, TITLEBAR_HEIGHT + 6, 28, 28)
}

fn btn_refresh_rect() -> Rect {
    Rect::new(106, TITLEBAR_HEIGHT + 6, 28, 28)
}

fn btn_term_rect(win_w: i32) -> Rect {
    Rect::new(win_w - 92, TITLEBAR_HEIGHT + 6, 82, 28)
}

fn place_rect(index: usize) -> Rect {
    Rect::new(
        8,
        TITLEBAR_HEIGHT + TOOLBAR_H + 30 + index as i32 * 34,
        SIDEBAR_W - 16,
        30,
    )
}

fn table_rows_rect(win_w: i32, win_h: i32) -> Rect {
    let top = TITLEBAR_HEIGHT + TOOLBAR_H + HEADER_H;
    let bottom = (win_h - STATUS_H).max(top);
    Rect::new(
        SIDEBAR_W,
        top,
        (win_w - SIDEBAR_W - SCROLLBAR_W - 2).max(1),
        (bottom - top).max(0),
    )
}

fn visible_row_count(win_h: i32) -> usize {
    let top = TITLEBAR_HEIGHT + TOOLBAR_H + HEADER_H;
    let h = (win_h - STATUS_H - top).max(ROW_H);
    (h / ROW_H).max(1) as usize
}

fn hit_test(state: &BrowserState, win_w: i32, win_h: i32, x: i32, y: i32) -> HoverTarget {
    if y < TITLEBAR_HEIGHT {
        return HoverTarget::None;
    }
    if btn_back_rect().contains(x, y) {
        return HoverTarget::Back;
    }
    if btn_up_rect().contains(x, y) {
        return HoverTarget::Up;
    }
    if btn_home_rect().contains(x, y) {
        return HoverTarget::Home;
    }
    if btn_refresh_rect().contains(x, y) {
        return HoverTarget::Refresh;
    }
    if btn_term_rect(win_w).contains(x, y) {
        return HoverTarget::OpenTerm;
    }
    for i in 0..state.places.len() {
        if place_rect(i).contains(x, y) {
            return HoverTarget::Place(i);
        }
    }
    let rows_area = table_rows_rect(win_w, win_h);
    if rows_area.contains(x, y) {
        let idx = state.scroll + ((y - rows_area.y) / ROW_H) as usize;
        if idx < state.entries.len() {
            return HoverTarget::Row(idx);
        }
    }
    HoverTarget::None
}

pub fn run(argument: Option<&str>) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("/root"));
    let initial = argument
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| home.clone());

    let mut app = App::new("文件管理器", Icon::Files, 880, 560)?;
    app.min_width = 560;
    app.min_height = 340;

    let mut state = BrowserState::new(
        initial,
        home,
        app.config.files_show_hidden,
        app.config.files_folders_first,
    );

    loop {
        if app.close {
            break;
        }
        for event in app.pump()? {
            let vis = visible_row_count(app.height as i32);
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose => app.needs_redraw = true,
                UiEvent::ConfigChanged => {
                    state.apply_prefs(
                        app.config.files_show_hidden,
                        app.config.files_folders_first,
                        vis,
                    );
                    app.needs_redraw = true;
                }
                UiEvent::Configure { .. } => {
                    let max_scroll = state.entries.len().saturating_sub(vis);
                    state.scroll = state.scroll.min(max_scroll);
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    let target = hit_test(&state, app.width as i32, app.height as i32, x, y);
                    if target != state.hover {
                        state.hover = target;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Wheel { delta, .. } => {
                    let max_scroll = state.entries.len().saturating_sub(vis);
                    let next = if delta > 0 {
                        (state.scroll + delta as usize).min(max_scroll)
                    } else {
                        state.scroll.saturating_sub((-delta) as usize)
                    };
                    if next != state.scroll {
                        state.scroll = next;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x,
                    y,
                    button: 1,
                    down: true,
                    ..
                } if y >= TITLEBAR_HEIGHT => {
                    match hit_test(&state, app.width as i32, app.height as i32, x, y) {
                        HoverTarget::Back => {
                            state.go_back();
                            app.needs_redraw = true;
                        }
                        HoverTarget::Up => {
                            state.go_up();
                            app.needs_redraw = true;
                        }
                        HoverTarget::Home => {
                            let h = state.home.clone();
                            state.navigate_to(h, true);
                            app.needs_redraw = true;
                        }
                        HoverTarget::Refresh => {
                            state.refresh(vis);
                            app.needs_redraw = true;
                        }
                        HoverTarget::OpenTerm => {
                            state.open_terminal_here();
                            app.needs_redraw = true;
                        }
                        HoverTarget::Place(i) => {
                            let p = state.places[i].path.clone();
                            state.navigate_to(p, true);
                            app.needs_redraw = true;
                        }
                        HoverTarget::Row(idx) => {
                            let now = Instant::now();
                            let dc_ms = app.config.mouse_double_click_ms;
                            let is_double =
                                state.last_row_click.is_some_and(|(prev_idx, prev_t)| {
                                    prev_idx == idx
                                        && now.duration_since(prev_t)
                                            <= Duration::from_millis(dc_ms)
                                });
                            state.selected = Some(idx);
                            if app.config.files_single_click || is_double {
                                state.last_row_click = None;
                                state.activate_entry(idx);
                            } else {
                                state.last_row_click = Some((idx, now));
                                if let Some(entry) = state.entries.get(idx) {
                                    state.status = format!(
                                        "{} · {} (双击或按 Enter 打开)",
                                        entry.name,
                                        if entry.directory {
                                            "文件夹".to_string()
                                        } else {
                                            format_size(entry.size)
                                        }
                                    );
                                }
                            }
                            app.needs_redraw = true;
                        }
                        HoverTarget::None => {
                            if state.selected.take().is_some() {
                                state.status = summarize_entries(&state.entries);
                                app.needs_redraw = true;
                            }
                        }
                    }
                }
                UiEvent::Key(key) => {
                    if key.is_backspace() {
                        state.go_up();
                        app.needs_redraw = true;
                    } else if key.is_enter() {
                        if let Some(idx) = state.selected {
                            state.activate_entry(idx);
                            app.needs_redraw = true;
                        }
                    } else if key.arrow() == Some(1) && !state.entries.is_empty() {
                        let next = state.selected.map_or(0, |s| s.saturating_sub(1));
                        state.selected = Some(next);
                        state.ensure_selected_visible(vis);
                        app.needs_redraw = true;
                    } else if key.arrow() == Some(3) && !state.entries.is_empty() {
                        let next = state
                            .selected
                            .map_or(0, |s| (s + 1).min(state.entries.len() - 1));
                        state.selected = Some(next);
                        state.ensure_selected_visible(vis);
                        app.needs_redraw = true;
                    } else if key.is_page_up() && !state.entries.is_empty() {
                        let next = state.selected.unwrap_or(0).saturating_sub(vis);
                        state.selected = Some(next);
                        state.ensure_selected_visible(vis);
                        app.needs_redraw = true;
                    } else if key.is_page_down() && !state.entries.is_empty() {
                        let next = (state.selected.unwrap_or(0) + vis).min(state.entries.len() - 1);
                        state.selected = Some(next);
                        state.ensure_selected_visible(vis);
                        app.needs_redraw = true;
                    } else if key.is_home() && !state.entries.is_empty() {
                        state.selected = Some(0);
                        state.ensure_selected_visible(vis);
                        app.needs_redraw = true;
                    } else if key.is_end() && !state.entries.is_empty() {
                        state.selected = Some(state.entries.len() - 1);
                        state.ensure_selected_visible(vis);
                        app.needs_redraw = true;
                    }
                }
                _ => {}
            }
        }

        if app.needs_redraw {
            draw_browser(&mut app.canvas, &mut app.text, &app.theme, &state);
            app.draw_chrome();
            app.present()?;
        }
    }
    Ok(())
}

fn draw_browser(canvas: &mut Canvas, text: &mut TextRenderer, theme: &Theme, state: &BrowserState) {
    let w = canvas.width as i32;
    let h = canvas.height as i32;
    canvas.clear(theme.window);

    // Top toolbar
    canvas.fill(Rect::new(0, TITLEBAR_HEIGHT, w, TOOLBAR_H), theme.panel);
    canvas.fill(
        Rect::new(0, TITLEBAR_HEIGHT + TOOLBAR_H - 1, w, 1),
        theme.window_border,
    );

    let can_back = !state.history.is_empty();
    let can_up = state.current.parent().is_some_and(|p| p != state.current);
    ui::icon_button(
        canvas,
        theme,
        btn_back_rect(),
        Icon::GoBack,
        can_back,
        state.hover == HoverTarget::Back,
    );
    ui::icon_button(
        canvas,
        theme,
        btn_up_rect(),
        Icon::GoUp,
        can_up,
        state.hover == HoverTarget::Up,
    );
    ui::icon_button(
        canvas,
        theme,
        btn_home_rect(),
        Icon::GoHome,
        true,
        state.hover == HoverTarget::Home,
    );
    ui::icon_button(
        canvas,
        theme,
        btn_refresh_rect(),
        Icon::Refresh,
        true,
        state.hover == HoverTarget::Refresh,
    );

    let term_btn = btn_term_rect(w);
    ui::button(
        canvas,
        text,
        theme,
        term_btn,
        "终端",
        Some(Icon::Terminal),
        false,
        state.hover == HoverTarget::OpenTerm,
    );

    let path_box = Rect::new(142, TITLEBAR_HEIGHT + 6, (term_btn.x - 150).max(80), 28);
    canvas.rounded_rect(path_box, RADIUS_CONTROL, theme.input_bg, 1.0);
    canvas.rounded_border(path_box, RADIUS_CONTROL, theme.panel_border, 1.0, 1);
    icons::draw(
        canvas,
        Icon::Folder,
        path_box.x + 8,
        path_box.y + 6,
        16,
        None,
    );
    let path_str = state.current.to_string_lossy();
    let shown_path = text.truncate(&path_str, (path_box.w - 36) as f32, 12.5, Face::Ui);
    text.draw(
        canvas,
        &shown_path,
        (path_box.x + 30) as f32,
        (path_box.y + 19) as f32,
        12.5,
        theme.text,
        Face::Ui,
    );

    // Left Places sidebar
    let body_top = TITLEBAR_HEIGHT + TOOLBAR_H;
    let body_bottom = (h - STATUS_H).max(body_top);
    let body_h = body_bottom - body_top;
    canvas.fill(Rect::new(0, body_top, SIDEBAR_W, body_h), theme.window_alt);
    canvas.fill(
        Rect::new(SIDEBAR_W - 1, body_top, 1, body_h),
        theme.window_border,
    );
    text.draw(
        canvas,
        "位置",
        16.0,
        (body_top + 20) as f32,
        11.0,
        theme.text_muted,
        Face::Bold,
    );

    for (i, place) in state.places.iter().enumerate() {
        let r = place_rect(i);
        let active = state.current == place.path;
        if active {
            canvas.rounded_rect(r, RADIUS_CONTROL, theme.accent_soft, 1.0);
            canvas.fill(Rect::new(r.x, r.y + 6, 3, r.h - 12), theme.accent);
        } else if state.hover == HoverTarget::Place(i) {
            canvas.rounded_rect(r, RADIUS_CONTROL, theme.panel_hover, 1.0);
        }
        icons::draw(canvas, place.icon, r.x + 10, r.y + 7, 16, None);
        let fg = if active { theme.text } else { theme.text_dim };
        text.draw(
            canvas,
            place.label,
            (r.x + 32) as f32,
            (r.y + 20) as f32,
            12.0,
            fg,
            Face::Ui,
        );
    }

    // Table header
    let table_x = SIDEBAR_W;
    let table_w = (w - SIDEBAR_W).max(1);
    canvas.fill(Rect::new(table_x, body_top, table_w, HEADER_H), theme.panel);
    canvas.fill(
        Rect::new(table_x, body_top + HEADER_H - 1, table_w, 1),
        theme.window_border,
    );
    let hdr_y = (body_top + 19) as f32;
    let col_type_x = (w - 170).max(table_x + 160);
    let col_size_right = (w - SCROLLBAR_W - 14) as f32;
    text.draw(
        canvas,
        "名称",
        (table_x + 36) as f32,
        hdr_y,
        11.5,
        theme.text_muted,
        Face::Bold,
    );
    text.draw(
        canvas,
        "类型",
        col_type_x as f32,
        hdr_y,
        11.5,
        theme.text_muted,
        Face::Bold,
    );
    text.draw_right(
        canvas,
        "大小",
        col_size_right,
        hdr_y,
        11.5,
        theme.text_muted,
        Face::Bold,
    );

    // Table rows
    let rows_top = body_top + HEADER_H;
    let vis_rows = visible_row_count(h);
    let rows_clip = Rect::new(
        table_x,
        rows_top,
        (table_w - SCROLLBAR_W - 2).max(1),
        (body_bottom - rows_top).max(0),
    );

    if state.entries.is_empty() {
        let cx = table_x + table_w / 2;
        let cy = rows_top + (body_bottom - rows_top) / 2;
        icons::draw(canvas, Icon::FolderLarge, cx - 24, cy - 38, 48, None);
        text.draw_centered(
            canvas,
            "此目录为空",
            cx as f32,
            (cy + 24) as f32,
            13.0,
            theme.text_muted,
            Face::Ui,
        );
    } else {
        canvas.with_clip(rows_clip, |c| {
            for (slot, entry) in state
                .entries
                .iter()
                .enumerate()
                .skip(state.scroll)
                .take(vis_rows + 1)
            {
                let ry = rows_top + (slot - state.scroll) as i32 * ROW_H;
                if ry >= body_bottom {
                    break;
                }
                let row_rect = Rect::new(table_x + 4, ry + 1, rows_clip.w - 6, ROW_H - 2);
                if state.selected == Some(slot) {
                    c.rounded_rect(row_rect, 4, theme.accent_soft, 1.0);
                    c.rounded_border(row_rect, 4, theme.accent, 0.55, 1);
                } else if state.hover == HoverTarget::Row(slot) {
                    c.rounded_rect(row_rect, 4, theme.panel_hover, 0.75);
                }

                icons::draw(c, entry.icon, table_x + 12, ry + (ROW_H - 16) / 2, 16, None);
                let max_name_w = (col_type_x - (table_x + 42)).max(48) as f32;
                let shown_name = text.truncate(&entry.name, max_name_w, 12.5, Face::Ui);
                let baseline = (ry + 20) as f32;
                text.draw(
                    c,
                    &shown_name,
                    (table_x + 36) as f32,
                    baseline,
                    12.5,
                    theme.text,
                    Face::Ui,
                );
                text.draw(
                    c,
                    entry.kind_label,
                    col_type_x as f32,
                    baseline,
                    11.5,
                    theme.text_muted,
                    Face::Ui,
                );
                let size_str = if entry.directory {
                    "—".to_string()
                } else {
                    format_size(entry.size)
                };
                text.draw_right(
                    c,
                    &size_str,
                    col_size_right,
                    baseline,
                    11.5,
                    theme.text_muted,
                    Face::Ui,
                );
            }
        });

        let sb_track = Rect::new(
            w - SCROLLBAR_W - 2,
            rows_top + 2,
            SCROLLBAR_W,
            (body_bottom - rows_top - 4).max(8),
        );
        ui::scrollbar_v(
            canvas,
            theme,
            sb_track,
            state.entries.len(),
            vis_rows,
            state.scroll,
        );
    }

    // Bottom status bar
    let status_y = h - STATUS_H;
    canvas.fill(Rect::new(0, status_y, w, STATUS_H), theme.panel);
    canvas.fill(Rect::new(0, status_y, w, 1), theme.window_border);
    let shown_status = text.truncate(&state.status, (w - 28) as f32, 11.5, Face::Ui);
    text.draw(
        canvas,
        &shown_status,
        12.0,
        (status_y + 17) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );
}

fn summarize_entries(entries: &[Entry]) -> String {
    let dirs = entries.iter().filter(|e| e.directory).count();
    let files = entries.len().saturating_sub(dirs);
    format!(
        "共 {} 项 ({} 个文件夹, {} 个文件)",
        entries.len(),
        dirs,
        files
    )
}

fn classify_kind(path: &Path, is_dir: bool) -> &'static str {
    if is_dir {
        return "文件夹";
    }
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("rs") => "Rust 源码",
        Some("c" | "h" | "cpp" | "hpp") => "C/C++ 源码",
        Some("sh") => "Shell 脚本",
        Some("py") => "Python 脚本",
        Some("toml" | "json" | "yaml" | "yml" | "conf" | "ini" | "service") => "配置文件",
        Some("md" | "txt") => "文本文档",
        Some("log") => "日志文件",
        Some("png" | "jpg" | "jpeg" | "svg" | "bmp") => "图像",
        None => "系统文件",
        _ => "文件",
    }
}

fn is_editable_text(path: &Path, size: u64) -> bool {
    if size > 2 * 1024 * 1024 {
        return false;
    }
    if matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "txt"
                | "md"
                | "rs"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "py"
                | "toml"
                | "json"
                | "yaml"
                | "yml"
                | "conf"
                | "log"
                | "sh"
                | "service"
                | "ini"
                | "desktop"
                | "xml"
                | "svg"
                | "alias"
                | "dir"
        )
    ) {
        return true;
    }
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut sample = [0u8; 512];
    let Ok(n) = f.read(&mut sample) else {
        return false;
    };
    !sample[..n].contains(&0)
}

fn scan(path: &Path, show_hidden: bool, folders_first: bool) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut result = read
        .filter_map(|item| {
            let item = item.ok()?;
            let name = item.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            let entry_path = item.path();
            let metadata = fs::metadata(&entry_path)
                .or_else(|_| item.metadata())
                .ok()?;
            let directory = metadata.is_dir();
            Some(Entry {
                icon: Icon::for_file(&entry_path, directory),
                kind_label: classify_kind(&entry_path, directory),
                path: entry_path,
                name,
                directory,
                size: metadata.len(),
            })
        })
        .collect::<Vec<_>>();
    result.sort_by(|a, b| {
        if folders_first {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        } else {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        }
    });
    result
}

fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    }
}
