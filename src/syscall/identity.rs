/*
 *
 *       src/syscall/identity.rs
 *       Linux credential, group, umask and affinity system calls
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! POSIX identity calls with the update rules from Linux `kernel/sys.c` and
//! `kernel/groups.c`.  SpaceOS starts every process as root in the initial
//! user namespace; subsequent set* calls maintain the real/effective/saved
//! credentials exactly as Linux does.

use alloc::vec::Vec;

use crate::{
    sched::{self, Credentials, TaskRef},
    user::uaccess,
    vfs::{self, abi},
};

pub const NGROUPS_MAX: usize = 65536;
const INVALID_ID: u32 = u32::MAX;

fn current() -> Result<TaskRef, i64> {
    sched::current().ok_or(-abi::ESRCH)
}

fn with_creds(mut update: impl FnMut(&mut Credentials) -> Result<(), i64>) -> i64 {
    let task = match current() {
        Ok(task) => task,
        Err(error) => return error,
    };
    let mut creds = task.credentials();
    if let Err(error) = update(&mut creds) {
        return error;
    }
    task.set_credentials(creds);
    0
}

/// Filesystem identity of the current task for newly created inodes.
pub fn fs_ids() -> (u32, u32) {
    current().map_or((0, 0), |task| {
        let creds = task.credentials();
        (creds.fsuid, creds.fsgid)
    })
}

pub fn getuid() -> i64 {
    current().map_or(0, |task| task.credentials().uid as i64)
}

pub fn geteuid() -> i64 {
    current().map_or(0, |task| task.credentials().euid as i64)
}

pub fn getgid() -> i64 {
    current().map_or(0, |task| task.credentials().gid as i64)
}

pub fn getegid() -> i64 {
    current().map_or(0, |task| task.credentials().egid as i64)
}

fn valid_id(id: u32) -> Result<(), i64> {
    if id == INVALID_ID {
        Err(-abi::EINVAL)
    } else {
        Ok(())
    }
}

pub fn setuid(uid: u32) -> i64 {
    if let Err(error) = valid_id(uid) {
        return error;
    }
    with_creds(|creds| {
        if creds.euid == 0 {
            creds.uid = uid;
            creds.euid = uid;
            creds.suid = uid;
            creds.fsuid = uid;
            Ok(())
        } else if uid == creds.uid || uid == creds.suid {
            creds.euid = uid;
            creds.fsuid = uid;
            Ok(())
        } else {
            Err(-abi::EPERM)
        }
    })
}

pub fn setgid(gid: u32) -> i64 {
    if let Err(error) = valid_id(gid) {
        return error;
    }
    with_creds(|creds| {
        // Linux grants setgid privilege from CAP_SETGID (euid 0 here), not
        // from the effective group itself. A setuid-root sudo therefore may
        // setgid even though it carries the caller's egid.
        if creds.euid == 0 {
            creds.gid = gid;
            creds.egid = gid;
            creds.sgid = gid;
            creds.fsgid = gid;
            Ok(())
        } else if gid == creds.gid || gid == creds.sgid {
            creds.egid = gid;
            creds.fsgid = gid;
            Ok(())
        } else {
            Err(-abi::EPERM)
        }
    })
}

pub fn setreuid(ruid: u32, euid: u32) -> i64 {
    if (ruid != INVALID_ID && valid_id(ruid).is_err())
        || (euid != INVALID_ID && valid_id(euid).is_err())
    {
        return -abi::EINVAL;
    }
    with_creds(|creds| {
        let old_uid = creds.uid;
        let privileged = creds.euid == 0;
        let new_ruid = if ruid == INVALID_ID { old_uid } else { ruid };
        let new_euid = if euid == INVALID_ID { creds.euid } else { euid };
        if ruid != INVALID_ID
            && !privileged
            && new_ruid != creds.uid
            && new_ruid != creds.euid
            && new_ruid != creds.suid
        {
            return Err(-abi::EPERM);
        }
        if euid != INVALID_ID
            && !privileged
            && new_euid != creds.uid
            && new_euid != creds.euid
            && new_euid != creds.suid
        {
            return Err(-abi::EPERM);
        }
        if ruid != INVALID_ID {
            creds.uid = new_ruid;
        }
        if euid != INVALID_ID {
            creds.euid = new_euid;
        }
        if (ruid != INVALID_ID && new_ruid != old_uid)
            || (euid != INVALID_ID && new_euid != old_uid)
        {
            creds.suid = new_euid;
        }
        creds.fsuid = new_euid;
        Ok(())
    })
}

pub fn setregid(rgid: u32, egid: u32) -> i64 {
    if (rgid != INVALID_ID && valid_id(rgid).is_err())
        || (egid != INVALID_ID && valid_id(egid).is_err())
    {
        return -abi::EINVAL;
    }
    with_creds(|creds| {
        let old_gid = creds.gid;
        let privileged = creds.euid == 0;
        let new_rgid = if rgid == INVALID_ID { old_gid } else { rgid };
        let new_egid = if egid == INVALID_ID { creds.egid } else { egid };
        if rgid != INVALID_ID
            && !privileged
            && new_rgid != creds.gid
            && new_rgid != creds.egid
            && new_rgid != creds.sgid
        {
            return Err(-abi::EPERM);
        }
        if egid != INVALID_ID
            && !privileged
            && new_egid != creds.gid
            && new_egid != creds.egid
            && new_egid != creds.sgid
        {
            return Err(-abi::EPERM);
        }
        if rgid != INVALID_ID {
            creds.gid = new_rgid;
        }
        if egid != INVALID_ID {
            creds.egid = new_egid;
        }
        if (rgid != INVALID_ID && new_rgid != old_gid)
            || (egid != INVALID_ID && new_egid != old_gid)
        {
            creds.sgid = new_egid;
        }
        creds.fsgid = new_egid;
        Ok(())
    })
}

pub fn setresuid(ruid: u32, euid: u32, suid: u32) -> i64 {
    for id in [ruid, euid, suid] {
        if id != INVALID_ID && valid_id(id).is_err() {
            return -abi::EINVAL;
        }
    }
    with_creds(|creds| {
        let privileged = creds.euid == 0;
        for id in [ruid, euid, suid] {
            if id != INVALID_ID
                && !privileged
                && id != creds.uid
                && id != creds.euid
                && id != creds.suid
            {
                return Err(-abi::EPERM);
            }
        }
        if ruid != INVALID_ID {
            creds.uid = ruid;
        }
        if euid != INVALID_ID {
            creds.euid = euid;
        }
        if suid != INVALID_ID {
            creds.suid = suid;
        }
        creds.fsuid = creds.euid;
        Ok(())
    })
}

pub fn setresgid(rgid: u32, egid: u32, sgid: u32) -> i64 {
    for id in [rgid, egid, sgid] {
        if id != INVALID_ID && valid_id(id).is_err() {
            return -abi::EINVAL;
        }
    }
    with_creds(|creds| {
        let privileged = creds.euid == 0;
        for id in [rgid, egid, sgid] {
            if id != INVALID_ID
                && !privileged
                && id != creds.gid
                && id != creds.egid
                && id != creds.sgid
            {
                return Err(-abi::EPERM);
            }
        }
        if rgid != INVALID_ID {
            creds.gid = rgid;
        }
        if egid != INVALID_ID {
            creds.egid = egid;
        }
        if sgid != INVALID_ID {
            creds.sgid = sgid;
        }
        creds.fsgid = creds.egid;
        Ok(())
    })
}

fn write_ids(ruid: u64, euid: u64, suid: u64, real: u32, effective: u32, saved: u32) -> i64 {
    for (pointer, value) in [(ruid, real), (euid, effective), (suid, saved)] {
        if let Err(error) = uaccess::write_value(pointer, &value) {
            return error.to_errno();
        }
    }
    0
}

pub fn getresuid(ruid: u64, euid: u64, suid: u64) -> i64 {
    match current() {
        Ok(task) => {
            let creds = task.credentials();
            write_ids(ruid, euid, suid, creds.uid, creds.euid, creds.suid)
        }
        Err(error) => error,
    }
}

pub fn getresgid(rgid: u64, egid: u64, sgid: u64) -> i64 {
    match current() {
        Ok(task) => {
            let creds = task.credentials();
            write_ids(rgid, egid, sgid, creds.gid, creds.egid, creds.sgid)
        }
        Err(error) => error,
    }
}

pub fn setfsuid(uid: u32) -> i64 {
    if uid == INVALID_ID {
        return -abi::EINVAL;
    }
    let task = match current() {
        Ok(task) => task,
        Err(_) => return 0,
    };
    let mut creds = task.credentials();
    let old = creds.fsuid;
    if creds.euid == 0
        || uid == creds.uid
        || uid == creds.euid
        || uid == creds.suid
        || uid == creds.fsuid
    {
        creds.fsuid = uid;
        task.set_credentials(creds);
    }
    old as i64
}

pub fn setfsgid(gid: u32) -> i64 {
    if gid == INVALID_ID {
        return -abi::EINVAL;
    }
    let task = match current() {
        Ok(task) => task,
        Err(_) => return 0,
    };
    let mut creds = task.credentials();
    let old = creds.fsgid;
    if creds.euid == 0
        || gid == creds.gid
        || gid == creds.egid
        || gid == creds.sgid
        || gid == creds.fsgid
    {
        creds.fsgid = gid;
        task.set_credentials(creds);
    }
    old as i64
}

pub fn getgroups(size: u32, list: u64) -> i64 {
    let task = match current() {
        Ok(task) => task,
        Err(error) => return error,
    };
    let groups = task.groups();
    if size == 0 {
        return groups.len() as i64;
    }
    if (size as usize) < groups.len() {
        return -abi::EINVAL;
    }
    for (index, group) in groups.iter().enumerate() {
        if let Err(error) = uaccess::write_value(list + index as u64 * 4, group) {
            return error.to_errno();
        }
    }
    groups.len() as i64
}

pub fn setgroups(size: u32, list: u64) -> i64 {
    let task = match current() {
        Ok(task) => task,
        Err(error) => return error,
    };
    if task.credentials().euid != 0 {
        return -abi::EPERM;
    }
    let count = size as usize;
    if count > NGROUPS_MAX {
        return -abi::EINVAL;
    }
    let bytes = match uaccess::read_vec(list, count * 4, NGROUPS_MAX * 4) {
        Ok(bytes) => bytes,
        Err(error) => return error.to_errno(),
    };
    let mut groups = Vec::with_capacity(count);
    for chunk in bytes.chunks_exact(4) {
        groups.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    task.set_groups(groups);
    0
}

pub fn umask(mask: u32) -> i64 {
    let fs = vfs::current_fs();
    let mut context = fs.lock();
    let previous = context.umask;
    context.umask = mask & 0o777;
    previous as i64
}

pub fn sched_getaffinity(pid: u64, size: u32, mask: u64) -> i64 {
    let task = if pid == 0 {
        match current() {
            Ok(task) => task,
            Err(error) => return error,
        }
    } else {
        match sched::find_task(pid) {
            Some(task) => task,
            None => return -abi::ESRCH,
        }
    };
    let needed = sched::cpu_mask_bytes();
    if (size as usize) < needed {
        return -abi::EINVAL;
    }
    let affinity = task.stats().affinity;
    let mut words = affinity.words();
    // Linux reports the task's allowed mask intersected with possible online
    // CPUs; SpawnWithAffinity(CpuMask::all()) includes reserved slots.
    let online = crate::boot::cpu_count();
    for cpu in online..(sched::cpu_mask_bytes() * 8) {
        words[cpu / 64] &= !(1u64 << (cpu % 64));
    }
    let mut bytes = alloc::vec![0u8; needed];
    for (index, word) in words.iter().enumerate() {
        bytes[index * 8..index * 8 + 8].copy_from_slice(&word.to_ne_bytes());
    }
    match uaccess::write(mask, &bytes) {
        Ok(()) => needed as i64,
        Err(error) => error.to_errno(),
    }
}

pub fn sched_setaffinity(pid: u64, size: u32, mask: u64) -> i64 {
    let task = if pid == 0 {
        match current() {
            Ok(task) => task,
            Err(error) => return error,
        }
    } else {
        match sched::find_task(pid) {
            Some(task) => task,
            None => return -abi::ESRCH,
        }
    };
    let needed = sched::cpu_mask_bytes();
    if (size as usize) < needed {
        return -abi::EINVAL;
    }
    let bytes = match uaccess::read_vec(mask, needed, needed) {
        Ok(bytes) => bytes,
        Err(error) => return error.to_errno(),
    };
    let mut words = [0u64; 4];
    for (index, word) in words.iter_mut().enumerate() {
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&bytes[index * 8..index * 8 + 8]);
        *word = u64::from_ne_bytes(raw);
    }
    let requested = sched::CpuMask::from_words(words);
    let mut allowed = sched::CpuMask::empty();
    for cpu in 0..crate::boot::cpu_count() {
        if requested.contains(cpu) {
            allowed = allowed.with_cpu(cpu);
        }
    }
    if allowed.is_empty() {
        return -abi::EINVAL;
    }
    task.set_affinity(allowed);
    0
}
