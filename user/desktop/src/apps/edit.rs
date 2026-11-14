/*
 *
 *       user/desktop/src/apps/edit.rs
 *       SPUI 4.0 UTF-8 text editor with 2D cursor, scrolling and live uxconfd preferences
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Clean native UTF-8 text editor with toggleable monospace line-number gutter,
//! configurable font size, active-line highlight and tab width, 2D keyboard &
//! mouse cursor positioning, wheel scrolling and toolbar / `Ctrl+S` saving.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::{self, Icon};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::{CursorKind, Result};
use std::fs;
use std::path::{Path, PathBuf};
use uxconf::DesktopConfig;

const TOOLBAR_H: i32 = 38;
const STATUS_H: i32 = 25;
const DEFAULT_GUTTER_W: i32 = 52;
const TEXT_PAD_X: i32 = 12;
const TEXT_PAD_Y: i32 = 6;
const SCROLLBAR_W: i32 = ui::SCROLLBAR_HIT_W;

#[derive(Clone, Copy)]
struct EditorPrefs {
    font_px: f32,
    line_h: i32,
    baseline_off: f32,
    gutter_w: i32,
    highlight_active: bool,
    tab_width: usize,
}

impl EditorPrefs {
    fn from_config(cfg: &DesktopConfig) -> Self {
        let font_px = (cfg.editor_font_size.clamp(11, 20) as f32) - 0.5;
        let line_h = ((font_px * 1.55).round() as i32).max(17);
        let baseline_off = (line_h as f32 * 0.72).round();
        Self {
            font_px,
            line_h,
            baseline_off,
            gutter_w: if cfg.editor_show_line_numbers {
                DEFAULT_GUTTER_W
            } else {
                0
            },
            highlight_active: cfg.editor_highlight_active_line,
            tab_width: cfg.editor_tab_width.clamp(2, 8),
        }
    }
}

struct Editor {
    path: PathBuf,
    lines: Vec<String>,
    cur_line: usize,
    cur_col: usize,
    preferred_col: usize,
    scroll: ui::SmoothScroll,
    saved: bool,
    save_hover: bool,
    status: String,
}

impl Editor {
    fn open(path: PathBuf, tab_width: usize) -> Self {
        let tab_spaces = " ".repeat(tab_width.clamp(2, 8));
        let (raw, status) = match fs::read_to_string(&path) {
            Ok(data) => {
                let normalized = data.replace("\r\n", "\n").replace('\t', &tab_spaces);
                let msg = format!("已打开 {}", path.display());
                (normalized, msg)
            }
            Err(_) if !path.exists() => (String::new(), format!("新建文件 {}", path.display())),
            Err(err) => (String::new(), format!("读取失败: {err}")),
        };
        let mut lines: Vec<String> = raw.split('\n').map(str::to_string).collect();
        if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            path,
            lines,
            cur_line: 0,
            cur_col: 0,
            preferred_col: 0,
            scroll: ui::SmoothScroll::new("edit_scroll"),
            saved: true,
            save_hover: false,
            status,
        }
    }

    fn window_title(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned());
        if self.saved {
            format!("{name} — 文本编辑器")
        } else {
            format!("• {name} — 文本编辑器")
        }
    }

    fn line_char_len(&self, line: usize) -> usize {
        self.lines[line].chars().count()
    }

    fn char_to_byte(s: &str, char_idx: usize) -> usize {
        s.char_indices().nth(char_idx).map_or(s.len(), |(b, _)| b)
    }

    fn mark_modified(&mut self) {
        self.saved = false;
    }

    fn save(&mut self) -> bool {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        let mut out = self.lines.join("\n");
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        match fs::write(&self.path, out) {
            Ok(()) => {
                self.saved = true;
                self.status = format!("已保存 {}", self.path.display());
                true
            }
            Err(err) => {
                self.status = format!("保存失败: {err}");
                false
            }
        }
    }

    fn insert_str(&mut self, s: &str) {
        let byte_idx = Self::char_to_byte(&self.lines[self.cur_line], self.cur_col);
        self.lines[self.cur_line].insert_str(byte_idx, s);
        self.cur_col += s.chars().count();
        self.preferred_col = self.cur_col;
        self.mark_modified();
    }

    fn insert_newline(&mut self) {
        let byte_idx = Self::char_to_byte(&self.lines[self.cur_line], self.cur_col);
        let tail = self.lines[self.cur_line].split_off(byte_idx);
        self.cur_line += 1;
        self.lines.insert(self.cur_line, tail);
        self.cur_col = 0;
        self.preferred_col = 0;
        self.mark_modified();
    }

    fn backspace(&mut self) {
        if self.cur_col > 0 {
            let start = Self::char_to_byte(&self.lines[self.cur_line], self.cur_col - 1);
            let end = Self::char_to_byte(&self.lines[self.cur_line], self.cur_col);
            self.lines[self.cur_line].replace_range(start..end, "");
            self.cur_col -= 1;
            self.preferred_col = self.cur_col;
            self.mark_modified();
        } else if self.cur_line > 0 {
            let current = self.lines.remove(self.cur_line);
            self.cur_line -= 1;
            let prev_chars = self.line_char_len(self.cur_line);
            self.lines[self.cur_line].push_str(&current);
            self.cur_col = prev_chars;
            self.preferred_col = self.cur_col;
            self.mark_modified();
        }
    }

    fn delete_forward(&mut self) {
        let len = self.line_char_len(self.cur_line);
        if self.cur_col < len {
            let start = Self::char_to_byte(&self.lines[self.cur_line], self.cur_col);
            let end = Self::char_to_byte(&self.lines[self.cur_line], self.cur_col + 1);
            self.lines[self.cur_line].replace_range(start..end, "");
            self.mark_modified();
        } else if self.cur_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cur_line + 1);
            self.lines[self.cur_line].push_str(&next);
            self.mark_modified();
        }
    }

    fn move_left(&mut self) {
        if self.cur_col > 0 {
            self.cur_col -= 1;
        } else if self.cur_line > 0 {
            self.cur_line -= 1;
            self.cur_col = self.line_char_len(self.cur_line);
        }
        self.preferred_col = self.cur_col;
    }

    fn move_right(&mut self) {
        let len = self.line_char_len(self.cur_line);
        if self.cur_col < len {
            self.cur_col += 1;
        } else if self.cur_line + 1 < self.lines.len() {
            self.cur_line += 1;
            self.cur_col = 0;
        }
        self.preferred_col = self.cur_col;
    }

    fn move_up(&mut self, delta: usize) {
        self.cur_line = self.cur_line.saturating_sub(delta);
        self.cur_col = self.preferred_col.min(self.line_char_len(self.cur_line));
    }

    fn move_down(&mut self, delta: usize) {
        self.cur_line = (self.cur_line + delta).min(self.lines.len() - 1);
        self.cur_col = self.preferred_col.min(self.line_char_len(self.cur_line));
    }

    fn sync_scroll_bounds(&mut self, win_w: i32, win_h: i32, line_h: i32) {
        let body = editor_body_rect(win_w, win_h);
        let vp_h = (body.h - TEXT_PAD_Y * 2).max(line_h) as f32;
        let total_h = (self.lines.len() as i32 * line_h) as f32;
        self.scroll
            .set_bounds(vp_h, total_h, scrollbar_track(win_w, win_h));
    }

    fn ensure_cursor_visible(&mut self, win_w: i32, win_h: i32, line_h: i32) {
        self.sync_scroll_bounds(win_w, win_h, line_h);
        let body = editor_body_rect(win_w, win_h);
        let vp_h = (body.h - TEXT_PAD_Y * 2).max(line_h) as f32;
        let line_top = (self.cur_line as i32 * line_h) as f32;
        let line_bot = ((self.cur_line + 1) as i32 * line_h) as f32;
        let cur = self.scroll.target();
        if line_top < cur {
            self.scroll.animate_to(line_top, ui::MOTION_SCROLL);
        } else if line_bot > cur + vp_h {
            self.scroll.animate_to(line_bot - vp_h, ui::MOTION_SCROLL);
        }
    }
}

fn save_btn_rect() -> Rect {
    Rect::new(10, TITLEBAR_HEIGHT + 5, 76, 28)
}

fn editor_body_rect(win_w: i32, win_h: i32) -> Rect {
    let top = TITLEBAR_HEIGHT + TOOLBAR_H;
    let bottom = (win_h - STATUS_H).max(top);
    Rect::new(0, top, win_w, bottom - top)
}

fn scrollbar_track(win_w: i32, win_h: i32) -> Rect {
    let body = editor_body_rect(win_w, win_h);
    Rect::new(
        win_w - SCROLLBAR_W - 2,
        body.y + 2,
        SCROLLBAR_W,
        (body.h - 4).max(8),
    )
}

fn visible_line_count(win_h: i32, line_h: i32) -> usize {
    let h = (win_h - TITLEBAR_HEIGHT - TOOLBAR_H - STATUS_H - TEXT_PAD_Y * 2).max(line_h);
    (h / line_h).max(1) as usize
}

fn hit_text_cursor(
    editor: &Editor,
    text: &mut TextRenderer,
    prefs: EditorPrefs,
    win_w: i32,
    win_h: i32,
    mx: i32,
    my: i32,
) -> Option<(usize, usize)> {
    let body = editor_body_rect(win_w, win_h);
    if my < body.y || my >= body.bottom() || mx >= win_w - SCROLLBAR_W - 2 {
        return None;
    }
    let scroll_px = editor.scroll.offset_i32().max(0);
    let rel_y = (my - body.y - TEXT_PAD_Y + scroll_px).max(0);
    let line_idx = ((rel_y / prefs.line_h) as usize).min(editor.lines.len() - 1);
    let line = &editor.lines[line_idx];
    let rel_x = (mx - (prefs.gutter_w + TEXT_PAD_X)) as f32;
    if rel_x <= 0.0 {
        return Some((line_idx, 0));
    }
    let mut acc = 0.0f32;
    let mut col = 0usize;
    for ch in line.chars() {
        let adv = text.glyph(Face::Mono, ch, prefs.font_px).advance;
        if rel_x < acc + adv * 0.5 {
            break;
        }
        acc += adv;
        col += 1;
    }
    Some((line_idx, col))
}

pub fn run(argument: Option<&str>) -> Result<()> {
    let path = argument.map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("untitled.txt")
    });

    let mut app = App::new("文本编辑器", Icon::Editor, 840, 580)?;
    app.min_width = 480;
    app.min_height = 300;
    app.set_content_cursor(CursorKind::Text);

    let mut prefs = EditorPrefs::from_config(&app.config);
    let mut editor = Editor::open(path, prefs.tab_width);
    editor.sync_scroll_bounds(app.width as i32, app.height as i32, prefs.line_h);
    app.title = editor.window_title();
    let _ = app.x.set_title(app.win, &app.title);

    loop {
        if app.close {
            break;
        }
        let prev_saved = editor.saved;

        for event in app.pump()? {
            let win_w = app.width as i32;
            let win_h = app.height as i32;
            let vis = visible_line_count(win_h, prefs.line_h);
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose => app.needs_redraw = true,
                UiEvent::ConfigChanged => {
                    prefs = EditorPrefs::from_config(&app.config);
                    editor.sync_scroll_bounds(win_w, win_h, prefs.line_h);
                    app.needs_redraw = true;
                }
                UiEvent::Configure { width, height } => {
                    editor.sync_scroll_bounds(width as i32, height as i32, prefs.line_h);
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    if editor.scroll.handle_motion(x, y) {
                        app.needs_redraw = true;
                    }
                    if !editor.scroll.is_dragging() {
                        let hover = save_btn_rect().contains(x, y);
                        if hover != editor.save_hover {
                            editor.save_hover = hover;
                            app.needs_redraw = true;
                        }
                    }
                    let body = editor_body_rect(win_w, win_h);
                    app.set_content_cursor(
                        if editor.scroll.is_hovered() || editor.scroll.is_dragging() {
                            CursorKind::Pointer
                        } else if body.contains(x, y)
                            && x >= prefs.gutter_w
                            && x < win_w - SCROLLBAR_W - 2
                        {
                            CursorKind::Text
                        } else {
                            CursorKind::Default
                        },
                    );
                }
                UiEvent::Wheel { delta, .. } => {
                    editor.sync_scroll_bounds(win_w, win_h, prefs.line_h);
                    if editor.scroll.wheel(delta as f32 * 20.0) {
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x, y, button, down, ..
                } => {
                    if editor.scroll.handle_button(x, y, button, down) {
                        app.set_content_cursor(
                            if editor.scroll.is_hovered() || editor.scroll.is_dragging() {
                                CursorKind::Pointer
                            } else {
                                CursorKind::Default
                            },
                        );
                        app.needs_redraw = true;
                    } else if button == 1 && down && y >= TITLEBAR_HEIGHT {
                        if save_btn_rect().contains(x, y) {
                            let sfx = if editor.save() {
                                crate::sound::SystemSound::Click
                            } else {
                                crate::sound::SystemSound::Alert
                            };
                            crate::sound::play(&app.config, sfx);
                            app.needs_redraw = true;
                        } else if let Some((line, col)) =
                            hit_text_cursor(&editor, &mut app.text, prefs, win_w, win_h, x, y)
                        {
                            editor.cur_line = line;
                            editor.cur_col = col;
                            editor.preferred_col = col;
                            app.needs_redraw = true;
                        }
                    }
                }
                UiEvent::Key(key) => {
                    if key.control() {
                        if key.text().is_some_and(|c| c.eq_ignore_ascii_case(&'s')) {
                            let sfx = if editor.save() {
                                crate::sound::SystemSound::Click
                            } else {
                                crate::sound::SystemSound::Alert
                            };
                            crate::sound::play(&app.config, sfx);
                            app.needs_redraw = true;
                        } else if key.is_home() {
                            editor.cur_line = 0;
                            editor.cur_col = 0;
                            editor.preferred_col = 0;
                            editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                            app.needs_redraw = true;
                        } else if key.is_end() {
                            editor.cur_line = editor.lines.len() - 1;
                            editor.cur_col = editor.line_char_len(editor.cur_line);
                            editor.preferred_col = editor.cur_col;
                            editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                            app.needs_redraw = true;
                        }
                    } else if key.is_enter() {
                        editor.insert_newline();
                        editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                        app.needs_redraw = true;
                    } else if key.is_tab() {
                        let spaces = " ".repeat(prefs.tab_width);
                        editor.insert_str(&spaces);
                        editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                        app.needs_redraw = true;
                    } else if key.is_backspace() {
                        editor.backspace();
                        editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                        app.needs_redraw = true;
                    } else if key.is_delete() {
                        editor.delete_forward();
                        editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                        app.needs_redraw = true;
                    } else if key.is_home() {
                        editor.cur_col = 0;
                        editor.preferred_col = 0;
                        app.needs_redraw = true;
                    } else if key.is_end() {
                        editor.cur_col = editor.line_char_len(editor.cur_line);
                        editor.preferred_col = editor.cur_col;
                        app.needs_redraw = true;
                    } else if key.is_page_up() {
                        editor.move_up(vis);
                        editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                        app.needs_redraw = true;
                    } else if key.is_page_down() {
                        editor.move_down(vis);
                        editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                        app.needs_redraw = true;
                    } else if let Some(dir) = key.arrow() {
                        match dir {
                            0 => editor.move_left(),
                            1 => editor.move_up(1),
                            2 => editor.move_right(),
                            _ => editor.move_down(1),
                        }
                        editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                        app.needs_redraw = true;
                    } else if let Some(ch) = key.text().filter(|c| !c.is_control()) {
                        let mut buf = [0u8; 4];
                        editor.insert_str(ch.encode_utf8(&mut buf));
                        editor.ensure_cursor_visible(win_w, win_h, prefs.line_h);
                        app.needs_redraw = true;
                    }
                }
            }
        }

        if editor.saved != prev_saved {
            app.title = editor.window_title();
            let _ = app.x.set_title(app.win, &app.title);
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            draw_editor(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &mut editor,
                prefs,
                app.active,
            );
            app.draw_chrome();
            app.present()?;
        }
    }
    Ok(())
}

fn draw_editor(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    editor: &mut Editor,
    prefs: EditorPrefs,
    window_active: bool,
) {
    let w = canvas.width as i32;
    let h = canvas.height as i32;
    let gutter_w = prefs.gutter_w;
    let line_h = prefs.line_h;
    let font_px = prefs.font_px;
    canvas.clear(theme.window);

    // Top toolbar
    canvas.fill(Rect::new(0, TITLEBAR_HEIGHT, w, TOOLBAR_H), theme.panel);
    canvas.fill(
        Rect::new(0, TITLEBAR_HEIGHT + TOOLBAR_H - 1, w, 1),
        theme.window_border,
    );

    ui::button(
        canvas,
        text,
        theme,
        save_btn_rect(),
        "保存",
        Some(Icon::Save),
        !editor.saved,
        editor.save_hover,
    );

    let file_icon = Icon::for_file(Path::new(&editor.path), false);
    icons::draw(canvas, file_icon, 98, TITLEBAR_HEIGHT + 11, 16, None);
    let path_str = editor.path.to_string_lossy();
    let shown_path = text.truncate(&path_str, (w - 230).max(60) as f32, 12.0, Face::Ui);
    text.draw(
        canvas,
        &shown_path,
        120.0,
        (TITLEBAR_HEIGHT + 24) as f32,
        12.0,
        theme.text_dim,
        Face::Ui,
    );

    let state_label = if editor.saved {
        "已保存"
    } else {
        "未保存 · Ctrl+S"
    };
    let state_color = if editor.saved {
        theme.text_muted
    } else {
        theme.warning
    };
    text.draw_right(
        canvas,
        state_label,
        (w - 14) as f32,
        (TITLEBAR_HEIGHT + 24) as f32,
        11.5,
        state_color,
        Face::Ui,
    );

    // Editor body: optional gutter + uniform-color monospace text canvas
    let body = editor_body_rect(w, h);
    if gutter_w > 0 {
        canvas.fill(Rect::new(0, body.y, gutter_w, body.h), theme.window_alt);
        canvas.fill(
            Rect::new(gutter_w - 1, body.y, 1, body.h),
            theme.window_border,
        );
    }

    let scroll_px = editor.scroll.offset_i32().max(0);
    let first_line = ((scroll_px - TEXT_PAD_Y).max(0) / line_h) as usize;
    let vis_span = ((body.h + line_h - 1) / line_h + 2).max(2) as usize;
    let text_x = (gutter_w + TEXT_PAD_X) as f32;
    let text_clip = Rect::new(
        gutter_w,
        body.y,
        (w - gutter_w - SCROLLBAR_W - 2).max(1),
        body.h,
    );

    // Subtle active-line highlight (clipped cleanly to text viewport)
    if prefs.highlight_active {
        let ay = body.y + TEXT_PAD_Y + editor.cur_line as i32 * line_h - scroll_px;
        if ay + line_h > body.y && ay < body.bottom() {
            let hl = crate::draw::mix(theme.window, theme.panel_hover, 0.45);
            canvas.with_clip(text_clip, |c| {
                c.fill(Rect::new(gutter_w, ay, text_clip.w, line_h), hl);
            });
        }
    }

    // Gutter line numbers
    if gutter_w > 0 {
        let gutter_clip = Rect::new(0, body.y, (gutter_w - 1).max(1), body.h);
        canvas.with_clip(gutter_clip, |c| {
            for (line_idx, _) in editor
                .lines
                .iter()
                .enumerate()
                .skip(first_line)
                .take(vis_span)
            {
                let ly = body.y + TEXT_PAD_Y + line_idx as i32 * line_h - scroll_px;
                if ly >= body.bottom() {
                    break;
                }
                let num_fg = if line_idx == editor.cur_line {
                    theme.text
                } else {
                    theme.text_muted
                };
                text.draw_right(
                    c,
                    &(line_idx + 1).to_string(),
                    (gutter_w - 10) as f32,
                    ly as f32 + prefs.baseline_off,
                    11.5,
                    num_fg,
                    Face::Mono,
                );
            }
        });
    }

    // Text lines + caret
    canvas.with_clip(text_clip, |c| {
        for (line_idx, line) in editor
            .lines
            .iter()
            .enumerate()
            .skip(first_line)
            .take(vis_span)
        {
            let ly = body.y + TEXT_PAD_Y + line_idx as i32 * line_h - scroll_px;
            if ly >= body.bottom() {
                break;
            }
            let baseline = ly as f32 + prefs.baseline_off;
            if !line.is_empty() {
                text.draw(c, line, text_x, baseline, font_px, theme.text, Face::Mono);
            }
            if line_idx == editor.cur_line && window_active {
                let byte_idx = Editor::char_to_byte(line, editor.cur_col);
                let caret_x = text_x + text.measure(&line[..byte_idx], font_px, Face::Mono);
                c.fill(
                    Rect::new(caret_x.round() as i32, ly + 2, 2, line_h - 4),
                    theme.accent,
                );
            }
        }
    });

    let sb_track = scrollbar_track(w, h);
    let vp_h = (body.h - TEXT_PAD_Y * 2).max(line_h) as f32;
    let total_h = (editor.lines.len() as i32 * line_h) as f32;
    editor.scroll.draw_v(canvas, theme, sb_track, vp_h, total_h);

    // Bottom status bar
    let status_y = h - STATUS_H;
    canvas.fill(Rect::new(0, status_y, w, STATUS_H), theme.panel);
    canvas.fill(Rect::new(0, status_y, w, 1), theme.window_border);

    let pos_info = format!(
        "行 {}, 列 {}  |  共 {} 行  |  缩进 {} 空格  |  UTF-8",
        editor.cur_line + 1,
        editor.cur_col + 1,
        editor.lines.len(),
        prefs.tab_width
    );
    let pos_w = text.measure(&pos_info, 11.5, Face::Ui);
    text.draw_right(
        canvas,
        &pos_info,
        (w - 12) as f32,
        (status_y + 16) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    let max_status_w = (w as f32 - pos_w - 36.0).max(60.0);
    let shown_status = text.truncate(&editor.status, max_status_w, 11.5, Face::Ui);
    text.draw(
        canvas,
        &shown_status,
        12.0,
        (status_y + 16) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );
}
