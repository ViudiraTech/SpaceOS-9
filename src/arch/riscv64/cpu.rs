/*
 *
 *       src/arch/riscv64/cpu.rs
 *       RISC-V interrupt state and architectural counter operations
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::arch::asm;

pub fn disable_interrupts() {
    // SAFETY: clear SIE on this hart.
    unsafe { asm!("csrci sstatus, 2", options(nomem, nostack)) }
}

pub fn enable_interrupts() {
    // SAFETY: set SIE on this hart.
    unsafe { asm!("csrsi sstatus, 2", options(nomem, nostack)) }
}

pub fn irqs_enabled() -> bool {
    let flags: u64;
    // SAFETY: reading sstatus has no side effects.
    unsafe { asm!("csrr {}, sstatus", out(reg) flags, options(nomem, nostack)) }
    flags & 2 != 0
}

pub fn irq_save() -> u64 {
    let flags: u64;
    // SAFETY: atomic CSR exchange saves the previous SIE bit.
    unsafe {
        asm!("csrrc {}, sstatus, {}", out(reg) flags, in(reg) 2usize, options(nomem, nostack))
    }
    flags
}

pub fn irq_restore(flags: u64) {
    if flags & 2 != 0 {
        // SAFETY: re-enable SIE only if it was set before the lock.
        unsafe { asm!("csrsi sstatus, 2", options(nomem, nostack)) }
    }
}

pub fn halt() {
    // SAFETY: WFI is valid in S mode.
    unsafe { asm!("wfi", options(nomem, nostack)) }
}

pub fn counter() -> u64 {
    let value: u64;
    // SAFETY: rdtime is available to S mode in the Limine boot environment.
    unsafe { asm!("rdtime {}", out(reg) value, options(nomem, nostack)) }
    value
}

/// Reset or power off the machine. Without a platform reset driver this halts.
pub fn system_reset(_restart: bool) -> ! {
    crate::arch::disable_interrupts();
    loop {
        halt();
    }
}
