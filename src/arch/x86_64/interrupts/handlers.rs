/*
 *
 *       src/arch/x86_64/interrupts/handlers.rs
 *       Fault reporting is lock independent and does not allocate
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Fault reporting is lock independent and does not allocate.
use crate::{arch, printk};
use x86_64::{
    registers::control::Cr2,
    structures::idt::{InterruptStackFrame, PageFaultErrorCode},
};

fn user_entry(frame: &InterruptStackFrame) -> bool {
    let user = frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3;
    if user {
        unsafe {
            core::arch::asm!("swapgs", "lfence", options(nostack));
        }
    }
    user
}
fn user_return(user: bool) {
    if user {
        unsafe {
            core::arch::asm!("swapgs", options(nostack));
        }
    }
}
fn fatal_user(name: &str, frame: InterruptStackFrame, code: Option<u64>, signal: u32) -> ! {
    // Interrupt entry from user mode leaves the kernel GS base parked.
    unsafe {
        core::arch::asm!("swapgs", "lfence", options(nostack));
    }
    crate::fpu::disable_current();
    crate::kwarn!(
        "user fault: {} code={:?} ip={:?}",
        name,
        code,
        frame.instruction_pointer
    );
    crate::syscall::set_last_user_exit_code(128 + signal as i32);
    crate::signal::force_fatal(signal, true)
}

fn fatal(name: &str, frame: InterruptStackFrame, code: Option<u64>) -> ! {
    if user_entry(&frame) {
        fatal_user(name, frame, code, crate::signal::SIGSEGV);
    }
    arch::disable_interrupts();
    printk::emergency(format_args!("{name} code={code:?} frame={frame:#?}"));
    loop {
        arch::halt();
    }
}

macro_rules! fatal_no_code {
    ($name:ident, $label:literal, $signal:expr) => {
        pub(super) extern "x86-interrupt" fn $name(frame: InterruptStackFrame) {
            if frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
                fatal_user($label, frame, None, $signal);
            }
            fatal($label, frame, None)
        }
    };
}

macro_rules! fatal_with_code {
    ($name:ident, $label:literal, $signal:expr) => {
        pub(super) extern "x86-interrupt" fn $name(frame: InterruptStackFrame, code: u64) {
            if frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
                fatal_user($label, frame, Some(code), $signal);
            }
            fatal($label, frame, Some(code))
        }
    };
}

fatal_no_code!(divide_error, "#DE divide error", crate::signal::SIGFPE);
fatal_no_code!(bound_range, "#BR bound range", crate::signal::SIGSEGV);
fatal_no_code!(invalid_opcode, "#UD invalid opcode", crate::signal::SIGILL);
fatal_no_code!(
    device_not_available,
    "#NM device unavailable",
    crate::signal::SIGILL
);
fatal_no_code!(
    x87_floating_point,
    "#MF floating point",
    crate::signal::SIGFPE
);
fatal_no_code!(
    simd_floating_point,
    "#XM SIMD floating point",
    crate::signal::SIGFPE
);
fatal_no_code!(virtualization, "#HV virtualization", crate::signal::SIGSEGV);
fatal_no_code!(
    unhandled_interrupt,
    "unhandled interrupt",
    crate::signal::SIGSEGV
);

pub(super) extern "x86-interrupt" fn external<const VECTOR: u8>(frame: InterruptStackFrame) {
    let user = user_entry(&frame);
    crate::fpu::disable_current();
    crate::arch::apic::dispatch(VECTOR);
    crate::sched::irq_exit();
    if user {
        unsafe {
            let _ = crate::fpu::enable_user();
        }
    }
    user_return(user);
}
fatal_with_code!(invalid_tss, "#TS invalid TSS", crate::signal::SIGSEGV);
fatal_with_code!(
    segment_not_present,
    "#NP segment absent",
    crate::signal::SIGSEGV
);
fatal_with_code!(
    stack_segment_fault,
    "#SS stack segment",
    crate::signal::SIGSEGV
);
fatal_with_code!(
    general_protection,
    "#GP general protection",
    crate::signal::SIGSEGV
);
fatal_with_code!(alignment_check, "#AC alignment", crate::signal::SIGBUS);
fatal_with_code!(
    cp_protection,
    "#CP control protection",
    crate::signal::SIGSEGV
);

pub(super) extern "x86-interrupt" fn debug_trap(frame: InterruptStackFrame) {
    if frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        fatal_user("#DB debug trap", frame, None, crate::signal::SIGTRAP);
    }
    printk::emergency(format_args!("#DB debug trap frame={frame:#?}"));
}

pub(super) extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    if frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        fatal_user("#BP breakpoint", frame, None, crate::signal::SIGTRAP);
    }
    printk::emergency(format_args!("#BP breakpoint frame={frame:#?}"));
}

pub(super) extern "x86-interrupt" fn overflow(frame: InterruptStackFrame) {
    if frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        fatal_user("#OF overflow", frame, None, crate::signal::SIGSEGV);
    }
    printk::emergency(format_args!("#OF overflow frame={frame:#?}"));
}

pub(super) extern "x86-interrupt" fn nmi(frame: InterruptStackFrame) {
    printk::emergency(format_args!("NMI frame={frame:#?}"));
}

pub(super) extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, code: u64) -> ! {
    fatal("#DF double fault", frame, Some(code))
}

pub(super) extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    fatal("#MC machine check", frame, None)
}

pub(super) extern "x86-interrupt" fn page_fault(
    frame: InterruptStackFrame,
    code: PageFaultErrorCode,
) {
    if frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3 {
        // Interrupt entry from user mode does not perform SWAPGS.
        unsafe { core::arch::asm!("swapgs", "lfence", options(nostack)) };
        let address = Cr2::read().map(|address| address.as_u64()).unwrap_or(0);
        // File-backed pages may sleep on filesystem locks, so this path follows
        // the same FPU ownership and interrupt rules as a syscall.
        crate::fpu::disable_current();
        arch::enable_interrupts();
        let handled = crate::sched::current()
            .and_then(|task| task.address_space())
            .is_some_and(|space| {
                crate::user::uaccess::resolve_fault(
                    &space,
                    address,
                    code.contains(PageFaultErrorCode::CAUSED_BY_WRITE),
                    code.contains(PageFaultErrorCode::INSTRUCTION_FETCH),
                )
                .unwrap_or(false)
            });
        arch::disable_interrupts();
        if handled {
            // SAFETY: this task is current and returns to user mode below.
            unsafe {
                let _ = crate::fpu::enable_user();
                core::arch::asm!("swapgs", options(nostack));
            }
            return;
        }
        crate::fpu::disable_current();
        crate::kwarn!(
            "user fault: #PF address={:#x} code={:?} ip={:?}",
            address,
            code,
            frame.instruction_pointer
        );
        crate::user::uaccess::report_fatal_fault(
            address,
            frame.instruction_pointer.as_u64(),
            frame.stack_pointer.as_u64(),
        );
        crate::syscall::set_last_user_exit_code(128 + crate::signal::SIGSEGV as i32);
        crate::signal::force_fatal(crate::signal::SIGSEGV, true);
    }
    arch::disable_interrupts();
    printk::emergency(format_args!(
        "#PF address={:?} code={code:?} frame={frame:#?}",
        Cr2::read()
    ));
    loop {
        arch::halt();
    }
}
