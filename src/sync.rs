/*
 *
 *       src/sync.rs
 *       Local IRQ-safe spin locks and guard ownership
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

use crate::arch;

/// IRQ-safe spin lock. Never sleep or allocate while it is held.
///
/// Lock order: virtual memory -> physical memory. A physical-memory holder
/// must not acquire any virtual-memory or slab lock. Slab -> physical memory
/// is allowed. Logging has its own terminal lock and must never call an
/// allocator while holding it. PCI config locks may enter VMM -> PMM; no code
/// may acquire a PCI config lock while holding VMM, PMM, or the slab locks.
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinLock<T> {}

pub struct Guard<'a, T> {
    lock: &'a SpinLock<T>,
    flags: u64,
    // Restoring interrupt state on another CPU would corrupt that CPU's state.
    _not_send: PhantomData<*mut ()>,
}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Raw pointer to the value, for fields documented as immutable after
    /// construction. Dereferencing anything else needs the lock.
    pub const fn as_ptr(&self) -> *mut T {
        self.value.get()
    }

    pub fn lock(&self) -> Guard<'_, T> {
        let flags = loop {
            let flags = arch::irq_save();
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break flags;
            }
            // Permit shootdown/IPI delivery while another CPU owns this lock.
            // The successful owner still runs with local IRQs masked.
            arch::irq_restore(flags);
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        };
        Guard {
            lock: self,
            flags,
            _not_send: PhantomData,
        }
    }

    /// # Safety
    /// The caller owns a guard transferred with Guard::handoff on this CPU.
    /// No access to the protected value may remain when this is called.
    pub unsafe fn unlock_handoff(&self) {
        self.locked.store(false, Ordering::Release);
    }

    pub fn try_lock(&self) -> Option<Guard<'_, T>> {
        let flags = arch::irq_save();
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(Guard {
                lock: self,
                flags,
                _not_send: PhantomData,
            })
        } else {
            arch::irq_restore(flags);
            None
        }
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard is the sole owner while the lock is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard is the sole owner while the lock is held.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        arch::irq_restore(self.flags);
    }
}

impl<T> Guard<'_, T> {
    /// # Safety
    /// IRQs must already be masked. The incoming stack must release this exact
    /// lock before enabling IRQs. No guard or protected borrow may be used again.
    pub unsafe fn handoff(self) {
        core::mem::forget(self);
    }
}

/// Sleeping mutual exclusion for long critical sections such as filesystem I/O.
///
/// Only task context may contend on it, and no `SpinLock` may be held across
/// `lock()`: the waiter blocks on a scheduler wait queue. An uncontended lock
/// never touches the scheduler, so early-boot single-CPU callers are safe.
pub struct Mutex<T> {
    locked: AtomicBool,
    waiters: spin_once::Lazy<crate::sched::WaitQueue>,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    lock: &'a Mutex<T>,
    _not_send: PhantomData<*mut ()>,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            waiters: spin_once::Lazy::new(),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            if self
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return MutexGuard {
                    lock: self,
                    _not_send: PhantomData,
                };
            }
            self.waiters
                .get(crate::sched::WaitQueue::new)
                .wait_until(|| !self.locked.load(Ordering::Acquire));
        }
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard is the sole owner while the mutex is held.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard is the sole owner while the mutex is held.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if let Some(queue) = self.lock.waiters.try_get() {
            queue.wake_one();
        }
    }
}

/// One-time initialization without a dependency on the allocator being ready
/// at construction, so `Mutex::new` stays `const`.
mod spin_once {
    use core::{
        cell::UnsafeCell,
        sync::atomic::{AtomicU8, Ordering},
    };

    pub struct Lazy<T> {
        state: AtomicU8,
        value: UnsafeCell<Option<T>>,
    }

    unsafe impl<T: Send + Sync> Sync for Lazy<T> {}
    unsafe impl<T: Send> Send for Lazy<T> {}

    impl<T> Lazy<T> {
        pub const fn new() -> Self {
            Self {
                state: AtomicU8::new(0),
                value: UnsafeCell::new(None),
            }
        }

        pub fn get(&self, init: impl FnOnce() -> T) -> &T {
            if self
                .state
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                // SAFETY: the 0->1 transition grants exclusive initialization.
                unsafe { *self.value.get() = Some(init()) };
                self.state.store(2, Ordering::Release);
            }
            while self.state.load(Ordering::Acquire) != 2 {
                core::hint::spin_loop();
            }
            // SAFETY: state 2 publishes an initialized, never-mutated value.
            unsafe { (*self.value.get()).as_ref().unwrap() }
        }

        pub fn try_get(&self) -> Option<&T> {
            if self.state.load(Ordering::Acquire) == 2 {
                // SAFETY: as in `get`.
                unsafe { (*self.value.get()).as_ref() }
            } else {
                None
            }
        }
    }
}
