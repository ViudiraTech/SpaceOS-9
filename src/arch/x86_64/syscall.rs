/*
 *
 *       src/arch/x86_64/syscall.rs
 *       Fast syscall (LSTAR) handling and user privilege transition for x86-64
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Fast system call MSR initialization and assembly entry.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text
    .global spacekernel_x86_64_syscall_entry
spacekernel_x86_64_syscall_entry:
    swapgs
    mov gs:[16], rsp
    mov rsp, gs:[8]
    /* Separate IRET state from RCX/R11: rt_sigreturn must restore both
       registers when an asynchronous signal interrupted ordinary code. */
    push 0x1b
    push qword ptr gs:[16]
    push r11
    push 0x23
    push rcx
    push rax
    push rdi
    push rsi
    push rdx
    push r10
    push r8
    push r9
    push rcx
    push r11
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    mov gs:[24], rsp
    mov rdi, rsp
    call spacekernel_syscall_c
    mov qword ptr gs:[24], 0
    test eax, eax
    jnz .Lsyscall_iret
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    pop r11
    pop rcx
    pop r9
    pop r8
    pop r10
    pop rdx
    pop rsi
    pop rdi
    pop rax
    add rsp, 24              /* skip RIP, CS, RFLAGS */
    pop rsp
    swapgs
    sysretq
.Lsyscall_iret:
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    pop r11
    pop rcx
    pop r9
    pop r8
    pop r10
    pop rdx
    pop rsi
    pop rdi
    pop rax
    swapgs
    iretq
"#
);

unsafe extern "C" {
    fn spacekernel_x86_64_syscall_entry();
}

pub fn init_syscall_msrs() {
    let (low, high): (u32, u32);
    // 1. Enable SCE (bit 0) in IA32_EFER (0xc000_0080)
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") 0xc000_0080u32,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack)
        );
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xc000_0080u32,
            in("eax") low | 1,
            in("edx") high,
            options(nostack)
        );
    }

    // 2. IA32_STAR (0xc000_0081):
    // Bits 47:32 = Kernel CS (0x08), Kernel SS (0x10)
    // Bits 63:48 = User Base (0x0010 | 3 = 0x0013) -> sysretq loads SS = 0x1b, CS = 0x23.
    // Include RPL=3 here because AMD CPUs preserve STAR[49:48] in SS on 64-bit sysretq.
    let star_high: u32 = (0x0013 << 16) | 0x0008;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xc000_0081u32,
            in("eax") 0u32,
            in("edx") star_high,
            options(nostack)
        );
    }

    // 3. IA32_LSTAR (0xc000_0082): Entry point RIP
    let entry_addr = spacekernel_x86_64_syscall_entry as *const () as usize as u64;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xc000_0082u32,
            in("eax") entry_addr as u32,
            in("edx") (entry_addr >> 32) as u32,
            options(nostack)
        );
    }

    // 4. IA32_FMASK (0xc000_0084): Mask flags during syscall
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xc000_0084u32,
            in("eax") 0x257fd5u32,
            in("edx") 0u32,
            options(nostack)
        );
    }
}

/// Enter user mode via IRETQ.
pub unsafe fn enter_user_mode(entry: u64, user_sp: u64) -> ! {
    const USER_DS: u64 = (3 << 3) | 3; // Selector 0x18 | 3 = 0x1b
    const USER_CS: u64 = (4 << 3) | 3; // Selector 0x20 | 3 = 0x23
    const RFLAGS_USER: u64 = 0x202; // IF enabled, bit 1 reserved

    unsafe {
        core::arch::asm!(
            "cli",
            "swapgs",
            "push {ss}",
            "push {rsp}",
            "push {rflags}",
            "push {cs}",
            "push {rip}",
            "xor eax, eax",
            "xor ebx, ebx",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor esi, esi",
            "xor edi, edi",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "iretq",
            ss = in(reg) USER_DS,
            rsp = in(reg) user_sp,
            rflags = in(reg) RFLAGS_USER,
            cs = in(reg) USER_CS,
            rip = in(reg) entry,
            options(noreturn)
        );
    }
}

/// User register state captured at a `syscall` boundary.  Only the state that
/// the x86-64 syscall ABI preserves is needed for a clone child to resume
/// after the syscall with RAX = 0, including the argument registers.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UserContext {
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub r10: u64,
    pub r8: u64,
    pub r9: u64,
}

fn read_msr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: reading a model-specific register is side-effect free here.
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nostack, preserves_flags));
    }
    ((high as u64) << 32) | low as u64
}

fn write_msr(msr: u32, value: u64) {
    // SAFETY: callers write the architectural FS/GS base MSRs before returning
    // to user mode.
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack, preserves_flags)
        );
    }
}

fn syscall_frame() -> *mut u64 {
    let pointer: u64;
    // SAFETY: the syscall entry publishes the saved-frame pointer at GS:24.
    unsafe {
        core::arch::asm!(
            "mov {}, gs:[24]",
            out(reg) pointer,
            options(nomem, nostack, preserves_flags)
        );
    }
    pointer as *mut u64
}

/// Capture the current user context while a syscall is executing.
pub fn capture_user_context() -> Option<UserContext> {
    let frame = syscall_frame();
    if frame.is_null() {
        return None;
    }
    // SAFETY: the syscall entry publishes its live full-register frame at
    // GS:24 before calling into Rust.
    unsafe {
        let frame = &*(frame as *const crate::arch::SyscallFrame);
        Some(UserContext {
            rip: frame.rip,
            rflags: frame.flags,
            rsp: frame.rsp,
            rbx: frame.gpr.rbx,
            rbp: frame.gpr.rbp,
            r12: frame.gpr.r12,
            r13: frame.gpr.r13,
            r14: frame.gpr.r14,
            r15: frame.gpr.r15,
            rdi: frame.gpr.rdi,
            rsi: frame.gpr.rsi,
            rdx: frame.gpr.rdx,
            r10: frame.gpr.r10,
            r8: frame.gpr.r8,
            r9: frame.gpr.r9,
            fs_base: read_msr(0xc000_0100),
            gs_base: read_msr(0xc000_0102),
        })
    }
}

/// Enter user mode with the captured context and return value 0.
///
/// # Safety
/// The current address space must be the target process's address space and the
/// user stack/register state must be owned by the new task.
pub unsafe fn enter_user_context(context: &UserContext) -> ! {
    // A timer interrupt between SWAPGS and SYSRETQ would run kernel code with
    // the user GS base, so the whole handoff is atomic with respect to IRQs.
    crate::arch::disable_interrupts();
    write_msr(0xc000_0100, context.fs_base);
    write_msr(0xc000_0102, context.gs_base);
    // SAFETY: this code runs on the child task immediately before sysretq.
    unsafe {
        let _ = crate::fpu::enable_user();
    }
    // SAFETY: address space and context are prepared by the caller.  RAX is
    // zeroed for the child's clone return value.
    unsafe {
        core::arch::asm!(
            "mov rbx, [rdi + 24]",
            "mov rbp, [rdi + 32]",
            "mov r12, [rdi + 40]",
            "mov r13, [rdi + 48]",
            "mov r14, [rdi + 56]",
            "mov r15, [rdi + 64]",
            "mov rcx, [rdi + 0]",
            "mov r11, [rdi + 8]",
            "mov rsp, [rdi + 16]",
            "mov rsi, [rdi + 96]",
            "mov rdx, [rdi + 104]",
            "mov r10, [rdi + 112]",
            "mov r8, [rdi + 120]",
            "mov r9, [rdi + 128]",
            "mov rdi, [rdi + 88]",
            "xor eax, eax",
            "swapgs",
            "sysretq",
            in("rdi") context as *const UserContext,
            options(noreturn)
        );
    }
}
