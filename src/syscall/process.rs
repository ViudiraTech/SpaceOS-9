/*
 *       src/syscall/process.rs
 *       Transactional execve, polling, clocks and random ABI boundaries
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
use crate::{
    arch, sched,
    sync::SpinLock,
    user::{UserAddressSpace, elf, stack, uaccess},
    vfs::{self, VfsError, abi},
};
use alloc::{string::String, sync::Arc, vec::Vec};
fn strings(pointer: u64) -> Result<Vec<String>, VfsError> {
    let mut result = Vec::new();
    let mut size = 0;
    if pointer == 0 {
        return Ok(result);
    }
    for i in 0..4096 {
        let address = pointer.checked_add(i * 8).ok_or(VfsError::Fault)?;
        let string = uaccess::read_value::<u64>(address)?;
        if string == 0 {
            return Ok(result);
        }
        let value = uaccess::string(string, 65536)?;
        size += value.len() + 1;
        if size > 128 * 1024 {
            return Err(VfsError::NoSpace);
        }
        result.push(value);
    }
    Err(VfsError::NoSpace)
}
pub fn execve(path: u64, argv: u64, envp: u64) -> i64 {
    let prepared = (|| -> Result<_, VfsError> {
        let path = uaccess::string(path, 4096)?;
        let cwd = vfs::current_cwd();
        let path = vfs::normalize_path(&cwd, &path);
        let (inode, _) = vfs::lookup_abs(&path, true)?;
        let meta = inode.metadata()?;
        if meta.kind != vfs::InodeKind::Regular || meta.mode & 0o111 == 0 {
            return Err(VfsError::PermissionDenied);
        }
        let argv = strings(argv)?;
        let env = strings(envp)?;
        let data = vfs::read_file(&path)?;
        let mut space = UserAddressSpace::new().map_err(|_| VfsError::NoMemory)?;
        let loaded = elf::load_elf(&mut space, &data).map_err(|_| VfsError::ExecFormat)?;
        let args: Vec<_> = argv.iter().map(|s| s.as_str()).collect();
        let environment: Vec<_> = env.iter().map(|s| s.as_str()).collect();
        let sp = stack::setup(&mut space, &loaded, &path, &args, &environment)
            .map_err(|_| VfsError::NoSpace)?;
        Ok((
            Arc::new(SpinLock::new(space)),
            loaded.initial_pc,
            sp,
            loaded.tls_tp,
            path,
        ))
    })();
    let (space, pc, sp, tls_tp, path) = match prepared {
        Ok(p) => p,
        Err(e) => return e.to_errno(),
    };
    let task = match sched::current() {
        Some(t) => t,
        None => return -abi::ESRCH,
    };
    task.set_comm_from_path(&path);
    let old = task.address_space();
    // No fallible operation remains after descriptor/image publication.
    if let Err(error) = crate::signal::prepare_exec(&task) {
        return error.to_errno();
    }
    if task.files_shared() {
        let replacement = Arc::new(SpinLock::new(task.files().lock().clone()));
        let mut locked = task.lock();
        locked.fd_table = replacement;
        locked.files_shared = false;
    }
    task.files().lock().close_on_exec();
    arch::disable_interrupts();
    {
        let mut t = task.lock();
        t.set_address_space(space.clone());
        if let Some(top) = t.kernel_stack_top() {
            arch::set_kernel_stack(crate::smp::current_cpu(), top as u64);
        }
        // SAFETY: this CPU exclusively owns the running task, IRQs are disabled.
        unsafe {
            if let Some(state) = (&mut *t.fpu_state.get()).as_mut() {
                state.reset().expect("valid task FPU reset");
                crate::fpu::restore_current(state).expect("valid task FPU restore");
            }
        }
    }
    space.lock().activate();
    drop(old);
    // enter_user_mode never returns, so anything still owned by this frame would
    // never be dropped; a leaked space would keep every page of this image alive.
    drop(space);
    drop(path);
    drop(task);
    unsafe {
        crate::user::process::install_tls(tls_tp);
        let _ = crate::fpu::enable_user();
        arch::enter_user_mode(pc, sp)
    }
}
pub fn poll(pointer: u64, count: u64, timeout_ms: i64) -> i64 {
    let result = (|| -> Result<usize, VfsError> {
        let count = usize::try_from(count).map_err(|_| VfsError::InvalidArgument)?;
        if count > 1024 {
            return Err(VfsError::InvalidArgument);
        }
        let mut bytes = uaccess::read_vec(pointer, count * 8, 8192)?;
        uaccess::check(pointer, bytes.len(), true)?;
        let deadline = if timeout_ms < 0 {
            u64::MAX
        } else {
            crate::time::uptime_micros().saturating_add((timeout_ms as u64).saturating_mul(1000))
        };
        let mut wait = crate::ipc::pollwait::PollWait::new();
        loop {
            if wait.registered() {
                let mut sources = crate::ipc::pollwait::PollSources::new();
                if deadline != u64::MAX {
                    sources.expire_at(deadline);
                }
                let table = vfs::current_fd_table();
                let table = table.lock();
                for item in bytes.chunks_exact(8) {
                    let fd = i32::from_le_bytes(item[..4].try_into().unwrap());
                    if fd >= 0
                        && let Some(entry) = table.get(fd as usize)
                    {
                        entry.file.inode.poll_sources(&mut sources);
                    }
                }
                drop(table);
                wait.arm(&sources);
            }
            let mut ready = 0;
            for item in bytes.chunks_exact_mut(8) {
                let fd = i32::from_le_bytes(item[..4].try_into().unwrap());
                let requested = u16::from_le_bytes(item[4..6].try_into().unwrap()) as u32;
                let available = if fd < 0 {
                    0
                } else {
                    match vfs::current_fd_table().lock().get(fd as usize) {
                        Some(e) => e.file.inode.poll(),
                        None => 0x20,
                    }
                };
                let events = available & (requested | 8 | 16 | 0x20);
                item[6..8].copy_from_slice(&(events as u16).to_le_bytes());
                ready += usize::from(events != 0);
            }
            if ready != 0 || crate::time::uptime_micros() >= deadline {
                uaccess::write(pointer, &bytes)?;
                return Ok(ready);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    })();
    match result {
        Ok(n) => n as i64,
        Err(e) => e.to_errno(),
    }
}
pub fn ppoll(pointer: u64, count: u64, timeout: u64, sigmask: u64, setsize: u64) -> i64 {
    if sigmask != 0 {
        if setsize != 8 {
            return -abi::EINVAL;
        }
        match uaccess::read_value::<u64>(sigmask) {
            Ok(mask) => crate::signal::install_wait_mask(mask),
            Err(e) => return e.to_errno(),
        }
    }
    let millis = if timeout == 0 {
        -1
    } else {
        let ts = match uaccess::read_value::<[u64; 2]>(timeout) {
            Ok(t) => t,
            Err(e) => return e.to_errno(),
        };
        if ts[0] > i64::MAX as u64 || ts[1] >= 1_000_000_000 {
            return -abi::EINVAL;
        }
        ts[0]
            .saturating_mul(1000)
            .saturating_add(ts[1].div_ceil(1_000_000))
            .min(i64::MAX as u64) as i64
    };
    poll(pointer, count, millis)
}
pub fn getrandom(pointer: u64, count: usize, flags: u32) -> i64 {
    if flags & !3 != 0 {
        return -abi::EINVAL;
    }
    let length = count.min(65536);
    if let Err(e) = uaccess::check(pointer, length, true) {
        return e.to_errno();
    }
    if length == 0 {
        return 0;
    }
    if !crate::random::supported() {
        return VfsError::WouldBlock.to_errno();
    }
    let mut bytes = alloc::vec![0; length];
    for _ in 0..64 {
        match crate::random::fill(&mut bytes) {
            Ok(()) => {
                return match uaccess::write(pointer, &bytes) {
                    Ok(()) => length as i64,
                    Err(e) => e.to_errno(),
                };
            }
            Err(VfsError::WouldBlock) if flags & 1 == 0 => sched::sleep_micros(1000),
            Err(e) => return e.to_errno(),
        }
    }
    VfsError::WouldBlock.to_errno()
}

/// Return the parent task id of the current process.
pub fn getppid() -> i64 {
    crate::sched::current()
        .map(|task| task.stats().ppid as i64)
        .unwrap_or(0)
}

pub fn getpid() -> i64 {
    crate::sched::current()
        .map(|task| task.stats().tgid as i64)
        .unwrap_or(0)
}

pub fn gettid() -> i64 {
    crate::sched::current()
        .map(|task| task.stats().ltid as i64)
        .unwrap_or(0)
}

const WNOHANG: u32 = 1;
const WUNTRACED: u32 = 2;
const WCONTINUED: u32 = 8;
const __WNOTHREAD: u32 = 0x2000_0000;
const __WCLONE: u32 = 0x8000_0000;
const __WALL: u32 = 0x4000_0000;

/// Linux wait4.  rusage accounting is not implemented yet and is rejected
/// rather than filled with fabricated values; stopped/continued states are
/// reported for WUNTRACED/WCONTINUED.
pub fn wait4(pid: i64, status: u64, options: u32, rusage: u64) -> i64 {
    if rusage != 0 {
        return -abi::ENOSYS;
    }
    if options & !(WNOHANG | WUNTRACED | WCONTINUED | __WNOTHREAD | __WCLONE | __WALL) != 0 {
        return -abi::EINVAL;
    }
    if pid == i32::MIN as i64 {
        return -abi::ESRCH;
    }
    // Normal wait4 ignores clone children whose exit signal is not SIGCHLD;
    // __WCLONE selects them and __WALL selects both.
    let exit_matches = |exit_signal: u32| -> bool {
        if options & __WALL != 0 {
            true
        } else if options & __WCLONE != 0 {
            exit_signal != crate::signal::SIGCHLD
        } else {
            exit_signal == crate::signal::SIGCHLD
        }
    };
    let Some(current) = crate::sched::current() else {
        return -abi::ESRCH;
    };
    let parent = current.stats().tgid;
    let pgid = current.stats().pgid;
    let matches = |child: u64, child_pgid: u64| -> bool {
        if pid == -1 {
            true
        } else if pid == 0 {
            child_pgid == pgid
        } else if pid > 0 {
            child == pid as u64
        } else {
            child_pgid == pid.unsigned_abs()
        }
    };
    let mut rechecked = false;
    loop {
        for zombie in crate::sched::zombies() {
            let stats = zombie.stats();
            if stats.ppid != parent
                || !matches(stats.tgid, stats.pgid)
                || !exit_matches(stats.exit_signal)
            {
                continue;
            }
            // A process with live threads cannot be reaped yet.
            if crate::sched::group_has_live_threads(stats.tgid) {
                continue;
            }
            let Some(zombie) = crate::sched::take_zombie(stats.tgid) else {
                continue;
            };
            let encoded = zombie.stats().exit_code;
            if status != 0
                && let Err(error) = uaccess::write_value(status, &encoded)
            {
                return error.to_errno();
            }
            return stats.tgid as i64;
        }
        if options & WUNTRACED != 0 {
            for task in crate::sched::tasks() {
                let stats = task.stats();
                if !stats.user
                    || stats.ppid != parent
                    || stats.ltid != stats.tgid
                    || stats.state == crate::sched::TaskState::Zombie
                    || !matches(stats.tgid, stats.pgid)
                    || !exit_matches(stats.exit_signal)
                {
                    continue;
                }
                let stopped = {
                    let mut task = task.lock();
                    if task.stop_signal != 0 && !task.stop_reported {
                        task.stop_reported = true;
                        Some(task.stop_signal)
                    } else {
                        None
                    }
                };
                if let Some(signal) = stopped {
                    let encoded = ((signal as i32) << 8) | 0x7f;
                    if status != 0
                        && let Err(error) = uaccess::write_value(status, &encoded)
                    {
                        return error.to_errno();
                    }
                    return stats.tgid as i64;
                }
            }
        }
        if options & WCONTINUED != 0 {
            for task in crate::sched::tasks() {
                let stats = task.stats();
                if !stats.user
                    || stats.ppid != parent
                    || stats.ltid != stats.tgid
                    || stats.state == crate::sched::TaskState::Zombie
                    || !matches(stats.tgid, stats.pgid)
                    || !exit_matches(stats.exit_signal)
                {
                    continue;
                }
                let continued = {
                    let mut task = task.lock();
                    core::mem::take(&mut task.continue_pending)
                };
                if continued {
                    let encoded = 0xffffi32;
                    if status != 0
                        && let Err(error) = uaccess::write_value(status, &encoded)
                    {
                        return error.to_errno();
                    }
                    return stats.tgid as i64;
                }
            }
        }
        if !crate::sched::has_children(parent) {
            return -abi::ECHILD;
        }
        if options & WNOHANG != 0 {
            return 0;
        }
        if crate::signal::pending_for_current() {
            // A SIGCHLD handler may be pending because the child just exited;
            // scan once more before reporting EINTR.
            if !rechecked {
                rechecked = true;
                continue;
            }
            return -abi::EINTR;
        }
        rechecked = false;
        crate::sched::sleep_micros(1000);
    }
}

#[cfg(target_arch = "x86_64")]
pub fn fork() -> i64 {
    clone(17, 0, 0, 0, 0)
}

const CLONE_VM: u64 = 0x0000_0100;
const CLONE_FS: u64 = 0x0000_0200;
const CLONE_FILES: u64 = 0x0000_0400;
const CLONE_SIGHAND: u64 = 0x0000_0800;
const CLONE_VFORK: u64 = 0x0000_4000;
const CLONE_PARENT: u64 = 0x0000_8000;
const CLONE_THREAD: u64 = 0x0001_0000;
// SysV semaphore operations are not implemented, so there can be no undo
// list to share yet. Keep this flag accepted for pthread implementations.
const CLONE_SYSVSEM: u64 = 0x0004_0000;
const CLONE_SETTLS: u64 = 0x0008_0000;
const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
// CLONE_DETACHED is ignored by Linux for the classic clone syscall.
const CLONE_DETACHED: u64 = 0x0040_0000;
const CLONE_CHILD_SETTID: u64 = 0x0100_0000;

const CLONE_KNOWN: u64 = CLONE_VM
    | CLONE_FS
    | CLONE_FILES
    | CLONE_SIGHAND
    | CLONE_VFORK
    | CLONE_PARENT
    | CLONE_THREAD
    | CLONE_SYSVSEM
    | CLONE_SETTLS
    | CLONE_PARENT_SETTID
    | CLONE_CHILD_CLEARTID
    | CLONE_DETACHED
    | CLONE_CHILD_SETTID
    | 0xff;

/// Linux clone with process and thread semantics.  CLONE_VFORK is reported as
/// unimplemented rather than silently emulated with different semantics.
#[cfg(target_arch = "x86_64")]
pub fn clone(flags: u64, stack: u64, parent_tid: u64, child_tid: u64, tls: u64) -> i64 {
    if flags & !CLONE_KNOWN != 0 {
        return -abi::ENOSYS;
    }
    let thread = flags & CLONE_THREAD != 0;
    if thread && (flags & (CLONE_SIGHAND | CLONE_VM)) != (CLONE_SIGHAND | CLONE_VM) {
        return -abi::EINVAL;
    }
    if flags & CLONE_SIGHAND != 0 && flags & CLONE_VM == 0 {
        return -abi::EINVAL;
    }
    let Some(parent) = crate::sched::current() else {
        return -abi::ESRCH;
    };
    let parent_stats = parent.stats();
    if !parent_stats.user {
        return -abi::EINVAL;
    }
    let Some(parent_space) = parent.address_space() else {
        return -abi::EINVAL;
    };
    let Some(context) = crate::arch::syscall::capture_user_context() else {
        return -abi::EINVAL;
    };

    let pid = crate::sched::alloc_pid();
    let tgid = if thread { parent_stats.tgid } else { pid };
    let ltid = pid;
    let ppid = if thread || flags & CLONE_PARENT != 0 {
        parent_stats.ppid
    } else {
        parent_stats.tgid
    };
    let exit_signal = if thread { 0 } else { (flags & 0xff) as u32 };

    let child_space = if flags & CLONE_VM != 0 {
        parent_space.clone()
    } else {
        let mut space = parent_space.lock();
        match space.fork_copy() {
            Ok(space) => Arc::new(SpinLock::new(space)),
            Err(_) => return -abi::ENOMEM,
        }
    };
    let files_shared = flags & CLONE_FILES != 0;
    let files = if files_shared {
        parent.files()
    } else {
        Arc::new(SpinLock::new(parent.files().lock().clone()))
    };
    let fs = if flags & CLONE_FS != 0 {
        parent.fs()
    } else {
        Arc::new(SpinLock::new((*parent.fs().lock()).clone()))
    };
    let signals = if flags & CLONE_SIGHAND != 0 {
        parent.signals()
    } else {
        Arc::new(parent.signals().fork_copy())
    };
    let mut thread_signals = crate::signal::ThreadSignals::new();
    {
        let parent_thread = parent.thread_signals();
        let state = parent_thread.lock();
        thread_signals.blocked = state.blocked;
        if flags & CLONE_VM == 0 {
            thread_signals.altstack_sp = state.altstack_sp;
            thread_signals.altstack_size = state.altstack_size;
            thread_signals.altstack_flags = state.altstack_flags;
        }
    }
    let thread_signals = Arc::new(SpinLock::new(thread_signals));

    let mut child_context = context;
    if stack != 0 {
        child_context.rsp = stack;
    }
    if flags & CLONE_SETTLS != 0 {
        child_context.fs_base = tls;
    }
    let child_context = alloc::boxed::Box::new(child_context);

    if flags & CLONE_PARENT_SETTID != 0
        && parent_tid != 0
        && let Err(error) = uaccess::write_value(parent_tid, &(ltid as i32))
    {
        return error.to_errno();
    }

    let set_child_tid = (flags & CLONE_CHILD_SETTID != 0).then_some(child_tid);
    let cleared_tid = if flags & CLONE_CHILD_CLEARTID != 0 {
        child_tid
    } else {
        0
    };
    let child_space_for_tid = child_space.clone();

    let spec = crate::sched::UserTaskSpec {
        tgid,
        ltid,
        ppid,
        pgid: parent_stats.pgid,
        sid: parent_stats.sid,
        exit_signal,
        clear_child_tid: cleared_tid,
        creds: parent.credentials(),
        groups: parent.groups(),
        comm: parent.comm(),
        no_new_privs: parent.no_new_privs(),
        fd_table: files,
        files_shared,
        fs,
        signals,
        thread_signals,
        address_space: child_space,
    };
    let result = crate::sched::spawn_user(
        move || {
            if let Some(address) = set_child_tid {
                // The TID word must be globally visible before this thread can
                // run user code on another CPU.  Interrupts stay masked from
                // the store through the GS/SYSRET handoff so the task cannot
                // migrate between them.
                crate::arch::disable_interrupts();
                let _ = child_space_for_tid
                    .lock()
                    .write_bytes(address, &(ltid as i32).to_le_bytes());
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            }
            // SAFETY: the child owns this address space and the captured user
            // context; it never returns to kernel mode here.
            unsafe { crate::arch::syscall::enter_user_context(&child_context) }
        },
        0,
        crate::sched::CpuMask::all(),
        spec,
    );
    match result {
        Ok(_) => {
            crate::syscall::inherit_limits(parent_stats.tgid, tgid);
            if flags & CLONE_VFORK != 0 {
                // The parent stays suspended until the child gives the address
                // space back, by exec or by exit.
                // `ltid` is the Linux thread id; `find_task` takes the kernel tid.
                // Mixing them up finds the parent itself whenever the two id
                // spaces collide, which suspended it forever.
                while let Some(child) = crate::sched::find_by_ltid(ltid) {
                    let shared = child
                        .address_space()
                        .is_some_and(|space| Arc::ptr_eq(&space, &parent_space));
                    if !shared {
                        break;
                    }
                    crate::sched::sleep_micros(50);
                }
            }
            ltid as i64
        }
        Err(_) => -abi::ENOMEM,
    }
}

pub fn set_tid_address(pointer: u64) -> i64 {
    if let Some(task) = crate::sched::current() {
        task.lock().clear_child_tid = pointer;
    }
    gettid()
}

pub fn getpgid(pid: i64) -> i64 {
    let Some(current) = crate::sched::current() else {
        return -abi::ESRCH;
    };
    let target = if pid == 0 {
        current.stats().tgid
    } else if pid > 0 {
        pid as u64
    } else {
        return -abi::ESRCH;
    };
    match crate::sched::tasks()
        .into_iter()
        .find(|task| task.stats().tgid == target && task.stats().user)
    {
        Some(task) => task.stats().pgid as i64,
        None => -abi::ESRCH,
    }
}

pub fn setpgid(pid: i64, pgid: i64) -> i64 {
    if pid < 0 || pgid < 0 {
        return -abi::EINVAL;
    }
    let Some(current) = crate::sched::current() else {
        return -abi::ESRCH;
    };
    let current_stats = current.stats();
    let target_pid = if pid == 0 {
        current_stats.tgid
    } else {
        pid as u64
    };
    let Some(target) = crate::sched::tasks().into_iter().find(|task| {
        let stats = task.stats();
        stats.user && stats.tgid == target_pid
    }) else {
        return -abi::ESRCH;
    };
    let target_stats = target.stats();
    if target_stats.tgid != current_stats.tgid && target_stats.ppid != current_stats.tgid {
        return -abi::ESRCH;
    }
    if target_stats.tgid == target_stats.sid {
        return -abi::EPERM;
    }
    let new_pgid = if pgid == 0 {
        target_stats.tgid
    } else {
        pgid as u64
    };
    let group_exists = crate::sched::tasks().into_iter().any(|task| {
        let stats = task.stats();
        stats.user && stats.tgid == new_pgid && stats.sid == target_stats.sid
    });
    if !group_exists {
        return -abi::EPERM;
    }
    target.lock().pgid = new_pgid;
    0
}

pub fn setsid() -> i64 {
    let Some(current) = crate::sched::current() else {
        return -abi::ESRCH;
    };
    let stats = current.stats();
    if stats.tgid == stats.pgid {
        return -abi::EPERM;
    }
    let mut task = current.lock();
    task.sid = stats.tgid;
    task.pgid = stats.tgid;
    stats.tgid as i64
}

pub fn getsid(pid: i64) -> i64 {
    if pid < 0 {
        return -abi::EINVAL;
    }
    let Some(current) = crate::sched::current() else {
        return -abi::ESRCH;
    };
    let target = if pid == 0 {
        current.stats().tgid
    } else {
        pid as u64
    };
    match crate::sched::tasks()
        .into_iter()
        .find(|task| task.stats().tgid == target && task.stats().user)
    {
        Some(task) => task.stats().sid as i64,
        None => -abi::ESRCH,
    }
}

const P_ALL: u32 = 0;
const P_PID: u32 = 1;
const P_PGID: u32 = 2;
const WSTOPPED: u32 = 2;
const WEXITED: u32 = 4;
const WNOWAIT: u32 = 0x0100_0000;

fn write_waitid_info(pointer: u64, code: i32, pid: u64, status: i32) -> Result<(), VfsError> {
    let mut info = [0u8; 128];
    info[0..4].copy_from_slice(&(crate::signal::SIGCHLD as i32).to_le_bytes());
    info[8..12].copy_from_slice(&code.to_le_bytes());
    info[16..20].copy_from_slice(&(pid as u32).to_le_bytes());
    info[20..24].copy_from_slice(&0u32.to_le_bytes());
    info[24..28].copy_from_slice(&status.to_le_bytes());
    uaccess::write(pointer, &info)
}

/// Linux waitid(2): idtype/id select the child, WEXITED/WSTOPPED/WCONTINUED
/// select event classes and WNOWAIT reports without consuming the event.
pub fn waitid(idtype: u32, id: u32, info: u64, options: u32, rusage: u64) -> i64 {
    if rusage != 0 {
        return -abi::ENOSYS;
    }
    if options & !(WNOHANG | WSTOPPED | WEXITED | WCONTINUED | WNOWAIT) != 0
        || options & (WSTOPPED | WEXITED | WCONTINUED) == 0
        || idtype > P_PGID
    {
        return -abi::EINVAL;
    }
    if info == 0 {
        return -abi::EFAULT;
    }
    let Some(current) = crate::sched::current() else {
        return -abi::ESRCH;
    };
    let parent = current.stats().tgid;
    let caller_pgid = current.stats().pgid;
    let selector_pid = if idtype == P_PID { id as u64 } else { 0 };
    let selector_pgid = if idtype == P_PGID {
        if id == 0 { caller_pgid } else { id as u64 }
    } else {
        0
    };
    let matches = |tgid: u64, pgid: u64| -> bool {
        match idtype {
            P_ALL => true,
            P_PID => tgid == selector_pid,
            _ => pgid == selector_pgid,
        }
    };
    let mut rechecked = false;
    loop {
        if options & WEXITED != 0 {
            for zombie in crate::sched::zombies() {
                let stats = zombie.stats();
                if stats.ppid != parent
                    || !matches(stats.tgid, stats.pgid)
                    || crate::sched::group_has_live_threads(stats.tgid)
                {
                    continue;
                }
                let encoded = stats.exit_code;
                let (code, status) = if encoded & 0x7f == 0 {
                    (crate::signal::CLD_EXITED, (encoded >> 8) & 0xff)
                } else if encoded & 0x80 != 0 {
                    (crate::signal::CLD_DUMPED, encoded & 0x7f)
                } else {
                    (crate::signal::CLD_KILLED, encoded & 0x7f)
                };
                if options & WNOWAIT == 0 && crate::sched::take_zombie(stats.tgid).is_none() {
                    continue;
                }
                if let Err(error) = write_waitid_info(info, code, stats.tgid, status) {
                    return error.to_errno();
                }
                return 0;
            }
        }
        if options & WSTOPPED != 0 {
            for task in crate::sched::tasks() {
                let stats = task.stats();
                if !stats.user
                    || stats.ppid != parent
                    || stats.ltid != stats.tgid
                    || stats.state == crate::sched::TaskState::Zombie
                    || !matches(stats.tgid, stats.pgid)
                {
                    continue;
                }
                let stopped = {
                    let mut task = task.lock();
                    if task.stop_signal != 0 && (!task.stop_reported || options & WNOWAIT != 0) {
                        if options & WNOWAIT == 0 {
                            task.stop_reported = true;
                        }
                        Some(task.stop_signal)
                    } else {
                        None
                    }
                };
                if let Some(signal) = stopped {
                    if let Err(error) = write_waitid_info(
                        info,
                        crate::signal::CLD_STOPPED,
                        stats.tgid,
                        signal as i32,
                    ) {
                        return error.to_errno();
                    }
                    return 0;
                }
            }
        }
        if options & WCONTINUED != 0 {
            for task in crate::sched::tasks() {
                let stats = task.stats();
                if !stats.user
                    || stats.ppid != parent
                    || stats.ltid != stats.tgid
                    || stats.state == crate::sched::TaskState::Zombie
                    || !matches(stats.tgid, stats.pgid)
                {
                    continue;
                }
                let continued = {
                    let mut task = task.lock();
                    if task.continue_pending {
                        if options & WNOWAIT == 0 {
                            task.continue_pending = false;
                        }
                        true
                    } else {
                        false
                    }
                };
                if continued {
                    if let Err(error) = write_waitid_info(
                        info,
                        crate::signal::CLD_CONTINUED,
                        stats.tgid,
                        crate::signal::SIGCONT as i32,
                    ) {
                        return error.to_errno();
                    }
                    return 0;
                }
            }
        }
        if !crate::sched::has_children(parent) {
            return -abi::ECHILD;
        }
        if options & WNOHANG != 0 {
            return 0;
        }
        if crate::signal::pending_for_current() {
            if !rechecked {
                rechecked = true;
                continue;
            }
            return -abi::EINTR;
        }
        rechecked = false;
        crate::sched::sleep_micros(1000);
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn clone3(_args: u64, _size: u64) -> i64 {
    -abi::ENOSYS
}

/// `clone3(2)` over the classic entry point. `args` is `struct clone_args`.
#[cfg(target_arch = "x86_64")]
pub fn clone3(args: u64, size: u64) -> i64 {
    if !(64..=4096).contains(&size) {
        return -abi::EINVAL;
    }
    let raw: [u64; 8] = match uaccess::read_value(args) {
        Ok(raw) => raw,
        Err(error) => return error.to_errno(),
    };
    let [
        flags,
        pidfd,
        child_tid,
        parent_tid,
        exit_signal,
        stack,
        stack_size,
        tls,
    ] = raw;
    if pidfd != 0 || exit_signal > 64 || flags & 0xff != 0 {
        return -abi::EINVAL;
    }
    if (stack == 0) != (stack_size == 0) {
        return -abi::EINVAL;
    }
    let top = stack.saturating_add(stack_size);
    clone(flags | exit_signal, top, parent_tid, child_tid, tls)
}

struct IntervalTimer {
    /// Absolute expiry in monotonic microseconds, zero when disarmed.
    expiry: u64,
    interval: u64,
}

static ALARMS: SpinLock<alloc::collections::BTreeMap<u64, IntervalTimer>> =
    SpinLock::new(alloc::collections::BTreeMap::new());
static ALARM_THREAD: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn start_alarm_thread() {
    if ALARM_THREAD.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let _ = sched::spawn(
        || {
            loop {
                sched::sleep_micros(1000);
                let now = crate::time::uptime_micros();
                let mut fired = alloc::vec::Vec::new();
                {
                    let mut alarms = ALARMS.lock();
                    alarms.retain(|&pid, timer| {
                        if timer.expiry == 0 || timer.expiry > now {
                            return true;
                        }
                        fired.push(pid);
                        if timer.interval == 0 {
                            return false;
                        }
                        timer.expiry = now + timer.interval;
                        true
                    });
                }
                for pid in fired {
                    let _ = crate::signal::kill(pid as i64, u64::from(crate::signal::SIGALRM));
                }
            }
        },
        0,
    );
}

/// `ITIMER_REAL` for a process; the virtual and profiling timers would need CPU
/// accounting hooks that nothing here uses, so they report `EINVAL`.
pub fn setitimer(which: u32, new_value: u64, old_value: u64) -> i64 {
    if which != 0 {
        return -abi::EINVAL;
    }
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    let pid = task.stats().tgid;
    let value: [u64; 4] = if new_value != 0 {
        match uaccess::read_value(new_value) {
            Ok(value) => value,
            Err(error) => return error.to_errno(),
        }
    } else {
        [0; 4]
    };
    if value[1] >= 1_000_000 || value[3] >= 1_000_000 {
        return -abi::EINVAL;
    }
    let now = crate::time::uptime_micros();
    let previous = {
        let mut alarms = ALARMS.lock();
        let previous = alarms.get(&pid).map_or([0u64; 4], |timer| {
            let left = timer.expiry.saturating_sub(now);
            [
                timer.interval / 1_000_000,
                timer.interval % 1_000_000,
                left / 1_000_000,
                left % 1_000_000,
            ]
        });
        if new_value != 0 {
            let delay = value[2] * 1_000_000 + value[3];
            if delay == 0 {
                alarms.remove(&pid);
            } else {
                alarms.insert(
                    pid,
                    IntervalTimer {
                        expiry: now + delay,
                        interval: value[0] * 1_000_000 + value[1],
                    },
                );
            }
        }
        previous
    };
    start_alarm_thread();
    if old_value != 0 {
        return match uaccess::write_value(old_value, &previous) {
            Ok(()) => 0,
            Err(error) => error.to_errno(),
        };
    }
    0
}

pub fn getitimer(which: u32, value: u64) -> i64 {
    if which != 0 {
        return -abi::EINVAL;
    }
    setitimer(0, 0, value)
}

pub fn alarm(seconds: u32) -> i64 {
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    let pid = task.stats().tgid;
    let now = crate::time::uptime_micros();
    let mut alarms = ALARMS.lock();
    let previous = alarms.get(&pid).map_or(0, |timer| {
        timer.expiry.saturating_sub(now).div_ceil(1_000_000)
    });
    if seconds == 0 {
        alarms.remove(&pid);
    } else {
        alarms.insert(
            pid,
            IntervalTimer {
                expiry: now + u64::from(seconds) * 1_000_000,
                interval: 0,
            },
        );
    }
    drop(alarms);
    start_alarm_thread();
    previous as i64
}
