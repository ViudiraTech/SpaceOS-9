/*
 *
 *       src/sound/dummy.rs
 *       Built-in dummy sound card for the boot self-test
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A full-duplex software card.  Playback consumes the ring at the configured
//! rate and capture produces silence; both advance through the same period
//! timer, so the core's mmap, xrun, drain and poll paths are exercised without
//! any hardware.  This is the reference implementation for loadable drivers.

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{
    sound::{
        card::Card,
        control,
        pcm::{self, Direction, Pcm, PcmCaps, PcmOps, Substream},
        uapi,
    },
    sync::Mutex,
    time,
};

static INITIALIZED: AtomicBool = AtomicBool::new(false);

struct DummyState {
    me: Weak<DummyPcm>,
    running: bool,
    position: u64,
    started_micros: u64,
    started_position: u64,
    rate: u32,
    period: u64,
    generation: u64,
}

struct DummyPcm {
    state: Mutex<DummyState>,
}

impl DummyPcm {
    fn new() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            state: Mutex::new(DummyState {
                me: weak.clone(),
                running: false,
                position: 0,
                started_micros: 0,
                started_position: 0,
                rate: 48_000,
                period: 1024,
                generation: 0,
            }),
        })
    }

    fn arm(engine: Arc<DummyPcm>, substream: Arc<Substream>, generation: u64) {
        let period_micros = {
            let state = engine.state.lock();
            if !state.running || state.generation != generation {
                return;
            }
            (state.period.saturating_mul(1_000_000) / u64::from(state.rate.max(1))).max(1_000)
        };
        crate::net::timer::schedule_in(
            period_micros,
            alloc::boxed::Box::new(move || {
                {
                    let state = engine.state.lock();
                    if !state.running || state.generation != generation {
                        return;
                    }
                }
                pcm::period_elapsed(&substream);
                Self::arm(engine.clone(), substream.clone(), generation);
            }),
        );
    }

    fn elapsed(&self, state: &DummyState) -> u64 {
        if !state.running {
            return 0;
        }
        let micros = time::uptime_micros().saturating_sub(state.started_micros);
        ((micros as u128 * u128::from(state.rate)) / 1_000_000) as u64
    }

    fn freeze(state: &mut DummyState) {
        if state.running {
            let micros = time::uptime_micros().saturating_sub(state.started_micros);
            let frames = ((micros as u128 * u128::from(state.rate)) / 1_000_000) as u64;
            state.position = state.started_position.saturating_add(frames);
            state.running = false;
        }
        state.generation = state.generation.wrapping_add(1);
    }

    fn start(&self, substream: &Substream) {
        let engine = self.state.lock().me.upgrade();
        let generation = {
            let mut state = self.state.lock();
            if state.running {
                return;
            }
            state.running = true;
            state.started_micros = time::uptime_micros();
            state.started_position = state.position;
            state.generation = state.generation.wrapping_add(1);
            state.generation
        };
        if let Some(engine) = engine
            && let Some(substream) = substream.arc()
        {
            Self::arm(engine, substream, generation);
        }
    }
}

impl PcmOps for DummyPcm {
    fn hw_params(
        &self,
        _substream: &Substream,
        params: &uapi::snd_pcm_hw_params,
    ) -> Result<(), i32> {
        let rate = params
            .interval(uapi::SNDRV_PCM_HW_PARAM_RATE)
            .exact_value()
            .unwrap_or(48_000);
        let period = params
            .interval(uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE)
            .exact_value()
            .unwrap_or(1024) as u64;
        let mut state = self.state.lock();
        state.rate = rate.max(1);
        state.period = period.max(1);
        state.position = 0;
        state.started_position = 0;
        state.running = false;
        state.generation = state.generation.wrapping_add(1);
        Ok(())
    }

    fn prepare(&self, _substream: &Substream) {
        let mut state = self.state.lock();
        state.position = 0;
        state.started_position = 0;
        state.running = false;
        state.generation = state.generation.wrapping_add(1);
    }

    fn trigger(&self, substream: &Substream, cmd: i32) -> Result<(), i32> {
        match cmd {
            uapi::SNDRV_PCM_TRIGGER_START | uapi::SNDRV_PCM_TRIGGER_PAUSE_RELEASE => {
                self.start(substream);
            }
            uapi::SNDRV_PCM_TRIGGER_STOP
            | uapi::SNDRV_PCM_TRIGGER_PAUSE_PUSH
            | uapi::SNDRV_PCM_TRIGGER_SUSPEND => {
                let mut state = self.state.lock();
                Self::freeze(&mut state);
            }
            uapi::SNDRV_PCM_TRIGGER_RESUME => self.start(substream),
            uapi::SNDRV_PCM_TRIGGER_DRAIN => {}
            _ => return Err(-22),
        }
        Ok(())
    }

    fn pointer(&self, _substream: &Substream) -> u64 {
        let state = self.state.lock();
        if state.running {
            state.started_position.saturating_add(self.elapsed(&state))
        } else {
            state.position
        }
    }
}

fn full_format_mask() -> u64 {
    let mut formats = 0u64;
    let mut format = 0;
    while format <= uapi::SNDRV_PCM_FORMAT_LAST {
        if uapi::format_bytes(format).is_some() {
            formats |= uapi::format_bit(format);
        }
        format += 1;
    }
    formats
}

pub fn init() {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return;
    }
    let card = match Card::new(
        -1,
        "Dummy",
        "Dummy",
        "Dummy",
        "SpaceKernel Dummy Sound Card",
    ) {
        Ok(card) => card,
        Err(_) => return,
    };
    card.add_component("spk-dummy");
    card.add_component("alsa-pcm");

    let flags = uapi::SNDRV_PCM_INFO_MMAP
        | uapi::SNDRV_PCM_INFO_MMAP_VALID
        | uapi::SNDRV_PCM_INFO_INTERLEAVED
        | uapi::SNDRV_PCM_INFO_BLOCK_TRANSFER
        | uapi::SNDRV_PCM_INFO_PAUSE
        | uapi::SNDRV_PCM_INFO_RESUME;
    let device = Pcm::new(card.number, 0, "Dummy PCM", "Dummy PCM", true, true, flags);

    let caps = PcmCaps {
        formats: full_format_mask(),
        rate_min: 8_000,
        rate_max: 192_000,
        channels_min: 1,
        channels_max: 2,
        periods_min: 2,
        periods_max: 32,
        buffer_bytes_max: 1024 * 1024,
    };
    for direction in [Direction::Playback, Direction::Capture] {
        if let Some(substream) = device.substream(direction) {
            *substream.caps.lock() = caps;
            substream.set_ops(DummyPcm::new());
        }
    }
    let _ = card.add_pcm(device);

    for element in [
        control::volume_element("Master Playback Volume", 2, 100),
        control::switch_element("Master Playback Switch", 2),
        control::volume_element("PCM Playback Volume", 2, 100),
        control::volume_element("Capture Volume", 2, 100),
        control::enumerated_element("Master Playback Route", Vec::from(["Stereo", "Mono"])),
    ] {
        let _ = card.add_control(element);
    }

    let _ = card.register();
}
