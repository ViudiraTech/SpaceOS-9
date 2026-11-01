/*
 *
 *       src/ipc/timerfd.rs
 *       Linux timerfd monotonic timers with read/poll expiration counts
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Expirations are computed from the monotonic uptime clock on every read or
//! poll.  The scheduler has no hrtimer wake channel, so a blocking read polls
//! once per millisecond; the configured deadline remains exact to the
//! microsecond clock.  CLOCK_REALTIME is not implemented because the kernel
//! has no wall-clock source yet.

use crate::{
    sched,
    sync::SpinLock,
    vfs::{self, Inode, InodeKind, Stat, VfsError},
};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

pub const TFD_TIMER_ABSTIME: u32 = 1;
pub const TFD_TIMER_CANCEL_ON_SET: u32 = 2;

struct TimerState {
    deadline: Option<u64>,
    interval: u64,
    ticks: u64,
}

pub struct TimerFd {
    state: SpinLock<TimerState>,
    nonblock: AtomicBool,
    clockid: u32,
    ino: u64,
}

impl TimerFd {
    pub fn create(clockid: u32, nonblock: bool) -> Arc<Self> {
        Arc::new(Self {
            state: SpinLock::new(TimerState {
                deadline: None,
                interval: 0,
                ticks: 0,
            }),
            nonblock: AtomicBool::new(nonblock),
            clockid,
            ino: vfs::tree::ino(),
        })
    }

    pub fn clockid(&self) -> u32 {
        self.clockid
    }

    fn update(&self, state: &mut TimerState, now: u64) {
        let Some(deadline) = state.deadline else {
            return;
        };
        if now < deadline {
            return;
        }
        if state.interval != 0 {
            let count = 1 + (now - deadline) / state.interval;
            state.ticks = state.ticks.saturating_add(count);
            state.deadline = Some(deadline.saturating_add(count.saturating_mul(state.interval)));
        } else {
            state.ticks = state.ticks.saturating_add(1);
            state.deadline = None;
        }
    }

    /// Apply `itimerspec` (interval then value, each seconds/nanoseconds).
    pub fn settime(&self, flags: u32, spec: &[u64; 4]) -> Result<[u64; 4], VfsError> {
        if flags & !(TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET) != 0 {
            return Err(VfsError::InvalidArgument);
        }
        if flags & TFD_TIMER_CANCEL_ON_SET != 0 {
            // Only meaningful for CLOCK_REALTIME, which is unsupported.
            return Err(VfsError::InvalidArgument);
        }
        let interval_sec = spec[0];
        let interval_nsec = spec[1];
        let value_sec = spec[2];
        let value_nsec = spec[3];
        if interval_sec > i64::MAX as u64
            || value_sec > i64::MAX as u64
            || interval_nsec >= 1_000_000_000
            || value_nsec >= 1_000_000_000
        {
            return Err(VfsError::InvalidArgument);
        }
        let interval = interval_sec
            .saturating_mul(1_000_000)
            .saturating_add(interval_nsec.div_ceil(1_000));
        let value = value_sec
            .saturating_mul(1_000_000)
            .saturating_add(value_nsec.div_ceil(1_000));
        let now = crate::time::uptime_micros();
        let mut state = self.state.lock();
        let old_value = state
            .deadline
            .map(|deadline| deadline.saturating_sub(now))
            .unwrap_or(0);
        let old = [
            state.interval / 1_000_000,
            (state.interval % 1_000_000) * 1_000,
            old_value / 1_000_000,
            (old_value % 1_000_000) * 1_000,
        ];
        state.ticks = 0;
        state.interval = interval;
        if value == 0 {
            state.deadline = None;
        } else if flags & TFD_TIMER_ABSTIME != 0 {
            state.deadline = Some(value);
        } else {
            state.deadline = Some(now.saturating_add(value));
        }
        Ok(old)
    }

    pub fn gettime(&self) -> [u64; 4] {
        let now = crate::time::uptime_micros();
        let mut state = self.state.lock();
        self.update(&mut state, now);
        let remaining = state
            .deadline
            .map(|deadline| deadline.saturating_sub(now))
            .unwrap_or(0);
        [
            state.interval / 1_000_000,
            (state.interval % 1_000_000) * 1_000,
            remaining / 1_000_000,
            (remaining % 1_000_000) * 1_000,
        ]
    }

    fn take(&self) -> u64 {
        let now = crate::time::uptime_micros();
        let mut state = self.state.lock();
        self.update(&mut state, now);
        let ticks = state.ticks;
        state.ticks = 0;
        ticks
    }

    fn readable(&self) -> bool {
        let now = crate::time::uptime_micros();
        let mut state = self.state.lock();
        self.update(&mut state, now);
        state.ticks != 0
    }
}

impl Inode for TimerFd {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::CharDevice, 0o600, 0))
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        if buf.len() < 8 {
            return Err(VfsError::InvalidArgument);
        }
        loop {
            let ticks = self.take();
            if ticks != 0 {
                buf[..8].copy_from_slice(&ticks.to_le_bytes());
                return Ok(8);
            }
            if self.nonblock.load(Ordering::Acquire) {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            sched::sleep_micros(1000);
        }
    }

    fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::InvalidArgument)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        if let Some(deadline) = self.state.lock().deadline {
            sources.expire_at(deadline);
        }
    }

    fn poll(&self) -> u32 {
        if self.readable() {
            crate::ipc::unix::POLLIN
        } else {
            0
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }
}

pub fn instance(fd: u64) -> Result<Arc<TimerFd>, VfsError> {
    if fd > i32::MAX as u64 {
        return Err(VfsError::BadFileDescriptor);
    }
    let inode = vfs::current_fd_table()
        .lock()
        .get(fd as usize)
        .ok_or(VfsError::BadFileDescriptor)?
        .file
        .inode
        .clone();
    let any: Arc<dyn core::any::Any + Send + Sync> = inode;
    any.downcast::<TimerFd>()
        .map_err(|_| VfsError::InvalidArgument)
}
