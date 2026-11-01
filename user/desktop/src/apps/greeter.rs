/*
 *
 *       user/desktop/src/apps/greeter.rs
 *       SPUI DDM greeter: full-screen login, session picker and power actions
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The greeter is an unprivileged X client.  It renders account and session
//! choices from DDM's configuration, sends password bytes over the local
//! socket, and exits as soon as the daemon reports a successful login.  It
//! never reads `/etc/shadow` and never starts a process as another user.

use crate::apps::session_ui::{draw_error, draw_logo, user_initial, Backend, Message, Screen};
use crate::ddm::wire::{ClientKind, ErrorCode, PowerAction};
use crate::draw::Rect;
use crate::text::Face;
use crate::ui;
use crate::x11::{Key, Result};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use x11rb::protocol::Event;

const CARD_WIDTH: i32 = 980;
const CARD_HEIGHT: i32 = 560;
const LEFT_WIDTH: i32 = 344;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Username,
    Password,
    Show,
    Session(usize),
    Login,
    User(usize),
    Reboot,
    PowerOff,
    None,
}

struct Greeter {
    screen: Screen,
    backend: Backend,
    username: String,
    password: String,
    session: String,
    selected_user: Option<usize>,
    focus: Focus,
    hover: Focus,
    error: String,
    waiting: bool,
    show_password: bool,
    caps_lock: bool,
    confirm_power: Option<PowerAction>,
    clock: String,
    date: String,
    last_clock: Instant,
}

pub fn run(arguments: &[String]) -> Result<()> {
    let socket = socket_argument(arguments).unwrap_or_else(|| "/run/ddm/greeter.sock".to_string());
    let backend = Backend::connect(&socket, ClientKind::Greeter)?;
    if backend.config.mode != ClientKind::Greeter {
        return Err("DDM is in first-boot setup mode".into());
    }
    let screen = Screen::new(&backend.config.wallpaper)?;
    let mut greeter = Greeter::new(screen, backend);
    greeter.loop_until_done()
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

impl Greeter {
    fn new(screen: Screen, backend: Backend) -> Self {
        let username = if backend
            .config
            .users
            .iter()
            .any(|user| user.name == backend.config.last_user)
        {
            backend.config.last_user.clone()
        } else {
            backend
                .config
                .users
                .first()
                .map(|user| user.name.clone())
                .unwrap_or_default()
        };
        let selected_user = backend
            .config
            .users
            .iter()
            .position(|user| user.name == username);
        let session = if backend
            .config
            .sessions
            .iter()
            .any(|session| session.id == backend.config.last_session)
        {
            backend.config.last_session.clone()
        } else {
            backend
                .config
                .sessions
                .first()
                .map(|session| session.id.clone())
                .unwrap_or_default()
        };
        let (clock, date) = clock_strings();
        Self {
            screen,
            backend,
            username,
            password: String::new(),
            session,
            selected_user,
            focus: if selected_user.is_some() {
                Focus::Password
            } else {
                Focus::Username
            },
            hover: Focus::None,
            error: String::new(),
            waiting: false,
            show_password: false,
            caps_lock: false,
            confirm_power: None,
            clock,
            date,
            last_clock: Instant::now(),
        }
    }

    fn loop_until_done(&mut self) -> Result<()> {
        loop {
            if self.screen.needs_redraw {
                self.draw();
                self.screen.present()?;
            }
            if self.last_clock.elapsed() >= Duration::from_secs(1) {
                let (clock, date) = clock_strings();
                if clock != self.clock || date != self.date {
                    self.clock = clock;
                    self.date = date;
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
                        self.handle_click(
                            button.event_x as i32,
                            button.event_y as i32,
                            button.detail,
                        );
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
            Message::LoginOk { .. } => return Ok(true),
            Message::PowerOk => return Ok(true),
            Message::Pong | Message::Config(_) => {}
            Message::Error {
                code,
                message,
                retry_ms,
            } => {
                self.waiting = false;
                self.error = if retry_ms > 0 {
                    let seconds = (retry_ms + 999) / 1000;
                    format!("{message}（{} 秒后可重试）", seconds)
                } else {
                    message
                };
                if code == ErrorCode::Protocol as u16 {
                    return Err(self.error.clone().into());
                }
                self.password.clear();
                self.focus = Focus::Password;
                self.screen.needs_redraw = true;
            }
            Message::OobeOk { .. } => {
                return Err("DDM returned an OOBE response to the greeter".into())
            }
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
        if self.confirm_power.is_some() {
            if key.is_escape() {
                self.confirm_power = None;
                self.screen.needs_redraw = true;
            } else if key.is_enter() {
                self.confirm_power_action();
            }
            return;
        }
        if key.is_tab() {
            self.cycle_focus(!key.shift());
            self.screen.needs_redraw = true;
            return;
        }
        if key.is_enter() {
            self.submit();
            return;
        }
        if key.is_escape() {
            if !self.error.is_empty() {
                self.error.clear();
                self.screen.needs_redraw = true;
            }
            return;
        }
        if key.is_backspace() {
            match self.focus {
                Focus::Username => {
                    self.username.pop();
                }
                Focus::Password => {
                    self.password.pop();
                }
                _ => {}
            }
            self.screen.needs_redraw = true;
            return;
        }
        if let Some(ch) = key.text().filter(|ch| !ch.is_control()) {
            match self.focus {
                Focus::Username if self.username.chars().count() < 32 => {
                    self.username.push(ch);
                    self.error.clear();
                    self.screen.needs_redraw = true;
                }
                Focus::Password if self.password.chars().count() < 128 => {
                    self.password.push(ch);
                    self.error.clear();
                    self.screen.needs_redraw = true;
                }
                _ => {}
            }
        }
    }

    fn cycle_focus(&mut self, forward: bool) {
        let mut targets = vec![Focus::Username, Focus::Password, Focus::Show];
        for index in 0..self.backend.config.sessions.len() {
            targets.push(Focus::Session(index));
        }
        targets.push(Focus::Login);
        for index in 0..self.backend.config.users.len() {
            targets.push(Focus::User(index));
        }
        if self.backend.config.allow_power {
            targets.push(Focus::Reboot);
            targets.push(Focus::PowerOff);
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

    fn handle_click(&mut self, x: i32, y: i32, button: u8) {
        if button != 1 {
            return;
        }
        if self.confirm_power.is_some() {
            let card = self.confirm_card();
            if self.confirm_cancel_rect(card).contains(x, y) {
                self.confirm_power = None;
            } else if self.confirm_ok_rect(card).contains(x, y) {
                self.confirm_power_action();
            }
            self.screen.needs_redraw = true;
            return;
        }
        let layout = self.layout();
        if layout.username.contains(x, y) {
            self.focus = Focus::Username;
        } else if layout.password.contains(x, y) {
            self.focus = Focus::Password;
        } else if layout.show.contains(x, y) {
            self.show_password = !self.show_password;
        } else if layout.login.contains(x, y) {
            self.submit();
            return;
        } else if layout.reboot.contains(x, y) {
            self.confirm_power = Some(PowerAction::Reboot);
        } else if layout.power_off.contains(x, y) {
            self.confirm_power = Some(PowerAction::PowerOff);
        } else {
            for (index, rect) in layout.user_rows.iter().enumerate() {
                if rect.contains(x, y) {
                    self.select_user(index);
                    break;
                }
            }
            for (index, rect) in layout.session_chips.iter().enumerate() {
                if rect.contains(x, y) {
                    if let Some(session) = self.backend.config.sessions.get(index) {
                        self.session = session.id.clone();
                    }
                    self.focus = Focus::Session(index);
                    break;
                }
            }
        }
        self.screen.needs_redraw = true;
    }

    fn handle_motion(&mut self, x: i32, y: i32) {
        let layout = self.layout();
        let hover = if layout.username.contains(x, y) {
            Focus::Username
        } else if layout.password.contains(x, y) {
            Focus::Password
        } else if layout.show.contains(x, y) {
            Focus::Show
        } else if layout.login.contains(x, y) {
            Focus::Login
        } else if layout.reboot.contains(x, y) {
            Focus::Reboot
        } else if layout.power_off.contains(x, y) {
            Focus::PowerOff
        } else if let Some(index) = layout.user_rows.iter().position(|rect| rect.contains(x, y)) {
            Focus::User(index)
        } else if let Some(index) = layout
            .session_chips
            .iter()
            .position(|rect| rect.contains(x, y))
        {
            Focus::Session(index)
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

    fn select_user(&mut self, index: usize) {
        if let Some(user) = self.backend.config.users.get(index) {
            self.username = user.name.clone();
            self.selected_user = Some(index);
            self.password.clear();
            self.error.clear();
            self.focus = Focus::Password;
        }
    }

    fn submit(&mut self) {
        if self.waiting {
            return;
        }
        if self.username.trim().is_empty() {
            self.error = "请输入用户名".to_string();
            self.focus = Focus::Username;
            self.screen.needs_redraw = true;
            return;
        }
        self.error.clear();
        self.waiting = true;
        self.screen.needs_redraw = true;
        if let Err(error) = self.backend.login(
            self.username.trim(),
            self.password.as_bytes(),
            &self.session,
        ) {
            self.waiting = false;
            self.error = error.to_string();
        }
    }

    fn confirm_power_action(&mut self) {
        if let Some(action) = self.confirm_power.take() {
            self.waiting = true;
            if let Err(error) = self.backend.power(action) {
                self.waiting = false;
                self.error = error.to_string();
            }
            self.screen.needs_redraw = true;
        }
    }

    fn layout(&self) -> Layout {
        let card = Rect::new(
            (self.screen.width - CARD_WIDTH) / 2,
            (self.screen.height - CARD_HEIGHT) / 2,
            CARD_WIDTH,
            CARD_HEIGHT,
        );
        let right_x = card.x + LEFT_WIDTH + 40;
        let right_w = card.w - LEFT_WIDTH - 72;
        let user_rows = (0..self.backend.config.users.len().min(4))
            .map(|index| {
                Rect::new(
                    card.x + 24,
                    card.y + 196 + index as i32 * 58,
                    LEFT_WIDTH - 48,
                    50,
                )
            })
            .collect();
        let mut session_chips = Vec::new();
        let mut chip_x = right_x;
        for (index, _) in self.backend.config.sessions.iter().enumerate().take(3) {
            let width = 132;
            session_chips.push(Rect::new(chip_x, card.y + 328, width, 32));
            chip_x += width + 8;
            let _ = index;
        }
        Layout {
            card,
            username: Rect::new(right_x, card.y + 190, right_w, 38),
            password: Rect::new(right_x, card.y + 258, right_w, 38),
            show: Rect::new(right_x + right_w - 42, card.y + 266, 30, 22),
            login: Rect::new(right_x, card.y + 404, 150, 40),
            reboot: Rect::new(card.right() - 204, card.bottom() - 54, 86, 32),
            power_off: Rect::new(card.right() - 108, card.bottom() - 54, 86, 32),
            user_rows,
            session_chips,
        }
    }

    fn confirm_card(&self) -> Rect {
        Rect::new(
            (self.screen.width - 430) / 2,
            (self.screen.height - 210) / 2,
            430,
            210,
        )
    }

    fn confirm_cancel_rect(&self, card: Rect) -> Rect {
        Rect::new(card.right() - 208, card.bottom() - 58, 90, 34)
    }

    fn confirm_ok_rect(&self, card: Rect) -> Rect {
        Rect::new(card.right() - 108, card.bottom() - 58, 90, 34)
    }

    fn draw(&mut self) {
        let theme = self.screen.theme;
        let layout = self.layout();
        self.screen.draw_background();

        // Clock and host identity.
        self.screen.text.draw_right(
            &mut self.screen.canvas,
            &self.clock,
            (self.screen.width - 42) as f32,
            44.0,
            27.0,
            theme.text,
            Face::Bold,
        );
        self.screen.text.draw_right(
            &mut self.screen.canvas,
            &self.date,
            (self.screen.width - 42) as f32,
            66.0,
            11.5,
            theme.text_muted,
            Face::Ui,
        );

        // Card surface.
        self.screen
            .canvas
            .rounded_rect(layout.card, 12, theme.window, 0.96);
        self.screen
            .canvas
            .rounded_border(layout.card, 12, theme.window_border, 1.0, 1);
        let left = Rect::new(layout.card.x, layout.card.y, LEFT_WIDTH, layout.card.h);
        self.screen
            .canvas
            .rounded_rect(left, 12, theme.window_alt, 0.92);
        self.screen.canvas.fill(
            Rect::new(left.right() - 1, left.y + 1, 1, left.h - 2),
            theme.window_border,
        );

        draw_logo(&mut self.screen, left.x + 32, left.y + 30, 42);
        self.screen.text.draw(
            &mut self.screen.canvas,
            "SpaceOS 9",
            (left.x + 84) as f32,
            (left.y + 51) as f32,
            20.0,
            theme.text,
            Face::Bold,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "SpaceOS Display Manager",
            (left.x + 84) as f32,
            (left.y + 70) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "欢迎",
            (left.x + 32) as f32,
            (left.y + 126) as f32,
            24.0,
            theme.text,
            Face::Bold,
        );
        let host = self.screen.text.truncate(
            &self.backend.config.hostname,
            (LEFT_WIDTH - 64) as f32,
            12.0,
            Face::Mono,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            &host,
            (left.x + 32) as f32,
            (left.y + 150) as f32,
            12.0,
            theme.text_muted,
            Face::Mono,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "选择一个账户登录",
            (left.x + 32) as f32,
            (left.y + 178) as f32,
            11.5,
            theme.text_dim,
            Face::Ui,
        );

        if self.backend.config.users.is_empty() {
            self.screen.text.draw(
                &mut self.screen.canvas,
                "没有可登录的本地账户",
                (left.x + 32) as f32,
                (left.y + 224) as f32,
                12.0,
                theme.warning,
                Face::Ui,
            );
            self.screen.text.draw(
                &mut self.screen.canvas,
                "请从串口控制台创建账户",
                (left.x + 32) as f32,
                (left.y + 246) as f32,
                11.0,
                theme.text_muted,
                Face::Ui,
            );
        }

        for (index, user) in self.backend.config.users.iter().take(4).enumerate() {
            let row = layout.user_rows[index];
            let selected = self.selected_user == Some(index);
            if selected {
                self.screen
                    .canvas
                    .rounded_rect(row, 8, theme.accent_soft, 1.0);
            } else if self.hover == Focus::User(index) {
                self.screen
                    .canvas
                    .rounded_rect(row, 8, theme.panel_hover, 1.0);
            }
            self.screen.canvas.circle(
                (row.x + 26) as f32,
                (row.y + 25) as f32,
                18.0,
                theme.panel_high,
                1.0,
            );
            let initial = user_initial(&user.display, &user.name);
            self.screen.text.draw_centered(
                &mut self.screen.canvas,
                &initial,
                (row.x + 26) as f32,
                (row.y + 30) as f32,
                14.0,
                theme.text,
                Face::Bold,
            );
            let display =
                self.screen
                    .text
                    .truncate(&user.display, (row.w - 70) as f32, 12.5, Face::Bold);
            self.screen.text.draw(
                &mut self.screen.canvas,
                &display,
                (row.x + 52) as f32,
                (row.y + 21) as f32,
                12.5,
                theme.text,
                Face::Bold,
            );
            self.screen.text.draw(
                &mut self.screen.canvas,
                &user.name,
                (row.x + 52) as f32,
                (row.y + 38) as f32,
                10.5,
                theme.text_muted,
                Face::Mono,
            );
        }

        // Right-hand login form.
        let right_x = layout.username.x;
        self.screen.text.draw(
            &mut self.screen.canvas,
            "登录到 SpaceOS",
            right_x as f32,
            (layout.card.y + 50) as f32,
            23.0,
            theme.text,
            Face::Bold,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "输入账户凭据以启动桌面会话",
            right_x as f32,
            (layout.card.y + 72) as f32,
            11.5,
            theme.text_muted,
            Face::Ui,
        );

        let avatar = Rect::new(right_x, layout.card.y + 108, 56, 56);
        self.screen.canvas.circle(
            (avatar.x + 28) as f32,
            (avatar.y + 28) as f32,
            28.0,
            theme.panel_high,
            1.0,
        );
        let initial = user_initial(
            &self
                .backend
                .config
                .users
                .get(self.selected_user.unwrap_or(0))
                .map(|user| user.display.clone())
                .unwrap_or_default(),
            &self.username,
        );
        self.screen.text.draw_centered(
            &mut self.screen.canvas,
            &initial,
            (avatar.x + 28) as f32,
            (avatar.y + 36) as f32,
            20.0,
            theme.text,
            Face::Bold,
        );
        let display_name = self
            .backend
            .config
            .users
            .iter()
            .find(|user| user.name == self.username.trim())
            .map(|user| user.display.clone())
            .unwrap_or_else(|| {
                if self.username.trim().is_empty() {
                    "本地账户".to_string()
                } else {
                    self.username.trim().to_string()
                }
            });
        let display_name = self.screen.text.truncate(
            &display_name,
            (layout.username.w - 76) as f32,
            15.0,
            Face::Bold,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            &display_name,
            (avatar.right() + 18) as f32,
            (avatar.y + 24) as f32,
            15.0,
            theme.text,
            Face::Bold,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            &self.username,
            (avatar.right() + 18) as f32,
            (avatar.y + 42) as f32,
            10.5,
            theme.text_muted,
            Face::Mono,
        );

        self.screen.text.draw(
            &mut self.screen.canvas,
            "用户名",
            right_x as f32,
            (layout.username.y - 10) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
        ui::text_input(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.username,
            &self.username,
            "输入用户名",
            self.focus == Focus::Username,
            None,
        );

        self.screen.text.draw(
            &mut self.screen.canvas,
            "密码",
            right_x as f32,
            (layout.password.y - 10) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
        let password_display = if self.show_password {
            self.password.clone()
        } else {
            let count = self.password.chars().count().min(24);
            "•".repeat(count)
        };
        ui::text_input(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            layout.password,
            &password_display,
            "输入密码",
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

        self.screen.text.draw(
            &mut self.screen.canvas,
            "会话",
            right_x as f32,
            (layout.card.y + 316) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
        for (index, session) in self.backend.config.sessions.iter().take(3).enumerate() {
            let rect = layout.session_chips[index];
            let active = session.id == self.session;
            self.screen.canvas.rounded_rect(
                rect,
                6,
                if active {
                    theme.accent_soft
                } else {
                    theme.panel_high
                },
                1.0,
            );
            if active {
                self.screen
                    .canvas
                    .rounded_border(rect, 6, theme.accent, 0.8, 1);
            }
            let name =
                self.screen
                    .text
                    .truncate(&session.name, (rect.w - 16) as f32, 10.5, Face::Ui);
            self.screen.text.draw(
                &mut self.screen.canvas,
                &name,
                (rect.x + 8) as f32,
                (rect.y + 20) as f32,
                10.5,
                if active { theme.text } else { theme.text_dim },
                Face::Ui,
            );
        }

        let error_rect = Rect::new(right_x, layout.card.y + 372, layout.username.w, 24);
        draw_error(&mut self.screen, error_rect, &self.error);
        if self.caps_lock {
            self.screen.text.draw(
                &mut self.screen.canvas,
                "大写锁定已开启",
                (right_x + 160) as f32,
                (layout.login.y + 25) as f32,
                10.5,
                theme.warning,
                Face::Ui,
            );
        }

        if self.waiting {
            self.screen.text.draw(
                &mut self.screen.canvas,
                "正在验证…",
                (layout.login.x + 170) as f32,
                (layout.login.y + 25) as f32,
                11.5,
                theme.text_muted,
                Face::Ui,
            );
        } else {
            ui::button(
                &mut self.screen.canvas,
                &mut self.screen.text,
                &theme,
                layout.login,
                "登录",
                None,
                true,
                self.hover == Focus::Login || self.focus == Focus::Login,
            );
        }

        if self.backend.config.allow_power {
            ui::button(
                &mut self.screen.canvas,
                &mut self.screen.text,
                &theme,
                layout.reboot,
                "重启",
                None,
                false,
                self.hover == Focus::Reboot,
            );
            ui::button(
                &mut self.screen.canvas,
                &mut self.screen.text,
                &theme,
                layout.power_off,
                "关机",
                None,
                false,
                self.hover == Focus::PowerOff,
            );
        }

        self.screen.text.draw(
            &mut self.screen.canvas,
            &format!(
                "DDM {} · {}",
                self.backend.config.version, self.backend.config.timezone
            ),
            right_x as f32,
            (layout.card.bottom() - 30) as f32,
            9.5,
            theme.text_muted,
            Face::Mono,
        );

        if let Some(action) = self.confirm_power {
            self.draw_power_confirm(action);
        }
    }

    fn draw_power_confirm(&mut self, action: PowerAction) {
        let theme = self.screen.theme;
        let card = self.confirm_card();
        self.screen
            .canvas
            .fill_alpha(self.screen.canvas.bounds(), 0x000000, 0.58);
        self.screen.canvas.rounded_rect(card, 10, theme.window, 1.0);
        self.screen
            .canvas
            .rounded_border(card, 10, theme.window_border, 1.0, 1);
        let title = match action {
            PowerAction::Reboot => "重新启动计算机？",
            PowerAction::PowerOff => "关闭计算机？",
        };
        self.screen.text.draw(
            &mut self.screen.canvas,
            title,
            (card.x + 24) as f32,
            (card.y + 48) as f32,
            18.0,
            theme.text,
            Face::Bold,
        );
        self.screen.text.draw(
            &mut self.screen.canvas,
            "所有未保存的工作都将丢失。",
            (card.x + 24) as f32,
            (card.y + 76) as f32,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
        let cancel = self.confirm_cancel_rect(card);
        let ok = self.confirm_ok_rect(card);
        ui::button(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            cancel,
            "取消",
            None,
            false,
            false,
        );
        ui::button(
            &mut self.screen.canvas,
            &mut self.screen.text,
            &theme,
            ok,
            if action == PowerAction::Reboot {
                "重启"
            } else {
                "关机"
            },
            None,
            true,
            true,
        );
    }
}

struct Layout {
    card: Rect,
    username: Rect,
    password: Rect,
    show: Rect,
    login: Rect,
    reboot: Rect,
    power_off: Rect,
    user_rows: Vec<Rect>,
    session_chips: Vec<Rect>,
}

fn clock_strings() -> (String, String) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&seconds, &mut tm);
    }
    let weekdays = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
    let weekday = weekdays.get(tm.tm_wday.clamp(0, 6) as usize).unwrap_or(&"");
    (
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min),
        format!(
            "{}年{}月{}日 {}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            weekday
        ),
    )
}
