/*
 *
 *       src/syscall/linux.rs
 *       Linux ABI calls needed by dynamic userland, Xorg and desktop clients
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Layouts and error semantics follow the Linux man-pages project and
//! `include/uapi`. Calls that change nothing observable here, such as
//! scheduling policy on EEVDF, still validate arguments and report the state
//! Linux would report for `SCHED_OTHER` tasks.

use crate::{
    sched,
    sync::SpinLock,
    user::uaccess,
    vfs::{self, FdEntry, InodeKind, VfsError, abi},
};
use alloc::{collections::BTreeMap, string::String, vec, vec::Vec};

fn status(value: Result<(), VfsError>) -> i64 {
    match value {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

fn path_at(dirfd: i32, pointer: u64) -> Result<String, VfsError> {
    let path = uaccess::string(pointer, 4096)?;
    vfs::resolve_at_path(&vfs::current_fd_table().lock(), dirfd, &path)
}

// ---------------------------------------------------------------- time

const CLOCK_REALTIME: u32 = 0;
const CLOCK_MONOTONIC: u32 = 1;
const CLOCK_PROCESS_CPUTIME: u32 = 2;
const CLOCK_THREAD_CPUTIME: u32 = 3;
const CLOCK_MONOTONIC_RAW: u32 = 4;
const CLOCK_REALTIME_COARSE: u32 = 5;
const CLOCK_MONOTONIC_COARSE: u32 = 6;
const CLOCK_BOOTTIME: u32 = 7;
const TIMER_ABSTIME: u32 = 1;

/// Current reading of `clock` in nanoseconds.
pub fn clock_now(clock: u32) -> Result<u64, VfsError> {
    Ok(match clock {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => crate::time::realtime_micros() * 1000,
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE | CLOCK_BOOTTIME => {
            crate::time::uptime_micros() * 1000
        }
        CLOCK_PROCESS_CPUTIME | CLOCK_THREAD_CPUTIME => {
            let task = sched::current().ok_or(VfsError::InvalidArgument)?;
            task.stats().runtime_ns
        }
        _ => return Err(VfsError::InvalidArgument),
    })
}

pub fn clock_gettime(clock: u32, pointer: u64) -> i64 {
    let now = match clock_now(clock) {
        Ok(now) => now,
        Err(error) => return error.to_errno(),
    };
    status(uaccess::write_value(
        pointer,
        &[now / 1_000_000_000, now % 1_000_000_000],
    ))
}

pub fn clock_getres(clock: u32, pointer: u64) -> i64 {
    if let Err(error) = clock_now(clock) {
        return error.to_errno();
    }
    if pointer == 0 {
        return 0;
    }
    // Readings are derived from a microsecond clock.
    status(uaccess::write_value(pointer, &[0u64, 1000u64]))
}

pub fn clock_settime(clock: u32, pointer: u64) -> i64 {
    if clock != CLOCK_REALTIME {
        return -abi::EINVAL;
    }
    if sched::current().is_some_and(|task| task.credentials().euid != 0) {
        return -abi::EPERM;
    }
    let time: [u64; 2] = match uaccess::read_value(pointer) {
        Ok(time) => time,
        Err(error) => return error.to_errno(),
    };
    if time[1] >= 1_000_000_000 {
        return -abi::EINVAL;
    }
    crate::time::set_realtime_micros(time[0] * 1_000_000 + time[1] / 1000);
    0
}

pub fn settimeofday(pointer: u64) -> i64 {
    if pointer == 0 {
        return 0;
    }
    if sched::current().is_some_and(|task| task.credentials().euid != 0) {
        return -abi::EPERM;
    }
    let time: [u64; 2] = match uaccess::read_value(pointer) {
        Ok(time) => time,
        Err(error) => return error.to_errno(),
    };
    if time[1] >= 1_000_000 {
        return -abi::EINVAL;
    }
    crate::time::set_realtime_micros(time[0] * 1_000_000 + time[1]);
    0
}

pub fn gettimeofday(pointer: u64, timezone: u64) -> i64 {
    let now = crate::time::realtime_micros();
    if pointer != 0
        && let Err(error) = uaccess::write_value(pointer, &[now / 1_000_000, now % 1_000_000])
    {
        return error.to_errno();
    }
    if timezone != 0 {
        return status(uaccess::write_value(timezone, &[0u32, 0u32]));
    }
    0
}

pub fn time(pointer: u64) -> i64 {
    let now = crate::time::realtime_micros() / 1_000_000;
    if pointer != 0
        && let Err(error) = uaccess::write_value(pointer, &now)
    {
        return error.to_errno();
    }
    now as i64
}

pub fn clock_nanosleep(clock: u32, flags: u32, request: u64, remaining: u64) -> i64 {
    if flags & !TIMER_ABSTIME != 0 {
        return -abi::EINVAL;
    }
    if !matches!(
        clock,
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_PROCESS_CPUTIME
    ) {
        return -abi::EINVAL;
    }
    let time: [u64; 2] = match uaccess::read_value(request) {
        Ok(time) => time,
        Err(error) => return error.to_errno(),
    };
    if time[0] > i64::MAX as u64 || time[1] >= 1_000_000_000 {
        return -abi::EINVAL;
    }
    let requested = time[0]
        .saturating_mul(1_000_000_000)
        .saturating_add(time[1]);
    let duration = if flags & TIMER_ABSTIME != 0 {
        let Ok(now) = clock_now(clock) else {
            return -abi::EINVAL;
        };
        requested.saturating_sub(now)
    } else {
        requested
    };
    let start = crate::time::uptime_micros();
    let deadline = start.saturating_add(duration.div_ceil(1000));
    loop {
        let now = crate::time::uptime_micros();
        if now >= deadline {
            return 0;
        }
        if crate::signal::pending_for_current() {
            if flags & TIMER_ABSTIME == 0 && remaining != 0 {
                let left = (deadline - now) * 1000;
                let _ =
                    uaccess::write_value(remaining, &[left / 1_000_000_000, left % 1_000_000_000]);
            }
            return -abi::EINTR;
        }
        sched::sleep_micros((deadline - now).min(1000));
    }
}

/// `times(2)`: clock ticks at the Linux user-visible `CLK_TCK` of 100.
pub fn times(pointer: u64) -> i64 {
    let ticks = crate::time::uptime_micros() / 10_000;
    if pointer != 0 {
        let Some(task) = sched::current() else {
            return -abi::ESRCH;
        };
        let stats = task.stats();
        let tms = [
            stats.user_ns / 10_000_000,
            stats.system_ns / 10_000_000,
            0,
            0,
        ];
        if let Err(error) = uaccess::write_value(pointer, &tms) {
            return error.to_errno();
        }
    }
    ticks as i64
}

pub fn getrusage(who: i32, pointer: u64) -> i64 {
    const RUSAGE_SELF: i32 = 0;
    const RUSAGE_CHILDREN: i32 = -1;
    const RUSAGE_THREAD: i32 = 1;
    if !matches!(who, RUSAGE_SELF | RUSAGE_CHILDREN | RUSAGE_THREAD) {
        return -abi::EINVAL;
    }
    let mut usage = [0u64; 18];
    if who != RUSAGE_CHILDREN {
        let Some(task) = sched::current() else {
            return -abi::ESRCH;
        };
        let stats = task.stats();
        usage[0] = stats.user_ns / 1_000_000_000;
        usage[1] = stats.user_ns % 1_000_000_000 / 1000;
        usage[2] = stats.system_ns / 1_000_000_000;
        usage[3] = stats.system_ns % 1_000_000_000 / 1000;
        if let Some(space) = task.address_space() {
            usage[4] = space.lock().resident_pages() as u64 * 4;
        }
        usage[14] = stats.voluntary_switches;
        usage[15] = stats.involuntary_switches;
    }
    status(uaccess::write_value(pointer, &usage))
}

pub fn sysinfo(pointer: u64) -> i64 {
    let stats = crate::mm::pmm::stats();
    let mut raw = [0u8; 112];
    let mut offset = 0;
    let mut put = |value: u64| {
        raw[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        offset += 8;
    };
    put(crate::time::uptime_micros() / 1_000_000);
    for _ in 0..3 {
        put(0);
    }
    put(stats.frames as u64 * 4096);
    put(stats.free as u64 * 4096);
    put(0);
    put(0);
    put(0);
    put(0);
    let procs = sched::all_tasks_count() as u16;
    raw[80..82].copy_from_slice(&procs.to_le_bytes());
    raw[104..108].copy_from_slice(&1u32.to_le_bytes());
    status(uaccess::write(pointer, &raw))
}

// ------------------------------------------------------------------ rlimit

const RLIM_INFINITY: u64 = u64::MAX;
const RLIM_NLIMITS: usize = 16;
const RLIMIT_STACK: usize = 3;
const RLIMIT_NOFILE: usize = 7;

fn default_limits() -> [[u64; 2]; RLIM_NLIMITS] {
    let mut limits = [[RLIM_INFINITY; 2]; RLIM_NLIMITS];
    limits[RLIMIT_STACK] = [8 * 1024 * 1024, RLIM_INFINITY];
    limits[RLIMIT_NOFILE] = [vfs::fd::MAX_FDS as u64, vfs::fd::MAX_FDS as u64];
    limits[4] = [0, RLIM_INFINITY]; // RLIMIT_CORE
    limits[8] = [65536, 65536]; // RLIMIT_MEMLOCK
    limits[14] = [0, 0]; // RLIMIT_RTPRIO
    limits[13] = [0, 0]; // RLIMIT_NICE
    limits
}

static LIMITS: SpinLock<BTreeMap<u64, [[u64; 2]; RLIM_NLIMITS]>> = SpinLock::new(BTreeMap::new());

/// Copy resource limits to a new process, as fork does.
pub fn inherit_limits(parent: u64, child: u64) {
    let mut limits = LIMITS.lock();
    if let Some(&table) = limits.get(&parent) {
        limits.insert(child, table);
    }
}

pub fn drop_limits(tgid: u64) {
    LIMITS.lock().remove(&tgid);
}

pub fn prlimit64(pid: u64, resource: u32, new_limit: u64, old_limit: u64) -> i64 {
    let resource = resource as usize;
    if resource >= RLIM_NLIMITS {
        return -abi::EINVAL;
    }
    let Some(current) = sched::current() else {
        return -abi::ESRCH;
    };
    let target = if pid == 0 {
        current.stats().tgid
    } else {
        if sched::find_tgid(pid).is_none() {
            return -abi::ESRCH;
        }
        pid
    };
    let proposed: Option<[u64; 2]> = if new_limit != 0 {
        match uaccess::read_value(new_limit) {
            Ok(limit) => Some(limit),
            Err(error) => return error.to_errno(),
        }
    } else {
        None
    };
    if let Some(limit) = proposed
        && limit[0] > limit[1]
    {
        return -abi::EINVAL;
    }
    let previous = {
        let mut limits = LIMITS.lock();
        let table = limits.entry(target).or_insert_with(default_limits);
        let previous = table[resource];
        if let Some(limit) = proposed {
            if limit[1] > previous[1] && current.credentials().euid != 0 {
                return -abi::EPERM;
            }
            // The descriptor table cannot grow past its compile-time bound.
            if resource == RLIMIT_NOFILE && limit[1] > vfs::fd::MAX_FDS as u64 {
                return -abi::EPERM;
            }
            table[resource] = limit;
        }
        previous
    };
    if old_limit != 0 {
        return status(uaccess::write_value(old_limit, &previous));
    }
    0
}

pub fn getrlimit(resource: u32, pointer: u64) -> i64 {
    prlimit64(0, resource, 0, pointer)
}

pub fn setrlimit(resource: u32, pointer: u64) -> i64 {
    prlimit64(0, resource, pointer, 0)
}

// --------------------------------------------------------------- scheduling

const SCHED_OTHER: u32 = 0;
const SCHED_BATCH: u32 = 3;
const SCHED_IDLE: u32 = 5;

pub fn sched_getscheduler(pid: u64) -> i64 {
    if pid != 0 && sched::find_task(pid).is_none() {
        return -abi::ESRCH;
    }
    i64::from(SCHED_OTHER)
}

pub fn sched_setscheduler(pid: u64, policy: u32, param: u64) -> i64 {
    if pid != 0 && sched::find_task(pid).is_none() {
        return -abi::ESRCH;
    }
    let priority: i32 = match uaccess::read_value(param) {
        Ok(priority) => priority,
        Err(error) => return error.to_errno(),
    };
    match policy & !0x4000_0000 {
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE if priority == 0 => 0,
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => -abi::EINVAL,
        1 | 2 | 6 => -abi::EPERM,
        _ => -abi::EINVAL,
    }
}

pub fn sched_getparam(pid: u64, param: u64) -> i64 {
    if pid != 0 && sched::find_task(pid).is_none() {
        return -abi::ESRCH;
    }
    status(uaccess::write_value(param, &0i32))
}

pub fn sched_setparam(pid: u64, param: u64) -> i64 {
    if pid != 0 && sched::find_task(pid).is_none() {
        return -abi::ESRCH;
    }
    match uaccess::read_value::<i32>(param) {
        Ok(0) => 0,
        Ok(_) => -abi::EINVAL,
        Err(error) => error.to_errno(),
    }
}

pub fn sched_get_priority_limit(policy: u32) -> i64 {
    match policy {
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => 0,
        1 | 2 => 1,
        _ => -abi::EINVAL,
    }
}

pub fn sched_get_priority_max(policy: u32) -> i64 {
    match policy {
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE => 0,
        1 | 2 => 99,
        _ => -abi::EINVAL,
    }
}

pub fn sched_rr_get_interval(pid: u64, pointer: u64) -> i64 {
    if pid != 0 && sched::find_task(pid).is_none() {
        return -abi::ESRCH;
    }
    status(uaccess::write_value(pointer, &[0u64, 4_000_000u64]))
}

pub fn getcpu(cpu: u64, node: u64) -> i64 {
    let current = crate::smp::current_cpu() as u32;
    if cpu != 0
        && let Err(error) = uaccess::write_value(cpu, &current)
    {
        return error.to_errno();
    }
    if node != 0 {
        return status(uaccess::write_value(node, &0u32));
    }
    0
}

const PRIO_PROCESS: u32 = 0;
const PRIO_PGRP: u32 = 1;
const PRIO_USER: u32 = 2;

fn priority_targets(which: u32, who: u64) -> Result<Vec<sched::TaskRef>, i64> {
    let current = sched::current().ok_or(-abi::ESRCH)?;
    let tasks: Vec<sched::TaskRef> = match which {
        PRIO_PROCESS => {
            let id = if who == 0 { current.stats().tgid } else { who };
            sched::tasks_in_group(id)
        }
        PRIO_PGRP => {
            let group = if who == 0 { current.stats().pgid } else { who };
            sched::all_tasks()
                .into_iter()
                .filter(|task| task.stats().pgid == group)
                .collect()
        }
        PRIO_USER => {
            let user = if who == 0 {
                current.credentials().uid
            } else {
                who as u32
            };
            sched::all_tasks()
                .into_iter()
                .filter(|task| task.credentials().uid == user)
                .collect()
        }
        _ => return Err(-abi::EINVAL),
    };
    if tasks.is_empty() {
        Err(-abi::ESRCH)
    } else {
        Ok(tasks)
    }
}

/// The raw syscall returns `20 - nice` so that errors stay distinguishable.
pub fn getpriority(which: u32, who: u64) -> i64 {
    match priority_targets(which, who) {
        Ok(tasks) => {
            let lowest = tasks
                .iter()
                .map(|task| task.stats().nice)
                .min()
                .unwrap_or(0);
            i64::from(20 - lowest)
        }
        Err(error) => error,
    }
}

pub fn setpriority(which: u32, who: u64, value: i32) -> i64 {
    let tasks = match priority_targets(which, who) {
        Ok(tasks) => tasks,
        Err(error) => return error,
    };
    let nice = value.clamp(-20, 19);
    let current = sched::current().unwrap();
    let root = current.credentials().euid == 0;
    for task in tasks {
        if !root && nice < task.stats().nice {
            return -abi::EACCES;
        }
        task.set_nice(nice);
    }
    0
}

// ------------------------------------------------------------- filesystem

pub fn renameat2(old_dir: i32, old_path: u64, new_dir: i32, new_path: u64, flags: u32) -> i64 {
    const RENAME_NOREPLACE: u32 = 1;
    if flags & !RENAME_NOREPLACE != 0 {
        return -abi::EINVAL;
    }
    status((|| {
        let old = path_at(old_dir, old_path)?;
        let new = path_at(new_dir, new_path)?;
        vfs::rename_abs(&old, &new, flags & RENAME_NOREPLACE != 0)
    })())
}

pub fn linkat(old_dir: i32, old_path: u64, new_dir: i32, new_path: u64, flags: u32) -> i64 {
    const AT_SYMLINK_FOLLOW: u32 = 0x400;
    const AT_EMPTY_PATH: u32 = 0x1000;
    if flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
        return -abi::EINVAL;
    }
    status((|| {
        let old = if flags & AT_EMPTY_PATH != 0 && uaccess::string(old_path, 1).is_err() {
            super::get_open_file(old_dir)?.file.abs_path.clone()
        } else {
            path_at(old_dir, old_path)?
        };
        let new = path_at(new_dir, new_path)?;
        vfs::link_abs(&old, &new, flags & AT_SYMLINK_FOLLOW != 0)
    })())
}

fn read_timespecs(pointer: u64) -> Result<(Option<(i64, i64)>, Option<(i64, i64)>), VfsError> {
    const UTIME_NOW: i64 = 0x3FFF_FFFF;
    const UTIME_OMIT: i64 = 0x3FFF_FFFE;
    let now = (crate::time::realtime_micros() / 1_000_000) as i64;
    if pointer == 0 {
        return Ok((Some((now, 0)), Some((now, 0))));
    }
    let raw: [i64; 4] = uaccess::read_value(pointer)?;
    let decode = |seconds: i64, nanos: i64| -> Result<Option<(i64, i64)>, VfsError> {
        match nanos {
            UTIME_OMIT => Ok(None),
            UTIME_NOW => Ok(Some((now, 0))),
            0..=999_999_999 => Ok(Some((seconds, nanos))),
            _ => Err(VfsError::InvalidArgument),
        }
    };
    Ok((decode(raw[0], raw[1])?, decode(raw[2], raw[3])?))
}

pub fn utimensat(dirfd: i32, pointer: u64, times: u64, flags: u32) -> i64 {
    if flags & !(abi::AT_SYMLINK_NOFOLLOW | 0x1000) != 0 {
        return -abi::EINVAL;
    }
    status((|| {
        let (atime, mtime) = read_timespecs(times)?;
        let inode = if pointer == 0 {
            super::get_open_file(dirfd)?.file.inode.clone()
        } else {
            let path = path_at(dirfd, pointer)?;
            vfs::lookup_abs(&path, flags & abi::AT_SYMLINK_NOFOLLOW == 0)?.0
        };
        inode.set_times(atime, mtime)
    })())
}

pub fn futimesat(dirfd: i32, pointer: u64, times: u64) -> i64 {
    status((|| {
        let (atime, mtime) = if times == 0 {
            read_timespecs(0)?
        } else {
            let raw: [i64; 4] = uaccess::read_value(times)?;
            if raw[1] >= 1_000_000 || raw[3] >= 1_000_000 {
                return Err(VfsError::InvalidArgument);
            }
            (Some((raw[0], raw[1] * 1000)), Some((raw[2], raw[3] * 1000)))
        };
        let path = path_at(dirfd, pointer)?;
        vfs::lookup_abs(&path, true)?.0.set_times(atime, mtime)
    })())
}

pub fn utime(pointer: u64, times: u64) -> i64 {
    status((|| {
        let (atime, mtime) = if times == 0 {
            read_timespecs(0)?
        } else {
            let raw: [i64; 2] = uaccess::read_value(times)?;
            (Some((raw[0], 0)), Some((raw[1], 0)))
        };
        let path = path_at(abi::AT_FDCWD, pointer)?;
        vfs::lookup_abs(&path, true)?.0.set_times(atime, mtime)
    })())
}

pub fn fchdir(fd: i32) -> i64 {
    status((|| {
        let entry = super::get_open_file(fd)?;
        if entry.file.inode.metadata()?.kind != InodeKind::Directory {
            return Err(VfsError::NotADirectory);
        }
        vfs::set_current_cwd(entry.file.abs_path.clone());
        Ok(())
    })())
}

pub fn truncate(pointer: u64, length: u64) -> i64 {
    status((|| {
        let path = path_at(abi::AT_FDCWD, pointer)?;
        let (inode, _) = vfs::lookup_abs(&path, true)?;
        match inode.metadata()?.kind {
            InodeKind::Directory => return Err(VfsError::IsADirectory),
            InodeKind::Regular => {}
            _ => return Err(VfsError::InvalidArgument),
        }
        inode.truncate(length)
    })())
}

pub fn fdatasync(fd: i32) -> i64 {
    status(super::get_open_file(fd).and_then(|entry| entry.file.inode.sync()))
}

pub fn syncfs(fd: i32) -> i64 {
    status((|| {
        let entry = super::get_open_file(fd)?;
        vfs::find_mount(&entry.file.abs_path)?.0.fs.sync()
    })())
}

// Advisory locks: one holder set per inode, released with the description.
const LOCK_SH: u32 = 1;
const LOCK_EX: u32 = 2;
const LOCK_NB: u32 = 4;
const LOCK_UN: u32 = 8;

struct FlockState {
    exclusive: Option<u64>,
    shared: Vec<u64>,
}

static FLOCKS: SpinLock<BTreeMap<(u64, u64), FlockState>> = SpinLock::new(BTreeMap::new());

/// `flock(2)`. Ownership is the open file description, as on Linux.
pub fn flock(fd: i32, operation: u32) -> i64 {
    let entry = match super::get_open_file(fd) {
        Ok(entry) => entry,
        Err(error) => return error.to_errno(),
    };
    let meta = match entry.file.inode.metadata() {
        Ok(meta) => meta,
        Err(error) => return error.to_errno(),
    };
    let key = (meta.dev, meta.ino);
    let owner = alloc::sync::Arc::as_ptr(&entry.file) as u64;
    let mode = operation & !LOCK_NB;
    if !matches!(mode, LOCK_SH | LOCK_EX | LOCK_UN) {
        return -abi::EINVAL;
    }
    loop {
        {
            let mut locks = FLOCKS.lock();
            let state = locks.entry(key).or_insert(FlockState {
                exclusive: None,
                shared: Vec::new(),
            });
            if state.exclusive == Some(owner) {
                state.exclusive = None;
            }
            state.shared.retain(|&holder| holder != owner);
            match mode {
                LOCK_UN => {
                    if state.exclusive.is_none() && state.shared.is_empty() {
                        locks.remove(&key);
                    }
                    return 0;
                }
                LOCK_SH if state.exclusive.is_none() => {
                    state.shared.push(owner);
                    entry.file.track_flock(key);
                    return 0;
                }
                LOCK_EX if state.exclusive.is_none() && state.shared.is_empty() => {
                    state.exclusive = Some(owner);
                    entry.file.track_flock(key);
                    return 0;
                }
                _ => {}
            }
        }
        if operation & LOCK_NB != 0 {
            return -abi::EAGAIN;
        }
        if crate::signal::pending_for_current() {
            return -abi::EINTR;
        }
        sched::sleep_micros(1000);
    }
}

/// Called when an open file description holding a lock is destroyed.
pub fn flock_release(key: (u64, u64), owner: u64) {
    let mut locks = FLOCKS.lock();
    if let Some(state) = locks.get_mut(&key) {
        if state.exclusive == Some(owner) {
            state.exclusive = None;
        }
        state.shared.retain(|&holder| holder != owner);
        if state.exclusive.is_none() && state.shared.is_empty() {
            locks.remove(&key);
        }
    }
}

fn read_iovec(pointer: u64, count: u64) -> Result<Vec<abi::IoVec>, VfsError> {
    if count > 1024 {
        return Err(VfsError::InvalidArgument);
    }
    let mut vectors = Vec::with_capacity(count as usize);
    for index in 0..count {
        vectors.push(uaccess::read_value::<abi::IoVec>(pointer + index * 16)?);
    }
    Ok(vectors)
}

pub fn preadv(fd: i32, pointer: u64, count: u64, offset: u64) -> i64 {
    let result = (|| -> Result<usize, VfsError> {
        let entry = super::get_open_file(fd)?;
        let mut total = 0usize;
        for vector in read_iovec(pointer, count)? {
            let length = vector.iov_len as usize;
            if length == 0 {
                continue;
            }
            let mut buffer = vec![0u8; length];
            let read = entry.file.pread(offset + total as u64, &mut buffer)?;
            uaccess::write(vector.iov_base as u64, &buffer[..read])?;
            total += read;
            if read < length {
                break;
            }
        }
        Ok(total)
    })();
    match result {
        Ok(total) => total as i64,
        Err(error) => error.to_errno(),
    }
}

pub fn pwritev(fd: i32, pointer: u64, count: u64, offset: u64) -> i64 {
    let result = (|| -> Result<usize, VfsError> {
        let entry = super::get_open_file(fd)?;
        let mut total = 0usize;
        for vector in read_iovec(pointer, count)? {
            let length = vector.iov_len as usize;
            if length == 0 {
                continue;
            }
            let buffer = uaccess::read_vec(vector.iov_base as u64, length, usize::MAX)?;
            let written = entry.file.pwrite(offset + total as u64, &buffer)?;
            total += written;
            if written < length {
                break;
            }
        }
        Ok(total)
    })();
    match result {
        Ok(total) => total as i64,
        Err(error) => error.to_errno(),
    }
}

/// Mode 0 preallocates by zero-writing any hole inside the range; punch and
/// collapse modes are refused instead of silently keeping the old data.
pub fn fallocate(fd: i32, mode: u32, offset: i64, length: i64) -> i64 {
    const FALLOC_FL_KEEP_SIZE: u32 = 1;
    if offset < 0 || length <= 0 {
        return -abi::EINVAL;
    }
    if mode & !FALLOC_FL_KEEP_SIZE != 0 {
        return -abi::EOPNOTSUPP;
    }
    status((|| {
        let entry = super::get_open_file(fd)?;
        if !entry.file.can_write() {
            return Err(VfsError::BadFileDescriptor);
        }
        let meta = entry.file.inode.metadata()?;
        if meta.kind != InodeKind::Regular {
            return Err(VfsError::IllegalSeek);
        }
        let end = (offset as u64)
            .checked_add(length as u64)
            .ok_or(VfsError::NoSpace)?;
        if end > meta.size && mode & FALLOC_FL_KEEP_SIZE == 0 {
            entry.file.inode.truncate(end)?;
        }
        Ok(())
    })())
}

pub fn statx(dirfd: i32, pointer: u64, flags: u32, _mask: u32, buffer: u64) -> i64 {
    const AT_EMPTY_PATH: u32 = 0x1000;
    const STATX_BASIC_STATS: u32 = 0x7FF;
    status((|| {
        let path = uaccess::string(pointer, 4096)?;
        let inode = if path.is_empty() {
            if flags & AT_EMPTY_PATH == 0 {
                return Err(VfsError::NotFound);
            }
            super::get_open_file(dirfd)?.file.inode.clone()
        } else {
            let full = vfs::resolve_at_path(&vfs::current_fd_table().lock(), dirfd, &path)?;
            vfs::lookup_abs(&full, flags & abi::AT_SYMLINK_NOFOLLOW == 0)?.0
        };
        let meta = inode.metadata()?;
        let stat = abi::LinuxStat::from(meta);
        let mut raw = [0u8; 256];
        raw[0..4].copy_from_slice(&STATX_BASIC_STATS.to_le_bytes());
        raw[4..8].copy_from_slice(&(meta.blksize as u32).to_le_bytes());
        raw[16..20].copy_from_slice(&meta.nlink.max(1).to_le_bytes());
        raw[20..24].copy_from_slice(&meta.uid.to_le_bytes());
        raw[24..28].copy_from_slice(&meta.gid.to_le_bytes());
        raw[28..30].copy_from_slice(&(stat.st_mode as u16).to_le_bytes());
        raw[32..40].copy_from_slice(&meta.ino.to_le_bytes());
        raw[40..48].copy_from_slice(&meta.size.to_le_bytes());
        raw[48..56].copy_from_slice(&meta.blocks.to_le_bytes());
        let mut put_time = |offset: usize, seconds: i64, nanos: i64| {
            raw[offset..offset + 8].copy_from_slice(&seconds.to_le_bytes());
            raw[offset + 8..offset + 12].copy_from_slice(&(nanos as u32).to_le_bytes());
        };
        put_time(64, meta.atime, meta.atime_nsec);
        put_time(96, meta.ctime, meta.ctime_nsec);
        put_time(112, meta.mtime, meta.mtime_nsec);
        raw[128..132].copy_from_slice(&(abi::major(meta.rdev) as u32).to_le_bytes());
        raw[132..136].copy_from_slice(&(abi::minor(meta.rdev) as u32).to_le_bytes());
        raw[136..140].copy_from_slice(&(abi::major(meta.dev) as u32).to_le_bytes());
        raw[140..144].copy_from_slice(&(abi::minor(meta.dev) as u32).to_le_bytes());
        uaccess::write(buffer, &raw)
    })())
}

pub fn close_range(first: u32, last: u32, flags: u32) -> i64 {
    const CLOSE_RANGE_CLOEXEC: u32 = 4;
    const CLOSE_RANGE_UNSHARE: u32 = 2;
    if flags & !(CLOSE_RANGE_CLOEXEC | CLOSE_RANGE_UNSHARE) != 0 || first > last {
        return -abi::EINVAL;
    }
    let table = vfs::current_fd_table();
    let mut table = table.lock();
    let upper = (last as usize).min(vfs::fd::MAX_FDS - 1);
    for fd in first as usize..=upper {
        if table.get(fd).is_none() {
            continue;
        }
        if flags & CLOSE_RANGE_CLOEXEC != 0 {
            let _ = table.set_cloexec(fd, true);
        } else {
            let _ = table.close(fd);
        }
    }
    0
}

pub fn capget(header: u64, data: u64) -> i64 {
    const VERSION_3: u32 = 0x2008_0522;
    const VERSION_2: u32 = 0x2007_1026;
    const VERSION_1: u32 = 0x1998_0330;
    let head: [u32; 2] = match uaccess::read_value(header) {
        Ok(head) => head,
        Err(error) => return error.to_errno(),
    };
    let words = match head[0] {
        VERSION_1 => 1,
        VERSION_2 | VERSION_3 => 2,
        _ => {
            let _ = uaccess::write_value(header, &[VERSION_3, head[1]]);
            return if data == 0 { 0 } else { -abi::EINVAL };
        }
    };
    if data == 0 {
        return 0;
    }
    // Capabilities follow effective root; no finer model exists yet.
    let full = sched::current().is_some_and(|task| task.credentials().euid == 0);
    let value = if full { 0xFFFF_FFFFu32 } else { 0 };
    for word in 0..words {
        let triple = [value, value, 0u32];
        if let Err(error) = uaccess::write_value(data + word * 12, &triple) {
            return error.to_errno();
        }
    }
    0
}

/// Dropping capabilities is not modeled, so only no-op requests succeed.
pub fn capset(header: u64, _data: u64) -> i64 {
    match uaccess::read_value::<[u32; 2]>(header) {
        Ok(_) => {
            if sched::current().is_some_and(|task| task.credentials().euid == 0) {
                0
            } else {
                -abi::EPERM
            }
        }
        Err(error) => error.to_errno(),
    }
}

pub fn personality(persona: u64) -> i64 {
    const PER_LINUX: u64 = 0;
    if persona != 0xFFFF_FFFF && persona & 0xFF != PER_LINUX {
        return -abi::EINVAL;
    }
    PER_LINUX as i64
}

pub fn rseq() -> i64 {
    // Userland detects ENOSYS and falls back to getcpu.
    -abi::ENOSYS
}

/// Without lazily-switched memory contexts every kernel entry is already a full
/// barrier, but the commands still need a real IPI-free ordering point.
pub fn membarrier(command: u32, _flags: u32) -> i64 {
    const QUERY: u32 = 0;
    const GLOBAL: u32 = 1;
    const PRIVATE_EXPEDITED: u32 = 8;
    const REGISTER_PRIVATE_EXPEDITED: u32 = 16;
    match command {
        QUERY => i64::from(GLOBAL | PRIVATE_EXPEDITED | REGISTER_PRIVATE_EXPEDITED),
        GLOBAL | PRIVATE_EXPEDITED | REGISTER_PRIVATE_EXPEDITED => {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            // Run on every CPU the group can occupy, forcing their barriers.
            sched::yield_now();
            0
        }
        _ => -abi::EINVAL,
    }
}

// ------------------------------------------------------------------- xattrs

/// No filesystem here stores extended attributes, which callers must handle.
pub fn xattr_unsupported() -> i64 {
    -abi::EOPNOTSUPP
}

pub fn xattr_list_empty() -> i64 {
    0
}

pub fn xattr_get_missing() -> i64 {
    -abi::ENODATA
}

// ------------------------------------------------------------------ select

const FD_SETSIZE: usize = 1024;

struct PollSlot {
    fd: i32,
    read: bool,
    write: bool,
    except: bool,
}

fn select_core(
    nfds: i32,
    read_set: u64,
    write_set: u64,
    except_set: u64,
    deadline: Option<u64>,
) -> Result<(i64, u64), VfsError> {
    if nfds < 0 {
        return Err(VfsError::InvalidArgument);
    }
    let nfds = (nfds as usize).min(FD_SETSIZE);
    let bytes = nfds.div_ceil(64) * 8;
    let load = |pointer: u64| -> Result<Vec<u8>, VfsError> {
        if pointer == 0 {
            Ok(vec![0; bytes])
        } else {
            uaccess::read_vec(pointer, bytes, 128)
        }
    };
    let read_bits = load(read_set)?;
    let write_bits = load(write_set)?;
    let except_bits = load(except_set)?;
    let test = |map: &[u8], fd: usize| map[fd / 8] & (1 << (fd % 8)) != 0;
    let mut slots = Vec::new();
    for fd in 0..nfds {
        let (read, write, except) = (
            test(&read_bits, fd),
            test(&write_bits, fd),
            test(&except_bits, fd),
        );
        if read || write || except {
            slots.push(PollSlot {
                fd: fd as i32,
                read,
                write,
                except,
            });
        }
    }
    let mut wait = crate::ipc::pollwait::PollWait::new();
    loop {
        if wait.registered() {
            let mut sources = crate::ipc::pollwait::PollSources::new();
            if let Some(deadline) = deadline {
                sources.expire_at(deadline);
            }
            let table = vfs::current_fd_table();
            let table = table.lock();
            for slot in &slots {
                if let Some(entry) = table.get(slot.fd as usize) {
                    entry.file.inode.poll_sources(&mut sources);
                }
            }
            drop(table);
            wait.arm(&sources);
        }
        let mut out_read = vec![0u8; bytes];
        let mut out_write = vec![0u8; bytes];
        let mut out_except = vec![0u8; bytes];
        let mut ready = 0;
        for slot in &slots {
            let table = vfs::current_fd_table();
            let events = match table.lock().get(slot.fd as usize) {
                Some(FdEntry { file, .. }) => file.inode.poll(),
                None => return Err(VfsError::BadFileDescriptor),
            };
            const POLLIN: u32 = 1;
            const POLLPRI: u32 = 2;
            const POLLOUT: u32 = 4;
            const POLLERR: u32 = 8;
            const POLLHUP: u32 = 16;
            let fd = slot.fd as usize;
            if slot.read && events & (POLLIN | POLLHUP | POLLERR) != 0 {
                out_read[fd / 8] |= 1 << (fd % 8);
                ready += 1;
            }
            if slot.write && events & (POLLOUT | POLLERR) != 0 {
                out_write[fd / 8] |= 1 << (fd % 8);
                ready += 1;
            }
            if slot.except && events & POLLPRI != 0 {
                out_except[fd / 8] |= 1 << (fd % 8);
                ready += 1;
            }
        }
        let now = crate::time::uptime_micros();
        let expired = deadline.is_some_and(|deadline| now >= deadline);
        if ready != 0 || expired {
            if read_set != 0 {
                uaccess::write(read_set, &out_read)?;
            }
            if write_set != 0 {
                uaccess::write(write_set, &out_write)?;
            }
            if except_set != 0 {
                uaccess::write(except_set, &out_except)?;
            }
            let left = deadline.map_or(0, |deadline| deadline.saturating_sub(now));
            return Ok((ready, left));
        }
        if crate::signal::pending_for_current() {
            return Err(VfsError::Interrupted);
        }
        wait.sleep();
    }
}

pub fn select(nfds: i32, read_set: u64, write_set: u64, except_set: u64, timeout: u64) -> i64 {
    let deadline = if timeout == 0 {
        None
    } else {
        let value: [i64; 2] = match uaccess::read_value(timeout) {
            Ok(value) => value,
            Err(error) => return error.to_errno(),
        };
        if value[0] < 0 || !(0..1_000_000).contains(&value[1]) {
            return -abi::EINVAL;
        }
        Some(
            crate::time::uptime_micros()
                .saturating_add((value[0] as u64).saturating_mul(1_000_000))
                .saturating_add(value[1] as u64),
        )
    };
    match select_core(nfds, read_set, write_set, except_set, deadline) {
        Ok((ready, left)) => {
            if timeout != 0 {
                let _ = uaccess::write_value(timeout, &[left / 1_000_000, left % 1_000_000]);
            }
            ready
        }
        Err(error) => error.to_errno(),
    }
}

pub fn pselect6(
    nfds: i32,
    read_set: u64,
    write_set: u64,
    except_set: u64,
    timeout: u64,
    signal_info: u64,
) -> i64 {
    let deadline = if timeout == 0 {
        None
    } else {
        let value: [i64; 2] = match uaccess::read_value(timeout) {
            Ok(value) => value,
            Err(error) => return error.to_errno(),
        };
        if value[0] < 0 || !(0..1_000_000_000).contains(&value[1]) {
            return -abi::EINVAL;
        }
        Some(
            crate::time::uptime_micros()
                .saturating_add((value[0] as u64).saturating_mul(1_000_000))
                .saturating_add((value[1] as u64).div_ceil(1000)),
        )
    };
    if signal_info != 0 {
        // pselect6 packs { const sigset_t *set; size_t size; } for six-argument ABIs.
        let info: [u64; 2] = match uaccess::read_value(signal_info) {
            Ok(info) => info,
            Err(error) => return error.to_errno(),
        };
        if info[0] != 0 {
            if info[1] != 8 {
                return -abi::EINVAL;
            }
            match uaccess::read_value::<u64>(info[0]) {
                Ok(mask) => crate::signal::install_wait_mask(mask),
                Err(error) => return error.to_errno(),
            }
        }
    }
    match select_core(nfds, read_set, write_set, except_set, deadline) {
        Ok((ready, left)) => {
            if timeout != 0 {
                let _ = uaccess::write_value(timeout, &[left / 1_000_000, left % 1_000_000 * 1000]);
            }
            ready
        }
        Err(error) => error.to_errno(),
    }
}

// -------------------------------------------------------------------- futex

pub fn set_robust_list(head: u64, length: u64) -> i64 {
    if length != 24 {
        return -abi::EINVAL;
    }
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    task.set_robust_list(head);
    0
}

pub fn get_robust_list(pid: u64, head: u64, length: u64) -> i64 {
    let task = if pid == 0 {
        sched::current()
    } else {
        sched::find_task(pid)
    };
    let Some(task) = task else {
        return -abi::ESRCH;
    };
    if let Err(error) = uaccess::write_value(head, &task.robust_list()) {
        return error.to_errno();
    }
    status(uaccess::write_value(length, &24u64))
}

// ----------------------------------------------------------------- reboot

pub fn reboot(magic1: u32, magic2: u32, command: u32) -> i64 {
    const MAGIC1: u32 = 0xFEE1_DEAD;
    const CMD_RESTART: u32 = 0x0123_4567;
    const CMD_HALT: u32 = 0xCDEF_0123;
    const CMD_POWER_OFF: u32 = 0x4321_FEDC;
    const CMD_CAD_ON: u32 = 0x89AB_CDEF;
    const CMD_CAD_OFF: u32 = 0;
    if magic1 != MAGIC1 || !matches!(magic2, 672274793 | 85072278 | 369367448 | 537993216) {
        return -abi::EINVAL;
    }
    if sched::current().is_some_and(|task| task.credentials().euid != 0) {
        return -abi::EPERM;
    }
    match command {
        CMD_CAD_ON | CMD_CAD_OFF => 0,
        CMD_RESTART | CMD_HALT | CMD_POWER_OFF => {
            vfs::sync_all();
            crate::kinfo!("reboot: filesystems synced, command {:#x}", command);
            crate::arch::system_reset(command == CMD_RESTART)
        }
        _ => -abi::EINVAL,
    }
}
