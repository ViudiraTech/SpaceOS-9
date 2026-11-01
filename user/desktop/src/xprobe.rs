/*
 *
 *       user/desktop/src/xprobe.rs
 *       Standalone X11 connection, mapping and image upload diagnostic
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Minimal smoke probe that isolates X client setup and PutImage from the WM.
//! `xprobe list` prints managed window geometry, `xprobe rtt [seconds]` measures
//! server round-trip latency; both exist for tools/desktop_vm.py driven tests.

use crate::draw::{Canvas, Rect};
use crate::x11::{Result, X};
use std::thread::sleep;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, EventMask};

/// Print `id x y width height` for every window the shell manages.
fn list() -> Result<()> {
    let x = X::connect()?;
    let reply = x
        .conn
        .get_property(
            false,
            x.root,
            x.atoms.net_client_list,
            AtomEnum::WINDOW,
            0,
            1024,
        )?
        .reply()?;
    for raw in reply.value.chunks_exact(4) {
        let window = u32::from_ne_bytes(raw.try_into()?);
        let geometry = x.conn.get_geometry(window)?.reply()?;
        println!(
            "{window:#x} {} {} {} {}",
            geometry.x, geometry.y, geometry.width, geometry.height
        );
    }
    Ok(())
}

/// Measure request round trips, which is what a client feels as server lag.
fn round_trips(seconds: u64) -> Result<()> {
    let x = X::connect()?;
    let mut samples = Vec::new();
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        let started = Instant::now();
        x.conn.get_input_focus()?.reply()?;
        samples.push(started.elapsed().as_micros() as u64);
        x.conn.flush()?;
        sleep(Duration::from_millis(5));
    }
    samples.sort_unstable();
    let at = |percent: usize| samples[(samples.len() - 1) * percent / 100];
    println!(
        "rtt_us n={} p50={} p95={} p99={} max={}",
        samples.len(),
        at(50),
        at(95),
        at(99),
        at(100)
    );
    Ok(())
}

pub fn run(arguments: &[String]) -> Result<()> {
    match arguments.first().map(String::as_str) {
        Some("list") => return list(),
        Some("rtt") => {
            let seconds = arguments.get(1).and_then(|text| text.parse().ok());
            return round_trips(seconds.unwrap_or(3));
        }
        _ => {}
    }
    let x = X::connect()?;
    eprintln!(
        "xprobe: connected screen={} size={}x{} depth={}",
        x.screen, x.width, x.height, x.depth
    );
    let window = x.generate_id()?;
    x.create_window(
        window,
        x.root,
        0,
        0,
        x.width,
        x.height,
        true,
        EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY,
        0x336699,
    )?;
    x.set_title(window, "SPUI X11 probe")?;
    x.map(window)?;
    x.flush()?;
    eprintln!("xprobe: mapped window={window:#x}");
    let canvas = Canvas::new(x.width as usize, x.height as usize, 0x336699);
    x.put_canvas(
        window,
        &canvas,
        Rect::new(0, 0, x.width as i32, x.height as i32),
    )?;
    x.flush()?;
    eprintln!("xprobe: uploaded {} pixels", canvas.width * canvas.height);

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        while let Some(event) = x.poll()? {
            if let x11rb::protocol::Event::Expose(expose) = event {
                if expose.window == window && expose.count == 0 {
                    x.put_canvas(
                        window,
                        &canvas,
                        Rect::new(0, 0, x.width as i32, x.height as i32),
                    )?;
                    x.flush()?;
                }
            }
        }
        sleep(Duration::from_millis(20));
    }
    x.destroy(window)?;
    x.flush()?;
    Ok(())
}
