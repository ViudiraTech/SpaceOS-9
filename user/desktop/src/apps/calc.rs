/*
 *
 *       user/desktop/src/apps/calc.rs
 *       SPUI 4.0 Calculator: responsive keypad, expression display and keyboard input
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Clean, responsive desktop calculator modeled after modern macOS & GNOME
//! Calculators. Features an adaptive readout display, expression history,
//! tactile button animations and full physical keyboard input.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::Icon;
use crate::text::{Face, TextRenderer};
use crate::theme::Theme;
use crate::ui;
use crate::x11::{Key, Result};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalcOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl CalcOp {
    const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "−",
            Self::Mul => "×",
            Self::Div => "÷",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BtnKind {
    Digit(u8),
    Dot,
    Op(CalcOp),
    Equal,
    Clear,
    Backspace,
    Negate,
    Percent,
}

const BUTTON_GRID: [[BtnKind; 4]; 5] = [
    [
        BtnKind::Clear,
        BtnKind::Backspace,
        BtnKind::Percent,
        BtnKind::Op(CalcOp::Div),
    ],
    [
        BtnKind::Digit(7),
        BtnKind::Digit(8),
        BtnKind::Digit(9),
        BtnKind::Op(CalcOp::Mul),
    ],
    [
        BtnKind::Digit(4),
        BtnKind::Digit(5),
        BtnKind::Digit(6),
        BtnKind::Op(CalcOp::Sub),
    ],
    [
        BtnKind::Digit(1),
        BtnKind::Digit(2),
        BtnKind::Digit(3),
        BtnKind::Op(CalcOp::Add),
    ],
    [
        BtnKind::Negate,
        BtnKind::Digit(0),
        BtnKind::Dot,
        BtnKind::Equal,
    ],
];

struct CalculatorState {
    current: String,
    accumulator: Option<f64>,
    pending_op: Option<CalcOp>,
    fresh_entry: bool,
    expression_history: String,
}

impl CalculatorState {
    fn new() -> Self {
        Self {
            current: "0".to_string(),
            accumulator: None,
            pending_op: None,
            fresh_entry: true,
            expression_history: String::new(),
        }
    }

    fn current_val(&self) -> f64 {
        self.current.parse::<f64>().unwrap_or(0.0)
    }

    fn set_result(&mut self, val: f64) {
        if !val.is_finite() {
            self.current = "错误".to_string();
            self.accumulator = None;
            self.pending_op = None;
            self.fresh_entry = true;
            return;
        }
        let s = format!("{:.8}", val);
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        self.current = if trimmed.is_empty() {
            "0".to_string()
        } else {
            trimmed.to_string()
        };
        self.fresh_entry = true;
    }

    fn on_digit(&mut self, digit: u8) {
        if self.fresh_entry || self.current == "0" || self.current == "错误" {
            self.current = digit.to_string();
            self.fresh_entry = false;
        } else if self.current.len() < 14 {
            self.current.push((b'0' + digit) as char);
        }
    }

    fn on_dot(&mut self) {
        if self.fresh_entry || self.current == "错误" {
            self.current = "0.".to_string();
            self.fresh_entry = false;
        } else if !self.current.contains('.') && self.current.len() < 14 {
            self.current.push('.');
        }
    }

    fn on_backspace(&mut self) {
        if self.fresh_entry || self.current == "错误" {
            return;
        }
        self.current.pop();
        if self.current.is_empty() || self.current == "-" {
            self.current = "0".to_string();
            self.fresh_entry = true;
        }
    }

    fn on_clear(&mut self) {
        self.current = "0".to_string();
        self.accumulator = None;
        self.pending_op = None;
        self.fresh_entry = true;
        self.expression_history.clear();
    }

    fn on_negate(&mut self) {
        if self.current == "0" || self.current == "错误" {
            return;
        }
        if self.current.starts_with('-') {
            self.current.remove(0);
        } else {
            self.current.insert(0, '-');
        }
    }

    fn on_percent(&mut self) {
        let val = self.current_val() / 100.0;
        self.set_result(val);
    }

    fn execute_pending(&mut self) {
        if let (Some(acc), Some(op)) = (self.accumulator, self.pending_op) {
            let rhs = self.current_val();
            let res = match op {
                CalcOp::Add => acc + rhs,
                CalcOp::Sub => acc - rhs,
                CalcOp::Mul => acc * rhs,
                CalcOp::Div => {
                    if rhs.abs() < 1e-12 {
                        f64::NAN
                    } else {
                        acc / rhs
                    }
                }
            };
            self.set_result(res);
            self.accumulator = if res.is_finite() { Some(res) } else { None };
        }
    }

    fn on_op(&mut self, op: CalcOp) {
        let cur = self.current_val();
        if self.accumulator.is_some() && !self.fresh_entry {
            self.execute_pending();
        } else {
            self.accumulator = Some(cur);
        }
        self.pending_op = Some(op);
        self.expression_history = format!("{} {}", self.current, op.symbol());
        self.fresh_entry = true;
    }

    fn on_equal(&mut self) {
        if self.pending_op.is_some() {
            let rhs_str = self.current.clone();
            self.execute_pending();
            if self.pending_op.is_some() {
                self.expression_history =
                    format!("{} {} {}", self.expression_history, rhs_str, "=");
            }
            self.pending_op = None;
            self.accumulator = None;
            self.fresh_entry = true;
        }
    }

    fn dispatch(&mut self, kind: BtnKind) {
        match kind {
            BtnKind::Digit(d) => self.on_digit(d),
            BtnKind::Dot => self.on_dot(),
            BtnKind::Backspace => self.on_backspace(),
            BtnKind::Clear => self.on_clear(),
            BtnKind::Negate => self.on_negate(),
            BtnKind::Percent => self.on_percent(),
            BtnKind::Op(op) => self.on_op(op),
            BtnKind::Equal => self.on_equal(),
        }
    }
}

pub fn run() -> Result<()> {
    let mut app = App::new("计算器", Icon::Calculator, 320, 460)?;
    app.min_width = 280;
    app.min_height = 400;

    let mut state = CalculatorState::new();
    let mut hover_btn: Option<(usize, usize)> = None;
    let mut pressed_btn: Option<(usize, usize)> = None;
    let mut last_press_time: Option<Instant> = None;

    loop {
        if app.close {
            break;
        }

        if app.needs_redraw {
            let content = app.content();
            draw_calc(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &state,
                content,
                hover_btn,
                pressed_btn,
            );
            app.draw_chrome();
            app.present()?;
        }

        let timeout = if pressed_btn.is_some() { 30 } else { 80 };
        for event in app.pump_with_fd(None, timeout)?.0 {
            let content = app.content();
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    let next = hit_test(x, y, content);
                    if next != hover_btn {
                        hover_btn = next;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x,
                    y,
                    button: 1,
                    down,
                    ..
                } => {
                    if down {
                        if let Some((r, c)) = hit_test(x, y, content) {
                            pressed_btn = Some((r, c));
                            last_press_time = Some(Instant::now());
                            state.dispatch(BUTTON_GRID[r][c]);
                            app.needs_redraw = true;
                        }
                    } else if pressed_btn.is_some() {
                        pressed_btn = None;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Key(key) => {
                    if let Some(kind) = translate_key(&key) {
                        state.dispatch(kind);
                        app.needs_redraw = true;
                    }
                }
                _ => {}
            }
        }

        if let Some(t) = last_press_time {
            if t.elapsed().as_millis() > 90 && pressed_btn.is_some() {
                pressed_btn = None;
                app.needs_redraw = true;
            }
        }
    }
    Ok(())
}

fn hit_test(x: i32, y: i32, content: Rect) -> Option<(usize, usize)> {
    let keypad_top = content.y + 104;
    let keypad_h = content.bottom() - keypad_top - 12;
    if x < content.x + 12
        || x >= content.right() - 12
        || y < keypad_top
        || y >= keypad_top + keypad_h
    {
        return None;
    }
    let cell_w = (content.w - 24) / 4;
    let cell_h = keypad_h / 5;
    if cell_w <= 0 || cell_h <= 0 {
        return None;
    }
    let col = ((x - (content.x + 12)) / cell_w).clamp(0, 3) as usize;
    let row = ((y - keypad_top) / cell_h).clamp(0, 4) as usize;
    Some((row, col))
}

fn translate_key(key: &Key) -> Option<BtnKind> {
    match key.sym {
        0x30..=0x39 => Some(BtnKind::Digit((key.sym - 0x30) as u8)),
        0xffb0..=0xffb9 => Some(BtnKind::Digit((key.sym - 0xffb0) as u8)), // Keypad 0..9
        0x2e | 0xffae => Some(BtnKind::Dot),                               // '.' or KP_Decimal
        0x2b | 0xffab => Some(BtnKind::Op(CalcOp::Add)),                   // '+' or KP_Add
        0x2d | 0xffad => Some(BtnKind::Op(CalcOp::Sub)),                   // '-' or KP_Subtract
        0x2a | 0xffaa => Some(BtnKind::Op(CalcOp::Mul)),                   // '*' or KP_Multiply
        0x2f | 0xffaf => Some(BtnKind::Op(CalcOp::Div)),                   // '/' or KP_Divide
        0x25 => Some(BtnKind::Percent),                                    // '%'
        0xff0d | 0xff8d | 0x3d => Some(BtnKind::Equal), // Return / KP_Enter / '='
        0xff08 => Some(BtnKind::Backspace),             // Backspace
        0xff1b | 0x63 | 0x43 => Some(BtnKind::Clear),   // Escape / 'c' / 'C'
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_calc(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &CalculatorState,
    content: Rect,
    hover: Option<(usize, usize)>,
    pressed: Option<(usize, usize)>,
) {
    let is_win98 = theme.is_win98();
    let ctrl_r = theme.radius_control();
    canvas.clear(theme.window);

    // Readout Display Area
    let display_r = Rect::new(content.x + 12, content.y + 8, content.w - 24, 88);
    if is_win98 {
        canvas.fill(display_r, theme.input_bg);
        ui::sunken_bevel(canvas, theme, display_r);
    } else {
        canvas.rounded_rect(display_r, 6, theme.window_alt, 1.0);
        canvas.rounded_border(display_r, 6, theme.window_border, 1.0, 1);
    }

    // Expression history line
    if !state.expression_history.is_empty() {
        let hist_w = text.measure(&state.expression_history, 11.5, Face::Ui);
        let hist_x = (display_r.right() - 14) as f32 - hist_w;
        text.draw(
            canvas,
            &state.expression_history,
            hist_x.max(display_r.x as f32 + 10.0),
            display_r.y as f32 + 22.0,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
    }

    // Main numeric readout (adaptive font size)
    let num_str = &state.current;
    let base_size = if num_str.len() > 10 {
        20.0
    } else if num_str.len() > 7 {
        24.0
    } else {
        28.0
    };
    let num_w = text.measure(num_str, base_size, Face::Bold);
    let num_x = (display_r.right() - 14) as f32 - num_w;
    text.draw(
        canvas,
        num_str,
        num_x.max(display_r.x as f32 + 10.0),
        display_r.bottom() as f32 - 16.0,
        base_size,
        theme.text,
        Face::Bold,
    );

    // Keypad Grid
    let keypad_top = content.y + 104;
    let pad_w = content.w - 24;
    let pad_h = content.bottom() - keypad_top - 12;
    let gap = 6;
    let btn_w = (pad_w - gap * 3) / 4;
    let btn_h = (pad_h - gap * 4) / 5;

    for row in 0..5 {
        for col in 0..4 {
            let bx = content.x + 12 + col as i32 * (btn_w + gap);
            let by = keypad_top + row as i32 * (btn_h + gap);
            let mut btn_r = Rect::new(bx, by, btn_w, btn_h);
            let is_hover = hover == Some((row, col));
            let is_pressed = pressed == Some((row, col));
            let kind = BUTTON_GRID[row][col];

            let (label, is_accent, is_func) = match kind {
                BtnKind::Clear => ("C", false, true),
                BtnKind::Backspace => ("⌫", false, true),
                BtnKind::Percent => ("%", false, true),
                BtnKind::Negate => ("±", false, true),
                BtnKind::Op(op) => (op.symbol(), true, false),
                BtnKind::Equal => ("=", true, false),
                BtnKind::Dot => (".", false, false),
                BtnKind::Digit(0) => ("0", false, false),
                BtnKind::Digit(1) => ("1", false, false),
                BtnKind::Digit(2) => ("2", false, false),
                BtnKind::Digit(3) => ("3", false, false),
                BtnKind::Digit(4) => ("4", false, false),
                BtnKind::Digit(5) => ("5", false, false),
                BtnKind::Digit(6) => ("6", false, false),
                BtnKind::Digit(7) => ("7", false, false),
                BtnKind::Digit(8) => ("8", false, false),
                BtnKind::Digit(9) => ("9", false, false),
                _ => ("", false, false),
            };

            if is_win98 {
                let bg = if is_accent {
                    theme.accent
                } else if is_hover {
                    theme.panel_hover
                } else {
                    theme.panel
                };
                canvas.fill(btn_r, bg);
                if is_pressed {
                    ui::sunken_bevel(canvas, theme, btn_r);
                } else {
                    ui::raised_bevel(canvas, theme, btn_r);
                }
            } else {
                // Press tactile micro-inset
                if is_pressed {
                    btn_r.x += 1;
                    btn_r.y += 1;
                    btn_r.w = (btn_r.w - 2).max(1);
                    btn_r.h = (btn_r.h - 2).max(1);
                }

                let hover_t = ui::animate_bool(
                    (row as u64) << 16 | (col as u64),
                    is_hover,
                    ui::MOTION_HOVER,
                );

                let bg = if is_accent {
                    if is_pressed {
                        theme.accent
                    } else if hover_t > 0.001 {
                        crate::draw::mix(theme.accent, 0xffffff, 0.12 * hover_t)
                    } else {
                        theme.accent
                    }
                } else if is_func {
                    let base = theme.window_high;
                    if is_pressed {
                        theme.window_alt
                    } else if hover_t > 0.001 {
                        crate::draw::mix(base, theme.panel_hover, hover_t)
                    } else {
                        base
                    }
                } else {
                    let base = theme.panel;
                    if is_pressed {
                        theme.window_alt
                    } else if hover_t > 0.001 {
                        crate::draw::mix(base, theme.panel_hover, hover_t)
                    } else {
                        base
                    }
                };

                canvas.rounded_rect(btn_r, ctrl_r, bg, 1.0);
                if !is_accent {
                    canvas.rounded_border(btn_r, ctrl_r, theme.window_border, 1.0, 1);
                }
            }

            let shift = if is_win98 && is_pressed { 1 } else { 0 };
            let fg = if is_accent {
                0xffffff
            } else if is_func {
                if is_win98 { 0x800000 } else { theme.text_dim }
            } else {
                theme.text
            };

            let face = if is_accent || is_func {
                Face::Bold
            } else {
                Face::Ui
            };
            let font_sz = if is_accent || is_func { 16.0 } else { 18.0 };
            text.draw_centered(
                canvas,
                label,
                btn_r.x as f32 + btn_r.w as f32 * 0.5 + shift as f32,
                btn_r.y as f32 + btn_r.h as f32 * 0.5 + font_sz * 0.35 + shift as f32,
                font_sz,
                fg,
                face,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculator_basic_arithmetic() {
        let mut calc = CalculatorState::new();
        calc.on_digit(7);
        calc.on_op(CalcOp::Add);
        calc.on_digit(5);
        calc.on_equal();
        assert_eq!(calc.current, "12");

        calc.on_op(CalcOp::Mul);
        calc.on_digit(3);
        calc.on_equal();
        assert_eq!(calc.current, "36");
    }

    #[test]
    fn test_calculator_negate_and_percent() {
        let mut calc = CalculatorState::new();
        calc.on_digit(5);
        calc.on_negate();
        assert_eq!(calc.current, "-5");
        calc.on_negate();
        assert_eq!(calc.current, "5");
        calc.on_percent();
        assert_eq!(calc.current, "0.05");
    }
}
