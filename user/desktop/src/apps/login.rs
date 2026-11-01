/*
 *
 *       user/desktop/src/apps/login.rs
 *       SPUI 4.0 local session and Unix account manager
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Displays active local session identity (`uid`/`gid` and `/etc/passwd`
//! accounts) in a compact native dialog without dark-navy splash gradients.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::{self, Icon};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::Result;
use std::fs;
use std::process::Command;

struct AccountInfo {
    name: String,
    uid: u32,
    home: String,
    shell: String,
}

fn read_accounts() -> Vec<AccountInfo> {
    let raw = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let mut out = Vec::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 7 {
            if let Ok(uid) = parts[2].parse::<u32>() {
                out.push(AccountInfo {
                    name: parts[0].to_string(),
                    uid,
                    home: parts[5].to_string(),
                    shell: parts[6].to_string(),
                });
            }
        }
    }
    if out.is_empty() {
        out.push(AccountInfo {
            name: "root".to_string(),
            uid: 0,
            home: "/root".to_string(),
            shell: "/bin/sh".to_string(),
        });
    }
    out
}

fn input_rect(w: i32) -> Rect {
    Rect::new(22, TITLEBAR_HEIGHT + 194, w - 134, 32)
}

fn action_btn_rect(w: i32) -> Rect {
    Rect::new(w - 102, TITLEBAR_HEIGHT + 194, 80, 32)
}

pub fn run() -> Result<()> {
    let mut app = App::new("会话账户", Icon::Login, 500, 340)?;
    app.min_width = 440;
    app.min_height = 320;

    let euid = unsafe { libc::geteuid() };
    let accounts = read_accounts();
    let current_user = accounts
        .iter()
        .find(|a| a.uid == euid)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| format!("uid:{euid}"));

    let mut query = current_user.clone();
    let mut focused = true;
    let mut btn_hover = false;
    let mut status = format!("当前活动桌面会话身份：{current_user} (UID {euid})");

    loop {
        if app.close {
            break;
        }
        for event in app.pump()? {
            let w = app.width as i32;
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    let h = action_btn_rect(w).contains(x, y);
                    if h != btn_hover {
                        btn_hover = h;
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
                    focused = input_rect(w).contains(x, y);
                    if action_btn_rect(w).contains(x, y) {
                        status = inspect_or_open(&accounts, query.trim(), euid);
                    }
                    app.needs_redraw = true;
                }
                UiEvent::Key(key) => {
                    if key.is_backspace() && focused {
                        query.pop();
                        app.needs_redraw = true;
                    } else if key.is_enter() {
                        status = inspect_or_open(&accounts, query.trim(), euid);
                        app.needs_redraw = true;
                    } else if focused {
                        if let Some(ch) = key.text().filter(|c| !c.is_control()) {
                            query.push(ch);
                            app.needs_redraw = true;
                        }
                    }
                }
                _ => {}
            }
        }

        if app.needs_redraw {
            draw_login(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &accounts,
                euid,
                &query,
                focused,
                btn_hover,
                &status,
            );
            app.draw_chrome();
            app.present()?;
        }
    }
    Ok(())
}

fn inspect_or_open(accounts: &[AccountInfo], target: &str, euid: u32) -> String {
    if target.is_empty() {
        return "请输入系统账户名".to_string();
    }
    if let Some(acc) = accounts.iter().find(|a| a.name == target) {
        if acc.uid == euid {
            if let Ok(exe) = std::env::current_exe() {
                let _ = Command::new(exe).arg("term").arg(&acc.home).spawn();
            }
            format!(
                "已为当前会话账户 {} 打开主目录终端 ({})",
                acc.name, acc.home
            )
        } else {
            format!(
                "账户 {} (UID {}, 主目录 {}, Shell {})",
                acc.name, acc.uid, acc.home, acc.shell
            )
        }
    } else {
        format!("未在 /etc/passwd 中找到账户 \"{target}\"")
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_login(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    accounts: &[AccountInfo],
    euid: u32,
    query: &str,
    focused: bool,
    btn_hover: bool,
    status: &str,
) {
    let w = canvas.width as i32;
    canvas.clear(theme.window);

    let card = Rect::new(18, TITLEBAR_HEIGHT + 16, w - 36, 152);
    ui::card(canvas, theme, card);
    icons::draw(canvas, Icon::Login, card.x + 16, card.y + 16, 36, None);
    text.draw(
        canvas,
        "本地会话账户",
        (card.x + 64) as f32,
        (card.y + 32) as f32,
        15.0,
        theme.text,
        Face::Bold,
    );
    text.draw(
        canvas,
        "系统已配置的本地 Unix 用户列表 (/etc/passwd)",
        (card.x + 64) as f32,
        (card.y + 52) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    for (idx, acc) in accounts.iter().take(3).enumerate() {
        let ry = card.y + 68 + idx as i32 * 26;
        let is_cur = acc.uid == euid;
        let line = format!(
            "{}  (UID {})  ·  {}  ·  {}",
            acc.name, acc.uid, acc.home, acc.shell
        );
        text.draw(
            canvas,
            &line,
            (card.x + 18) as f32,
            (ry + 16) as f32,
            12.0,
            if is_cur { theme.text } else { theme.text_dim },
            Face::Mono,
        );
        if is_cur {
            text.draw_right(
                canvas,
                "当前会话",
                (card.right() - 16) as f32,
                (ry + 16) as f32,
                11.0,
                theme.accent,
                Face::Bold,
            );
        }
    }

    text.draw(
        canvas,
        "查询账户或打开终端",
        22.0,
        (TITLEBAR_HEIGHT + 186) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );
    ui::text_input(
        canvas,
        text,
        theme,
        input_rect(w),
        query,
        "输入账户名...",
        focused,
        None,
    );
    ui::button(
        canvas,
        text,
        theme,
        action_btn_rect(w),
        "打开",
        None,
        true,
        btn_hover,
    );

    let shown = text.truncate(status, (w - 44) as f32, 11.5, Face::Ui);
    text.draw(
        canvas,
        &shown,
        22.0,
        (TITLEBAR_HEIGHT + 252) as f32,
        11.5,
        theme.text_dim,
        Face::Ui,
    );
}
