/*
 *
 *       src/user/mod.rs
 *       User space process model, ELF execution and Linux ABI verification
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! User space support, isolated address spaces, ELF loading, and process lifecycle.

pub mod address_space;
pub mod elf;
mod elf_format;
pub mod exec;
pub mod process;
#[cfg(feature = "boot-self-test")]
pub mod self_test;
pub mod stack;
pub mod test_binaries;
pub mod uaccess;

pub use address_space::UserAddressSpace;
pub use process::{spawn_init, spawn_user_elf};

/// Execute the architecture-specific built-in user ELF test binary and verify
/// that it exits with code 42 via the Linux ABI.
pub fn run_self_test() -> Result<(), &'static str> {
    crate::verbose::log(format_args!("Starting user mode Linux ABI self-test..."));
    let tid = spawn_user_elf(test_binaries::TEST_USER_ELF, "user_test")?;
    crate::verbose::log(format_args!("Spawned user test task (tid={})", tid));

    // Wait for the user task to finish and check its exit code
    let mut loops = 0;
    while crate::syscall::last_user_exit_code() != 42 {
        crate::sched::sleep_micros(10_000);
        loops += 1;
        if loops > 1000 {
            return Err("user mode self-test timed out waiting for exit code 42");
        }
    }
    let _ = crate::sched::take_zombie(tid);
    crate::verbose::log(format_args!("user self-test passed: exit code 42 verified"));
    // A minimal executable whose PT_INTERP is a position-independent copy of
    // the real architecture test. The main entry must never be entered.
    let mut interpreter = test_binaries::TEST_USER_ELF.to_vec();
    interpreter[16..18].copy_from_slice(&3u16.to_le_bytes());
    crate::vfs::write_file("/tmp/spaceos-test-interpreter", &interpreter)
        .map_err(|_| "cannot stage interpreter")?;
    let mut main = alloc::vec![0u8;4096];
    main[..64].copy_from_slice(&test_binaries::TEST_USER_ELF[..64]);
    main[16..18].copy_from_slice(&2u16.to_le_bytes());
    main[24..32].copy_from_slice(&0x100100u64.to_le_bytes());
    main[32..40].copy_from_slice(&64u64.to_le_bytes());
    main[40..48].fill(0);
    main[56..58].copy_from_slice(&2u16.to_le_bytes());
    main[58..64].fill(0);
    main[64..68].copy_from_slice(&1u32.to_le_bytes());
    main[68..72].copy_from_slice(&5u32.to_le_bytes());
    main[80..88].copy_from_slice(&0x100000u64.to_le_bytes());
    main[96..104].copy_from_slice(&4096u64.to_le_bytes());
    main[104..112].copy_from_slice(&4096u64.to_le_bytes());
    main[112..120].copy_from_slice(&4096u64.to_le_bytes());
    let path = b"/tmp/spaceos-test-interpreter\0";
    main[512..512 + path.len()].copy_from_slice(path);
    main[120..124].copy_from_slice(&3u32.to_le_bytes());
    main[128..136].copy_from_slice(&512u64.to_le_bytes());
    main[152..160].copy_from_slice(&(path.len() as u64).to_le_bytes());
    crate::syscall::set_last_user_exit_code(-1);
    let interpreter_tid = spawn_user_elf(&main, "interpreter_test")?;
    for _ in 0..1000 {
        if crate::syscall::last_user_exit_code() == 42 {
            let _ = crate::sched::take_zombie(interpreter_tid);
            crate::verbose::log(format_args!(
                "ELF PT_INTERP/PIE user handoff self-test passed"
            ));
            return Ok(());
        }
        crate::sched::sleep_micros(1000);
    }
    Err("ELF interpreter handoff timed out")
}

/// x86_64 thread, POSIX signal, process-group and cross-process kill test.
#[cfg(target_arch = "x86_64")]
pub fn process_signal_self_test() -> Result<(), &'static str> {
    crate::vfs::write_file("/tmp/exec_target", test_binaries::EXEC_TARGET_ELF)
        .map_err(|_| "cannot stage exec target")?;
    crate::vfs::lookup_abs("/tmp/exec_target", true)
        .map_err(|_| "cannot find exec target")?
        .0
        .chmod(0o755)
        .map_err(|_| "cannot chmod exec target")?;
    crate::syscall::set_last_user_exit_code(-1);
    let pid = spawn_user_elf(test_binaries::PROCESS_SIGNAL_TEST_ELF, "process_signal")?;
    for _ in 0..8000 {
        if crate::syscall::last_user_exit_code() == 43 {
            let _ = crate::sched::take_zombie(pid);
            crate::verbose::log(format_args!(
                "thread/signal/process-group user self-test passed"
            ));
            return Ok(());
        }
        crate::sched::sleep_micros(1000);
    }
    Err("process/signal user self-test failed or timed out")
}

/// x86_64 vertical-slice test: fork, COW stack/data write, wait4, zombie reap.
#[cfg(target_arch = "x86_64")]
pub fn fork_wait_self_test() -> Result<(), &'static str> {
    crate::syscall::set_last_user_exit_code(-1);
    spawn_user_elf(test_binaries::FORK_TEST_ELF, "fork_wait")?;
    for _ in 0..3000 {
        if crate::syscall::last_user_exit_code() == 42 {
            crate::verbose::log(format_args!("fork/wait4/CoW user self-test passed"));
            return Ok(());
        }
        crate::sched::sleep_micros(1000);
    }
    Err("fork/wait4 user self-test failed or timed out")
}
