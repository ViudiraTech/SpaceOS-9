/*
 *
 *       user/desktop/src/apps/music.rs
 *       SPUI 4.0 Music Player
 *
 *       2026/11/11 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Desktop MP3 music player built on top of `/bin/mp3play`.
//!
//! Playback runs in an out-of-process `/bin/mp3play` child controlled via POSIX
//! signals (`SIGUSR1` for ALSA hardware pause/resume preserving position,
//! `SIGTERM` for immediate `SNDRV_PCM_IOCTL_DROP`) and `--start-ms` seeking.
//! Track duration probing runs on a background worker thread so startup never
//! blocks the window event loop.

use crate::app::{App, UiEvent};
use crate::draw::{mix, Canvas, Rect};
use crate::icons::Icon;
use crate::sound::{apply_mixer_volume, play as play_sound, SystemSound};
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::{CursorKind, Result};
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use uxconf::Value;

const MUSIC_DIR: &str = "/usr/share/music";
const DEFAULT_TRACK: &str = "/usr/share/music/qingtian.mp3";
const SPECTRUM_BARS: usize = 20;
const PLAYLIST_ROW_H: i32 = 42;
const SEEK_STEP_MS: i64 = 5_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TrackMeta {
    duration_ms: Option<u64>,
    rate: Option<u32>,
    channels: Option<u32>,
}

struct TrackInfo {
    path: String,
    title: String,
    subtitle: String,
    meta: TrackMeta,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlayState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepeatMode {
    Sequential,
    RepeatAll,
    RepeatOne,
}

impl RepeatMode {
    fn next(self) -> Self {
        match self {
            Self::Sequential => Self::RepeatAll,
            Self::RepeatAll => Self::RepeatOne,
            Self::RepeatOne => Self::Sequential,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sequential => "顺序播放",
            Self::RepeatAll => "列表循环",
            Self::RepeatOne => "单曲循环",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HitTarget {
    Prev,
    PlayPause,
    Stop,
    Next,
    Repeat,
    Mute,
    VolumeSlider,
    ProgressBar,
    TrackRow(usize),
}

struct Layout {
    album: Rect,
    art: Rect,
    spectrum: Rect,
    progress_row: Rect,
    progress_bar: Rect,
    prev: Rect,
    play_pause: Rect,
    stop: Rect,
    next: Rect,
    repeat: Rect,
    mute: Rect,
    vol_slider: Rect,
    vol_text_x: i32,
    playlist_card: Rect,
    playlist_vp: Rect,
    playlist_sb: Rect,
}

impl Layout {
    fn compute(width: i32, height: i32) -> Self {
        let margin = 18;
        let card_w = (width - margin * 2).max(260);
        let album = Rect::new(margin, TITLEBAR_HEIGHT + 14, card_w, 192);
        let art = Rect::new(album.x + 16, album.y + 16, 128, 128);
        let info_x = art.right() + 18;
        let info_w = (album.right() - 16 - info_x).max(80);
        let spectrum = Rect::new(info_x, art.y + 72, info_w, 56);
        let progress_row = Rect::new(album.x + 14, album.bottom() - 36, album.w - 28, 26);
        let time_col = 48;
        let progress_bar = Rect::new(
            progress_row.x + time_col,
            progress_row.y + 3,
            (progress_row.w - time_col * 2).max(40),
            20,
        );

        let tr_y = album.bottom() + 12;
        let prev = Rect::new(margin, tr_y + 3, 34, 34);
        let play_pause = Rect::new(prev.right() + 8, tr_y, 42, 40);
        let stop = Rect::new(play_pause.right() + 8, tr_y + 3, 34, 34);
        let next = Rect::new(stop.right() + 8, tr_y + 3, 34, 34);
        let repeat = Rect::new(next.right() + 10, tr_y + 4, 74, 32);

        let right_edge = width - margin;
        let vol_text_w = 34;
        let vol_text_x = right_edge - vol_text_w;
        let vol_slider_w = (right_edge - repeat.right() - 12 - vol_text_w - 32).clamp(64, 116);
        let vol_slider = Rect::new(vol_text_x - 6 - vol_slider_w, tr_y + 9, vol_slider_w, 22);
        let mute = Rect::new(vol_slider.x - 30, tr_y + 6, 28, 28);

        let pl_y = tr_y + 52;
        let pl_h = (height - 26 - pl_y).max(96);
        let playlist_card = Rect::new(margin, pl_y, card_w, pl_h);
        let playlist_vp = Rect::new(
            playlist_card.x + 8,
            playlist_card.y + 32,
            playlist_card.w - 24,
            (playlist_card.h - 40).max(20),
        );
        let playlist_sb = Rect::new(
            playlist_vp.right() + 4,
            playlist_vp.y + 2,
            6,
            (playlist_vp.h - 4).max(8),
        );

        Self {
            album,
            art,
            spectrum,
            progress_row,
            progress_bar,
            prev,
            play_pause,
            stop,
            next,
            repeat,
            mute,
            vol_slider,
            vol_text_x,
            playlist_card,
            playlist_vp,
            playlist_sb,
        }
    }

    fn hit_test(&self, x: i32, y: i32, scroll_off: i32, track_count: usize) -> Option<HitTarget> {
        if self.prev.contains(x, y) {
            return Some(HitTarget::Prev);
        }
        if self.play_pause.contains(x, y) {
            return Some(HitTarget::PlayPause);
        }
        if self.stop.contains(x, y) {
            return Some(HitTarget::Stop);
        }
        if self.next.contains(x, y) {
            return Some(HitTarget::Next);
        }
        if self.repeat.contains(x, y) {
            return Some(HitTarget::Repeat);
        }
        if self.mute.contains(x, y) {
            return Some(HitTarget::Mute);
        }
        if self.vol_slider.contains(x, y) {
            return Some(HitTarget::VolumeSlider);
        }
        if self.progress_bar.contains(x, y) {
            return Some(HitTarget::ProgressBar);
        }
        if self.playlist_vp.contains(x, y) {
            let rel = y - self.playlist_vp.y + scroll_off;
            if rel >= 0 {
                let idx = (rel / PLAYLIST_ROW_H) as usize;
                if idx < track_count {
                    return Some(HitTarget::TrackRow(idx));
                }
            }
        }
        None
    }
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
            subtitle: String::from("周杰伦 / 示例典藏 · MP3"),
            meta: TrackMeta::default(),
        },
        "spaceos-theme.mp3" => TrackInfo {
            path: path.to_string(),
            title: String::from("SpaceOS 主题曲"),
            subtitle: String::from("SpaceOS 内置合成原声 · MP3"),
            meta: TrackMeta::default(),
        },
        _ => TrackInfo {
            path: path.to_string(),
            title: name.strip_suffix(".mp3").unwrap_or(name).to_string(),
            subtitle: String::from("本地音频 · /usr/share/music"),
            meta: TrackMeta::default(),
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

fn parse_mp3_info_line(output: &str) -> Option<TrackMeta> {
    let mut duration_ms = None;
    let mut rate = None;
    let mut channels = None;
    for token in output.split_whitespace() {
        if let Some(val) = token.strip_prefix("duration_ms=") {
            duration_ms = val.parse::<u64>().ok();
        } else if let Some(val) = token.strip_prefix("rate=") {
            rate = val.parse::<u32>().ok();
        } else if let Some(val) = token.strip_prefix("channels=") {
            channels = val.parse::<u32>().ok();
        }
    }
    if duration_ms.is_none() && rate.is_none() && channels.is_none() {
        None
    } else {
        Some(TrackMeta {
            duration_ms,
            rate,
            channels,
        })
    }
}

fn spawn_probe_worker(paths: Vec<String>) -> Receiver<(usize, TrackMeta)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for (idx, path) in paths.into_iter().enumerate() {
            let out = Command::new("/bin/mp3play")
                .arg("--info")
                .arg(&path)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output();
            if let Ok(output) = out {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if let Some(meta) = parse_mp3_info_line(&stdout) {
                        if tx.send((idx, meta)).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
    rx
}

fn format_duration_ms(ms: u64) -> String {
    let total_sec = ms / 1000;
    let hours = total_sec / 3600;
    let mins = (total_sec % 3600) / 60;
    let secs = total_sec % 60;
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

fn terminate_and_reap(mut child: Child) {
    let pid = child.id() as libc::pid_t;
    // SAFETY: `pid` belongs to our direct child still owned by `Child`.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    for _ in 0..50 {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(3)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn send_pause_toggle(child: &Child) -> bool {
    let pid = child.id() as libc::pid_t;
    // SAFETY: `pid` belongs to our live child process.
    unsafe { libc::kill(pid, libc::SIGUSR1) == 0 }
}

fn advance_track_after_exit(
    current: usize,
    track_count: usize,
    repeat: RepeatMode,
    success: bool,
    consecutive_failures: usize,
) -> Option<usize> {
    if track_count == 0 {
        return None;
    }
    if success {
        match repeat {
            RepeatMode::RepeatOne => Some(current),
            RepeatMode::RepeatAll => Some((current + 1) % track_count),
            RepeatMode::Sequential => {
                if current + 1 < track_count {
                    Some(current + 1)
                } else {
                    None
                }
            }
        }
    } else if track_count > 1
        && repeat != RepeatMode::RepeatOne
        && consecutive_failures < track_count
    {
        Some((current + 1) % track_count)
    } else {
        None
    }
}

struct PlayerState {
    tracks: Vec<TrackInfo>,
    current: usize,
    child: Option<Child>,
    play_state: PlayState,
    repeat_mode: RepeatMode,
    status: String,
    status_error: bool,
    base_position_ms: u64,
    segment_started: Option<Instant>,
    consecutive_failures: usize,
    probe_rx: Receiver<(usize, TrackMeta)>,
}

impl PlayerState {
    fn new() -> Self {
        let tracks = load_tracks();
        let probe_paths = tracks.iter().map(|t| t.path.clone()).collect();
        let probe_rx = spawn_probe_worker(probe_paths);
        Self {
            tracks,
            current: 0,
            child: None,
            play_state: PlayState::Stopped,
            repeat_mode: RepeatMode::RepeatAll,
            status: String::from("就绪"),
            status_error: false,
            base_position_ms: 0,
            segment_started: None,
            consecutive_failures: 0,
            probe_rx,
        }
    }

    fn current_track(&self) -> &TrackInfo {
        &self.tracks[self.current]
    }

    fn current_position_ms(&self) -> u64 {
        let mut pos = self.base_position_ms;
        if self.play_state == PlayState::Playing {
            if let Some(started) = self.segment_started {
                pos = pos.saturating_add(started.elapsed().as_millis() as u64);
            }
        }
        if let Some(dur) = self.current_track().meta.duration_ms {
            pos = pos.min(dur);
        }
        pos
    }

    fn stop_child(&mut self) {
        if let Some(child) = self.child.take() {
            terminate_and_reap(child);
        }
    }

    fn play_from(&mut self, start_ms: u64) {
        self.stop_child();
        let clamped_ms = match self.current_track().meta.duration_ms {
            Some(dur) if dur > 0 => start_ms.min(dur.saturating_sub(250)),
            _ => start_ms,
        };
        let path = self.current_track().path.clone();
        let mut cmd = Command::new("/bin/mp3play");
        if clamped_ms > 0 {
            cmd.arg("--start-ms").arg(clamped_ms.to_string());
        }
        cmd.arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.spawn() {
            Ok(child) => {
                self.child = Some(child);
                self.play_state = PlayState::Playing;
                self.base_position_ms = clamped_ms;
                self.segment_started = Some(Instant::now());
                self.status = String::from("正在播放");
                self.status_error = false;
            }
            Err(error) => {
                self.play_state = PlayState::Stopped;
                self.base_position_ms = 0;
                self.segment_started = None;
                self.status = format!("无法启动播放器：{error}");
                self.status_error = true;
            }
        }
    }

    fn toggle_play_pause(&mut self) {
        self.consecutive_failures = 0;
        match self.play_state {
            PlayState::Stopped => {
                let start = self.base_position_ms;
                self.play_from(start);
            }
            PlayState::Playing => {
                self.base_position_ms = self.current_position_ms();
                self.segment_started = None;
                if let Some(child) = self.child.as_ref() {
                    if send_pause_toggle(child) {
                        self.play_state = PlayState::Paused;
                        self.status = String::from("已暂停");
                        self.status_error = false;
                        return;
                    }
                }
                self.stop_child();
                self.play_state = PlayState::Paused;
                self.status = String::from("已暂停");
            }
            PlayState::Paused => {
                if let Some(child) = self.child.as_ref() {
                    if send_pause_toggle(child) {
                        self.play_state = PlayState::Playing;
                        self.segment_started = Some(Instant::now());
                        self.status = String::from("正在播放");
                        self.status_error = false;
                        return;
                    }
                }
                let start = self.base_position_ms;
                self.play_from(start);
            }
        }
    }

    fn stop(&mut self) {
        self.stop_child();
        self.play_state = PlayState::Stopped;
        self.base_position_ms = 0;
        self.segment_started = None;
        self.consecutive_failures = 0;
        self.status = String::from("已停止");
        self.status_error = false;
    }

    fn select_track(&mut self, idx: usize, force_play: bool) {
        if idx >= self.tracks.len() {
            return;
        }
        let was_active = force_play || self.play_state != PlayState::Stopped;
        self.stop_child();
        self.current = idx;
        self.base_position_ms = 0;
        self.segment_started = None;
        self.consecutive_failures = 0;
        if was_active {
            self.play_from(0);
        } else {
            self.play_state = PlayState::Stopped;
            self.status = String::from("就绪");
            self.status_error = false;
        }
    }

    fn prev_track(&mut self) {
        if self.tracks.is_empty() {
            return;
        }
        if self.current_position_ms() > 3_000 && self.play_state == PlayState::Playing {
            self.consecutive_failures = 0;
            self.play_from(0);
            return;
        }
        let prev = if self.current == 0 {
            self.tracks.len() - 1
        } else {
            self.current - 1
        };
        let force = self.play_state != PlayState::Stopped;
        self.select_track(prev, force);
    }

    fn next_track(&mut self) {
        if self.tracks.is_empty() {
            return;
        }
        let next = (self.current + 1) % self.tracks.len();
        let force = self.play_state != PlayState::Stopped;
        self.select_track(next, force);
    }

    fn seek_to(&mut self, target_ms: u64) {
        let dur = match self.current_track().meta.duration_ms {
            Some(d) if d > 0 => d,
            _ => return,
        };
        let clamped = target_ms.min(dur);
        match self.play_state {
            PlayState::Playing => {
                self.consecutive_failures = 0;
                self.play_from(clamped);
            }
            PlayState::Paused => {
                self.stop_child();
                self.base_position_ms = clamped;
                self.segment_started = None;
                self.status = String::from("已暂停");
            }
            PlayState::Stopped => {
                self.base_position_ms = clamped;
            }
        }
    }

    fn seek_relative(&mut self, delta_ms: i64) {
        let cur = self.current_position_ms() as i64;
        let next = (cur + delta_ms).max(0) as u64;
        self.seek_to(next);
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((idx, meta)) = self.probe_rx.try_recv() {
            if let Some(track) = self.tracks.get_mut(idx) {
                track.meta = meta;
                changed = true;
            }
        }
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.child = None;
                    let ran_long = self
                        .segment_started
                        .is_some_and(|t| t.elapsed() >= Duration::from_secs(1));
                    self.segment_started = None;
                    let ok = exit.success();
                    if ok {
                        self.consecutive_failures = 0;
                    } else if ran_long {
                        self.consecutive_failures = 1;
                    } else {
                        self.consecutive_failures += 1;
                    }

                    let next = advance_track_after_exit(
                        self.current,
                        self.tracks.len(),
                        self.repeat_mode,
                        ok,
                        self.consecutive_failures,
                    );
                    if let Some(next_idx) = next {
                        self.current = next_idx;
                        self.base_position_ms = 0;
                        self.play_from(0);
                        if !ok {
                            let code =
                                exit.code().map_or(String::from("信号"), |c| c.to_string());
                            self.status = format!("上一首异常({code})，已切换下一首");
                            self.status_error = true;
                        }
                    } else {
                        self.play_state = PlayState::Stopped;
                        self.base_position_ms = 0;
                        if ok {
                            self.status = String::from("播放结束");
                            self.status_error = false;
                        } else {
                            let code =
                                exit.code().map_or(String::from("信号"), |c| c.to_string());
                            self.status = format!("播放失败（退出码 {code}）");
                            self.status_error = true;
                        }
                    }
                    changed = true;
                }
                Ok(None) => {}
                Err(error) => {
                    self.child = None;
                    self.play_state = PlayState::Stopped;
                    self.segment_started = None;
                    self.base_position_ms = 0;
                    self.status = format!("播放状态异常：{error}");
                    self.status_error = true;
                    changed = true;
                }
            }
        }
        changed
    }
}

impl Drop for PlayerState {
    fn drop(&mut self) {
        self.stop_child();
    }
}

pub fn run() -> Result<()> {
    let mut app = App::new("音乐播放器", Icon::Music, 540, 520)?;
    app.min_width = 440;
    app.min_height = 420;

    let mut state = PlayerState::new();
    let mut scroll = ui::SmoothScroll::new("music-playlist");
    let mut hover: Option<HitTarget> = None;
    let mut dragging_volume = false;
    let mut dragging_seek = false;
    let mut last_live = Instant::now();

    loop {
        if app.close {
            break;
        }
        let width = app.width as i32;
        let height = app.height as i32;
        let layout = Layout::compute(width, height);
        scroll.set_bounds(
            layout.playlist_vp.h as f32,
            (state.tracks.len() as i32 * PLAYLIST_ROW_H) as f32,
            layout.playlist_sb,
        );

        if state.poll() {
            app.needs_redraw = true;
        }
        if scroll.is_animating() || ui::has_active_animations() {
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            draw_player(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &app.config,
                &state,
                &layout,
                &mut scroll,
                hover,
                dragging_volume,
                dragging_seek,
                width,
                height,
            );
            app.draw_chrome();
            app.present()?;
            last_live = Instant::now();
        } else if state.play_state == PlayState::Playing
            && last_live.elapsed() >= Duration::from_millis(80)
        {
            // Repaint only the spectrum and live progress strip so playback
            // animations never trigger a full-window X11 upload every 80 ms.
            draw_live_regions(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                &state,
                &layout,
                hover == Some(HitTarget::ProgressBar) || dragging_seek,
            );
            app.needs_redraw = true;
            app.present()?;
            last_live = Instant::now();
        }

        let timeout = if ui::has_active_animations() || scroll.is_animating() || dragging_seek {
            16
        } else if state.play_state == PlayState::Playing {
            75
        } else {
            200
        };

        for event in app.pump_with_fd(None, timeout)?.0 {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Wheel { x, y, delta } => {
                    if layout.playlist_vp.contains(x, y) && scroll.wheel(delta as f32 * 24.0) {
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Motion { x, y, .. } => {
                    if dragging_volume {
                        let vol = volume_from_slider(layout.vol_slider, x);
                        if vol != app.config.sound_master_volume || app.config.sound_muted {
                            stage_volume(&mut app, vol, false);
                            app.needs_redraw = true;
                        }
                        app.set_content_cursor(CursorKind::Pointer);
                        continue;
                    }
                    if dragging_seek {
                        if let Some(dur) = state.current_track().meta.duration_ms {
                            let target = seek_ms_from_x(layout.progress_bar, x, dur);
                            state.seek_to(target);
                            app.needs_redraw = true;
                        }
                        app.set_content_cursor(CursorKind::Pointer);
                        continue;
                    }
                    if scroll.handle_motion(x, y) {
                        app.needs_redraw = true;
                    }
                    let next_hover = if !scroll.is_dragging() {
                        layout.hit_test(x, y, scroll.offset_i32(), state.tracks.len())
                    } else {
                        None
                    };
                    let want_pointer = scroll.is_hovered()
                        || scroll.is_dragging()
                        || matches!(
                            next_hover,
                            Some(
                                HitTarget::Prev
                                    | HitTarget::PlayPause
                                    | HitTarget::Stop
                                    | HitTarget::Next
                                    | HitTarget::Repeat
                                    | HitTarget::Mute
                                    | HitTarget::VolumeSlider
                                    | HitTarget::ProgressBar
                                    | HitTarget::TrackRow(_)
                            )
                        );
                    app.set_content_cursor(if want_pointer {
                        CursorKind::Pointer
                    } else {
                        CursorKind::Default
                    });
                    if next_hover != hover {
                        hover = next_hover;
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
                    if !down {
                        if dragging_volume {
                            dragging_volume = false;
                            commit_volume(&mut app);
                            play_sound(&app.config, SystemSound::Volume);
                            app.needs_redraw = true;
                        }
                        if dragging_seek {
                            dragging_seek = false;
                            app.needs_redraw = true;
                        }
                        if scroll.handle_button(x, y, 1, false) {
                            app.needs_redraw = true;
                        }
                        continue;
                    }
                    if scroll.handle_button(x, y, 1, true) {
                        app.needs_redraw = true;
                        continue;
                    }
                    if let Some(target) =
                        layout.hit_test(x, y, scroll.offset_i32(), state.tracks.len())
                    {
                        match target {
                            HitTarget::Prev => {
                                state.prev_track();
                                play_sound(&app.config, SystemSound::Click);
                            }
                            HitTarget::PlayPause => {
                                state.toggle_play_pause();
                                play_sound(&app.config, SystemSound::Click);
                            }
                            HitTarget::Stop => {
                                state.stop();
                                play_sound(&app.config, SystemSound::Click);
                            }
                            HitTarget::Next => {
                                state.next_track();
                                play_sound(&app.config, SystemSound::Click);
                            }
                            HitTarget::Repeat => {
                                state.repeat_mode = state.repeat_mode.next();
                                play_sound(&app.config, SystemSound::Click);
                            }
                            HitTarget::Mute => {
                                let muted = !app.config.sound_muted;
                                app.set_config_property("sound", "/muted", Value::Bool(muted));
                                apply_mixer_volume(app.config.sound_master_volume, muted);
                                play_sound(&app.config, SystemSound::Volume);
                            }
                            HitTarget::VolumeSlider => {
                                dragging_volume = true;
                                let vol = volume_from_slider(layout.vol_slider, x);
                                stage_volume(&mut app, vol, false);
                            }
                            HitTarget::ProgressBar => {
                                if let Some(dur) = state.current_track().meta.duration_ms {
                                    dragging_seek = true;
                                    let target_ms = seek_ms_from_x(layout.progress_bar, x, dur);
                                    state.seek_to(target_ms);
                                }
                            }
                            HitTarget::TrackRow(idx) => {
                                if idx == state.current && state.play_state != PlayState::Stopped {
                                    state.toggle_play_pause();
                                } else {
                                    state.select_track(idx, true);
                                }
                                play_sound(&app.config, SystemSound::Click);
                            }
                        }
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Key(key) => {
                    let mut handled = true;
                    match key.sym {
                        0x20 => state.toggle_play_pause(),
                        0xff51 => state.seek_relative(-SEEK_STEP_MS),
                        0xff53 => state.seek_relative(SEEK_STEP_MS),
                        0x6d | 0x4d => {
                            let muted = !app.config.sound_muted;
                            app.set_config_property("sound", "/muted", Value::Bool(muted));
                            apply_mixer_volume(app.config.sound_master_volume, muted);
                        }
                        _ => handled = false,
                    }
                    if handled {
                        app.needs_redraw = true;
                    }
                }
                _ => {}
            }
        }
    }

    state.stop();
    Ok(())
}

fn volume_from_slider(rect: Rect, x: i32) -> i32 {
    let t = ((x - rect.x) as f32 / rect.w.max(1) as f32).clamp(0.0, 1.0);
    (t * 100.0).round() as i32
}

fn seek_ms_from_x(rect: Rect, x: i32, duration_ms: u64) -> u64 {
    let t = ((x - rect.x) as f32 / rect.w.max(1) as f32).clamp(0.0, 1.0);
    ((duration_ms as f64) * f64::from(t)).round() as u64
}

fn stage_volume(app: &mut App, vol: i32, persist: bool) {
    let clamped = vol.clamp(0, 100);
    if clamped != app.config.sound_master_volume {
        let value = Value::Int(i64::from(clamped));
        if persist {
            app.set_config_property("sound", "/master-volume", value);
        } else {
            app.stage_config_property("sound", "/master-volume", value);
        }
    }
    if app.config.sound_muted && clamped > 0 {
        if persist {
            app.set_config_property("sound", "/muted", Value::Bool(false));
        } else {
            app.stage_config_property("sound", "/muted", Value::Bool(false));
        }
    }
    let muted_eff = app.config.sound_muted && clamped == 0;
    apply_mixer_volume(clamped, muted_eff);
}

fn commit_volume(app: &mut App) {
    let vol = app.config.sound_master_volume;
    let muted = app.config.sound_muted;
    app.set_config_property("sound", "/master-volume", Value::Int(i64::from(vol)));
    app.set_config_property("sound", "/muted", Value::Bool(muted));
}

#[allow(clippy::too_many_arguments)]
fn draw_player(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    config: &uxconf::DesktopConfig,
    state: &PlayerState,
    layout: &Layout,
    scroll: &mut ui::SmoothScroll,
    hover: Option<HitTarget>,
    dragging_volume: bool,
    dragging_seek: bool,
    width: i32,
    height: i32,
) {
    canvas.clear(theme.window);

    // Top Now-Playing Card
    ui::card(canvas, theme, layout.album);
    let art = layout.art;
    canvas.rounded_rect(art, 8, theme.accent, 1.0);
    canvas.circle(
        art.x as f32 + art.w as f32 * 0.5,
        art.y as f32 + art.h as f32 * 0.5,
        40.0,
        theme.window_high,
        0.24,
    );
    text.draw_centered(
        canvas,
        "♪",
        art.x as f32 + art.w as f32 * 0.5,
        art.y as f32 + art.h as f32 * 0.5 + 16.0,
        46.0,
        0xffffff,
        Face::Bold,
    );

    let track = state.current_track();
    let title_x = (art.right() + 18) as f32;
    let title_max_w = (layout.album.right() - 16 - (art.right() + 18)).max(60) as f32;
    let title_str = text.truncate(&track.title, title_max_w, 19.5, Face::Bold);
    text.draw(
        canvas,
        &title_str,
        title_x,
        art.y as f32 + 24.0,
        19.5,
        theme.text,
        Face::Bold,
    );

    let sub_line = match (track.meta.rate, track.meta.channels) {
        (Some(rate), Some(ch)) => {
            let ch_str = if ch == 2 { "立体声" } else { "单声道" };
            format!(
                "{} · {:.1} kHz · {}",
                track.subtitle,
                rate as f32 / 1000.0,
                ch_str
            )
        }
        _ => track.subtitle.clone(),
    };
    let sub_str = text.truncate(&sub_line, title_max_w, 11.5, Face::Ui);
    text.draw(
        canvas,
        &sub_str,
        title_x,
        art.y as f32 + 44.0,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    let status_color = if state.status_error {
        theme.danger
    } else if state.play_state == PlayState::Playing {
        theme.accent
    } else {
        theme.text_dim
    };
    let status_str = text.truncate(&state.status, title_max_w, 12.0, Face::Ui);
    text.draw(
        canvas,
        &status_str,
        title_x,
        art.y as f32 + 64.0,
        12.0,
        status_color,
        Face::Ui,
    );

    draw_spectrum(canvas, theme, state, layout.spectrum, false);
    draw_progress_row(
        canvas,
        text,
        theme,
        state,
        layout,
        hover == Some(HitTarget::ProgressBar) || dragging_seek,
        false,
    );

    // Transport Controls + Volume Cluster
    draw_transport_button(
        canvas,
        theme,
        layout.prev,
        GlyphKind::Prev,
        false,
        hover == Some(HitTarget::Prev),
    );
    draw_transport_button(
        canvas,
        theme,
        layout.play_pause,
        if state.play_state == PlayState::Playing {
            GlyphKind::Pause
        } else {
            GlyphKind::Play
        },
        true,
        hover == Some(HitTarget::PlayPause),
    );
    draw_transport_button(
        canvas,
        theme,
        layout.stop,
        GlyphKind::Stop,
        false,
        hover == Some(HitTarget::Stop),
    );
    draw_transport_button(
        canvas,
        theme,
        layout.next,
        GlyphKind::Next,
        false,
        hover == Some(HitTarget::Next),
    );
    ui::button(
        canvas,
        text,
        theme,
        layout.repeat,
        state.repeat_mode.label(),
        None,
        false,
        hover == Some(HitTarget::Repeat),
    );

    draw_mute_button(
        canvas,
        theme,
        layout.mute,
        config.sound_muted || config.sound_master_volume == 0,
        hover == Some(HitTarget::Mute),
    );
    let vol_eff = if config.sound_muted {
        0
    } else {
        config.sound_master_volume.clamp(0, 100)
    };
    ui::slider(
        canvas,
        theme,
        layout.vol_slider,
        vol_eff as f32 / 100.0,
        hover == Some(HitTarget::VolumeSlider),
        dragging_volume,
    );
    let vol_label = format!("{vol_eff}%");
    text.draw_right(
        canvas,
        &vol_label,
        (layout.vol_text_x + 34) as f32,
        layout.vol_slider.y as f32 + 15.0,
        11.5,
        theme.text_dim,
        Face::Mono,
    );

    // Playlist Card
    ui::card(canvas, theme, layout.playlist_card);
    let hdr = format!("播放列表（{} 首）", state.tracks.len());
    text.draw(
        canvas,
        &hdr,
        layout.playlist_card.x as f32 + 14.0,
        layout.playlist_card.y as f32 + 22.0,
        12.5,
        theme.text_dim,
        Face::Bold,
    );

    let scroll_off = scroll.offset_i32();
    let vp = layout.playlist_vp;
    canvas.with_clip(vp, |cv| {
        for (idx, item) in state.tracks.iter().enumerate() {
            let row = Rect::new(
                vp.x,
                vp.y + idx as i32 * PLAYLIST_ROW_H - scroll_off,
                vp.w,
                PLAYLIST_ROW_H - 4,
            );
            if row.bottom() <= vp.y || row.y >= vp.bottom() {
                continue;
            }
            let selected = idx == state.current;
            let is_hover = hover == Some(HitTarget::TrackRow(idx));
            if selected {
                cv.rounded_rect(row, 5, theme.accent_soft, 1.0);
                cv.rounded_border(row, 5, theme.accent, 0.55, 1);
            } else if is_hover {
                cv.rounded_rect(row, 5, theme.panel_hover, 0.85);
            }

            let badge = if selected && state.play_state == PlayState::Playing {
                "▶"
            } else if selected && state.play_state == PlayState::Paused {
                "⏸"
            } else {
                ""
            };
            if badge.is_empty() {
                let num = format!("{:02}", idx + 1);
                text.draw(
                    cv,
                    &num,
                    row.x as f32 + 10.0,
                    row.y as f32 + 23.0,
                    11.5,
                    theme.text_muted,
                    Face::Mono,
                );
            } else {
                text.draw(
                    cv,
                    badge,
                    row.x as f32 + 10.0,
                    row.y as f32 + 23.0,
                    12.0,
                    theme.accent,
                    Face::Bold,
                );
            }

            let dur_str = item
                .meta
                .duration_ms
                .map_or_else(|| String::from("--:--"), format_duration_ms);
            text.draw_right(
                cv,
                &dur_str,
                row.right() as f32 - 12.0,
                row.y as f32 + 23.0,
                11.5,
                theme.text_muted,
                Face::Mono,
            );

            let text_left = row.x + 34;
            let text_max = (row.w - 34 - 56).max(40) as f32;
            let row_title = text.truncate(&item.title, text_max, 12.5, Face::Bold);
            text.draw(
                cv,
                &row_title,
                text_left as f32,
                row.y as f32 + 17.0,
                12.5,
                if selected { theme.text } else { theme.text_dim },
                Face::Bold,
            );
            let row_sub = text.truncate(&item.subtitle, text_max, 10.5, Face::Ui);
            text.draw(
                cv,
                &row_sub,
                text_left as f32,
                row.y as f32 + 31.0,
                10.5,
                theme.text_muted,
                Face::Ui,
            );
        }
    });
    scroll.draw_v(
        canvas,
        theme,
        layout.playlist_sb,
        layout.playlist_vp.h as f32,
        (state.tracks.len() as i32 * PLAYLIST_ROW_H) as f32,
    );

    // Footer hints
    let hint = text.truncate(
        "空格 播放/暂停 · ←/→ 跳转 5s · M 静音 · 点击进度条拖动定位",
        (width - 36).max(80) as f32,
        11.0,
        Face::Ui,
    );
    text.draw(
        canvas,
        &hint,
        18.0,
        height as f32 - 10.0,
        11.0,
        theme.text_muted,
        Face::Ui,
    );
}

fn draw_live_regions(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &PlayerState,
    layout: &Layout,
    progress_hot: bool,
) {
    draw_spectrum(canvas, theme, state, layout.spectrum, true);
    draw_progress_row(canvas, text, theme, state, layout, progress_hot, true);
}

fn draw_spectrum(
    canvas: &mut Canvas,
    theme: &Theme,
    state: &PlayerState,
    rect: Rect,
    clear_bg: bool,
) {
    if clear_bg {
        canvas.fill(rect, theme.window_high);
        canvas.mark_dirty(rect);
    }
    let bar_slot = (rect.w / SPECTRUM_BARS as i32).max(4);
    let bar_w = (bar_slot - 3).max(2);
    let t = state.current_position_ms() as f32 / 1000.0;
    let active = state.play_state == PlayState::Playing;
    let paused = state.play_state == PlayState::Paused;
    canvas.with_clip(rect, |cv| {
        for i in 0..SPECTRUM_BARS {
            let phase = t * 5.2 + i as f32 * 0.61;
            let wave = if active || paused {
                let harmonic = ((phase * 1.7 + i as f32 * 0.27).sin() * 0.5 + 0.5) * 0.35;
                ((phase.sin() * 0.5 + 0.5).powi(2) * 0.65 + harmonic).clamp(0.08, 1.0)
            } else {
                0.08
            };
            let max_h = (rect.h - 4).max(8) as f32;
            let bar_h = (5.0 + wave * (max_h - 5.0)).round() as i32;
            let x = rect.x + i as i32 * bar_slot;
            let bar = Rect::new(x, rect.bottom() - bar_h, bar_w, bar_h);
            let alpha = if active {
                0.90
            } else if paused {
                0.45
            } else {
                0.28
            };
            cv.rounded_rect(bar, 2, theme.accent, alpha);
        }
    });
}

fn draw_progress_row(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &PlayerState,
    layout: &Layout,
    hot: bool,
    clear_bg: bool,
) {
    let row = layout.progress_row;
    if clear_bg {
        canvas.fill(row, theme.window_high);
        canvas.mark_dirty(row);
    }
    let cur_ms = state.current_position_ms();
    let total_ms = state.current_track().meta.duration_ms;
    let frac = match total_ms {
        Some(total) if total > 0 => (cur_ms as f32 / total as f32).clamp(0.0, 1.0),
        _ => 0.0,
    };

    let cur_str = format_duration_ms(cur_ms);
    let tot_str = total_ms.map_or_else(|| String::from("--:--"), format_duration_ms);
    let base_y = row.y as f32 + 17.0;
    text.draw(
        canvas,
        &cur_str,
        row.x as f32,
        base_y,
        11.0,
        theme.text_dim,
        Face::Mono,
    );
    text.draw_right(
        canvas,
        &tot_str,
        row.right() as f32,
        base_y,
        11.0,
        theme.text_muted,
        Face::Mono,
    );

    let bar = layout.progress_bar;
    let track_h = 5;
    let track = Rect::new(bar.x, bar.y + (bar.h - track_h) / 2, bar.w, track_h);
    canvas.rounded_rect(track, 2, theme.window_alt, 1.0);
    canvas.rounded_border(track, 2, theme.window_border, 1.0, 1);

    let fill_w = ((track.w as f32) * frac).round() as i32;
    if fill_w > 0 {
        let filled = Rect::new(track.x, track.y, fill_w.min(track.w), track_h);
        canvas.rounded_rect(filled, 2, theme.accent, 1.0);
    }
    if hot || state.play_state != PlayState::Stopped {
        let knob_d = if hot { 12 } else { 9 };
        let kx = (track.x + fill_w).clamp(track.x, track.right()) - knob_d / 2;
        let ky = track.y + track_h / 2 - knob_d / 2;
        canvas.rounded_rect(Rect::new(kx, ky, knob_d, knob_d), knob_d / 2, 0xffffff, 1.0);
        canvas.rounded_border(
            Rect::new(kx, ky, knob_d, knob_d),
            knob_d / 2,
            theme.accent,
            if hot { 0.9 } else { 0.5 },
            1,
        );
    }
}

#[derive(Clone, Copy)]
enum GlyphKind {
    Prev,
    Play,
    Pause,
    Stop,
    Next,
}

fn draw_transport_button(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    kind: GlyphKind,
    primary: bool,
    hover: bool,
) {
    let base = if primary {
        theme.accent
    } else {
        theme.panel_high
    };
    let bg = if hover {
        if theme.is_dark() {
            mix(base, 0xffffff, 0.14)
        } else {
            mix(base, 0x000000, 0.08)
        }
    } else {
        base
    };
    let radius = if primary { rect.h / 2 } else { 6 };
    canvas.rounded_rect(rect, radius, bg, 1.0);
    if !primary {
        canvas.rounded_border(rect, radius, theme.window_border, 1.0, 1);
    }
    let fg = if primary { 0xffffff } else { theme.text };
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    match kind {
        GlyphKind::Play => {
            fill_triangle(
                canvas,
                [(cx - 5, cy - 8), (cx + 8, cy), (cx - 5, cy + 8)],
                fg,
            );
        }
        GlyphKind::Pause => {
            canvas.fill(Rect::new(cx - 5, cy - 7, 3, 14), fg);
            canvas.fill(Rect::new(cx + 2, cy - 7, 3, 14), fg);
        }
        GlyphKind::Stop => {
            canvas.fill(Rect::new(cx - 5, cy - 5, 10, 10), fg);
        }
        GlyphKind::Prev => {
            canvas.fill(Rect::new(cx - 7, cy - 6, 2, 12), fg);
            fill_triangle(
                canvas,
                [(cx + 6, cy - 6), (cx - 3, cy), (cx + 6, cy + 6)],
                fg,
            );
        }
        GlyphKind::Next => {
            fill_triangle(
                canvas,
                [(cx - 6, cy - 6), (cx + 3, cy), (cx - 6, cy + 6)],
                fg,
            );
            canvas.fill(Rect::new(cx + 5, cy - 6, 2, 12), fg);
        }
    }
}

fn draw_mute_button(canvas: &mut Canvas, theme: &Theme, rect: Rect, muted: bool, hover: bool) {
    let bg = if hover {
        theme.panel_hover
    } else {
        theme.panel_high
    };
    canvas.rounded_rect(rect, 5, bg, 1.0);
    canvas.rounded_border(rect, 5, theme.window_border, 1.0, 1);
    let fg = if muted { theme.danger } else { theme.text };
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2;
    canvas.fill(Rect::new(cx - 7, cy - 3, 4, 6), fg);
    fill_triangle(
        canvas,
        [(cx - 4, cy - 3), (cx + 1, cy - 6), (cx + 1, cy + 6)],
        fg,
    );
    fill_triangle(
        canvas,
        [(cx - 4, cy - 3), (cx - 4, cy + 3), (cx + 1, cy + 6)],
        fg,
    );
    if muted {
        canvas.line(cx + 3, cy - 4, cx + 8, cy + 4, theme.danger, 1.0);
        canvas.line(cx + 8, cy - 4, cx + 3, cy + 4, theme.danger, 1.0);
    } else {
        canvas.fill(Rect::new(cx + 3, cy - 3, 2, 6), fg);
        canvas.fill(Rect::new(cx + 6, cy - 5, 2, 10), fg);
    }
}

/// Horizontal scanline rasterizer for solid triangles (play/prev/next/speaker glyphs).
fn fill_triangle(canvas: &mut Canvas, pts: [(i32, i32); 3], color: u32) {
    let mut v = pts;
    v.sort_by_key(|p| p.1);
    let [(x0, y0), (x1, y1), (x2, y2)] = v;
    if y0 == y2 {
        return;
    }
    let edge_x = |ax: i32, ay: i32, bx: i32, by: i32, y: i32| -> f32 {
        if by == ay {
            ax as f32
        } else {
            ax as f32 + (bx - ax) as f32 * (y - ay) as f32 / (by - ay) as f32
        }
    };
    for y in y0..=y2 {
        let xa = edge_x(x0, y0, x2, y2, y);
        let xb = if y < y1 || y1 == y2 {
            edge_x(x0, y0, x1, y1, y)
        } else {
            edge_x(x1, y1, x2, y2, y)
        };
        let left = xa.min(xb).floor() as i32;
        let right = xa.max(xb).ceil() as i32;
        if right >= left {
            canvas.fill(Rect::new(left, y, right - left + 1, 1), color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mp3play_info_output() {
        let parsed = parse_mp3_info_line("duration_ms=245120 rate=44100 channels=2\n")
            .expect("valid info output");
        assert_eq!(parsed.duration_ms, Some(245_120));
        assert_eq!(parsed.rate, Some(44_100));
        assert_eq!(parsed.channels, Some(2));
        assert!(parse_mp3_info_line("mp3play: cannot read").is_none());
    }

    #[test]
    fn formats_timestamps_consistently() {
        assert_eq!(format_duration_ms(0), "00:00");
        assert_eq!(format_duration_ms(59_999), "00:59");
        assert_eq!(format_duration_ms(61_000), "01:01");
        assert_eq!(format_duration_ms(3_661_000), "1:01:01");
    }

    #[test]
    fn repeat_mode_cycles_and_auto_advances_safely() {
        assert_eq!(RepeatMode::Sequential.next(), RepeatMode::RepeatAll);
        assert_eq!(RepeatMode::RepeatAll.next(), RepeatMode::RepeatOne);
        assert_eq!(RepeatMode::RepeatOne.next(), RepeatMode::Sequential);

        assert_eq!(
            advance_track_after_exit(0, 3, RepeatMode::Sequential, true, 0),
            Some(1)
        );
        assert_eq!(
            advance_track_after_exit(2, 3, RepeatMode::Sequential, true, 0),
            None
        );
        assert_eq!(
            advance_track_after_exit(2, 3, RepeatMode::RepeatAll, true, 0),
            Some(0)
        );
        assert_eq!(
            advance_track_after_exit(1, 3, RepeatMode::RepeatOne, true, 0),
            Some(1)
        );
        // Broken tracks skip forward up to track_count - 1 failures, then halt.
        assert_eq!(
            advance_track_after_exit(0, 2, RepeatMode::RepeatAll, false, 1),
            Some(1)
        );
        assert_eq!(
            advance_track_after_exit(1, 2, RepeatMode::RepeatAll, false, 2),
            None
        );
    }
}
