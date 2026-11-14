/*
 *
 *       user/desktop/src/apps/monitor.rs
 *       SPUI 4.0 System Monitor: real-time multi-core telemetry, sparklines and process manager
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mission Center / Windows 11 Task Manager styled System Monitor.
//! Features live 60-second CPU & memory performance sparklines, per-core
//! utilization, and a full process table with live CPU% telemetry and SIGTERM.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::Icon;
use crate::text::{Face, TextRenderer};
use crate::theme::Theme;
use crate::ui;
use crate::x11::{CursorKind, Result};
use std::collections::HashMap;
use std::fs;
use std::time::Instant;

const MAX_HISTORY: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Performance,
    Processes,
}

struct CpuSample {
    total: u64,
    idle: u64,
}

impl CpuSample {
    fn from_line(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            return None;
        }
        let user: u64 = parts[1].parse().ok()?;
        let nice: u64 = parts[2].parse().ok()?;
        let system: u64 = parts[3].parse().ok()?;
        let idle: u64 = parts[4].parse().ok()?;
        let iowait: u64 = parts.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
        let irq: u64 = parts.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);
        let softirq: u64 = parts.get(7).and_then(|s| s.parse().ok()).unwrap_or(0);
        let steal: u64 = parts.get(8).and_then(|s| s.parse().ok()).unwrap_or(0);

        let total = user + nice + system + idle + iowait + irq + softirq + steal;
        let idle_all = idle + iowait;
        Some(Self {
            total,
            idle: idle_all,
        })
    }
}

struct ProcessInfo {
    pid: i32,
    name: String,
    state: char,
    rss_kib: u64,
    cpu_percent: f32,
    prev_ticks: u64,
}

struct SystemMonitorData {
    prev_cpu: Option<CpuSample>,
    prev_cores: Vec<CpuSample>,
    cpu_history: Vec<f32>,
    mem_history: Vec<f32>,
    current_cpu_pct: f32,
    core_pcts: Vec<f32>,
    mem_total_kib: u64,
    mem_used_kib: u64,
    processes: Vec<ProcessInfo>,
    proc_ticks: HashMap<i32, u64>,
    last_sample_time: Instant,
    uptime_str: String,
    cpu_model: String,
}

impl SystemMonitorData {
    fn new() -> Self {
        let cpu_model = read_cpu_model();
        let mut data = Self {
            prev_cpu: None,
            prev_cores: Vec::new(),
            cpu_history: vec![0.0; MAX_HISTORY],
            mem_history: vec![0.0; MAX_HISTORY],
            current_cpu_pct: 0.0,
            core_pcts: Vec::new(),
            mem_total_kib: 1,
            mem_used_kib: 0,
            processes: Vec::new(),
            proc_ticks: HashMap::new(),
            last_sample_time: Instant::now(),
            uptime_str: "0:00".to_string(),
            cpu_model,
        };
        data.sample();
        data
    }

    fn sample(&mut self) {
        let now = Instant::now();
        let delta_sec = now
            .duration_since(self.last_sample_time)
            .as_secs_f32()
            .max(0.1);
        self.last_sample_time = now;

        // 1. Read /proc/stat
        if let Ok(raw) = fs::read_to_string("/proc/stat") {
            let mut lines = raw.lines();
            if let Some(first) = lines.next() {
                if let Some(cur) = CpuSample::from_line(first) {
                    if let Some(prev) = &self.prev_cpu {
                        let d_tot = cur.total.saturating_sub(prev.total);
                        let d_idle = cur.idle.saturating_sub(prev.idle);
                        if d_tot > 0 {
                            self.current_cpu_pct = ((d_tot.saturating_sub(d_idle)) as f32
                                / d_tot as f32)
                                .clamp(0.0, 1.0);
                        }
                    }
                    self.prev_cpu = Some(cur);
                }
            }

            let mut new_core_pcts = Vec::new();
            let mut new_core_samples = Vec::new();
            for (idx, line) in lines.enumerate() {
                if !line.starts_with("cpu") {
                    break;
                }
                if let Some(cur) = CpuSample::from_line(line) {
                    if let Some(prev) = self.prev_cores.get(idx) {
                        let d_tot = cur.total.saturating_sub(prev.total);
                        let d_idle = cur.idle.saturating_sub(prev.idle);
                        let pct = if d_tot > 0 {
                            ((d_tot.saturating_sub(d_idle)) as f32 / d_tot as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        new_core_pcts.push(pct);
                    } else {
                        new_core_pcts.push(0.0);
                    }
                    new_core_samples.push(cur);
                }
            }
            if !new_core_pcts.is_empty() {
                self.core_pcts = new_core_pcts;
                self.prev_cores = new_core_samples;
            }
        }

        self.cpu_history.remove(0);
        self.cpu_history.push(self.current_cpu_pct);

        // 2. Read /proc/meminfo
        if let Ok(raw) = fs::read_to_string("/proc/meminfo") {
            let parse_val = |key: &str| -> Option<u64> {
                raw.lines().find_map(|l| {
                    let rest = l.strip_prefix(key)?;
                    rest.split_whitespace().next()?.parse::<u64>().ok()
                })
            };
            let tot = parse_val("MemTotal:").unwrap_or(1);
            let free = parse_val("MemFree:").unwrap_or(0);
            let avail = parse_val("MemAvailable:").unwrap_or(free);
            self.mem_total_kib = tot;
            self.mem_used_kib = tot.saturating_sub(avail);
            let mem_pct = (self.mem_used_kib as f32 / tot as f32).clamp(0.0, 1.0);
            self.mem_history.remove(0);
            self.mem_history.push(mem_pct);
        }

        // 3. Read /proc/uptime
        if let Ok(raw) = fs::read_to_string("/proc/uptime") {
            if let Some(s) = raw.split_whitespace().next() {
                if let Ok(secs) = s.parse::<f64>() {
                    let total_sec = secs as u64;
                    let h = total_sec / 3600;
                    let m = (total_sec % 3600) / 60;
                    let s = total_sec % 60;
                    self.uptime_str = if h > 0 {
                        format!("{h}:{m:02}:{s:02}")
                    } else {
                        format!("{m:02}:{s:02}")
                    };
                }
            }
        }

        // 4. Read process list from /proc/[0-9]*
        let mut procs = Vec::new();
        let mut new_ticks = HashMap::new();
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                let Ok(pid) = name_str.parse::<i32>() else {
                    continue;
                };

                let stat_file = format!("/proc/{pid}/stat");
                if let Ok(raw) = fs::read_to_string(&stat_file) {
                    if let Some(p) = parse_proc_stat(pid, &raw) {
                        let total_ticks = p.prev_ticks;
                        new_ticks.insert(pid, total_ticks);
                        let prev = self.proc_ticks.get(&pid).copied().unwrap_or(total_ticks);
                        let d_ticks = total_ticks.saturating_sub(prev);
                        // Approximate ticks per second (100 HZ standard Linux USER_HZ)
                        let cpu_pct =
                            ((d_ticks as f32 / 100.0) / delta_sec * 100.0).clamp(0.0, 400.0);
                        procs.push(ProcessInfo {
                            pid,
                            name: p.name,
                            state: p.state,
                            rss_kib: p.rss_kib,
                            cpu_percent: cpu_pct,
                            prev_ticks: total_ticks,
                        });
                    }
                }
            }
        }

        procs.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.processes = procs;
        self.proc_ticks = new_ticks;
    }
}

fn parse_proc_stat(pid: i32, raw: &str) -> Option<ProcessInfo> {
    let open_paren = raw.find('(')?;
    let close_paren = raw.rfind(')')?;
    let name = raw[open_paren + 1..close_paren].to_string();
    let rest = raw[close_paren + 1..].trim();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 22 {
        return None;
    }
    let state = fields[0].chars().next().unwrap_or('?');
    let utime: u64 = fields.get(11).and_then(|s| s.parse().ok()).unwrap_or(0);
    let stime: u64 = fields.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);
    let rss_pages: u64 = fields.get(21).and_then(|s| s.parse().ok()).unwrap_or(0);

    Some(ProcessInfo {
        pid,
        name,
        state,
        rss_kib: rss_pages.saturating_mul(4),
        cpu_percent: 0.0,
        prev_ticks: utime + stime,
    })
}

fn read_cpu_model() -> String {
    let raw = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    for line in raw.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if matches!(k.trim(), "model name" | "Hardware" | "Processor") {
                return v.trim().to_string();
            }
        }
    }
    "x86_64 处理器".to_string()
}

fn format_kib(kib: u64) -> String {
    if kib >= 1024 * 1024 {
        format!("{:.1} GiB", kib as f64 / (1024.0 * 1024.0))
    } else if kib >= 1024 {
        format!("{:.1} MiB", kib as f64 / 1024.0)
    } else {
        format!("{kib} KiB")
    }
}

const PROC_TH_H: i32 = 28;
const PROC_ROW_H: i32 = 30;
const PROC_SCROLLBAR_W: i32 = ui::SCROLLBAR_HIT_W;

fn content_rect(theme: &Theme, w: i32, h: i32) -> Rect {
    let bw = theme.window_border_width();
    let th = theme.titlebar_height();
    Rect::new(bw, th, (w - bw * 2).max(1), (h - th - bw).max(1))
}

fn proc_table_rect(content: Rect) -> Rect {
    let top = content.y + 58;
    Rect::new(
        content.x + 14,
        top + 26,
        (content.w - 28).max(1),
        (content.bottom() - (top + 26) - 56).max(1),
    )
}

fn proc_rows_clip(table_r: Rect) -> Rect {
    Rect::new(
        table_r.x,
        table_r.y + PROC_TH_H,
        (table_r.w - PROC_SCROLLBAR_W - 4).max(1),
        (table_r.h - PROC_TH_H - 2).max(0),
    )
}

fn proc_scrollbar_track(table_r: Rect) -> Rect {
    Rect::new(
        table_r.right() - PROC_SCROLLBAR_W - 2,
        table_r.y + PROC_TH_H + 2,
        PROC_SCROLLBAR_W,
        (table_r.h - PROC_TH_H - 4).max(8),
    )
}

fn filtered_proc_count(data: &SystemMonitorData, query: &str) -> usize {
    data.processes
        .iter()
        .filter(|p| query.is_empty() || p.name.contains(query) || p.pid.to_string().contains(query))
        .count()
}

fn sync_proc_scroll(
    data: &SystemMonitorData,
    query: &str,
    scroll: &mut ui::SmoothScroll,
    content: Rect,
) {
    let table_r = proc_table_rect(content);
    let rows_r = proc_rows_clip(table_r);
    let track = proc_scrollbar_track(table_r);
    let total_h = (filtered_proc_count(data, query) as i32 * PROC_ROW_H) as f32;
    scroll.set_bounds(rows_r.h as f32, total_h, track);
}

pub fn run() -> Result<()> {
    let mut app = App::new("系统监视器", Icon::Monitor, 780, 520)?;
    app.min_width = 680;
    app.min_height = 440;

    let mut tab = Tab::Performance;
    let mut data = SystemMonitorData::new();
    let mut selected_pid: Option<i32> = None;
    let mut proc_scroll = ui::SmoothScroll::new("monitor_proc_scroll");
    let query = String::new();
    let mut last_sample = Instant::now();
    sync_proc_scroll(&data, &query, &mut proc_scroll, app.content());

    loop {
        if app.close {
            break;
        }

        if last_sample.elapsed().as_millis() >= 1000 {
            data.sample();
            sync_proc_scroll(&data, &query, &mut proc_scroll, app.content());
            last_sample = Instant::now();
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            let content = app.content();
            draw_monitor(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                tab,
                &data,
                selected_pid,
                &mut proc_scroll,
                &query,
                content,
            );
            app.draw_chrome();
            app.present()?;
        }

        let timeout = if ui::has_active_animations() {
            16
        } else {
            (1000 - last_sample.elapsed().as_millis() as i32).clamp(20, 100)
        };
        for event in app.pump_with_fd(None, timeout)?.0 {
            let content = app.content();
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose => app.needs_redraw = true,
                UiEvent::ConfigChanged => {
                    sync_proc_scroll(&data, &query, &mut proc_scroll, app.content());
                    app.needs_redraw = true;
                }
                UiEvent::Configure { width, height } => {
                    let resized = content_rect(&app.theme, width as i32, height as i32);
                    sync_proc_scroll(&data, &query, &mut proc_scroll, resized);
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    if tab == Tab::Processes {
                        if proc_scroll.handle_motion(x, y) {
                            app.needs_redraw = true;
                        }
                        app.set_content_cursor(
                            if proc_scroll.is_hovered() || proc_scroll.is_dragging() {
                                CursorKind::Pointer
                            } else {
                                CursorKind::Default
                            },
                        );
                    } else {
                        app.set_content_cursor(CursorKind::Default);
                    }
                }
                UiEvent::Wheel { delta, .. } => {
                    if tab == Tab::Processes {
                        sync_proc_scroll(&data, &query, &mut proc_scroll, content);
                        if proc_scroll.wheel(delta as f32 * 22.0) {
                            app.needs_redraw = true;
                        }
                    }
                }
                UiEvent::Button {
                    x, y, button, down, ..
                } => {
                    if tab == Tab::Processes && proc_scroll.handle_button(x, y, button, down) {
                        app.set_content_cursor(
                            if proc_scroll.is_hovered() || proc_scroll.is_dragging() {
                                CursorKind::Pointer
                            } else {
                                CursorKind::Default
                            },
                        );
                        app.needs_redraw = true;
                        continue;
                    }
                    if button == 1 && down && content.contains(x, y) {
                        // Check tab bar
                        let tab_w = 200;
                        let tab_r = Rect::new(content.right() - 16 - tab_w, content.y + 10, tab_w, 28);
                        if tab_r.contains(x, y) {
                            let seg0 = ui::segmented_item_rect(tab_r, 2, 0);
                            tab = if seg0.contains(x, y) {
                                Tab::Performance
                            } else {
                                sync_proc_scroll(&data, &query, &mut proc_scroll, content);
                                Tab::Processes
                            };
                            crate::sound::play(&app.config, crate::sound::SystemSound::Click);
                            app.needs_redraw = true;
                            continue;
                        }

                        if tab == Tab::Processes {
                            let kill_btn =
                                Rect::new(content.right() - 18 - 100, content.bottom() - 44, 100, 30);
                            if kill_btn.contains(x, y) {
                                if let Some(pid) = selected_pid {
                                    // SAFETY: Standard POSIX kill signal sent to designated PID.
                                    unsafe {
                                        libc::kill(pid, libc::SIGTERM);
                                    }
                                    data.sample();
                                    sync_proc_scroll(&data, &query, &mut proc_scroll, content);
                                    crate::sound::play(
                                        &app.config,
                                        crate::sound::SystemSound::Click,
                                    );
                                    app.needs_redraw = true;
                                }
                                continue;
                            }

                            let table_r = proc_table_rect(content);
                            let rows_clip = proc_rows_clip(table_r);
                            if rows_clip.contains(x, y) {
                                let scroll_px = proc_scroll.offset_i32().max(0);
                                let idx = ((y - rows_clip.y + scroll_px) / PROC_ROW_H) as usize;
                                let filtered: Vec<&ProcessInfo> = data
                                    .processes
                                    .iter()
                                    .filter(|p| {
                                        query.is_empty()
                                            || p.name.contains(&query)
                                            || p.pid.to_string().contains(&query)
                                    })
                                    .collect();
                                if let Some(target) = filtered.get(idx) {
                                    selected_pid = Some(target.pid);
                                    crate::sound::play(
                                        &app.config,
                                        crate::sound::SystemSound::Click,
                                    );
                                    app.needs_redraw = true;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_monitor(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    tab: Tab,
    data: &SystemMonitorData,
    selected_pid: Option<i32>,
    proc_scroll: &mut ui::SmoothScroll,
    query: &str,
    content: Rect,
) {
    canvas.clear(theme.window);

    // Header Toolbar
    text.draw(
        canvas,
        "系统监视器",
        (content.x + 18) as f32,
        (content.y + 31) as f32,
        16.0,
        theme.text,
        Face::Bold,
    );

    let tab_w = 200;
    let tab_r = Rect::new(content.right() - 18 - tab_w, content.y + 12, tab_w, 28);
    let selected_tab = match tab {
        Tab::Performance => 0,
        Tab::Processes => 1,
    };
    ui::segmented_control(
        canvas,
        text,
        theme,
        tab_r,
        &["性能", "进程"],
        selected_tab,
        None,
    );

    canvas.fill(
        Rect::new(content.x, content.y + 48, content.w, 1),
        theme.window_border,
    );

    match tab {
        Tab::Performance => {
            draw_performance_tab(canvas, text, theme, data, content);
        }
        Tab::Processes => {
            draw_processes_tab(
                canvas,
                text,
                theme,
                data,
                selected_pid,
                proc_scroll,
                query,
                content,
            );
        }
    }
}

fn draw_performance_tab(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    data: &SystemMonitorData,
    content: Rect,
) {
    let top = content.y + 62;
    let card_w = (content.w - 36) / 2;
    let card_h = (content.bottom() - top - 18).max(300);

    // 1. Left Card: CPU Dashboard
    let cpu_card = Rect::new(content.x + 14, top, card_w, card_h);
    ui::card(canvas, theme, cpu_card);

    let cpu_pct_text = format!("{:.0}%", data.current_cpu_pct * 100.0);
    text.draw(
        canvas,
        "处理器 (CPU)",
        (cpu_card.x + 16) as f32,
        (cpu_card.y + 26) as f32,
        14.0,
        theme.text,
        Face::Bold,
    );
    let cpu_pct_w = text.measure(&cpu_pct_text, 18.0, Face::Bold);
    text.draw(
        canvas,
        &cpu_pct_text,
        (cpu_card.right() - 16) as f32 - cpu_pct_w,
        (cpu_card.y + 28) as f32,
        18.0,
        theme.accent,
        Face::Bold,
    );

    // CPU Sparkline
    let spark_h = (cpu_card.h - 160).max(80);
    let spark_r = Rect::new(cpu_card.x + 16, cpu_card.y + 44, cpu_card.w - 32, spark_h);
    if theme.is_win98() {
        canvas.fill(spark_r, theme.input_bg);
        ui::sunken_bevel(canvas, theme, spark_r);
    } else {
        canvas.rounded_rect(spark_r, 4, theme.window_alt, 1.0);
        canvas.rounded_border(spark_r, 4, theme.window_border, 1.0, 1);
    }

    // Grid lines (25%, 50%, 75%)
    for pct in [0.25, 0.50, 0.75] {
        let gy = spark_r.bottom() - (spark_r.h as f32 * pct).round() as i32;
        canvas.fill(
            Rect::new(spark_r.x + 1, gy, spark_r.w - 2, 1),
            theme.window_border,
        );
    }
    canvas.draw_sparkline(spark_r, &data.cpu_history, theme.accent, Some(theme.accent));

    // Multi-core bar meters
    let cores_y = spark_r.bottom() + 14;
    text.draw(
        canvas,
        "各核心利用率",
        (cpu_card.x + 16) as f32,
        (cores_y + 12) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    let num_cores = data.core_pcts.len().max(1);
    let bar_area_w = cpu_card.w - 32;
    let bar_gap = 8;
    let single_bar_w = (bar_area_w - (num_cores as i32 - 1) * bar_gap) / num_cores as i32;
    for (i, &pct) in data.core_pcts.iter().enumerate() {
        let bx = cpu_card.x + 16 + i as i32 * (single_bar_w + bar_gap);
        let by = cores_y + 20;
        let bar_r = Rect::new(bx, by, single_bar_w, 12);
        ui::progress_bar(canvas, theme, bar_r, pct, Some(theme.accent));
        let core_lbl = format!("C{i}: {:.0}%", pct * 100.0);
        text.draw(
            canvas,
            &core_lbl,
            bx as f32,
            (by + 24) as f32,
            10.5,
            theme.text_dim,
            Face::Ui,
        );
    }

    // Spec rows
    let spec_y = cpu_card.bottom() - 28;
    let uptime_info = format!("运行时间: {}  |  {}", data.uptime_str, data.cpu_model);
    let trunc_spec = text.truncate(&uptime_info, (cpu_card.w - 32) as f32, 11.0, Face::Ui);
    text.draw(
        canvas,
        &trunc_spec,
        (cpu_card.x + 16) as f32,
        spec_y as f32,
        11.0,
        theme.text_muted,
        Face::Ui,
    );

    // 2. Right Card: Memory Dashboard
    let mem_card = Rect::new(cpu_card.right() + 8, top, card_w, card_h);
    ui::card(canvas, theme, mem_card);

    let used_fmt = format_kib(data.mem_used_kib);
    let total_fmt = format_kib(data.mem_total_kib);
    let mem_pct_num =
        (data.mem_used_kib as f32 / data.mem_total_kib as f32 * 100.0).clamp(0.0, 100.0);
    let mem_pct_text = format!("{:.0}%", mem_pct_num);

    text.draw(
        canvas,
        "物理内存 (RAM)",
        (mem_card.x + 16) as f32,
        (mem_card.y + 26) as f32,
        14.0,
        theme.text,
        Face::Bold,
    );
    let mem_pct_w = text.measure(&mem_pct_text, 18.0, Face::Bold);
    text.draw(
        canvas,
        &mem_pct_text,
        (mem_card.right() - 16) as f32 - mem_pct_w,
        (mem_card.y + 28) as f32,
        18.0,
        0x10b981, // Emerald green for memory
        Face::Bold,
    );

    // Memory Sparkline
    let m_spark_r = Rect::new(mem_card.x + 16, mem_card.y + 44, mem_card.w - 32, spark_h);
    if theme.is_win98() {
        canvas.fill(m_spark_r, theme.input_bg);
        ui::sunken_bevel(canvas, theme, m_spark_r);
    } else {
        canvas.rounded_rect(m_spark_r, 4, theme.window_alt, 1.0);
        canvas.rounded_border(m_spark_r, 4, theme.window_border, 1.0, 1);
    }
    for pct in [0.25, 0.50, 0.75] {
        let gy = m_spark_r.bottom() - (m_spark_r.h as f32 * pct).round() as i32;
        canvas.fill(
            Rect::new(m_spark_r.x + 1, gy, m_spark_r.w - 2, 1),
            theme.window_border,
        );
    }
    canvas.draw_sparkline(m_spark_r, &data.mem_history, 0x10b981, Some(0x10b981));

    // Big memory usage bar
    let m_bar_y = m_spark_r.bottom() + 18;
    let m_bar_r = Rect::new(mem_card.x + 16, m_bar_y, mem_card.w - 32, 18);
    ui::progress_bar(canvas, theme, m_bar_r, mem_pct_num / 100.0, Some(0x10b981));

    let m_stat_y = m_bar_y + 36;
    let m_detail = format!("已用: {used_fmt}  /  总计: {total_fmt}");
    text.draw(
        canvas,
        &m_detail,
        (mem_card.x + 16) as f32,
        m_stat_y as f32,
        12.5,
        theme.text,
        Face::Bold,
    );

    let m_avail_fmt = format_kib(data.mem_total_kib.saturating_sub(data.mem_used_kib));
    let m_sub = format!("当前可用空闲内存: {m_avail_fmt}");
    text.draw(
        canvas,
        &m_sub,
        (mem_card.x + 16) as f32,
        (m_stat_y + 22) as f32,
        11.5,
        theme.text_dim,
        Face::Ui,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_processes_tab(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    data: &SystemMonitorData,
    selected_pid: Option<i32>,
    proc_scroll: &mut ui::SmoothScroll,
    query: &str,
    content: Rect,
) {
    let top = content.y + 58;
    let is_win98 = theme.is_win98();

    // Search bar & count
    let filtered: Vec<&ProcessInfo> = data
        .processes
        .iter()
        .filter(|p| query.is_empty() || p.name.contains(query) || p.pid.to_string().contains(query))
        .collect();

    let count_lbl = format!("活动进程 ({} 个)", filtered.len());
    text.draw(
        canvas,
        &count_lbl,
        (content.x + 18) as f32,
        (top + 18) as f32,
        12.5,
        theme.text_muted,
        Face::Bold,
    );

    // Kill button
    let kill_rect = Rect::new(content.right() - 18 - 100, content.bottom() - 14 - 30, 100, 30);
    let has_selection = selected_pid.is_some();
    ui::button(
        canvas,
        text,
        theme,
        kill_rect,
        "结束进程",
        None,
        has_selection,
        false,
    );

    // Table Header
    let table_r = proc_table_rect(content);
    ui::card(canvas, theme, table_r);

    canvas.fill(
        Rect::new(table_r.x, table_r.y, table_r.w, PROC_TH_H),
        theme.window_alt,
    );
    canvas.fill(
        Rect::new(table_r.x, table_r.y + PROC_TH_H - 1, table_r.w, 1),
        theme.window_border,
    );

    let col_name = table_r.x + 16;
    let col_pid = table_r.x + (table_r.w * 45) / 100;
    let col_cpu = table_r.x + (table_r.w * 62) / 100;
    let col_rss = table_r.x + (table_r.w * 78) / 100;
    let rows_clip = proc_rows_clip(table_r);
    let col_st = rows_clip.right() - 28;

    let th_baseline = table_r.y as f32 + 18.0;
    text.draw(
        canvas,
        "进程名称",
        col_name as f32,
        th_baseline,
        11.5,
        theme.text_muted,
        Face::Bold,
    );
    text.draw(
        canvas,
        "PID",
        col_pid as f32,
        th_baseline,
        11.5,
        theme.text_muted,
        Face::Bold,
    );
    text.draw(
        canvas,
        "CPU %",
        col_cpu as f32,
        th_baseline,
        11.5,
        theme.text_muted,
        Face::Bold,
    );
    text.draw(
        canvas,
        "物理内存",
        col_rss as f32,
        th_baseline,
        11.5,
        theme.text_muted,
        Face::Bold,
    );
    text.draw(
        canvas,
        "状态",
        col_st as f32,
        th_baseline,
        11.5,
        theme.text_muted,
        Face::Bold,
    );

    // Rows with smooth pixel clipping and animated scrollbar
    let sb_track = proc_scrollbar_track(table_r);
    let total_h = (filtered.len() as i32 * PROC_ROW_H) as f32;
    proc_scroll.set_bounds(rows_clip.h as f32, total_h, sb_track);
    let scroll_px = proc_scroll.offset_i32().max(0);
    let first_row = (scroll_px / PROC_ROW_H).max(0) as usize;
    let vis_span = ((rows_clip.h + PROC_ROW_H - 1) / PROC_ROW_H + 1).max(1) as usize;

    canvas.with_clip(rows_clip, |c| {
        for (idx, proc) in filtered.iter().enumerate().skip(first_row).take(vis_span) {
            let ry = rows_clip.y + idx as i32 * PROC_ROW_H - scroll_px;
            if ry >= rows_clip.bottom() {
                break;
            }
            let is_sel = selected_pid == Some(proc.pid);
            let row_r = Rect::new(rows_clip.x + 2, ry, rows_clip.w - 4, PROC_ROW_H);
            let win98_sel = is_win98 && is_sel;

            if is_win98 {
                if is_sel {
                    let _ = ui::list_item_bg(c, theme, row_r, true, false);
                } else if idx % 2 == 1 {
                    c.fill(row_r, theme.window_alt);
                }
            } else if is_sel {
                c.rounded_rect(row_r, 4, theme.accent_soft, 1.0);
                c.fill(
                    Rect::new(row_r.x, row_r.y + 4, 3, row_r.h - 8),
                    theme.accent,
                );
            } else if idx % 2 == 1 {
                c.fill(row_r, theme.window_alt);
            }

            let baseline = ry as f32 + 20.0;
            let name_shown =
                text.truncate(&proc.name, (col_pid - col_name - 16) as f32, 12.0, Face::Ui);
            text.draw(
                c,
                &name_shown,
                col_name as f32,
                baseline,
                12.0,
                if win98_sel {
                    theme.selection_text
                } else if is_sel {
                    theme.accent
                } else {
                    theme.text
                },
                if is_sel { Face::Bold } else { Face::Ui },
            );

            text.draw(
                c,
                &proc.pid.to_string(),
                col_pid as f32,
                baseline,
                12.0,
                if win98_sel {
                    theme.selection_text
                } else {
                    theme.text_dim
                },
                Face::Ui,
            );

            let cpu_txt = format!("{:.1}%", proc.cpu_percent);
            text.draw(
                c,
                &cpu_txt,
                col_cpu as f32,
                baseline,
                12.0,
                if win98_sel {
                    theme.selection_text
                } else if proc.cpu_percent > 10.0 {
                    theme.accent
                } else {
                    theme.text_dim
                },
                if proc.cpu_percent > 10.0 {
                    Face::Bold
                } else {
                    Face::Ui
                },
            );

            let rss_txt = format_kib(proc.rss_kib);
            text.draw(
                c,
                &rss_txt,
                col_rss as f32,
                baseline,
                12.0,
                if win98_sel {
                    theme.selection_text
                } else {
                    theme.text_dim
                },
                Face::Ui,
            );

            let st_str = proc.state.to_string();
            text.draw(
                c,
                &st_str,
                col_st as f32,
                baseline,
                12.0,
                if win98_sel {
                    theme.selection_text
                } else {
                    theme.text_muted
                },
                Face::Ui,
            );
        }
    });

    proc_scroll.draw_v(canvas, theme, sb_track, rows_clip.h as f32, total_h);
}
