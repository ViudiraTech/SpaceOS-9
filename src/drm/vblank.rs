/*
 *
 *       src/drm/vblank.rs
 *       Vblank counting, timer-simulated vblank and DRM vblank events
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Devices here have no vblank interrupt, so, like Linux's vblank timer
//! (`drm_crtc_vblank_start_timer`, from vkms), each active CRTC counts
//! frames of its mode's period from the moment it was enabled. The count
//! freezes while the CRTC is off. One kernel thread parks until the earliest
//! queued event's vblank and delivers flip completions, WAIT_VBLANK events
//! and CRTC_QUEUE_SEQUENCE events at that time. The timing is not locked to
//! a physical display, which matches what such hardware can promise.

use super::{
    Device, Mode,
    ioctl::Ioctl,
    kms::{KIND_CRTC_PUBLIC, object_id},
};
use crate::{
    sched::{WaitQueue, WaitQueueHead},
    sync::SpinLock,
    vfs::VfsError,
};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const EVENT_VBLANK: u32 = 0x01;
const EVENT_FLIP_COMPLETE: u32 = 0x02;
const EVENT_CRTC_SEQUENCE: u32 = 0x03;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Flip,
    Vblank,
    Sequence,
}

struct Pending {
    target: u64,
    kind: Kind,
    file: u64,
    user_data: u64,
}

struct Inner {
    active: bool,
    /// Uptime of vblank number `base`.
    epoch_ns: u64,
    period_ns: u64,
    base: u64,
    pending: Vec<Pending>,
}

impl Inner {
    fn count(&self, now: u64) -> u64 {
        if self.active {
            self.base + now.saturating_sub(self.epoch_ns) / self.period_ns
        } else {
            self.base
        }
    }
    fn time_of(&self, sequence: u64) -> u64 {
        self.epoch_ns + sequence.saturating_sub(self.base) * self.period_ns
    }
}

pub(crate) struct Crtc {
    inner: SpinLock<Inner>,
    wait: WaitQueue,
    flip_pending: AtomicBool,
}

impl Crtc {
    pub(crate) fn new() -> Self {
        Self {
            inner: SpinLock::new(Inner {
                active: false,
                epoch_ns: 0,
                period_ns: 16_666_667,
                base: 0,
                pending: Vec::new(),
            }),
            wait: WaitQueue::new(),
            flip_pending: AtomicBool::new(false),
        }
    }

    pub(crate) fn flip_pending(&self) -> bool {
        self.flip_pending.load(Ordering::Acquire)
    }
}

static THREAD: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static THREAD_WAIT: WaitQueueHead = WaitQueueHead::new();

fn kick() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    THREAD_WAIT.wake_all();
}

pub(crate) fn start_thread() {
    if THREAD.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = crate::sched::spawn(
        || loop {
            let seen = GENERATION.load(Ordering::Acquire);
            let now = crate::time::uptime_nanos();
            let mut next: Option<u64> = None;
            for device in super::devices() {
                for (pipe, crtc) in device.vblank.iter().enumerate() {
                    deliver_due(&device, pipe, now);
                    let inner = crtc.inner.lock();
                    if let Some(target) = inner.pending.iter().map(|p| p.target).min() {
                        let at = inner.time_of(target);
                        next = Some(next.map_or(at, |n| n.min(at)));
                    }
                }
            }
            let deadline = next.map(|ns| ns.div_ceil(1000));
            THREAD_WAIT.wait_event(
                || GENERATION.load(Ordering::Acquire) != seen,
                false,
                deadline,
            );
        },
        -5, // timely events matter more than throughput here
    );
}

fn event_bytes(kind: Kind, user_data: u64, sequence: u64, time_ns: u64, crtc_id: u32) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let (code, put_time) = match kind {
        Kind::Flip => (EVENT_FLIP_COMPLETE, true),
        Kind::Vblank => (EVENT_VBLANK, true),
        Kind::Sequence => (EVENT_CRTC_SEQUENCE, false),
    };
    bytes[0..4].copy_from_slice(&code.to_le_bytes());
    bytes[4..8].copy_from_slice(&32u32.to_le_bytes());
    bytes[8..16].copy_from_slice(&user_data.to_le_bytes());
    if put_time {
        // struct drm_event_vblank: tv_sec, tv_usec, sequence, crtc_id.
        let micros = time_ns / 1000;
        bytes[16..20].copy_from_slice(&((micros / 1_000_000) as u32).to_le_bytes());
        bytes[20..24].copy_from_slice(&((micros % 1_000_000) as u32).to_le_bytes());
        bytes[24..28].copy_from_slice(&(sequence as u32).to_le_bytes());
        bytes[28..32].copy_from_slice(&crtc_id.to_le_bytes());
    } else {
        // struct drm_event_crtc_sequence: time_ns, sequence.
        bytes[16..24].copy_from_slice(&time_ns.to_le_bytes());
        bytes[24..32].copy_from_slice(&sequence.to_le_bytes());
    }
    bytes
}

/// Deliver every pending event whose vblank has passed (or all of them when
/// the CRTC is off: Linux sends them on drm_crtc_vblank_off).
fn deliver_due(device: &Arc<Device>, pipe: usize, now: u64) {
    let crtc = &device.vblank[pipe];
    let (due, count, flips_left) = {
        let mut inner = crtc.inner.lock();
        let count = inner.count(now);
        let active = inner.active;
        let mut due = Vec::new();
        let mut index = 0;
        while index < inner.pending.len() {
            if !active || inner.pending[index].target <= count {
                let event = inner.pending.swap_remove(index);
                let time = inner.time_of(event.target.min(count));
                due.push((event, time));
            } else {
                index += 1;
            }
        }
        let flips_left = inner.pending.iter().any(|p| p.kind == Kind::Flip);
        (due, count, flips_left)
    };
    if !flips_left {
        crtc.flip_pending.store(false, Ordering::Release);
    }
    if due.is_empty() {
        return;
    }
    let crtc_id = object_id(pipe, KIND_CRTC_PUBLIC);
    for (event, time) in due {
        let sequence = event.target.min(count);
        super::file::deliver(
            device,
            event.file,
            &event_bytes(event.kind, event.user_data, sequence, time, crtc_id),
        );
    }
    crtc.wait.wake_all();
}

pub(crate) fn crtc_on(device: &Device, pipe: usize, mode: &Mode) {
    let mut inner = device.vblank[pipe].inner.lock();
    if !inner.active {
        inner.active = true;
        inner.epoch_ns = crate::time::uptime_nanos();
    }
    inner.period_ns = mode.frame_ns().max(1_000_000);
}

pub(crate) fn crtc_off(device: &Arc<Device>, pipe: usize) {
    {
        let mut inner = device.vblank[pipe].inner.lock();
        let now = crate::time::uptime_nanos();
        inner.base = inner.count(now);
        inner.active = false;
    }
    deliver_due(device, pipe, crate::time::uptime_nanos());
    device.vblank[pipe].wait.wake_all();
}

pub(crate) fn cancel_device(device: &Arc<Device>) {
    for pipe in 0..device.vblank.len() {
        crtc_off(device, pipe);
    }
}

/// A commit on an active CRTC completes at its next vblank. Returns that
/// vblank number; queues a FLIP_COMPLETE event for `(file, user_data)`.
pub(crate) fn queue_flip(device: &Arc<Device>, pipe: usize, event: Option<(u64, u64)>) -> u64 {
    let crtc = &device.vblank[pipe];
    let target = {
        let mut inner = crtc.inner.lock();
        let target = inner.count(crate::time::uptime_nanos()) + 1;
        if let Some((file, user_data)) = event {
            inner.pending.push(Pending {
                target,
                kind: Kind::Flip,
                file,
                user_data,
            });
            crtc.flip_pending.store(true, Ordering::Release);
        }
        target
    };
    if event.is_some() {
        kick();
    }
    target
}

/// Sleep until vblank `target` has passed or the CRTC is turned off.
pub(crate) fn wait_sequence(device: &Arc<Device>, pipe: usize, target: u64) {
    let crtc = &device.vblank[pipe];
    loop {
        let (done, deadline) = {
            let inner = crtc.inner.lock();
            let now = crate::time::uptime_nanos();
            (
                !inner.active || inner.count(now) >= target,
                inner.time_of(target).div_ceil(1000),
            )
        };
        if done {
            return;
        }
        let result = crtc.wait.wait_event(
            || {
                let inner = crtc.inner.lock();
                !inner.active || inner.count(crate::time::uptime_nanos()) >= target
            },
            true,
            Some(deadline),
        );
        if result == crate::sched::WaitResult::Interrupted {
            return;
        }
    }
}

fn pipe_of(device: &Device, crtc_id: u32) -> Result<usize, VfsError> {
    (0..device.config.pipes.len())
        .find(|&p| object_id(p, KIND_CRTC_PUBLIC) == crtc_id)
        .ok_or(VfsError::NotFound)
}

fn queue(device: &Arc<Device>, pipe: usize, target: u64, kind: Kind, file: u64, user_data: u64) {
    device.vblank[pipe].inner.lock().pending.push(Pending {
        target,
        kind,
        file,
        user_data,
    });
    kick();
}

/// Resolve a 32-bit sequence from userspace against the 64-bit counter.
fn widen(current: u64, sequence: u32) -> u64 {
    let delta = sequence.wrapping_sub(current as u32) as i32 as i64;
    (current as i64 + delta).max(0) as u64
}

/// DRM_IOCTL_WAIT_VBLANK.
pub(crate) fn wait_vblank(device: &Arc<Device>, file: u64, arg: &mut Ioctl) -> Result<(), VfsError> {
    const RELATIVE: u32 = 0x1;
    const EVENT: u32 = 0x0400_0000;
    const NEXTONMISS: u32 = 0x1000_0000;
    const SECONDARY: u32 = 0x2000_0000;
    const SIGNAL: u32 = 0x4000_0000;
    const HIGH_CRTC_MASK: u32 = 0x3e;
    let request = arg.u32(0);
    if request & SIGNAL != 0 {
        return Err(VfsError::InvalidArgument);
    }
    let pipe = if request & HIGH_CRTC_MASK != 0 {
        ((request & HIGH_CRTC_MASK) >> 1) as usize
    } else if request & SECONDARY != 0 {
        1
    } else {
        0
    };
    if pipe >= device.vblank.len() {
        return Err(VfsError::InvalidArgument);
    }
    let crtc = &device.vblank[pipe];
    let (current, active) = {
        let inner = crtc.inner.lock();
        (inner.count(crate::time::uptime_nanos()), inner.active)
    };
    if !active {
        return Err(VfsError::InvalidArgument);
    }
    let sequence = arg.u32(4);
    let mut target = if request & RELATIVE != 0 {
        current + sequence as u64
    } else {
        widen(current, sequence)
    };
    if request & NEXTONMISS != 0 && target <= current {
        target = current + 1;
    }
    if request & EVENT != 0 {
        queue(device, pipe, target.max(current), Kind::Vblank, file, arg.u64(8));
        arg.set_u32(4, target as u32);
        return Ok(());
    }
    wait_sequence(device, pipe, target);
    let inner = crtc.inner.lock();
    let reached = inner.count(crate::time::uptime_nanos()).min(target.max(current));
    let micros = inner.time_of(reached) / 1000;
    drop(inner);
    arg.set_u32(4, reached as u32);
    arg.set_u64(8, micros / 1_000_000);
    arg.set_u64(16, micros % 1_000_000);
    Ok(())
}

/// DRM_IOCTL_CRTC_GET_SEQUENCE.
pub(crate) fn get_sequence(device: &Arc<Device>, arg: &mut Ioctl) -> Result<(), VfsError> {
    let pipe = pipe_of(device, arg.u32(0))?;
    let inner = device.vblank[pipe].inner.lock();
    let count = inner.count(crate::time::uptime_nanos());
    arg.set_u32(4, inner.active as u32);
    arg.set_u64(8, count);
    arg.set_u64(16, inner.time_of(count));
    Ok(())
}

/// DRM_IOCTL_CRTC_QUEUE_SEQUENCE.
pub(crate) fn queue_sequence(device: &Arc<Device>, file: u64, arg: &mut Ioctl) -> Result<(), VfsError> {
    const RELATIVE: u32 = 1;
    const NEXT_ON_MISS: u32 = 2;
    let pipe = pipe_of(device, arg.u32(0))?;
    let flags = arg.u32(4);
    if flags & !(RELATIVE | NEXT_ON_MISS) != 0 {
        return Err(VfsError::InvalidArgument);
    }
    let (current, active) = {
        let inner = device.vblank[pipe].inner.lock();
        (inner.count(crate::time::uptime_nanos()), inner.active)
    };
    if !active {
        return Err(VfsError::InvalidArgument);
    }
    let mut target = if flags & RELATIVE != 0 {
        current + arg.u64(8)
    } else {
        arg.u64(8)
    };
    if flags & NEXT_ON_MISS != 0 && target <= current {
        target = current + 1;
    }
    queue(device, pipe, target.max(current), Kind::Sequence, file, arg.u64(16));
    arg.set_u64(8, target);
    Ok(())
}
