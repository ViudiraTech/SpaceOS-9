/*
 *
 *       src/net/timer.rs
 *       Software timers for protocol retries, aging and watchdogs
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! One kernel thread owns the timer list and runs callbacks in process
//! context, so callbacks may allocate and sleep. A callback that wants to run
//! periodically simply schedules itself again. Resolution is the scheduler
//! tick; the thread sleeps until the earliest deadline (capped so a timer
//! queued from interrupt context is noticed promptly even before the waker
//! path is available).

use crate::{
    sched::{self, WaitQueue},
    sync::SpinLock,
    time,
};
use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Timers scheduled earlier than this before the worker starts still fire once
/// it does, so early-boot registrations are not lost.
const MAX_SLEEP_MICROS: u64 = 20_000;

pub type TimerId = u64;
pub type TimerCallback = Box<dyn FnOnce() + Send>;

struct Entry {
    id: TimerId,
    deadline_us: u64,
    callback: Option<TimerCallback>,
}

static TIMERS: SpinLock<Vec<Entry>> = SpinLock::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
static WORKER_WAIT: SpinLock<Option<WaitQueue>> = SpinLock::new(None);

/// Run `callback` once, `delay_micros` from now.
pub fn schedule_in(delay_micros: u64, callback: TimerCallback) -> TimerId {
    let deadline = time::uptime_micros().saturating_add(delay_micros);
    schedule_at(deadline, callback)
}

/// Run `callback` once the monotonic clock reaches `deadline_us`.
pub fn schedule_at(deadline_us: u64, callback: TimerCallback) -> TimerId {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    TIMERS.lock().push(Entry {
        id,
        deadline_us,
        callback: Some(callback),
    });
    GENERATION.fetch_add(1, Ordering::AcqRel);
    if let Some(queue) = WORKER_WAIT.lock().clone() {
        queue.wake_all();
    }
    id
}

/// Cancel a timer that has not fired yet. A callback already running cannot be
/// stopped; `false` is returned for both cases so callers can treat the answer
/// as "did this call remove pending work".
pub fn cancel(id: TimerId) -> bool {
    let mut timers = TIMERS.lock();
    let before = timers.len();
    timers.retain(|entry| entry.id != id);
    timers.len() != before
}

/// Whether a timer with this id is still waiting to run.
pub fn pending(id: TimerId) -> bool {
    TIMERS.lock().iter().any(|entry| entry.id == id)
}

/// Start the timer thread. Call once after the scheduler is ready; timers
/// scheduled before this survive until it starts.
pub fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let queue = WaitQueue::new();
    *WORKER_WAIT.lock() = Some(queue.clone());
    let result = sched::spawn(
        move || {
            loop {
                run_due();
                let now = time::uptime_micros();
                let next = TIMERS.lock().iter().map(|entry| entry.deadline_us).min();
                match next {
                    None => {
                        let generation = GENERATION.load(Ordering::Acquire);
                        queue.wait_until(|| GENERATION.load(Ordering::Acquire) != generation);
                    }
                    Some(deadline) if deadline > now => {
                        sched::sleep_micros((deadline - now).min(MAX_SLEEP_MICROS));
                    }
                    Some(_) => {}
                }
            }
        },
        0,
    );
    if result.is_err() {
        STARTED.store(false, Ordering::Release);
        crate::kwarn!("net: timer thread could not start");
    }
}

fn run_due() {
    let now = time::uptime_micros();
    let due: Vec<Entry> = {
        let mut timers = TIMERS.lock();
        let mut due = Vec::new();
        let mut index = 0;
        while index < timers.len() {
            if timers[index].deadline_us <= now {
                due.push(timers.swap_remove(index));
            } else {
                index += 1;
            }
        }
        due
    };
    for mut entry in due {
        if let Some(callback) = entry.callback.take() {
            callback();
        }
    }
}

/// Run all expired timers synchronously. Self-tests use this before the worker
/// thread exists.
pub fn process_pending() {
    run_due();
}

#[cfg(feature = "boot-self-test")]
pub mod self_test {
    use super::*;
    use alloc::sync::Arc;
    use core::sync::atomic::AtomicUsize;

    pub fn run() {
        let counter = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicUsize::new(0));
        {
            let counter = counter.clone();
            schedule_in(
                0,
                Box::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        let doomed = {
            let cancelled = cancelled.clone();
            schedule_in(
                60_000,
                Box::new(move || {
                    cancelled.fetch_add(1, Ordering::SeqCst);
                }),
            )
        };
        assert!(pending(doomed));
        assert!(cancel(doomed));
        assert!(!pending(doomed));
        process_pending();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(cancelled.load(Ordering::SeqCst), 0);
        // A callback may enqueue more work.
        let chained = Arc::new(AtomicUsize::new(0));
        {
            let chained = chained.clone();
            schedule_in(
                0,
                Box::new(move || {
                    let chained = chained.clone();
                    schedule_in(
                        0,
                        Box::new(move || {
                            chained.fetch_add(1, Ordering::SeqCst);
                        }),
                    );
                }),
            );
        }
        process_pending();
        process_pending();
        assert_eq!(chained.load(Ordering::SeqCst), 1);
    }
}
