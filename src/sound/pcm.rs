/*
 *
 *       src/sound/pcm.rs
 *       PCM runtime, constraint solver, ring buffer and ALSA PCM ioctls
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The PCM core follows Linux's `sound/core/pcm_native.c` split: a substream
//! owns the runtime, hardware parameters are solved against driver-declared
//! capabilities, and the state machine only changes through `hw_params`,
//! `prepare`, trigger and xrun paths.  The ring buffer and the status/control
//! pages are coherent DMA allocations mapped into userspace at the offsets the
//! ALSA UAPI reserves.
//!
//! Drivers implement [`PcmOps`].  A hardware driver advances the hardware
//! pointer in its period interrupt and calls [`period_elapsed`]; the core does
//! the wrapping, availability accounting, xrun detection and wakeups.

use alloc::{
    string::{String, ToString},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use crate::{
    ipc::pollwait,
    mm::dma::{self, Coherent},
    sched::WaitQueue,
    sync::{Mutex, SpinLock},
    time,
    user::uaccess,
    vfs::{Inode, InodeKind, Stat, VfsError, tree::ino},
};

use super::{from_errno, set_string, uapi};

pub const POLLIN: u32 = 1;
pub const POLLOUT: u32 = 4;
pub const POLLERR: u32 = 8;
pub const POLLHUP: u32 = 16;

const PAGE_SIZE: usize = 4096;
const MAX_RW_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Playback,
    Capture,
}

impl Direction {
    pub fn stream(self) -> i32 {
        match self {
            Self::Playback => uapi::SNDRV_PCM_STREAM_PLAYBACK,
            Self::Capture => uapi::SNDRV_PCM_STREAM_CAPTURE,
        }
    }
    pub fn suffix(self) -> char {
        match self {
            Self::Playback => 'p',
            Self::Capture => 'c',
        }
    }
}

/// Driver-declared capabilities used by the hardware-parameter solver.
#[derive(Clone, Copy)]
pub struct PcmCaps {
    pub formats: u64,
    pub rate_min: u32,
    pub rate_max: u32,
    pub channels_min: u32,
    pub channels_max: u32,
    pub periods_min: u32,
    pub periods_max: u32,
    pub buffer_bytes_max: usize,
    pub period_step_bytes: usize,
}

impl PcmCaps {
    pub fn default_hardware() -> Self {
        Self {
            formats: uapi::format_bit(uapi::SNDRV_PCM_FORMAT_S16_LE)
                | uapi::format_bit(uapi::SNDRV_PCM_FORMAT_S32_LE)
                | uapi::format_bit(uapi::SNDRV_PCM_FORMAT_U8),
            rate_min: 8000,
            rate_max: 192_000,
            channels_min: 1,
            channels_max: 2,
            periods_min: 2,
            periods_max: 32,
            buffer_bytes_max: 1024 * 1024,
            period_step_bytes: 0,
        }
    }
}

/// Values fixed by a successful `HW_PARAMS`.
#[derive(Clone, Copy)]
pub struct HardwareParams {
    pub access: u32,
    pub format: u32,
    pub channels: u32,
    pub rate: u32,
    pub sample_bits: u32,
    pub frame_bits: u32,
    pub frame_bytes: usize,
    pub period_size: u64,
    pub period_bytes: u64,
    pub periods: u32,
    pub buffer_size: u64,
    pub buffer_bytes: u64,
    pub noninterleaved: bool,
}

pub struct RingBuffer {
    coherent: Coherent,
    pub bytes: usize,
    pub mapped_bytes: usize,
    pub frames: u64,
    pub frame_bytes: usize,
}

impl RingBuffer {
    pub(crate) fn pointer(&self) -> *mut u8 {
        self.coherent.as_ptr::<u8>()
    }
    fn dma_address(&self) -> u64 {
        self.coherent.dma_address()
    }
    fn zero(&self) {
        // SAFETY: the coherent allocation is writable and `mapped_bytes` is
        // exactly the allocation length rounded to pages.
        unsafe { core::ptr::write_bytes(self.pointer(), 0, self.mapped_bytes) }
    }
}

/// The ring and its two mmap pages share one reference-counted allocation.
/// A userspace mapping holds an [`MmapPin`] to this set, so `HW_FREE` or a new
/// `HW_PARAMS` drops only the runtime's reference; physical pages stay alive
/// until the last mapping is torn down.
pub struct BufferSet {
    pub ring: RingBuffer,
    pub status: Coherent,
    pub control: Coherent,
}

struct MmapPin {
    _pages: Arc<BufferSet>,
}

impl Inode for MmapPin {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }
}

pub struct Runtime {
    pub hw: Option<HardwareParams>,
    pub sw: uapi::snd_pcm_sw_params,
    pub pages: Option<Arc<BufferSet>>,
    pub hw_ptr: u64,
    pub raw_hw_ptr: u64,
    pub appl_ptr: u64,
    pub boundary: u64,
    pub state: i32,
    pub suspended_state: i32,
    pub trigger_tstamp: uapi::timespec,
    pub tstamp: uapi::timespec,
    pub last_elapsed_us: u64,
    pub avail_max: u64,
    pub overrange: u64,
    pub user_pversion: u32,
    pub tstamp_mode: i32,
    pub tstamp_type: i32,
    pub no_period_wakeup: bool,
}

impl Runtime {
    fn new() -> Self {
        Self {
            hw: None,
            sw: uapi::snd_pcm_sw_params::zeroed(),
            pages: None,
            hw_ptr: 0,
            raw_hw_ptr: 0,
            appl_ptr: 0,
            boundary: 0,
            state: uapi::SNDRV_PCM_STATE_OPEN,
            suspended_state: 0,
            trigger_tstamp: uapi::timespec::zero(),
            tstamp: uapi::timespec::zero(),
            last_elapsed_us: 0,
            avail_max: 0,
            overrange: 0,
            user_pversion: 0,
            tstamp_mode: uapi::SNDRV_PCM_TSTAMP_NONE,
            tstamp_type: 0,
            no_period_wakeup: false,
        }
    }
}

/// Driver-facing operations.  All callbacks run without any core lock held.
pub trait PcmOps: Send + Sync {
    fn open(&self, _substream: &Substream) -> Result<(), i32> {
        Ok(())
    }
    fn close(&self, _substream: &Substream) {}
    fn hw_params(
        &self,
        _substream: &Substream,
        _params: &uapi::snd_pcm_hw_params,
    ) -> Result<(), i32> {
        Ok(())
    }
    fn hw_free(&self, _substream: &Substream) {}
    fn prepare(&self, _substream: &Substream) {}
    fn trigger(&self, _substream: &Substream, _cmd: i32) -> Result<(), i32> {
        Ok(())
    }
    /// Current hardware position in frames, already wrapped to `boundary` by
    /// the core.  It must not sleep or take the runtime lock.
    fn pointer(&self, _substream: &Substream) -> u64 {
        0
    }
}

pub struct Substream {
    pub card: u32,
    pub device: u32,
    pub subdevice: u32,
    pub direction: Direction,
    pub id: String,
    pub name: String,
    pub info_flags: u32,
    pub ops: Mutex<Option<Arc<dyn PcmOps>>>,
    pub runtime: Mutex<Runtime>,
    pub caps: Mutex<PcmCaps>,
    pub wait: WaitQueue,
    pub opened: AtomicBool,
    pub generation: AtomicU64,
    pub avail: AtomicU64,
    pub avail_min: AtomicU64,
    pub state: AtomicI32,
    pub isa_dma: AtomicBool,
    pub(crate) forward_wake: SpinLock<Option<(WaitQueue, Option<Arc<AtomicU64>>)>>,
    self_reference: SpinLock<Weak<Substream>>,
    siblings: Mutex<Vec<Weak<Substream>>>,
    group: Mutex<Vec<Weak<Substream>>>,
}

impl Substream {
    pub fn new(
        card: u32,
        device: u32,
        subdevice: u32,
        direction: Direction,
        id: &str,
        name: &str,
        info_flags: u32,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            card,
            device,
            subdevice,
            direction,
            id: String::from(id),
            name: String::from(name),
            info_flags,
            ops: Mutex::new(None),
            runtime: Mutex::new(Runtime::new()),
            caps: Mutex::new(PcmCaps::default_hardware()),
            wait: WaitQueue::new(),
            opened: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            avail: AtomicU64::new(0),
            avail_min: AtomicU64::new(1),
            state: AtomicI32::new(uapi::SNDRV_PCM_STATE_OPEN),
            isa_dma: AtomicBool::new(false),
            forward_wake: SpinLock::new(None),
            self_reference: SpinLock::new(weak.clone()),
            siblings: Mutex::new(Vec::new()),
            group: Mutex::new(Vec::new()),
        })
    }

    /// Force the ring buffer into ISA-DMA-safe memory: physically contiguous,
    /// below 16 MiB and aligned so a 16-bit channel cannot cross a 128 KiB
    /// boundary.  Called by ISA drivers from `open`.
    pub fn set_isa_dma(&self) {
        self.isa_dma.store(true, Ordering::Release);
    }

    /// Strong handle used by drivers that need to schedule a period callback
    /// from `trigger`.
    pub fn arc(&self) -> Option<Arc<Substream>> {
        self.self_reference.lock().upgrade()
    }

    pub fn node_name(&self) -> String {
        alloc::format!(
            "pcmC{}D{}{}",
            self.card,
            self.device,
            self.direction.suffix()
        )
    }

    pub fn set_ops(&self, ops: Arc<dyn PcmOps>) {
        *self.ops.lock() = Some(ops);
    }

    pub fn info(&self) -> uapi::snd_pcm_info {
        let mut info = uapi::snd_pcm_info::zeroed();
        info.device = self.device;
        info.subdevice = self.subdevice;
        info.stream = self.direction.stream();
        info.card = self.card as i32;
        set_string(&mut info.id, &self.id);
        set_string(&mut info.name, &self.name);
        set_string(&mut info.subname, "subdevice #0");
        info.dev_class = uapi::SNDRV_PCM_CLASS_GENERIC;
        info.dev_subclass = uapi::SNDRV_PCM_SUBCLASS_GENERIC_MIX;
        let siblings = self.siblings.lock();
        info.subdevices_count = siblings.len().max(1) as u32;
        info.subdevices_avail = siblings
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|substream| !substream.opened.load(Ordering::Acquire))
            .count() as u32;
        info
    }

    pub fn buffer_pointer(&self) -> Option<*mut u8> {
        self.runtime
            .lock()
            .pages
            .as_ref()
            .map(|pages| pages.ring.pointer())
    }

    pub fn dma_address(&self) -> u64 {
        self.runtime
            .lock()
            .pages
            .as_ref()
            .map_or(0, |pages| pages.ring.dma_address())
    }

    pub fn period_size(&self) -> u64 {
        self.runtime.lock().hw.map_or(0, |hw| hw.period_size)
    }

    pub fn buffer_size(&self) -> u64 {
        self.runtime.lock().hw.map_or(0, |hw| hw.buffer_size)
    }

    pub fn period_bytes(&self) -> u64 {
        self.runtime.lock().hw.map_or(0, |hw| hw.period_bytes)
    }

    pub fn buffer_bytes(&self) -> u64 {
        self.runtime.lock().hw.map_or(0, |hw| hw.buffer_bytes)
    }

    pub fn rate(&self) -> u32 {
        self.runtime.lock().hw.map_or(0, |hw| hw.rate)
    }

    pub fn channels(&self) -> u32 {
        self.runtime.lock().hw.map_or(0, |hw| hw.channels)
    }

    pub fn format(&self) -> u32 {
        self.runtime.lock().hw.map_or(0, |hw| hw.format)
    }

    /// Stop a running stream from driver context and publish the error state.
    pub fn stop(&self, state: i32) {
        let mut runtime = self.runtime.lock();
        runtime.state = state;
        publish(self, &mut runtime);
        drop(runtime);
        wake(self);
        if let Some(ops) = self.ops.lock().clone() {
            let _ = ops.trigger(self, uapi::SNDRV_PCM_TRIGGER_STOP);
        }
    }
}

pub(crate) fn now() -> uapi::timespec {
    uapi::timespec::from_micros(time::uptime_micros())
}

pub(crate) fn avail_locked(runtime: &Runtime, direction: Direction) -> u64 {
    let Some(hw) = runtime.hw else {
        return 0;
    };
    if runtime.boundary == 0 {
        return 0;
    }
    // Playback counts frames written but not consumed: appl - hw.  Capture
    // counts frames produced but not read: hw - appl.
    match direction {
        Direction::Playback => {
            let queued = (runtime.appl_ptr + runtime.boundary - runtime.hw_ptr) % runtime.boundary;
            hw.buffer_size.saturating_sub(queued.min(hw.buffer_size))
        }
        Direction::Capture => {
            let produced =
                (runtime.hw_ptr + runtime.boundary - runtime.appl_ptr) % runtime.boundary;
            produced.min(hw.buffer_size)
        }
    }
}

fn sync_pages(runtime: &mut Runtime) {
    if let Some(pages) = &runtime.pages {
        let page = &pages.status;
        let status = uapi::snd_pcm_mmap_status {
            state: runtime.state,
            pad1: 0,
            hw_ptr: runtime.hw_ptr,
            tstamp: clock_from_uptime(timespec_micros(runtime.tstamp), runtime.tstamp_type),
            suspended_state: runtime.suspended_state,
            pad2: 0,
            audio_tstamp: clock_from_uptime(timespec_micros(runtime.tstamp), runtime.tstamp_type),
        };
        // SAFETY: the coherent page is owned by this substream and is exactly
        // one page; the status structure lives in its first 56 bytes.
        unsafe { (page.as_ptr::<uapi::snd_pcm_mmap_status>()).write_volatile(status) };
    }
    if let Some(pages) = &runtime.pages {
        let page = &pages.control;
        let control = uapi::snd_pcm_mmap_control {
            appl_ptr: runtime.appl_ptr,
            avail_min: runtime.sw.avail_min,
        };
        // SAFETY: as above; the control structure lives in the first 16 bytes.
        unsafe { (page.as_ptr::<uapi::snd_pcm_mmap_control>()).write_volatile(control) };
    }
}

fn refresh_control(runtime: &mut Runtime) {
    if let Some(pages) = &runtime.pages {
        let page = &pages.control;
        // SAFETY: the page is owned by this substream; userspace may update the
        // control area concurrently, so the read is volatile.
        let control = unsafe { (page.as_ptr::<uapi::snd_pcm_mmap_control>()).read_volatile() };
        if runtime.boundary != 0 && control.appl_ptr < runtime.boundary {
            runtime.appl_ptr = control.appl_ptr;
        }
        if let Some(hw) = runtime.hw
            && control.avail_min <= hw.buffer_size
        {
            runtime.sw.avail_min = control.avail_min;
        }
    }
}

pub(crate) fn publish(substream: &Substream, runtime: &mut Runtime) {
    let avail = avail_locked(runtime, substream.direction);
    runtime.avail_max = runtime.avail_max.max(avail);
    let min_thresh = runtime
        .hw
        .map_or(1, |hw| runtime.sw.avail_min.min(hw.buffer_size).max(1));
    substream.avail.store(avail, Ordering::Release);
    substream.avail_min.store(min_thresh, Ordering::Release);
    substream.state.store(runtime.state, Ordering::Release);
    substream.generation.fetch_add(1, Ordering::AcqRel);
    sync_pages(runtime);
}

pub(crate) fn wake(substream: &Substream) {
    wake_inner(substream, true);
}

fn wake_inner(substream: &Substream, notify_forward: bool) {
    pollwait::wake_object(substream as *const Substream);
    substream.wait.wake_all();
    if notify_forward {
        let forward = substream.forward_wake.lock().clone();
        if let Some((queue, counter)) = forward {
            if let Some(seq) = counter {
                seq.fetch_add(1, Ordering::AcqRel);
            }
            queue.wake_all();
        }
    }
}

/// Re-read the hardware pointer and publish a new state.  Driver period
/// interrupts call this; `status`, `hwsync`, `delay` and blocking reads call it
/// before sampling the runtime.
pub fn period_elapsed(substream: &Substream) {
    period_elapsed_inner(substream, true, false);
}

/// Refresh hardware pointer from inside an owning software mixer without
/// re-notifying the mixer's own `forward_wake` counter on no-op polls.
pub(crate) fn period_elapsed_direct(substream: &Substream) {
    period_elapsed_inner(substream, false, false);
}

/// Completion notification used by hardware drivers after a period IRQ.
pub fn snd_pcm_period_elapsed(substream: &Substream) {
    period_elapsed_inner(substream, true, true);
}

fn period_elapsed_inner(substream: &Substream, notify_forward: bool, in_interrupt: bool) {
    let Some(ops) = substream.ops.lock().clone() else {
        return;
    };
    let mut stop = false;
    let mut changed = false;
    {
        // Hold `runtime` across `ops.pointer()` so concurrent `STATUS`/`DELAY`
        // callers and IRQ work cannot interleave an older `raw` sample after a
        // newer one and fabricate a nearly full-buffer wrap jump.
        let mut runtime = substream.runtime.lock();
        if runtime.state != uapi::SNDRV_PCM_STATE_RUNNING
            && runtime.state != uapi::SNDRV_PCM_STATE_DRAINING
        {
            return;
        }
        let prev_state = runtime.state;
        let Some(hw) = runtime.hw else {
            return;
        };
        let boundary = runtime.boundary;
        let buffer_size = hw.buffer_size;
        if boundary == 0 || buffer_size == 0 {
            return;
        }

        let raw = ops.pointer(substream) % buffer_size;
        let raw_delta = (raw + buffer_size - runtime.raw_hw_ptr) % buffer_size;
        let now_us = time::uptime_micros();
        let buffer_us = (buffer_size as u128 * 1_000_000 / u128::from(hw.rate.max(1))) as u64;
        let dt_us = if runtime.last_elapsed_us == 0 {
            0
        } else {
            now_us.saturating_sub(runtime.last_elapsed_us)
        };

        // Guard against tiny hardware/emulation LPIB read-back jitter going
        // slightly backwards without an actual wrap-around (Linux snd_pcm_update_hw_ptr0).
        let backwards_jitter = raw < runtime.raw_hw_ptr
            && (runtime.raw_hw_ptr - raw) <= (hw.period_size / 2).max(16)
            && buffer_us > 0
            && dt_us < buffer_us / 2;

        // Follow Linux `snd_pcm_update_hw_ptr0`: never extrapolate synthetic
        // wall-clock XRUN laps during normal operation (`xrun_debug` off), since
        // virtualized HDA / scheduler stalls pause DMA rather than lapping silently.
        let delta = if backwards_jitter { 0 } else { raw_delta };

        if !backwards_jitter {
            runtime.raw_hw_ptr = raw;
            if delta > 0 {
                runtime.hw_ptr = (runtime.hw_ptr + delta) % boundary;
                runtime.last_elapsed_us = now_us;
                changed = true;
            }
        }
        runtime.tstamp = uapi::timespec::from_micros(now_us);
        let queued = match substream.direction {
            Direction::Playback => {
                (runtime.appl_ptr + runtime.boundary - runtime.hw_ptr) % runtime.boundary
            }
            Direction::Capture => {
                (runtime.hw_ptr + runtime.boundary - runtime.appl_ptr) % runtime.boundary
            }
        };
        if runtime.state == uapi::SNDRV_PCM_STATE_DRAINING
            && substream.direction == Direction::Playback
        {
            if queued == 0 || delta > buffer_size || queued > hw.buffer_size {
                runtime.hw_ptr = runtime.appl_ptr;
                runtime.state = uapi::SNDRV_PCM_STATE_SETUP;
                stop = true;
            }
        } else if delta > buffer_size || queued > hw.buffer_size {
            stop = true;
            runtime.state = uapi::SNDRV_PCM_STATE_XRUN;
            runtime.hw_ptr = match substream.direction {
                Direction::Playback => runtime.appl_ptr,
                Direction::Capture => (runtime.appl_ptr + hw.buffer_size) % runtime.boundary,
            };
        }
        if stop || runtime.state != prev_state {
            changed = true;
        }
        if changed || in_interrupt {
            publish(substream, &mut runtime);
        }
    }
    if changed || in_interrupt {
        wake_inner(substream, notify_forward);
    }
    if stop {
        let _ = ops.trigger(substream, uapi::SNDRV_PCM_TRIGGER_STOP);
    }
}

fn timespec_micros(value: uapi::timespec) -> u64 {
    (value.tv_sec.max(0) as u64)
        .saturating_mul(1_000_000)
        .saturating_add((value.tv_nsec.max(0) as u64) / 1_000)
}

fn clock_from_uptime(micros: u64, kind: i32) -> uapi::timespec {
    if kind == uapi::SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY {
        let now_uptime = time::uptime_micros();
        let now_realtime = time::realtime_micros();
        uapi::timespec::from_micros(now_realtime.saturating_sub(now_uptime.saturating_sub(micros)))
    } else {
        uapi::timespec::from_micros(micros)
    }
}

/// Build the status structure.  `config` is present for `STATUS_EXT`, whose
/// `audio_tstamp_data` is read/write; legacy `STATUS` leaves it zero.
fn fill_status(
    substream: &Substream,
    runtime: &Runtime,
    config: Option<u32>,
) -> uapi::snd_pcm_status {
    let mut status = uapi::snd_pcm_status::zeroed();
    status.state = runtime.state;
    let kind = runtime.tstamp_type;
    status.trigger_tstamp = clock_from_uptime(timespec_micros(runtime.trigger_tstamp), kind);
    status.tstamp = if runtime.tstamp_mode == uapi::SNDRV_PCM_TSTAMP_ENABLE {
        clock_from_uptime(timespec_micros(runtime.tstamp), kind)
    } else {
        uapi::timespec::zero()
    };
    status.appl_ptr = runtime.appl_ptr;
    status.hw_ptr = runtime.hw_ptr;
    let in_flight = if runtime.boundary == 0 {
        0
    } else {
        (runtime.appl_ptr + runtime.boundary - runtime.hw_ptr) % runtime.boundary
    };
    let in_flight = match substream.direction {
        Direction::Playback => in_flight.min(runtime.hw.map_or(0, |hw| hw.buffer_size)),
        Direction::Capture => ((runtime.hw_ptr + runtime.boundary - runtime.appl_ptr)
            % runtime.boundary.max(1))
        .min(runtime.hw.map_or(0, |hw| hw.buffer_size)),
    };
    status.delay = in_flight as i64;
    status.avail = avail_locked(runtime, substream.direction);
    status.avail_max = runtime.avail_max.max(status.avail);
    status.overrange = runtime.overrange;
    status.suspended_state = runtime.suspended_state;
    if runtime.tstamp_mode == uapi::SNDRV_PCM_TSTAMP_ENABLE {
        // Default DMA timestamp: the time at which the current hw_ptr was
        // reached, anchored at the trigger timestamp.
        let dma_micros = runtime.hw.map_or(0, |hw| {
            runtime.hw_ptr.saturating_mul(1_000_000) / u64::from(hw.rate.max(1))
        });
        status.audio_tstamp = clock_from_uptime(
            timespec_micros(runtime.trigger_tstamp).saturating_add(dma_micros),
            kind,
        );
        status.driver_tstamp = status.tstamp;
        status.audio_tstamp_data = if let Some(config) = config {
            let requested = config & 0x1F;
            let type_requested = (config & 0xF) as i32;
            let actual = match type_requested {
                uapi::SNDRV_PCM_AUDIO_TSTAMP_TYPE_COMPAT
                | uapi::SNDRV_PCM_AUDIO_TSTAMP_TYPE_DEFAULT => {
                    uapi::SNDRV_PCM_AUDIO_TSTAMP_TYPE_DEFAULT
                }
                other => other,
            };
            // valid | actual_type << 1 | accuracy_report << 5, packed at bit 16.
            requested | (1 << 16) | (((actual as u32) & 0xF) << 17)
        } else {
            0
        };
        status.audio_tstamp_accuracy = 0;
    }
    status
}

fn mask_from_formats(formats: u64) -> uapi::snd_mask {
    let mut mask = uapi::snd_mask::zeroed();
    for bit in 0..64 {
        if formats & (1u64 << bit) != 0 {
            mask.set(bit);
        }
    }
    mask
}

fn default_access_mask(info_flags: u32) -> uapi::snd_mask {
    let mut mask = uapi::snd_mask::zeroed();
    mask.set(uapi::SNDRV_PCM_ACCESS_MMAP_INTERLEAVED);
    mask.set(uapi::SNDRV_PCM_ACCESS_RW_INTERLEAVED);
    if info_flags & uapi::SNDRV_PCM_INFO_NONINTERLEAVED != 0 {
        mask.set(uapi::SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED);
        mask.set(uapi::SNDRV_PCM_ACCESS_RW_NONINTERLEAVED);
    }
    mask
}

fn sample_bits_range(caps: &PcmCaps) -> (u32, u32) {
    let mut min = 64;
    let mut max = 0;
    for format in 0..=uapi::SNDRV_PCM_FORMAT_LAST {
        if caps.formats & uapi::format_bit(format) != 0
            && let Some(bits) = uapi::format_bits(format)
        {
            min = min.min(bits);
            max = max.max(bits);
        }
    }
    (min.min(64), max)
}

/// Fill a `HW_REFINE` response with the driver capabilities, intersected with
/// the values the caller already fixed.
fn refine(substream: &Substream, params: &mut uapi::snd_pcm_hw_params) {
    let caps = *substream.caps.lock();
    let requested = params.rmask;

    let access = default_access_mask(substream.info_flags);
    let format = mask_from_formats(caps.formats);
    let mut subformat = uapi::snd_mask::zeroed();
    subformat.set(uapi::SNDRV_PCM_SUBFORMAT_STD);

    for (param, base) in [
        (uapi::SNDRV_PCM_HW_PARAM_ACCESS, access),
        (uapi::SNDRV_PCM_HW_PARAM_FORMAT, format),
        (uapi::SNDRV_PCM_HW_PARAM_SUBFORMAT, subformat),
    ] {
        if requested != 0 && requested & (1 << param) == 0 {
            continue;
        }
        let current = *params.mask(param);
        let mut result = base;
        if !current.is_empty() {
            result.intersect(&current);
        }
        params.set_mask(param, result);
    }

    let (sample_min, sample_max) = sample_bits_range(&caps);
    let frame_min = sample_min.saturating_mul(caps.channels_min);
    let frame_max = sample_max.saturating_mul(caps.channels_max);
    let frame_min_bytes = (frame_min / 8).max(1) as u64;
    let max_period = (caps.buffer_bytes_max as u64 / frame_min_bytes)
        .max(1)
        .min(u32::MAX as u64) as u32;
    let defaults: [(u32, uapi::snd_interval); 12] = [
        (
            uapi::SNDRV_PCM_HW_PARAM_SAMPLE_BITS,
            uapi::snd_interval::range(sample_min, sample_max),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_FRAME_BITS,
            uapi::snd_interval::range(frame_min, frame_max),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_CHANNELS,
            uapi::snd_interval::range(caps.channels_min, caps.channels_max),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_RATE,
            uapi::snd_interval::range(caps.rate_min, caps.rate_max),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_PERIOD_TIME,
            uapi::snd_interval::range(1, 1_000_000),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
            uapi::snd_interval::range(16, max_period),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
            uapi::snd_interval::range(1, caps.buffer_bytes_max as u32),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_PERIODS,
            uapi::snd_interval::range(caps.periods_min, caps.periods_max),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_BUFFER_TIME,
            uapi::snd_interval::range(1, 10_000_000),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
            uapi::snd_interval::range(16, u32::MAX / 2),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
            uapi::snd_interval::range(1, caps.buffer_bytes_max as u32),
        ),
        (
            uapi::SNDRV_PCM_HW_PARAM_TICK_TIME,
            uapi::snd_interval::range(0, 1_000_000),
        ),
    ];
    for (param, base) in defaults {
        if requested != 0 && requested & (1 << param) == 0 {
            continue;
        }
        let current = *params.interval(param);
        let mut result = base;
        // A zeroed interval is the normal HW_REFINE input and means "any".
        if !current.is_empty() && !(current.min == 0 && current.max == 0 && current.flags == 0) {
            result.intersect(&current);
        }
        params.set_interval(param, result);
    }
    params.info = substream.info_flags;
}

fn choose_mask_value(mask: &uapi::snd_mask, preferred: u32) -> Option<u32> {
    if mask.test(preferred) {
        return Some(preferred);
    }
    mask.single().or_else(|| mask.first())
}

fn solve(
    substream: &Substream,
    input: &uapi::snd_pcm_hw_params,
) -> Result<HardwareParams, VfsError> {
    let caps = *substream.caps.lock();
    // EXPORT_BUFFER is a userspace-plugin hint; the kernel only carries it.

    let access = choose_mask_value(
        input.mask(uapi::SNDRV_PCM_HW_PARAM_ACCESS),
        uapi::SNDRV_PCM_ACCESS_MMAP_INTERLEAVED,
    )
    .ok_or(VfsError::InvalidArgument)?;
    if access == uapi::SNDRV_PCM_ACCESS_MMAP_COMPLEX
        || access > uapi::SNDRV_PCM_ACCESS_RW_NONINTERLEAVED
    {
        return Err(VfsError::InvalidArgument);
    }
    if matches!(
        access,
        uapi::SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED | uapi::SNDRV_PCM_ACCESS_RW_NONINTERLEAVED
    ) && substream.info_flags & uapi::SNDRV_PCM_INFO_NONINTERLEAVED == 0
    {
        return Err(VfsError::InvalidArgument);
    }
    if !input
        .mask(uapi::SNDRV_PCM_HW_PARAM_SUBFORMAT)
        .test(uapi::SNDRV_PCM_SUBFORMAT_STD)
    {
        return Err(VfsError::InvalidArgument);
    }

    let format_mask = input.mask(uapi::SNDRV_PCM_HW_PARAM_FORMAT);
    let format = format_mask.single().ok_or(VfsError::InvalidArgument)?;
    if caps.formats & uapi::format_bit(format) == 0 {
        return Err(VfsError::InvalidArgument);
    }
    let sample_bits = uapi::format_bits(format).ok_or(VfsError::InvalidArgument)?;
    let sample_bytes = uapi::format_bytes(format).ok_or(VfsError::InvalidArgument)?;

    let channels = input
        .interval(uapi::SNDRV_PCM_HW_PARAM_CHANNELS)
        .exact_value()
        .ok_or(VfsError::InvalidArgument)?;
    if channels < caps.channels_min || channels > caps.channels_max {
        return Err(VfsError::InvalidArgument);
    }
    let rate_interval = *input.interval(uapi::SNDRV_PCM_HW_PARAM_RATE);
    let rate = rate_interval
        .exact_value()
        .unwrap_or_else(|| rate_interval.max.min(caps.rate_max));
    if rate < caps.rate_min || rate > caps.rate_max {
        return Err(VfsError::InvalidArgument);
    }

    let frame_bytes = sample_bytes
        .checked_mul(channels as usize)
        .ok_or(VfsError::InvalidArgument)?;
    let frame_bits = (frame_bytes * 8) as u32;

    let period_interval = *input.interval(uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE);
    let period_time = *input.interval(uapi::SNDRV_PCM_HW_PARAM_PERIOD_TIME);
    let buffer_interval = *input.interval(uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE);
    let buffer_time = *input.interval(uapi::SNDRV_PCM_HW_PARAM_BUFFER_TIME);
    let periods_interval = *input.interval(uapi::SNDRV_PCM_HW_PARAM_PERIODS);

    let mut period_size = period_interval.exact_value();
    if period_size.is_none() {
        if let Some(micros) = period_time.exact_value() {
            period_size = Some(((micros as u64 * rate as u64) / 1_000_000).max(1) as u32);
        }
    }
    let mut periods = periods_interval.exact_value().unwrap_or(4).max(1);
    let mut buffer_size = buffer_interval.exact_value();
    if buffer_size.is_none() {
        if let Some(micros) = buffer_time.exact_value() {
            buffer_size = Some(((micros as u64 * rate as u64) / 1_000_000).max(1) as u32);
        }
    }
    let mut period_size = period_size.unwrap_or(1024).max(1);
    if caps.period_step_bytes > 1 {
        let frames_step = (caps.period_step_bytes).div_ceil(frame_bytes).max(1) as u32;
        period_size = period_size.div_ceil(frames_step) * frames_step;
    }
    if buffer_size.is_none() {
        buffer_size = Some(period_size.saturating_mul(periods));
    }
    let mut buffer_size = buffer_size.unwrap().max(1).max(period_size);
    if buffer_size % period_size != 0 {
        periods = buffer_size.div_ceil(period_size);
        buffer_size = period_size.saturating_mul(periods);
    } else {
        periods = (buffer_size / period_size).max(1);
    }
    if periods < caps.periods_min || periods > caps.periods_max {
        return Err(VfsError::InvalidArgument);
    }

    let period_bytes = (period_size as u64)
        .checked_mul(frame_bytes as u64)
        .ok_or(VfsError::InvalidArgument)?;
    let buffer_bytes = (buffer_size as u64)
        .checked_mul(frame_bytes as u64)
        .ok_or(VfsError::InvalidArgument)?;
    if buffer_bytes as usize > caps.buffer_bytes_max {
        return Err(VfsError::InvalidArgument);
    }

    Ok(HardwareParams {
        access,
        format,
        channels,
        rate,
        sample_bits,
        frame_bits,
        frame_bytes,
        period_size: period_size as u64,
        period_bytes,
        periods,
        buffer_size: buffer_size as u64,
        buffer_bytes,
        noninterleaved: matches!(
            access,
            uapi::SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED | uapi::SNDRV_PCM_ACCESS_RW_NONINTERLEAVED
        ),
    })
}

fn write_hw_params(params: &mut uapi::snd_pcm_hw_params, hw: &HardwareParams, info_flags: u32) {
    let mut access = uapi::snd_mask::zeroed();
    access.set(hw.access);
    let mut format = uapi::snd_mask::zeroed();
    format.set(hw.format);
    let mut subformat = uapi::snd_mask::zeroed();
    subformat.set(uapi::SNDRV_PCM_SUBFORMAT_STD);
    params.set_mask(uapi::SNDRV_PCM_HW_PARAM_ACCESS, access);
    params.set_mask(uapi::SNDRV_PCM_HW_PARAM_FORMAT, format);
    params.set_mask(uapi::SNDRV_PCM_HW_PARAM_SUBFORMAT, subformat);
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_SAMPLE_BITS,
        uapi::snd_interval::exact(hw.sample_bits),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_FRAME_BITS,
        uapi::snd_interval::exact(hw.frame_bits),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_CHANNELS,
        uapi::snd_interval::exact(hw.channels),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_RATE,
        uapi::snd_interval::exact(hw.rate),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE,
        uapi::snd_interval::exact(hw.period_size as u32),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
        uapi::snd_interval::exact(hw.period_bytes as u32),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_PERIODS,
        uapi::snd_interval::exact(hw.periods),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE,
        uapi::snd_interval::exact(hw.buffer_size as u32),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
        uapi::snd_interval::exact(hw.buffer_bytes as u32),
    );
    let period_time = (hw.period_size as u64 * 1_000_000) / hw.rate as u64;
    let buffer_time = (hw.buffer_size as u64 * 1_000_000) / hw.rate as u64;
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_PERIOD_TIME,
        uapi::snd_interval::exact(period_time.min(u32::MAX as u64) as u32),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_BUFFER_TIME,
        uapi::snd_interval::exact(buffer_time.min(u32::MAX as u64) as u32),
    );
    params.set_interval(
        uapi::SNDRV_PCM_HW_PARAM_TICK_TIME,
        uapi::snd_interval::range(0, 0),
    );
    params.cmask = (1 << 20) - 1;
    params.info = info_flags;
    params.msbits = hw.sample_bits;
    params.rate_num = hw.rate;
    params.rate_den = 1;
    params.fifo_size = 0;
}

/// Compute Linux ALSA default ring boundary (largest power-of-two multiple of `buffer_size` <= INT_MAX).
pub(crate) fn default_boundary(buffer_size: u64) -> u64 {
    let mut boundary = buffer_size.max(1);
    while boundary.saturating_mul(2) <= 0x7fff_ffffu64.saturating_sub(buffer_size) {
        boundary *= 2;
    }
    boundary
}

fn default_sw_params(hw: &HardwareParams, direction: Direction) -> uapi::snd_pcm_sw_params {
    let mut sw = uapi::snd_pcm_sw_params::zeroed();
    sw.proto = uapi::SNDRV_PCM_VERSION;
    sw.period_step = 1;
    sw.avail_min = hw.period_size;
    sw.start_threshold = match direction {
        Direction::Playback => hw.buffer_size,
        Direction::Capture => 1,
    };
    sw.stop_threshold = hw.buffer_size;
    sw.boundary = default_boundary(hw.buffer_size);
    sw
}

fn configure(
    substream: &Arc<Substream>,
    mut params: uapi::snd_pcm_hw_params,
    arg: u64,
) -> Result<(), VfsError> {
    apply_hw_params(substream, &mut params)?;
    uaccess::write_value(arg, &params)
}

pub(crate) fn apply_hw_params(
    substream: &Arc<Substream>,
    params: &mut uapi::snd_pcm_hw_params,
) -> Result<(), VfsError> {
    let hw = solve(substream, &params)?;
    {
        let runtime = substream.runtime.lock();
        if runtime.state != uapi::SNDRV_PCM_STATE_OPEN
            && runtime.state != uapi::SNDRV_PCM_STATE_SETUP
        {
            return Err(VfsError::Busy);
        }
    }
    let mapped_bytes = (hw.buffer_bytes as usize)
        .checked_add(PAGE_SIZE - 1)
        .ok_or(VfsError::NoMemory)?
        / PAGE_SIZE
        * PAGE_SIZE;
    let isa = substream.isa_dma.load(Ordering::Acquire);
    if isa && mapped_bytes > 64 * 1024 {
        return Err(VfsError::InvalidArgument);
    }
    let buffer = if isa {
        dma::alloc_coherent_below(mapped_bytes, 64 * 1024, 16 * 1024 * 1024)
            .ok_or(VfsError::NoMemory)?
    } else {
        dma::alloc_coherent(mapped_bytes, PAGE_SIZE).ok_or(VfsError::NoMemory)?
    };
    let status_page = dma::alloc_coherent(PAGE_SIZE, PAGE_SIZE).ok_or(VfsError::NoMemory)?;
    let control_page = dma::alloc_coherent(PAGE_SIZE, PAGE_SIZE).ok_or(VfsError::NoMemory)?;
    let ring = RingBuffer {
        coherent: buffer,
        bytes: hw.buffer_bytes as usize,
        mapped_bytes,
        frames: hw.buffer_size,
        frame_bytes: hw.frame_bytes,
    };
    let pages = Arc::new(BufferSet {
        ring,
        status: status_page,
        control: control_page,
    });
    {
        let mut runtime = substream.runtime.lock();
        runtime.hw = Some(hw);
        runtime.pages = Some(pages);
        runtime.sw = default_sw_params(&hw, substream.direction);
        runtime.hw_ptr = 0;
        runtime.raw_hw_ptr = 0;
        runtime.appl_ptr = 0;
        runtime.boundary = runtime.sw.boundary.max(hw.buffer_size + 1);
        runtime.sw.boundary = runtime.boundary;
        runtime.state = uapi::SNDRV_PCM_STATE_SETUP;
        runtime.trigger_tstamp = now();
        runtime.tstamp = now();
        runtime.last_elapsed_us = 0;
        runtime.avail_max = 0;
        runtime.no_period_wakeup = params.flags & uapi::SNDRV_PCM_HW_PARAMS_NO_PERIOD_WAKEUP != 0;
        publish(substream, &mut runtime);
    }
    write_hw_params(params, &hw, substream.info_flags);
    if let Some(ops) = substream.ops.lock().clone()
        && let Err(error) = ops.hw_params(substream, params)
    {
        let mut runtime = substream.runtime.lock();
        runtime.hw = None;
        runtime.pages = None;
        runtime.state = uapi::SNDRV_PCM_STATE_OPEN;
        publish(substream, &mut runtime);
        return Err(from_errno(error));
    }
    Ok(())
}

pub(crate) fn release_buffer(substream: &Arc<Substream>) {
    let mut runtime = substream.runtime.lock();
    runtime.hw = None;
    runtime.pages = None;
    runtime.boundary = 0;
    runtime.hw_ptr = 0;
    runtime.raw_hw_ptr = 0;
    runtime.appl_ptr = 0;
    runtime.last_elapsed_us = 0;
    runtime.state = uapi::SNDRV_PCM_STATE_OPEN;
    publish(substream, &mut runtime);
}

pub(crate) fn prepare_one(substream: &Arc<Substream>) -> Result<(), VfsError> {
    {
        let mut runtime = substream.runtime.lock();
        if runtime.hw.is_none() {
            return Err(VfsError::InvalidArgument);
        }
        if let Some(pages) = &runtime.pages {
            pages.ring.zero();
        }
        runtime.hw_ptr = 0;
        runtime.raw_hw_ptr = 0;
        runtime.appl_ptr = 0;
        runtime.avail_max = 0;
        runtime.overrange = 0;
        runtime.last_elapsed_us = 0;
        runtime.state = uapi::SNDRV_PCM_STATE_PREPARED;
        runtime.trigger_tstamp = now();
        runtime.tstamp = now();
        publish(substream, &mut runtime);
    }
    if let Some(ops) = substream.ops.lock().clone() {
        ops.prepare(substream);
    }
    Ok(())
}

pub(crate) fn trigger_one(substream: &Arc<Substream>, cmd: i32) -> Result<(), VfsError> {
    let (new_state, allowed): (i32, &'static [i32]) = match cmd {
        uapi::SNDRV_PCM_TRIGGER_START => (
            uapi::SNDRV_PCM_STATE_RUNNING,
            &[uapi::SNDRV_PCM_STATE_PREPARED, uapi::SNDRV_PCM_STATE_PAUSED],
        ),
        uapi::SNDRV_PCM_TRIGGER_STOP => (
            uapi::SNDRV_PCM_STATE_SETUP,
            &[
                uapi::SNDRV_PCM_STATE_PREPARED,
                uapi::SNDRV_PCM_STATE_RUNNING,
                uapi::SNDRV_PCM_STATE_PAUSED,
                uapi::SNDRV_PCM_STATE_DRAINING,
                uapi::SNDRV_PCM_STATE_XRUN,
            ],
        ),
        uapi::SNDRV_PCM_TRIGGER_PAUSE_PUSH => (
            uapi::SNDRV_PCM_STATE_PAUSED,
            &[uapi::SNDRV_PCM_STATE_RUNNING],
        ),
        uapi::SNDRV_PCM_TRIGGER_PAUSE_RELEASE => (
            uapi::SNDRV_PCM_STATE_RUNNING,
            &[uapi::SNDRV_PCM_STATE_PAUSED],
        ),
        uapi::SNDRV_PCM_TRIGGER_SUSPEND => (
            uapi::SNDRV_PCM_STATE_SUSPENDED,
            &[uapi::SNDRV_PCM_STATE_RUNNING, uapi::SNDRV_PCM_STATE_PAUSED],
        ),
        uapi::SNDRV_PCM_TRIGGER_RESUME => (
            uapi::SNDRV_PCM_STATE_RUNNING,
            &[uapi::SNDRV_PCM_STATE_SUSPENDED],
        ),
        _ => return Err(VfsError::InvalidArgument),
    };
    {
        let mut runtime = substream.runtime.lock();
        if runtime.hw.is_none() {
            return Err(VfsError::InvalidArgument);
        }
        if runtime.state == new_state {
            return Ok(());
        }
        if !allowed.contains(&runtime.state) {
            return Err(VfsError::InvalidArgument);
        }
        if runtime.state == uapi::SNDRV_PCM_STATE_SUSPENDED {
            runtime.suspended_state = runtime.state;
        }
        runtime.state = new_state;
        let now_us = time::uptime_micros();
        runtime.trigger_tstamp = uapi::timespec::from_micros(now_us);
        runtime.tstamp = runtime.trigger_tstamp;
        if new_state == uapi::SNDRV_PCM_STATE_RUNNING {
            runtime.last_elapsed_us = now_us;
        } else if new_state == uapi::SNDRV_PCM_STATE_SETUP {
            runtime.last_elapsed_us = 0;
        }
        publish(substream, &mut runtime);
    }
    if let Some(ops) = substream.ops.lock().clone()
        && let Err(error) = ops.trigger(substream, cmd)
    {
        let mut runtime = substream.runtime.lock();
        runtime.state = uapi::SNDRV_PCM_STATE_SETUP;
        runtime.last_elapsed_us = 0;
        publish(substream, &mut runtime);
        return Err(from_errno(error));
    }
    wake(substream);
    Ok(())
}

fn prepare(substream: &Arc<Substream>) -> Result<(), VfsError> {
    for member in group_members(substream) {
        prepare_one(&member)?;
    }
    Ok(())
}

fn group_members(substream: &Arc<Substream>) -> Vec<Arc<Substream>> {
    let group = substream.group.lock();
    if group.is_empty() {
        return alloc::vec![substream.clone()];
    }
    let members: Vec<_> = group.iter().filter_map(Weak::upgrade).collect();
    if members.is_empty() {
        alloc::vec![substream.clone()]
    } else {
        members
    }
}

pub(crate) fn trigger(substream: &Arc<Substream>, cmd: i32) -> Result<(), VfsError> {
    for member in group_members(substream) {
        trigger_one(&member, cmd)?;
    }
    Ok(())
}

fn link(substream: &Arc<Substream>, fd: i32) -> Result<(), VfsError> {
    if fd < 0 {
        return Err(VfsError::InvalidArgument);
    }
    let entry = crate::vfs::current_fd_table()
        .lock()
        .get(fd as usize)
        .ok_or(VfsError::BadFileDescriptor)?;
    let target = entry
        .file
        .inode
        .sound_substream()
        .ok_or(VfsError::InvalidArgument)?;
    if Arc::ptr_eq(substream, &target) {
        return Err(VfsError::InvalidArgument);
    }
    {
        let own = substream.group.lock();
        let other = target.group.lock();
        if !own.is_empty() || !other.is_empty() {
            return Err(VfsError::Busy);
        }
    }
    {
        let mut own = substream.group.lock();
        own.push(Arc::downgrade(substream));
        own.push(Arc::downgrade(&target));
    }
    {
        let mut other = target.group.lock();
        other.push(Arc::downgrade(substream));
        other.push(Arc::downgrade(&target));
    }
    Ok(())
}

fn unlink(substream: &Arc<Substream>) -> Result<(), VfsError> {
    let members = group_members(substream);
    for member in &members {
        member.group.lock().clear();
    }
    substream.group.lock().clear();
    Ok(())
}

fn reset(substream: &Arc<Substream>) -> Result<(), VfsError> {
    for member in group_members(substream) {
        reset_one(&member)?;
    }
    Ok(())
}

pub(crate) fn reset_one(substream: &Arc<Substream>) -> Result<(), VfsError> {
    {
        let runtime = substream.runtime.lock();
        if runtime.hw.is_none() {
            return Err(VfsError::InvalidArgument);
        }
    }
    if substream.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_RUNNING {
        let _ = trigger(substream, uapi::SNDRV_PCM_TRIGGER_STOP);
    }
    let mut runtime = substream.runtime.lock();
    runtime.hw_ptr = 0;
    runtime.raw_hw_ptr = 0;
    runtime.appl_ptr = 0;
    runtime.avail_max = 0;
    runtime.last_elapsed_us = 0;
    runtime.state = uapi::SNDRV_PCM_STATE_SETUP;
    publish(substream, &mut runtime);
    drop(runtime);
    wake(substream);
    Ok(())
}

pub(crate) fn start_if_threshold(substream: &Arc<Substream>) {
    let (state, queued, threshold) = {
        let runtime = substream.runtime.lock();
        let (queued, limit) = match runtime.hw {
            Some(hw) if runtime.boundary != 0 => {
                let raw_queued =
                    (runtime.appl_ptr + runtime.boundary - runtime.hw_ptr) % runtime.boundary;
                (raw_queued.min(hw.buffer_size), hw.buffer_size)
            }
            _ => (0, 0),
        };
        let threshold = if limit != 0 {
            runtime.sw.start_threshold.min(limit)
        } else {
            runtime.sw.start_threshold
        };
        (runtime.state, queued, threshold)
    };
    if state == uapi::SNDRV_PCM_STATE_PREPARED
        && threshold != 0
        && queued >= threshold
        && substream.direction == Direction::Playback
    {
        let _ = trigger(substream, uapi::SNDRV_PCM_TRIGGER_START);
    }
}

fn partial_or_err(err: VfsError, done: u64) -> Result<u64, VfsError> {
    // Linux `snd_pcm_lib_write1()`/`read1()` report a short transfer instead
    // of the error once any frame moved (`xfer > 0`), including on a signal.
    if matches!(
        err,
        VfsError::WouldBlock | VfsError::BrokenPipe | VfsError::Interrupted
    ) && done != 0
    {
        Ok(done)
    } else {
        Err(err)
    }
}

enum IoScratch {
    Stack([u8; 4096]),
    Heap(Vec<u8>),
}

impl IoScratch {
    fn with_capacity(bytes: usize) -> Self {
        if bytes <= 4096 {
            Self::Stack([0u8; 4096])
        } else {
            Self::Heap(vec![0u8; bytes])
        }
    }

    fn as_mut_slice(&mut self, len: usize) -> &mut [u8] {
        match self {
            Self::Stack(buf) => &mut buf[..len],
            Self::Heap(buf) => &mut buf[..len],
        }
    }
}

fn wait_for_io(substream: &Substream, nonblock: bool, remaining: u64) -> Result<(), VfsError> {
    loop {
        let state = substream.state.load(Ordering::Acquire);
        if state == uapi::SNDRV_PCM_STATE_XRUN {
            return Err(VfsError::BrokenPipe);
        }
        if substream.avail.load(Ordering::Acquire) != 0 {
            return Ok(());
        }
        if state == uapi::SNDRV_PCM_STATE_SETUP || state == uapi::SNDRV_PCM_STATE_OPEN {
            return Err(VfsError::InvalidArgument);
        }
        if nonblock {
            return Err(VfsError::WouldBlock);
        }
        // Linux `wait_for_avail()`: sleep until `twake = min(avail_min, size)`
        // frames are available, the stream leaves a runnable state, or a
        // signal is pending (-ERESTARTSYS there, EINTR here). `PollWait` is
        // used instead of `substream.wait` because signal delivery kicks every
        // poll waiter, so SIGTERM/SIGKILL/SIGUSR1 end a blocked write promptly.
        let target = substream
            .avail_min
            .load(Ordering::Acquire)
            .min(remaining)
            .max(1);
        let mut wait = pollwait::PollWait::new();
        loop {
            wait.arm_object(substream as *const Substream);
            if substream.avail.load(Ordering::Acquire) >= target
                || !matches!(
                    substream.state.load(Ordering::Acquire),
                    uapi::SNDRV_PCM_STATE_RUNNING
                        | uapi::SNDRV_PCM_STATE_PREPARED
                        | uapi::SNDRV_PCM_STATE_PAUSED
                )
            {
                break;
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    }
}

fn write_frames(
    substream: &Arc<Substream>,
    buffer: u64,
    frames: u64,
    nonblock: bool,
) -> Result<u64, VfsError> {
    let (frame_bytes, ring_frames) = {
        let runtime = substream.runtime.lock();
        let hw = runtime.hw.ok_or(VfsError::InvalidArgument)?;
        (hw.frame_bytes, hw.buffer_size)
    };
    if frames == 0 {
        return Ok(0);
    }
    let max_chunk = frames
        .min((MAX_RW_BYTES / frame_bytes.max(1)) as u64)
        .min(ring_frames)
        .max(1) as usize;
    let mut scratch = IoScratch::with_capacity(max_chunk * frame_bytes.max(1));
    let mut done = 0u64;
    while done < frames {
        let remaining = frames - done;
        // Wait for space using only atomics; the runtime mutex is not valid in a
        // wait predicate.
        if let Err(err) = wait_for_io(substream, nonblock, remaining) {
            return partial_or_err(err, done);
        }

        let avail_hint = substream.avail.load(Ordering::Acquire).max(1);
        let chunk = remaining.min(avail_hint).min(max_chunk as u64).max(1);
        let temporary = scratch.as_mut_slice(chunk as usize * frame_bytes);
        let user = buffer
            .checked_add(done.saturating_mul(frame_bytes as u64))
            .ok_or(VfsError::Fault)?;
        uaccess::read(user, temporary)?;

        let mut runtime = substream.runtime.lock();
        refresh_control(&mut runtime);
        let avail = avail_locked(&runtime, Direction::Playback);
        if avail == 0 {
            drop(runtime);
            continue;
        }
        let take = avail.min(chunk) as usize;
        let _ = runtime.hw.ok_or(VfsError::InvalidArgument)?;
        let appl = runtime.appl_ptr;
        let pages = runtime.pages.clone().ok_or(VfsError::InvalidArgument)?;
        let ring = &pages.ring;
        let start = (appl % ring.frames) as usize * ring.frame_bytes;
        let bytes = take * ring.frame_bytes;
        let first = bytes.min(ring.bytes - start);
        // SAFETY: `start` is inside the ring and `first` stays within it; the
        // source is a temporary kernel buffer of at least `bytes` bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(temporary.as_ptr(), ring.pointer().add(start), first);
            if bytes > first {
                core::ptr::copy_nonoverlapping(
                    temporary.as_ptr().add(first),
                    ring.pointer(),
                    bytes - first,
                );
            }
        }
        runtime.appl_ptr = (runtime.appl_ptr + take as u64) % runtime.boundary;
        runtime.tstamp = now();
        publish(substream, &mut runtime);
        drop(runtime);
        wake(substream);
        done += take as u64;
        start_if_threshold(substream);
    }
    Ok(done)
}

fn read_frames(
    substream: &Arc<Substream>,
    buffer: u64,
    frames: u64,
    nonblock: bool,
) -> Result<u64, VfsError> {
    let (frame_bytes, ring_frames) = {
        let runtime = substream.runtime.lock();
        let hw = runtime.hw.ok_or(VfsError::InvalidArgument)?;
        (hw.frame_bytes, hw.buffer_size)
    };
    if frames == 0 {
        return Ok(0);
    }
    if substream.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_PREPARED {
        let _ = trigger(substream, uapi::SNDRV_PCM_TRIGGER_START);
    }
    let max_chunk = frames
        .min((MAX_RW_BYTES / frame_bytes.max(1)) as u64)
        .min(ring_frames)
        .max(1) as usize;
    let mut scratch = IoScratch::with_capacity(max_chunk * frame_bytes.max(1));
    let mut done = 0u64;
    while done < frames {
        period_elapsed(substream);
        let remaining = frames - done;
        if let Err(err) = wait_for_io(substream, nonblock, remaining) {
            return partial_or_err(err, done);
        }

        let avail_hint = substream.avail.load(Ordering::Acquire).max(1);
        let chunk = remaining.min(avail_hint).min(max_chunk as u64).max(1);
        let temporary = scratch.as_mut_slice(chunk as usize * frame_bytes);
        let mut runtime = substream.runtime.lock();
        refresh_control(&mut runtime);
        let avail = avail_locked(&runtime, Direction::Capture);
        if avail == 0 {
            drop(runtime);
            continue;
        }
        let take = avail.min(chunk) as usize;
        let pages = runtime.pages.clone().ok_or(VfsError::InvalidArgument)?;
        let ring = &pages.ring;
        let start = (runtime.appl_ptr % ring.frames) as usize * ring.frame_bytes;
        let bytes = take * ring.frame_bytes;
        let first = bytes.min(ring.bytes - start);
        // SAFETY: the source range is inside the ring buffer, as in the write path.
        unsafe {
            core::ptr::copy_nonoverlapping(
                ring.pointer().add(start),
                temporary.as_mut_ptr(),
                first,
            );
            if bytes > first {
                core::ptr::copy_nonoverlapping(
                    ring.pointer(),
                    temporary.as_mut_ptr().add(first),
                    bytes - first,
                );
            }
        }
        runtime.appl_ptr = (runtime.appl_ptr + take as u64) % runtime.boundary;
        runtime.tstamp = now();
        publish(substream, &mut runtime);
        drop(runtime);
        wake(substream);
        let user = buffer
            .checked_add(done.saturating_mul(frame_bytes as u64))
            .ok_or(VfsError::Fault)?;
        uaccess::write(user, &temporary[..bytes])?;
        done += take as u64;
    }
    Ok(done)
}

fn channel_pointers(bufs: u64, channels: usize) -> Result<Vec<u64>, VfsError> {
    if bufs == 0 || channels == 0 || channels > 32 {
        return Err(VfsError::InvalidArgument);
    }
    let bytes = uaccess::read_vec(bufs, channels * 8, 256)?;
    Ok(bytes
        .chunks_exact(8)
        .map(|chunk| {
            u64::from_ne_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ])
        })
        .collect())
}

fn write_frames_planar(
    substream: &Arc<Substream>,
    bufs: u64,
    frames: u64,
    nonblock: bool,
) -> Result<u64, VfsError> {
    let (frame_bytes, channels, ring_frames) = {
        let runtime = substream.runtime.lock();
        let hw = runtime.hw.ok_or(VfsError::InvalidArgument)?;
        (hw.frame_bytes, hw.channels as usize, hw.buffer_size)
    };
    if frames == 0 {
        return Ok(0);
    }
    let sample_bytes = frame_bytes / channels.max(1);
    let pointers = channel_pointers(bufs, channels)?;
    let mut done = 0u64;
    while done < frames {
        let remaining = frames - done;
        if let Err(err) = wait_for_io(substream, nonblock, remaining) {
            return partial_or_err(err, done);
        }

        let chunk = remaining
            .min((MAX_RW_BYTES / frame_bytes.max(1)) as u64)
            .min(ring_frames)
            .max(1) as usize;
        let plane_bytes = chunk * sample_bytes;
        let mut planes = vec![0u8; plane_bytes * channels];
        for channel in 0..channels {
            let user = pointers[channel]
                .checked_add(done.saturating_mul(sample_bytes as u64))
                .ok_or(VfsError::Fault)?;
            let start = channel * plane_bytes;
            uaccess::read(user, &mut planes[start..start + plane_bytes])?;
        }

        let mut runtime = substream.runtime.lock();
        refresh_control(&mut runtime);
        let avail = avail_locked(&runtime, Direction::Playback);
        if avail == 0 {
            drop(runtime);
            continue;
        }
        let take = avail.min(chunk as u64) as usize;
        let pages = runtime.pages.clone().ok_or(VfsError::InvalidArgument)?;
        let ring = &pages.ring;
        let frames_per_plane = ring.frames as usize;
        let plane_total = frames_per_plane * sample_bytes;
        let start_frame = (runtime.appl_ptr % ring.frames) as usize;
        let bytes = take * sample_bytes;
        for channel in 0..channels {
            let source = channel * plane_bytes;
            let plane = channel * plane_total;
            let start = start_frame * sample_bytes;
            let first = bytes.min(plane_total - start);
            // SAFETY: both ranges stay inside the ring allocation; the source is
            // a channel slice of the temporary plane buffer.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    planes.as_ptr().add(source),
                    ring.pointer().add(plane + start),
                    first,
                );
                if bytes > first {
                    core::ptr::copy_nonoverlapping(
                        planes.as_ptr().add(source + first),
                        ring.pointer().add(plane),
                        bytes - first,
                    );
                }
            }
        }
        runtime.appl_ptr = (runtime.appl_ptr + take as u64) % runtime.boundary;
        runtime.tstamp = now();
        publish(substream, &mut runtime);
        drop(runtime);
        wake(substream);
        done += take as u64;
        start_if_threshold(substream);
    }
    Ok(done)
}

fn read_frames_planar(
    substream: &Arc<Substream>,
    bufs: u64,
    frames: u64,
    nonblock: bool,
) -> Result<u64, VfsError> {
    let (frame_bytes, channels, ring_frames) = {
        let runtime = substream.runtime.lock();
        let hw = runtime.hw.ok_or(VfsError::InvalidArgument)?;
        (hw.frame_bytes, hw.channels as usize, hw.buffer_size)
    };
    if frames == 0 {
        return Ok(0);
    }
    if substream.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_PREPARED {
        let _ = trigger(substream, uapi::SNDRV_PCM_TRIGGER_START);
    }
    let sample_bytes = frame_bytes / channels.max(1);
    let pointers = channel_pointers(bufs, channels)?;
    let mut done = 0u64;
    while done < frames {
        period_elapsed(substream);
        let remaining = frames - done;
        if let Err(err) = wait_for_io(substream, nonblock, remaining) {
            return partial_or_err(err, done);
        }

        let chunk = remaining
            .min((MAX_RW_BYTES / frame_bytes.max(1)) as u64)
            .min(ring_frames)
            .max(1) as usize;
        let plane_bytes = chunk * sample_bytes;
        let mut planes = vec![0u8; plane_bytes * channels];
        let mut runtime = substream.runtime.lock();
        refresh_control(&mut runtime);
        let avail = avail_locked(&runtime, Direction::Capture);
        if avail == 0 {
            drop(runtime);
            continue;
        }
        let take = avail.min(chunk as u64) as usize;
        let pages = runtime.pages.clone().ok_or(VfsError::InvalidArgument)?;
        let ring = &pages.ring;
        let frames_per_plane = ring.frames as usize;
        let plane_total = frames_per_plane * sample_bytes;
        let start_frame = (runtime.appl_ptr % ring.frames) as usize;
        let bytes = take * sample_bytes;
        for channel in 0..channels {
            let destination = channel * plane_bytes;
            let plane = channel * plane_total;
            let start = start_frame * sample_bytes;
            let first = bytes.min(plane_total - start);
            // SAFETY: as in the write path; both sides are bounded ring or
            // temporary plane slices.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    ring.pointer().add(plane + start),
                    planes.as_mut_ptr().add(destination),
                    first,
                );
                if bytes > first {
                    core::ptr::copy_nonoverlapping(
                        ring.pointer().add(plane),
                        planes.as_mut_ptr().add(destination + first),
                        bytes - first,
                    );
                }
            }
        }
        runtime.appl_ptr = (runtime.appl_ptr + take as u64) % runtime.boundary;
        runtime.tstamp = now();
        publish(substream, &mut runtime);
        drop(runtime);
        wake(substream);
        for channel in 0..channels {
            let user = pointers[channel]
                .checked_add(done.saturating_mul(sample_bytes as u64))
                .ok_or(VfsError::Fault)?;
            let start = channel * plane_bytes;
            uaccess::write(user, &planes[start..start + bytes])?;
        }
        done += take as u64;
    }
    Ok(done)
}

fn drain_one(substream: &Arc<Substream>) -> Result<(), VfsError> {
    if substream.direction == Direction::Capture {
        return Ok(());
    }
    let (buffer_size, need_start) = {
        let mut runtime = substream.runtime.lock();
        let Some(hw) = runtime.hw else {
            return Err(VfsError::InvalidArgument);
        };
        let avail = avail_locked(&runtime, Direction::Playback);
        match runtime.state {
            uapi::SNDRV_PCM_STATE_XRUN => return Err(VfsError::BrokenPipe),
            uapi::SNDRV_PCM_STATE_PREPARED => {
                if avail >= hw.buffer_size {
                    runtime.state = uapi::SNDRV_PCM_STATE_SETUP;
                    runtime.hw_ptr = 0;
                    runtime.appl_ptr = 0;
                    runtime.raw_hw_ptr = 0;
                    runtime.last_elapsed_us = 0;
                    publish(substream, &mut runtime);
                    drop(runtime);
                    wake(substream);
                    return Ok(());
                }
                (hw.buffer_size, true)
            }
            uapi::SNDRV_PCM_STATE_RUNNING | uapi::SNDRV_PCM_STATE_DRAINING => {
                runtime.state = uapi::SNDRV_PCM_STATE_DRAINING;
                publish(substream, &mut runtime);
                drop(runtime);
                wake(substream);
                (hw.buffer_size, false)
            }
            _ => return Ok(()),
        }
    };
    if need_start {
        trigger(substream, uapi::SNDRV_PCM_TRIGGER_START)?;
        let mut runtime = substream.runtime.lock();
        if runtime.state == uapi::SNDRV_PCM_STATE_RUNNING {
            runtime.state = uapi::SNDRV_PCM_STATE_DRAINING;
            publish(substream, &mut runtime);
            drop(runtime);
            wake(substream);
        }
    }
    loop {
        let state = substream.state.load(Ordering::Acquire);
        if state == uapi::SNDRV_PCM_STATE_XRUN {
            return Err(VfsError::BrokenPipe);
        }
        if state != uapi::SNDRV_PCM_STATE_DRAINING
            || substream.avail.load(Ordering::Acquire) >= buffer_size
        {
            break;
        }
        // Linux `snd_pcm_drain()` returns -ERESTARTSYS on a pending signal and
        // leaves the stream DRAINING; the period path finishes it to SETUP.
        let mut wait = pollwait::PollWait::new();
        loop {
            wait.arm_object(Arc::as_ptr(substream));
            let state = substream.state.load(Ordering::Acquire);
            if state != uapi::SNDRV_PCM_STATE_DRAINING
                || substream.avail.load(Ordering::Acquire) >= buffer_size
            {
                break;
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    }
    if let Some(ops) = substream.ops.lock().clone() {
        let _ = ops.trigger(substream, uapi::SNDRV_PCM_TRIGGER_DRAIN);
        let _ = ops.trigger(substream, uapi::SNDRV_PCM_TRIGGER_STOP);
    }
    let mut runtime = substream.runtime.lock();
    runtime.state = uapi::SNDRV_PCM_STATE_SETUP;
    runtime.hw_ptr = 0;
    runtime.appl_ptr = 0;
    runtime.raw_hw_ptr = 0;
    runtime.last_elapsed_us = 0;
    publish(substream, &mut runtime);
    drop(runtime);
    wake(substream);
    Ok(())
}

fn drain(substream: &Arc<Substream>) -> Result<(), VfsError> {
    for member in group_members(substream) {
        drain_one(&member)?;
    }
    Ok(())
}

/// Handle one PCM ioctl.  `nonblock` comes from the file description and is
/// used by the readi/writei blocking paths.
pub fn ioctl(
    substream: &Arc<Substream>,
    cmd: u32,
    arg: u64,
    nonblock: bool,
) -> Result<u64, VfsError> {
    match cmd {
        uapi::SNDRV_PCM_IOCTL_PVERSION => {
            uaccess::write_value(arg, &(uapi::SNDRV_PCM_VERSION as i32))?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_USER_PVERSION => {
            let version = uaccess::read_value::<i32>(arg)?;
            substream.runtime.lock().user_pversion = version as u32;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_TSTAMP => {
            let mode = uaccess::read_value::<i32>(arg)?;
            if mode != uapi::SNDRV_PCM_TSTAMP_NONE && mode != uapi::SNDRV_PCM_TSTAMP_ENABLE {
                return Err(VfsError::InvalidArgument);
            }
            substream.runtime.lock().tstamp_mode = mode;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_TTSTAMP => {
            let kind = uaccess::read_value::<i32>(arg)?;
            if !(0..=2).contains(&kind) {
                return Err(VfsError::InvalidArgument);
            }
            substream.runtime.lock().tstamp_type = kind;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_INFO => {
            let info = substream.info();
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_HW_REFINE => {
            let mut params = uaccess::read_value::<uapi::snd_pcm_hw_params>(arg)?;
            refine(substream, &mut params);
            uaccess::write_value(arg, &params)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_HW_PARAMS => {
            let params = uaccess::read_value::<uapi::snd_pcm_hw_params>(arg)?;
            configure(substream, params, arg)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_HW_FREE => {
            match substream.state.load(Ordering::Acquire) {
                uapi::SNDRV_PCM_STATE_RUNNING | uapi::SNDRV_PCM_STATE_DRAINING => {
                    trigger(substream, uapi::SNDRV_PCM_TRIGGER_STOP)?;
                }
                _ => {}
            }
            {
                let runtime = substream.runtime.lock();
                if runtime.hw.is_none() {
                    return Ok(0);
                }
            }
            release_buffer(substream);
            if let Some(ops) = substream.ops.lock().clone() {
                ops.hw_free(substream);
            }
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_SW_PARAMS => {
            let mut params = uaccess::read_value::<uapi::snd_pcm_sw_params>(arg)?;
            let mut runtime = substream.runtime.lock();
            let hw = runtime.hw.ok_or(VfsError::InvalidArgument)?;
            if params.boundary == 0 {
                params.boundary = default_boundary(hw.buffer_size);
            }
            if params.boundary <= hw.buffer_size {
                return Err(VfsError::InvalidArgument);
            }
            if params.avail_min > hw.buffer_size {
                params.avail_min = hw.buffer_size;
            }
            if params.start_threshold == 0 {
                params.start_threshold = match substream.direction {
                    Direction::Playback => hw.buffer_size,
                    Direction::Capture => 1,
                };
            }
            if params.stop_threshold == 0 {
                params.stop_threshold = hw.buffer_size;
            }
            runtime.sw = params;
            runtime.boundary = params.boundary;
            publish(substream, &mut runtime);
            drop(runtime);
            uaccess::write_value(arg, &params)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_STATUS | uapi::SNDRV_PCM_IOCTL_STATUS_EXT => {
            let config = if cmd == uapi::SNDRV_PCM_IOCTL_STATUS_EXT {
                Some(
                    uaccess::read_value::<uapi::snd_pcm_status>(arg)
                        .map_or(0, |status| status.audio_tstamp_data),
                )
            } else {
                None
            };
            period_elapsed(substream);
            let mut runtime = substream.runtime.lock();
            refresh_control(&mut runtime);
            let status = fill_status(substream, &runtime, config);
            runtime.avail_max = status.avail;
            runtime.overrange = 0;
            sync_pages(&mut runtime);
            drop(runtime);
            uaccess::write_value(arg, &status)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_DELAY => {
            period_elapsed(substream);
            let runtime = substream.runtime.lock();
            let status = fill_status(substream, &runtime, None);
            drop(runtime);
            uaccess::write_value(arg, &status.delay)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_HWSYNC => {
            period_elapsed(substream);
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_SYNC_PTR => {
            let mut request = uaccess::read_value::<uapi::snd_pcm_sync_ptr>(arg)?;
            if request.flags & uapi::SNDRV_PCM_SYNC_PTR_HWSYNC != 0 {
                period_elapsed(substream);
            }
            let mut runtime = substream.runtime.lock();
            refresh_control(&mut runtime);
            let status = fill_status(substream, &runtime, None);
            let mmap_status = uapi::snd_pcm_mmap_status {
                state: status.state,
                pad1: 0,
                hw_ptr: status.hw_ptr,
                tstamp: status.tstamp,
                suspended_state: status.suspended_state,
                pad2: 0,
                audio_tstamp: status.audio_tstamp,
            };
            request.s.status = mmap_status;
            request.c.control = uapi::snd_pcm_mmap_control {
                appl_ptr: runtime.appl_ptr,
                avail_min: runtime.sw.avail_min,
            };
            sync_pages(&mut runtime);
            drop(runtime);
            uaccess::write_value(arg, &request)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_CHANNEL_INFO => {
            let mut request = uaccess::read_value::<uapi::snd_pcm_channel_info>(arg)?;
            let runtime = substream.runtime.lock();
            let hw = runtime.hw.ok_or(VfsError::InvalidArgument)?;
            if request.channel >= hw.channels {
                return Err(VfsError::InvalidArgument);
            }
            let sample_bytes = hw.frame_bytes / hw.channels.max(1) as usize;
            if hw.noninterleaved {
                request.offset =
                    (request.channel as u64 * hw.buffer_size * sample_bytes as u64) as i64;
                request.first = 0;
                request.step = hw.sample_bits;
            } else {
                request.offset = 0;
                request.first = request.channel * hw.sample_bits;
                request.step = hw.frame_bits;
            }
            drop(runtime);
            uaccess::write_value(arg, &request)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_PREPARE => {
            prepare(substream)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_RESET => {
            reset(substream)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_START => {
            trigger(substream, uapi::SNDRV_PCM_TRIGGER_START)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_DROP => {
            trigger(substream, uapi::SNDRV_PCM_TRIGGER_STOP)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_DRAIN => {
            drain(substream)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_PAUSE => {
            let pause = uaccess::read_value::<i32>(arg)?;
            trigger(
                substream,
                if pause != 0 {
                    uapi::SNDRV_PCM_TRIGGER_PAUSE_PUSH
                } else {
                    uapi::SNDRV_PCM_TRIGGER_PAUSE_RELEASE
                },
            )?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_RESUME => {
            if substream.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_SUSPENDED {
                trigger(substream, uapi::SNDRV_PCM_TRIGGER_RESUME)?;
            }
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_XRUN => {
            if substream.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_XRUN {
                Err(VfsError::BrokenPipe)
            } else {
                Ok(0)
            }
        }
        uapi::SNDRV_PCM_IOCTL_REWIND | uapi::SNDRV_PCM_IOCTL_FORWARD => {
            let frames = uaccess::read_value::<u64>(arg)?;
            let mut runtime = substream.runtime.lock();
            if runtime.hw.is_none() || runtime.boundary == 0 {
                return Err(VfsError::InvalidArgument);
            }
            refresh_control(&mut runtime);
            let hw = runtime.hw.unwrap();
            let in_flight =
                (runtime.hw_ptr + runtime.boundary - runtime.appl_ptr) % runtime.boundary;
            let free = hw.buffer_size.saturating_sub(in_flight);
            let max = match (substream.direction, cmd) {
                (Direction::Playback, uapi::SNDRV_PCM_IOCTL_REWIND) => in_flight,
                (Direction::Playback, _) => free,
                (Direction::Capture, uapi::SNDRV_PCM_IOCTL_REWIND) => 0,
                (Direction::Capture, _) => in_flight,
            };
            let frames = frames.min(max);
            runtime.appl_ptr = if cmd == uapi::SNDRV_PCM_IOCTL_REWIND {
                (runtime.appl_ptr + runtime.boundary - frames) % runtime.boundary
            } else {
                (runtime.appl_ptr + frames) % runtime.boundary
            };
            runtime.tstamp = now();
            publish(substream, &mut runtime);
            drop(runtime);
            wake(substream);
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_WRITEI_FRAMES => {
            let mut request = uaccess::read_value::<uapi::snd_xferi>(arg)?;
            let done = write_frames(substream, request.buf, request.frames, nonblock)?;
            request.result = done as i64;
            uaccess::write_value(arg, &request)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_READI_FRAMES => {
            let mut request = uaccess::read_value::<uapi::snd_xferi>(arg)?;
            let done = read_frames(substream, request.buf, request.frames, nonblock)?;
            request.result = done as i64;
            uaccess::write_value(arg, &request)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_WRITEN_FRAMES | uapi::SNDRV_PCM_IOCTL_READN_FRAMES => {
            Err(VfsError::InvalidArgument)
        }
        uapi::SNDRV_PCM_IOCTL_LINK => {
            let fd = uaccess::read_value::<i32>(arg)?;
            link(substream, fd)?;
            Ok(0)
        }
        uapi::SNDRV_PCM_IOCTL_UNLINK => {
            unlink(substream)?;
            Ok(0)
        }
        _ => Err(VfsError::NotATty),
    }
}

pub struct Pcm {
    pub card: u32,
    pub device: u32,
    pub id: String,
    pub name: String,
    pub playback: Vec<Arc<Substream>>,
    pub capture: Vec<Arc<Substream>>,
    pub info_flags: u32,
}

impl Pcm {
    pub fn new(
        card: u32,
        device: u32,
        id: &str,
        name: &str,
        playback_count: u32,
        capture_count: u32,
        info_flags: u32,
    ) -> Arc<Self> {
        fn build(
            card: u32,
            device: u32,
            direction: Direction,
            id: &str,
            name: &str,
            count: u32,
            info_flags: u32,
        ) -> Vec<Arc<Substream>> {
            let stream: Vec<_> = (0..count)
                .map(|subdevice| {
                    Substream::new(card, device, subdevice, direction, id, name, info_flags)
                })
                .collect();
            for substream in &stream {
                *substream.siblings.lock() = stream.iter().map(Arc::downgrade).collect();
            }
            stream
        }
        Arc::new(Self {
            card,
            device,
            id: id.to_string(),
            name: name.to_string(),
            playback: build(
                card,
                device,
                Direction::Playback,
                id,
                name,
                playback_count,
                info_flags,
            ),
            capture: build(
                card,
                device,
                Direction::Capture,
                id,
                name,
                capture_count,
                info_flags,
            ),
            info_flags,
        })
    }

    pub fn substream(&self, direction: Direction, subdevice: u32) -> Option<Arc<Substream>> {
        self.substreams(direction)
            .into_iter()
            .find(|substream| substream.subdevice == subdevice)
    }

    pub fn first_substream(&self, direction: Direction) -> Option<Arc<Substream>> {
        self.substreams(direction).into_iter().next()
    }

    pub fn substreams(&self, direction: Direction) -> Vec<Arc<Substream>> {
        match direction {
            Direction::Playback => self.playback.clone(),
            Direction::Capture => self.capture.clone(),
        }
    }

    pub fn substream_count(&self, direction: Direction) -> u32 {
        match direction {
            Direction::Playback => self.playback.len() as u32,
            Direction::Capture => self.capture.len() as u32,
        }
    }
}

/// Direction node (`pcmC<n>D<m>{p,c}`).  An open picks the first free
/// substream, matching Linux's `snd_pcm_attach_substream`; when every
/// substream is busy the open fails with EAGAIN.
pub struct PcmDeviceNode {
    substreams: Vec<Arc<Substream>>,
}

impl PcmDeviceNode {
    pub fn new(substreams: Vec<Arc<Substream>>) -> Arc<Self> {
        Arc::new(Self { substreams })
    }
}

impl Inode for PcmDeviceNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        let nonblock = flags & crate::vfs::abi::O_NONBLOCK != 0;
        for substream in &self.substreams {
            if substream.opened.swap(true, Ordering::AcqRel) {
                continue;
            }
            if let Some(ops) = substream.ops.lock().clone()
                && let Err(error) = ops.open(substream)
            {
                substream.opened.store(false, Ordering::Release);
                return Err(from_errno(error));
            }
            return Ok(Some(PcmSession::session(substream.clone(), nonblock)));
        }
        Err(VfsError::WouldBlock)
    }

    fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }

    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
}

#[cfg(feature = "boot-self-test")]
fn link_self_test() -> bool {
    let Some(card) = crate::sound::find_card(0) else {
        return false;
    };
    let Some(pcm) = card.find_pcm(0) else {
        return false;
    };
    let (Some(playback), Some(capture)) = (
        pcm.first_substream(Direction::Playback),
        pcm.first_substream(Direction::Capture),
    ) else {
        return false;
    };
    if prepare(&playback).is_err() || prepare(&capture).is_err() {
        return false;
    }
    let session: Arc<dyn Inode> = PcmSession::session(capture.clone(), false);
    let file = Arc::new(crate::vfs::OpenFile::new(
        session,
        String::from("sound-link"),
        crate::vfs::abi::O_RDWR,
    ));
    let fd = match crate::vfs::current_fd_table().lock().alloc_fd(
        0,
        crate::vfs::FdEntry {
            file,
            cloexec: true,
        },
    ) {
        Ok(fd) => fd,
        Err(_) => return false,
    };
    let linked = link(&playback, fd as i32).is_ok()
        && playback.group.lock().len() == 2
        && capture.group.lock().len() == 2
        && trigger(&playback, uapi::SNDRV_PCM_TRIGGER_START).is_ok()
        && playback.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_RUNNING
        && capture.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_RUNNING;
    let _ = unlink(&playback);
    let groups_empty = playback.group.lock().is_empty() && capture.group.lock().is_empty();
    let _ = trigger(&playback, uapi::SNDRV_PCM_TRIGGER_STOP);
    let _ = prepare(&playback);
    let _ = prepare(&capture);
    let _ = crate::vfs::current_fd_table().lock().close(fd);
    linked && groups_empty
}

#[cfg(feature = "boot-self-test")]
fn test_params() -> uapi::snd_pcm_hw_params {
    let mut params = uapi::snd_pcm_hw_params::zeroed();
    params.masks[uapi::SNDRV_PCM_HW_PARAM_ACCESS as usize]
        .set(uapi::SNDRV_PCM_ACCESS_RW_INTERLEAVED);
    params.masks[uapi::SNDRV_PCM_HW_PARAM_FORMAT as usize].set(uapi::SNDRV_PCM_FORMAT_S16_LE);
    params.masks[uapi::SNDRV_PCM_HW_PARAM_SUBFORMAT as usize].set(uapi::SNDRV_PCM_SUBFORMAT_STD);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_CHANNELS - 8) as usize] =
        uapi::snd_interval::exact(2);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_RATE - 8) as usize] =
        uapi::snd_interval::exact(44_100);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_PERIOD_SIZE - 8) as usize] =
        uapi::snd_interval::exact(1024);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_PERIODS - 8) as usize] =
        uapi::snd_interval::exact(4);
    params.intervals[(uapi::SNDRV_PCM_HW_PARAM_BUFFER_SIZE - 8) as usize] =
        uapi::snd_interval::exact(4096);
    params
}

#[cfg(feature = "boot-self-test")]
fn module_driver_direction(substream: &Arc<Substream>) -> bool {
    let Some(ops) = substream.ops.lock().clone() else {
        return false;
    };
    if ops.open(substream).is_err() {
        return false;
    }
    substream.opened.store(true, Ordering::Release);
    let mut params = test_params();
    let configure_result = apply_hw_params(substream, &mut params);
    let configured = configure_result.is_ok();
    let prepared = configured && prepare(substream).is_ok();
    if prepared && substream.direction == Direction::Playback {
        let mut runtime = substream.runtime.lock();
        if let Some(hw) = runtime.hw {
            runtime.appl_ptr = (runtime.hw_ptr + hw.buffer_size) % runtime.boundary;
            publish(substream, &mut runtime);
        }
    }
    let started = prepared && trigger(substream, uapi::SNDRV_PCM_TRIGGER_START).is_ok();
    // Sample progress while the stream is still RUNNING. A single sample after
    // a fixed sleep is racy: emulated codecs (QEMU audio backends) pull their
    // first bursts faster than real time, so the one prefilled buffer can be
    // fully consumed and the stream correctly enters XRUN (Linux semantics:
    // avail >= stop_threshold) before the sample is taken.
    let mut running = false;
    let mut pointer = 0;
    let mut last_state = -1;
    let mut polls = 0;
    if started {
        for _ in 0..60 {
            crate::sched::sleep_micros(5_000);
            polls += 1;
            let runtime = substream.runtime.lock();
            let status = fill_status(substream, &runtime, None);
            last_state = status.state;
            pointer = status.hw_ptr;
            if status.state == uapi::SNDRV_PCM_STATE_RUNNING && status.hw_ptr > 0 {
                running = true;
                break;
            }
            if status.state != uapi::SNDRV_PCM_STATE_RUNNING {
                break;
            }
        }
    }
    if !running {
        crate::kinfo!(
            "sound: acceptance {:?}: configured={:?} prepared={} started={} state={} hw_ptr={} raw={} polls={}",
            substream.direction,
            configure_result.err(),
            prepared,
            started,
            last_state,
            pointer,
            ops.pointer(substream),
            polls
        );
    }
    let _ = trigger(substream, uapi::SNDRV_PCM_TRIGGER_STOP);
    let _ = prepare(substream);
    release_buffer(substream);
    ops.hw_free(substream);
    ops.close(substream);
    substream.opened.store(false, Ordering::Release);
    started && running && pointer > 0
}

/// End-to-end acceptance for a loaded hardware driver: open, configure,
/// start, wait for period interrupts, inspect the hardware pointer, stop.
/// `capture` selects the direction so QEMU sb16 (playback-only emulation) can
/// validate its DAC path while a real SB16 also exercises the ADC path.
#[cfg(feature = "boot-self-test")]
pub fn module_driver_test(card_id: &str, capture: bool) -> bool {
    let Some(card) = crate::sound::cards()
        .into_iter()
        .find(|card| card.id == card_id)
    else {
        return false;
    };
    let Some(pcm) = card.find_pcm(0) else {
        return false;
    };
    let direction = if capture {
        Direction::Capture
    } else {
        Direction::Playback
    };
    let Some(substream) = pcm.first_substream(direction) else {
        return false;
    };
    if !module_driver_direction(&substream) {
        crate::kinfo!(
            "sound: {} {} acceptance failed",
            card_id,
            if capture { "capture" } else { "playback" }
        );
        return false;
    }
    true
}

#[cfg(feature = "boot-self-test")]
pub fn self_test_late() -> bool {
    let Some(card) = crate::sound::find_card(0) else {
        return false;
    };
    let Some(pcm) = card.find_pcm(0) else {
        return false;
    };
    let Some(playback) = pcm.first_substream(Direction::Playback) else {
        return false;
    };
    if prepare(&playback).is_err() || trigger(&playback, uapi::SNDRV_PCM_TRIGGER_START).is_err() {
        return false;
    }
    // Fill the ring by moving the application pointer, exactly as a userspace
    // buffer-full write would, so the dummy device has data to consume.
    {
        let mut runtime = playback.runtime.lock();
        let Some(hw) = runtime.hw else {
            return false;
        };
        runtime.appl_ptr = (runtime.hw_ptr + hw.buffer_size) % runtime.boundary;
        publish(&playback, &mut runtime);
    }
    // Poll for progress rather than sampling once after a fixed sleep: an
    // oversleep past the one prefilled buffer is a legitimate XRUN.
    let mut playing = false;
    for _ in 0..50 {
        crate::sched::sleep_micros(1_000);
        period_elapsed(&playback);
        let runtime = playback.runtime.lock();
        let status = fill_status(&playback, &runtime, None);
        if status.state == uapi::SNDRV_PCM_STATE_RUNNING && status.hw_ptr > 0 {
            playing = true;
            break;
        }
        if status.state != uapi::SNDRV_PCM_STATE_RUNNING {
            break;
        }
    }
    let paused = trigger(&playback, uapi::SNDRV_PCM_TRIGGER_PAUSE_PUSH).is_ok()
        && playback.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_PAUSED;
    let resumed = trigger(&playback, uapi::SNDRV_PCM_TRIGGER_PAUSE_RELEASE).is_ok()
        && playback.state.load(Ordering::Acquire) == uapi::SNDRV_PCM_STATE_RUNNING;
    let _ = trigger(&playback, uapi::SNDRV_PCM_TRIGGER_STOP);
    let _ = prepare(&playback);

    let mut capture_ok = true;
    if let Some(capture) = pcm.first_substream(Direction::Capture) {
        capture_ok =
            prepare(&capture).is_ok() && trigger(&capture, uapi::SNDRV_PCM_TRIGGER_START).is_ok();
        if capture_ok {
            capture_ok = false;
            for _ in 0..50 {
                crate::sched::sleep_micros(1_000);
                period_elapsed(&capture);
                let runtime = capture.runtime.lock();
                if runtime.state == uapi::SNDRV_PCM_STATE_RUNNING
                    && avail_locked(&runtime, Direction::Capture) > 0
                {
                    capture_ok = true;
                    break;
                }
                if runtime.state != uapi::SNDRV_PCM_STATE_RUNNING {
                    break;
                }
            }
            let _ = trigger(&capture, uapi::SNDRV_PCM_TRIGGER_STOP);
            let _ = prepare(&capture);
        }
    }
    playing && paused && resumed && capture_ok && link_self_test()
}

/// Per-open PCM file state.  The object stored in the device registry is a
/// template whose `open` creates one session per file descriptor.
pub struct PcmSession {
    substream: Arc<Substream>,
    nonblock: AtomicBool,
    owns_open: AtomicBool,
    self_reference: SpinLock<Weak<PcmSession>>,
}

impl PcmSession {
    pub(crate) fn session(substream: Arc<Substream>, nonblock: bool) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            substream,
            nonblock: AtomicBool::new(nonblock),
            owns_open: AtomicBool::new(true),
            self_reference: SpinLock::new(weak.clone()),
        })
    }

    pub fn substream(&self) -> &Arc<Substream> {
        &self.substream
    }
}

impl Drop for PcmSession {
    fn drop(&mut self) {
        if self.owns_open.swap(false, Ordering::AcqRel) {
            if matches!(
                self.substream.state.load(Ordering::Acquire),
                uapi::SNDRV_PCM_STATE_RUNNING | uapi::SNDRV_PCM_STATE_DRAINING
            ) {
                let _ = trigger(&self.substream, uapi::SNDRV_PCM_TRIGGER_STOP);
            }
            if let Some(ops) = self.substream.ops.lock().clone() {
                ops.close(&self.substream);
            }
            self.substream.opened.store(false, Ordering::Release);
            wake(&self.substream);
        }
    }
}

impl Inode for PcmSession {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }

    fn open(&self, _flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        Ok(None)
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        ioctl(
            &self.substream,
            cmd as u32,
            arg,
            self.nonblock.load(Ordering::Acquire),
        )
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }

    #[cfg(feature = "sound")]
    fn sound_substream(&self) -> Option<Arc<Substream>> {
        Some(self.substream.clone())
    }

    fn poll(&self) -> u32 {
        let state = self.substream.state.load(Ordering::Acquire);
        if state == uapi::SNDRV_PCM_STATE_XRUN {
            return POLLERR;
        }
        if state == uapi::SNDRV_PCM_STATE_DISCONNECTED {
            return POLLERR | POLLHUP;
        }
        let avail = self.substream.avail.load(Ordering::Acquire);
        let runtime = self.substream.runtime.lock();
        let threshold = runtime.sw.avail_min;
        drop(runtime);
        let ready = avail >= threshold;
        match self.substream.direction {
            Direction::Playback => {
                if ready {
                    POLLOUT
                } else {
                    0
                }
            }
            Direction::Capture => {
                if ready {
                    POLLIN
                } else {
                    0
                }
            }
        }
    }

    fn poll_sources(&self, sources: &mut pollwait::PollSources) {
        sources.object(self.substream.as_ref());
        sources.tick();
    }

    fn mmap(
        &self,
        space: &mut crate::user::UserAddressSpace,
        address: u64,
        length: u64,
        offset: u64,
        prot: u32,
    ) -> Result<(), VfsError> {
        let runtime = self.substream.runtime.lock();
        let pages = runtime.pages.clone().ok_or(VfsError::InvalidArgument)?;
        let (physical, limit) = match offset {
            uapi::SNDRV_PCM_MMAP_OFFSET_DATA => {
                (pages.ring.dma_address(), pages.ring.mapped_bytes as u64)
            }
            uapi::SNDRV_PCM_MMAP_OFFSET_STATUS_OLD | uapi::SNDRV_PCM_MMAP_OFFSET_STATUS_NEW => {
                (pages.status.dma_address(), PAGE_SIZE as u64)
            }
            uapi::SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD | uapi::SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW => {
                (pages.control.dma_address(), PAGE_SIZE as u64)
            }
            _ => return Err(VfsError::InvalidArgument),
        };
        if length == 0 || length > limit {
            return Err(VfsError::InvalidArgument);
        }
        drop(runtime);
        let pin: Arc<dyn Inode> = Arc::new(MmapPin { _pages: pages });
        let mut page = 0;
        while page < length {
            space
                .map_external(address + page, physical + page, prot, pin.clone())
                .map_err(|_| VfsError::NoMemory)?;
            page += PAGE_SIZE as u64;
        }
        Ok(())
    }

    fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }

    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
}
