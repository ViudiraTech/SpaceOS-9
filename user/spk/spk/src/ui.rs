//
//       user/spk/spk/src/ui.rs
//       pacman-style progress output
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::io::Write;
use std::time::{Duration, Instant};

/// pacman redraws roughly ten times a second; faster than that only wastes
/// terminal bandwidth and makes the rate estimate jittery.
const REDRAW: Duration = Duration::from_millis(100);
const SPINNER: [&str; 4] = ["-", "\\", "|", "/"];

pub fn progress_json_enabled() -> bool {
    std::env::var_os("SPK_PROGRESS_JSON").is_some()
}

pub fn emit_phase(phase: &str, message: &str) {
    if !progress_json_enabled() {
        return;
    }
    let payload = serde_json::json!({
        "event": "phase",
        "phase": phase,
        "message": message,
    });
    let _ = writeln!(std::io::stderr(), "{payload}");
    let _ = std::io::stderr().flush();
}

pub fn enabled() -> bool {
    if std::env::var_os("SPK_NO_PROGRESS").is_some() {
        return false;
    }
    if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
        return false;
    }
    // Progress goes to stderr so that `spk ... > file` still shows it.
    unsafe { libc::isatty(2) == 1 }
}

fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 20)
        .unwrap_or(80)
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn speed(bytes_per_second: f64) -> String {
    if bytes_per_second <= 0.0 {
        return "-- ".into();
    }
    format!("{}/s", human(bytes_per_second as u64))
}

fn eta(remaining: u64, rate: f64) -> String {
    if rate <= 0.0 {
        return "--:--".into();
    }
    let seconds = (remaining as f64 / rate).round() as u64;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    if limit <= 1 {
        return String::new();
    }
    let mut out: String = text.chars().take(limit - 1).collect();
    out.push('…');
    out
}

fn render(
    label: &str,
    done: u64,
    total: Option<u64>,
    rate: f64,
    cols: usize,
    bytes: bool,
) -> String {
    let percent = match total {
        Some(total) if total > 0 => ((done.min(total) * 100 / total) as usize).min(100),
        _ => 0,
    };
    let tail = format!(" {percent:>3}%");
    let mut head = label.to_string();
    if bytes {
        if let Some(total) = total {
            if total > 0 {
                head = format!(
                    "{label}  {}  {}  {}",
                    human(total),
                    speed(rate),
                    eta(total.saturating_sub(done), rate)
                );
            }
        }
    }
    let budget = cols.saturating_sub(tail.chars().count() + 3);
    let head = truncate(&head, budget.saturating_sub(10));
    let bar_len = budget.saturating_sub(head.chars().count()).clamp(8, 40);
    let filled = bar_len * percent / 100;
    format!(
        "{head} [{}{}]{tail}",
        "#".repeat(filled),
        " ".repeat(bar_len - filled)
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    Bytes,
    Count,
}

pub struct Bar {
    enabled: bool,
    label: String,
    kind: BarKind,
    total: Option<u64>,
    done: u64,
    rate: f64,
    last_pos: u64,
    started: Instant,
    last_draw: Instant,
    finished: bool,
}

impl Bar {
    pub fn new(label: impl Into<String>, total: Option<u64>) -> Self {
        Self::with_kind(label, total, BarKind::Bytes)
    }

    /// A bar over a number of items (files, packages) rather than bytes.
    pub fn counter(label: impl Into<String>, total: u64) -> Self {
        Self::with_kind(label, Some(total), BarKind::Count)
    }

    fn with_kind(label: impl Into<String>, total: Option<u64>, kind: BarKind) -> Self {
        Self {
            enabled: enabled() || progress_json_enabled(),
            label: label.into(),
            kind,
            total,
            done: 0,
            rate: 0.0,
            last_pos: 0,
            started: Instant::now(),
            last_draw: Instant::now(),
            finished: false,
        }
    }

    fn emit_json_progress(&self, rate: f64, done_final: bool) {
        let percent = match self.total {
            Some(total) if total > 0 => ((self.done.min(total) * 100 / total) as usize).min(100),
            _ if done_final => 100,
            _ => 0,
        };
        let kind_str = match self.kind {
            BarKind::Bytes => "download",
            BarKind::Count => "write",
        };
        let payload = serde_json::json!({
            "event": "progress",
            "kind": kind_str,
            "label": self.label,
            "done": self.done,
            "total": self.total,
            "percent": percent,
            "rate_bps": rate.max(0.0) as u64,
        });
        let _ = writeln!(std::io::stderr(), "{payload}");
        let _ = std::io::stderr().flush();
    }

    pub fn advance(&mut self, bytes: u64) {
        self.done += bytes;
        // The final position is drawn by finish(), so skip a redundant frame.
        if self.total.is_some_and(|total| self.done >= total) {
            return;
        }
        if !self.enabled || self.last_draw.elapsed() < REDRAW {
            return;
        }
        self.redraw();
    }

    /// redraw with the current position, recomputing a smoothed rate
    pub fn redraw(&mut self) {
        if !self.enabled {
            return;
        }
        let elapsed = self.last_draw.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            let delta = self.done.saturating_sub(self.last_pos) as f64;
            let instant = delta / elapsed;
            // pacman smooths with (new + 2*old)/3 so the ETA stops jumping.
            self.rate = if self.rate == 0.0 {
                instant
            } else {
                (instant + 2.0 * self.rate) / 3.0
            };
        }
        self.last_pos = self.done;
        self.last_draw = Instant::now();
        if progress_json_enabled() {
            self.emit_json_progress(self.rate, false);
            return;
        }
        let line = render(
            &self.label,
            self.done,
            self.total,
            self.rate,
            width(),
            self.kind == BarKind::Bytes,
        );
        let _ = write!(std::io::stderr(), "\r{line}\x1b[K");
        let _ = std::io::stderr().flush();
    }

    pub fn finish(mut self) {
        if !self.enabled || self.finished {
            return;
        }
        self.finished = true;
        if let Some(total) = self.total {
            self.done = self.done.max(total);
        }
        let elapsed = self.started.elapsed().as_secs_f64();
        // Very short bars (a local repository, a cached download) produce
        // meaningless rates; pacman shows "--" in that case.
        let rate = if elapsed > 0.1 {
            self.done as f64 / elapsed
        } else {
            0.0
        };
        if progress_json_enabled() {
            self.emit_json_progress(rate, true);
            return;
        }
        let line = render(
            &self.label,
            self.done,
            self.total,
            rate,
            width(),
            self.kind == BarKind::Bytes,
        );
        let _ = writeln!(std::io::stderr(), "\r{line}\x1b[K");
        let _ = std::io::stderr().flush();
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        if self.enabled && !self.finished && !progress_json_enabled() {
            let _ = writeln!(std::io::stderr(), "\x1b[K");
        }
    }
}

/// A spinner for phases that have no byte count, e.g. signature checks.
pub struct Spinner {
    enabled: bool,
    label: String,
    frame: usize,
    last_draw: Instant,
}

impl Spinner {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            enabled: enabled(),
            label: label.into(),
            frame: 0,
            last_draw: Instant::now() - REDRAW,
        }
    }

    pub fn tick(&mut self) {
        if !self.enabled || self.last_draw.elapsed() < REDRAW {
            return;
        }
        self.frame = (self.frame + 1) % SPINNER.len();
        self.last_draw = Instant::now();
        let _ = write!(
            std::io::stderr(),
            "\r{} {}\x1b[K",
            SPINNER[self.frame],
            self.label
        );
        let _ = std::io::stderr().flush();
    }

    pub fn finish(self, message: &str) {
        if !self.enabled {
            return;
        }
        let _ = writeln!(
            std::io::stderr(),
            "\r{} {}\x1b[K",
            SPINNER[self.frame],
            message
        );
        let _ = std::io::stderr().flush();
    }
}
