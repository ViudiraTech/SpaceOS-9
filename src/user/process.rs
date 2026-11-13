/*
 *
 *       src/user/process.rs
 *       User process creation, address space binding and lifecycle execution
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! User process creation and execution.

use crate::{
    arch, sched,
    sync::SpinLock,
    user::{address_space::UserAddressSpace, elf, stack},
};
use alloc::sync::Arc;

pub unsafe fn install_tls(tp: u64) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xc000_0100u32,
            in("eax") (tp as u32),
            in("edx") ((tp >> 32) as u32),
            options(nostack)
        );
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("msr tpidr_el0, {}", in(reg) tp, options(nostack));
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("mv tp, {}", in(reg) tp, options(nostack));
    }
}

/// Start the init process as PID 1.  `path` may be an ELF image or a `#!`
/// script; must be called before any other user process so the PID allocator
/// yields 1.
pub fn spawn_init(path: &str) -> Result<u64, &'static str> {
    if sched::init_pid() != 0 {
        return Err("init already started");
    }
    let argv = [alloc::string::String::from(path)];
    let resolved = match crate::user::exec::resolve(path, &argv) {
        Ok(resolved) => resolved,
        Err(error) => {
            crate::kwarn!("init {} resolve failed: {:?}", path, error);
            return Err("init image unavailable or not executable");
        }
    };
    let mut space = UserAddressSpace::new()?;
    let loaded = elf::load_elf(&mut space, &resolved.data)?;
    let args: alloc::vec::Vec<&str> = resolved.argv.iter().map(|s| s.as_str()).collect();
    let user_sp = stack::setup(&mut space, &loaded, path, &args, &[])?;
    if sched::alloc_pid() != 1 {
        return Err("PID 1 was already allocated");
    }
    let space_arc = Arc::new(SpinLock::new(space));
    let entry_va = loaded.initial_pc;
    let tls_tp = loaded.tls_tp;
    let spec = sched::UserTaskSpec {
        tgid: 1,
        ltid: 1,
        ppid: 0,
        pgid: 1,
        sid: 1,
        exit_signal: crate::signal::SIGCHLD,
        clear_child_tid: 0,
        creds: sched::Credentials::default(),
        groups: alloc::vec::Vec::new(),
        comm: sched::comm_from_path(path),
        no_new_privs: false,
        fd_table: Arc::new(SpinLock::new(
            crate::vfs::FileDescriptorTable::new_with_stdio(),
        )),
        files_shared: false,
        fs: Arc::new(SpinLock::new(crate::vfs::FsContext::new())),
        signals: Arc::new(crate::signal::SignalState::new()),
        thread_signals: Arc::new(SpinLock::new(crate::signal::ThreadSignals::new())),
        address_space: space_arc,
    };
    sched::spawn_user(
        move || {
            crate::verbose::log(format_args!(
                "Entering user mode: entry={:#x} sp={:#x}",
                entry_va, user_sp
            ));
            // SAFETY: the scheduler installed and activated this address space.
            unsafe {
                install_tls(tls_tp);
                let _ = crate::fpu::enable_user();
                arch::enter_user_mode(entry_va, user_sp);
            }
        },
        0,
        sched::CpuMask::all(),
        spec,
    )
    .map_err(|_| "failed to spawn init")?;
    sched::set_init_pid(1);
    Ok(1)
}

pub fn spawn_user_elf(data: &[u8], name: &str) -> Result<u64, &'static str> {
    let mut space = UserAddressSpace::new()?;
    let loaded = elf::load_elf(&mut space, data)?;
    let user_sp = stack::setup_user_stack(&mut space, &loaded, name)?;
    let space_arc = Arc::new(SpinLock::new(space));

    let entry_va = loaded.initial_pc;
    let tls_tp = loaded.tls_tp;
    let pid = sched::alloc_pid();
    let spec = sched::UserTaskSpec {
        tgid: pid,
        ltid: pid,
        ppid: 1,
        pgid: pid,
        sid: pid,
        exit_signal: crate::signal::SIGCHLD,
        clear_child_tid: 0,
        creds: sched::Credentials::default(),
        groups: alloc::vec::Vec::new(),
        comm: sched::comm_from_path(name),
        no_new_privs: false,
        fd_table: Arc::new(SpinLock::new(
            crate::vfs::FileDescriptorTable::new_with_stdio(),
        )),
        files_shared: false,
        fs: Arc::new(SpinLock::new(crate::vfs::FsContext::new())),
        signals: Arc::new(crate::signal::SignalState::new()),
        thread_signals: Arc::new(SpinLock::new(crate::signal::ThreadSignals::new())),
        address_space: space_arc,
    };

    let task = sched::spawn_user(
        move || {
            crate::verbose::log(format_args!(
                "Entering user mode: entry={:#x} sp={:#x}",
                entry_va, user_sp
            ));
            // SAFETY: the address space is installed and activated by the
            // scheduler before this closure runs.
            unsafe {
                install_tls(tls_tp);
                let _ = crate::fpu::enable_user();
                arch::enter_user_mode(entry_va, user_sp);
            }
        },
        0,
        sched::CpuMask::all(),
        spec,
    )
    .map_err(|_| "failed to spawn user task")?;

    Ok(task.stats().tgid)
}
