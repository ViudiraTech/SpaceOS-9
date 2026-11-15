/*
 *
 *       src/irq/mod.rs
 *       Architecture independent synchronous interrupt dispatch
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Architecture independent synchronous interrupt dispatch.
//!
//! Controller drivers own routing and acknowledgement. A handler is called with
//! interrupts masked on the current CPU and must not sleep.

use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

pub type Handler = fn(u32);
pub const MAX_IRQS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqError {
    Invalid,
    InUse,
    NotRegistered,
    Unsupported,
    NoController,
    NoVector,
    Timeout,
    Mapping,
}

/// Handler table read without a lock on every interrupt (Linux `irq_desc`
/// lookup): a null slot is free; `register` claims one with compare-exchange.
static HANDLERS: [AtomicPtr<()>; MAX_IRQS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_IRQS];
static COUNTS: [AtomicU64; MAX_IRQS] = [const { AtomicU64::new(0) }; MAX_IRQS];
static UNHANDLED: AtomicU64 = AtomicU64::new(0);

pub fn register(irq: u32, handler: Handler) -> Result<(), IrqError> {
    let slot = HANDLERS.get(irq as usize).ok_or(IrqError::Invalid)?;
    slot.compare_exchange(
        core::ptr::null_mut(),
        handler as *mut (),
        Ordering::AcqRel,
        Ordering::Acquire,
    )
    .map(|_| ())
    .map_err(|_| IrqError::InUse)
}

/// The caller must mask the source and synchronize any in-flight handler first.
pub fn unregister(irq: u32) -> Result<(), IrqError> {
    let slot = HANDLERS.get(irq as usize).ok_or(IrqError::Invalid)?;
    if slot.swap(core::ptr::null_mut(), Ordering::AcqRel).is_null() {
        Err(IrqError::NotRegistered)
    } else {
        Ok(())
    }
}

pub fn dispatch(irq: u32) {
    let raw = HANDLERS
        .get(irq as usize)
        .map_or(core::ptr::null_mut(), |slot| slot.load(Ordering::Acquire));
    // Interrupt arrival times feed the entropy pool (Linux
    // add_interrupt_randomness); cheap, lock-free and per-CPU.
    crate::random::add_interrupt_randomness(irq);
    if raw.is_null() {
        UNHANDLED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    COUNTS[irq as usize].fetch_add(1, Ordering::Relaxed);
    // SAFETY: non-null slots only ever hold a `Handler` stored by `register`.
    let handler: Handler = unsafe { core::mem::transmute::<*mut (), Handler>(raw) };
    handler(irq);
}

pub fn count(irq: u32) -> Option<u64> {
    COUNTS.get(irq as usize).map(|n| n.load(Ordering::Relaxed))
}

pub fn unhandled_count() -> u64 {
    UNHANDLED.load(Ordering::Relaxed)
}
