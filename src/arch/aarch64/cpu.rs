/*
 *
 *       src/arch/aarch64/cpu.rs
 *       AArch64 interrupt state and architectural counter operations
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::arch::asm;

pub fn disable_interrupts() {
    // SAFETY: masks interrupts on the local processing element.
    unsafe { asm!("msr daifset, #0xf", options(nomem, nostack)) }
}

pub fn enable_interrupts() {
    // SAFETY: unmasks IRQ on the local processing element.
    unsafe { asm!("msr daifclr, #0x2", options(nomem, nostack)) }
}

pub fn irqs_enabled() -> bool {
    let flags: u64;
    // SAFETY: reading DAIF has no side effects.
    unsafe { asm!("mrs {}, daif", out(reg) flags, options(nomem, nostack)) }
    flags & (1 << 7) == 0
}

pub fn irq_save() -> u64 {
    let flags: u64;
    // SAFETY: capture and mask interrupts on the same processing element.
    unsafe { asm!("mrs {}, daif", "msr daifset, #0xf", out(reg) flags, options(nomem, nostack)) }
    flags
}

pub fn irq_restore(flags: u64) {
    // SAFETY: flags were captured by irq_save on this processing element.
    unsafe { asm!("msr daif, {}", in(reg) flags, options(nomem, nostack)) }
}

pub fn halt() {
    // SAFETY: WFE is usable at the privilege level supplied by Limine.
    unsafe { asm!("wfe", options(nomem, nostack)) }
}

pub fn counter() -> u64 {
    let value: u64;
    // SAFETY: Limine runs the kernel with the architectural counter accessible.
    unsafe { asm!("mrs {}, cntpct_el0", out(reg) value, options(nomem, nostack)) }
    value
}

/// Reset or power off the machine. Without a platform reset driver this halts.
pub fn system_reset(_restart: bool) -> ! {
    crate::arch::disable_interrupts();
    loop {
        halt();
    }
}
