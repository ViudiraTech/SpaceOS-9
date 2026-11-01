/*
 *
 *       src/arch/x86_64/cpu.rs
 *       x86-64 interrupt state and architectural counter operations
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::arch::asm;

#[inline]
pub fn disable_interrupts() {
    // SAFETY: cli changes only the local CPU interrupt flag.
    unsafe { asm!("cli", options(nomem, nostack)) }
}

pub fn enable_interrupts() {
    // SAFETY: the BSP enables delivery only after IDT and APIC setup.
    unsafe { asm!("sti", options(nomem, nostack)) }
}

#[inline]
pub fn irqs_enabled() -> bool {
    let flags: u64;
    // SAFETY: reading RFLAGS has no side effects.
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags)) }
    flags & (1 << 9) != 0
}

#[inline]
pub fn irq_save() -> u64 {
    let flags: u64;
    // SAFETY: flags are captured and interrupts disabled on the same CPU.
    unsafe { asm!("pushfq", "pop {}", "cli", out(reg) flags, options(nomem)) }
    flags
}

#[inline]
pub fn irq_restore(flags: u64) {
    if flags & (1 << 9) != 0 {
        // SAFETY: restore IF only when it was enabled before the lock.
        unsafe { asm!("sti", options(nomem, nostack)) }
    }
}

#[inline]
pub fn halt() {
    // SAFETY: ring 0 halt with interrupts disabled.
    unsafe { asm!("hlt", options(nomem, nostack)) }
}

pub fn counter() -> u64 {
    // SAFETY: RDTSC has no memory side effects; Limine reports its frequency.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Reset or power off the machine. Never returns.
///
/// Restart pulses the keyboard controller reset line. Power off enters S5 through
/// the ACPI PM1a control block at the fixed port QEMU's chipsets decode; real
/// hardware that ignores it simply halts.
pub fn system_reset(restart: bool) -> ! {
    use x86_64::instructions::port::Port;
    crate::arch::disable_interrupts();
    // SAFETY: the machine is going down, and these ports only trigger reset or sleep.
    unsafe {
        if restart {
            Port::<u8>::new(0x64).write(0xFE);
        } else {
            Port::<u16>::new(0x604).write(0x2000);
            Port::<u16>::new(0xB004).write(0x2000);
        }
    }
    loop {
        halt();
    }
}
