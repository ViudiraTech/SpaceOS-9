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
use crate::theme::Theme;
use crate::ui;
use crate::x11::{CursorKind, Result};
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
const SCROLLBAR_W: i32 = ui::SCROLLBAR_HIT_W;

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
    scroll: ui::SmoothScroll,
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
            scroll: ui::SmoothScroll::new("files_scroll"),
            hover: HoverTarget::None,
            status,
            last_row_click: None,
            show_hidden,
            folders_first,
        }
    }

    fn sync_scroll_bounds(&mut self, content: Rect) {
        let rows_area = table_rows_rect(content);
        let track = scrollbar_track(content);
        let total_h = (self.entries.len() as i32 * ROW_H) as f32;
        self.scroll.set_bounds(rows_area.h as f32, total_h, track);
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
        self.scroll.snap_to(0.0);
        self.last_row_click = None;
        self.status = summarize_entries(&self.entries);
    }

    fn go_back(&mut self) -> bool {
        if let Some(prev) = self.history.pop() {
            self.navigate_to(prev, false);
            return true;
        }
        false
    }

    fn go_up(&mut self) -> bool {
        if let Some(parent) = self.current.parent().map(Path::to_path_buf) {
            if parent != self.current {
                self.navigate_to(parent, true);
                return true;
            }
        }
        false
    }

    fn refresh(&mut self, content: Rect) {
        self.entries = scan(&self.current, self.show_hidden, self.folders_first);
        if let Some(sel) = self.selected {
            if sel >= self.entries.len() {
                self.selected = self.entries.len().checked_sub(1);
            }
        }
        self.sync_scroll_bounds(content);
        self.status = format!("已刷新 · {}", summarize_entries(&self.entries));
    }

    fn apply_prefs(&mut self, show_hidden: bool, folders_first: bool, content: Rect) {
        if self.show_hidden != show_hidden || self.folders_first != folders_first {
            self.show_hidden = show_hidden;
            self.folders_first = folders_first;
            self.entries = scan(&self.current, self.show_hidden, self.folders_first);
            if let Some(sel) = self.selected {
                if sel >= self.entries.len() {
                    self.selected = self.entries.len().checked_sub(1);
                }
            }
            self.sync_scroll_bounds(content);
            self.status = summarize_entries(&self.entries);
        }
    }

    fn ensure_selected_visible(&mut self, content: Rect) {
        self.sync_scroll_bounds(content);
        let Some(sel) = self.selected else { return };
        let vp_h = table_rows_rect(content).h.max(ROW_H) as f32;
        let sel_top = (sel as i32 * ROW_H) as f32;
        let sel_bot = ((sel + 1) as i32 * ROW_H) as f32;
        let cur = self.scroll.target();
        if sel_top < cur {
            self.scroll.animate_to(sel_top, ui::MOTION_SCROLL);
        } else if sel_bot > cur + vp_h {
            self.scroll.animate_to(sel_bot - vp_h, ui::MOTION_SCROLL);
        }
    }

    fn activate_entry(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index).cloned() else {
            return;
        };
        if entry.directory {
            self.navigate_to(entry.path, true);
            return;
        }
        if is_image_file(&entry.path) {
            match std::env::current_exe() {
                Ok(exe) => match Command::new(exe).arg("view").arg(&entry.path).spawn() {
                    Ok(_) => self.status = format!("已在看图工具中打开 {}", entry.name),
                    Err(e) => self.status = format!("启动看图工具失败: {e}"),
                },
                Err(e) => self.status = format!("定位程序失败: {e}"),
            }
            return;
        }
        if is_html_file(&entry.path) {
            let url = format!("file://{}", entry.path.display());
            let bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("space-browser")))
                .filter(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from("/usr/bin/space-browser"));
            match Command::new(bin).arg(&url).spawn() {
                Ok(_) => self.status = format!("已在 Space 浏览器中打开 {}", entry.name),
                Err(e) => self.status = format!("启动浏览器失败: {e}"),
            }
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

fn content_rect(theme: &Theme, win_w: i32, win_h: i32) -> Rect {
    let bw = theme.window_border_width();
    let th = theme.titlebar_height();
    Rect::new(bw, th, (win_w - bw * 2).max(1), (win_h - th - bw).max(1))
}

fn btn_back_rect(content: Rect) -> Rect {
    Rect::new(content.x + 10, content.y + 6, 28, 28)
}

fn btn_up_rect(content: Rect) -> Rect {
    Rect::new(content.x + 42, content.y + 6, 28, 28)
}

fn btn_home_rect(content: Rect) -> Rect {
    Rect::new(content.x + 74, content.y + 6, 28, 28)
}

fn btn_refresh_rect(content: Rect) -> Rect {
    Rect::new(content.x + 106, content.y + 6, 28, 28)
}

fn btn_term_rect(content: Rect) -> Rect {
    Rect::new(content.right() - 92, content.y + 6, 82, 28)
}

fn place_rect(content: Rect, index: usize) -> Rect {
    Rect::new(
        content.x + 8,
        content.y + TOOLBAR_H + 30 + index as i32 * 34,
        SIDEBAR_W - 16,
        30,
    )
}

fn scrollbar_track(content: Rect) -> Rect {
    let top = content.y + TOOLBAR_H + HEADER_H;
    let bottom = (content.bottom() - STATUS_H).max(top);
    Rect::new(
        content.right() - SCROLLBAR_W - 2,
        top + 2,
        SCROLLBAR_W,
        (bottom - top - 4).max(8),
    )
}

fn table_rows_rect(content: Rect) -> Rect {
    let top = content.y + TOOLBAR_H + HEADER_H;
    let bottom = (content.bottom() - STATUS_H).max(top);
    Rect::new(
        content.x + SIDEBAR_W,
        top,
        (content.w - SIDEBAR_W - SCROLLBAR_W - 2).max(1),
        (bottom - top).max(0),
    )
}

fn visible_row_count(content: Rect) -> usize {
    let top = content.y + TOOLBAR_H + HEADER_H;
    let h = (content.bottom() - STATUS_H - top).max(ROW_H);
    (h / ROW_H).max(1) as usize
}

fn hit_test(state: &BrowserState, content: Rect, x: i32, y: i32) -> HoverTarget {
    if !content.contains(x, y) {
        return HoverTarget::None;
    }
    if btn_back_rect(content).contains(x, y) {
        return HoverTarget::Back;
    }
    if btn_up_rect(content).contains(x, y) {
        return HoverTarget::Up;
    }
    if btn_home_rect(content).contains(x, y) {
        return HoverTarget::Home;
    }
    if btn_refresh_rect(content).contains(x, y) {
        return HoverTarget::Refresh;
    }
    if btn_term_rect(content).contains(x, y) {
        return HoverTarget::OpenTerm;
    }
    for i in 0..state.places.len() {
        if place_rect(content, i).contains(x, y) {
            return HoverTarget::Place(i);
        }
    }
    let rows_area = table_rows_rect(content);
    if rows_area.contains(x, y) {
        let scroll_px = state.scroll.offset_i32().max(0);
        let idx = ((y - rows_area.y + scroll_px) / ROW_H) as usize;
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
        .unwrap_or_else(|| PathBuf::from("/"));
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
    state.sync_scroll_bounds(app.content());

    loop {
        if app.close {
            break;
        }
        for event in app.pump()? {
            let content = app.content();
            let vis = visible_row_count(content);
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose => app.needs_redraw = true,
                UiEvent::ConfigChanged => {
                    state.apply_prefs(
                        app.config.files_show_hidden,
                        app.config.files_folders_first,
                        content,
                    );
                    app.needs_redraw = true;
                }
                UiEvent::Configure { width, height } => {
                    state.sync_scroll_bounds(content_rect(&app.theme, width as i32, height as i32));
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    if state.scroll.handle_motion(x, y) {
                        app.needs_redraw = true;
                    }
                    app.set_content_cursor(
                        if state.scroll.is_hovered() || state.scroll.is_dragging() {
                            CursorKind::Pointer
                        } else {
                            CursorKind::Default
                        },
                    );
                    if !state.scroll.is_dragging() {
                        let target = hit_test(&state, content, x, y);
                        if target != state.hover {
                            state.hover = target;
                            app.needs_redraw = true;
                        }
                    }
                }
                UiEvent::Wheel { delta, .. } => {
                    state.sync_scroll_bounds(content);
                    if state.scroll.wheel(delta as f32 * 22.0) {
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x, y, button, down, ..
                } => {
                    if state.scroll.handle_button(x, y, button, down) {
                        app.set_content_cursor(
                            if state.scroll.is_hovered() || state.scroll.is_dragging() {
                                CursorKind::Pointer
                            } else {
                                CursorKind::Default
                            },
                        );
                        app.needs_redraw = true;
                    } else if button == 1 && down && content.contains(x, y) {
                        match hit_test(&state, content, x, y) {
                            HoverTarget::Back => {
                                if state.go_back() {
                                    crate::sound::play(
                                        &app.config,
                                        crate::sound::SystemSound::Click,
                                    );
                                    state.sync_scroll_bounds(content);
                                }
                                app.needs_redraw = true;
                            }
                            HoverTarget::Up => {
                                if state.go_up() {
                                    crate::sound::play(
                                        &app.config,
                                        crate::sound::SystemSound::Click,
                                    );
                                    state.sync_scroll_bounds(content);
                                }
                                app.needs_redraw = true;
                            }
                            HoverTarget::Home => {
                                let h = state.home.clone();
                                state.navigate_to(h, true);
                                state.sync_scroll_bounds(content);
                                crate::sound::play(&app.config, crate::sound::SystemSound::Click);
                                app.needs_redraw = true;
                            }
                            HoverTarget::Refresh => {
                                state.refresh(content);
                                crate::sound::play(&app.config, crate::sound::SystemSound::Click);
                                app.needs_redraw = true;
                            }
                            HoverTarget::OpenTerm => {
                                state.open_terminal_here();
                                crate::sound::play(&app.config, crate::sound::SystemSound::Click);
                                app.needs_redraw = true;
                            }
                            HoverTarget::Place(i) => {
                                let p = state.places[i].path.clone();
                                state.navigate_to(p, true);
                                state.sync_scroll_bounds(content);
                                crate::sound::play(&app.config, crate::sound::SystemSound::Click);
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
                                    state.sync_scroll_bounds(content);
                                    crate::sound::play(
                                        &app.config,
                                        crate::sound::SystemSound::Click,
                                    );
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
                }
                UiEvent::Key(key) => {
                    if key.is_backspace() {
                        if state.go_up() {
                            state.sync_scroll_bounds(content);
                        }
                        app.needs_redraw = true;
                    } else if key.is_enter() {
                        if let Some(idx) = state.selected {
                            state.activate_entry(idx);
                            state.sync_scroll_bounds(content);
                            app.needs_redraw = true;
                        }
                    } else if key.arrow() == Some(1) && !state.entries.is_empty() {
                        let next = state.selected.map_or(0, |s| s.saturating_sub(1));
                        state.selected = Some(next);
                        state.ensure_selected_visible(content);
                        app.needs_redraw = true;
                    } else if key.arrow() == Some(3) && !state.entries.is_empty() {
                        let next = state
                            .selected
                            .map_or(0, |s| (s + 1).min(state.entries.len() - 1));
                        state.selected = Some(next);
                        state.ensure_selected_visible(content);
                        app.needs_redraw = true;
                    } else if key.is_page_up() && !state.entries.is_empty() {
                        let next = state.selected.unwrap_or(0).saturating_sub(vis);
                        state.selected = Some(next);
                        state.ensure_selected_visible(content);
                        app.needs_redraw = true;
                    } else if key.is_page_down() && !state.entries.is_empty() {
                        let next = (state.selected.unwrap_or(0) + vis).min(state.entries.len() - 1);
                        state.selected = Some(next);
                        state.ensure_selected_visible(content);
                        app.needs_redraw = true;
                    } else if key.is_home() && !state.entries.is_empty() {
                        state.selected = Some(0);
                        state.ensure_selected_visible(content);
                        app.needs_redraw = true;
                    } else if key.is_end() && !state.entries.is_empty() {
                        state.selected = Some(state.entries.len() - 1);
                        state.ensure_selected_visible(content);
                        app.needs_redraw = true;
                    }
                }
            }
        }

        if app.needs_redraw {
            draw_browser(&mut app.canvas, &mut app.text, &app.theme, &mut state);
            app.draw_chrome();
            app.present()?;
        }
    }
    Ok(())
}

fn draw_browser(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut BrowserState,
) {
    let w = canvas.width as i32;
    let h = canvas.height as i32;
    let content = content_rect(theme, w, h);
    let is_win98 = theme.is_win98();
    let ctrl_r = theme.radius_control();
    canvas.clear(theme.window);

    // Top toolbar
    canvas.fill(Rect::new(content.x, content.y, content.w, TOOLBAR_H), theme.panel);
    if is_win98 {
        canvas.fill(
            Rect::new(content.x, content.y + TOOLBAR_H - 2, content.w, 1),
            theme.bevel_shadow,
        );
        canvas.fill(
            Rect::new(content.x, content.y + TOOLBAR_H - 1, content.w, 1),
            theme.bevel_hi,
        );
    } else {
        canvas.fill(
            Rect::new(content.x, content.y + TOOLBAR_H - 1, content.w, 1),
            theme.window_border,
        );
    }

    let can_back = !state.history.is_empty();
    let can_up = state.current.parent().is_some_and(|p| p != state.current);
    ui::icon_button(
        canvas,
        theme,
        btn_back_rect(content),
        Icon::GoBack,
        can_back,
        state.hover == HoverTarget::Back,
    );
    ui::icon_button(
        canvas,
        theme,
        btn_up_rect(content),
        Icon::GoUp,
        can_up,
        state.hover == HoverTarget::Up,
    );
    ui::icon_button(
        canvas,
        theme,
        btn_home_rect(content),
        Icon::GoHome,
        true,
        state.hover == HoverTarget::Home,
    );
    ui::icon_button(
        canvas,
        theme,
        btn_refresh_rect(content),
        Icon::Refresh,
        true,
        state.hover == HoverTarget::Refresh,
    );

    let term_btn = btn_term_rect(content);
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

    let path_box = Rect::new(
        content.x + 142,
        content.y + 6,
        (term_btn.x - (content.x + 150)).max(80),
        28,
    );
    if is_win98 {
        canvas.fill(path_box, theme.input_bg);
        ui::sunken_bevel(canvas, theme, path_box);
    } else {
        canvas.rounded_rect(path_box, ctrl_r, theme.input_bg, 1.0);
        canvas.rounded_border(path_box, ctrl_r, theme.panel_border, 1.0, 1);
    }
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
    let body_top = content.y + TOOLBAR_H;
    let body_bottom = (content.bottom() - STATUS_H).max(body_top);
    let body_h = body_bottom - body_top;
    canvas.fill(
        Rect::new(content.x, body_top, SIDEBAR_W, body_h),
        theme.window_alt,
    );
    canvas.fill(
        Rect::new(content.x + SIDEBAR_W - 1, body_top, 1, body_h),
        theme.window_border,
    );
    text.draw(
        canvas,
        "位置",
        (content.x + 16) as f32,
        (body_top + 20) as f32,
        11.0,
        theme.text_muted,
        Face::Bold,
    );

    for (i, place) in state.places.iter().enumerate() {
        let r = place_rect(content, i);
        let active = state.current == place.path;
        let fg = if is_win98 {
            ui::list_item_bg(canvas, theme, r, active, state.hover == HoverTarget::Place(i))
        } else {
            if active {
                canvas.rounded_rect(r, ctrl_r, theme.accent_soft, 1.0);
                canvas.fill(Rect::new(r.x, r.y + 6, 3, r.h - 12), theme.accent);
            } else if state.hover == HoverTarget::Place(i) {
                canvas.rounded_rect(r, ctrl_r, theme.panel_hover, 1.0);
            }
            if active { theme.text } else { theme.text_dim }
        };
        icons::draw(canvas, place.icon, r.x + 10, r.y + 7, 16, None);
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
    let table_x = content.x + SIDEBAR_W;
    let table_w = (content.w - SIDEBAR_W).max(1);
    canvas.fill(Rect::new(table_x, body_top, table_w, HEADER_H), theme.panel);
    canvas.fill(
        Rect::new(table_x, body_top + HEADER_H - 1, table_w, 1),
        theme.window_border,
    );
    let hdr_y = (body_top + 19) as f32;
    let col_type_x = (content.right() - 170).max(table_x + 160);
    let col_size_right = (content.right() - SCROLLBAR_W - 14) as f32;
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
    let rows_clip = Rect::new(
        table_x,
        rows_top,
        (table_w - SCROLLBAR_W - 2).max(1),
        (body_bottom - rows_top).max(0),
    );
    if is_win98 {
        canvas.fill(
            Rect::new(table_x, rows_top, table_w, (body_bottom - rows_top).max(0)),
            theme.input_bg,
        );
    }
    let sb_track = scrollbar_track(content);
    let total_rows_h = (state.entries.len() as i32 * ROW_H) as f32;

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
        let scroll_px = state.scroll.offset_i32().max(0);
        let first_row = (scroll_px / ROW_H).max(0) as usize;
        let vis_span = ((rows_clip.h + ROW_H - 1) / ROW_H + 1).max(1) as usize;
        canvas.with_clip(rows_clip, |c| {
            for (slot, entry) in state
                .entries
                .iter()
                .enumerate()
                .skip(first_row)
                .take(vis_span)
            {
                let ry = rows_top + slot as i32 * ROW_H - scroll_px;
                if ry >= body_bottom {
                    break;
                }
                let is_sel = state.selected == Some(slot);
                let is_hov = state.hover == HoverTarget::Row(slot);
                let row_rect = Rect::new(table_x + 4, ry + 1, rows_clip.w - 6, ROW_H - 2);
                let row_fg = if is_win98 {
                    ui::list_item_bg(c, theme, row_rect, is_sel, is_hov)
                } else {
                    if is_sel {
                        c.rounded_rect(row_rect, 4, theme.accent_soft, 1.0);
                        c.rounded_border(row_rect, 4, theme.accent, 0.55, 1);
                    } else if is_hov {
                        c.rounded_rect(row_rect, 4, theme.panel_hover, 0.75);
                    }
                    theme.text
                };
                let sub_fg = if is_win98 && is_sel {
                    theme.selection_text
                } else {
                    theme.text_muted
                };

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
                    row_fg,
                    Face::Ui,
                );
                text.draw(
                    c,
                    entry.kind_label,
                    col_type_x as f32,
                    baseline,
                    11.5,
                    sub_fg,
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
                    sub_fg,
                    Face::Ui,
                );
            }
        });
    }
    state
        .scroll
        .draw_v(canvas, theme, sb_track, rows_clip.h as f32, total_rows_h);

    // Bottom status bar
    let status_y = content.bottom() - STATUS_H;
    canvas.fill(
        Rect::new(content.x, status_y, content.w, STATUS_H),
        theme.panel,
    );
    canvas.fill(
        Rect::new(content.x, status_y, content.w, 1),
        theme.window_border,
    );
    let shown_status = text.truncate(&state.status, (content.w - 28) as f32, 11.5, Face::Ui);
    text.draw(
        canvas,
        &shown_status,
        (content.x + 12) as f32,
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

fn is_image_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "bmp" | "webp" | "gif" | "pnm" | "tga")
    )
}

fn is_html_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("html" | "htm")
    )
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
