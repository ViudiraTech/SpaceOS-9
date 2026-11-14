/*
 *
 *       user/desktop/src/apps/oobe.rs
 *       SPUI out-of-box experience: first-boot account and system setup
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! OOBE runs only while no human account exists and `/var/lib/spaceos/oobe-complete`
//! is absent.  The wizard collects non-secret choices locally, validates the
//! account fields, then sends one `OobeCommit` transaction to DDM.  DDM is the
//! authority: it re-validates every field, creates the account, and writes the
//! completion marker only after the home directory and shadow entry exist.

use crate::apps::session_ui::{draw_logo, Backend, Message, Screen};
use crate::ddm::auth::{password_strength_label, validate_password};
use crate::ddm::config::is_valid_user_name;
use crate::ddm::wire::ClientKind;
use crate::draw::Rect;
use crate::text::Face;
use crate::ui;
use crate::x11::{Key, Result};
use std::time::{Duration, Instant};
use x11rb::protocol::Event;

const CARD_WIDTH: i32 = 1080;
const CARD_HEIGHT: i32 = 640;
const RAIL_WIDTH: i32 = 292;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    None,
    FullName,
    Username,
    Password,
    Confirm,
    Show,
    Hostname,
    AutoLogin,
    Language(usize),
    Keyboard(usize),
    Timezone(usize),
    Step(usize),
    Back,
    Next,
    Finish,
}

struct Oobe {
    screen: Screen,
    backend: Backend,
    step: usize,
    full_name: String,
    username: String,
    password: String,
    confirm: String,
    hostname: String,
    timezone: String,
    keyboard: String,
    language: String,
    auto_login: bool,
    show_password: bool,
    focus: Focus,
    hover: Focus,
    error: String,
    waiting: bool,
    caps_lock: bool,
    last_clock: Instant,
    clock: String,
}

pub fn run(arguments: &[String]) -> Result<()> {
    let socket = socket_argument(arguments).unwrap_or_else(|| "/run/ddm/greeter.sock".to_string());
    let backend = Backend::connect(&socket, ClientKind::Oobe)?;
    if backend.config.mode != ClientKind::Oobe {
        return Err("DDM is not in first-boot setup mode".into());
    }
    let screen = Screen::new(&backend.config.wallpaper)?;
    let mut oobe = Oobe::new(screen, backend);
    oobe.loop_until_done()
}

fn socket_argument(arguments: &[String]) -> Option<String> {
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--socket" {
            return arguments.get(index + 1).cloned();
        }
        index += 1;
    }
    None
}

impl Oobe {
    fn new(screen: Screen, backend: Backend) -> Self {
        let language = if backend.config.locale.starts_with("en") {
            "en_US".to_string()
        } else {
            "zh_CN".to_string()
        };
        let timezone = if backend.config.timezone.is_empty() {
            "UTC".to_string()
        } else {
            backend.config.timezone.clone()
        };
        let keyboard = if backend.config.keyboard.is_empty() {
            "us".to_string()
        } else {
            backend.config.keyboard.clone()
        };
        let hostname = if backend.config.hostname.is_empty() {
            "spaceos".to_string()
        } else {
            backend.config.hostname.clone()
        };
        let username = if backend.config.suggested_user.is_empty() {
            "user".to_string()
        } else {
            backend.config.suggested_user.clone()
        };
        let (clock, _) = clock_string();
        Self {
            screen,
            backend,
            step: 0,
            full_name: "SpaceOS User".to_string(),
            username,
            password: String::new(),
            confirm: String::new(),
            hostname,
            timezone,
            keyboard,
            language,
            auto_login: false,
            show_password: false,
            focus: Focus::Language(0),
            hover: Focus::None,
            error: String::new(),
            waiting: false,
            caps_lock: false,
            last_clock: Instant::now(),
            clock,
        }
    }

    fn loop_until_done(&mut self) -> Result<()> {
        loop {
            if self.screen.needs_redraw {
                self.draw();
                self.screen.present()?;
            }
            if self.last_clock.elapsed() >= Duration::from_secs(1) {
                let (clock, _) = clock_string();
                if clock != self.clock {
                    self.clock = clock;
                    self.screen.needs_redraw = true;
                }
                self.last_clock = Instant::now();
            }
            let (_, backend_ready) = self.screen.wait(self.backend.fd(), 100);
            if backend_ready {
                loop {
                    match self.backend.read_available() {
                        Ok(Some(message)) => {
                            if self.handle_message(message)? {
                                return Ok(());
                            }
                        }
                        Ok(None) => break,
                        Err(error) => return Err(error),
                    }
                }
            }
            while let Some(event) = self.screen.x.poll()? {
                if self.screen.handle_event(&event) {
                    continue;
                }
                match event {
                    Event::KeyPress(key) => self.handle_key(key),
                    Event::ButtonPress(button) => {
                        let _ = self.screen.x.focus(self.screen.window);
                        self.handle_click(button.event_x as i32, button.event_y as i32);
                    }
                    Event::MotionNotify(motion) => {
                        self.handle_motion(motion.event_x as i32, motion.event_y as i32);
                    }
                    Event::DestroyNotify(destroy) if destroy.window == self.screen.window => {
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }

    fn handle_message(&mut self, message: Message) -> Result<bool> {
        match message {
            Message::OobeOk { .. } => return Ok(true),
            Message::Pong | Message::Config(_) => {}
            Message::Error {
                message, retry_ms, ..
            } => {
                self.waiting = false;
                self.error = if retry_ms > 0 {
                    let seconds = (retry_ms + 999) / 1000;
                    format!("{message}（{} 秒后可重试）", seconds)
                } else {
                    message
                };
                self.screen.needs_redraw = true;
            }
            Message::LoginOk { .. } => {
                return Err("DDM returned a login response to the OOBE client".into())
            }
            Message::PowerOk => {}
        }
        Ok(false)
    }

    fn handle_key(&mut self, event: x11rb::protocol::xproto::KeyPressEvent) {
        let key = Key {
            code: event.detail,
            state: event.state.into(),
            sym: self.screen.x.key_symbol(event.detail, event.state.into()),
        };
        self.caps_lock = event
            .state
            .contains(x11rb::protocol::xproto::KeyButMask::LOCK);
        if self.waiting {
            return;
        }
        if key.is_tab() {
            self.cycle_focus(!key.shift());
            self.screen.needs_redraw = true;
            return;
        }
        if key.is_enter() {
            self.primary_action();
            return;
        }
        if key.is_escape() {
            if !self.error.is_empty() {
                self.error.clear();
                self.screen.needs_redraw = true;
            } else if self.step > 0 {
                self.step -= 1;
                self.focus = self.first_focus();
                self.screen.needs_redraw = true;
            }
            return;
        }
        if key.is_backspace() {
            match self.focus {
                Focus::FullName => {
                    self.full_name.pop();
                }
                Focus::Username => {
                    self.username.pop();
                }
                Focus::Password => {
                    self.password.pop();
                }
                Focus::Confirm => {
                    self.confirm.pop();
                }
                Focus::Hostname => {
                    self.hostname.pop();
                }
                _ => {}
            }
            self.screen.needs_redraw = true;
            return;
        }
        if key.sym == 0x20 {
            if self.focus == Focus::AutoLogin {
                self.auto_login = !self.auto_login;
                self.screen.needs_redraw = true;
                return;
            }
            if self.focus == Focus::Show {
                self.show_password = !self.show_password;
                self.screen.needs_redraw = true;
                return;
            }
        }
        if let Some(ch) = key.text().filter(|ch| !ch.is_control()) {
            match self.focus {
                Focus::FullName if self.full_name.chars().count() < 64 => {
                    self.full_name.push(ch);
                    self.error.clear();
                }
                Focus::Username if self.username.chars().count() < 32 => {
                    self.username.push(ch);
                    self.error.clear();
                }
                Focus::Password if self.password.chars().count() < 128 => {
                    self.password.push(ch);
                    self.error.clear();
                }
                Focus::Confirm if self.confirm.chars().count() < 128 => {
                    self.confirm.push(ch);
                    self.error.clear();
                }
                Focus::Hostname if self.hostname.chars().count() < 63 => {
                    self.hostname.push(ch);
                    self.error.clear();
                }
                _ => return,
            }
            self.screen.needs_redraw = true;
        }
    }

    fn cycle_focus(&mut self, forward: bool) {
        let targets = self.focus_targets();
        if targets.is_empty() {
            return;
        }
        let current = targets
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % targets.len()
        } else {
            (current + targets.len() - 1) % targets.len()
        };
        self.focus = targets[next];
    }

    fn first_focus(&self) -> Focus {
        self.focus_targets().first().copied().unwrap_or(Focus::Next)
    }

    fn focus_targets(&self) -> Vec<Focus> {
        match self.step {
            0 => {
                let mut targets = Vec::new();
                for index in 0..LANGUAGES.len() {
                    targets.push(Focus::Language(index));
                }
                for index in 0..KEYBOARDS.len() {
                    targets.push(Focus::Keyboard(index));
                }
                for index in 0..TIMEZONES.len() {
                    targets.push(Focus::Timezone(index));
                }
                targets.push(Focus::Next);
                targets
            }
            1 => vec![
                Focus::FullName,
                Focus::Username,
                Focus::Password,
                Focus::Confirm,
                Focus::Show,
                Focus::Back,
                Focus::Next,
            ],
            2 => vec![Focus::Hostname, Focus::AutoLogin, Focus::Back, Focus::Next],
            _ => vec![Focus::Back, Focus::Finish],
        }
    }

    fn handle_click(&mut self, x: i32, y: i32) {
        if self.waiting {
            return;
        }
        let layout = self.layout();
        if layout.next.contains(x, y) {
            self.primary_action();
            return;
        }
        if layout.back.contains(x, y) {
            if self.step > 0 {
                self.step -= 1;
                self.focus = self.first_focus();
            }
            self.screen.needs_redraw = true;
            return;
        }
        for (index, rect) in layout.step_rows.iter().enumerate() {
            if rect.contains(x, y) && index < self.step {
                self.step = index;
                self.focus = self.first_focus();
                self.error.clear();
                self.screen.needs_redraw = true;
                return;
            }
        }
        match self.step {
            0 => {
                for (index, rect) in layout.languages.iter().enumerate() {
                    if rect.contains(x, y) {
                        self.language = LANGUAGES[index].0.to_string();
                        self.focus = Focus::Language(index);
                        self.screen.needs_redraw = true;
                        return;
                    }
                }
                for (index, rect) in layout.keyboards.iter().enumerate() {
                    if rect.contains(x, y) {
                        self.keyboard = KEYBOARDS[index].0.to_string();
                        self.focus = Focus::Keyboard(index);
                        self.screen.needs_redraw = true;
                        return;
                    }
                }
                for (index, rect) in layout.timezones.iter().enumerate() {
                    if rect.contains(x, y) {
                        self.timezone = TIMEZONES[index].0.to_string();
                        self.focus = Focus::Timezone(index);
                        self.screen.needs_redraw = true;
                        return;
                    }
                }
            }
            1 => {
                if layout.full_name.contains(x, y) {
                    self.focus = Focus::FullName;
                } else if layout.username.contains(x, y) {
                    self.focus = Focus::Username;
                } else if layout.password.contains(x, y) {
                    self.focus = Focus::Password;
                } else if layout.confirm.contains(x, y) {
                    self.focus = Focus::Confirm;
                } else if layout.show.contains(x, y) {
                    self.show_password = !self.show_password;
                }
                self.screen.needs_redraw = true;
            }
            2 => {
                if layout.hostname.contains(x, y) {
                    self.focus = Focus::Hostname;
                } else if layout.auto_login.contains(x, y) {
                    self.auto_login = !self.auto_login;
                    self.screen.needs_redraw = true;
                }
                self.screen.needs_redraw = true;
            }
            _ => {}
        }
    }

    fn handle_motion(&mut self, x: i32, y: i32) {
        let layout = self.layout();
        let hover = if layout.next.contains(x, y) {
            Focus::Next
        } else if layout.back.contains(x, y) {
            Focus::Back
        } else if self.step == 1 && layout.full_name.contains(x, y) {
            Focus::FullName
        } else if self.step == 1 && layout.username.contains(x, y) {
            Focus::Username
        } else if self.step == 1 && layout.password.contains(x, y) {
            Focus::Password
        } else if self.step == 1 && layout.confirm.contains(x, y) {
            Focus::Confirm
        } else if self.step == 1 && layout.show.contains(x, y) {
            Focus::Show
        } else if self.step == 2 && layout.hostname.contains(x, y) {
            Focus::Hostname
        } else if self.step == 2 && layout.auto_login.contains(x, y) {
            Focus::AutoLogin
        } else if self.step == 0 {
            if let Some(index) = layout.languages.iter().position(|rect| rect.contains(x, y)) {
                Focus::Language(index)
            } else if let Some(index) = layout.keyboards.iter().position(|rect| rect.contains(x, y))
            {
                Focus::Keyboard(index)
            } else if let Some(index) = layout.timezones.iter().position(|rect| rect.contains(x, y))
            {
                Focus::Timezone(index)
            } else if let Some(index) = layout.step_rows.iter().position(|rect| rect.contains(x, y))
            {
                Focus::Step(index)
            } else {
                Focus::None
            }
        } else if let Some(index) = layout.step_rows.iter().position(|rect| rect.contains(x, y)) {
            Focus::Step(index)
        } else {
            Focus::None
        };
        if hover != self.hover {
            self.hover = hover;
            let cursor = if hover == Focus::None {
                crate::x11::CursorKind::Default
            } else {
                crate::x11::CursorKind::Pointer
            };
            let _ = self.screen.x.set_cursor(self.screen.window, cursor);
            self.screen.needs_redraw = true;
        }
    }

    fn primary_action(&mut self) {
        if self.waiting {
            return;
        }
        match self.step {
            0 => {
                self.step = 1;
                self.focus = Focus::FullName;
                self.error.clear();
                self.screen.needs_redraw = true;
            }
            1 => {
                if let Err(error) = self.validate_account() {
                    self.error = error;
                    self.screen.needs_redraw = true;
                    return;
                }
                self.step = 2;
                self.focus = Focus::Hostname;
                self.error.clear();
                self.screen.needs_redraw = true;
            }
            2 => {
                if let Err(error) = self.validate_system() {
                    self.error = error;
                    self.screen.needs_redraw = true;
                    return;
                }
                self.step = 3;
                self.focus = Focus::Finish;
                self.error.clear();
                self.screen.needs_redraw = true;
            }
            _ => self.commit(),
        }
    }

    fn validate_account(&self) -> std::result::Result<(), String> {
        if self.full_name.trim().is_empty() {
            return Err("请输入显示名称".to_string());
        }
        if !is_valid_user_name(&self.username) {
            return Err(
                "用户名需以小写字母或下划线开头，只包含小写字母、数字、下划线和连字符".to_string(),
            );
        }
        if self.password != self.confirm {
            return Err("两次输入的密码不一致".to_string());
        }
        Ok(())
    }

    fn validate_system(&self) -> std::result::Result<(), String> {
        if self.hostname.trim().is_empty() {
            return Err("请输入主机名".to_string());
        }
        if !self
            .hostname
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
        {
            return Err("主机名只能包含字母、数字、连字符和点".to_string());
        }
        if !TIMEZONES.iter().any(|(zone, _)| *zone == self.timezone) {
            return Err("请选择有效的时区".to_string());
        }
        Ok(())
    }

    fn commit(&mut self) {
        if let Err(error) = self.validate_account().and_then(|_| self.validate_system()) {
            self.error = error;
            self.screen.needs_redraw = true;
            return;
        }
        self.waiting = true;
        self.error.clear();
        self.screen.needs_redraw = true;
        if let Err(error) = self.backend.oobe_commit(
            self.username.trim(),
            self.full_name.trim(),
            self.password.as_bytes(),
            &self.timezone,
            &self.language,
            &self.keyboard,
            self.hostname.trim(),
            self.auto_login,
        ) {
            self.waiting = false;
            self.error = error.to_string();
        }
    }

    fn layout(&self) -> Layout {
        let card = Rect::new(
            (self.screen.width - CARD_WIDTH) / 2,
            (self.screen.height - CARD_HEIGHT) / 2,
            CARD_WIDTH,
            CARD_HEIGHT,
        );
        let content_x = card.x + RAIL_WIDTH + 48;
        let content_w = card.w - RAIL_WIDTH - 96;
        let step_rows = (0..STEP_TITLES.len())
            .map(|index| {
                Rect::new(
                    card.x + 28,
                    card.y + 178 + index as i32 * 62,
                    RAIL_WIDTH - 56,
                    50,
                )
            })
            .collect();
        let mut languages = Vec::new();
        let mut keyboards = Vec::new();
        let mut timezones = Vec::new();
        if self.step == 0 {
            for index in 0..LANGUAGES.len() {
                languages.push(Rect::new(
                    content_x + index as i32 * 160,
                    card.y + 142,
                    150,
                    38,
                ));
            }
            for index in 0..KEYBOARDS.len() {
                keyboards.push(Rect::new(
                    content_x + index as i32 * 132,
                    card.y + 246,
                    122,
                    34,
                ));
            }
            for index in 0..TIMEZONES.len() {
                let row = index / 2;
                let column = index % 2;
                timezones.push(Rect::new(
                    content_x + column as i32 * 270,
                    card.y + 338 + row as i32 * 42,
                    258,
                    34,
                ));
            }
        }
        Layout {
            card,
            content_x,
            content_w,
            step_rows,
            languages,
            keyboards,
            timezones,
            full_name: Rect::new(content_x, card.y + 156, content_w, 38),
            username: Rect::new(content_x, card.y + 230, content_w, 38),
            password: Rect::new(content_x, card.y + 304, content_w, 38),
            confirm: Rect::new(content_x, card.y + 378, content_w, 38),
            show: Rect::new(content_x + content_w - 50, card.y + 312, 40, 22),
            hostname: Rect::new(content_x, card.y + 186, content_w, 38),
            auto_login: Rect::new(content_x, card.y + 302, content_w, 48),
            back: Rect::new(card.right() - 300, card.bottom() - 62, 108, 38),
            next: Rect::new(card.right() - 172, card.bottom() - 62, 132, 38),
        }
    }

    fn draw(&mut self) {
        let theme = self.screen.theme;
        let is_win98 = theme.is_win98();
        let layout = self.layout();
        self.screen.draw_background();
        let rail = Rect::new(layout.card.x, layout.card.y, RAIL_WIDTH, layout.card.h);
        if is_win98 {
            self.screen.canvas.fill(layout.card, theme.panel);
            ui::window_outer_bevel(&mut self.screen.canvas, &theme, layout.card);
            let tb = Rect::new(layout.card.x + 3, layout.card.y + 3, layout.card.w - 6, 22);
            self.screen
                .canvas
                .horizontal_gradient(tb, theme.titlebar, theme.titlebar_hi);
            self.screen.text.draw(
                &mut self.screen.canvas,
                "SpaceOS 安装向导",
                (tb.x + 8) as f32,
                (tb.y + 15) as f32,
                11.5,
                theme.titlebar_text,
                Face::Bold,
            );
            let sep_x = rail.right() - 1;
            self.screen.canvas.fill(
                Rect::new(sep_x, tb.bottom() + 6, 1, layout.card.h - 36),
                theme.bevel_shadow,
            );
            self.screen.canvas.fill(
                Rect::new(sep_x + 1, tb.bottom() + 6, 1, layout.card.h - 36),
                theme.bevel_light,
            );
        } else {
            self.screen
                .canvas
                .rounded_rect(layout.card, 12, theme.window, 0.97);
            self.screen
                .canvas
                .rounded_border(layout.card, 12, theme.window_border, 1.0, 1);
            self.screen
                .canvas
                .rounded_rect(rail, 12, theme.window_alt, 0.96);
            self.screen.canvas.fill(
                Rect::new(rail.right() - 1, rail.y + 1, 1, rail.h - 2),
                theme.window_border,
            );
        }

        draw_logo(&mut self.screen, rail.x + 28, rail.y + 30, 40);
        self.screen.text.draw(
            &mut self.screen.canvas,
            "SpaceOS 9",
            (rail.x + 80) as f32,
            (rail.y + 48) as f32,
            19.0,
            theme.text,
            Face::Bold,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "首次设置",
            (rail.x + 80) as f32,
            (rail.y + 66) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "完成以下步骤以创建管理员账户",
            (rail.x + 28) as f32,
            (rail.y + 112) as f32,
            10.5,
            theme.text_dim,
            Face::Ui,
        );

        for (index, (title, subtitle)) in STEP_TITLES.iter().enumerate() {
            let row = layout.step_rows[index];
            let active = index == self.step;
            let done = index < self.step;
            let hovered = self.hover == Focus::Step(index);
            let win98_act = is_win98 && active;
            let row_fg = if is_win98 {
                ui::list_item_bg(&mut self.screen.canvas, &theme, row, active, hovered)
            } else {
                if active || hovered {
                    self.screen.canvas.rounded_rect(
                        row,
                        8,
                        if active {
                            theme.accent_soft
                        } else {
                            theme.panel_hover
                        },
                        1.0,
                    );
                }
                if active { theme.text } else { theme.text_dim }
            };
            if is_win98 {
                let b = Rect::new(row.x + 11, row.y + 14, 22, 22);
                self.screen.canvas.fill(
                    b,
                    if active || done {
                        theme.accent
                    } else {
                        theme.panel
                    },
                );
                ui::raised_bevel(&mut self.screen.canvas, &theme, b);
            } else {
                self.screen.canvas.circle(
                    (row.x + 22) as f32,
                    (row.y + 25) as f32,
                    15.0,
                    if active || done {
                        theme.accent
                    } else {
                        theme.panel_high
                    },
                    1.0,
                );
            }
            self.screen.text.draw_centered(
                &mut self.screen.canvas,
                &format!("{}", index + 1),
                (row.x + 22) as f32,
                (row.y + 30) as f32,
                11.0,
                if active || done {
                    0xffffff
                } else {
                    theme.text_muted
                },
                Face::Bold,
            );
            self.screen.text.draw(
                &mut self.screen.canvas,
                title,
                (row.x + 46) as f32,
                (row.y + 21) as f32,
                12.0,
                row_fg,
                Face::Bold,
            );
            self.screen.text.draw(
                &mut self.screen.canvas,
                subtitle,
                (row.x + 46) as f32,
                (row.y + 37) as f32,
                9.5,
                if win98_act { row_fg } else { theme.text_muted },
                Face::Ui,
            );
        }

        let title = STEP_TITLES[self.step].0;
        let subtitle = STEP_SUBTITLES[self.step];
        self.screen.text.draw(
            &mut self.screen.canvas,
            title,
            layout.content_x as f32,
            (layout.card.y + 56) as f32,
            24.0,
            theme.text,
            Face::Bold,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            subtitle,
            layout.content_x as f32,
            (layout.card.y + 80) as f32,
            11.5,
            theme.text_muted,
            Face::Ui,
        );

        match self.step {
            0 => self.draw_language(&layout),
            1 => self.draw_account(&layout),
            2 => self.draw_system(&layout),
            _ => self.draw_finish(&layout),
        }

        if !self.error.is_empty() {
            let error =
                self.screen
                    .text
                    .truncate(&self.error, layout.content_w as f32, 11.5, Face::Ui);
            self.screen.text.draw(
                &mut self.screen.canvas,
                &error,
                layout.content_x as f32,
                (layout.card.bottom() - 92) as f32,
                11.5,
                theme.danger,
                Face::Ui,
            );
        }

        if self.step > 0 {
            ui::button(
                &mut self.screen.canvas,
                &mut self.screen.text,
                &theme,
                layout.back,
                "上一步",
                None,
                false,
                self.hover == Focus::Back || self.focus == Focus::Back,
            );
        }
        let next_label = if self.step == 3 {
            "开始使用"
        } else {
            "下一步"
        };
        ui::button(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.next,
            next_label,
            None,
            true,
            self.hover == Focus::Next || self.focus == Focus::Next,
        );
        if self.waiting {
            self.screen.text.draw(
                &mut self.screen.canvas,
                "正在创建账户并应用系统设置…",
                (layout.next.x - 280) as f32,
                (layout.next.y + 25) as f32,
                11.0,
                theme.text_muted,
                Face::Ui,
            );
        }
        self.screen.text.draw_right(
            &mut self.screen.canvas,
            &self.clock,
            (layout.card.right() - 24) as f32,
            (layout.card.y + 56) as f32,
            12.0,
            theme.text_muted,
            Face::Mono,
        );
    }

    fn draw_language(&mut self, layout: &Layout) {
        let theme = self.screen.theme;
        self.screen.text.draw(
            &mut self.screen.canvas,
            "语言",
            layout.content_x as f32,
            (layout.card.y + 124) as f32,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
        for (index, (_, label)) in LANGUAGES.iter().enumerate() {
            let rect = layout.languages[index];
            let active = self.language == LANGUAGES[index].0;
            choice(
                &mut self.screen,
                rect,
                label,
                active,
                self.hover == Focus::Language(index),
            );
        }
        self.screen.text.draw(
            &mut self.screen.canvas,
            "键盘布局",
            layout.content_x as f32,
            (layout.card.y + 228) as f32,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
        for (index, (_, label)) in KEYBOARDS.iter().enumerate() {
            let rect = layout.keyboards[index];
            let active = self.keyboard == KEYBOARDS[index].0;
            choice(
                &mut self.screen,
                rect,
                label,
                active,
                self.hover == Focus::Keyboard(index),
            );
        }
        self.screen.text.draw(
            &mut self.screen.canvas,
            "时区",
            layout.content_x as f32,
            (layout.card.y + 320) as f32,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
        for (index, (_, label)) in TIMEZONES.iter().enumerate() {
            let rect = layout.timezones[index];
            let active = self.timezone == TIMEZONES[index].0;
            choice(
                &mut self.screen,
                rect,
                label,
                active,
                self.hover == Focus::Timezone(index),
            );
        }
        self.screen.text.draw(
            &mut self.screen.canvas,
            "这些选择会写入系统区域设置；桌面界面当前使用简体中文。",
            layout.content_x as f32,
            (layout.card.y + 528) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
    }

    fn draw_account(&mut self, layout: &Layout) {
        let theme = self.screen.theme;
        field_label(&mut self.screen, layout.full_name, "显示名称");
        ui::text_input(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.full_name,
            &self.full_name,
            "例如：SpaceOS User",
            self.focus == Focus::FullName,
            None,
        );
        field_label(&mut self.screen, layout.username, "用户名");
        ui::text_input(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.username,
            &self.username,
            "小写字母、数字、下划线",
            self.focus == Focus::Username,
            None,
        );
        field_label(&mut self.screen, layout.password, "密码");
        let password_display = if self.show_password {
            self.password.clone()
        } else {
            "•".repeat(self.password.chars().count().min(24))
        };
        ui::text_input(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.password,
            &password_display,
            "至少 8 个字符",
            self.focus == Focus::Password,
            None,
        );
        ui::button(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.show,
            if self.show_password {
                "隐藏"
            } else {
                "显示"
            },
            None,
            false,
            self.hover == Focus::Show,
        );
        field_label(&mut self.screen, layout.confirm, "确认密码");
        let confirm_display = if self.show_password {
            self.confirm.clone()
        } else {
            "•".repeat(self.confirm.chars().count().min(24))
        };
        ui::text_input(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.confirm,
            &confirm_display,
            "再次输入密码",
            self.focus == Focus::Confirm,
            None,
        );

        let (score, label) = if self.password.is_empty() {
            (0, "未设置（允许空密码）")
        } else {
            match validate_password(&self.password, &self.username, &self.full_name) {
                Ok(score) => (score, password_strength_label(score)),
                Err(_) => (1, "简单密码（允许使用）"),
            }
        };
        let bar = Rect::new(layout.content_x, layout.card.y + 438, layout.content_w, 5);
        ui::progress_bar(
            &mut self.screen.canvas,
            &theme,
            bar,
            score as f32 / 4.0,
            Some(match score {
                0 => theme.danger,
                1 => theme.warning,
                2 => theme.warning,
                _ => theme.success,
            }),
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            &format!("密码强度：{label}"),
            layout.content_x as f32,
            (layout.card.y + 462) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "密码使用 SHA-512 crypt 哈希保存，DDM 不会以明文记录密码。",
            layout.content_x as f32,
            (layout.card.y + 486) as f32,
            10.0,
            theme.text_muted,
            Face::Ui,
        );
        if self.caps_lock {
            self.screen.text.draw(
                &mut self.screen.canvas,
                "大写锁定已开启",
                (layout.content_x + layout.content_w - 120) as f32,
                (layout.card.y + 420) as f32,
                10.5,
                theme.warning,
                Face::Ui,
            );
        }
    }

    fn draw_system(&mut self, layout: &Layout) {
        let theme = self.screen.theme;
        field_label(&mut self.screen, layout.hostname, "计算机名");
        ui::text_input(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.hostname,
            &self.hostname,
            "例如：spaceos",
            self.focus == Focus::Hostname,
            None,
        );
        let auto = layout.auto_login;
        if self.hover == Focus::AutoLogin || self.focus == Focus::AutoLogin {
            self.screen
                .canvas
                .rounded_rect(auto, theme.radius_control(), theme.panel_hover, 0.8);
        }
        ui::toggle_switch(
            &mut self.screen.canvas,
            &theme,
            Rect::new(auto.right() - 50, auto.y + 13, 42, 22),
            self.auto_login,
            self.focus == Focus::AutoLogin || self.hover == Focus::AutoLogin,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "自动登录",
            (auto.x + 12) as f32,
            (auto.y + 21) as f32,
            12.5,
            theme.text,
            Face::Bold,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "启动后直接进入该账户的桌面；公共或共享设备不建议开启。",
            (auto.x + 12) as f32,
            (auto.y + 38) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
        let note = Rect::new(layout.content_x, layout.card.y + 382, layout.content_w, 118);
        ui::card(&mut self.screen.canvas, &theme, note);
        self.screen.text.draw(
            &mut self.screen.canvas,
            "隐私与安全",
            (note.x + 16) as f32,
            (note.y + 24) as f32,
            12.5,
            theme.text,
            Face::Bold,
        );
        for (index, line) in [
            "SpaceOS 9 不上传使用统计或遥测数据。",
            "root 账户不能通过图形登录；管理员操作请使用 doas。",
            "首次设置完成后，OOBE 将不再出现。",
        ]
        .iter()
        .enumerate()
        {
            self.screen.text.draw(
                &mut self.screen.canvas,
                line,
                (note.x + 16) as f32,
                (note.y + 48 + index as i32 * 20) as f32,
                10.5,
                theme.text_dim,
                Face::Ui,
            );
        }
    }

    fn draw_finish(&mut self, layout: &Layout) {
        let theme = self.screen.theme;
        let summary = Rect::new(layout.content_x, layout.card.y + 132, layout.content_w, 300);
        ui::card(&mut self.screen.canvas, &theme, summary);
        let rows = [
            ("显示名称", self.full_name.trim().to_string()),
            ("用户名", self.username.trim().to_string()),
            ("计算机名", self.hostname.trim().to_string()),
            ("时区", timezone_label(&self.timezone).to_string()),
            ("键盘", keyboard_label(&self.keyboard).to_string()),
            (
                "系统语言",
                if self.language == "en_US" {
                    "English (US)"
                } else {
                    "简体中文"
                }
                .to_string(),
            ),
            (
                "自动登录",
                if self.auto_login {
                    "已开启"
                } else {
                    "关闭"
                }
                .to_string(),
            ),
        ];
        for (index, (label, value)) in rows.iter().enumerate() {
            let y = summary.y + 28 + index as i32 * 36;
            self.screen.text.draw(
                &mut self.screen.canvas,
                label,
                (summary.x + 18) as f32,
                (y + 14) as f32,
                11.0,
                theme.text_muted,
                Face::Ui,
            );
            self.screen.text.draw(
                &mut self.screen.canvas,
                value,
                (summary.x + 150) as f32,
                (y + 14) as f32,
                11.5,
                theme.text,
                Face::Mono,
            );
            if index + 1 < rows.len() {
                self.screen.canvas.fill(
                    Rect::new(summary.x + 18, y + 27, summary.w - 36, 1),
                    theme.window_border,
                );
            }
        }
        self.screen.text.draw(
            &mut self.screen.canvas,
            "点击「开始使用」后，DDM 将创建账户并自动登录到桌面。",
            layout.content_x as f32,
            (layout.card.y + 462) as f32,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "请妥善保管密码；忘记密码需要通过控制台中的管理员账户重置。",
            layout.content_x as f32,
            (layout.card.y + 486) as f32,
            10.0,
            theme.text_muted,
            Face::Ui,
        );
    }
}

fn choice(screen: &mut Screen, rect: Rect, label: &str, active: bool, hover: bool) {
    let theme = screen.theme;
    let fg = if theme.is_win98() {
        screen.canvas.fill(
            rect,
            if active {
                theme.selection_bg
            } else if hover {
                theme.panel_hover
            } else {
                theme.panel
            },
        );
        if active {
            ui::sunken_bevel(&mut screen.canvas, &theme, rect);
            theme.selection_text
        } else {
            ui::raised_bevel(&mut screen.canvas, &theme, rect);
            theme.text
        }
    } else {
        screen.canvas.rounded_rect(
            rect,
            6,
            if active {
                theme.accent_soft
            } else if hover {
                theme.panel_hover
            } else {
                theme.panel_high
            },
            1.0,
        );
        if active {
            screen.canvas.rounded_border(rect, 6, theme.accent, 0.9, 1);
        }
        if active { theme.text } else { theme.text_dim }
    };
    let shown = screen
        .text
        .truncate(label, (rect.w - 16) as f32, 10.5, Face::Ui);
    screen.text.draw(
        &mut screen.canvas,
        &shown,
        (rect.x + 8) as f32,
        (rect.y + 22) as f32,
        10.5,
        fg,
        Face::Ui,
    );
}

fn field_label(screen: &mut Screen, rect: Rect, label: &str) {
    let theme = screen.theme;
    screen.text.draw(
        &mut screen.canvas,
        label,
        rect.x as f32,
        (rect.y - 10) as f32,
        10.5,
        theme.text_muted,
        Face::Ui,
    );
}

struct Layout {
    card: Rect,
    content_x: i32,
    content_w: i32,
    step_rows: Vec<Rect>,
    languages: Vec<Rect>,
    keyboards: Vec<Rect>,
    timezones: Vec<Rect>,
    full_name: Rect,
    username: Rect,
    password: Rect,
    confirm: Rect,
    show: Rect,
    hostname: Rect,
    auto_login: Rect,
    back: Rect,
    next: Rect,
}

const STEP_TITLES: [(&str, &str); 4] = [
    ("语言与区域", "选择系统语言、键盘和时区"),
    ("创建账户", "建立第一个管理员账户"),
    ("系统选项", "设置计算机名和登录行为"),
    ("完成", "确认设置并进入桌面"),
];

const STEP_SUBTITLES: [&str; 4] = [
    "这些设置可以在桌面「系统设置」中再次修改。",
    "该账户将加入 wheel 组，可通过 doas 执行管理员命令。",
    "root 账户默认锁定，不能通过图形登录。",
    "首次设置完成后会自动进入新账户的桌面。",
];

const LANGUAGES: [(&str, &str); 2] = [("zh_CN", "简体中文"), ("en_US", "English")];

const KEYBOARDS: [(&str, &str); 4] = [
    ("us", "美式英语"),
    ("de", "德语"),
    ("fr", "法语"),
    ("jp", "日语"),
];

const TIMEZONES: [(&str, &str); 8] = [
    ("UTC", "UTC 协调世界时"),
    ("Asia/Shanghai", "中国标准时间"),
    ("Asia/Tokyo", "日本标准时间"),
    ("Asia/Singapore", "新加坡时间"),
    ("Europe/London", "伦敦时间"),
    ("Europe/Berlin", "柏林时间"),
    ("America/New_York", "纽约时间"),
    ("America/Los_Angeles", "洛杉矶时间"),
];

fn timezone_label(zone: &str) -> &'static str {
    TIMEZONES
        .iter()
        .find(|(value, _)| *value == zone)
        .map(|(_, label)| *label)
        .unwrap_or("UTC 协调世界时")
}

fn keyboard_label(layout: &str) -> &'static str {
    KEYBOARDS
        .iter()
        .find(|(value, _)| *value == layout)
        .map(|(_, label)| *label)
        .unwrap_or("美式英语")
}

fn clock_string() -> (String, ()) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&seconds, &mut tm);
    }
    (format!("{:02}:{:02}", tm.tm_hour, tm.tm_min), ())
}
