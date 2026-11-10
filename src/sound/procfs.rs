/*
 *
 *       src/sound/procfs.rs
 *       /proc/asound compatibility files
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! alsa-lib and the ALSA utilities read card, PCM and timer enumeration from
//! `/proc/asound`.  The tree is built from the live card registry, and every
//! dynamic attribute re-reads the registry so it never goes stale.

use alloc::{
    format,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::Ordering;

use crate::{
    sync::Mutex,
    vfs::{
        Inode,
        tree::{Attribute, Directory},
    },
};

use super::{card::Card, pcm::Direction, uapi};

static ROOT: Mutex<Option<Arc<Directory>>> = Mutex::new(None);
static CARD_DIRS: Mutex<Vec<(u32, Arc<Directory>)>> = Mutex::new(Vec::new());

fn ensure_root() -> Arc<Directory> {
    let mut root = ROOT.lock();
    if let Some(existing) = root.as_ref() {
        return existing.clone();
    }
    let directory = Directory::new(0o555, false);
    directory
        .insert(
            "version",
            Attribute::text("Advanced Linux Sound Architecture Driver Version k7.0.0-spk.\n"),
        )
        .ok();
    directory
        .insert(
            "cards",
            Attribute::new(0o444, || {
                let mut text = String::new();
                for card in crate::sound::card::cards() {
                    text.push_str(&format!(
                        "{:2} [{}]: {} - {}\n                      {}\n",
                        card.number,
                        pad(&card.id, 15),
                        card.driver,
                        card.shortname,
                        card.longname
                    ));
                }
                Ok(text.into_bytes())
            }),
        )
        .ok();
    directory
        .insert(
            "pcm",
            Attribute::new(0o444, || {
                let mut text = String::new();
                for card in crate::sound::card::cards() {
                    for pcm in card.pcms() {
                        text.push_str(&format!(
                            "{:02}-{:02}: {} : {} : playback {} : capture {}\n",
                            card.number,
                            pcm.device,
                            pcm.id,
                            pcm.name,
                            u8::from(!pcm.playback.is_empty()),
                            u8::from(!pcm.capture.is_empty())
                        ));
                    }
                }
                Ok(text.into_bytes())
            }),
        )
        .ok();
    directory
        .insert(
            "devices",
            Attribute::new(0o444, || {
                let mut text = String::new();
                for card in crate::sound::card::cards() {
                    let minors = card.minors();
                    let mut cursor = 0;
                    let mut next = || {
                        let minor = minors.get(cursor).copied().unwrap_or(0);
                        cursor += 1;
                        minor
                    };
                    text.push_str(&format!(
                        "  {:3}: [ {:2}-{:2}]: control\n",
                        next(),
                        card.number,
                        0
                    ));
                    for pcm in card.pcms() {
                        if !pcm.playback.is_empty() {
                            text.push_str(&format!(
                                "  {:3}: [ {:2}-{:2}]: digital audio playback\n",
                                next(),
                                card.number,
                                pcm.device
                            ));
                        }
                        if !pcm.capture.is_empty() {
                            text.push_str(&format!(
                                "  {:3}: [ {:2}-{:2}]: digital audio capture\n",
                                next(),
                                card.number,
                                pcm.device
                            ));
                        }
                    }
                    for hwdep in card.hwdeps() {
                        text.push_str(&format!(
                            "  {:3}: [ {:2}-{:2}]: hardware dependent\n",
                            next(),
                            card.number,
                            hwdep.device
                        ));
                    }
                    for midi in card.rawmidis() {
                        text.push_str(&format!(
                            "  {:3}: [ {:2}-{:2}]: raw midi\n",
                            next(),
                            card.number,
                            midi.device
                        ));
                    }
                }
                text.push_str(&format!(
                    "  {:3}: [  0- 0]: system timer\n",
                    uapi::SNDRV_MINOR_TIMER
                ));
                Ok(text.into_bytes())
            }),
        )
        .ok();
    directory
        .insert(
            "timers",
            Attribute::new(0o444, || {
                Ok("# name: system\n# id: System\n# : timer_id: 0\n"
                    .as_bytes()
                    .to_vec())
            }),
        )
        .ok();
    *root = Some(directory.clone());
    directory
}

fn pad(value: &str, width: usize) -> String {
    let mut result = value.to_string();
    while result.len() < width {
        result.push(' ');
    }
    result.truncate(width);
    result
}

fn state_name(state: i32) -> &'static str {
    match state {
        uapi::SNDRV_PCM_STATE_OPEN => "OPEN",
        uapi::SNDRV_PCM_STATE_SETUP => "SETUP",
        uapi::SNDRV_PCM_STATE_PREPARED => "PREPARED",
        uapi::SNDRV_PCM_STATE_RUNNING => "RUNNING",
        uapi::SNDRV_PCM_STATE_XRUN => "XRUN",
        uapi::SNDRV_PCM_STATE_DRAINING => "DRAINING",
        uapi::SNDRV_PCM_STATE_PAUSED => "PAUSED",
        uapi::SNDRV_PCM_STATE_SUSPENDED => "SUSPENDED",
        uapi::SNDRV_PCM_STATE_DISCONNECTED => "DISCONNECTED",
        _ => "UNKNOWN",
    }
}

fn pcm_directory(card: Weak<Card>, device: u32, direction: Direction) -> Arc<Directory> {
    let directory = Directory::new(0o555, false);
    let info_card = card.clone();
    directory
        .insert(
            "info",
            Attribute::new(0o444, move || {
                let Some(card) = info_card.upgrade() else {
                    return Ok(Vec::new());
                };
                let Some(pcm) = card.find_pcm(device) else {
                    return Ok(Vec::new());
                };
                let stream = if direction == Direction::Playback {
                    "PLAYBACK"
                } else {
                    "CAPTURE"
                };
                Ok(format!(
                    "card: {}\ndevice: {}\nsubdevice: 0\nstream: {}\nid: {}\nname: {}\nsubname: subdevice #0\nclass: 0\nsubclass: 0\nsubdevices_count: {}\nsubdevices_avail: {}\n",
                    card.number,
                    pcm.device,
                    stream,
                    pcm.id,
                    pcm.name,
                    pcm.substream_count(direction),
                    pcm.substreams(direction)
                        .iter()
                        .filter(|substream| !substream.opened.load(Ordering::Acquire))
                        .count()
                )
                .into_bytes())
            }),
        )
        .ok();
    let count = card
        .upgrade()
        .and_then(|card| card.find_pcm(device))
        .map_or(0, |pcm| pcm.substream_count(direction));
    for subdevice in 0..count {
        let sub = Directory::new(0o555, false);
        let status_card = card.clone();
        sub.insert(
            "status",
            Attribute::new(0o444, move || {
                let Some(card) = status_card.upgrade() else {
                    return Ok(Vec::new());
                };
                let Some(pcm) = card.find_pcm(device) else {
                    return Ok(Vec::new());
                };
                let Some(substream) = pcm.substream(direction, subdevice) else {
                    return Ok(Vec::new());
                };
                let runtime = substream.runtime.lock();
                let avail = substream.avail.load(Ordering::Acquire);
                Ok(format!(
                    "state: {}\nowner_pid : 0\ntstamp: {} ns\ndelay: 0\navail: {}\navail_max: {}\n",
                    state_name(runtime.state),
                    runtime.tstamp.tv_sec * 1_000_000_000 + runtime.tstamp.tv_nsec,
                    avail,
                    runtime.avail_max.max(avail)
                )
                .into_bytes())
            }),
        )
        .ok();
        let hw_card = card.clone();
        sub.insert(
            "hw_params",
            Attribute::new(0o444, move || {
                let Some(card) = hw_card.upgrade() else {
                    return Ok(Vec::new());
                };
                let Some(pcm) = card.find_pcm(device) else {
                    return Ok(Vec::new());
                };
                let Some(substream) = pcm.substream(direction, subdevice) else {
                    return Ok(Vec::new());
                };
                let runtime = substream.runtime.lock();
                let Some(hw) = runtime.hw else {
                    return Ok(b"closed\n".to_vec());
                };
                Ok(format!(
                    "access: {}\nformat: {}\nsubformat: {}\nchannels: {}\nrate: {}\nperiod_size: {}\nperiod_time: {}\nbuffer_size: {}\nbuffer_time: {}\n",
                    hw.access,
                    hw.format,
                    uapi::SNDRV_PCM_SUBFORMAT_STD,
                    hw.channels,
                    hw.rate,
                    hw.period_size,
                    hw.period_size * 1_000_000 / hw.rate as u64,
                    hw.buffer_size,
                    hw.buffer_size * 1_000_000 / hw.rate as u64
                )
                .into_bytes())
            }),
        )
        .ok();
        let sw_card = card.clone();
        sub.insert(
            "sw_params",
            Attribute::new(0o444, move || {
                let Some(card) = sw_card.upgrade() else {
                    return Ok(Vec::new());
                };
                let Some(pcm) = card.find_pcm(device) else {
                    return Ok(Vec::new());
                };
                let Some(substream) = pcm.substream(direction, subdevice) else {
                    return Ok(Vec::new());
                };
                let runtime = substream.runtime.lock();
                Ok(format!(
                    "tstamp_mode: {}\nperiod_step: {}\navail_min: {}\nstart_threshold: {}\nstop_threshold: {}\nboundary: {}\n",
                    runtime.sw.tstamp_mode,
                    runtime.sw.period_step,
                    runtime.sw.avail_min,
                    runtime.sw.start_threshold,
                    runtime.sw.stop_threshold,
                    runtime.sw.boundary
                )
                .into_bytes())
            }),
        )
        .ok();
        let _ = directory.insert(&format!("sub{subdevice}"), sub);
    }
    directory
}

pub fn root() -> Arc<dyn Inode> {
    ensure_root()
}

pub fn card_added(card: &Arc<Card>) {
    ensure_root();
    let directory = Directory::new(0o555, false);
    directory
        .insert("id", Attribute::text(format!("{}\n", card.id)))
        .ok();
    let weak = Arc::downgrade(card);
    for pcm in card.pcms() {
        for direction in [Direction::Playback, Direction::Capture] {
            if pcm.substream_count(direction) == 0 {
                continue;
            }
            let name = format!("pcm{}D{}{}", card.number, pcm.device, direction.suffix());
            let _ = directory.insert(&name, pcm_directory(weak.clone(), pcm.device, direction));
        }
    }
    for midi in card.rawmidis() {
        let midi_dir = Directory::new(0o555, false);
        let info_card = weak.clone();
        let device = midi.device;
        let id = midi.id.clone();
        let name = midi.name.clone();
        let flags = midi.info_flags;
        midi_dir
            .insert(
                "info",
                Attribute::new(0o444, move || {
                    let Some(_card) = info_card.upgrade() else {
                        return Ok(Vec::new());
                    };
                    Ok(format!(
                        "device: {device}\nid: {id}\nname: {name}\nflags: {flags:#x}\nsubdevices_count: 1\n"
                    )
                    .into_bytes())
                }),
            )
            .ok();
        let _ = directory.insert(&format!("midi{device}"), midi_dir);
    }
    let root = ensure_root();
    let name = format!("card{}", card.number);
    let _ = root.insert(&name, directory.clone());
    CARD_DIRS.lock().push((card.number, directory));
}

pub fn card_removed(number: u32) {
    let mut directories = CARD_DIRS.lock();
    if let Some((_, directory)) = directories
        .iter()
        .find(|(card, _)| *card == number)
        .cloned()
    {
        directories.retain(|(card, _)| *card != number);
        drop(directories);
        if let Some(root) = ROOT.lock().as_ref() {
            let name = format!("card{}", number);
            if let Ok(node) = root.lookup(&name)
                && node.metadata().map(|stat| stat.ino).ok()
                    == directory.metadata().ok().map(|stat| stat.ino)
            {
                let _ = root.remove(&name);
            }
        }
    }
}
