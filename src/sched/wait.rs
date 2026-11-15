/*
 *
 *       src/sched/wait.rs
 *       Wait queues: condition waits with interruptible and timed variants
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux `wait_queue_head` / `wait_event` semantics on top of the scheduler's
//! `prepare_wait` / `park` / `wake_task` primitives.
//!
//! A waiter runs `prepare_wait`, publishes itself on the queue, then checks its
//! condition and parks. A waker publishes the condition first and then wakes
//! queued tasks. Every interleaving ends the wait: a wake before `park` leaves
//! a token that makes `park` return, and a wake after it unparks the task.
//!
//! Conditions run with no scheduler lock held, so they may take ordinary
//! locks (for example the object's own state lock). They must not sleep.
//! Signals reach interruptible waiters directly: signal delivery calls
//! `wake_task` on the target thread.

use super::{TaskRef, park, prepare_wait, wake_task};
use crate::sync::SpinLock;
use alloc::{collections::VecDeque, sync::Arc};

/// Why a wait returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitResult {
    /// The condition became true.
    Ready,
    /// A signal is pending for the current task (interruptible waits only).
    Interrupted,
    /// The deadline passed before the condition became true.
    TimedOut,
}

struct Waiter {
    tid: u64,
    task: TaskRef,
}

/// Linux `wait_queue_head`. Const-constructible so global events can use a
/// `static` queue; per-object queues usually use the shared `WaitQueue`.
pub struct WaitQueueHead {
    waiters: SpinLock<VecDeque<Waiter>>,
}

/// A reference-counted `WaitQueueHead` that objects can clone out of their
/// own locks before sleeping on it.
#[derive(Clone)]
pub struct WaitQueue(Arc<WaitQueueHead>);

impl WaitQueue {
    pub fn new() -> Self {
        Self(Arc::new(WaitQueueHead::new()))
    }
}

impl core::ops::Deref for WaitQueue {
    type Target = WaitQueueHead;

    fn deref(&self) -> &WaitQueueHead {
        &self.0
    }
}

impl WaitQueueHead {
    pub const fn new() -> Self {
        Self {
            waiters: SpinLock::new(VecDeque::new()),
        }
    }

    /// Uninterruptible wait until `ready` holds.
    pub fn wait_until(&self, ready: impl FnMut() -> bool) {
        let _ = self.wait_event(ready, false, None);
    }

    /// Wait until `ready` holds or a signal is pending.
    pub fn wait_interruptible(&self, ready: impl FnMut() -> bool) -> WaitResult {
        self.wait_event(ready, true, None)
    }

    /// General form: optional signal interruption and an absolute deadline on
    /// the uptime clock in microseconds.
    pub fn wait_event(
        &self,
        mut ready: impl FnMut() -> bool,
        interruptible: bool,
        deadline: Option<u64>,
    ) -> WaitResult {
        let Some(me) = super::current() else {
            // Before the scheduler runs there is nothing to switch to.
            while !ready() {
                core::hint::spin_loop();
            }
            return WaitResult::Ready;
        };
        let tid = me.tid();
        loop {
            prepare_wait(&me);
            self.enqueue(tid, &me);
            if ready() {
                self.dequeue(tid);
                return WaitResult::Ready;
            }
            if interruptible && crate::signal::pending_for_current() {
                return self.abandon(tid, WaitResult::Interrupted);
            }
            if deadline.is_some_and(|deadline| crate::time::uptime_micros() >= deadline) {
                return self.abandon(tid, WaitResult::TimedOut);
            }
            park(deadline);
        }
    }

    /// Wake the longest waiter. Returns whether there was one.
    pub fn wake_one(&self) -> bool {
        let waiter = self.waiters.lock().pop_front();
        match waiter {
            Some(waiter) => {
                wake_task(&waiter.task);
                true
            }
            None => false,
        }
    }

    /// Wake every current waiter. Tasks that re-queue during this call are
    /// not woken again by it.
    pub fn wake_all(&self) -> usize {
        let batch = core::mem::take(&mut *self.waiters.lock());
        let count = batch.len();
        for waiter in batch {
            wake_task(&waiter.task);
        }
        count
    }

    pub fn len(&self) -> usize {
        self.waiters.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn enqueue(&self, tid: u64, task: &TaskRef) {
        let mut waiters = self.waiters.lock();
        if !waiters.iter().any(|waiter| waiter.tid == tid) {
            waiters.push_back(Waiter {
                tid,
                task: task.clone(),
            });
        }
    }

    /// Remove `tid`; false when a waker already took it off the queue.
    fn dequeue(&self, tid: u64) -> bool {
        let mut waiters = self.waiters.lock();
        match waiters.iter().position(|waiter| waiter.tid == tid) {
            Some(index) => {
                waiters.remove(index);
                true
            }
            None => false,
        }
    }

    /// Leave without consuming the condition. If `wake_one` already picked
    /// this task, pass that wakeup on so another waiter does not miss it.
    fn abandon(&self, tid: u64, result: WaitResult) -> WaitResult {
        if !self.dequeue(tid) {
            self.wake_one();
        }
        result
    }
}

/// Linux `struct completion`: a one-shot event that any number of tasks can
/// wait for. Used for vfork, where the parent sleeps until the child stops
/// borrowing its address space.
pub struct Completion {
    done: core::sync::atomic::AtomicBool,
    queue: WaitQueue,
}

impl Completion {
    pub fn new() -> Self {
        Self {
            done: core::sync::atomic::AtomicBool::new(false),
            queue: WaitQueue::new(),
        }
    }

    pub fn complete(&self) {
        self.done.store(true, core::sync::atomic::Ordering::Release);
        self.queue.wake_all();
    }

    pub fn is_done(&self) -> bool {
        self.done.load(core::sync::atomic::Ordering::Acquire)
    }

    pub fn wait(&self) {
        self.queue.wait_until(|| self.is_done());
    }
}

impl Default for Completion {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for WaitQueueHead {
    fn default() -> Self {
        Self::new()
    }
}
