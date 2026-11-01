/*
 *
 *       src/arch/riscv64/mod.rs
 *       RISC-V architecture backend interfaces
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

pub mod clint;
mod context;
mod scheduler;
pub(crate) use scheduler::*;
mod cpu;
pub mod cpuid;
mod exceptions;
pub mod fpu;
mod paging;
pub mod plic;
mod serial;

pub use context::{Context, context_init, switch};
pub use cpu::{
    counter, disable_interrupts, enable_interrupts, halt, irq_restore, irq_save, irqs_enabled, system_reset,
};
pub(crate) use exceptions::current_cpu_index;
pub use exceptions::{init_bsp as init_exceptions, load_cpu as load_exceptions};
pub use paging::{
    flush_page, flush_table, page_root, paging_geometry, pte_is_table, pte_leaf, pte_phys,
    pte_present, pte_table, set_page_root, tlb_shootdown,
};
pub use serial::{SERIAL_PHYS, serial_enable_irq, serial_init, serial_try_read, serial_write};

pub fn set_kernel_stack(cpu: usize, stack_top: u64) {
    exceptions::set_kernel_stack(cpu, stack_top);
}

pub unsafe fn enter_user_mode(entry: u64, user_sp: u64) -> ! {
    unsafe {
        let mut sstatus: u64;
        core::arch::asm!("csrr {}, sstatus", out(reg) sstatus, options(nomem, nostack));
        sstatus &= !(1 << 8); // SPP = 0 (U-mode)
        sstatus |= 1 << 5; // SPIE = 1 (enable interrupts in U-mode)
        sstatus |= 1 << 18; // SUM = 1 (permit S-mode to access user memory)
        core::arch::asm!(
            "csrc sstatus, 2",
            "csrw sstatus, {status}",
            "csrw sepc, {entry}",
            "mv sp, {sp}",
            "mv ra, zero",
            "mv gp, zero",
            "mv t0, zero",
            "mv t1, zero",
            "mv t2, zero",
            "mv t3, zero",
            "mv t4, zero",
            "mv t5, zero",
            "mv t6, zero",
            "mv s0, zero",
            "mv s1, zero",
            "mv s2, zero",
            "mv s3, zero",
            "mv s4, zero",
            "mv s5, zero",
            "mv s6, zero",
            "mv s7, zero",
            "mv s8, zero",
            "mv s9, zero",
            "mv s10, zero",
            "mv s11, zero",
            "mv a0, zero",
            "mv a1, zero",
            "mv a2, zero",
            "mv a3, zero",
            "mv a4, zero",
            "mv a5, zero",
            "mv a6, zero",
            "mv a7, zero",
            "sret",
            status = in(reg) sstatus,
            entry = in(reg) entry,
            sp = in(reg) user_sp,
            options(noreturn)
        );
    }
}
