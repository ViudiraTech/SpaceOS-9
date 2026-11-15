/*
 *
 *       src/ipc/futex.rs
 *       Linux futex wait/wake/requeue subset for threads and shared memory
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Futex keys are `(address-space root, virtual address)`, which matches
//! Linux's private futexes and all CLONE_VM threads.  A global queue lock is
//! held across the value re-check and enqueue, and wake also takes it, so a
//! wake racing with the wait cannot be lost.  A waiter calls `prepare_wait`
//! before it is published, so a wake that lands before it parks leaves a
//! token and its `park` returns at once; later wakes unpark it directly.

use crate::{sched, sync::SpinLock, user::uaccess, vfs::abi};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const FUTEX_WAIT: u32 = 0;
const FUTEX_WAKE: u32 = 1;
const FUTEX_REQUEUE: u32 = 3;
const FUTEX_CMP_REQUEUE: u32 = 4;
const FUTEX_WAKE_OP: u32 = 5;
const FUTEX_WAIT_BITSET: u32 = 9;
const FUTEX_WAKE_BITSET: u32 = 10;

const FUTEX_PRIVATE_FLAG: u32 = 128;
const FUTEX_CLOCK_REALTIME: u32 = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Key {
    space: u64,
    address: u64,
}

struct Waiter {
    id: u64,
    key: Key,
    bitset: u32,
    woken: Arc<AtomicBool>,
    task: sched::TaskRef,
}

/// Wake tasks that were taken off the waiter list. Runs after the futex
/// lock is released because `wake_task` takes scheduler locks.
fn wake_tasks(tasks: Vec<sched::TaskRef>) {
    for task in tasks {
        sched::wake_task(&task);
    }
}

static WAITERS: SpinLock<Vec<Waiter>> = SpinLock::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn key_for(address: u64) -> Result<Key, i64> {
    if address & 3 != 0 {
        return Err(-abi::EINVAL);
    }
    let space = uaccess::current_space().map_err(|error| error.to_errno())?;
    let space = space.lock();
    let (space_id, address) = space.futex_key(address).ok_or(-abi::EFAULT)?;
    Ok(Key {
        space: space_id,
        address,
    })
}

fn parse_timeout(pointer: u64, absolute: bool) -> Result<Option<u64>, i64> {
    if pointer == 0 {
        return Ok(None);
    }
    let timestamp = uaccess::read_value::<[u64; 2]>(pointer).map_err(|e| e.to_errno())?;
    let [seconds, nanos] = timestamp;
    if seconds > i64::MAX as u64 || nanos >= 1_000_000_000 {
        return Err(-abi::EINVAL);
    }
    let micros = seconds
        .saturating_mul(1_000_000)
        .saturating_add(nanos.div_ceil(1_000));
    let now = crate::time::uptime_micros();
    Ok(Some(if absolute {
        micros
    } else {
        now.saturating_add(micros)
    }))
}

fn remove(id: u64) {
    WAITERS.lock().retain(|waiter| waiter.id != id);
}

fn wait(
    address: u64,
    expected: u32,
    timeout: u64,
    bitset: u32,
    absolute: bool,
    realtime: bool,
) -> i64 {
    if bitset == 0 {
        return -abi::EINVAL;
    }
    if realtime {
        // No wall-clock source exists yet; do not pretend CLOCK_REALTIME
        // deadlines work.
        return -abi::ENOSYS;
    }
    let key = match key_for(address) {
        Ok(key) => key,
        Err(error) => return error,
    };
    let deadline = match parse_timeout(timeout, absolute) {
        Ok(deadline) => deadline,
        Err(error) => return error,
    };
    let current = match sched::current() {
        Some(task) => task,
        None => return -abi::ESRCH,
    };
    let woken = Arc::new(AtomicBool::new(false));
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    sched::prepare_wait(&current);
    {
        let mut waiters = WAITERS.lock();
        let value = match uaccess::read_value::<u32>(address) {
            Ok(value) => value,
            Err(error) => return error.to_errno(),
        };
        if value != expected {
            return -abi::EAGAIN;
        }
        waiters.push(Waiter {
            id,
            key,
            bitset,
            woken: woken.clone(),
            task: current.clone(),
        });
    }
    loop {
        if woken.load(Ordering::Acquire) {
            return 0;
        }
        // On EINTR/ETIMEDOUT, a wake that raced the removal still counts as
        // success so that the waker's wakeup is not lost.
        if crate::signal::pending_for_current() {
            remove(id);
            return if woken.load(Ordering::Acquire) {
                0
            } else {
                -abi::EINTR
            };
        }
        if deadline.is_some_and(|deadline| crate::time::uptime_micros() >= deadline) {
            remove(id);
            return if woken.load(Ordering::Acquire) {
                0
            } else {
                -abi::ETIMEDOUT
            };
        }
        sched::park(deadline);
    }
}

/// Wake at most `count` waiters on `key` whose bitsets intersect `bitset`.
pub fn wake_key(key: Key, bitset: u32, count: u32) -> u32 {
    if count == 0 || bitset == 0 {
        return 0;
    }
    let mut tasks = Vec::new();
    {
        let mut waiters = WAITERS.lock();
        let mut index = 0;
        while index < waiters.len() && (tasks.len() as u32) < count {
            if waiters[index].key == key && waiters[index].bitset & bitset != 0 {
                let waiter = waiters.remove(index);
                waiter.woken.store(true, Ordering::Release);
                tasks.push(waiter.task);
            } else {
                index += 1;
            }
        }
    }
    let woken = tasks.len() as u32;
    wake_tasks(tasks);
    woken
}

/// Called after CLONE_CHILD_CLEARTID publishes zero so `pthread_join` waiters
/// observe it.
pub fn wake_cleared(root_physical: u64, address: u64, count: u32) {
    let _ = wake_key(
        Key {
            space: root_physical,
            address,
        },
        !0,
        count,
    );
}

fn wake(address: u64, count: u32, bitset: u32) -> i64 {
    if bitset == 0 {
        return -abi::EINVAL;
    }
    let key = match key_for(address) {
        Ok(key) => key,
        Err(error) => return error,
    };
    wake_key(key, bitset, count) as i64
}

fn requeue(
    address: u64,
    count: u32,
    requeue_count: u32,
    target_address: u64,
    compare: Option<u32>,
    bitset: u32,
) -> i64 {
    let key = match key_for(address) {
        Ok(key) => key,
        Err(error) => return error,
    };
    let target = match key_for(target_address) {
        Ok(key) => key,
        Err(error) => return error,
    };
    let mut waiters = WAITERS.lock();
    if let Some(expected) = compare {
        let value = match uaccess::read_value::<u32>(address) {
            Ok(value) => value,
            Err(error) => return error.to_errno(),
        };
        if value != expected {
            return -abi::EAGAIN;
        }
    }
    let mut tasks = Vec::new();
    let mut index = 0;
    while index < waiters.len() && (tasks.len() as u32) < count {
        if waiters[index].key == key && waiters[index].bitset & bitset != 0 {
            let waiter = waiters.remove(index);
            waiter.woken.store(true, Ordering::Release);
            tasks.push(waiter.task);
        } else {
            index += 1;
        }
    }
    let woken = tasks.len() as u32;
    let mut moved = 0;
    for waiter in waiters.iter_mut() {
        if moved >= requeue_count {
            break;
        }
        if waiter.key == key && waiter.bitset & bitset != 0 {
            waiter.key = target;
            moved += 1;
        }
    }
    drop(waiters);
    wake_tasks(tasks);
    (woken + moved) as i64
}

pub fn futex(address: u64, op: u32, value: u32, timeout: u64, address2: u64, value3: u32) -> i64 {
    let command = op & 0x7f;
    let flags = op & !0x7f;
    if flags & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME) != 0 {
        return -abi::EINVAL;
    }
    if flags & FUTEX_CLOCK_REALTIME != 0 && !matches!(command, FUTEX_WAIT_BITSET) {
        return -abi::ENOSYS;
    }
    let realtime = flags & FUTEX_CLOCK_REALTIME != 0;
    match command {
        FUTEX_WAIT => wait(address, value, timeout, !0, false, realtime),
        FUTEX_WAIT_BITSET => wait(address, value, timeout, value3, true, realtime),
        FUTEX_WAKE => wake(address, value, !0),
        FUTEX_WAKE_BITSET => wake(address, value, value3),
        FUTEX_REQUEUE => requeue(address, value, timeout as u32, address2, None, !0),
        FUTEX_CMP_REQUEUE => requeue(address, value, timeout as u32, address2, Some(value3), !0),
        FUTEX_WAKE_OP => -abi::ENOSYS,
        _ => -abi::ENOSYS,
    }
}
