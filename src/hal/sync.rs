/*
 *
 *       src/hal/sync.rs
 *       HAL completions and sleeping mutexes for drivers
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux `struct completion` (counting: each `complete` satisfies one wait)
//! and `struct mutex`, built on the scheduler's wait queues. `spk_complete`
//! is safe from interrupt handlers; waiting and locking are process-context
//! only.

use crate::sched::{WaitQueue, WaitResult};
use alloc::boxed::Box;
use core::ffi::{c_int, c_ulong};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub struct Completion {
    done: AtomicU32,
    wait: WaitQueue,
}

pub extern "C" fn spk_completion_alloc() -> *mut Completion {
    Box::into_raw(Box::new(Completion {
        done: AtomicU32::new(0),
        wait: WaitQueue::new(),
    }))
}

/// # Safety
/// `completion` came from `spk_completion_alloc` and nobody waits on it.
pub unsafe extern "C" fn spk_completion_free(completion: *mut Completion) {
    if !completion.is_null() {
        // SAFETY: caller contract.
        drop(unsafe { Box::from_raw(completion) });
    }
}

/// # Safety
/// `completion` is a live completion.
pub unsafe extern "C" fn spk_completion_reinit(completion: *mut Completion) {
    // SAFETY: caller contract.
    if let Some(completion) = unsafe { completion.as_ref() } {
        completion.done.store(0, Ordering::Release);
    }
}

/// # Safety
/// `completion` is a live completion. Callable from interrupt context.
pub unsafe extern "C" fn spk_complete(completion: *mut Completion) {
    // SAFETY: caller contract.
    if let Some(completion) = unsafe { completion.as_ref() } {
        completion.done.fetch_add(1, Ordering::AcqRel);
        completion.wait.wake_all();
    }
}

/// Returns 1 when a completion was consumed, 0 on timeout.
///
/// # Safety
/// `completion` is a live completion; process context only.
pub unsafe extern "C" fn spk_wait_for_completion_timeout(
    completion: *mut Completion,
    timeout_ms: c_ulong,
) -> c_int {
    // SAFETY: caller contract.
    let Some(completion) = (unsafe { completion.as_ref() }) else {
        return 0;
    };
    let deadline =
        crate::time::uptime_micros().saturating_add((timeout_ms as u64).saturating_mul(1000));
    loop {
        // Consume one completion if available.
        let mut current = completion.done.load(Ordering::Acquire);
        while current > 0 {
            match completion.done.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return 1,
                Err(seen) => current = seen,
            }
        }
        let result = completion.wait.wait_event(
            || completion.done.load(Ordering::Acquire) > 0,
            false,
            Some(deadline),
        );
        if result == WaitResult::TimedOut && completion.done.load(Ordering::Acquire) == 0 {
            return 0;
        }
    }
}

pub struct Mutex {
    locked: AtomicBool,
    wait: WaitQueue,
}

pub extern "C" fn spk_mutex_alloc() -> *mut Mutex {
    Box::into_raw(Box::new(Mutex {
        locked: AtomicBool::new(false),
        wait: WaitQueue::new(),
    }))
}

/// # Safety
/// `mutex` came from `spk_mutex_alloc` and is unlocked.
pub unsafe extern "C" fn spk_mutex_free(mutex: *mut Mutex) {
    if !mutex.is_null() {
        // SAFETY: caller contract.
        drop(unsafe { Box::from_raw(mutex) });
    }
}

/// # Safety
/// `mutex` is live; process context only; not recursive.
pub unsafe extern "C" fn spk_mutex_lock(mutex: *mut Mutex) {
    // SAFETY: caller contract.
    let Some(mutex) = (unsafe { mutex.as_ref() }) else {
        return;
    };
    while mutex
        .locked
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        mutex
            .wait
            .wait_until(|| !mutex.locked.load(Ordering::Acquire));
    }
}

/// # Safety
/// `mutex` is live and held by the caller.
pub unsafe extern "C" fn spk_mutex_unlock(mutex: *mut Mutex) {
    // SAFETY: caller contract.
    if let Some(mutex) = unsafe { mutex.as_ref() } {
        mutex.locked.store(false, Ordering::Release);
        mutex.wait.wake_one();
    }
}
