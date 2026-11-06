/*
 *
 *       src/arch/aarch64/mod.rs
 *       AArch64 architecture backend interfaces
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

mod context;
mod scheduler;
pub(crate) use scheduler::*;
mod cpu;
pub mod cpuid;
mod exceptions;
pub mod fpu;
pub mod gic;
mod paging;
mod serial;

pub use context::{Context, context_init, switch};
pub use cpu::{
    counter, disable_interrupts, enable_interrupts, halt, irq_restore, irq_save, irqs_enabled,
    system_reset,
};
pub use exceptions::{init_bsp as init_exceptions, load_cpu as load_exceptions};
pub use paging::{
    flush_page, flush_table, page_root, paging_geometry, pte_is_table, pte_kernel, pte_leaf,
    pte_phys, pte_present, pte_table, set_page_root, setup_device_memory, sync_instruction_cache,
    tlb_shootdown,
};
pub use serial::{SERIAL_PHYS, serial_enable_irq, serial_init, serial_try_read, serial_write};

pub(crate) use exceptions::current_cpu_index;

pub fn set_kernel_stack(_cpu: usize, _stack_top: u64) {}

pub unsafe fn enter_user_mode(entry: u64, user_sp: u64) -> ! {
    unsafe {
        core::arch::asm!(
            "msr daifset, #0xf",
            "msr elr_el1, {entry}",
            "msr sp_el0, {sp}",
            "msr spsr_el1, xzr",
            "isb",
            "mov x0, xzr",
            "mov x1, xzr",
            "mov x2, xzr",
            "mov x3, xzr",
            "mov x4, xzr",
            "mov x5, xzr",
            "mov x6, xzr",
            "mov x7, xzr",
            "mov x8, xzr",
            "mov x9, xzr",
            "mov x10, xzr",
            "mov x11, xzr",
            "mov x12, xzr",
            "mov x13, xzr",
            "mov x14, xzr",
            "mov x15, xzr",
            "mov x16, xzr",
            "mov x17, xzr",
            "mov x18, xzr",
            "mov x19, xzr",
            "mov x20, xzr",
            "mov x21, xzr",
            "mov x22, xzr",
            "mov x23, xzr",
            "mov x24, xzr",
            "mov x25, xzr",
            "mov x26, xzr",
            "mov x27, xzr",
            "mov x28, xzr",
            "mov x29, xzr",
            "mov x30, xzr",
            "eret",
            entry = in(reg) entry,
            sp = in(reg) user_sp,
            options(noreturn)
        );
    }
}
