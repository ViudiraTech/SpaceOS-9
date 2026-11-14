/*
 *
 *       src/sound/mixer.rs
 *       Multi-stream PCM software mixer and resampler for single-substream DACs
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! ALSA device nodes on hardware codecs such as HDA and SB16 expose a single
//! hardware DMA playback substream.  When multiple userspace applications open
//! `/dev/snd/pcmC*D0p` concurrently (for example background MP3 playback plus
//! desktop UI SFX), opening directly against [`super::pcm::PcmDeviceNode`]
//! fails with `EAGAIN` for every caller after the first.
//!
//! [`PcmMixerNode`] attaches a per-client virtual [`Substream`] per `open()`
//! call, mixes all active client streams with fixed-point linear resampling
//! and format/channel conversion into the underlying hardware substream, and
//! closes the hardware stream cleanly when the last client exits.

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{
    sched::WaitQueue,
    sync::Mutex,
    vfs::{Inode, InodeKind, Stat, VfsError, tree::ino},
};

use super::{
    from_errno,
    pcm::{self, Direction, HardwareParams, PcmCaps, PcmOps, PcmSession, Substream},
    uapi,
};

const MAX_MIX_FRAMES: usize = 512;
const MIX_KNEE: i32 = 24_576;
const MIX_HEADROOM: i32 = 8_191;

/// Shared wakeup + teardown signal between `PcmMixer` and its worker kthread.
struct WorkerSignal {
    wake: WaitQueue,
    notify_seq: Arc<AtomicU64>,
    shutdown: AtomicBool,
}

/// Hardware configuration selected for the shared backing DMA substream.
#[derive(Clone, Copy)]
struct HwConfig {
    rate: u32,
    channels: u32,
    format: u32,
    period_size: u64,
    buffer_size: u64,
    start_threshold: u64,
}

/// Per-client mixer cursor and linear resampler state.
struct ClientStream {
    id: u64,
    substream: Arc<Substream>,
    ops: Arc<ClientPcmOps>,
    seen_reset: u64,
    read_ptr: u64,
    have_prev: bool,
    prev_l: i32,
    prev_r: i32,
    frac: u32,
    skip_frames: u64,
}

impl ClientStream {
    fn new(id: u64, substream: Arc<Substream>, ops: Arc<ClientPcmOps>) -> Self {
        Self {
            id,
            substream,
            ops,
            seen_reset: 0,
            read_ptr: 0,
            have_prev: false,
            prev_l: 0,
            prev_r: 0,
            frac: 0,
            skip_frames: 0,
        }
    }

    fn reset_cursor(&mut self, hw_ptr: u64, reset_seq: u64) {
        self.seen_reset = reset_seq;
        self.read_ptr = hw_ptr;
        self.have_prev = false;
        self.prev_l = 0;
        self.prev_r = 0;
        self.frac = 0;
        self.skip_frames = 0;
    }

    /// Mix as many frames as available up to `out_frames` into `acc_l`/`acc_r`.
    /// Returns `(rendered_frames, is_running_or_draining)` for this client.
    fn mix_into(
        &mut self,
        hw_rate: u32,
        out_frames: usize,
        acc_l: &mut [i32],
        acc_r: &mut [i32],
        contrib: &mut [u16],
    ) -> (usize, bool) {
        let reset_seq = self.ops.reset_seq.load(Ordering::Acquire);
        let substream = self.substream.clone();
        let mut advance_pointer = false;
        let mut consumed_any = 0usize;
        let is_active;

        {
            let runtime = substream.runtime.lock();
            if self.seen_reset != reset_seq {
                self.reset_cursor(runtime.hw_ptr, reset_seq);
            }
            let state = runtime.state;
            if state != uapi::SNDRV_PCM_STATE_RUNNING && state != uapi::SNDRV_PCM_STATE_DRAINING {
                return (0, false);
            }
            is_active = true;
            let Some(hw) = runtime.hw else {
                return (0, true);
            };
            let Some(pages) = runtime.pages.as_ref() else {
                return (0, true);
            };
            if runtime.boundary == 0 || hw.buffer_size == 0 || hw_rate == 0 {
                return (0, true);
            }

            let boundary = runtime.boundary;
            let draining = state == uapi::SNDRV_PCM_STATE_DRAINING;
            let mut queued = (runtime.appl_ptr + boundary - self.read_ptr) % boundary;
            if queued > hw.buffer_size {
                queued = 0;
            }
            if queued == 0 && !self.have_prev {
                if draining && self.skip_frames == 0 {
                    self.ops
                        .hw_cursor
                        .store(runtime.appl_ptr % hw.buffer_size, Ordering::Release);
                    advance_pointer = true;
                }
            } else {
                let step_fp = ((u64::from(hw.rate) << 32) / u64::from(hw_rate)).max(1);
                // Cap input frames consumed per pass below half the client ring so
                // `raw_hw_ptr` deltas in `period_elapsed` never alias modulo `buffer_size`.
                let max_consume = (hw.buffer_size / 2).max(1);
                let mut consumed_pass = 0u64;

                for out_idx in 0..out_frames {
                    while self.skip_frames > 0 && queued > 0 {
                        let take = self
                            .skip_frames
                            .min(queued)
                            .min(max_consume.saturating_sub(consumed_pass));
                        if take == 0 {
                            break;
                        }
                        self.read_ptr = (self.read_ptr + take) % boundary;
                        self.skip_frames -= take;
                        queued -= take;
                        consumed_pass += take;
                    }
                    if self.skip_frames > 0 {
                        if draining && queued == 0 {
                            self.skip_frames = 0;
                            self.frac = 0;
                        }
                        break;
                    }

                    if !self.have_prev {
                        if queued == 0 || consumed_pass >= max_consume {
                            if queued == 0 {
                                self.frac = 0;
                            }
                            break;
                        }
                        let (l, r) = read_ring_sample(&pages.ring, &hw, self.read_ptr);
                        self.prev_l = l;
                        self.prev_r = r;
                        self.have_prev = true;
                        self.read_ptr = (self.read_ptr + 1) % boundary;
                        queued -= 1;
                        consumed_pass += 1;
                    }

                    let (sl, sr) = if self.frac == 0 {
                        (self.prev_l, self.prev_r)
                    } else if queued == 0 {
                        if draining {
                            self.have_prev = false;
                            self.frac = 0;
                        }
                        break;
                    } else {
                        let (nl, nr) = read_ring_sample(&pages.ring, &hw, self.read_ptr);
                        let alpha = i64::from(self.frac >> 16);
                        let il =
                            self.prev_l + (((i64::from(nl - self.prev_l) * alpha) >> 16) as i32);
                        let ir =
                            self.prev_r + (((i64::from(nr - self.prev_r) * alpha) >> 16) as i32);
                        (il, ir)
                    };

                    let frac_acc = u64::from(self.frac) + step_fp;
                    let adv = frac_acc >> 32;
                    let new_frac = (frac_acc & 0xFFFF_FFFF) as u32;

                    if adv > 0 && adv <= queued && consumed_pass + adv > max_consume {
                        // Hit per-pass input limit before emitting this frame; pick it up next chunk.
                        break;
                    }

                    acc_l[out_idx] = acc_l[out_idx].saturating_add(sl);
                    acc_r[out_idx] = acc_r[out_idx].saturating_add(sr);
                    contrib[out_idx] = contrib[out_idx].saturating_add(1);
                    consumed_any += 1;

                    if adv == 0 {
                        self.frac = new_frac;
                        continue;
                    }
                    if adv <= queued {
                        let last_idx = (self.read_ptr + adv - 1) % boundary;
                        let (nl, nr) = read_ring_sample(&pages.ring, &hw, last_idx);
                        self.prev_l = nl;
                        self.prev_r = nr;
                        self.have_prev = true;
                        self.read_ptr = (self.read_ptr + adv) % boundary;
                        queued -= adv;
                        consumed_pass += adv;
                        self.frac = new_frac;
                    } else {
                        // Consumed everything queued in the client's ring.
                        let extra = adv - 1 - queued;
                        if queued > 0 {
                            let last_idx = (self.read_ptr + queued - 1) % boundary;
                            let (nl, nr) = read_ring_sample(&pages.ring, &hw, last_idx);
                            self.prev_l = nl;
                            self.prev_r = nr;
                            self.read_ptr = (self.read_ptr + queued) % boundary;
                            consumed_pass += queued;
                            queued = 0;
                        }
                        if extra == 0 && new_frac == 0 {
                            self.have_prev = false;
                            self.frac = 0;
                        } else if extra == 0 {
                            self.have_prev = true;
                            self.frac = new_frac;
                        } else {
                            self.have_prev = false;
                            self.skip_frames = extra;
                            self.frac = new_frac;
                        }
                    }
                }

                if consumed_pass > 0 || (draining && queued == 0) {
                    self.ops
                        .hw_cursor
                        .store(self.read_ptr % hw.buffer_size, Ordering::Release);
                    advance_pointer = true;
                }
            }
        }

        if advance_pointer {
            pcm::period_elapsed_direct(&self.substream);
        }
        (consumed_any, is_active)
    }
}

/// Read one frame from a client ring buffer as signed 16-bit-scaled `(L, R)`.
#[inline]
fn read_ring_sample(ring: &pcm::RingBuffer, hw: &HardwareParams, ptr: u64) -> (i32, i32) {
    if ring.frames == 0 || ring.frame_bytes == 0 {
        return (0, 0);
    }
    let frame_idx = (ptr % ring.frames) as usize;
    let offset = frame_idx * ring.frame_bytes;
    if offset + ring.frame_bytes > ring.bytes {
        return (0, 0);
    }
    // SAFETY: `offset + ring.frame_bytes <= ring.bytes`, so the frame lies wholly
    // inside the coherent ring allocation.
    let bytes =
        unsafe { core::slice::from_raw_parts(ring.pointer().add(offset), ring.frame_bytes) };
    let channels = hw.channels as usize;
    match hw.format {
        uapi::SNDRV_PCM_FORMAT_S16_LE => {
            if bytes.len() < 2 {
                return (0, 0);
            }
            let l = i32::from(i16::from_le_bytes([bytes[0], bytes[1]]));
            let r = if channels >= 2 && bytes.len() >= 4 {
                i32::from(i16::from_le_bytes([bytes[2], bytes[3]]))
            } else {
                l
            };
            (l, r)
        }
        uapi::SNDRV_PCM_FORMAT_U8 => {
            if bytes.is_empty() {
                return (0, 0);
            }
            let l = (i32::from(bytes[0]) - 128) << 8;
            let r = if channels >= 2 && bytes.len() >= 2 {
                (i32::from(bytes[1]) - 128) << 8
            } else {
                l
            };
            (l, r)
        }
        uapi::SNDRV_PCM_FORMAT_S32_LE => {
            if bytes.len() < 4 {
                return (0, 0);
            }
            let l = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) >> 16;
            let r = if channels >= 2 && bytes.len() >= 8 {
                i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) >> 16
            } else {
                l
            };
            (l, r)
        }
        _ => (0, 0),
    }
}

/// Unity gain up to `-2.5 dBFS` (`MIX_KNEE`), then hyperbolic soft-knee limiting
/// so overlapping SFX and music never hard-clip or crackle.
#[inline]
fn soft_limit_i16(sample: i32, active_streams: u16) -> i16 {
    if active_streams <= 1 {
        return sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    }
    let sign = if sample < 0 { -1 } else { 1 };
    let mag = sample.unsigned_abs();
    if mag <= MIX_KNEE as u32 {
        sample as i16
    } else {
        let excess = u64::from(mag - MIX_KNEE as u32);
        let headroom = u64::from(MIX_HEADROOM as u32);
        let compressed = (excess * headroom) / (excess + headroom);
        (sign * (MIX_KNEE + compressed as i32)).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
            as i16
    }
}

/// Encode mixed `(L, R)` accumulators directly into hardware PCM frame bytes.
fn encode_hw_frames(
    out: &mut [u8],
    frames: usize,
    acc_l: &[i32],
    acc_r: &[i32],
    contrib: &[u16],
    cfg: HwConfig,
) -> usize {
    let mut pos = 0usize;
    match (cfg.format, cfg.channels) {
        (uapi::SNDRV_PCM_FORMAT_S16_LE, 2) => {
            for i in 0..frames {
                let l = soft_limit_i16(acc_l[i], contrib[i]).to_le_bytes();
                let r = soft_limit_i16(acc_r[i], contrib[i]).to_le_bytes();
                out[pos] = l[0];
                out[pos + 1] = l[1];
                out[pos + 2] = r[0];
                out[pos + 3] = r[1];
                pos += 4;
            }
        }
        (uapi::SNDRV_PCM_FORMAT_S16_LE, 1) => {
            for i in 0..frames {
                let mono = (acc_l[i] + acc_r[i]) / 2;
                let s = soft_limit_i16(mono, contrib[i]).to_le_bytes();
                out[pos] = s[0];
                out[pos + 1] = s[1];
                pos += 2;
            }
        }
        (uapi::SNDRV_PCM_FORMAT_U8, 2) => {
            for i in 0..frames {
                let l = ((soft_limit_i16(acc_l[i], contrib[i]) >> 8) + 128) as u8;
                let r = ((soft_limit_i16(acc_r[i], contrib[i]) >> 8) + 128) as u8;
                out[pos] = l;
                out[pos + 1] = r;
                pos += 2;
            }
        }
        (uapi::SNDRV_PCM_FORMAT_U8, _) => {
            for i in 0..frames {
                let mono = (acc_l[i] + acc_r[i]) / 2;
                out[pos] = ((soft_limit_i16(mono, contrib[i]) >> 8) + 128) as u8;
                pos += 1;
            }
        }
        _ => {
            let sample_bytes = uapi::format_bytes(cfg.format).unwrap_or(2);
            pos = frames * sample_bytes * cfg.channels as usize;
            out[..pos].fill(0);
        }
    }
    pos
}

struct MixerInner {
    hw_substream: Arc<Substream>,
    hw_lock: Mutex<()>,
    hw_active: Mutex<Option<HwConfig>>,
    clients: Mutex<Vec<ClientStream>>,
    next_id: AtomicU64,
    worker_started: AtomicBool,
    signal: Arc<WorkerSignal>,
    idle_zero_frames: Mutex<u64>,
}

impl MixerInner {
    fn open_hw_locked(&self) -> Result<HwConfig, VfsError> {
        if let Some(cfg) = *self.hw_active.lock() {
            return Ok(cfg);
        }
        if self.hw_substream.opened.swap(true, Ordering::AcqRel) {
            return Err(VfsError::WouldBlock);
        }
        if let Some(ops) = self.hw_substream.ops.lock().clone()
            && let Err(err) = ops.open(&self.hw_substream)
        {
            self.hw_substream.opened.store(false, Ordering::Release);
            return Err(from_errno(err));
        }

        let caps = *self.hw_substream.caps.lock();
        let rate = if (caps.rate_min..=caps.rate_max).contains(&48_000) {
            48_000
        } else {
            44_100.clamp(caps.rate_min, caps.rate_max)
        };
        let channels = 2u32.clamp(caps.channels_min.max(1), caps.channels_max.max(1));
        let format = if caps.formats & uapi::format_bit(uapi::SNDRV_PCM_FORMAT_S16_LE) != 0 {
            uapi::SNDRV_PCM_FORMAT_S16_LE
        } else {
            uapi::SNDRV_PCM_FORMAT_U8
        };
        let sample_bytes = uapi::format_bytes(format).unwrap_or(2);
        let frame_bytes = sample_bytes * channels as usize;
        let mut period_size = 1024u32;
        if caps.period_step_bytes > 1 && frame_bytes > 0 {
            let step_frames = caps.period_step_bytes.div_ceil(frame_bytes).max(1) as u32;
            period_size = period_size.div_ceil(step_frames) * step_frames;
        }
        let max_buf_frames =
            (caps.buffer_bytes_max / frame_bytes.max(1)).max(period_size as usize * 2) as u32;
        let periods = 8u32
            .min((max_buf_frames / period_size.max(1)).max(2))
            .clamp(caps.periods_min.max(2), caps.periods_max.max(2));
        let buffer_size = period_size.saturating_mul(periods);

        let mut params = uapi::snd_pcm_hw_params::zeroed();
        params.masks[uapi::SNDRV_PCM_HW_PARAM_ACCESS as usize]
            .set(uapi::SNDRV_PCM_ACCESS_RW_INTERLEAVED);
        params.masks[uapi::SNDRV_PCM_HW_PARAM_FORMAT as usize].set(format);
        params.masks[uapi::SNDRV_PCM_HW_PARAM_SUBFORMAT as usize]
            .set(uapi::SNDRV_PCM_SUBFORMAT_STD);
        params.intervals[(uapi::SNDRV_PCM_HW_PARAM_CHANNELS - 8) as usize] =
            uapi::snd_interval::exact(channels);
        params.intervals[(uapi::SNDRV_PCM_HW_PARAM_RATE - 8) as usize] =
            uapi::snd_interval::exact(rate);
        params.intervals[(uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE - 8) as usize] =
            uapi::snd_interval::exact(period_size);
        params.intervals[(uapi::SNDRV_PCM_HW_PARAM_PERIODS - 8) as usize] =
            uapi::snd_interval::exact(periods);
        params.intervals[(uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE - 8) as usize] =
            uapi::snd_interval::exact(buffer_size);

        if let Err(err) = pcm::apply_hw_params(&self.hw_substream, &mut params) {
            if let Some(ops) = self.hw_substream.ops.lock().clone() {
                ops.close(&self.hw_substream);
            }
            self.hw_substream.opened.store(false, Ordering::Release);
            return Err(err);
        }

        let start_threshold = {
            let mut runtime = self.hw_substream.runtime.lock();
            let hw = runtime.hw.ok_or(VfsError::InvalidArgument)?;
            runtime.sw.start_threshold = hw.period_size.min(hw.buffer_size).max(1);
            runtime.sw.avail_min = hw.period_size.max(1);
            pcm::publish(&self.hw_substream, &mut runtime);
            runtime.sw.start_threshold
        };

        if let Err(err) = pcm::prepare_one(&self.hw_substream) {
            pcm::release_buffer(&self.hw_substream);
            if let Some(ops) = self.hw_substream.ops.lock().clone() {
                ops.hw_free(&self.hw_substream);
                ops.close(&self.hw_substream);
            }
            self.hw_substream.opened.store(false, Ordering::Release);
            return Err(err);
        }

        *self.hw_substream.forward_wake.lock() = Some((
            self.signal.wake.clone(),
            Some(self.signal.notify_seq.clone()),
        ));
        let cfg = HwConfig {
            rate,
            channels,
            format,
            period_size: u64::from(period_size),
            buffer_size: u64::from(buffer_size),
            start_threshold,
        };
        *self.hw_active.lock() = Some(cfg);
        *self.idle_zero_frames.lock() = 0;
        Ok(cfg)
    }

    fn teardown_hw_locked(&self) {
        if self.hw_active.lock().take().is_none() {
            return;
        }
        *self.hw_substream.forward_wake.lock() = None;
        if matches!(
            self.hw_substream.state.load(Ordering::Acquire),
            uapi::SNDRV_PCM_STATE_RUNNING | uapi::SNDRV_PCM_STATE_DRAINING
        ) {
            let _ = pcm::trigger(&self.hw_substream, uapi::SNDRV_PCM_TRIGGER_STOP);
        }
        pcm::release_buffer(&self.hw_substream);
        if let Some(ops) = self.hw_substream.ops.lock().clone() {
            ops.hw_free(&self.hw_substream);
            ops.close(&self.hw_substream);
        }
        self.hw_substream.opened.store(false, Ordering::Release);
        *self.idle_zero_frames.lock() = 0;
    }

    fn ensure_worker_started(self: &Arc<Self>) {
        if !self.worker_started.swap(true, Ordering::AcqRel) {
            let weak = Arc::downgrade(self);
            let signal = self.signal.clone();
            let hw_substream = self.hw_substream.clone();
            if crate::sched::spawn(move || worker_loop(weak, signal, hw_substream), -5).is_err() {
                self.worker_started.store(false, Ordering::Release);
            }
        }
    }

    fn add_client(self: &Arc<Self>, nonblock: bool) -> Result<Arc<PcmSession>, VfsError> {
        let _guard = self.hw_lock.lock();
        self.open_hw_locked()?;

        let id = self.next_id.fetch_add(1, Ordering::AcqRel) + 1;
        let info_flags = self.hw_substream.info_flags & !uapi::SNDRV_PCM_INFO_NONINTERLEAVED;
        let client_sub = Substream::new(
            self.hw_substream.card,
            self.hw_substream.device,
            id as u32,
            Direction::Playback,
            &self.hw_substream.id,
            &self.hw_substream.name,
            info_flags,
        );
        *client_sub.caps.lock() = PcmCaps::default_hardware();
        *client_sub.forward_wake.lock() = Some((
            self.signal.wake.clone(),
            Some(self.signal.notify_seq.clone()),
        ));

        let ops = Arc::new(ClientPcmOps {
            mixer: Arc::downgrade(self),
            client_id: id,
            hw_cursor: AtomicU64::new(0),
            reset_seq: AtomicU64::new(1),
            signal: self.signal.clone(),
        });
        *client_sub.ops.lock() = Some(ops.clone());
        client_sub.opened.store(true, Ordering::Release);

        self.clients
            .lock()
            .push(ClientStream::new(id, client_sub.clone(), ops));
        self.ensure_worker_started();
        self.signal.notify_seq.fetch_add(1, Ordering::AcqRel);
        self.signal.wake.wake_all();

        Ok(PcmSession::session(client_sub, nonblock))
    }

    fn remove_client(&self, client_id: u64, client_sub: &Arc<Substream>) {
        let _guard = self.hw_lock.lock();
        let empty = {
            let mut clients = self.clients.lock();
            clients.retain(|client| client.id != client_id);
            clients.is_empty()
        };
        *client_sub.forward_wake.lock() = None;
        pcm::release_buffer(client_sub);
        if empty {
            self.teardown_hw_locked();
        } else {
            self.signal.notify_seq.fetch_add(1, Ordering::AcqRel);
            self.signal.wake.wake_all();
        }
    }

    /// Execute one mix-and-feed iteration. Returns `true` if the worker should
    /// immediately loop to push more frames (e.g. pre-filling to start threshold).
    fn step(&self) -> bool {
        let _guard = self.hw_lock.lock();
        let Some(cfg) = *self.hw_active.lock() else {
            return false;
        };

        let hw_state_initial = self.hw_substream.state.load(Ordering::Acquire);
        if hw_state_initial == uapi::SNDRV_PCM_STATE_RUNNING {
            pcm::period_elapsed_direct(&self.hw_substream);
        }
        if self.hw_substream.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_XRUN {
            if pcm::prepare_one(&self.hw_substream).is_err() {
                return false;
            }
            *self.idle_zero_frames.lock() = 0;
        }

        let (hw_state, hw_queued, hw_avail) = {
            let runtime = self.hw_substream.runtime.lock();
            let Some(hw) = runtime.hw else {
                return false;
            };
            if runtime.boundary == 0 {
                return false;
            }
            let queued = ((runtime.appl_ptr + runtime.boundary - runtime.hw_ptr)
                % runtime.boundary)
                .min(hw.buffer_size);
            let avail = pcm::avail_locked(&runtime, Direction::Playback);
            (runtime.state, queued, avail)
        };

        if hw_avail == 0 {
            return false;
        }

        let active_target = (cfg.period_size * 4).min(cfg.buffer_size);
        let max_want = active_target.saturating_sub(hw_queued).min(hw_avail);
        if max_want == 0 && hw_state == uapi::SNDRV_PCM_STATE_RUNNING {
            return false;
        }
        let chunk = max_want.min(MAX_MIX_FRAMES as u64) as usize;

        let mut acc_l = [0i32; MAX_MIX_FRAMES];
        let mut acc_r = [0i32; MAX_MIX_FRAMES];
        let mut contrib = [0u16; MAX_MIX_FRAMES];
        let mut rendered = 0usize;
        let mut any_client_active = false;

        if chunk > 0 {
            let mut clients = self.clients.lock();
            for client in clients.iter_mut() {
                let (c_rendered, c_active) =
                    client.mix_into(cfg.rate, chunk, &mut acc_l, &mut acc_r, &mut contrib);
                rendered = rendered.max(c_rendered);
                any_client_active |= c_active;
            }
        }

        let write_frames = if rendered > 0 {
            *self.idle_zero_frames.lock() = 0;
            rendered
        } else if any_client_active && hw_state == uapi::SNDRV_PCM_STATE_RUNNING {
            // Client is still RUNNING but temporarily starved: keep a slim
            // 2-period cushion so brief decode jitter doesn't XRUN hardware.
            let slim_target = (cfg.period_size * 2).min(cfg.buffer_size);
            slim_target
                .saturating_sub(hw_queued)
                .min(hw_avail)
                .min(MAX_MIX_FRAMES as u64) as usize
        } else if hw_state == uapi::SNDRV_PCM_STATE_PREPARED
            && hw_queued > 0
            && hw_queued < cfg.start_threshold
        {
            // Flush a tiny/draining (< start_threshold) clip by padding silence to start DMA.
            cfg.start_threshold
                .saturating_sub(hw_queued)
                .min(hw_avail)
                .min(MAX_MIX_FRAMES as u64) as usize
        } else if hw_state == uapi::SNDRV_PCM_STATE_RUNNING {
            let mut idle_zeros = self.idle_zero_frames.lock();
            if *idle_zeros >= cfg.period_size.saturating_mul(4).min(cfg.buffer_size) {
                drop(idle_zeros);
                let _ = pcm::trigger_one(&self.hw_substream, uapi::SNDRV_PCM_TRIGGER_STOP);
                let _ = pcm::prepare_one(&self.hw_substream);
                *self.idle_zero_frames.lock() = 0;
                return false;
            }
            let slim_target = (cfg.period_size * 2).min(cfg.buffer_size);
            let z = slim_target
                .saturating_sub(hw_queued)
                .min(hw_avail)
                .min(MAX_MIX_FRAMES as u64) as usize;
            *idle_zeros += z as u64;
            z
        } else {
            return false;
        };

        if write_frames == 0 {
            return false;
        }

        let mut encoded = [0u8; MAX_MIX_FRAMES * 4];
        let encoded_bytes =
            encode_hw_frames(&mut encoded, write_frames, &acc_l, &acc_r, &contrib, cfg);

        let new_queued = {
            let mut runtime = self.hw_substream.runtime.lock();
            if runtime.state != uapi::SNDRV_PCM_STATE_PREPARED
                && runtime.state != uapi::SNDRV_PCM_STATE_RUNNING
            {
                return false;
            }
            let Some(pages) = runtime.pages.clone() else {
                return false;
            };
            let ring = &pages.ring;
            let avail_now = pcm::avail_locked(&runtime, Direction::Playback) as usize;
            let take = write_frames.min(avail_now);
            if take == 0 {
                return false;
            }
            let bytes = (take * ring.frame_bytes).min(encoded_bytes);
            let start = (runtime.appl_ptr % ring.frames) as usize * ring.frame_bytes;
            let first = bytes.min(ring.bytes - start);
            // SAFETY: `start` and `first` stay inside the hardware ring buffer,
            // and `bytes <= encoded_bytes`.
            unsafe {
                core::ptr::copy_nonoverlapping(encoded.as_ptr(), ring.pointer().add(start), first);
                if bytes > first {
                    core::ptr::copy_nonoverlapping(
                        encoded.as_ptr().add(first),
                        ring.pointer(),
                        bytes - first,
                    );
                }
            }
            runtime.appl_ptr = (runtime.appl_ptr + take as u64) % runtime.boundary;
            runtime.tstamp = pcm::now();
            pcm::publish(&self.hw_substream, &mut runtime);
            ((runtime.appl_ptr + runtime.boundary - runtime.hw_ptr) % runtime.boundary)
                .min(cfg.buffer_size)
        };

        pcm::start_if_threshold(&self.hw_substream);

        // Keep filling synchronously until we either reach the hardware cushion or run dry.
        rendered > 0 && new_queued < active_target && hw_avail > write_frames as u64
    }
}

/// Arm a one-shot fallback wakeup in case a period interrupt is lost (MSI
/// coalescing, virtualized DMA stalls). Linux dmix relies purely on period
/// wakeups; the watchdog only guarantees forward progress and never fires
/// more than once per hardware period while the DMA is running.
fn arm_watchdog(signal: &Arc<WorkerSignal>, delay_us: u64) -> crate::net::timer::TimerId {
    let weak_signal = Arc::downgrade(signal);
    crate::net::timer::schedule_in(
        delay_us,
        alloc::boxed::Box::new(move || {
            if let Some(signal) = weak_signal.upgrade() {
                signal.notify_seq.fetch_add(1, Ordering::AcqRel);
                signal.wake.wake_all();
            }
        }),
    )
}

fn worker_loop(weak: Weak<MixerInner>, signal: Arc<WorkerSignal>, hw_substream: Arc<Substream>) {
    while !signal.shutdown.load(Ordering::Acquire) {
        // Sample the notification sequence *before* mixing so an IRQ or client
        // trigger racing `step()` is never lost. The hardware substream's own
        // `generation` is deliberately not part of the predicate: `step()`
        // publishes to it on every refill, which previously turned the worker
        // into a busy poll of the HDA LPIB register that starved userspace.
        let seen_seq = signal.notify_seq.load(Ordering::Acquire);

        let Some(mixer) = weak.upgrade() else {
            break;
        };
        let keep_going = mixer.step();
        let watchdog_us = (*mixer.hw_active.lock()).and_then(|cfg| {
            (hw_substream.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_RUNNING)
                .then(|| (cfg.period_size * 1_000_000 / u64::from(cfg.rate.max(1))).max(2_000))
        });
        drop(mixer);

        if keep_going || signal.shutdown.load(Ordering::Acquire) {
            continue;
        }
        let watchdog = watchdog_us.map(|delay| arm_watchdog(&signal, delay));
        signal.wake.wait_until(|| {
            signal.shutdown.load(Ordering::Acquire)
                || signal.notify_seq.load(Ordering::Acquire) != seen_seq
        });
        if let Some(id) = watchdog {
            crate::net::timer::cancel(id);
        }
    }
}

struct ClientPcmOps {
    mixer: Weak<MixerInner>,
    client_id: u64,
    hw_cursor: AtomicU64,
    reset_seq: AtomicU64,
    signal: Arc<WorkerSignal>,
}

impl PcmOps for ClientPcmOps {
    fn prepare(&self, _substream: &Substream) {
        self.hw_cursor.store(0, Ordering::Release);
        self.reset_seq.fetch_add(1, Ordering::AcqRel);
        self.signal.notify_seq.fetch_add(1, Ordering::AcqRel);
        self.signal.wake.wake_all();
    }

    fn trigger(&self, _substream: &Substream, _cmd: i32) -> Result<(), i32> {
        self.signal.notify_seq.fetch_add(1, Ordering::AcqRel);
        self.signal.wake.wake_all();
        Ok(())
    }

    fn pointer(&self, _substream: &Substream) -> u64 {
        self.hw_cursor.load(Ordering::Acquire)
    }

    fn close(&self, substream: &Substream) {
        let Some(mixer) = self.mixer.upgrade() else {
            return;
        };
        // Find the `Arc<Substream>` by pointer match (avoid holding any locks on `Substream`
        // while `hw_lock` is taken inside `remove_client`).
        let found = {
            let clients = mixer.clients.lock();
            clients
                .iter()
                .find(|client| core::ptr::eq(Arc::as_ptr(&client.substream), substream))
                .map(|client| client.substream.clone())
        };
        if let Some(client_sub) = found {
            mixer.remove_client(self.client_id, &client_sub);
        }
    }
}

/// Multi-stream software mixing PCM device node (`pcmC<n>D<m>p`).
pub struct PcmMixerNode {
    inner: Arc<MixerInner>,
}

impl Drop for PcmMixerNode {
    fn drop(&mut self) {
        self.inner.signal.shutdown.store(true, Ordering::Release);
        self.inner.signal.wake.wake_all();
        let _guard = self.inner.hw_lock.lock();
        self.inner.teardown_hw_locked();
    }
}

impl PcmMixerNode {
    pub fn new(hw_substream: Arc<Substream>) -> Arc<Self> {
        let signal = Arc::new(WorkerSignal {
            wake: WaitQueue::new(),
            notify_seq: Arc::new(AtomicU64::new(0)),
            shutdown: AtomicBool::new(false),
        });
        Arc::new(Self {
            inner: Arc::new(MixerInner {
                hw_substream,
                hw_lock: Mutex::new(()),
                hw_active: Mutex::new(None),
                clients: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(0),
                worker_started: AtomicBool::new(false),
                signal,
                idle_zero_frames: Mutex::new(0),
            }),
        })
    }
}

impl Inode for PcmMixerNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        let nonblock = flags & crate::vfs::abi::O_NONBLOCK != 0;
        let session: Arc<dyn Inode> = self.inner.add_client(nonblock)?;
        Ok(Some(session))
    }

    fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }

    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
}

#[cfg(feature = "boot-self-test")]
fn fill_client_test_frames(
    substream: &Arc<Substream>,
    rate: u32,
    channels: u32,
    format: u32,
    samples: &[i16],
) -> bool {
    let mut params = uapi::snd_pcm_hw_params::zeroed();
    params.masks[uapi::SNDRV_PCM_HW_PARAM_ACCESS as usize]
        .set(uapi::SNDRV_PCM_ACCESS_RW_INTERLEAVED);
    params.masks[uapi::SNDRV_PCM_HW_PARAM_FORMAT as usize].set(format);
    params.masks[uapi::SNDRV_PCM_HW_PARAM_SUBFORMAT as usize].set(uapi::SNDRV_PCM_SUBFORMAT_STD);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_CHANNELS - 8) as usize] =
        uapi::snd_interval::exact(channels);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_RATE - 8) as usize] =
        uapi::snd_interval::exact(rate);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE - 8) as usize] =
        uapi::snd_interval::exact(512);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_PERIODS - 8) as usize] =
        uapi::snd_interval::exact(4);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE - 8) as usize] =
        uapi::snd_interval::exact(2048);

    if pcm::apply_hw_params(substream, &mut params).is_err() || pcm::prepare_one(substream).is_err()
    {
        return false;
    }

    {
        let mut runtime = substream.runtime.lock();
        let Some(pages) = runtime.pages.as_ref() else {
            return false;
        };
        let ptr = pages.ring.pointer();
        let frames = match (format, channels) {
            (uapi::SNDRV_PCM_FORMAT_U8, 1) => {
                for (i, &s) in samples.iter().enumerate() {
                    let byte = ((s >> 8) + 128).clamp(0, 255) as u8;
                    unsafe { *ptr.add(i) = byte };
                }
                samples.len() as u64
            }
            (uapi::SNDRV_PCM_FORMAT_S16_LE, 2) => {
                for (i, &s) in samples.iter().enumerate() {
                    let le = s.to_le_bytes();
                    unsafe {
                        *ptr.add(i * 2) = le[0];
                        *ptr.add(i * 2 + 1) = le[1];
                    }
                }
                (samples.len() / 2) as u64
            }
            _ => return false,
        };
        runtime.appl_ptr = frames % runtime.boundary;
        pcm::publish(substream, &mut runtime);
    }

    pcm::trigger(substream, uapi::SNDRV_PCM_TRIGGER_START).is_ok()
}

#[cfg(feature = "boot-self-test")]
pub fn self_test_late() -> bool {
    struct TestHwOps(AtomicU64);
    impl PcmOps for TestHwOps {
        fn pointer(&self, _substream: &Substream) -> u64 {
            self.0.load(Ordering::Acquire)
        }
    }

    if soft_limit_i16(10_000, 1) != 10_000 || soft_limit_i16(10_000, 2) != 10_000 {
        return false;
    }
    let clipped_hi = soft_limit_i16(25_000 + 25_000, 2);
    let clipped_lo = soft_limit_i16(-25_000 - 25_000, 2);
    if !(MIX_KNEE as i16..=i16::MAX).contains(&clipped_hi)
        || !(i16::MIN..=-(MIX_KNEE as i16)).contains(&clipped_lo)
    {
        return false;
    }

    let hw_ops = Arc::new(TestHwOps(AtomicU64::new(0)));
    let hw_sub = Substream::new(
        99,
        0,
        0,
        Direction::Playback,
        "mix-test",
        "Mixer Self-Test",
        uapi::SNDRV_PCM_INFO_INTERLEAVED,
    );
    *hw_sub.ops.lock() = Some(hw_ops.clone());

    let node = PcmMixerNode::new(hw_sub.clone());
    let Ok(Some(inode1)) = node.open(0) else {
        return false;
    };
    let Ok(Some(inode2)) = node.open(0) else {
        return false;
    };
    let Some(sub1) = inode1.sound_substream() else {
        return false;
    };
    let Some(sub2) = inode2.sound_substream() else {
        return false;
    };

    let s1 = [12_000i16; 400];
    let s2 = [8_000i16; 800];
    if !fill_client_test_frames(&sub1, 22_050, 1, uapi::SNDRV_PCM_FORMAT_U8, &s1)
        || !fill_client_test_frames(&sub2, 44_100, 2, uapi::SNDRV_PCM_FORMAT_S16_LE, &s2)
    {
        return false;
    }

    // Wake worker and give it up to 40ms to resample + mix both streams into `hw_sub`.
    let mut mixed_nonzero = false;
    let mut hw_appl = 0u64;
    for _ in 0..40 {
        crate::sched::sleep_micros(5_000);
        let runtime = hw_sub.runtime.lock();
        hw_appl = runtime.appl_ptr;
        if hw_appl >= 256
            && let Some(pages) = runtime.pages.as_ref()
        {
            let slice = unsafe {
                core::slice::from_raw_parts(
                    pages.ring.pointer(),
                    (hw_appl as usize * 4).min(pages.ring.bytes),
                )
            };
            mixed_nonzero = slice.chunks_exact(4).take(64).any(|frame| {
                let l = i16::from_le_bytes([frame[0], frame[1]]);
                let r = i16::from_le_bytes([frame[2], frame[3]]);
                l > 15_000 && r > 15_000
            });
        }
        drop(runtime);
        if mixed_nonzero && sub1.runtime.lock().hw_ptr > 0 && sub2.runtime.lock().hw_ptr > 0 {
            break;
        }
    }

    let c1_progressed = sub1.runtime.lock().hw_ptr > 0;
    let c2_progressed = sub2.runtime.lock().hw_ptr > 0;

    // Advance mock DMA pointer once so period_elapsed path runs against hw_sub too.
    hw_ops.0.store(256, Ordering::Release);
    pcm::period_elapsed(&hw_sub);

    drop(sub1);
    drop(sub2);
    drop(inode1);
    drop(inode2);

    let hw_closed = !hw_sub.opened.load(Ordering::Acquire);
    drop(node);

    hw_appl > 0 && mixed_nonzero && c1_progressed && c2_progressed && hw_closed
}
