/*
 *
 *       user/desktop/src/apps/notify.rs
 *       SPUI 4.0 standalone desktop notification dialog
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Standalone notification client window (`spui notify <title> <body>`)
//! rendered with native SPUI 4.0 chrome and Breeze notification iconography.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::{self, Icon};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::Result;
use std::time::{Duration, Instant};

fn dismiss_btn_rect(w: i32, h: i32) -> Rect {
    Rect::new(w - 84, h - 38, 70, 26)
}

pub fn run(title: &str, body: &str) -> Result<()> {
    let mut app = App::new(title, Icon::Notify, 380, 134)?;
    app.min_width = 320;
    app.min_height = 124;
    crate::sound::play(&app.config, crate::sound::SystemSound::Notify);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut btn_hover = false;

    loop {
        if app.close || Instant::now() >= deadline {
            break;
        }
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(100) as i32;
        let (events, _) = app.pump_with_fd(None, remaining_ms.max(10))?;

        for event in events {
            let (w, h) = (app.width as i32, app.height as i32);
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    let hov = dismiss_btn_rect(w, h).contains(x, y);
                    if hov != btn_hover {
                        btn_hover = hov;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Key(key) if key.is_escape() || key.is_enter() => app.request_close(),
                UiEvent::Button {
                    x,
                    y,
                    button: 1,
                    down: true,
                    ..
                } if dismiss_btn_rect(w, h).contains(x, y) => {
                    crate::sound::play(&app.config, crate::sound::SystemSound::Click);
                    app.request_close();
                }
                _ => {}
            }
        }

        if app.needs_redraw {
            draw_notify(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                title,
                body,
                btn_hover,
            );
            app.draw_chrome();
            app.present()?;
        }
    }
    Ok(())
}

fn draw_notify(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    title: &str,
    body: &str,
    btn_hover: bool,
) {
    let w = canvas.width as i32;
    let h = canvas.height as i32;
    canvas.clear(theme.window);

    icons::draw(canvas, Icon::Notify, 16, TITLEBAR_HEIGHT + 16, 32, None);
    let max_w = (w - 72) as f32;
    let shown_title = text.truncate(title, max_w, 13.5, Face::Bold);
    text.draw(
        canvas,
        &shown_title,
        60.0,
        (TITLEBAR_HEIGHT + 30) as f32,
        13.5,
        theme.text,
        Face::Bold,
    );

    let shown_body = text.truncate(
        if body.is_empty() {
            "系统通知"
        } else {
            body
        },
        max_w,
        12.0,
        Face::Ui,
    );
    text.draw(
        canvas,
        &shown_body,
        60.0,
        (TITLEBAR_HEIGHT + 52) as f32,
        12.0,
        theme.text_dim,
        Face::Ui,
    );

    ui::button(
        canvas,
        text,
        theme,
        dismiss_btn_rect(w, h),
        "确定",
        None,
        false,
        btn_hover,
    );
}
