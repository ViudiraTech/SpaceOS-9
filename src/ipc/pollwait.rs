/*
 *
 *       src/ipc/pollwait.rs
 *       Targeted readiness wakeups for poll, select, epoll and blocking I/O
 *
 *       2026/10/31 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A blocked task names the objects whose readiness it depends on, and a state
//! change wakes only the tasks that named that object. Waking every poller on
//! every event costs a context switch per open client per X11 request, which is
//! what made the desktop slow down with each additional window.
//!
//! Objects are hashed into a small bitmask, so a collision causes one harmless
//! spurious recheck and never a missed wakeup. An inode that does not announce
//! its changes leaves the default `Inode::poll_sources`, which rechecks every
//! millisecond for just the tasks actually waiting on that inode.

use crate::{sched::WaitQueue, sync::SpinLock};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const TICK_MICROS: u64 = 1000;
/// Upper bound on a sleep, so a state change with no announcement is late rather
/// than lost forever.
const SAFETY_MICROS: u64 = 100_000;
const MASK_WORDS: usize = 2;

fn slot(key: usize) -> (usize, u64) {
    // Fibonacci hashing: allocation addresses differ mostly in their middle bits.
    let bit = (key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 57;
    ((bit / 64) as usize, 1 << (bit % 64))
}

/// What a waiter depends on: announced objects, a deadline, or periodic checks.
#[derive(Clone, Copy, Default)]
pub struct PollSources {
    mask: [u64; MASK_WORDS],
    tick: bool,
    deadline: Option<u64>,
}

impl PollSources {
    pub const fn new() -> Self {
        Self {
            mask: [0; MASK_WORDS],
            tick: false,
            deadline: None,
        }
    }

    /// Depend on an object whose changes are announced through `wake_object`.
    pub fn object<T: ?Sized>(&mut self, object: *const T) {
        let (word, bit) = slot(object as *const () as usize);
        self.mask[word] |= bit;
    }

    /// Depend on state that nothing announces; recheck every millisecond.
    pub fn tick(&mut self) {
        self.tick = true;
    }

    /// Wake no later than `micros` on the uptime clock.
    pub fn expire_at(&mut self, micros: u64) {
        self.deadline = Some(self.deadline.map_or(micros, |current| current.min(micros)));
    }
}

struct Waiter {
    woken: AtomicBool,
    mask: [AtomicU64; MASK_WORDS],
    deadline: AtomicU64,
    queue: WaitQueue,
}

impl Waiter {
    fn wake(&self) {
        if !self.woken.swap(true, Ordering::AcqRel) {
            self.queue.wake_all();
        }
    }
}

static WAITERS: SpinLock<Vec<Arc<Waiter>>> = SpinLock::new(Vec::new());
static NEXT_DEADLINE: AtomicU64 = AtomicU64::new(u64::MAX);
static KICK: AtomicBool = AtomicBool::new(false);

/// One blocking operation. The fast path allocates and registers nothing:
///
/// ```text
/// let mut wait = PollWait::new();
/// loop {
///     wait.arm(&sources);     // no-op until the first sleep()
///     if ready { return; }
///     wait.sleep();           // the first call only registers, then rechecks
/// }
/// ```
///
/// A state change published after `arm` always ends the following `sleep`.
pub struct PollWait {
    waiter: Option<Arc<Waiter>>,
}

impl PollWait {
    pub const fn new() -> Self {
        Self { waiter: None }
    }

    /// Whether `arm` has any effect, so callers can skip gathering sources.
    pub fn registered(&self) -> bool {
        self.waiter.is_some()
    }

    pub fn arm(&mut self, sources: &PollSources) {
        let Some(waiter) = &self.waiter else {
            return;
        };
        let now = crate::time::uptime_micros();
        let mut deadline = now.saturating_add(SAFETY_MICROS);
        if sources.tick {
            deadline = deadline.min(now.saturating_add(TICK_MICROS));
        }
        if let Some(requested) = sources.deadline {
            deadline = deadline.min(requested);
        }
        let _waiters = WAITERS.lock();
        waiter.woken.store(false, Ordering::Release);
        for (word, value) in waiter.mask.iter().zip(sources.mask) {
            word.store(value, Ordering::Release);
        }
        waiter.deadline.store(deadline, Ordering::Release);
        NEXT_DEADLINE.fetch_min(deadline, Ordering::AcqRel);
    }

    /// `arm` for the common case of waiting on one object.
    pub fn arm_object<T: ?Sized>(&mut self, object: *const T) {
        if self.registered() {
            let mut sources = PollSources::new();
            sources.object(object);
            self.arm(&sources);
        }
    }

    pub fn sleep(&mut self) {
        match &self.waiter {
            Some(waiter) => waiter
                .queue
                .wait_until(|| waiter.woken.load(Ordering::Acquire)),
            None => {
                let waiter = Arc::new(Waiter {
                    woken: AtomicBool::new(false),
                    mask: [const { AtomicU64::new(0) }; MASK_WORDS],
                    deadline: AtomicU64::new(u64::MAX),
                    queue: WaitQueue::new(),
                });
                WAITERS.lock().push(waiter.clone());
                self.waiter = Some(waiter);
            }
        }
    }
}

impl Drop for PollWait {
    fn drop(&mut self) {
        if let Some(waiter) = self.waiter.take() {
            WAITERS.lock().retain(|entry| !Arc::ptr_eq(entry, &waiter));
        }
    }
}

/// Announce that `object` may have changed readiness. Callable from IRQ context,
/// but not while holding a scheduler lock; use `poll_kick` where that is unknown.
pub fn wake_object<T: ?Sized>(object: *const T) {
    let (word, bit) = slot(object as *const () as usize);
    for waiter in WAITERS.lock().iter() {
        if waiter.mask[word].load(Ordering::Acquire) & bit != 0 {
            waiter.wake();
        }
    }
}

/// Wake every waiter, for rare events whose affected objects are not known here.
pub fn wake_pollers() {
    for waiter in WAITERS.lock().iter() {
        waiter.wake();
    }
}

/// Request `wake_pollers` from the next timer tick. Safe in any context, which
/// signal delivery and object teardown on the task exit path both need.
pub fn poll_kick() {
    KICK.store(true, Ordering::Release);
}

/// Serve deadlines and deferred wakeups; called from one CPU's scheduler tick.
pub fn timer_tick(now: u64) {
    let kicked = KICK.load(Ordering::Acquire) && KICK.swap(false, Ordering::AcqRel);
    if !kicked && now < NEXT_DEADLINE.load(Ordering::Acquire) {
        return;
    }
    let waiters = WAITERS.lock();
    let mut next = u64::MAX;
    for waiter in waiters.iter() {
        let deadline = waiter.deadline.load(Ordering::Acquire);
        if kicked || deadline <= now {
            // The woken task re-arms with a fresh deadline before sleeping again.
            waiter.deadline.store(u64::MAX, Ordering::Release);
            waiter.wake();
        } else {
            next = next.min(deadline);
        }
    }
    NEXT_DEADLINE.store(next, Ordering::Release);
}

/// Back off for blocking paths whose state changes rarely and are not announced.
pub fn block_briefly() {
    crate::sched::sleep_micros(TICK_MICROS);
}
