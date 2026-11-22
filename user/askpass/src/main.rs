/*
 *
 *       user/askpass/src/main.rs
 *       SPUI 4.0 System-wide graphical authentication helper (`spui-askpass`)
 *
 *       2026/11/22 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Standalone unprivileged GUI authentication dialog used by `sudo -A` (`spui-askpass`)
//! and desktop administrative utilities. Uses `spui_ui::ui` semantic surfaces with
//! solid opaque panels so content remains crisp across all SpaceOS themes.

use spui_ui::app::{App, UiEvent};
use spui_ui::draw::{mix, Canvas, Rect};
use spui_ui::icons::{self, Icon};
use spui_ui::text::{Face, TextRenderer};
use spui_ui::theme::Theme;
use spui_ui::ui;
use spui_ui::x11::{CursorKind, Key, Result};
use std::fs;
use std::io::Write;
use std::process::ExitCode;

struct AuthContext {
    username: String,
    title: String,
    command: String,
    error: Option<String>,
    attempt_label: Option<String>,
}

fn current_username() -> String {
    // SAFETY: reading real UID of invoking user process.
    let uid = unsafe { libc::getuid() };
    if let Ok(raw) = fs::read_to_string("/etc/passwd") {
        for line in raw.lines() {
            let mut cols = line.split(':');
            let Some(name) = cols.next() else { continue };
            let _ = cols.next();
            if let Some(uid_str) = cols.next() {
                if uid_str.parse::<u32>().ok() == Some(uid) {
                    return name.to_string();
                }
            }
        }
    }
    std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("uid:{uid}"))
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn clean_prompt_arg(raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_end_matches(['：', ':'])
        .trim();
    if trimmed.is_empty()
        || trimmed.starts_with("为安全起见")
        || trimmed.eq_ignore_ascii_case("password")
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn format_command_preview(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("sudo ") || trimmed == "sudo" {
        trimmed.to_string()
    } else {
        format!("sudo {trimmed}")
    }
}

fn load_context(args: &[String]) -> AuthContext {
    let title = env_non_empty("SPUI_AUTH_TITLE")
        .or_else(|| args.first().and_then(|a| clean_prompt_arg(a)))
        .unwrap_or_else(|| "需要管理员权限以继续当前操作".into());
    let command = env_non_empty("SPUI_AUTH_CMD").unwrap_or_else(|| {
        if args.len() > 1 {
            args[1..].join(" ")
        } else {
            String::new()
        }
    });
    let error = env_non_empty("SPUI_AUTH_ERROR");
    let attempt_label = match (
        env_non_empty("SPUI_AUTH_ATTEMPT"),
        env_non_empty("SPUI_AUTH_MAX_ATTEMPTS"),
    ) {
        (Some(cur), Some(max)) => Some(format!("第 {cur}/{max} 次验证")),
        _ => None,
    };
    AuthContext {
        username: current_username(),
        title,
        command,
        error,
        attempt_label,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    None,
    Input,
    RevealToggle,
    Cancel,
    Submit,
}

fn layout(content: Rect, ctx: &AuthContext) -> AuthLayout {
    let pad = 22;
    let body_w = (content.w - pad * 2).max(200);
    let has_cmd = !ctx.command.is_empty();
    let has_err = ctx.error.is_some();

    let header_y = content.y + 18;
    let cmd_y = header_y + 56;
    let cmd_h = if has_cmd { 46 } else { 0 };
    let input_label_y = if has_cmd { cmd_y + cmd_h + 14 } else { cmd_y + 4 };
    let input_y = input_label_y + 8;
    let reveal_w = 56;
    let input_rect = Rect::new(content.x + pad, input_y, (body_w - reveal_w - 8).max(120), 34);
    let reveal_rect = Rect::new(input_rect.right() + 8, input_y, reveal_w, 34);
    let err_rect = if has_err {
        Rect::new(content.x + pad, input_y + 44, body_w, 42)
    } else {
        Rect::new(0, 0, 0, 0)
    };
    let footer_y = content.bottom() - 48;
    let cancel_rect = Rect::new(content.right() - pad - 196, footer_y, 92, 32);
    let submit_rect = Rect::new(content.right() - pad - 96, footer_y, 96, 32);

    AuthLayout {
        pad,
        header_y,
        cmd_rect: Rect::new(content.x + pad, cmd_y, body_w, cmd_h),
        input_label_y,
        input_rect,
        reveal_rect,
        err_rect,
        cancel_rect,
        submit_rect,
    }
}

struct AuthLayout {
    pad: i32,
    header_y: i32,
    cmd_rect: Rect,
    input_label_y: i32,
    input_rect: Rect,
    reveal_rect: Rect,
    err_rect: Rect,
    cancel_rect: Rect,
    submit_rect: Rect,
}

fn hit_test(l: &AuthLayout, x: i32, y: i32) -> Hit {
    if l.submit_rect.contains(x, y) {
        Hit::Submit
    } else if l.cancel_rect.contains(x, y) {
        Hit::Cancel
    } else if l.reveal_rect.contains(x, y) {
        Hit::RevealToggle
    } else if l.input_rect.contains(x, y) {
        Hit::Input
    } else {
        Hit::None
    }
}

fn zero_string(s: &mut String) {
    // SAFETY: zeros raw UTF-8 buffer bytes before dropping/clearing password string.
    unsafe {
        s.as_mut_vec().fill(0);
    }
    s.clear();
}

fn wrap_lines(text: &mut TextRenderer, s: &str, max_w: f32, px: f32, face: Face, limit: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch == '\n' {
            lines.push(std::mem::take(&mut cur));
            if lines.len() >= limit {
                return lines;
            }
            continue;
        }
        let mut probe = cur.clone();
        probe.push(ch);
        if text.measure(&probe, px, face) > max_w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            if lines.len() >= limit {
                if let Some(last) = lines.last_mut() {
                    *last = text.truncate(last, max_w, px, face);
                    if !last.ends_with('…') {
                        last.pop();
                        last.push('…');
                    }
                }
                return lines;
            }
        }
        cur.push(ch);
    }
    if !cur.is_empty() && lines.len() < limit {
        lines.push(cur);
    }
    lines
}

pub fn run(args: &[String]) -> Result<bool> {
    let ctx = load_context(args);
    let height = 256
        + if ctx.command.is_empty() { 0 } else { 58 }
        + if ctx.error.is_some() { 54 } else { 0 };
    let mut app = App::new("管理员身份验证", Icon::Shield, 468, height)?;
    app.min_width = 420;
    app.min_height = 240;
    // Never engage Chinese IME preedit in a password field.
    app.set_ime_focus(false);

    let mut password = String::with_capacity(64);
    let mut focused = true;
    let mut reveal = false;
    let mut hover = Hit::None;
    let mut accepted = false;

    loop {
        if app.close {
            break;
        }
        let events = app.pump()?;
        let content = app.content();
        let lay = layout(content, &ctx);

        for event in events {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, state } => {
                    ui::notify_pointer_motion(x, y, state & 0x100 != 0);
                    let next = hit_test(&lay, x, y);
                    let cursor = match next {
                        Hit::Input => CursorKind::Text,
                        Hit::RevealToggle | Hit::Cancel | Hit::Submit => CursorKind::Pointer,
                        Hit::None => CursorKind::Default,
                    };
                    app.set_content_cursor(cursor);
                    if next != hover {
                        hover = next;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x,
                    y,
                    button,
                    down,
                    ..
                } => {
                    ui::notify_pointer_button(x, y, button, down);
                    if button == 1 && down {
                        let hit = hit_test(&lay, x, y);
                        focused = hit == Hit::Input;
                        app.set_ime_focus(false);
                        app.needs_redraw = true;
                    } else if button == 1 && !down {
                        match hit_test(&lay, x, y) {
                            Hit::RevealToggle => {
                                reveal = !reveal;
                                focused = true;
                                app.needs_redraw = true;
                            }
                            Hit::Cancel => {
                                accepted = false;
                                app.request_close();
                            }
                            Hit::Submit => {
                                accepted = true;
                                app.request_close();
                            }
                            Hit::Input => {
                                focused = true;
                                app.needs_redraw = true;
                            }
                            Hit::None => {}
                        }
                    }
                }
                UiEvent::CommitText(s) => {
                    if focused {
                        password.push_str(&s);
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Key(key) => {
                    if handle_auth_key(key, &mut password, &mut focused, &mut reveal) {
                        app.needs_redraw = true;
                    } else if key.is_enter() {
                        accepted = true;
                        app.request_close();
                    } else if key.is_escape() {
                        accepted = false;
                        app.request_close();
                    }
                }
                _ => {}
            }
        }

        if app.needs_redraw {
            draw_dialog(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                content,
                &ctx,
                &lay,
                &password,
                focused,
                reveal,
                hover,
            );
            app.draw_chrome();
            app.present()?;
        }
    }

    if accepted {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{password}");
        let _ = out.flush();
    }
    zero_string(&mut password);
    Ok(accepted)
}

fn handle_auth_key(key: Key, password: &mut String, focused: &mut bool, reveal: &mut bool) -> bool {
    if key.is_enter() || key.is_escape() {
        return false;
    }
    if key.is_backspace() || key.sym == 0xff08 {
        if *focused {
            password.pop();
        }
        return true;
    }
    if key.control() {
        if let Some(ch) = key.text() {
            match ch.to_ascii_lowercase() {
                'u' | 'a' | 'k' => {
                    zero_string(password);
                    return true;
                }
                'r' => {
                    *reveal = !*reveal;
                    return true;
                }
                _ => {}
            }
        }
        return false;
    }
    if !key.alt() {
        if let Some(ch) = key.text() {
            if !ch.is_control() {
                *focused = true;
                password.push(ch);
                return true;
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn draw_dialog(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    content: Rect,
    ctx: &AuthContext,
    lay: &AuthLayout,
    password: &str,
    focused: bool,
    reveal: bool,
    hover: Hit,
) {
    // Solid opaque backing across the full client area even under composited themes.
    canvas.clear(theme.window);
    canvas.fill(content, theme.window);

    let radius = theme.radius_control().max(6);
    let badge_rect = Rect::new(content.x + lay.pad, lay.header_y, 44, 44);
    let badge_top = mix(theme.accent, 0x60a5fa, 0.35);
    let badge_bot = mix(theme.accent, 0x1e3a8a, 0.55);
    canvas.rounded_gradient(badge_rect, 11, badge_top, badge_bot, 1.0);
    icons::draw(
        canvas,
        Icon::Shield,
        badge_rect.x + 8,
        badge_rect.y + 8,
        28,
        None,
    );

    let tx = badge_rect.right() + 14;
    let tw = (content.right() - lay.pad - tx).max(120) as f32;
    let title = text.truncate(&ctx.title, tw, 15.0, Face::Bold);
    text.draw(
        canvas,
        &title,
        tx as f32,
        (lay.header_y + 18) as f32,
        15.0,
        theme.text,
        Face::Bold,
    );
    let mut sub = format!("当前用户：{}", ctx.username);
    if let Some(att) = &ctx.attempt_label {
        sub.push_str("  ·  ");
        sub.push_str(att);
    }
    let sub_t = text.truncate(&sub, tw, 11.5, Face::Ui);
    text.draw(
        canvas,
        &sub_t,
        tx as f32,
        (lay.header_y + 37) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    if !ctx.command.is_empty() {
        canvas.rounded_rect(lay.cmd_rect, radius, theme.panel, 1.0);
        canvas.rounded_border(lay.cmd_rect, radius, theme.panel_border, 1.0, 1);
        text.draw(
            canvas,
            "将执行授权命令",
            (lay.cmd_rect.x + 12) as f32,
            (lay.cmd_rect.y + 16) as f32,
            10.5,
            theme.text_muted,
            Face::Ui,
        );
        let cmd_line = text.truncate(
            &format_command_preview(&ctx.command),
            (lay.cmd_rect.w - 24).max(60) as f32,
            12.0,
            Face::Mono,
        );
        text.draw(
            canvas,
            &cmd_line,
            (lay.cmd_rect.x + 12) as f32,
            (lay.cmd_rect.y + 35) as f32,
            12.0,
            theme.text,
            Face::Mono,
        );
    }

    text.draw(
        canvas,
        "管理员密码",
        (content.x + lay.pad) as f32,
        lay.input_label_y as f32,
        11.5,
        theme.text_dim,
        Face::Ui,
    );

    let shown_pw: String = if reveal {
        password.to_string()
    } else {
        "●".repeat(password.chars().count())
    };
    ui::text_input_ime(
        canvas,
        text,
        theme,
        lay.input_rect,
        &shown_pw,
        "",
        "请输入密码并按 Enter 确认",
        focused,
        None,
    );

    ui::button(
        canvas,
        text,
        theme,
        lay.reveal_rect,
        if reveal { "隐藏" } else { "显示" },
        None,
        false,
        hover == Hit::RevealToggle,
    );

    if let Some(err) = &ctx.error {
        let er = lay.err_rect;
        canvas.rounded_rect(er, radius, mix(theme.window, theme.danger, 0.14), 1.0);
        canvas.rounded_border(er, radius, theme.danger, 0.72, 1);
        let lines = wrap_lines(
            text,
            err,
            (er.w - 22).max(80) as f32,
            11.5,
            Face::Ui,
            2,
        );
        for (idx, line) in lines.iter().enumerate() {
            text.draw(
                canvas,
                line,
                (er.x + 11) as f32,
                (er.y + 17 + idx as i32 * 16) as f32,
                11.5,
                theme.danger,
                Face::Ui,
            );
        }
    }

    canvas.line(
        content.x + lay.pad,
        lay.cancel_rect.y - 12,
        content.right() - lay.pad,
        lay.cancel_rect.y - 12,
        theme.panel_border,
        0.7,
    );
    ui::button(
        canvas,
        text,
        theme,
        lay.cancel_rect,
        "取消",
        None,
        false,
        hover == Hit::Cancel,
    );
    ui::button(
        canvas,
        text,
        theme,
        lay.submit_rect,
        "授权",
        None,
        true,
        hover == Hit::Submit,
    );
    canvas.enforce_opaque_rect(canvas.bounds());
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    if let Some(flag) = arguments.get(1).map(String::as_str) {
        if matches!(flag, "-h" | "--help") {
            println!("用法: spui-askpass [提示标题] [命令预览]");
            return ExitCode::SUCCESS;
        }
        if matches!(flag, "-V" | "--version") {
            println!("spui-askpass 1.0.0 (SpaceOS-9)");
            return ExitCode::SUCCESS;
        }
    }
    match run(arguments.get(1..).unwrap_or(&[])) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("spui-askpass: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_prompt_ignores_boilerplate() {
        assert_eq!(clean_prompt_arg("Password: "), None);
        assert_eq!(clean_prompt_arg("为安全起见，您键入的密码将不可见。"), None);
        assert_eq!(
            clean_prompt_arg("需要安装 theme-winxp："),
            Some("需要安装 theme-winxp".into())
        );
    }

    #[test]
    fn format_command_preview_avoids_double_sudo() {
        assert_eq!(
            format_command_preview("spk install theme-winxp"),
            "sudo spk install theme-winxp"
        );
        assert_eq!(
            format_command_preview("sudo spk install theme-winxp"),
            "sudo spk install theme-winxp"
        );
    }

    #[test]
    fn key_handler_accepts_ascii_backspace_and_clear() {
        let mut pw = String::new();
        let mut focused = true;
        let mut reveal = false;
        assert!(handle_auth_key(
            Key {
                code: 0,
                state: 0,
                sym: b'a' as u32,
            },
            &mut pw,
            &mut focused,
            &mut reveal
        ));
        assert!(handle_auth_key(
            Key {
                code: 0,
                state: 1,
                sym: b'9' as u32,
            },
            &mut pw,
            &mut focused,
            &mut reveal
        ));
        assert_eq!(pw, "a9");
        assert!(handle_auth_key(
            Key {
                code: 0,
                state: 0,
                sym: 0xff08,
            },
            &mut pw,
            &mut focused,
            &mut reveal
        ));
        assert_eq!(pw, "a");
        assert!(handle_auth_key(
            Key {
                code: 0,
                state: 4,
                sym: b'u' as u32,
            },
            &mut pw,
            &mut focused,
            &mut reveal
        ));
        assert!(pw.is_empty());
    }
}
