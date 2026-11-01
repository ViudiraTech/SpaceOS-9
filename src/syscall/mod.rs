/*
 *
 *       src/syscall/mod.rs
 *       Architecture-independent Linux ABI system call dispatcher
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux ABI system call dispatcher integrated with the VFS and scheduler subsystems.

pub mod identity;
mod linux;
pub use linux::{flock_release, inherit_limits};
mod memory;
mod metadata;
pub mod numbers;
mod prctl;
mod process;
mod system;

use crate::{
    kwarn, sched,
    user::uaccess,
    vfs::{
        self, FdEntry, VfsError,
        abi::{self, IoVec, LinuxStat, LinuxStatFs, WinSize},
    },
};
use alloc::{string::String, vec};
use core::sync::atomic::{AtomicI32, Ordering};
use numbers::*;

static LAST_USER_EXIT_CODE: AtomicI32 = AtomicI32::new(-1);

pub fn last_user_exit_code() -> i32 {
    LAST_USER_EXIT_CODE.load(Ordering::Acquire)
}

pub fn set_last_user_exit_code(code: i32) {
    LAST_USER_EXIT_CODE.store(code, Ordering::Release);
}

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "C" fn spacekernel_syscall_c(frame: *mut crate::arch::SyscallFrame) -> u64 {
    // SAFETY: the assembly entry passes its live saved-register frame.
    let frame = unsafe { &mut *frame };
    crate::fpu::disable_current();
    crate::arch::enable_interrupts();
    let full_restore = frame.gpr.rax == SYS_RT_SIGRETURN;
    if full_restore {
        crate::signal::rt_sigreturn(frame);
    } else {
        let result = dispatch(
            frame.gpr.rax,
            frame.gpr.rdi,
            frame.gpr.rsi,
            frame.gpr.rdx,
            frame.gpr.r10,
            frame.gpr.r8,
            frame.gpr.r9,
        );
        frame.gpr.rax = result as u64;
    }
    crate::arch::disable_interrupts();
    crate::signal::deliver_on_syscall_return(frame);
    if !full_restore {
        frame.gpr.rcx = frame.rip;
        frame.gpr.r11 = frame.flags;
    }
    // SAFETY: this CPU owns the task and interrupts are masked until SYSRETQ.
    unsafe {
        let _ = crate::fpu::enable_user();
    }
    u64::from(full_restore)
}

#[cfg(not(target_arch = "x86_64"))]
#[unsafe(no_mangle)]
pub extern "C" fn spacekernel_syscall_c(
    num: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> i64 {
    crate::fpu::disable_current();
    crate::arch::enable_interrupts();
    let result = dispatch(num, arg0, arg1, arg2, arg3, arg4, arg5);
    crate::arch::disable_interrupts();
    // SAFETY: the scheduler restored the current task before this return.
    unsafe {
        let _ = crate::fpu::enable_user();
    }
    result
}

/// Syscall each task is currently inside, exposed as `/proc/<pid>/syscall`
/// (the standard way to see where a hung process is blocked).
const CURRENT_SLOTS: usize = 1024;
static CURRENT_NR: [core::sync::atomic::AtomicU64; CURRENT_SLOTS] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; CURRENT_SLOTS];
static CURRENT_ARG: [[core::sync::atomic::AtomicU64; 2]; CURRENT_SLOTS] =
    [const { [const { core::sync::atomic::AtomicU64::new(0) }; 2] }; CURRENT_SLOTS];

/// `(number, arg0, arg1)` of the syscall `tid` is executing, or `None` when it
/// is running in user mode.
pub fn current_syscall(tid: u64) -> Option<(u64, u64, u64)> {
    use core::sync::atomic::Ordering::Relaxed;
    let slot = tid as usize % CURRENT_SLOTS;
    let nr = CURRENT_NR[slot].load(Relaxed);
    (nr != u64::MAX).then(|| (nr, CURRENT_ARG[slot][0].load(Relaxed), CURRENT_ARG[slot][1].load(Relaxed)))
}

pub fn dispatch(num: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i64 {
    let slot = sched::current().map_or(0, |task| task.stats().tid as usize % CURRENT_SLOTS);
    CURRENT_ARG[slot][0].store(arg0, core::sync::atomic::Ordering::Relaxed);
    CURRENT_ARG[slot][1].store(arg1, core::sync::atomic::Ordering::Relaxed);
    CURRENT_NR[slot].store(num, core::sync::atomic::Ordering::Relaxed);
    let result = dispatch_inner(num, arg0, arg1, arg2, arg3, arg4, arg5);
    CURRENT_NR[slot].store(u64::MAX, core::sync::atomic::Ordering::Relaxed);
    // Data transfer announces itself per object. These rarer calls change
    // connection state or signals for tasks this path cannot identify.
    if matches!(
        num,
        SYS_CLOSE
            | SYS_SHUTDOWN
            | SYS_CONNECT
            | SYS_ACCEPT
            | SYS_ACCEPT4
            | SYS_KILL
            | SYS_TKILL
            | SYS_TGKILL
    ) {
        crate::ipc::wake_pollers();
    }
    result
}

fn dispatch_inner(
    num: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> i64 {
    match num {
        // Core File I/O
        SYS_READ => sys_read(arg0 as i32, arg1 as *mut u8, arg2 as usize),
        SYS_WRITE => sys_write(arg0 as i32, arg1 as *const u8, arg2 as usize),
        SYS_OPENAT => sys_openat(arg0 as i32, arg1 as *const u8, arg2 as u32, arg3 as u32),
        SYS_CLOSE => sys_close(arg0 as i32),
        SYS_LSEEK => sys_lseek(arg0 as i32, arg1 as i64, arg2 as u32),
        SYS_PREAD64 => sys_pread64(arg0 as i32, arg1 as *mut u8, arg2 as usize, arg3),
        SYS_PWRITE64 => sys_pwrite64(arg0 as i32, arg1 as *const u8, arg2 as usize, arg3),
        SYS_READV => sys_readv(arg0 as i32, arg1 as *const IoVec, arg2 as usize),
        SYS_WRITEV => sys_writev(arg0 as i32, arg1 as *const IoVec, arg2 as usize),

        // Descriptor Management & Pipes
        SYS_DUP => sys_dup(arg0 as i32),
        SYS_DUP3 => sys_dup3(arg0 as i32, arg1 as i32, arg2 as u32),
        SYS_PIPE2 => sys_pipe2(arg0 as *mut i32, arg1 as u32),
        SYS_EVENTFD2 => crate::ipc::eventfd2(arg0 as u32, arg1 as u32),
        SYS_SOCKETPAIR => crate::ipc::socketpair(arg0, arg1, arg2, arg3),
        SYS_SENDMSG => crate::ipc::sendmsg(arg0, arg1, arg2),
        SYS_RECVMSG => crate::ipc::recvmsg(arg0, arg1, arg2),
        SYS_SHUTDOWN => crate::ipc::shutdown(arg0, arg1 as u32),
        SYS_SOCKET => crate::ipc::socket(arg0, arg1, arg2),
        SYS_BIND => crate::ipc::bind(arg0, arg1, arg2),
        SYS_LISTEN => crate::ipc::listen(arg0, arg1),
        SYS_CONNECT => crate::ipc::connect(arg0, arg1, arg2),
        SYS_ACCEPT => crate::ipc::accept4(arg0, arg1, arg2, 0),
        SYS_ACCEPT4 => crate::ipc::accept4(arg0, arg1, arg2, arg3),
        SYS_GETSOCKNAME => crate::ipc::getsockname(arg0, arg1, arg2),
        SYS_GETPEERNAME => crate::ipc::getpeername(arg0, arg1, arg2),
        SYS_SENDTO => crate::ipc::sendto(arg0, arg1, arg2, arg3, arg4, arg5),
        SYS_RECVFROM => crate::ipc::recvfrom(arg0, arg1, arg2, arg3, arg4, arg5),
        SYS_SETSOCKOPT => crate::ipc::setsockopt(arg0, arg1, arg2, arg3, arg4),
        SYS_GETSOCKOPT => crate::ipc::getsockopt(arg0, arg1, arg2, arg3, arg4),
        SYS_FCNTL => sys_fcntl(arg0 as i32, arg1 as u32, arg2),
        SYS_IOCTL => sys_ioctl(arg0 as i32, arg1 as u32, arg2),

        // Metadata, Directories & Links
        SYS_FSTAT => sys_fstat(arg0 as i32, arg1 as *mut LinuxStat),
        SYS_NEWFSTATAT => sys_newfstatat(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as *mut LinuxStat,
            arg3 as u32,
        ),
        SYS_GETDENTS64 => sys_getdents64(arg0 as i32, arg1 as *mut u8, arg2 as usize),
        SYS_MKDIRAT => sys_mkdirat(arg0 as i32, arg1 as *const u8, arg2 as u32),
        SYS_UNLINKAT => sys_unlinkat(arg0 as i32, arg1 as *const u8, arg2 as u32),
        SYS_SYMLINKAT => sys_symlinkat(arg0 as *const u8, arg1 as i32, arg2 as *const u8),
        SYS_READLINKAT => sys_readlinkat(
            arg0 as i32,
            arg1 as *const u8,
            arg2 as *mut u8,
            arg3 as usize,
        ),
        SYS_FACCESSAT => sys_faccessat(arg0 as i32, arg1 as *const u8, arg2 as u32, arg3 as u32),
        SYS_FTRUNCATE => sys_ftruncate(arg0 as i32, arg1),
        SYS_GETCWD => sys_getcwd(arg0 as *mut u8, arg1 as usize),
        SYS_CHDIR => sys_chdir(arg0 as *const u8),

        // Filesystem & Sync
        SYS_FSYNC => sys_fsync(arg0 as i32),
        SYS_SYNC => {
            vfs::sync_all();
            0
        }
        SYS_STATFS => sys_statfs(arg0 as *const u8, arg1 as *mut LinuxStatFs),
        SYS_FSTATFS => sys_fstatfs(arg0 as i32, arg1 as *mut LinuxStatFs),
        SYS_MOUNT => sys_mount(arg0 as *const u8, arg1 as *const u8, arg2 as *const u8),
        SYS_UMOUNT2 => sys_umount2(arg0 as *const u8, arg1 as u32),

        // Legacy x86_64 non-*at syscalls
        #[cfg(target_arch = "x86_64")]
        SYS_OPEN => sys_openat(abi::AT_FDCWD, arg0 as *const u8, arg1 as u32, arg2 as u32),
        #[cfg(target_arch = "x86_64")]
        SYS_CREAT => sys_openat(
            abi::AT_FDCWD,
            arg0 as *const u8,
            abi::O_CREAT | abi::O_WRONLY | abi::O_TRUNC,
            arg1 as u32,
        ),
        #[cfg(target_arch = "x86_64")]
        SYS_STAT => sys_newfstatat(abi::AT_FDCWD, arg0 as *const u8, arg1 as *mut LinuxStat, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_LSTAT => sys_newfstatat(
            abi::AT_FDCWD,
            arg0 as *const u8,
            arg1 as *mut LinuxStat,
            abi::AT_SYMLINK_NOFOLLOW,
        ),
        #[cfg(target_arch = "x86_64")]
        SYS_ACCESS => sys_faccessat(abi::AT_FDCWD, arg0 as *const u8, arg1 as u32, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_DUP2 => sys_dup2(arg0 as i32, arg1 as i32),
        #[cfg(target_arch = "x86_64")]
        SYS_PIPE => sys_pipe2(arg0 as *mut i32, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_MKDIR => sys_mkdirat(abi::AT_FDCWD, arg0 as *const u8, arg1 as u32),
        #[cfg(target_arch = "x86_64")]
        SYS_RMDIR => sys_unlinkat(abi::AT_FDCWD, arg0 as *const u8, abi::AT_REMOVEDIR),
        #[cfg(target_arch = "x86_64")]
        SYS_UNLINK => sys_unlinkat(abi::AT_FDCWD, arg0 as *const u8, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_SYMLINK => sys_symlinkat(arg0 as *const u8, abi::AT_FDCWD, arg1 as *const u8),
        #[cfg(target_arch = "x86_64")]
        SYS_READLINK => sys_readlinkat(
            abi::AT_FDCWD,
            arg0 as *const u8,
            arg1 as *mut u8,
            arg2 as usize,
        ),

        // Process, Memory & Scheduling
        SYS_GETPID => process::getpid(),
        SYS_GETTID => process::gettid(),
        SYS_GETPPID => process::getppid(),
        SYS_GETUID => identity::getuid(),
        SYS_GETEUID => identity::geteuid(),
        SYS_GETGID => identity::getgid(),
        SYS_GETEGID => identity::getegid(),
        SYS_SETUID => identity::setuid(arg0 as u32),
        SYS_SETGID => identity::setgid(arg0 as u32),
        SYS_SETREUID => identity::setreuid(arg0 as u32, arg1 as u32),
        SYS_SETREGID => identity::setregid(arg0 as u32, arg1 as u32),
        SYS_SETRESUID => identity::setresuid(arg0 as u32, arg1 as u32, arg2 as u32),
        SYS_GETRESUID => identity::getresuid(arg0, arg1, arg2),
        SYS_SETRESGID => identity::setresgid(arg0 as u32, arg1 as u32, arg2 as u32),
        SYS_GETRESGID => identity::getresgid(arg0, arg1, arg2),
        SYS_SETFSUID => identity::setfsuid(arg0 as u32),
        SYS_SETFSGID => identity::setfsgid(arg0 as u32),
        SYS_GETGROUPS => identity::getgroups(arg0 as u32, arg1),
        SYS_SETGROUPS => identity::setgroups(arg0 as u32, arg1),
        SYS_UMASK => identity::umask(arg0 as u32),
        SYS_SCHED_GETAFFINITY => identity::sched_getaffinity(arg0, arg1 as u32, arg2),
        SYS_SCHED_SETAFFINITY => identity::sched_setaffinity(arg0, arg1 as u32, arg2),
        SYS_UNAME => system::uname(arg0),
        SYS_SETHOSTNAME => system::sethostname(arg0, arg1),
        SYS_SETDOMAINNAME => system::setdomainname(arg0, arg1),
        SYS_PRCTL => prctl::prctl(arg0, arg1, arg2, arg3, arg4),
        SYS_SENDFILE => sys_sendfile(arg0 as i32, arg1 as i32, arg2, arg3 as usize),
        SYS_RT_SIGACTION => crate::signal::rt_sigaction(arg0, arg1, arg2, arg3),
        SYS_RT_SIGPROCMASK => crate::signal::rt_sigprocmask(arg0, arg1, arg2, arg3),
        SYS_RT_SIGPENDING => crate::signal::rt_sigpending(arg0, arg1),
        SYS_RT_SIGSUSPEND => crate::signal::rt_sigsuspend(arg0, arg1),
        SYS_SIGALTSTACK => crate::signal::sigaltstack(arg0, arg1),
        SYS_KILL => crate::signal::kill(arg0 as i64, arg1),
        SYS_TKILL => crate::signal::tkill(arg0, arg1),
        SYS_TGKILL => crate::signal::tgkill(arg0, arg1, arg2),
        #[cfg(target_arch = "x86_64")]
        SYS_PAUSE => crate::signal::pause(),
        SYS_SETPGID => process::setpgid(arg0 as i64, arg1 as i64),
        SYS_GETPGID => process::getpgid(arg0 as i64),
        SYS_SETSID => process::setsid(),
        SYS_GETSID => process::getsid(arg0 as i64),
        SYS_MEMFD_CREATE => crate::ipc::memfd_create(arg0, arg1 as u32),
        SYS_EPOLL_CREATE1 => crate::ipc::epoll_create1(arg0 as u32),
        #[cfg(target_arch = "x86_64")]
        SYS_EPOLL_CREATE => crate::ipc::epoll_create(arg0 as i32),
        SYS_EPOLL_CTL => crate::ipc::epoll_ctl(arg0, arg1 as u32, arg2 as i32, arg3),
        #[cfg(target_arch = "x86_64")]
        SYS_EPOLL_WAIT => crate::ipc::epoll_wait(arg0, arg1, arg2 as u32, arg3 as i64),
        SYS_EPOLL_PWAIT => {
            crate::ipc::epoll_pwait(arg0, arg1, arg2 as u32, arg3 as i64, arg4, arg5)
        }
        #[cfg(target_arch = "x86_64")]
        SYS_SIGNALFD => crate::ipc::signalfd(arg0 as i64, arg1, arg2),
        SYS_SIGNALFD4 => crate::ipc::signalfd4(arg0 as i64, arg1, arg2, arg3 as u32),
        SYS_TIMERFD_CREATE => crate::ipc::timerfd_create(arg0 as u32, arg1 as u32),
        SYS_TIMERFD_SETTIME => crate::ipc::timerfd_settime(arg0, arg1 as u32, arg2, arg3),
        SYS_TIMERFD_GETTIME => crate::ipc::timerfd_gettime(arg0, arg1),
        SYS_FUTEX => {
            crate::ipc::futex::futex(arg0, arg1 as u32, arg2 as u32, arg3, arg4, arg5 as u32)
        }
        #[cfg(not(target_arch = "x86_64"))]
        SYS_RT_SIGRETURN => -abi::ENOSYS,
        SYS_WAIT4 => process::wait4(arg0 as i64, arg1, arg2 as u32, arg3),
        SYS_WAITID => process::waitid(arg0 as u32, arg1 as u32, arg2, arg3 as u32, arg4),
        #[cfg(target_arch = "x86_64")]
        SYS_CLONE => process::clone(arg0, arg1, arg2, arg3, arg4),
        #[cfg(target_arch = "x86_64")]
        SYS_FORK => process::fork(),
        #[cfg(not(target_arch = "x86_64"))]
        SYS_CLONE => -abi::ENOSYS,
        SYS_SCHED_YIELD => sys_sched_yield(),
        SYS_NANOSLEEP => sys_nanosleep(arg0),
        SYS_EXIT => sys_exit(arg0 as i32, false),
        SYS_EXIT_GROUP => sys_exit(arg0 as i32, true),
        SYS_SET_TID_ADDRESS => sys_set_tid_address(arg0),
        SYS_MKNODAT => metadata::mknodat(arg0 as i32, arg1, arg2 as u32, arg3),
        SYS_FCHMOD => metadata::fchmod(arg0 as i32, arg1 as u32),
        SYS_FCHMODAT => metadata::fchmodat(arg0 as i32, arg1, arg2 as u32, 0),
        SYS_FCHOWN => metadata::fchown(arg0 as i32, arg1 as u32, arg2 as u32),
        SYS_FCHOWNAT => {
            metadata::fchownat(arg0 as i32, arg1, arg2 as u32, arg3 as u32, arg4 as u32)
        }
        SYS_EXECVE => process::execve(arg0, arg1, arg2),
        SYS_PPOLL => process::ppoll(arg0, arg1, arg2, arg3, arg4),
        #[cfg(target_arch = "x86_64")]
        SYS_POLL => process::poll(arg0, arg1, arg2 as i32 as i64),
        SYS_CLOCK_GETTIME => linux::clock_gettime(arg0 as u32, arg1),
        SYS_GETRANDOM => process::getrandom(arg0, arg1 as usize, arg2 as u32),
        SYS_BRK => sys_brk(arg0),
        SYS_MMAP => memory::mmap(arg0, arg1, arg2 as u32, arg3 as u32, arg4 as i32, arg5),
        SYS_MUNMAP => memory::munmap(arg0, arg1),
        SYS_MPROTECT => memory::mprotect(arg0, arg1, arg2 as u32),
        #[cfg(target_arch = "x86_64")]
        SYS_ARCH_PRCTL => sys_arch_prctl(arg0 as u32, arg1),
        SYS_MREMAP => memory::mremap(arg0, arg1, arg2, arg3 as u32, arg4),
        SYS_MADVISE => memory::madvise(arg0, arg1, arg2 as u32),
        SYS_MSYNC => memory::msync(arg0, arg1, arg2 as u32),
        SYS_MINCORE => memory::mincore(arg0, arg1, arg2),
        SYS_MLOCK | SYS_MUNLOCK => memory::mlock(arg0, arg1),
        SYS_MLOCK2 => memory::mlock(arg0, arg1),
        SYS_MLOCKALL | SYS_MUNLOCKALL => 0,
        SYS_SHMGET => crate::ipc::shm::shmget(arg0 as i32, arg1, arg2 as u32),
        SYS_SHMAT => crate::ipc::shm::shmat(arg0 as i32, arg1, arg2 as u32),
        SYS_SHMDT => crate::ipc::shm::shmdt(arg0),
        SYS_SHMCTL => crate::ipc::shm::shmctl(arg0 as i32, arg1 as u32, arg2),
        SYS_CLOCK_GETRES => linux::clock_getres(arg0 as u32, arg1),
        SYS_CLOCK_NANOSLEEP => linux::clock_nanosleep(arg0 as u32, arg1 as u32, arg2, arg3),
        SYS_CLOCK_SETTIME => linux::clock_settime(arg0 as u32, arg1),
        SYS_SETTIMEOFDAY => linux::settimeofday(arg0),
        SYS_GETTIMEOFDAY => linux::gettimeofday(arg0, arg1),
        SYS_TIMES => linux::times(arg0),
        SYS_GETRUSAGE => linux::getrusage(arg0 as i32, arg1),
        SYS_SYSINFO => linux::sysinfo(arg0),
        SYS_GETRLIMIT => linux::getrlimit(arg0 as u32, arg1),
        SYS_SETRLIMIT => linux::setrlimit(arg0 as u32, arg1),
        SYS_PRLIMIT64 => linux::prlimit64(arg0, arg1 as u32, arg2, arg3),
        SYS_SET_ROBUST_LIST => linux::set_robust_list(arg0, arg1),
        SYS_GET_ROBUST_LIST => linux::get_robust_list(arg0, arg1, arg2),
        SYS_RT_SIGTIMEDWAIT => crate::signal::rt_sigtimedwait(arg0, arg1, arg2, arg3),
        SYS_STATX => linux::statx(arg0 as i32, arg1, arg2 as u32, arg3 as u32, arg4),
        SYS_PSELECT6 => linux::pselect6(arg0 as i32, arg1, arg2, arg3, arg4, arg5),
        SYS_EPOLL_PWAIT2 => crate::ipc::epoll_pwait2(arg0, arg1, arg2 as u32, arg3, arg4, arg5),
        SYS_RENAMEAT => linux::renameat2(arg0 as i32, arg1, arg2 as i32, arg3, 0),
        SYS_RENAMEAT2 => linux::renameat2(arg0 as i32, arg1, arg2 as i32, arg3, arg4 as u32),
        SYS_LINKAT => linux::linkat(arg0 as i32, arg1, arg2 as i32, arg3, arg4 as u32),
        SYS_UTIMENSAT => linux::utimensat(arg0 as i32, arg1, arg2, arg3 as u32),
        SYS_FCHDIR => linux::fchdir(arg0 as i32),
        SYS_TRUNCATE => linux::truncate(arg0, arg1),
        SYS_FDATASYNC => linux::fdatasync(arg0 as i32),
        SYS_SYNCFS => linux::syncfs(arg0 as i32),
        SYS_FLOCK => linux::flock(arg0 as i32, arg1 as u32),
        SYS_PREADV => linux::preadv(arg0 as i32, arg1, arg2, arg3),
        SYS_PWRITEV => linux::pwritev(arg0 as i32, arg1, arg2, arg3),
        SYS_FALLOCATE => linux::fallocate(arg0 as i32, arg1 as u32, arg2 as i64, arg3 as i64),
        SYS_READAHEAD => 0,
        SYS_CLOSE_RANGE => linux::close_range(arg0 as u32, arg1 as u32, arg2 as u32),
        SYS_FACCESSAT2 => sys_faccessat(arg0 as i32, arg1 as *const u8, arg2 as u32, arg3 as u32),
        SYS_CAPGET => linux::capget(arg0, arg1),
        SYS_CAPSET => linux::capset(arg0, arg1),
        SYS_PERSONALITY => linux::personality(arg0),
        SYS_RSEQ => linux::rseq(),
        SYS_MEMBARRIER => linux::membarrier(arg0 as u32, arg1 as u32),
        SYS_GETCPU => linux::getcpu(arg0, arg1),
        SYS_GETPRIORITY => linux::getpriority(arg0 as u32, arg1),
        SYS_SETPRIORITY => linux::setpriority(arg0 as u32, arg1, arg2 as i32),
        SYS_SCHED_GETSCHEDULER => linux::sched_getscheduler(arg0),
        SYS_SCHED_SETSCHEDULER => linux::sched_setscheduler(arg0, arg1 as u32, arg2),
        SYS_SCHED_GETPARAM => linux::sched_getparam(arg0, arg1),
        SYS_SCHED_SETPARAM => linux::sched_setparam(arg0, arg1),
        SYS_SCHED_GET_PRIORITY_MAX => linux::sched_get_priority_max(arg0 as u32),
        SYS_SCHED_GET_PRIORITY_MIN => linux::sched_get_priority_limit(arg0 as u32),
        SYS_SCHED_RR_GET_INTERVAL => linux::sched_rr_get_interval(arg0, arg1),
        SYS_REBOOT => linux::reboot(arg0 as u32, arg1 as u32, arg2 as u32),
        SYS_SETXATTR | SYS_LSETXATTR | SYS_FSETXATTR => linux::xattr_unsupported(),
        SYS_REMOVEXATTR | SYS_LREMOVEXATTR | SYS_FREMOVEXATTR => linux::xattr_unsupported(),
        SYS_GETXATTR | SYS_LGETXATTR | SYS_FGETXATTR => linux::xattr_get_missing(),
        SYS_LISTXATTR | SYS_LLISTXATTR | SYS_FLISTXATTR => linux::xattr_list_empty(),
        SYS_CHROOT => -abi::EPERM,
        SYS_SETITIMER => process::setitimer(arg0 as u32, arg1, arg2),
        SYS_GETITIMER => process::getitimer(arg0 as u32, arg1),
        SYS_INOTIFY_INIT1 => crate::ipc::inotify_init1(arg0 as u32),
        SYS_INOTIFY_ADD_WATCH => crate::ipc::inotify_add_watch(arg0, arg1, arg2 as u32),
        SYS_INOTIFY_RM_WATCH => crate::ipc::inotify_rm_watch(arg0, arg1 as i32),
        SYS_CLONE3 => process::clone3(arg0, arg1),
        #[cfg(target_arch = "x86_64")]
        SYS_SELECT => linux::select(arg0 as i32, arg1, arg2, arg3, arg4),
        #[cfg(target_arch = "x86_64")]
        SYS_RENAME => linux::renameat2(abi::AT_FDCWD, arg0, abi::AT_FDCWD, arg1, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_LINK => linux::linkat(abi::AT_FDCWD, arg0, abi::AT_FDCWD, arg1, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_CHMOD => metadata::fchmodat(abi::AT_FDCWD, arg0, arg1 as u32, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_CHOWN => metadata::fchownat(abi::AT_FDCWD, arg0, arg1 as u32, arg2 as u32, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_LCHOWN => metadata::fchownat(
            abi::AT_FDCWD,
            arg0,
            arg1 as u32,
            arg2 as u32,
            abi::AT_SYMLINK_NOFOLLOW,
        ),
        #[cfg(target_arch = "x86_64")]
        SYS_TIME => linux::time(arg0),
        #[cfg(target_arch = "x86_64")]
        SYS_FUTIMESAT => linux::futimesat(arg0 as i32, arg1, arg2),
        #[cfg(target_arch = "x86_64")]
        SYS_UTIMES => linux::futimesat(abi::AT_FDCWD, arg0, arg1),
        #[cfg(target_arch = "x86_64")]
        SYS_UTIME => linux::utime(arg0, arg1),
        #[cfg(target_arch = "x86_64")]
        SYS_INOTIFY_INIT => crate::ipc::inotify_init1(0),
        #[cfg(target_arch = "x86_64")]
        SYS_VFORK => process::clone(0x4000 | 0x100 | 17, 0, 0, 0, 0),
        #[cfg(target_arch = "x86_64")]
        SYS_ALARM => process::alarm(arg0 as u32),
        // I/O port access has no user of record, and granting IOPL 3 would bypass
        // every kernel driver. Xorg treats this failure as "no legacy VGA ports".
        #[cfg(target_arch = "x86_64")]
        SYS_IOPL | SYS_IOPERM => -abi::EPERM,
        _ => {
            kwarn!("unimplemented syscall: num={}", num);
            -abi::ENOSYS
        }
    }
}

fn read_c_string(ptr: *const u8, max_len: usize) -> Result<String, VfsError> {
    uaccess::string(ptr as u64, max_len)
}

fn get_open_file(fd: i32) -> Result<FdEntry, VfsError> {
    if fd < 0 {
        return Err(VfsError::BadFileDescriptor);
    }
    let tbl = vfs::current_fd_table();
    let guard = tbl.lock();
    guard.get(fd as usize).ok_or(VfsError::BadFileDescriptor)
}

fn sys_openat(dirfd: i32, path_ptr: *const u8, flags: u32, mode: u32) -> i64 {
    let path_str = match read_c_string(path_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_path = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    let file = match vfs::open_abs(&abs_path, flags, mode) {
        Ok(f) => f,
        Err(e) => return e.to_errno(),
    };
    let cloexec = (flags & abi::O_CLOEXEC) != 0;
    let mut guard = tbl.lock();
    match guard.alloc_fd(0, FdEntry { file, cloexec }) {
        Ok(fd) => fd as i64,
        Err(e) => e.to_errno(),
    }
}

fn sys_close(fd: i32) -> i64 {
    if fd < 0 {
        return -abi::EBADF;
    }
    let tbl = vfs::current_fd_table();
    let res = tbl.lock().close(fd as usize);
    match res {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

/// Bytes moved per kernel buffer. Regular files and memory devices continue
/// past it, so large transfers do not surface as short counts.
const IO_CHUNK: usize = 1024 * 1024;

fn continues_past_chunk(entry: &FdEntry) -> bool {
    entry.file.inode.metadata().is_ok_and(|meta| {
        matches!(
            meta.kind,
            vfs::InodeKind::Regular | vfs::InodeKind::BlockDevice | vfs::InodeKind::CharDevice
        ) && !entry.file.abs_path.starts_with("/dev/tty")
            && entry.file.abs_path != "/dev/console"
    })
}

fn sys_read(fd: i32, buf: *mut u8, count: usize) -> i64 {
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    if count == 0 {
        return 0;
    }
    if buf.is_null() {
        return -abi::EFAULT;
    }
    let chunk = count.min(IO_CHUNK);
    let mut kbuf = vec![0u8; chunk];
    let mut total = 0usize;
    loop {
        let want = (count - total).min(chunk);
        if let Err(e) = uaccess::check(buf as u64 + total as u64, want, true) {
            return if total > 0 {
                total as i64
            } else {
                e.to_errno()
            };
        }
        match entry.file.read(&mut kbuf[..want]) {
            Ok(n) => {
                if let Err(e) = uaccess::write(buf as u64 + total as u64, &kbuf[..n]) {
                    return if total > 0 {
                        total as i64
                    } else {
                        e.to_errno()
                    };
                }
                total += n;
                if n < want || total == count || !continues_past_chunk(&entry) {
                    return total as i64;
                }
            }
            Err(e) => {
                return if total > 0 {
                    total as i64
                } else {
                    e.to_errno()
                };
            }
        }
    }
}
fn sys_write(fd: i32, buf: *const u8, count: usize) -> i64 {
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    if count == 0 {
        return 0;
    }
    if buf.is_null() {
        return -abi::EFAULT;
    }
    let chunk = count.min(IO_CHUNK);
    let mut kbuf = vec![0u8; chunk];
    let mut total = 0usize;
    loop {
        let want = (count - total).min(chunk);
        if let Err(e) = uaccess::read(buf as u64 + total as u64, &mut kbuf[..want]) {
            return if total > 0 {
                total as i64
            } else {
                e.to_errno()
            };
        }
        match entry.file.write(&kbuf[..want]) {
            Ok(n) => {
                total += n;
                if n < want || total == count || !continues_past_chunk(&entry) {
                    return total as i64;
                }
            }
            Err(e) => {
                return if total > 0 {
                    total as i64
                } else {
                    e.to_errno()
                };
            }
        }
    }
}

fn sys_lseek(fd: i32, offset: i64, whence: u32) -> i64 {
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    match entry.file.seek(offset, whence) {
        Ok(pos) => pos as i64,
        Err(e) => e.to_errno(),
    }
}

fn sys_pread64(fd: i32, buf: *mut u8, count: usize, pos: u64) -> i64 {
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    if count == 0 {
        return 0;
    }
    if buf.is_null() {
        return -abi::EFAULT;
    }
    if let Err(e) = uaccess::check(buf as u64, count.min(65536), true) {
        return e.to_errno();
    }
    let mut kbuf = vec![0u8; count.min(65536)];
    match entry.file.pread(pos, &mut kbuf) {
        Ok(n) => {
            if let Err(e) = uaccess::write(buf as u64, &kbuf[..n]) {
                return e.to_errno();
            }
            n as i64
        }
        Err(e) => e.to_errno(),
    }
}

fn sys_pwrite64(fd: i32, buf: *const u8, count: usize, pos: u64) -> i64 {
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    if count == 0 {
        return 0;
    }
    if buf.is_null() {
        return -abi::EFAULT;
    }
    let limit = count.min(65536);
    let mut kbuf = vec![0u8; limit];
    if let Err(e) = uaccess::read(buf as u64, &mut kbuf) {
        return e.to_errno();
    }
    match entry.file.pwrite(pos, &kbuf) {
        Ok(n) => n as i64,
        Err(e) => e.to_errno(),
    }
}

fn sys_readv(fd: i32, iov_ptr: *const IoVec, iovcnt: usize) -> i64 {
    if let Err(e) = get_open_file(fd) {
        return e.to_errno();
    }
    if iovcnt > 1024 {
        return -abi::EINVAL;
    }
    if iovcnt == 0 {
        return 0;
    }
    if iov_ptr.is_null() {
        return -abi::EFAULT;
    }
    let mut total = 0i64;
    for i in 0..iovcnt {
        let address = match (iov_ptr as u64).checked_add((i * core::mem::size_of::<IoVec>()) as u64)
        {
            Some(a) => a,
            None => return if total > 0 { total } else { -abi::EFAULT },
        };
        let iov = match uaccess::read_value::<IoVec>(address) {
            Ok(v) => v,
            Err(e) => return if total > 0 { total } else { e.to_errno() },
        };
        if iov.iov_len > i64::MAX as u64 {
            return if total > 0 { total } else { -abi::EINVAL };
        }
        if iov.iov_len == 0 {
            continue;
        }
        let n = sys_read(fd, iov.iov_base as *mut u8, iov.iov_len as usize);
        if n < 0 {
            return if total > 0 { total } else { n };
        }
        total = total.saturating_add(n);
        if (n as u64) < iov.iov_len {
            break;
        }
    }
    total
}

fn sys_writev(fd: i32, iov_ptr: *const IoVec, iovcnt: usize) -> i64 {
    if let Err(e) = get_open_file(fd) {
        return e.to_errno();
    }
    if iovcnt > 1024 {
        return -abi::EINVAL;
    }
    if iovcnt == 0 {
        return 0;
    }
    if iov_ptr.is_null() {
        return -abi::EFAULT;
    }
    let mut total = 0i64;
    for i in 0..iovcnt {
        let address = match (iov_ptr as u64).checked_add((i * core::mem::size_of::<IoVec>()) as u64)
        {
            Some(a) => a,
            None => return if total > 0 { total } else { -abi::EFAULT },
        };
        let iov = match uaccess::read_value::<IoVec>(address) {
            Ok(v) => v,
            Err(e) => return if total > 0 { total } else { e.to_errno() },
        };
        if iov.iov_len > i64::MAX as u64 {
            return if total > 0 { total } else { -abi::EINVAL };
        }
        if iov.iov_len == 0 {
            continue;
        }
        let n = sys_write(fd, iov.iov_base as *const u8, iov.iov_len as usize);
        if n < 0 {
            return if total > 0 { total } else { n };
        }
        total = total.saturating_add(n);
        if (n as u64) < iov.iov_len {
            break;
        }
    }
    total
}

fn sys_dup(oldfd: i32) -> i64 {
    let entry = match get_open_file(oldfd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let res = tbl.lock().alloc_fd(
        0,
        FdEntry {
            file: entry.file,
            cloexec: false,
        },
    );
    match res {
        Ok(fd) => fd as i64,
        Err(e) => e.to_errno(),
    }
}

#[cfg(target_arch = "x86_64")]
fn sys_dup2(oldfd: i32, newfd: i32) -> i64 {
    if oldfd < 0 || newfd < 0 {
        return -abi::EBADF;
    }
    if oldfd == newfd {
        return match get_open_file(oldfd) {
            Ok(_) => newfd as i64,
            Err(e) => e.to_errno(),
        };
    }
    sys_dup3(oldfd, newfd, 0)
}

fn sys_dup3(oldfd: i32, newfd: i32, flags: u32) -> i64 {
    if oldfd < 0 || newfd < 0 || oldfd == newfd || (flags & !abi::O_CLOEXEC) != 0 {
        return -abi::EINVAL;
    }
    let entry = match get_open_file(oldfd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    let cloexec = (flags & abi::O_CLOEXEC) != 0;
    let tbl = vfs::current_fd_table();
    let res = tbl.lock().insert_at(
        newfd as usize,
        FdEntry {
            file: entry.file,
            cloexec,
        },
    );
    match res {
        Ok(fd) => fd as i64,
        Err(e) => e.to_errno(),
    }
}

fn sys_pipe2(pipefd_ptr: *mut i32, flags: u32) -> i64 {
    if (flags & !(abi::O_CLOEXEC | abi::O_NONBLOCK)) != 0 {
        return -abi::EINVAL;
    }
    if pipefd_ptr.is_null() {
        return -abi::EFAULT;
    }
    let cloexec = (flags & abi::O_CLOEXEC) != 0;
    let (reader, writer) = vfs::create_pipe_pair_with_flags(flags);
    let tbl = vfs::current_fd_table();
    let mut guard = tbl.lock();
    let rfd = match guard.alloc_fd(
        0,
        FdEntry {
            file: reader,
            cloexec,
        },
    ) {
        Ok(fd) => fd,
        Err(e) => return e.to_errno(),
    };
    let wfd = match guard.alloc_fd(
        0,
        FdEntry {
            file: writer,
            cloexec,
        },
    ) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = guard.close(rfd);
            return e.to_errno();
        }
    };
    drop(guard);
    if let Err(e) = uaccess::write_value(pipefd_ptr as u64, &[rfd as i32, wfd as i32]) {
        let mut fds = tbl.lock();
        let _ = fds.close(rfd);
        let _ = fds.close(wfd);
        return e.to_errno();
    }
    0
}

/// Linux sendfile(2): copy between file descriptors without bouncing through
/// a user buffer.  With a non-NULL offset, `in_fd`'s open-file position is
/// left unchanged and `*offset` is advanced instead.
fn sys_sendfile(out_fd: i32, in_fd: i32, offset_ptr: u64, count: usize) -> i64 {
    if count == 0 {
        return 0;
    }
    let output = match get_open_file(out_fd) {
        Ok(entry) => entry,
        Err(error) => return error.to_errno(),
    };
    let input = match get_open_file(in_fd) {
        Ok(entry) => entry,
        Err(error) => return error.to_errno(),
    };
    let mut position = if offset_ptr == 0 {
        None
    } else {
        match uaccess::read_value::<u64>(offset_ptr) {
            Ok(position) => Some(position),
            Err(error) => return error.to_errno(),
        }
    };
    let mut buffer = alloc::vec![0u8; 64 * 1024];
    let mut total = 0usize;
    let store_offset = |position: Option<u64>| -> i64 {
        if let (Some(position), true) = (position, offset_ptr != 0) {
            match uaccess::write_value(offset_ptr, &position) {
                Ok(()) => 0,
                Err(error) => error.to_errno(),
            }
        } else {
            0
        }
    };
    while total < count {
        let want = (count - total).min(buffer.len());
        let read = match position {
            Some(offset) => match input.file.pread(offset, &mut buffer[..want]) {
                Ok(0) => break,
                Ok(read) => {
                    position = Some(offset.saturating_add(read as u64));
                    read
                }
                Err(error) => {
                    let stored = store_offset(position);
                    return if stored != 0 {
                        stored
                    } else {
                        error.to_errno()
                    };
                }
            },
            None => match input.file.read(&mut buffer[..want]) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => return error.to_errno(),
            },
        };
        let mut written = 0usize;
        while written < read {
            match output.file.write(&buffer[written..read]) {
                Ok(0) => break,
                Ok(bytes) => written += bytes,
                Err(VfsError::Interrupted) => continue,
                Err(error) => {
                    if total == 0 && written == 0 {
                        let stored = store_offset(position);
                        return if stored != 0 {
                            stored
                        } else {
                            error.to_errno()
                        };
                    }
                    break;
                }
            }
        }
        total = total.saturating_add(written);
        if written < read {
            break;
        }
    }
    let stored = store_offset(position);
    if stored != 0 { stored } else { total as i64 }
}

fn sys_fcntl(fd: i32, cmd: u32, arg: u64) -> i64 {
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    match cmd {
        abi::F_DUPFD | abi::F_DUPFD_CLOEXEC => {
            let cloexec = cmd == abi::F_DUPFD_CLOEXEC;
            let res = tbl.lock().alloc_fd(
                arg as usize,
                FdEntry {
                    file: entry.file,
                    cloexec,
                },
            );
            match res {
                Ok(new_fd) => new_fd as i64,
                Err(e) => e.to_errno(),
            }
        }
        abi::F_GETFD => {
            if entry.cloexec {
                abi::FD_CLOEXEC as i64
            } else {
                0
            }
        }
        abi::F_SETFD => {
            let cloexec = (arg as u32 & abi::FD_CLOEXEC) != 0;
            match tbl.lock().set_cloexec(fd as usize, cloexec) {
                Ok(()) => 0,
                Err(e) => e.to_errno(),
            }
        }
        abi::F_ADD_SEALS => match crate::ipc::memfd::instance(fd as u64) {
            Ok(memfd) => match memfd.add_seals(arg as u32) {
                Ok(()) => 0,
                Err(error) => error.to_errno(),
            },
            Err(error) => error.to_errno(),
        },
        abi::F_GET_SEALS => match crate::ipc::memfd::instance(fd as u64) {
            Ok(memfd) => memfd.seals() as i64,
            Err(error) => error.to_errno(),
        },
        abi::F_GETFL => entry.file.flags() as i64,
        abi::F_SETFL => match entry.file.set_flags(arg as u32) {
            Ok(()) => 0,
            Err(e) => e.to_errno(),
        },
        _ => -abi::EINVAL,
    }
}

const TERMIOS_ICRNL: u32 = 0x100;
const TERMIOS_ISIG: u32 = 0x1;
const TERMIOS_ICANON: u32 = 0x2;
const TERMIOS_ECHO: u32 = 0x8;
const TERMIOS_CS8: u32 = 0x30;
const TERMIOS_CREAD: u32 = 0x80;

fn encode_termios(settings: &crate::tty::Termios) -> [u8; 36] {
    let mut bytes = [0u8; 36];
    let iflag = if settings.icrnl { TERMIOS_ICRNL } else { 0 };
    let cflag = TERMIOS_CS8 | TERMIOS_CREAD;
    let mut lflag = 0;
    if settings.signals {
        lflag |= TERMIOS_ISIG;
    }
    if settings.canonical {
        lflag |= TERMIOS_ICANON;
    }
    if settings.echo {
        lflag |= TERMIOS_ECHO;
    }
    bytes[0..4].copy_from_slice(&iflag.to_le_bytes());
    bytes[8..12].copy_from_slice(&cflag.to_le_bytes());
    bytes[12..16].copy_from_slice(&lflag.to_le_bytes());
    bytes[17] = settings.interrupt;
    bytes[18] = settings.quit;
    bytes[19] = settings.erase;
    bytes[20] = settings.kill;
    bytes[21] = settings.eof;
    bytes[23] = 1; // VMIN
    bytes[27] = settings.suspend;
    bytes
}

fn decode_termios(bytes: &[u8]) -> crate::tty::Termios {
    let iflag = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let lflag = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    crate::tty::Termios {
        canonical: lflag & TERMIOS_ICANON != 0,
        echo: lflag & TERMIOS_ECHO != 0,
        signals: lflag & TERMIOS_ISIG != 0,
        icrnl: iflag & TERMIOS_ICRNL != 0,
        erase: bytes[19],
        kill: bytes[20],
        eof: bytes[21],
        interrupt: bytes[17],
        quit: bytes[18],
        suspend: bytes[27],
    }
}

const FIONBIO: u64 = 0x5421;
const FIONCLEX: u64 = 0x5450;
const FIOCLEX: u64 = 0x5451;
const FIOASYNC: u64 = 0x5452;
const TIOCSCTTY: u64 = 0x540E;
const TIOCNOTTY: u64 = 0x5422;

fn sys_ioctl(fd: i32, cmd: u32, arg: u64) -> i64 {
    // Linux declares the request as `unsigned int`; musl passes its 32-bit
    // `int` request through the 64-bit syscall register, so the sign-extended
    // high half must be discarded before comparing against _IOC-encoded
    // numbers whose direction bits set bit 31.
    let cmd = u64::from(cmd);
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    match cmd {
        // Generic descriptor ioctls, valid on every file type.
        FIONBIO => {
            let enable = match uaccess::read_value::<i32>(arg) {
                Ok(value) => value != 0,
                Err(e) => return e.to_errno(),
            };
            let flags = entry.file.flags();
            let updated = if enable {
                flags | abi::O_NONBLOCK
            } else {
                flags & !abi::O_NONBLOCK
            };
            return match entry.file.set_flags(updated) {
                Ok(()) => 0,
                Err(e) => e.to_errno(),
            };
        }
        FIOCLEX | FIONCLEX => {
            let result = vfs::current_fd_table()
                .lock()
                .set_cloexec(fd as usize, cmd == FIOCLEX);
            return match result {
                Ok(()) => 0,
                Err(e) => e.to_errno(),
            };
        }
        FIOASYNC => return 0,
        _ => {}
    }
    let console_vt = entry.file.inode.console_vt();
    if let Some(vt) = console_vt
        && let Some(result) = crate::tty::vt::ioctl(vt, cmd, arg)
    {
        return match result {
            Ok(value) => value as i64,
            Err(e) => e.to_errno(),
        };
    }
    if console_vt.is_some() && cmd == abi::TIOCGWINSZ {
        let status = crate::tty::status();
        let ws = WinSize {
            ws_row: status.rows.max(25) as u16,
            ws_col: status.columns.max(80) as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        return match uaccess::write_value(arg, &ws) {
            Ok(()) => 0,
            Err(e) => e.to_errno(),
        };
    }
    let console = console_vt.is_some();
    if console && cmd == TIOCNOTTY {
        let Some(task) = sched::current() else {
            return -abi::ESRCH;
        };
        let stats = task.stats();
        if crate::tty::controlling_session() != stats.sid {
            return -abi::ENOTTY;
        }
        if stats.tgid == stats.sid {
            crate::tty::release_session(stats.sid);
        }
        return 0;
    }
    if console && cmd == TIOCSCTTY {
        let Some(task) = sched::current() else {
            return -abi::ESRCH;
        };
        let stats = task.stats();
        if stats.tgid != stats.sid {
            return -abi::EPERM;
        }
        if crate::tty::controlling_session() == stats.sid {
            return 0;
        }
        if !crate::tty::assign_session(stats.sid) {
            if arg != 1 || task.credentials().euid != 0 {
                return -abi::EPERM;
            }
            crate::tty::release_session(crate::tty::controlling_session());
            crate::tty::assign_session(stats.sid);
        }
        crate::tty::set_foreground_pgrp(stats.sid, stats.pgid);
        return 0;
    }
    if console && matches!(cmd, abi::TCGETS | abi::TCSETS | abi::TCSETSW | abi::TCSETSF) {
        if cmd == abi::TCGETS {
            let settings = match crate::tty::termios(crate::tty::TtyDevice::Foreground) {
                Ok(settings) => settings,
                Err(_) => return -abi::ENOTTY,
            };
            let bytes = encode_termios(&settings);
            return match uaccess::write(arg, &bytes) {
                Ok(()) => 0,
                Err(e) => e.to_errno(),
            };
        }
        let bytes = match uaccess::read_vec(arg, 36, 36) {
            Ok(bytes) => bytes,
            Err(e) => return e.to_errno(),
        };
        let settings = decode_termios(&bytes);
        // The console merges the serial and framebuffer input paths, so the
        // controlling terminal's termios applies to both queues.
        let foreground = crate::tty::set_termios(crate::tty::TtyDevice::Foreground, settings);
        let serial = crate::tty::set_termios(crate::tty::TtyDevice::Serial0, settings);
        return if foreground.is_ok() || serial.is_ok() {
            0
        } else {
            -abi::ENOTTY
        };
    }
    // Only the console is handled here; other terminals (pty slaves) implement
    // these requests in their own `Inode::ioctl`.
    if console && matches!(cmd, abi::TIOCGPGRP | abi::TIOCSPGRP | abi::TIOCGSID) {
        let Some(task) = sched::current() else {
            return -abi::ESRCH;
        };
        let stats = task.stats();
        if !stats.user {
            return -abi::ENOTTY;
        }
        match cmd {
            abi::TIOCGPGRP => {
                if crate::tty::controlling_session() != stats.sid {
                    return -abi::ENOTTY;
                }
                let pgrp = crate::tty::foreground_pgrp() as u32;
                return match uaccess::write_value(arg, &pgrp) {
                    Ok(()) => 0,
                    Err(e) => e.to_errno(),
                };
            }
            abi::TIOCGSID => {
                if crate::tty::controlling_session() != stats.sid {
                    return -abi::ENOTTY;
                }
                let sid = stats.sid as u32;
                return match uaccess::write_value(arg, &sid) {
                    Ok(()) => 0,
                    Err(e) => e.to_errno(),
                };
            }
            _ => {
                if crate::tty::controlling_session() != stats.sid {
                    return -abi::ENOTTY;
                }
                let pgrp = match uaccess::read_value::<i32>(arg) {
                    Ok(pgrp) if pgrp >= 0 => pgrp as u64,
                    Ok(_) => return -abi::EINVAL,
                    Err(e) => return e.to_errno(),
                };
                let target = match sched::tasks().into_iter().find(|candidate| {
                    let other = candidate.stats();
                    other.user && (other.pgid == pgrp || other.tgid == pgrp || other.ltid == pgrp)
                }) {
                    Some(target) => target,
                    None => return -abi::ESRCH,
                };
                if target.stats().sid != stats.sid {
                    return -abi::EPERM;
                }
                // Linux tty_check_change(): an orphaned background group gets
                // EIO; otherwise SIGTTOU is queued (if not ignored) and the
                // request still proceeds.  Interactive shells ignore SIGTTOU
                // so they can hand the foreground group back and forth.
                if stats.pgid != crate::tty::foreground_pgrp() {
                    if crate::signal::pgrp_orphaned(stats.pgid, stats.sid) {
                        return -abi::EIO;
                    }
                    if !crate::signal::signal_ignored(&task, crate::signal::SIGTTOU) {
                        let _ = crate::signal::send_to_pgrp(stats.pgid, crate::signal::SIGTTOU);
                    }
                }
                if !crate::tty::set_foreground_pgrp(stats.sid, pgrp) {
                    return -abi::EPERM;
                }
                return 0;
            }
        }
    }
    match entry.file.inode.ioctl(cmd, arg) {
        Ok(val) => {
            if cmd == abi::BLKGETSIZE64 || cmd == abi::BLKGETSIZE {
                return match uaccess::write_value(arg, &val) {
                    Ok(()) => 0,
                    Err(e) => e.to_errno(),
                };
            }
            if cmd == abi::BLKSSZGET {
                return match uaccess::write_value(arg, &(val as u32)) {
                    Ok(()) => 0,
                    Err(e) => e.to_errno(),
                };
            }
            val as i64
        }
        Err(e) => e.to_errno(),
    }
}

fn sys_fstat(fd: i32, stat_ptr: *mut LinuxStat) -> i64 {
    if stat_ptr.is_null() {
        return -abi::EFAULT;
    }
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    match entry.file.inode.metadata() {
        Ok(meta) => {
            let lstat = LinuxStat::from(meta);
            match uaccess::write_value(stat_ptr as u64, &lstat) {
                Ok(()) => 0,
                Err(e) => e.to_errno(),
            }
        }
        Err(e) => e.to_errno(),
    }
}

fn sys_newfstatat(dirfd: i32, path_ptr: *const u8, stat_ptr: *mut LinuxStat, flags: u32) -> i64 {
    if stat_ptr.is_null() {
        return -abi::EFAULT;
    }
    if (flags & abi::AT_EMPTY_PATH) != 0 && !path_ptr.is_null() {
        let b0 = match uaccess::read_value::<u8>(path_ptr as u64) {
            Ok(b) => b,
            Err(e) => return e.to_errno(),
        };
        if b0 == 0 {
            return sys_fstat(dirfd, stat_ptr);
        }
    }
    let path_str = match read_c_string(path_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_path = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    let follow = (flags & abi::AT_SYMLINK_NOFOLLOW) == 0;
    let (inode, _) = match vfs::lookup_abs(&abs_path, follow) {
        Ok(r) => r,
        Err(VfsError::NotFound) => {
            if let Some(meta) = crate::ipc::unix::path_metadata(&abs_path) {
                let lstat = LinuxStat::from(meta);
                return match uaccess::write_value(stat_ptr as u64, &lstat) {
                    Ok(()) => 0,
                    Err(e) => e.to_errno(),
                };
            }
            return -abi::ENOENT;
        }
        Err(e) => return e.to_errno(),
    };
    match inode.metadata() {
        Ok(meta) => {
            let lstat = LinuxStat::from(meta);
            match uaccess::write_value(stat_ptr as u64, &lstat) {
                Ok(()) => 0,
                Err(e) => e.to_errno(),
            }
        }
        Err(e) => e.to_errno(),
    }
}

fn sys_getdents64(fd: i32, dirp: *mut u8, count: usize) -> i64 {
    if dirp.is_null() {
        return -abi::EFAULT;
    }
    if count < 32 {
        return -abi::EINVAL;
    }
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    let mut kbuf = vec![0u8; count.min(32768)];
    match entry.file.getdents64(&mut kbuf) {
        Ok(written) => {
            if written > 0 {
                if let Err(e) = uaccess::write(dirp as u64, &kbuf[..written]) {
                    return e.to_errno();
                }
            }
            written as i64
        }
        Err(e) => e.to_errno(),
    }
}

fn sys_mkdirat(dirfd: i32, path_ptr: *const u8, mode: u32) -> i64 {
    let path_str = match read_c_string(path_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_path = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    match vfs::mkdir_abs(&abs_path, mode) {
        Ok(_) => 0,
        Err(e) => e.to_errno(),
    }
}

fn sys_unlinkat(dirfd: i32, path_ptr: *const u8, flags: u32) -> i64 {
    let path_str = match read_c_string(path_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_path = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    let remove_dir = (flags & abi::AT_REMOVEDIR) != 0;
    match vfs::unlink_abs(&abs_path, remove_dir) {
        Ok(()) => 0,
        Err(VfsError::NotFound) if !remove_dir => {
            // Pathname AF_UNIX sockets have no VFS directory entry in this
            // single-namespace implementation; unlink removes the binding.
            if crate::ipc::unix::unbind_path(&abs_path) {
                0
            } else {
                -abi::ENOENT
            }
        }
        Err(e) => e.to_errno(),
    }
}

fn sys_symlinkat(target_ptr: *const u8, newdirfd: i32, linkpath_ptr: *const u8) -> i64 {
    let target = match read_c_string(target_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let link_str = match read_c_string(linkpath_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_link = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, newdirfd, &link_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    match vfs::symlink_abs(&target, &abs_link) {
        Ok(_) => 0,
        Err(e) => e.to_errno(),
    }
}

fn sys_readlinkat(dirfd: i32, path_ptr: *const u8, buf: *mut u8, bufsiz: usize) -> i64 {
    if buf.is_null() {
        return -abi::EFAULT;
    }
    if bufsiz == 0 {
        return -abi::EINVAL;
    }
    let path_str = match read_c_string(path_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_path = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    match vfs::readlink_abs(&abs_path) {
        Ok(target) => {
            let bytes = target.as_bytes();
            let copy_len = bytes.len().min(bufsiz);
            if let Err(e) = uaccess::write(buf as u64, &bytes[..copy_len]) {
                return e.to_errno();
            }
            copy_len as i64
        }
        Err(e) => e.to_errno(),
    }
}

fn sys_faccessat(dirfd: i32, path_ptr: *const u8, mode: u32, flags: u32) -> i64 {
    if mode & !7 != 0 || flags & !(abi::AT_SYMLINK_NOFOLLOW | abi::AT_EACCESS) != 0 {
        return -abi::EINVAL;
    }
    let path_str = match read_c_string(path_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_path = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    let follow = (flags & abi::AT_SYMLINK_NOFOLLOW) == 0;
    match vfs::lookup_abs(&abs_path, follow).and_then(|(inode, _)| inode.metadata()) {
        Ok(meta) => {
            if (mode & 4 != 0 && meta.mode & 0o444 == 0)
                || (mode & 2 != 0 && meta.mode & 0o222 == 0)
                || (mode & 1 != 0 && meta.mode & 0o111 == 0)
            {
                return -abi::EACCES;
            }
            0
        }
        Err(e) => e.to_errno(),
    }
}

fn sys_ftruncate(fd: i32, length: u64) -> i64 {
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    if !entry.file.can_write() {
        return -abi::EINVAL;
    }
    match entry.file.inode.truncate(length) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

fn sys_getcwd(buf: *mut u8, size: usize) -> i64 {
    if buf.is_null() {
        return -abi::EFAULT;
    }
    let cwd = vfs::current_cwd();
    let bytes = cwd.as_bytes();
    if bytes.len() + 1 > size {
        return -abi::ERANGE;
    }
    let mut terminated = bytes.to_vec();
    terminated.push(0);
    if let Err(e) = uaccess::write(buf as u64, &terminated) {
        return e.to_errno();
    }
    (bytes.len() + 1) as i64
}

fn sys_chdir(path_ptr: *const u8) -> i64 {
    let path_str = match read_c_string(path_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_path = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, abi::AT_FDCWD, &path_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    let (inode, final_path) = match vfs::lookup_abs(&abs_path, true) {
        Ok(r) => r,
        Err(e) => return e.to_errno(),
    };
    match inode.metadata() {
        Ok(meta) if meta.kind == vfs::InodeKind::Directory => {
            vfs::set_current_cwd(final_path);
            0
        }
        Ok(_) => -abi::ENOTDIR,
        Err(e) => e.to_errno(),
    }
}

fn sys_fsync(fd: i32) -> i64 {
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    match entry.file.inode.sync() {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

fn sys_statfs(path_ptr: *const u8, buf_ptr: *mut LinuxStatFs) -> i64 {
    if buf_ptr.is_null() {
        return -abi::EFAULT;
    }
    let path_str = match read_c_string(path_ptr, 512) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let tbl = vfs::current_fd_table();
    let abs_path = {
        let guard = tbl.lock();
        match vfs::resolve_at_path(&guard, abi::AT_FDCWD, &path_str) {
            Ok(p) => p,
            Err(e) => return e.to_errno(),
        }
    };
    let (mount, _) = match vfs::find_mount(&abs_path) {
        Ok(m) => m,
        Err(e) => return e.to_errno(),
    };
    match mount.fs.statfs() {
        Ok(sfs) => match uaccess::write_value(buf_ptr as u64, &LinuxStatFs::from(sfs)) {
            Ok(()) => 0,
            Err(e) => e.to_errno(),
        },
        Err(e) => e.to_errno(),
    }
}

fn sys_fstatfs(fd: i32, buf_ptr: *mut LinuxStatFs) -> i64 {
    if buf_ptr.is_null() {
        return -abi::EFAULT;
    }
    let entry = match get_open_file(fd) {
        Ok(e) => e,
        Err(e) => return e.to_errno(),
    };
    let (mount, _) = match vfs::find_mount(&entry.file.abs_path) {
        Ok(m) => m,
        Err(e) => return e.to_errno(),
    };
    match mount.fs.statfs() {
        Ok(sfs) => match uaccess::write_value(buf_ptr as u64, &LinuxStatFs::from(sfs)) {
            Ok(()) => 0,
            Err(e) => e.to_errno(),
        },
        Err(e) => e.to_errno(),
    }
}

fn sys_mount(source_ptr: *const u8, target_ptr: *const u8, fstype_ptr: *const u8) -> i64 {
    let source = if source_ptr.is_null() {
        String::from("none")
    } else {
        match read_c_string(source_ptr, 256) {
            Ok(s) => s,
            Err(e) => return e.to_errno(),
        }
    };
    let target = match read_c_string(target_ptr, 256) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let fstype = match read_c_string(fstype_ptr, 64) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    match vfs::mount_by_type(&source, &target, &fstype) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

fn sys_umount2(target_ptr: *const u8, _flags: u32) -> i64 {
    let target = match read_c_string(target_ptr, 256) {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    match vfs::umount(&target) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

fn sys_sched_yield() -> i64 {
    sched::yield_now();
    0
}

fn sys_nanosleep(req_ptr: u64) -> i64 {
    if req_ptr == 0 {
        return -abi::EFAULT;
    }
    let ts = match uaccess::read_value::<[u64; 2]>(req_ptr) {
        Ok(ts) => ts,
        Err(e) => return e.to_errno(),
    };
    let [sec, nsec] = ts;
    if sec > i64::MAX as u64 || nsec >= 1_000_000_000 {
        return -abi::EINVAL;
    }
    let micros = sec.saturating_mul(1_000_000).saturating_add(nsec / 1_000);
    let mut remaining = micros;
    while remaining != 0 {
        if crate::signal::pending_for_current() {
            return -abi::EINTR;
        }
        let slice = remaining.min(1000);
        sched::sleep_micros(slice);
        remaining -= slice;
    }
    0
}

fn sys_exit(code: i32, group: bool) -> i64 {
    if let Some(task) = sched::current() {
        let stats = task.stats();
        if group || stats.ltid == stats.tgid {
            crate::tty::vt::release_owner(stats.tgid);
        }
    }
    let leader = sched::current()
        .map(|task| {
            let stats = task.stats();
            stats.ltid == stats.tgid
        })
        .unwrap_or(true);
    if leader {
        crate::verbose::log(format_args!(
            "Linux ABI: user process exited with code {}",
            code
        ));
        set_last_user_exit_code(code);
    }
    // wait(2) encodes a normal exit in bits 8..15.
    let status = (code & 0xff) << 8;
    if group {
        sched::exit_group(status)
    } else {
        sched::exit(status)
    }
}

fn sys_set_tid_address(tidptr: u64) -> i64 {
    process::set_tid_address(tidptr)
}

fn sys_brk(addr: u64) -> i64 {
    let space = match uaccess::current_space() {
        Ok(s) => s,
        Err(e) => return e.to_errno(),
    };
    let mut guard = space.lock();
    if addr == 0 {
        guard.brk as i64
    } else {
        guard.resize_brk(addr) as i64
    }
}

#[cfg(target_arch = "x86_64")]
fn sys_arch_prctl(code: u32, addr: u64) -> i64 {
    const ARCH_SET_FS: u32 = 0x1002;
    const ARCH_GET_FS: u32 = 0x1003;
    const IA32_FS_BASE: u32 = 0xc000_0100;

    match code {
        ARCH_SET_FS => {
            if addr >= crate::user::UserAddressSpace::user_limit() {
                return -abi::EPERM;
            }
            unsafe {
                core::arch::asm!(
                    "wrmsr",
                    in("ecx") IA32_FS_BASE,
                    in("eax") (addr as u32),
                    in("edx") ((addr >> 32) as u32),
                    options(nostack)
                );
            }
            0
        }
        ARCH_GET_FS => {
            let (low, high): (u32, u32);
            unsafe {
                core::arch::asm!(
                    "rdmsr",
                    in("ecx") IA32_FS_BASE,
                    out("eax") low,
                    out("edx") high,
                    options(nomem, nostack)
                );
            }
            let val = ((high as u64) << 32) | (low as u64);
            if addr != 0 {
                if let Err(e) = uaccess::write_value(addr, &val) {
                    return e.to_errno();
                }
            }
            0
        }
        _ => -abi::EINVAL,
    }
}

#[cfg(feature = "boot-self-test")]
pub fn run_vfs_syscall_self_test() {
    // Exercise Linux ABI syscalls directly through `dispatch`
    let path = b"/mnt/syscall_test.txt\0";
    let fd = dispatch(
        SYS_OPENAT,
        abi::AT_FDCWD as i64 as u64,
        path.as_ptr() as u64,
        (abi::O_CREAT | abi::O_RDWR | abi::O_TRUNC) as u64,
        0o644,
        0,
        0,
    );
    assert!(fd >= 3, "syscall SYS_OPENAT failed: {}", fd);

    let payload = b"linux-abi-vfs-syscall-verified";
    let written = dispatch(
        SYS_WRITE,
        fd as u64,
        payload.as_ptr() as u64,
        payload.len() as u64,
        0,
        0,
        0,
    );
    assert_eq!(written, payload.len() as i64);

    let mut st = LinuxStat::default();
    let fstat_rc = dispatch(
        SYS_FSTAT,
        fd as u64,
        (&mut st as *mut LinuxStat) as u64,
        0,
        0,
        0,
        0,
    );
    assert_eq!(fstat_rc, 0);
    assert_eq!(st.st_size, payload.len() as i64);

    let pos = dispatch(SYS_LSEEK, fd as u64, 0, abi::SEEK_SET as u64, 0, 0, 0);
    assert_eq!(pos, 0);

    let mut read_buf = [0u8; 64];
    let read_n = dispatch(
        SYS_READ,
        fd as u64,
        read_buf.as_mut_ptr() as u64,
        payload.len() as u64,
        0,
        0,
        0,
    );
    assert_eq!(read_n, payload.len() as i64);
    assert_eq!(&read_buf[..payload.len()], payload);

    // Test getdents64 on /mnt
    let mnt_path = b"/mnt\0";
    let dir_fd = dispatch(
        SYS_OPENAT,
        abi::AT_FDCWD as i64 as u64,
        mnt_path.as_ptr() as u64,
        (abi::O_RDONLY | abi::O_DIRECTORY_GENERIC) as u64,
        0,
        0,
        0,
    );
    assert!(dir_fd >= 3);
    let mut dirent_buf = [0u8; 512];
    let dents_n = dispatch(
        SYS_GETDENTS64,
        dir_fd as u64,
        dirent_buf.as_mut_ptr() as u64,
        dirent_buf.len() as u64,
        0,
        0,
        0,
    );
    assert!(dents_n > 0, "SYS_GETDENTS64 returned {}", dents_n);
    assert_eq!(dispatch(SYS_CLOSE, dir_fd as u64, 0, 0, 0, 0, 0), 0);

    // Test pipe2, dup, close, unlinkat
    let mut pipefds = [0i32; 2];
    assert_eq!(
        dispatch(SYS_PIPE2, pipefds.as_mut_ptr() as u64, 0, 0, 0, 0, 0),
        0
    );
    let msg = b"pipe-ok";
    assert_eq!(
        dispatch(
            SYS_WRITE,
            pipefds[1] as u64,
            msg.as_ptr() as u64,
            msg.len() as u64,
            0,
            0,
            0
        ),
        msg.len() as i64
    );
    let mut pbuf = [0u8; 16];
    assert_eq!(
        dispatch(
            SYS_READ,
            pipefds[0] as u64,
            pbuf.as_mut_ptr() as u64,
            msg.len() as u64,
            0,
            0,
            0
        ),
        msg.len() as i64
    );
    assert_eq!(&pbuf[..msg.len()], msg);
    assert_eq!(dispatch(SYS_CLOSE, pipefds[0] as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, pipefds[1] as u64, 0, 0, 0, 0, 0), 0);

    assert_eq!(dispatch(SYS_FSYNC, fd as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, fd as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(
        dispatch(
            SYS_UNLINKAT,
            abi::AT_FDCWD as i64 as u64,
            path.as_ptr() as u64,
            0,
            0,
            0,
            0,
        ),
        0
    );

    crate::verbose::log(format_args!("Linux ABI VFS syscall self-test passed"));
}
