/*
 *
 *       src/arch/x86_64/mod.rs
 *       x86-64 architecture backend interfaces
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

pub mod apic;
mod context;
mod scheduler;
pub(crate) use scheduler::*;
mod cpu;
pub mod cpuid;
pub mod fpu;
mod interrupts;
mod paging;
mod serial;
mod signal;
pub mod syscall;

pub use context::{Context, context_init, switch};
pub use cpu::{
    counter, disable_interrupts, enable_interrupts, halt, irq_restore, irq_save, irqs_enabled, system_reset,
};
pub use interrupts::{init_bsp as init_exceptions, load_cpu as load_exceptions};
pub use paging::setup_memory_protection;
pub use paging::{
    flush_page, flush_table, page_root, paging_geometry, pte_is_table, pte_leaf, pte_phys,
    pte_present, pte_table, set_page_root, tlb_shootdown,
};
pub use scheduler::set_kernel_stack;
pub use serial::{serial_enable_irq, serial_init, serial_try_read, serial_write};
#[allow(unused_imports)]
pub use signal::{GprFrame, InterruptFrame, SyscallFrame, spacekernel_x86_64_timer_stub};
pub use syscall::enter_user_mode;
