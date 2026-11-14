/*
 *
 *       user/desktop/src/apps/power_splash.rs
 *       Windows-style ProgressRing shutdown/reboot splash with cyberpunk log HUD
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use crate::ddm::wire::PowerAction;
use crate::draw::{mix, Canvas, Rect};
use crate::text::{Face, TextRenderer};
use crate::x11::{CursorKind, X};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use x11rb::protocol::xproto::EventMask;

pub const SHUTDOWN_LOG_PATH: &str = "/run/systemg-shutdown.log";

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn handle_stop_signal(_sig: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

#[inline]
fn cubic_coord(t: f32, p1: f32, p2: f32) -> f32 {
    let inv = 1.0 - t;
    3.0 * inv * inv * t * p1 + 3.0 * inv * t * t * p2 + t * t * t
}

#[inline]
fn cubic_deriv(t: f32, p1: f32, p2: f32) -> f32 {
    let inv = 1.0 - t;
    3.0 * inv * inv * p1 + 6.0 * inv * t * (p2 - p1) + 3.0 * t * t * (1.0 - p2)
}

/// Evaluate CSS-style `cubic-bezier(x1, y1, x2, y2)` at progress `x in [0, 1]`.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let mut t = x;
    for _ in 0..8 {
        let err = cubic_coord(t, x1, x2) - x;
        if err.abs() < 1e-5 {
            return cubic_coord(t, y1, y2);
        }
        let d = cubic_deriv(t, x1, x2);
        if d.abs() < 1e-5 {
            break;
        }
        t = (t - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    t = x;
    for _ in 0..16 {
        let v = cubic_coord(t, x1, x2);
        if (v - x).abs() < 1e-5 {
            break;
        }
        if v < x {
            lo = t;
        } else {
            hi = t;
        }
        t = 0.5 * (lo + hi);
    }
    cubic_coord(t, y1, y2)
}

/// Evaluate one dot of a 6-dot Windows 8/10 `ProgressRing` orbit curve.
/// Returns `(angle_degrees_from_top_cw, opacity_0_to_1)` for elapsed `elapsed_s`
/// and zero-based dot index `0..6`.
pub fn progress_ring_dot(elapsed_s: f32, dot_index: usize) -> (f32, f32) {
    const PERIOD_S: f32 = 3.8;
    const STAGGER_S: f32 = 0.19;

    let shifted = elapsed_s - (dot_index as f32) * STAGGER_S;
    if shifted < 0.0 {
        return (225.0, 0.0);
    }
    let p = (shifted % PERIOD_S) / PERIOD_S;

    let angle = if p < 0.07 {
        let k = cubic_bezier(0.1, 0.9, 0.2, 1.0, p / 0.07);
        225.0 + (345.0 - 225.0) * k
    } else if p < 0.30 {
        let k = (p - 0.07) / 0.23;
        345.0 + (455.0 - 345.0) * k
    } else if p < 0.39 {
        let k = cubic_bezier(0.4, 0.0, 0.9, 0.7, (p - 0.30) / 0.09);
        455.0 + (690.0 - 455.0) * k
    } else if p < 0.70 {
        let k = cubic_bezier(0.0, 0.0, 0.2, 1.0, (p - 0.39) / 0.31);
        690.0 + (810.0 - 690.0) * k
    } else if p < 0.75 {
        let k = (p - 0.70) / 0.05;
        810.0 + (840.0 - 810.0) * k
    } else {
        840.0
    };

    let opacity = if p < 0.025 {
        (p / 0.025).clamp(0.0, 1.0)
    } else if p < 0.70 {
        1.0
    } else if p < 0.75 {
        (1.0 - (p - 0.70) / 0.05).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (angle, opacity)
}

#[derive(Clone)]
struct LogLine {
    text: String,
    accent: u32,
}

fn classify_log_line(raw: &str) -> u32 {
    if raw.contains("[WARN") || raw.contains("timed out") || raw.contains("Deferred") {
        0xfbbf24
    } else if raw.contains("[DONE") || raw.contains("[TARGET") || raw.contains("Reached target") {
        0x34d399
    } else if raw.contains("[UMOUNT") || raw.contains("Unmounted") {
        0x818cf8
    } else if raw.contains("[SYNC") {
        0x38bdf8
    } else if raw.contains("[STOP") || raw.contains("[OK") {
        0x6ee7b7
    } else {
        0x94a3b8
    }
}

struct LogTailer {
    offset: u64,
    partial: String,
    lines: Vec<LogLine>,
    done: bool,
}

impl LogTailer {
    fn new() -> Self {
        Self {
            offset: 0,
            partial: String::new(),
            lines: Vec::with_capacity(64),
            done: false,
        }
    }

    fn poll(&mut self) {
        let Ok(mut file) = File::open(SHUTDOWN_LOG_PATH) else {
            return;
        };
        if let Ok(meta) = file.metadata() {
            if meta.len() < self.offset {
                self.offset = 0;
                self.partial.clear();
                self.lines.clear();
            }
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() || buf.is_empty() {
            return;
        }
        self.offset += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));

        while let Some(pos) = self.partial.find('\n') {
            let line = self.partial[..pos].trim_end_matches('\r').to_string();
            self.partial.drain(..=pos);
            if line.is_empty() {
                continue;
            }
            if line.contains("[DONE")
                || line.contains("poweroff.target")
                || line.contains("reboot.target")
            {
                self.done = true;
            }
            self.lines.push(LogLine {
                accent: classify_log_line(&line),
                text: line,
            });
        }
        if self.lines.len() > 200 {
            let drain = self.lines.len() - 200;
            self.lines.drain(..drain);
        }
    }
}

fn draw_backdrop(canvas: &mut Canvas) {
    let bounds = canvas.bounds();
    canvas.vertical_gradient(bounds, 0x040812, 0x09152b);

    // Subtle cyberpunk grid lines.
    let step = 52;
    let mut gx = step;
    while gx < bounds.w {
        canvas.fill_alpha(Rect::new(gx, 0, 1, bounds.h), 0x1e3a5f, 0.22);
        gx += step;
    }
    let mut gy = step;
    while gy < bounds.h {
        canvas.fill_alpha(Rect::new(0, gy, bounds.w, 1), 0x1e3a5f, 0.22);
        gy += step;
    }

    // Corner HUD calibration brackets.
    let m = 24;
    let arm = 26;
    let col = 0x22d3ee;
    for &(cx, cy, sx, sy) in &[
        (m, m, 1, 1),
        (bounds.w - m - 1, m, -1, 1),
        (m, bounds.h - m - 1, 1, -1),
        (bounds.w - m - 1, bounds.h - m - 1, -1, -1),
    ] {
        canvas.line(cx, cy, cx + sx * arm, cy, col, 0.72);
        canvas.line(cx, cy, cx, cy + sy * arm, col, 0.72);
    }
}

fn draw_progress_ring(canvas: &mut Canvas, cx: f32, cy: f32, elapsed_s: f32, done: bool) {
    let orbit_r = 30.0f32;
    let ring_col = if done { 0x34d399 } else { 0x38bdf8 };
    canvas.ring(cx, cy, orbit_r + 16.0, 1.5, ring_col, 0.22);

    // Subtle rotating outer reticle ticks.
    let rot_offset = elapsed_s * 24.0;
    for k in 0..12 {
        let ang = ((k as f32) * 30.0 + rot_offset).to_radians();
        let inner = orbit_r + 18.0;
        let outer = if k % 3 == 0 {
            orbit_r + 26.0
        } else {
            orbit_r + 22.0
        };
        canvas.line(
            (cx + inner * ang.cos()).round() as i32,
            (cy + inner * ang.sin()).round() as i32,
            (cx + outer * ang.cos()).round() as i32,
            (cy + outer * ang.sin()).round() as i32,
            ring_col,
            if k % 3 == 0 { 0.55 } else { 0.24 },
        );
    }

    for i in 0..6 {
        let (deg, alpha) = progress_ring_dot(elapsed_s, i);
        if alpha <= 0.01 {
            continue;
        }
        let rad = (deg - 90.0).to_radians();
        let px = cx + orbit_r * rad.cos();
        let py = cy + orbit_r * rad.sin();
        let dot_col = mix(0xe0f2fe, ring_col, 0.28);
        canvas.circle(px, py, 7.6, ring_col, alpha * 0.22);
        canvas.circle(px, py, 4.1, dot_col, alpha);
    }
}

fn draw_frame(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    action: PowerAction,
    elapsed_s: f32,
    tailer: &LogTailer,
) {
    draw_backdrop(canvas);
    let w = canvas.bounds().w;
    let h = canvas.bounds().h;
    let cx = (w as f32) * 0.5;
    let is_reboot = matches!(action, PowerAction::Reboot);
    let accent = if tailer.done {
        0x34d399
    } else if is_reboot {
        0x22d3ee
    } else {
        0x38bdf8
    };

    // Top telemetry HUD header.
    let header_y = 46.0f32;
    text.draw(
        canvas,
        "SPACEOS // POWER SEQUENCE",
        40.0,
        header_y,
        16.0,
        0x38bdf8,
        Face::CyberDisplay,
    );
    let mode_tag = if is_reboot {
        "MODE: WARM-REBOOT [RB_AUTOBOOT]"
    } else {
        "MODE: ACPI-S5 [RB_POWER_OFF]"
    };
    text.draw(
        canvas,
        mode_tag,
        40.0,
        header_y + 20.0,
        12.5,
        0x64748b,
        Face::CyberMono,
    );

    let tele = format!("T+{elapsed_s:06.2}s   EVENTS:{:03}", tailer.lines.len());
    text.draw_right(
        canvas,
        &tele,
        (w - 40) as f32,
        header_y,
        14.0,
        0x7dd3fc,
        Face::CyberMono,
    );
    text.draw_right(
        canvas,
        if tailer.done {
            "STAGE: KERNEL HANDOFF"
        } else {
            "STAGE: SERVICE & VFS TEARDOWN"
        },
        (w - 40) as f32,
        header_y + 18.0,
        12.0,
        accent,
        Face::CyberMono,
    );

    // Center Windows ProgressRing + Cyber Reticle.
    let panel_top = ((h as f32) * 0.53).round() as i32;
    let ring_cy = ((panel_top as f32) * 0.42).max(150.0);
    draw_progress_ring(canvas, cx, ring_cy, elapsed_s, tailer.done);

    let title_cn = if is_reboot {
        "正在重新启动 SpaceOS"
    } else {
        "正在关闭计算机"
    };
    text.draw_centered(
        canvas,
        title_cn,
        cx,
        ring_cy + 88.0,
        24.0,
        0xf1f5f9,
        Face::Bold,
    );
    let subtitle = if tailer.done {
        "TARGET REACHED // READY FOR KERNEL HANDOFF"
    } else if is_reboot {
        "STOPPING UNITS & SYNCING FILESYSTEMS BEFORE REBOOT..."
    } else {
        "STOPPING UNITS & SYNCING FILESYSTEMS BEFORE POWEROFF..."
    };
    text.draw_centered(
        canvas,
        subtitle,
        cx,
        ring_cy + 114.0,
        13.0,
        accent,
        Face::CyberMono,
    );

    // Bottom cyberpunk telemetry console box.
    let box_w = (w - 96).clamp(480, 960);
    let box_x = (w - box_w) / 2;
    let box_h = (h - panel_top - 40).max(160);
    let log_rect = Rect::new(box_x, panel_top, box_w, box_h);

    canvas.rounded_rect(log_rect, 8, 0x050b18, 0.90);
    canvas.rounded_border(log_rect, 8, accent, 0.50, 1);

    let bar = Rect::new(box_x + 1, panel_top + 1, box_w - 2, 30);
    canvas.fill_alpha(bar, 0x0f1f3a, 0.92);
    canvas.fill_alpha(
        Rect::new(box_x + 1, panel_top + 31, box_w - 2, 1),
        accent,
        0.35,
    );

    text.draw(
        canvas,
        "SYSTEMG PID-1 // SHUTDOWN TELEMETRY",
        (box_x + 14) as f32,
        (panel_top + 20) as f32,
        13.0,
        0x7dd3fc,
        Face::CyberDisplay,
    );
    text.draw_right(
        canvas,
        SHUTDOWN_LOG_PATH,
        (box_x + box_w - 14) as f32,
        (panel_top + 20) as f32,
        11.5,
        0x64748b,
        Face::CyberMono,
    );

    let body = Rect::new(box_x + 12, panel_top + 38, box_w - 24, box_h - 46);
    let line_h = 19;
    let max_lines = (body.h / line_h).max(1) as usize;
    let start_idx = tailer.lines.len().saturating_sub(max_lines);
    let max_w = (body.w - 8) as f32;

    canvas.with_clip(body, |canvas| {
        if tailer.lines.is_empty() {
            text.draw(
                canvas,
                "[  +0.000s] [INIT  ] Awaiting telemetry stream from systemg PID 1...",
                body.x as f32,
                (body.y + 15) as f32,
                13.0,
                0x94a3b8,
                Face::CyberMono,
            );
            return;
        }
        let mut y = body.y + 15;
        let visible = &tailer.lines[start_idx..];
        let blink_cursor = ((elapsed_s * 2.5) as u32).is_multiple_of(2);
        for (idx, line) in visible.iter().enumerate() {
            let is_last = idx + 1 == visible.len();
            let raw = if is_last && blink_cursor {
                format!("{} _", line.text)
            } else {
                line.text.clone()
            };
            let clipped = text.truncate(&raw, max_w, 12.5, Face::CyberMono);
            text.draw(
                canvas,
                &clipped,
                body.x as f32,
                y as f32,
                12.5,
                line.accent,
                Face::CyberMono,
            );
            y += line_h;
        }
    });
}

/// Entrypoint for `spui power-splash [poweroff|reboot]`.
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let action = match args.first().map(String::as_str) {
        Some("reboot") => PowerAction::Reboot,
        _ => PowerAction::PowerOff,
    };

    // SAFETY: Signal handler only sets an atomic boolean flag.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_stop_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            handle_stop_signal as *const () as libc::sighandler_t,
        );
    }

    let x = X::connect()?;
    let mut text = TextRenderer::load()?;
    let win = x.generate_id()?;
    let mask = EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY;
    x.create_window(win, x.root, 0, 0, x.width, x.height, true, mask, 0x040812)?;
    x.set_title(win, "SpaceOS Shutdown")?;
    x.set_class(win, "spui-power-splash", "SPUI")?;
    let _ = x.set_cursor(win, CursorKind::Default);

    let mut canvas = x.create_canvas(usize::from(x.width), usize::from(x.height), 0x040812);
    let mut tailer = LogTailer::new();
    let start = Instant::now();

    draw_frame(&mut canvas, &mut text, action, 0.0, &tailer);
    let _ = x.put_canvas(win, &canvas, canvas.bounds());
    x.map(win)?;
    x.raise(win)?;
    let _ = x.focus(win);
    x.flush()?;

    while RUNNING.load(Ordering::Relaxed) {
        while let Some(ev) = x.poll()? {
            if let x11rb::protocol::Event::DestroyNotify(_) = ev {
                RUNNING.store(false, Ordering::Relaxed);
                break;
            }
        }
        if !RUNNING.load(Ordering::Relaxed) {
            break;
        }
        tailer.poll();
        let elapsed_s = start.elapsed().as_secs_f32();
        draw_frame(&mut canvas, &mut text, action, elapsed_s, &tailer);
        let _ = x.put_canvas(win, &canvas, canvas.bounds());
        let _ = x.flush();
        std::thread::sleep(Duration::from_millis(25));
    }

    x.release_canvas(&canvas);
    let _ = x.destroy(win);
    let _ = x.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_bezier_hits_endpoints_and_stays_monotone() {
        assert!((cubic_bezier(0.1, 0.9, 0.2, 1.0, 0.0) - 0.0).abs() < 1e-6);
        assert!((cubic_bezier(0.1, 0.9, 0.2, 1.0, 1.0) - 1.0).abs() < 1e-6);
        let mut prev = -0.01f32;
        for step in 0..=20 {
            let x = step as f32 / 20.0;
            let y = cubic_bezier(0.4, 0.0, 0.9, 0.7, x);
            assert!(y >= prev - 1e-4, "non-monotone at x={x}: {y} < {prev}");
            prev = y;
        }
    }

    #[test]
    fn progress_ring_staggers_dots_and_cycles_cleanly() {
        // Dot 0 starts immediately at ~225 deg while delayed dots wait for stagger.
        let (a0, o0) = progress_ring_dot(0.10, 0);
        assert!(a0 > 225.0 && a0 < 400.0);
        assert!(o0 > 0.99);

        let (_, o5_early) = progress_ring_dot(0.10, 5);
        assert_eq!(o5_early, 0.0);

        // Once stagger elapses, dot 5 becomes active too and wraps over period.
        let (a5, o5) = progress_ring_dot(1.30, 5);
        assert!(a5 > 225.0 && o5 > 0.9);
    }
}
