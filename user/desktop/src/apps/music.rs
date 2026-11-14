/*
 *
 *       user/desktop/src/apps/music.rs
 *       SPUI 4.0 Music Player
 *
 *       2026/11/11 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A compact music player for the bundled MP3 sample. Decoding and ALSA
//! playback run in `/bin/mp3play` (minimp3 + the kernel ALSA ABI); this window
//! provides the library entry, transport controls and live playback state.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::Icon;
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::Result;
use std::fs;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const MUSIC_DIR: &str = "/usr/share/music";
const DEFAULT_TRACK: &str = "/usr/share/music/qingtian.mp3";

struct TrackInfo {
    path: String,
    title: String,
    subtitle: String,
}

fn describe_track(path: &str) -> TrackInfo {
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    match name {
        "qingtian.mp3" => TrackInfo {
            path: path.to_string(),
            title: String::from("晴天"),
            subtitle: String::from("Opus 5.5 生成 · MP3 · 44.1 kHz · 立体声"),
        },
        "spaceos-theme.mp3" => TrackInfo {
            path: path.to_string(),
            title: String::from("SpaceOS 主题曲"),
            subtitle: String::from("示例 MP3 · 44.1 kHz · 立体声"),
        },
        _ => TrackInfo {
            path: path.to_string(),
            title: name.strip_suffix(".mp3").unwrap_or(name).to_string(),
            subtitle: String::from("MP3 · /usr/share/music"),
        },
    }
}

fn load_tracks() -> Vec<TrackInfo> {
    let mut paths = Vec::new();
    if let Ok(entries) = fs::read_dir(MUSIC_DIR) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mp3"))
            {
                paths.push(path.to_string_lossy().into_owned());
            }
        }
    }
    paths.sort();
    if let Some(pos) = paths.iter().position(|p| p == DEFAULT_TRACK) {
        let preferred = paths.remove(pos);
        paths.insert(0, preferred);
    }
    if paths.is_empty() {
        paths.push(String::from(DEFAULT_TRACK));
    }
    paths.into_iter().map(|p| describe_track(&p)).collect()
}

struct PlayerState {
    tracks: Vec<TrackInfo>,
    current: usize,
    child: Option<Child>,
    status: String,
    started: Instant,
}

impl PlayerState {
    fn new() -> Self {
        Self {
            tracks: load_tracks(),
            current: 0,
            child: None,
            status: String::from("就绪"),
            started: Instant::now(),
        }
    }

    fn current_track(&self) -> &TrackInfo {
        &self.tracks[self.current]
    }

    fn playing(&self) -> bool {
        self.child.is_some()
    }

    fn play(&mut self) {
        if self.child.is_some() {
            return;
        }
        let path = self.current_track().path.clone();
        match Command::new("/bin/mp3play").arg(&path).spawn() {
            Ok(child) => {
                self.child = Some(child);
                self.status = String::from("正在播放");
                self.started = Instant::now();
            }
            Err(error) => {
                self.status = format!("无法启动播放器：{error}");
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.status = String::from("已停止");
    }

    fn next_track(&mut self) {
        if self.tracks.len() <= 1 {
            return;
        }
        let was_playing = self.playing();
        self.stop();
        self.current = (self.current + 1) % self.tracks.len();
        if was_playing {
            self.play();
        } else {
            self.status = String::from("就绪");
        }
    }

    fn poll(&mut self) -> bool {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.child = None;
                    self.status = if exit.success() {
                        String::from("播放结束")
                    } else {
                        String::from("播放失败（请检查音频设备）")
                    };
                    return true;
                }
                Ok(None) => {}
                Err(error) => {
                    self.child = None;
                    self.status = format!("播放异常：{error}");
                    return true;
                }
            }
        }
        false
    }
}

pub fn run() -> Result<()> {
    let mut app = App::new("音乐播放器", Icon::Music, 460, 380)?;
    app.min_width = 380;
    app.min_height = 330;

    let mut state = PlayerState::new();
    let mut hover_play = false;
    let mut hover_stop = false;
    let mut hover_next = false;
    let mut last_spectrum = Instant::now();

    loop {
        if app.close {
            break;
        }
        if state.poll() {
            app.needs_redraw = true;
        }
        if app.needs_redraw {
            draw_player(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &state,
                app.width as i32,
                app.height as i32,
                hover_play,
                hover_stop,
                hover_next,
            );
            app.draw_chrome();
            app.present()?;
            last_spectrum = Instant::now();
        } else if state.playing() && last_spectrum.elapsed() >= Duration::from_millis(80) {
            draw_spectrum(&mut app.canvas, &app.theme, &state, app.width as i32, true);
            app.needs_redraw = true;
            app.present()?;
            last_spectrum = Instant::now();
        }

        for event in app
            .pump_with_fd(None, if state.playing() { 80 } else { 200 })?
            .0
        {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    let (play, stop, next) = button_rects(app.height as i32);
                    let next_play = play.contains(x, y);
                    let next_stop = stop.contains(x, y);
                    let next_btn = state.tracks.len() > 1 && next.contains(x, y);
                    if next_play != hover_play || next_stop != hover_stop || next_btn != hover_next
                    {
                        hover_play = next_play;
                        hover_stop = next_stop;
                        hover_next = next_btn;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x,
                    y,
                    button: 1,
                    down: true,
                    ..
                } => {
                    let (play, stop, next) = button_rects(app.height as i32);
                    if play.contains(x, y) {
                        state.play();
                    } else if stop.contains(x, y) {
                        state.stop();
                    } else if state.tracks.len() > 1 && next.contains(x, y) {
                        state.next_track();
                    }
                    app.needs_redraw = true;
                }
                UiEvent::Key(key) if key.sym == 0x20 => {
                    if state.playing() {
                        state.stop();
                    } else {
                        state.play();
                    }
                    app.needs_redraw = true;
                }
                _ => {}
            }
        }
    }
    state.stop();
    Ok(())
}

fn button_rects(height: i32) -> (Rect, Rect, Rect) {
    let y = height - 64;
    (
        Rect::new(24, y, 120, 40),
        Rect::new(154, y, 100, 40),
        Rect::new(264, y, 100, 40),
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_player(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &PlayerState,
    width: i32,
    height: i32,
    hover_play: bool,
    hover_stop: bool,
    hover_next: bool,
) {
    canvas.clear(theme.window);

    let album = Rect::new(24, TITLEBAR_HEIGHT + 22, width - 48, 186);
    ui::card(canvas, theme, album);
    let art = Rect::new(album.x + 18, album.y + 18, 150, 150);
    canvas.rounded_rect(art, 8, theme.accent, 1.0);
    canvas.circle(
        art.x as f32 + 75.0,
        art.y as f32 + 75.0,
        46.0,
        theme.window_high,
        0.25,
    );
    text.draw_centered(
        canvas,
        "♪",
        art.x as f32 + 75.0,
        art.y as f32 + 92.0,
        54.0,
        0xffffff,
        Face::Bold,
    );

    let track = state.current_track();
    let title_x = art.right() as f32 + 22.0;
    text.draw(
        canvas,
        &track.title,
        title_x,
        album.y as f32 + 48.0,
        21.0,
        theme.text,
        Face::Bold,
    );
    text.draw(
        canvas,
        &track.subtitle,
        title_x,
        album.y as f32 + 72.0,
        12.0,
        theme.text_muted,
        Face::Ui,
    );
    text.draw(
        canvas,
        &state.status,
        title_x,
        album.y as f32 + 100.0,
        12.5,
        if state.playing() {
            theme.accent
        } else {
            theme.text_muted
        },
        Face::Ui,
    );

    // Decorative spectrum that moves while playback is active.
    draw_spectrum(canvas, theme, state, width, false);

    let (play, stop, next) = button_rects(height);
    let play_label = if state.playing() {
        "播放中"
    } else {
        "播放"
    };
    ui::button(
        canvas,
        text,
        theme,
        play,
        play_label,
        Some(Icon::Music),
        true,
        hover_play,
    );
    ui::button(
        canvas,
        text,
        theme,
        stop,
        "停止",
        Some(Icon::Shutdown),
        false,
        hover_stop,
    );
    if state.tracks.len() > 1 {
        ui::button(canvas, text, theme, next, "下一首", None, false, hover_next);
    }

    text.draw(
        canvas,
        "空格键播放 / 停止 · 曲目来自 /usr/share/music",
        24.0,
        height as f32 - 18.0,
        11.5,
        theme.text_muted,
        Face::Ui,
    );
}

fn draw_spectrum(
    canvas: &mut Canvas,
    theme: &Theme,
    state: &PlayerState,
    width: i32,
    clear_bg: bool,
) {
    let album = Rect::new(24, TITLEBAR_HEIGHT + 22, width - 48, 186);
    let art_right = album.x + 18 + 150;
    let bars = 18;
    let bar_w = ((album.w - 206) / bars).max(4);
    let base_y = album.bottom() - 22;
    let x0 = art_right + 22;
    if clear_bg {
        let strip_w = (bars * bar_w).max(0);
        canvas.fill(Rect::new(x0, base_y - 54, strip_w, 56), theme.window_high);
    }
    let elapsed = state.started.elapsed().as_secs_f32();
    for index in 0..bars {
        let phase = elapsed * 5.0 + index as f32 * 0.63;
        let wave = if state.playing() {
            (phase.sin() * 0.5 + 0.5).powi(2)
        } else {
            0.08
        };
        let bar_h = (10.0 + wave * 42.0) as i32;
        let x = x0 + index as i32 * bar_w;
        let rect = Rect::new(x, base_y - bar_h, bar_w - 3, bar_h);
        canvas.rounded_rect(rect, 2, theme.accent, 0.85);
    }
}
