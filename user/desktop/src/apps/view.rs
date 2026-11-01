/*
 *
 *       user/desktop/src/apps/view.rs
 *       SPUI 4.0 Image Viewer: Loupe-styled image inspection, pan/zoom and wallpaper integration
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Modern content-first image viewer inspired by GNOME Loupe and macOS Preview.
//! Supports fluid mouse wheel zoom, pan dragging, sibling folder navigation,
//! rotation, and one-click wallpaper setting via `uxconfd`.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect, Sprite};
use crate::icons::{self, Icon};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, RADIUS_CONTROL, TITLEBAR_HEIGHT};
use crate::x11::Result;
use std::fs;
use std::path::{Path, PathBuf};
use uxconf::client::UxConfClient;
use uxconf::Value;

pub fn load_image_file(path: &Path) -> Option<Sprite> {
    if let Some(ext) = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
    {
        if ext == "png" {
            if let Some(s) = icons::decode_png(path) {
                return Some(s);
            }
        }
        if matches!(ext.as_str(), "jpg" | "jpeg") {
            if let Some(s) = icons::decode_jpeg(path) {
                return Some(s);
            }
        }
    }
    // Fallback: probe both
    icons::decode_jpeg(path).or_else(|| icons::decode_png(path))
}

struct ViewerState {
    current_path: PathBuf,
    siblings: Vec<PathBuf>,
    sibling_idx: usize,
    image: Option<Sprite>,
    zoom: f32,
    offset_x: f32,
    offset_y: f32,
    rotation_steps: u8, // 0, 1, 2, 3 (* 90 deg)
    status_msg: Option<String>,
}

impl ViewerState {
    fn new(initial_path: Option<&str>) -> Self {
        let fallback = PathBuf::from("/usr/share/spui/wallpapers/alpine-peaks.jpg");
        let path = initial_path
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .unwrap_or(fallback);

        let mut state = Self {
            current_path: path.clone(),
            siblings: Vec::new(),
            sibling_idx: 0,
            image: None,
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            rotation_steps: 0,
            status_msg: None,
        };
        state.scan_siblings();
        state.load_current();
        state
    }

    fn scan_siblings(&mut self) {
        let mut list = Vec::new();
        if let Some(parent) = self.current_path.parent() {
            if let Ok(entries) = fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if let Some(ext) = p
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(str::to_ascii_lowercase)
                    {
                        if matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
                            list.push(p);
                        }
                    }
                }
            }
        }
        list.sort();
        if let Some(pos) = list.iter().position(|p| p == &self.current_path) {
            self.sibling_idx = pos;
        } else {
            self.sibling_idx = 0;
            list.insert(0, self.current_path.clone());
        }
        self.siblings = list;
    }

    fn load_current(&mut self) {
        self.image = load_image_file(&self.current_path);
        self.zoom = 1.0;
        self.offset_x = 0.0;
        self.offset_y = 0.0;
        self.rotation_steps = 0;
        self.status_msg = None;
    }

    fn prev_image(&mut self) {
        if self.siblings.is_empty() {
            return;
        }
        if self.sibling_idx == 0 {
            self.sibling_idx = self.siblings.len() - 1;
        } else {
            self.sibling_idx -= 1;
        }
        self.current_path = self.siblings[self.sibling_idx].clone();
        self.load_current();
    }

    fn next_image(&mut self) {
        if self.siblings.is_empty() {
            return;
        }
        self.sibling_idx = (self.sibling_idx + 1) % self.siblings.len();
        self.current_path = self.siblings[self.sibling_idx].clone();
        self.load_current();
    }

    fn rotate_90(&mut self) {
        if let Some(sprite) = self.image.take() {
            let nw = sprite.height;
            let nh = sprite.width;
            let mut new_pixels = vec![0u32; (nw * nh) as usize];
            for y in 0..sprite.height {
                for x in 0..sprite.width {
                    let src_idx = (y * sprite.width + x) as usize;
                    let nx = sprite.height - 1 - y;
                    let ny = x;
                    let dst_idx = (ny * nw + nx) as usize;
                    new_pixels[dst_idx] = sprite.pixels[src_idx];
                }
            }
            self.image = Some(Sprite {
                width: nw,
                height: nh,
                pixels: new_pixels,
            });
            self.rotation_steps = (self.rotation_steps + 1) % 4;
        }
    }

    fn set_as_wallpaper(&mut self) {
        let name = self
            .current_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("alpine-peaks.jpg");
        if let Ok(mut client) = UxConfClient::connect() {
            let _ = client.set(
                "desktop",
                "/backdrop/wallpaper",
                Value::String(name.to_string()),
            );
            let _ = client.set(
                "desktop",
                "/backdrop/style",
                Value::String("cover".to_string()),
            );
            self.status_msg = Some("已设置为桌面壁纸".to_string());
        }
    }

    fn fit_to_viewport(&mut self, vp_w: f32, vp_h: f32) {
        if let Some(img) = &self.image {
            let iw = img.width as f32;
            let ih = img.height as f32;
            let scale_x = (vp_w - 32.0) / iw;
            let scale_y = (vp_h - 32.0) / ih;
            self.zoom = scale_x.min(scale_y).clamp(0.05, 5.0);
            self.offset_x = 0.0;
            self.offset_y = 0.0;
        }
    }
}

pub fn run(initial_path: Option<&str>) -> Result<()> {
    let mut state = ViewerState::new(initial_path);

    let mut app = App::new("看图工具", Icon::ImageViewer, 740, 520)?;
    app.min_width = 480;
    app.min_height = 360;

    // Initial fit
    state.fit_to_viewport(
        (app.width as f32).max(100.0),
        (app.height as f32 - TITLEBAR_HEIGHT as f32 - 48.0).max(100.0),
    );

    let mut drag_origin: Option<(i32, i32, f32, f32)> = None;
    let mut hover_item: Option<usize> = None;

    loop {
        if app.close {
            break;
        }

        if app.needs_redraw {
            draw_viewer(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &state,
                app.width as i32,
                app.height as i32,
                hover_item,
            );
            app.draw_chrome();
            app.present()?;
        }

        for event in app.pump_with_fd(None, 60)?.0 {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Wheel { delta, .. } => {
                    let old_zoom = state.zoom;
                    if delta < 0 {
                        state.zoom = (state.zoom * 1.15).min(8.0);
                    } else {
                        state.zoom = (state.zoom / 1.15).max(0.05);
                    }
                    if (state.zoom - old_zoom).abs() > 0.001 {
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
                        if y >= TITLEBAR_HEIGHT {
                            let tb_r = toolbar_rect(app.width as i32, app.height as i32);
                            if tb_r.contains(x, y) {
                                if let Some(btn_idx) = hit_toolbar(tb_r, x, y) {
                                    match btn_idx {
                                        0 => state.prev_image(),
                                        1 => state.next_image(),
                                        2 => state.zoom = (state.zoom / 1.25).max(0.05),
                                        3 => state.zoom = (state.zoom * 1.25).min(8.0),
                                        4 => state.fit_to_viewport(
                                            app.width as f32,
                                            (app.height as f32 - TITLEBAR_HEIGHT as f32 - 48.0)
                                                .max(100.0),
                                        ),
                                        5 => state.rotate_90(),
                                        6 => state.set_as_wallpaper(),
                                        _ => {}
                                    }
                                    app.needs_redraw = true;
                                    continue;
                                }
                            }
                            drag_origin = Some((x, y, state.offset_x, state.offset_y));
                        }
                    } else {
                        drag_origin = None;
                    }
                }
                UiEvent::Motion { x, y, .. } => {
                    let tb_r = toolbar_rect(app.width as i32, app.height as i32);
                    let next_hover = if tb_r.contains(x, y) {
                        hit_toolbar(tb_r, x, y)
                    } else {
                        None
                    };
                    if next_hover != hover_item {
                        hover_item = next_hover;
                        app.needs_redraw = true;
                    }

                    if let Some((ox, oy, init_x, init_y)) = drag_origin {
                        state.offset_x = init_x + (x - ox) as f32;
                        state.offset_y = init_y + (y - oy) as f32;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Key(key) => {
                    match key.sym {
                        0xff51 => {
                            // Left
                            state.prev_image();
                            app.needs_redraw = true;
                        }
                        0xff53 => {
                            // Right
                            state.next_image();
                            app.needs_redraw = true;
                        }
                        0x2b | 0x3d => {
                            // '+'
                            state.zoom = (state.zoom * 1.2).min(8.0);
                            app.needs_redraw = true;
                        }
                        0x2d => {
                            // '-'
                            state.zoom = (state.zoom / 1.2).max(0.05);
                            app.needs_redraw = true;
                        }
                        0x30 => {
                            // '0' reset fit
                            state.fit_to_viewport(
                                app.width as f32,
                                (app.height as f32 - TITLEBAR_HEIGHT as f32 - 48.0).max(100.0),
                            );
                            app.needs_redraw = true;
                        }
                        0x72 | 0x52 => {
                            // 'r' / 'R'
                            state.rotate_90();
                            app.needs_redraw = true;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn toolbar_rect(w: i32, h: i32) -> Rect {
    let tb_w = 420;
    let tb_h = 36;
    Rect::new((w - tb_w) / 2, h - tb_h - 14, tb_w, tb_h)
}

fn hit_toolbar(tb: Rect, x: i32, y: i32) -> Option<usize> {
    if !tb.contains(x, y) {
        return None;
    }
    // 7 buttons: Prev, Next, ZoomOut, ZoomIn, Fit, Rotate, SetWallpaper
    let item_w = tb.w / 7;
    let idx = (x - tb.x) / item_w;
    Some(idx.clamp(0, 6) as usize)
}

fn draw_viewer(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &ViewerState,
    w: i32,
    h: i32,
    hover: Option<usize>,
) {
    // Dark neutral canvas for image presentation
    let canvas_bg = if theme.is_dark() { 0x121214 } else { 0x222226 };
    canvas.clear(canvas_bg);

    // Canvas viewport
    let vp_y = TITLEBAR_HEIGHT;
    let vp_h = h - vp_y;
    let _vp_rect = Rect::new(0, vp_y, w, vp_h);

    if let Some(sprite) = &state.image {
        let sw = (sprite.width as f32 * state.zoom).round() as i32;
        let sh = (sprite.height as f32 * state.zoom).round() as i32;

        let center_x = w / 2 + state.offset_x.round() as i32;
        let center_y = vp_y + vp_h / 2 + state.offset_y.round() as i32;
        let draw_x = center_x - sw / 2;
        let draw_y = center_y - sh / 2;

        // Bilinear or nearest scaled blit
        if (state.zoom - 1.0).abs() < 0.01 {
            canvas.blit_sprite(draw_x, draw_y, sprite);
        } else if sw > 0 && sh > 0 {
            let scaled = sprite.scale_bilinear(sw as u32, sh as u32);
            canvas.blit_sprite(draw_x, draw_y, &scaled);
        }
    } else {
        text.draw_centered(
            canvas,
            "无法解码或读取图片",
            w as f32 * 0.5,
            vp_y as f32 + vp_h as f32 * 0.5,
            14.0,
            theme.text_muted,
            Face::Ui,
        );
    }

    // Top metadata badge
    let filename = state
        .current_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("图片");
    let meta_str = if let Some(img) = &state.image {
        format!(
            "{filename}  ·  {} × {}  ·  {:.0}%",
            img.width,
            img.height,
            state.zoom * 100.0
        )
    } else {
        filename.to_string()
    };
    let meta_w = text.measure(&meta_str, 11.5, Face::Ui) + 24.0;
    let meta_r = Rect::new(
        (w - meta_w as i32) / 2,
        TITLEBAR_HEIGHT + 10,
        meta_w as i32,
        26,
    );
    canvas.rounded_rect(meta_r, RADIUS_CONTROL, 0x18181b, 0.88);
    canvas.rounded_border(meta_r, RADIUS_CONTROL, 0x3f3f46, 0.70, 1);
    text.draw_centered(
        canvas,
        &meta_str,
        meta_r.x as f32 + meta_r.w as f32 * 0.5,
        meta_r.y as f32 + 18.0,
        11.5,
        0xf4f4f5,
        Face::Ui,
    );

    // Floating pill toolbar
    let tb_r = toolbar_rect(w, h);
    canvas.rounded_rect(tb_r, RADIUS_CONTROL + 2, 0x18181b, 0.92);
    canvas.rounded_border(tb_r, RADIUS_CONTROL + 2, 0x3f3f46, 0.85, 1);

    let tools = [
        "上一张",
        "下一张",
        "缩小",
        "放大",
        "适应",
        "旋转",
        "设为壁纸",
    ];

    let item_w = tb_r.w / 7;
    for (i, label) in tools.iter().enumerate() {
        let bx = tb_r.x + i as i32 * item_w;
        let br = Rect::new(bx, tb_r.y, item_w, tb_r.h);
        if hover == Some(i) {
            canvas.rounded_rect(br.inset(2), RADIUS_CONTROL, 0x27272a, 1.0);
        }
        let is_special = i == 6;
        let fg = if is_special { theme.accent } else { 0xe4e4e7 };
        text.draw_centered(
            canvas,
            label,
            br.x as f32 + br.w as f32 * 0.5,
            br.y as f32 + br.h as f32 * 0.5 + 4.0,
            11.5,
            fg,
            if is_special { Face::Bold } else { Face::Ui },
        );
        if i > 0 {
            canvas.fill(Rect::new(bx, tb_r.y + 8, 1, tb_r.h - 16), 0x27272a);
        }
    }

    // Temporary status badge (e.g. "已设置为桌面壁纸")
    if let Some(msg) = &state.status_msg {
        let msg_w = text.measure(msg, 12.0, Face::Bold) + 24.0;
        let msg_r = Rect::new((w - msg_w as i32) / 2, tb_r.y - 36, msg_w as i32, 28);
        canvas.rounded_rect(msg_r, RADIUS_CONTROL, theme.accent, 0.95);
        text.draw_centered(
            canvas,
            msg,
            msg_r.x as f32 + msg_r.w as f32 * 0.5,
            msg_r.y as f32 + 19.0,
            12.0,
            0xffffff,
            Face::Bold,
        );
    }
}
