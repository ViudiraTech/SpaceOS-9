/*
 *
 *       user/desktop/src/apps/term.rs
 *       PTY (/dev/ptmx) VT100/ANSI terminal emulator for SPUI 4.0
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Interactive pseudo-terminal emulator backed by `/dev/ptmx` and `/dev/pts/<n>`.
//! Multiplexes X11 events and non-blocking PTY output via `poll(2)` with zero
//! polling delay, supporting VT100/ANSI cursor addressing, SGR 16/256/truecolor
//! attributes, alternate screen buffer, UTF-8/CJK wide cells and scrollback.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::Icon;
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::{CursorKind, Key, Result};
use std::collections::VecDeque;
use std::ffi::CString;
use std::io;
use std::path::{Path, PathBuf};
use uxconf::CursorStyle;

const PAD_X: i32 = 10;
const PAD_Y: i32 = 8;
const SCROLLBAR_W: i32 = 8;

const TIOCSWINSZ_REQ: u64 = 0x5414;
const TIOCSCTTY_REQ: u64 = 0x540e;
const TIOCGPTN_REQ: u64 = 0x8004_5430;
const TIOCSPTLCK_REQ: u64 = 0x4004_5431;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorSpec {
    DefaultFg,
    DefaultBg,
    Ansi(u8),
    Rgb(u32),
}

impl ColorSpec {
    fn resolve(self, theme: &Theme, bold_bright: bool) -> u32 {
        match self {
            Self::DefaultFg => {
                if theme.is_dark() {
                    0xe4e4e7
                } else {
                    0x18181b
                }
            }
            Self::DefaultBg => {
                if theme.is_dark() {
                    0x141417
                } else {
                    0xffffff
                }
            }
            Self::Ansi(mut idx) => {
                if bold_bright && idx < 8 {
                    idx += 8;
                }
                ansi_256_color(idx, theme.is_dark())
            }
            Self::Rgb(rgb) => rgb & 0x00ff_ffff,
        }
    }
}

fn ansi_256_color(idx: u8, dark: bool) -> u32 {
    const DARK_16: [u32; 16] = [
        0x27272a, // 0 black
        0xef4444, // 1 red
        0x22c55e, // 2 green
        0xeab308, // 3 yellow
        0x3b82f6, // 4 blue
        0xa855f7, // 5 magenta
        0x06b6d4, // 6 cyan
        0xd4d4d8, // 7 white
        0x71717a, // 8 bright black
        0xf87171, // 9 bright red
        0x4ade80, // 10 bright green
        0xfacc15, // 11 bright yellow
        0x60a5fa, // 12 bright blue
        0xc084fc, // 13 bright magenta
        0x22d3ee, // 14 bright cyan
        0xfafafa, // 15 bright white
    ];
    const LIGHT_16: [u32; 16] = [
        0x18181b, // 0 black
        0xdc2626, // 1 red
        0x16a34a, // 2 green
        0xb45309, // 3 yellow
        0x2563eb, // 4 blue
        0x9333ea, // 5 magenta
        0x0891b2, // 6 cyan
        0x52525b, // 7 white
        0x71717a, // 8 bright black
        0xef4444, // 9 bright red
        0x22c55e, // 10 bright green
        0xd97706, // 11 bright yellow
        0x3b82f6, // 12 bright blue
        0xa855f7, // 13 bright magenta
        0x06b6d4, // 14 bright cyan
        0x09090b, // 15 bright white
    ];
    match idx {
        0..=15 => {
            if dark {
                DARK_16[idx as usize]
            } else {
                LIGHT_16[idx as usize]
            }
        }
        16..=231 => {
            let n = idx - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            let comp = |v: u8| if v == 0 { 0u32 } else { 55 + v as u32 * 40 };
            (comp(r) << 16) | (comp(g) << 8) | comp(b)
        }
        232..=255 => {
            let v = 8 + (idx - 232) as u32 * 10;
            (v << 16) | (v << 8) | v
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    ch: char,
    fg: ColorSpec,
    bg: ColorSpec,
    bold: bool,
    dim: bool,
    inverse: bool,
    wide_cont: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: ColorSpec::DefaultFg,
            bg: ColorSpec::DefaultBg,
            bold: false,
            dim: false,
            inverse: false,
            wide_cont: false,
        }
    }
}

impl Cell {
    fn blank_with_bg(bg: ColorSpec) -> Self {
        Self {
            bg,
            ..Self::default()
        }
    }
}

fn char_display_width(ch: char) -> usize {
    let u = ch as u32;
    if (0x1100..=0x115f).contains(&u)
        || (0x2e80..=0x303e).contains(&u)
        || (0x3040..=0xa4cf).contains(&u)
        || (0xac00..=0xd7a3).contains(&u)
        || (0xf900..=0xfaff).contains(&u)
        || (0xfe10..=0xfe6f).contains(&u)
        || (0xff01..=0xff60).contains(&u)
        || (0xffe0..=0xffe6).contains(&u)
        || (0x1f300..=0x1faff).contains(&u)
        || (0x20000..=0x3ffff).contains(&u)
    {
        2
    } else {
        1
    }
}

enum ParseState {
    Ground,
    Esc,
    EscCharset,
    Csi {
        private: u8,
        params: Vec<u16>,
        cur: Option<u16>,
    },
    Osc {
        buf: Vec<u8>,
        esc_seen: bool,
    },
}

struct VtTerm {
    cols: usize,
    rows: usize,
    screen: Vec<Vec<Cell>>,
    alt_saved: Option<(Vec<Vec<Cell>>, usize, usize)>,
    scrollback: VecDeque<Vec<Cell>>,
    max_scrollback: usize,
    scroll_offset: usize,
    cur_row: usize,
    cur_col: usize,
    saved_cursor: (usize, usize),
    wrap_pending: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    cursor_visible: bool,
    app_cursor_keys: bool,
    autowrap: bool,
    fg: ColorSpec,
    bg: ColorSpec,
    bold: bool,
    dim: bool,
    inverse: bool,
    state: ParseState,
    utf8_buf: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
    pending_reply: Vec<u8>,
    title_change: Option<String>,
}

impl VtTerm {
    fn new(cols: usize, rows: usize, max_scrollback: usize) -> Self {
        let cols = cols.max(4);
        let rows = rows.max(2);
        Self {
            cols,
            rows,
            screen: vec![vec![Cell::default(); cols]; rows],
            alt_saved: None,
            scrollback: VecDeque::with_capacity(256),
            max_scrollback: max_scrollback.clamp(100, 10_000),
            scroll_offset: 0,
            cur_row: 0,
            cur_col: 0,
            saved_cursor: (0, 0),
            wrap_pending: false,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            cursor_visible: true,
            app_cursor_keys: false,
            autowrap: true,
            fg: ColorSpec::DefaultFg,
            bg: ColorSpec::DefaultBg,
            bold: false,
            dim: false,
            inverse: false,
            state: ParseState::Ground,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
            pending_reply: Vec::new(),
            title_change: None,
        }
    }

    fn blank_row(&self) -> Vec<Cell> {
        vec![Cell::blank_with_bg(self.bg); self.cols]
    }

    fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let new_cols = new_cols.max(4);
        let new_rows = new_rows.max(2);
        if new_cols == self.cols && new_rows == self.rows {
            return;
        }
        for row in &mut self.screen {
            row.resize(new_cols, Cell::default());
        }
        if new_rows < self.rows {
            let excess = self.rows - new_rows;
            let push_count = excess.min(self.cur_row);
            for _ in 0..push_count {
                let top = self.screen.remove(0);
                if self.alt_saved.is_none() {
                    self.push_scrollback(top);
                }
            }
            self.cur_row = self.cur_row.saturating_sub(push_count);
            self.screen.truncate(new_rows);
        } else if new_rows > self.rows {
            let mut needed = new_rows - self.rows;
            while needed > 0 && self.alt_saved.is_none() {
                let Some(mut prev) = self.scrollback.pop_back() else {
                    break;
                };
                prev.resize(new_cols, Cell::default());
                self.screen.insert(0, prev);
                self.cur_row += 1;
                needed -= 1;
            }
            while self.screen.len() < new_rows {
                self.screen.push(vec![Cell::default(); new_cols]);
            }
        }
        self.cols = new_cols;
        self.rows = new_rows;
        self.cur_row = self.cur_row.min(self.rows - 1);
        self.cur_col = self.cur_col.min(self.cols - 1);
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.wrap_pending = false;
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    fn set_max_scrollback(&mut self, limit: usize) {
        self.max_scrollback = limit.clamp(100, 10_000);
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    fn push_scrollback(&mut self, row: Vec<Cell>) {
        if self.scrollback.len() >= self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(row);
    }

    fn scroll_up_region(&mut self, top: usize, bottom: usize, count: usize) {
        let top = top.min(self.rows - 1);
        let bottom = bottom.min(self.rows - 1).max(top);
        for _ in 0..count {
            let removed = self.screen.remove(top);
            if top == 0 && bottom + 1 == self.rows && self.alt_saved.is_none() {
                self.push_scrollback(removed);
            }
            let blank = self.blank_row();
            self.screen.insert(bottom, blank);
        }
    }

    fn scroll_down_region(&mut self, top: usize, bottom: usize, count: usize) {
        let top = top.min(self.rows - 1);
        let bottom = bottom.min(self.rows - 1).max(top);
        for _ in 0..count {
            self.screen.remove(bottom);
            let blank = self.blank_row();
            self.screen.insert(top, blank);
        }
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.cur_row == self.scroll_bottom {
            self.scroll_up_region(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cur_row + 1 < self.rows {
            self.cur_row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.wrap_pending = false;
        if self.cur_row == self.scroll_top {
            self.scroll_down_region(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cur_row > 0 {
            self.cur_row -= 1;
        }
    }

    fn put_char(&mut self, ch: char) {
        let width = char_display_width(ch).min(self.cols);
        if self.wrap_pending {
            self.cur_col = 0;
            self.linefeed();
        }
        if width == 2 && self.cur_col + 1 >= self.cols {
            if self.autowrap {
                self.screen[self.cur_row][self.cur_col] = Cell::blank_with_bg(self.bg);
                self.cur_col = 0;
                self.linefeed();
            } else {
                self.cur_col = self.cols.saturating_sub(2);
            }
        }

        // Clear broken wide-character halves when overwriting.
        let r = self.cur_row;
        let c = self.cur_col;
        if self.screen[r][c].wide_cont && c > 0 {
            self.screen[r][c - 1] = Cell::blank_with_bg(self.bg);
        }
        if c + 1 < self.cols && self.screen[r][c + 1].wide_cont {
            self.screen[r][c + 1] = Cell::blank_with_bg(self.bg);
        }

        self.screen[r][c] = Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
            bold: self.bold,
            dim: self.dim,
            inverse: self.inverse,
            wide_cont: false,
        };
        if width == 2 && c + 1 < self.cols {
            if c + 2 < self.cols && self.screen[r][c + 2].wide_cont {
                self.screen[r][c + 2] = Cell::blank_with_bg(self.bg);
            }
            self.screen[r][c + 1] = Cell {
                ch: ' ',
                fg: self.fg,
                bg: self.bg,
                bold: self.bold,
                dim: self.dim,
                inverse: self.inverse,
                wide_cont: true,
            };
        }

        if c + width >= self.cols {
            self.cur_col = self.cols - 1;
            self.wrap_pending = self.autowrap;
        } else {
            self.cur_col = c + width;
            self.wrap_pending = false;
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.utf8_need > 0 {
                if (b & 0xc0) == 0x80 {
                    self.utf8_buf[self.utf8_len] = b;
                    self.utf8_len += 1;
                    if self.utf8_len == self.utf8_need {
                        let ch = std::str::from_utf8(&self.utf8_buf[..self.utf8_len])
                            .ok()
                            .and_then(|s| s.chars().next())
                            .unwrap_or('\u{fffd}');
                        self.utf8_len = 0;
                        self.utf8_need = 0;
                        self.put_char(ch);
                    }
                    continue;
                } else {
                    self.utf8_len = 0;
                    self.utf8_need = 0;
                }
            }
            self.step_byte(b);
        }
    }

    fn step_byte(&mut self, b: u8) {
        // Handle C0 controls even inside escape/CSI sequences (except OSC payload).
        if !matches!(self.state, ParseState::Osc { .. }) {
            match b {
                0x1b => {
                    self.state = ParseState::Esc;
                    return;
                }
                b'\r' => {
                    self.cur_col = 0;
                    self.wrap_pending = false;
                    return;
                }
                b'\n' | 0x0b | 0x0c => {
                    self.linefeed();
                    return;
                }
                0x08 => {
                    self.wrap_pending = false;
                    self.cur_col = self.cur_col.saturating_sub(1);
                    return;
                }
                b'\t' => {
                    self.wrap_pending = false;
                    let next = ((self.cur_col / 8) + 1) * 8;
                    self.cur_col = next.min(self.cols - 1);
                    return;
                }
                0x00..=0x07 | 0x0e..=0x1a | 0x1c..=0x1f => return,
                _ => {}
            }
        }

        let prev = std::mem::replace(&mut self.state, ParseState::Ground);
        match prev {
            ParseState::Ground => {
                if b < 0x80 {
                    if b >= 0x20 && b != 0x7f {
                        self.put_char(b as char);
                    }
                } else if (b & 0xe0) == 0xc0 {
                    self.utf8_buf[0] = b;
                    self.utf8_len = 1;
                    self.utf8_need = 2;
                } else if (b & 0xf0) == 0xe0 {
                    self.utf8_buf[0] = b;
                    self.utf8_len = 1;
                    self.utf8_need = 3;
                } else if (b & 0xf8) == 0xf0 {
                    self.utf8_buf[0] = b;
                    self.utf8_len = 1;
                    self.utf8_need = 4;
                }
            }
            ParseState::Esc => match b {
                b'[' => {
                    self.state = ParseState::Csi {
                        private: 0,
                        params: Vec::with_capacity(4),
                        cur: None,
                    };
                }
                b']' => {
                    self.state = ParseState::Osc {
                        buf: Vec::with_capacity(32),
                        esc_seen: false,
                    };
                }
                b'(' | b')' | b'*' | b'+' | b'#' => {
                    self.state = ParseState::EscCharset;
                }
                b'7' => {
                    self.saved_cursor = (self.cur_row, self.cur_col);
                }
                b'8' => {
                    self.cur_row = self.saved_cursor.0.min(self.rows - 1);
                    self.cur_col = self.saved_cursor.1.min(self.cols - 1);
                    self.wrap_pending = false;
                }
                b'D' => self.linefeed(),
                b'E' => {
                    self.cur_col = 0;
                    self.linefeed();
                }
                b'M' => self.reverse_index(),
                b'c' => self.full_reset(),
                _ => {}
            },
            ParseState::EscCharset => {}
            ParseState::Csi {
                mut private,
                mut params,
                mut cur,
            } => match b {
                b'?' | b'>' | b'!' if params.is_empty() && cur.is_none() => {
                    private = b;
                    self.state = ParseState::Csi {
                        private,
                        params,
                        cur,
                    };
                }
                b'0'..=b'9' => {
                    let v = cur
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add((b - b'0') as u16);
                    self.state = ParseState::Csi {
                        private,
                        params,
                        cur: Some(v),
                    };
                }
                b';' | b':' => {
                    params.push(cur.unwrap_or(0));
                    cur = None;
                    self.state = ParseState::Csi {
                        private,
                        params,
                        cur,
                    };
                }
                0x20..=0x2f => {
                    self.state = ParseState::Csi {
                        private,
                        params,
                        cur,
                    };
                }
                0x40..=0x7e => {
                    if let Some(v) = cur {
                        params.push(v);
                    }
                    self.dispatch_csi(private, &params, b);
                }
                _ => {}
            },
            ParseState::Osc { mut buf, esc_seen } => {
                if b == 0x07 || (esc_seen && b == b'\\') {
                    if esc_seen && buf.last() == Some(&0x1b) {
                        buf.pop();
                    }
                    self.dispatch_osc(&buf);
                } else if b == 0x1b {
                    buf.push(b);
                    self.state = ParseState::Osc {
                        buf,
                        esc_seen: true,
                    };
                } else {
                    if buf.len() < 512 {
                        buf.push(b);
                    }
                    self.state = ParseState::Osc {
                        buf,
                        esc_seen: false,
                    };
                }
            }
        }
    }

    fn full_reset(&mut self) {
        self.screen = vec![vec![Cell::default(); self.cols]; self.rows];
        self.alt_saved = None;
        self.cur_row = 0;
        self.cur_col = 0;
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.cursor_visible = true;
        self.app_cursor_keys = false;
        self.autowrap = true;
        self.reset_sgr();
    }

    fn reset_sgr(&mut self) {
        self.fg = ColorSpec::DefaultFg;
        self.bg = ColorSpec::DefaultBg;
        self.bold = false;
        self.dim = false;
        self.inverse = false;
    }

    fn dispatch_osc(&mut self, raw: &[u8]) {
        let s = String::from_utf8_lossy(raw);
        if let Some((code, rest)) = s.split_once(';') {
            if matches!(code, "0" | "2") {
                let trimmed = rest.trim();
                if !trimmed.is_empty() {
                    self.title_change = Some(trimmed.to_string());
                }
            }
        }
    }

    fn dispatch_csi(&mut self, private: u8, params: &[u16], final_byte: u8) {
        let p0 = params.first().copied().unwrap_or(0) as usize;
        let n1 = p0.max(1);

        if private == b'?' {
            let set = final_byte == b'h';
            if !set && final_byte != b'l' {
                return;
            }
            for &mode in params {
                match mode {
                    1 => self.app_cursor_keys = set,
                    7 => self.autowrap = set,
                    25 => self.cursor_visible = set,
                    47 | 1047 | 1049 => {
                        if set && self.alt_saved.is_none() {
                            let saved = std::mem::replace(
                                &mut self.screen,
                                vec![vec![Cell::default(); self.cols]; self.rows],
                            );
                            self.alt_saved = Some((saved, self.cur_row, self.cur_col));
                            self.cur_row = 0;
                            self.cur_col = 0;
                            self.wrap_pending = false;
                        } else if !set {
                            if let Some((mut saved, sr, sc)) = self.alt_saved.take() {
                                saved.resize(self.rows, vec![Cell::default(); self.cols]);
                                for row in &mut saved {
                                    row.resize(self.cols, Cell::default());
                                }
                                self.screen = saved;
                                self.cur_row = sr.min(self.rows - 1);
                                self.cur_col = sc.min(self.cols - 1);
                                self.wrap_pending = false;
                            }
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        match final_byte {
            b'A' => {
                self.wrap_pending = false;
                self.cur_row = self.cur_row.saturating_sub(n1);
            }
            b'B' => {
                self.wrap_pending = false;
                self.cur_row = (self.cur_row + n1).min(self.rows - 1);
            }
            b'C' => {
                self.wrap_pending = false;
                self.cur_col = (self.cur_col + n1).min(self.cols - 1);
            }
            b'D' => {
                self.wrap_pending = false;
                self.cur_col = self.cur_col.saturating_sub(n1);
            }
            b'E' => {
                self.wrap_pending = false;
                self.cur_col = 0;
                self.cur_row = (self.cur_row + n1).min(self.rows - 1);
            }
            b'F' => {
                self.wrap_pending = false;
                self.cur_col = 0;
                self.cur_row = self.cur_row.saturating_sub(n1);
            }
            b'G' | b'`' => {
                self.wrap_pending = false;
                self.cur_col = n1.saturating_sub(1).min(self.cols - 1);
            }
            b'd' => {
                self.wrap_pending = false;
                self.cur_row = n1.saturating_sub(1).min(self.rows - 1);
            }
            b'H' | b'f' => {
                let row = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                let col = params.get(1).copied().unwrap_or(1).max(1) as usize - 1;
                self.cur_row = row.min(self.rows - 1);
                self.cur_col = col.min(self.cols - 1);
                self.wrap_pending = false;
            }
            b'J' => {
                let blank = Cell::blank_with_bg(self.bg);
                match p0 {
                    0 => {
                        for c in self.cur_col..self.cols {
                            self.screen[self.cur_row][c] = blank;
                        }
                        for r in (self.cur_row + 1)..self.rows {
                            self.screen[r].fill(blank);
                        }
                    }
                    1 => {
                        for r in 0..self.cur_row {
                            self.screen[r].fill(blank);
                        }
                        for c in 0..=self.cur_col.min(self.cols - 1) {
                            self.screen[self.cur_row][c] = blank;
                        }
                    }
                    2 => {
                        for r in 0..self.rows {
                            self.screen[r].fill(blank);
                        }
                    }
                    3 => {
                        self.scrollback.clear();
                        self.scroll_offset = 0;
                        for r in 0..self.rows {
                            self.screen[r].fill(blank);
                        }
                    }
                    _ => {}
                }
            }
            b'K' => {
                let blank = Cell::blank_with_bg(self.bg);
                let row = &mut self.screen[self.cur_row];
                match p0 {
                    0 => row[self.cur_col..].fill(blank),
                    1 => row[..=self.cur_col.min(self.cols - 1)].fill(blank),
                    2 => row.fill(blank),
                    _ => {}
                }
            }
            b'L' => {
                if self.cur_row >= self.scroll_top && self.cur_row <= self.scroll_bottom {
                    let count = n1.min(self.scroll_bottom - self.cur_row + 1);
                    self.scroll_down_region(self.cur_row, self.scroll_bottom, count);
                }
            }
            b'M' => {
                if self.cur_row >= self.scroll_top && self.cur_row <= self.scroll_bottom {
                    let count = n1.min(self.scroll_bottom - self.cur_row + 1);
                    self.scroll_up_region(self.cur_row, self.scroll_bottom, count);
                }
            }
            b'P' => {
                let blank = Cell::blank_with_bg(self.bg);
                let row = &mut self.screen[self.cur_row];
                let count = n1.min(self.cols - self.cur_col);
                for _ in 0..count {
                    row.remove(self.cur_col);
                    row.push(blank);
                }
            }
            b'@' => {
                let blank = Cell::blank_with_bg(self.bg);
                let row = &mut self.screen[self.cur_row];
                let count = n1.min(self.cols - self.cur_col);
                for _ in 0..count {
                    row.insert(self.cur_col, blank);
                    row.pop();
                }
            }
            b'X' => {
                let blank = Cell::blank_with_bg(self.bg);
                let end = (self.cur_col + n1).min(self.cols);
                self.screen[self.cur_row][self.cur_col..end].fill(blank);
            }
            b'S' => self.scroll_up_region(self.scroll_top, self.scroll_bottom, n1),
            b'T' => self.scroll_down_region(self.scroll_top, self.scroll_bottom, n1),
            b'r' => {
                let top = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                let bot = params
                    .get(1)
                    .copied()
                    .filter(|&v| v > 0)
                    .map_or(self.rows - 1, |v| (v as usize - 1).min(self.rows - 1));
                if top < bot {
                    self.scroll_top = top;
                    self.scroll_bottom = bot;
                    self.cur_row = 0;
                    self.cur_col = 0;
                    self.wrap_pending = false;
                }
            }
            b's' => {
                self.saved_cursor = (self.cur_row, self.cur_col);
            }
            b'u' => {
                self.cur_row = self.saved_cursor.0.min(self.rows - 1);
                self.cur_col = self.saved_cursor.1.min(self.cols - 1);
                self.wrap_pending = false;
            }
            b'n' => {
                if p0 == 6 {
                    let reply = format!("\x1b[{};{}R", self.cur_row + 1, self.cur_col + 1);
                    self.pending_reply.extend_from_slice(reply.as_bytes());
                } else if p0 == 5 {
                    self.pending_reply.extend_from_slice(b"\x1b[0n");
                }
            }
            b'c' if private == 0 => {
                self.pending_reply.extend_from_slice(b"\x1b[?6c");
            }
            b'm' => self.apply_sgr(params),
            _ => {}
        }
    }

    fn apply_sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.reset_sgr();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => self.reset_sgr(),
                1 => {
                    self.bold = true;
                    self.dim = false;
                }
                2 => {
                    self.dim = true;
                    self.bold = false;
                }
                7 => self.inverse = true,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                27 => self.inverse = false,
                30..=37 => self.fg = ColorSpec::Ansi((params[i] - 30) as u8),
                39 => self.fg = ColorSpec::DefaultFg,
                40..=47 => self.bg = ColorSpec::Ansi((params[i] - 40) as u8),
                49 => self.bg = ColorSpec::DefaultBg,
                90..=97 => self.fg = ColorSpec::Ansi((params[i] - 90 + 8) as u8),
                100..=107 => self.bg = ColorSpec::Ansi((params[i] - 100 + 8) as u8),
                38 | 48 => {
                    let is_fg = params[i] == 38;
                    if let Some(&mode) = params.get(i + 1) {
                        if mode == 5 {
                            if let Some(&idx) = params.get(i + 2) {
                                let spec = ColorSpec::Ansi((idx & 0xff) as u8);
                                if is_fg {
                                    self.fg = spec;
                                } else {
                                    self.bg = spec;
                                }
                            }
                            i += 3;
                            continue;
                        } else if mode == 2 {
                            let r = params.get(i + 2).copied().unwrap_or(0).min(255) as u32;
                            let g = params.get(i + 3).copied().unwrap_or(0).min(255) as u32;
                            let b = params.get(i + 4).copied().unwrap_or(0).min(255) as u32;
                            let spec = ColorSpec::Rgb((r << 16) | (g << 8) | b);
                            if is_fg {
                                self.fg = spec;
                            } else {
                                self.bg = spec;
                            }
                            i += 5;
                            continue;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn view_row(&self, vis_row: usize) -> &[Cell] {
        let total_sb = self.scrollback.len();
        let top_global = total_sb.saturating_sub(self.scroll_offset);
        let idx = top_global + vis_row;
        if idx < total_sb {
            &self.scrollback[idx]
        } else {
            &self.screen[(idx - total_sb).min(self.rows - 1)]
        }
    }
}

struct Pty {
    master_fd: i32,
    child_pid: i32,
}

impl Pty {
    fn spawn(
        workdir: &Path,
        rows: u16,
        cols: u16,
        xpixel: u16,
        ypixel: u16,
    ) -> std::result::Result<Self, String> {
        let ptmx = CString::new("/dev/ptmx").unwrap();
        let master_fd = unsafe {
            libc::open(
                ptmx.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        if master_fd < 0 {
            return Err(format!("open(/dev/ptmx): {}", io::Error::last_os_error()));
        }

        let unlock: libc::c_int = 0;
        if unsafe { libc::ioctl(master_fd, TIOCSPTLCK_REQ as _, &unlock) } < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(master_fd) };
            return Err(format!("TIOCSPTLCK: {err}"));
        }

        let mut ptn: u32 = 0;
        if unsafe { libc::ioctl(master_fd, TIOCGPTN_REQ as _, &mut ptn) } < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(master_fd) };
            return Err(format!("TIOCGPTN: {err}"));
        }

        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: xpixel,
            ws_ypixel: ypixel,
        };
        unsafe {
            libc::ioctl(master_fd, TIOCSWINSZ_REQ as _, &ws);
        }

        let slave_c =
            CString::new(format!("/dev/pts/{ptn}")).map_err(|e| format!("pts path: {e}"))?;
        let dir_str = if workdir.is_dir() {
            workdir.to_string_lossy().into_owned()
        } else {
            "/root".to_string()
        };
        let workdir_c = CString::new(dir_str).unwrap_or_else(|_| CString::new("/root").unwrap());
        let sh_path = CString::new("/bin/sh").unwrap();
        let sh_arg0 = CString::new("-sh").unwrap();

        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
        let env_strings = [
            CString::new("PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin").unwrap(),
            CString::new("HOME=/root").unwrap(),
            CString::new("USER=root").unwrap(),
            CString::new("LOGNAME=root").unwrap(),
            CString::new("SHELL=/bin/sh").unwrap(),
            CString::new("TERM=xterm-256color").unwrap(),
            CString::new("COLORTERM=truecolor").unwrap(),
            CString::new("LANG=zh_CN.UTF-8").unwrap(),
            CString::new(format!("DISPLAY={display}")).unwrap(),
        ];
        let mut env_ptrs: Vec<*const libc::c_char> =
            env_strings.iter().map(|s| s.as_ptr()).collect();
        env_ptrs.push(std::ptr::null());
        let argv_ptrs: [*const libc::c_char; 2] = [sh_arg0.as_ptr(), std::ptr::null()];

        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(master_fd) };
            return Err(format!("fork: {err}"));
        }

        if pid == 0 {
            unsafe {
                libc::setsid();
                let slave_fd = libc::open(slave_c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
                if slave_fd < 0 {
                    libc::_exit(127);
                }
                if libc::ioctl(slave_fd, TIOCSCTTY_REQ as _, 0 as libc::c_int) < 0 {
                    libc::ioctl(slave_fd, TIOCSCTTY_REQ as _, 1 as libc::c_int);
                }
                libc::dup2(slave_fd, 0);
                libc::dup2(slave_fd, 1);
                libc::dup2(slave_fd, 2);
                if slave_fd > 2 {
                    libc::close(slave_fd);
                }
                libc::close(master_fd);
                for fd in 3..64 {
                    libc::close(fd);
                }
                for sig in [
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTSTP,
                    libc::SIGTTIN,
                    libc::SIGTTOU,
                    libc::SIGCHLD,
                    libc::SIGPIPE,
                ] {
                    libc::signal(sig, libc::SIG_DFL);
                }
                libc::chdir(workdir_c.as_ptr());
                libc::execve(sh_path.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr());
                libc::_exit(127);
            }
        }

        unsafe {
            let flags = libc::fcntl(master_fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        Ok(Self {
            master_fd,
            child_pid: pid,
        })
    }

    fn resize(&self, rows: u16, cols: u16, xpixel: u16, ypixel: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: xpixel,
            ws_ypixel: ypixel,
        };
        unsafe {
            libc::ioctl(self.master_fd, TIOCSWINSZ_REQ as _, &ws);
        }
    }

    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let rc = unsafe {
            libc::read(
                self.master_fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(rc as usize)
        }
    }

    fn write_all(&self, mut data: &[u8]) {
        while !data.is_empty() {
            let rc = unsafe {
                libc::write(
                    self.master_fd,
                    data.as_ptr() as *const libc::c_void,
                    data.len(),
                )
            };
            if rc > 0 {
                data = &data[rc as usize..];
            } else {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }

    fn child_exited(&mut self) -> bool {
        if self.child_pid <= 0 {
            return true;
        }
        let mut status: libc::c_int = 0;
        let rc = unsafe { libc::waitpid(self.child_pid, &mut status, libc::WNOHANG) };
        if rc == self.child_pid {
            self.child_pid = -1;
            true
        } else {
            false
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if self.master_fd >= 0 {
            unsafe {
                libc::close(self.master_fd);
            }
            self.master_fd = -1;
        }
        if self.child_pid > 0 {
            unsafe {
                libc::kill(self.child_pid, libc::SIGHUP);
                let mut status: libc::c_int = 0;
                libc::waitpid(self.child_pid, &mut status, libc::WNOHANG);
            }
            self.child_pid = -1;
        }
    }
}

fn key_to_pty_bytes(key: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    if key.is_modifier() {
        return None;
    }
    if let Some(ctrl) = key.ctrl_byte() {
        return Some(vec![ctrl]);
    }
    if key.is_escape() {
        return Some(vec![0x1b]);
    }
    if key.is_enter() {
        return Some(vec![b'\r']);
    }
    if key.is_backspace() {
        return Some(vec![0x7f]);
    }
    if key.is_tab() {
        return Some(if key.shift() {
            b"\x1b[Z".to_vec()
        } else {
            vec![b'\t']
        });
    }
    if key.is_delete() {
        return Some(b"\x1b[3~".to_vec());
    }
    if key.is_home() {
        return Some(if app_cursor {
            b"\x1bOH".to_vec()
        } else {
            b"\x1b[H".to_vec()
        });
    }
    if key.is_end() {
        return Some(if app_cursor {
            b"\x1bOF".to_vec()
        } else {
            b"\x1b[F".to_vec()
        });
    }
    if key.is_page_up() {
        return Some(b"\x1b[5~".to_vec());
    }
    if key.is_page_down() {
        return Some(b"\x1b[6~".to_vec());
    }
    if let Some(dir) = key.arrow() {
        let seq: &[u8] = match (dir, app_cursor) {
            (0, false) => b"\x1b[D",
            (0, true) => b"\x1bOD",
            (1, false) => b"\x1b[A",
            (1, true) => b"\x1bOA",
            (2, false) => b"\x1b[C",
            (2, true) => b"\x1bOC",
            (_, false) => b"\x1b[B",
            (_, true) => b"\x1bOB",
        };
        return Some(seq.to_vec());
    }
    if let Some(ch) = key.text() {
        let mut out = Vec::with_capacity(5);
        if key.alt() {
            out.push(0x1b);
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        return Some(out);
    }
    None
}

#[derive(Clone, Copy)]
struct TermMetrics {
    font_px: f32,
    cell_w: i32,
    cell_h: i32,
    baseline_off: f32,
}

fn compute_metrics(text: &mut TextRenderer, font_size: i32) -> TermMetrics {
    let font_px = (font_size.clamp(11, 20) as f32) - 0.5;
    let cell_h = ((font_px * 1.48).round() as i32).max(16);
    let baseline_off = (cell_h as f32 * 0.75).round();
    let cell_w = text.mono_advance(font_px).round().max(7.0) as i32;
    TermMetrics {
        font_px,
        cell_w,
        cell_h,
        baseline_off,
    }
}

fn calc_grid(content: Rect, cell_w: i32, cell_h: i32) -> (usize, usize) {
    let usable_w = (content.w - PAD_X * 2 - SCROLLBAR_W).max(cell_w * 10);
    let usable_h = (content.h - PAD_Y * 2).max(cell_h * 3);
    let cols = (usable_w / cell_w).max(10) as usize;
    let rows = (usable_h / cell_h).max(3) as usize;
    (cols, rows)
}

pub fn run(argument: Option<&str>) -> Result<()> {
    let mut app = App::new("终端", Icon::Terminal, 820, 520)?;
    app.min_width = 420;
    app.min_height = 240;
    app.set_content_cursor(CursorKind::Text);

    let mut metrics = compute_metrics(&mut app.text, app.config.term_font_size);
    let (init_cols, init_rows) = calc_grid(app.content(), metrics.cell_w, metrics.cell_h);
    let mut vt = VtTerm::new(init_cols, init_rows, app.config.term_scrollback_lines);

    let workdir = argument
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/root"));

    let mut pty = match Pty::spawn(
        &workdir,
        init_rows as u16,
        init_cols as u16,
        (init_cols as i32 * metrics.cell_w) as u16,
        (init_rows as i32 * metrics.cell_h) as u16,
    ) {
        Ok(pty) => Some(pty),
        Err(err) => {
            vt.feed(format!("\x1b[1;31m无法打开伪终端 (/dev/ptmx): {err}\x1b[0m\r\n").as_bytes());
            None
        }
    };

    let mut read_buf = [0u8; 8192];
    loop {
        if app.close {
            break;
        }

        let extra_fd = pty.as_ref().map(|p| p.master_fd);
        let (events, pty_ready) = app.pump_with_fd(extra_fd, 60)?;

        if pty_ready || pty.is_some() {
            let mut eof = false;
            if let Some(p) = &pty {
                loop {
                    match p.read(&mut read_buf) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(n) => {
                            vt.feed(&read_buf[..n]);
                            if !vt.pending_reply.is_empty() {
                                p.write_all(&vt.pending_reply);
                                vt.pending_reply.clear();
                            }
                            app.needs_redraw = true;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            eof = true;
                            break;
                        }
                    }
                }
            }
            if let Some(new_title) = vt.title_change.take() {
                app.title = new_title;
                let _ = app.x.set_title(app.win, &app.title);
                app.needs_redraw = true;
            }
            if eof || pty.as_mut().is_some_and(Pty::child_exited) {
                app.request_close();
                break;
            }
        }

        for event in events {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose => app.needs_redraw = true,
                UiEvent::ConfigChanged => {
                    metrics = compute_metrics(&mut app.text, app.config.term_font_size);
                    vt.set_max_scrollback(app.config.term_scrollback_lines);
                    let (cols, rows) = calc_grid(app.content(), metrics.cell_w, metrics.cell_h);
                    if cols != vt.cols || rows != vt.rows {
                        vt.resize(cols, rows);
                        if let Some(p) = &pty {
                            p.resize(
                                rows as u16,
                                cols as u16,
                                (cols as i32 * metrics.cell_w) as u16,
                                (rows as i32 * metrics.cell_h) as u16,
                            );
                        }
                    }
                    app.needs_redraw = true;
                }
                UiEvent::Configure { .. } => {
                    let (cols, rows) = calc_grid(app.content(), metrics.cell_w, metrics.cell_h);
                    if cols != vt.cols || rows != vt.rows {
                        vt.resize(cols, rows);
                        if let Some(p) = &pty {
                            p.resize(
                                rows as u16,
                                cols as u16,
                                (cols as i32 * metrics.cell_w) as u16,
                                (rows as i32 * metrics.cell_h) as u16,
                            );
                        }
                    }
                    app.needs_redraw = true;
                }
                UiEvent::Wheel { delta, .. } => {
                    let max_off = vt.scrollback.len();
                    let next = if delta < 0 {
                        (vt.scroll_offset + (-delta as usize)).min(max_off)
                    } else {
                        vt.scroll_offset.saturating_sub(delta as usize)
                    };
                    if next != vt.scroll_offset {
                        vt.scroll_offset = next;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Key(key) => {
                    if key.shift() && key.is_page_up() {
                        vt.scroll_offset = (vt.scroll_offset + vt.rows.saturating_sub(2).max(1))
                            .min(vt.scrollback.len());
                        app.needs_redraw = true;
                    } else if key.shift() && key.is_page_down() {
                        vt.scroll_offset = vt
                            .scroll_offset
                            .saturating_sub(vt.rows.saturating_sub(2).max(1));
                        app.needs_redraw = true;
                    } else if let Some(bytes) = key_to_pty_bytes(&key, vt.app_cursor_keys) {
                        if vt.scroll_offset != 0 {
                            vt.scroll_offset = 0;
                            app.needs_redraw = true;
                        }
                        if let Some(p) = &pty {
                            p.write_all(&bytes);
                        }
                    }
                }
                _ => {}
            }
        }

        if app.needs_redraw {
            draw_terminal(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &vt,
                metrics,
                app.config.term_cursor_style,
                app.active,
            );
            app.draw_chrome();
            app.present()?;
        }
    }
    Ok(())
}

fn draw_terminal(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    vt: &VtTerm,
    metrics: TermMetrics,
    cursor_style: CursorStyle,
    window_active: bool,
) {
    let cell_w = metrics.cell_w;
    let cell_h = metrics.cell_h;
    let font_px = metrics.font_px;
    let default_bg = ColorSpec::DefaultBg.resolve(theme, false);
    let body = Rect::new(
        0,
        TITLEBAR_HEIGHT,
        canvas.width as i32,
        (canvas.height as i32 - TITLEBAR_HEIGHT).max(0),
    );
    canvas.fill(body, default_bg);

    let origin_x = PAD_X;
    let origin_y = TITLEBAR_HEIGHT + PAD_Y;

    for row_idx in 0..vt.rows {
        let line = vt.view_row(row_idx);
        let row_y = origin_y + row_idx as i32 * cell_h;
        if row_y >= canvas.height as i32 - 1 {
            break;
        }

        // Coalesce contiguous non-default background runs per row.
        let mut col = 0;
        while col < vt.cols && col < line.len() {
            let cell = line[col];
            let cell_bg = if cell.inverse {
                cell.fg.resolve(theme, cell.bold)
            } else {
                cell.bg.resolve(theme, false)
            };
            if cell_bg != default_bg {
                let start = col;
                col += 1;
                while col < vt.cols && col < line.len() {
                    let next = line[col];
                    let next_bg = if next.inverse {
                        next.fg.resolve(theme, next.bold)
                    } else {
                        next.bg.resolve(theme, false)
                    };
                    if next_bg != cell_bg {
                        break;
                    }
                    col += 1;
                }
                canvas.fill(
                    Rect::new(
                        origin_x + start as i32 * cell_w,
                        row_y,
                        (col - start) as i32 * cell_w,
                        cell_h,
                    ),
                    cell_bg,
                );
            } else {
                col += 1;
            }
        }

        // Draw foreground glyphs.
        let baseline = row_y as f32 + metrics.baseline_off;
        let mut buf = [0u8; 4];
        for (c_idx, cell) in line.iter().take(vt.cols).enumerate() {
            if cell.wide_cont || cell.ch == ' ' {
                continue;
            }
            let mut fg = if cell.inverse {
                cell.bg.resolve(theme, false)
            } else {
                cell.fg.resolve(theme, cell.bold)
            };
            if cell.dim {
                fg = crate::draw::mix(fg, default_bg, 0.38);
            }
            let face = if cell.bold { Face::Bold } else { Face::Mono };
            let s = cell.ch.encode_utf8(&mut buf);
            text.draw(
                canvas,
                s,
                (origin_x + c_idx as i32 * cell_w) as f32,
                baseline,
                font_px,
                fg,
                face,
            );
        }
    }

    // Draw cursor when viewing live bottom screen.
    if vt.scroll_offset == 0 && vt.cursor_visible && vt.cur_row < vt.rows && vt.cur_col < vt.cols {
        let cur_cell = vt.screen[vt.cur_row][vt.cur_col];
        let cur_w = if !cur_cell.wide_cont && char_display_width(cur_cell.ch) == 2 {
            cell_w * 2
        } else {
            cell_w
        };
        let cx = origin_x + vt.cur_col as i32 * cell_w;
        let cy = origin_y + vt.cur_row as i32 * cell_h;
        let cursor_color = theme.accent;
        match cursor_style {
            CursorStyle::Beam => {
                let thickness = if window_active { 2 } else { 1 };
                canvas.fill(Rect::new(cx, cy + 2, thickness, cell_h - 4), cursor_color);
            }
            CursorStyle::Underline => {
                let thickness = if window_active { 2 } else { 1 };
                canvas.fill(
                    Rect::new(cx, cy + cell_h - 3, cur_w, thickness),
                    cursor_color,
                );
            }
            CursorStyle::Block => {
                let cursor_rect = Rect::new(cx, cy + 1, cur_w, cell_h - 2);
                if window_active {
                    canvas.fill(cursor_rect, cursor_color);
                    if cur_cell.ch != ' ' && !cur_cell.wide_cont {
                        let mut buf = [0u8; 4];
                        let s = cur_cell.ch.encode_utf8(&mut buf);
                        text.draw(
                            canvas,
                            s,
                            cx as f32,
                            cy as f32 + metrics.baseline_off,
                            font_px,
                            0xffffff,
                            Face::Mono,
                        );
                    }
                } else {
                    canvas.border_1px(cursor_rect, cursor_color);
                }
            }
        }
    }

    if !vt.scrollback.is_empty() {
        let total = vt.scrollback.len() + vt.rows;
        let top_offset = vt.scrollback.len().saturating_sub(vt.scroll_offset);
        let track = Rect::new(
            canvas.width as i32 - SCROLLBAR_W - 2,
            TITLEBAR_HEIGHT + 2,
            SCROLLBAR_W,
            (canvas.height as i32 - TITLEBAR_HEIGHT - 4).max(8),
        );
        ui::scrollbar_v(canvas, theme, track, total, vt.rows, top_offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vt_ansi_colors_cursor_and_split_utf8() {
        let mut vt = VtTerm::new(40, 10, 1000);
        // Feed split UTF-8 ("中文") and SGR colors + cursor motion.
        let payload = "\x1b[1;32mroot@spaceos\x1b[0m:~$ echo 中文\r\nOK";
        for chunk in payload.as_bytes().chunks(3) {
            vt.feed(chunk);
        }
        assert_eq!(vt.screen[0][0].ch, 'r');
        assert!(vt.screen[0][0].bold);
        assert_eq!(vt.screen[0][0].fg, ColorSpec::Ansi(2));
        // Wide CJK cells occupy 2 columns with wide_cont on the trailing half.
        let zhong_col = vt.screen[0].iter().position(|c| c.ch == '中').unwrap();
        assert!(vt.screen[0][zhong_col + 1].wide_cont);
        assert_eq!(vt.screen[0][zhong_col + 2].ch, '文');
        assert_eq!(vt.screen[1][0].ch, 'O');
        assert_eq!(vt.screen[1][1].ch, 'K');
    }

    #[test]
    fn escape_and_shift_translate_cleanly_to_pty() {
        let esc = Key {
            code: 9,
            state: 0,
            sym: 0xff1b,
        };
        assert_eq!(key_to_pty_bytes(&esc, false), Some(vec![0x1b]));

        let shift_l = Key {
            code: 50,
            state: 0,
            sym: 0xffe1,
        };
        assert_eq!(key_to_pty_bytes(&shift_l, false), None);

        let ctrl_c = Key {
            code: 54,
            state: 4,
            sym: b'c' as u32,
        };
        assert_eq!(key_to_pty_bytes(&ctrl_c, false), Some(vec![0x03]));
    }
}
