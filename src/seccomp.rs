/*
 *       src/seccomp.rs
 *       Linux seccomp(2) classic-BPF filters
 *
 *       2026/11/22 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

//! Linux seccomp ABI: SECCOMP_SET_MODE_STRICT/FILTER, classic BPF
//! verification and evaluation, TSYNC/LOG, and fork/exec inheritance.
//! USER_NOTIF (NEW_LISTENER) is deliberately rejected with EOPNOTSUPP; it
//! requires the ptrace/notification layer and is not used by the spk sandbox.

use alloc::{sync::Arc, vec::Vec};

use crate::{sched, user::uaccess, vfs::abi};

pub const SECCOMP_SET_MODE_STRICT: u64 = 0;
pub const SECCOMP_SET_MODE_FILTER: u64 = 1;
pub const SECCOMP_GET_ACTION_AVAIL: u64 = 2;

pub const SECCOMP_FILTER_FLAG_TSYNC: u64 = 1;
pub const SECCOMP_FILTER_FLAG_LOG: u64 = 2;
pub const SECCOMP_FILTER_FLAG_SPEC_ALLOW: u64 = 4;
pub const SECCOMP_FILTER_FLAG_NEW_LISTENER: u64 = 8;
pub const SECCOMP_FILTER_FLAG_TSYNC_ESRCH: u64 = 16;

pub const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
pub const SECCOMP_RET_KILL_THREAD: u32 = 0x0000_0000;
pub const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
pub const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
pub const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
pub const SECCOMP_RET_TRACE: u32 = 0x7ff0_0000;
pub const SECCOMP_RET_LOG: u32 = 0x7ffc_0000;
pub const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
pub const SECCOMP_RET_ACTION_FULL: u32 = 0xffff_0000;
pub const SECCOMP_RET_DATA: u32 = 0x0000_ffff;

pub const BPF_MAXINSNS: usize = 4096;
const SECCOMP_DATA_SIZE: usize = 64;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

pub struct Filter {
    insns: Vec<SockFilter>,
    strict: bool,
    log: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Allow,
    Log,
    Errno(u16),
    Trap,
    KillThread,
    KillProcess,
}

fn action_of(value: u32) -> Action {
    match value & SECCOMP_RET_ACTION_FULL {
        SECCOMP_RET_ALLOW => Action::Allow,
        SECCOMP_RET_LOG => Action::Log,
        SECCOMP_RET_ERRNO => Action::Errno((value & SECCOMP_RET_DATA) as u16),
        SECCOMP_RET_TRAP => Action::Trap,
        SECCOMP_RET_KILL_THREAD => Action::KillThread,
        SECCOMP_RET_KILL_PROCESS => Action::KillProcess,
        // Unknown actions are treated as kill, matching Linux.
        _ => Action::KillProcess,
    }
}

fn action_rank(action: Action) -> u8 {
    match action {
        Action::KillProcess => 0,
        Action::KillThread => 1,
        Action::Trap => 2,
        Action::Errno(_) => 3,
        Action::Log => 4,
        Action::Allow => 5,
    }
}

fn arch_value() -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        0xC000_003E
    }
    #[cfg(target_arch = "aarch64")]
    {
        0xC000_00B7
    }
    #[cfg(target_arch = "riscv64")]
    {
        0xC000_00F3
    }
}

fn seccomp_data(nr: u64, args: &[u64; 6]) -> [u8; SECCOMP_DATA_SIZE] {
    let mut data = [0u8; SECCOMP_DATA_SIZE];
    data[0..4].copy_from_slice(&(nr as u32).to_le_bytes());
    data[4..8].copy_from_slice(&arch_value().to_le_bytes());
    // instruction_pointer is not part of dispatch(); keep it zero.
    for (index, arg) in args.iter().enumerate() {
        let offset = 16 + index * 8;
        data[offset..offset + 8].copy_from_slice(&arg.to_le_bytes());
    }
    data
}

fn bpf_class(code: u16) -> u16 {
    code & 7
}

fn bpf_size(code: u16) -> u16 {
    code & 0x18
}

fn bpf_mode(code: u16) -> u16 {
    code & 0xe0
}

fn bpf_source(code: u16) -> u16 {
    code & 0x08
}

fn bpf_op(code: u16) -> u16 {
    code & 0xf0
}

fn bpf_jump_op(code: u16) -> u16 {
    code & 0xf0
}

fn verify_filter(insns: &[SockFilter]) -> Result<(), i64> {
    const BPF_LD: u16 = 0;
    const BPF_LDX: u16 = 1;
    const BPF_ALU: u16 = 4;
    const BPF_JMP: u16 = 5;
    const BPF_RET: u16 = 6;
    const BPF_MISC: u16 = 7;
    const BPF_W: u16 = 0;
    const BPF_IMM: u16 = 0;
    const BPF_ABS: u16 = 0x20;
    const BPF_LEN: u16 = 0x80;
    const BPF_X: u16 = 0x08;
    const BPF_JA: u16 = 0x00;
    const BPF_K: u16 = 0x00;
    const BPF_A: u16 = 0x10;
    const BPF_TAX: u16 = 0x00;
    const BPF_TXA: u16 = 0x80;

    for (pc, insn) in insns.iter().enumerate() {
        if bpf_class(insn.code) == 6 {
            let value = if bpf_source(insn.code) == BPF_A {
                None
            } else {
                Some(insn.k)
            };
            if let Some(value) = value {
                let action = value & SECCOMP_RET_ACTION_FULL;
                if action == SECCOMP_RET_USER_NOTIF || action == SECCOMP_RET_TRACE {
                    return Err(-abi::EOPNOTSUPP);
                }
            }
        }
        match bpf_class(insn.code) {
            BPF_LD => {
                if bpf_size(insn.code) != BPF_W {
                    return Err(-abi::EINVAL);
                }
                match bpf_mode(insn.code) {
                    BPF_ABS => {
                        if !(insn.k as usize).is_multiple_of(4)
                            || insn.k as usize + 4 > SECCOMP_DATA_SIZE
                        {
                            return Err(-abi::EINVAL);
                        }
                    }
                    BPF_LEN | BPF_IMM => {}
                    _ => return Err(-abi::EINVAL),
                }
            }
            BPF_LDX => {
                if bpf_size(insn.code) != BPF_W {
                    return Err(-abi::EINVAL);
                }
                match bpf_mode(insn.code) {
                    BPF_LEN | BPF_IMM => {}
                    _ => return Err(-abi::EINVAL),
                }
            }
            BPF_ALU => {
                let op = bpf_op(insn.code);
                if !matches!(
                    op,
                    0x00 | 0x10 | 0x20 | 0x30 | 0x40 | 0x50 | 0x60 | 0x70 | 0x80 | 0x90 | 0xa0
                ) {
                    return Err(-abi::EINVAL);
                }
                if (op == 0x80 && bpf_source(insn.code) == BPF_X)
                    || (bpf_source(insn.code) != 0 && bpf_source(insn.code) != BPF_X)
                {
                    return Err(-abi::EINVAL);
                }
            }
            BPF_JMP => {
                let op = bpf_jump_op(insn.code);
                if !matches!(op, 0x00 | 0x10 | 0x20 | 0x30 | 0x40) {
                    return Err(-abi::EINVAL);
                }
                if op == BPF_JA {
                    let target = (pc as u64).saturating_add(1).saturating_add(insn.k as u64);
                    if target >= insns.len() as u64 {
                        return Err(-abi::EINVAL);
                    }
                } else {
                    let jt = (pc as u64).saturating_add(1).saturating_add(insn.jt as u64);
                    let jf = (pc as u64).saturating_add(1).saturating_add(insn.jf as u64);
                    if jt >= insns.len() as u64 || jf >= insns.len() as u64 {
                        return Err(-abi::EINVAL);
                    }
                    if bpf_source(insn.code) != BPF_K && bpf_source(insn.code) != BPF_X {
                        return Err(-abi::EINVAL);
                    }
                }
            }
            BPF_RET => match bpf_source(insn.code) {
                BPF_K | BPF_A => {}
                _ => return Err(-abi::EINVAL),
            },
            BPF_MISC => {
                if !matches!(insn.code, 0x07 | 0x87) {
                    return Err(-abi::EINVAL);
                }
            }
            _ => return Err(-abi::EINVAL),
        }
    }
    Ok(())
}

fn run_filter(filter: &Filter, nr: u64, args: &[u64; 6]) -> Action {
    if filter.strict {
        let allowed = [
            crate::syscall::numbers::SYS_READ,
            crate::syscall::numbers::SYS_WRITE,
            crate::syscall::numbers::SYS_EXIT,
            crate::syscall::numbers::SYS_RT_SIGRETURN,
        ];
        return if allowed.contains(&nr) {
            Action::Allow
        } else {
            Action::KillProcess
        };
    }

    let data = seccomp_data(nr, args);
    let mut accumulator = 0u32;
    let mut index = 0u32;
    let mut pc = 0usize;
    let mut steps = 0usize;
    loop {
        steps += 1;
        if steps > BPF_MAXINSNS * 2 || pc >= filter.insns.len() {
            return Action::KillProcess;
        }
        let insn = filter.insns[pc];
        match bpf_class(insn.code) {
            0 => {
                // BPF_LD
                match bpf_mode(insn.code) {
                    0x20 => {
                        let offset = insn.k as usize;
                        accumulator = u32::from_le_bytes(
                            data[offset..offset + 4].try_into().unwrap_or([0; 4]),
                        );
                    }
                    0x80 => accumulator = SECCOMP_DATA_SIZE as u32,
                    0x00 => accumulator = insn.k,
                    _ => return Action::KillProcess,
                }
                pc += 1;
            }
            1 => {
                // BPF_LDX
                match bpf_mode(insn.code) {
                    0x80 => index = SECCOMP_DATA_SIZE as u32,
                    0x00 => index = insn.k,
                    _ => return Action::KillProcess,
                }
                pc += 1;
            }
            4 => {
                // BPF_ALU
                let source = if bpf_source(insn.code) == 0x08 {
                    index
                } else {
                    insn.k
                };
                match bpf_op(insn.code) {
                    0x00 => accumulator = accumulator.wrapping_add(source),
                    0x10 => accumulator = accumulator.wrapping_sub(source),
                    0x20 => accumulator = accumulator.wrapping_mul(source),
                    0x30 => {
                        if source == 0 {
                            return Action::KillProcess;
                        }
                        accumulator /= source;
                    }
                    0x40 => accumulator |= source,
                    0x50 => accumulator &= source,
                    0x60 => accumulator = accumulator.wrapping_shl(source & 31),
                    0x70 => accumulator = accumulator.wrapping_shr(source & 31),
                    0x80 => accumulator = accumulator.wrapping_neg(),
                    0x90 => {
                        if source == 0 {
                            return Action::KillProcess;
                        }
                        accumulator %= source;
                    }
                    0xa0 => accumulator ^= source,
                    _ => return Action::KillProcess,
                }
                pc += 1;
            }
            5 => {
                // BPF_JMP
                let op = bpf_jump_op(insn.code);
                let source = if bpf_source(insn.code) == 0x08 {
                    index
                } else {
                    insn.k
                };
                let take = match op {
                    0x00 => {
                        pc = pc.saturating_add(1).saturating_add(insn.k as usize);
                        continue;
                    }
                    0x10 => accumulator == source,
                    0x20 => accumulator > source,
                    0x30 => accumulator >= source,
                    0x40 => accumulator & source != 0,
                    _ => return Action::KillProcess,
                };
                pc = if take {
                    pc.saturating_add(1).saturating_add(insn.jt as usize)
                } else {
                    pc.saturating_add(1).saturating_add(insn.jf as usize)
                };
            }
            6 => {
                // BPF_RET
                let value = if bpf_source(insn.code) == 0x10 {
                    accumulator
                } else {
                    insn.k
                };
                return action_of(value);
            }
            7 => {
                // BPF_MISC
                match insn.code {
                    0x07 => index = accumulator,
                    0x87 => accumulator = index,
                    _ => return Action::KillProcess,
                }
                pc += 1;
            }
            _ => return Action::KillProcess,
        }
    }
}

fn evaluate(filters: &[Arc<Filter>], nr: u64, args: &[u64; 6]) -> Action {
    let mut result = Action::Allow;
    for filter in filters {
        let action = run_filter(filter, nr, args);
        if filter.log && action != Action::Allow {
            crate::kwarn!("seccomp: syscall {} action {:?}", nr, action_rank(action));
        }
        if action_rank(action) < action_rank(result) {
            result = action;
        }
    }
    result
}

/// Called by the syscall dispatcher before the real handler.
pub fn pre_syscall(nr: u64, args: [u64; 6]) -> Option<i64> {
    let task = sched::current()?;
    let filters = task.seccomp_filters();
    if filters.is_empty() {
        return None;
    }
    match evaluate(&filters, nr, &args) {
        Action::Allow | Action::Log => None,
        Action::Errno(errno) => Some(-(errno as i64)),
        Action::Trap => {
            crate::signal::send_to_thread(
                &task,
                crate::signal::Signal::kernel(crate::signal::SIGSYS),
            );
            Some(-(crate::signal::SIGSYS as i64))
        }
        Action::KillThread => crate::sched::exit(-(crate::signal::SIGSYS as i32)),
        Action::KillProcess => crate::sched::exit_group(-(crate::signal::SIGSYS as i32)),
    }
}

fn parse_program(program_ptr: u64) -> Result<Vec<SockFilter>, i64> {
    let raw = uaccess::read_vec(program_ptr, 16, 16).map_err(|error| error.to_errno())?;
    let length = u16::from_le_bytes([raw[0], raw[1]]) as usize;
    let filter_ptr = u64::from_le_bytes(raw[8..16].try_into().unwrap());
    if length == 0 || length > BPF_MAXINSNS || filter_ptr == 0 {
        return Err(-abi::EINVAL);
    }
    let bytes = uaccess::read_vec(filter_ptr, length * 8, BPF_MAXINSNS * 8)
        .map_err(|error| error.to_errno())?;
    let mut insns = Vec::with_capacity(length);
    for chunk in bytes.chunks_exact(8) {
        insns.push(SockFilter {
            code: u16::from_le_bytes([chunk[0], chunk[1]]),
            jt: chunk[2],
            jf: chunk[3],
            k: u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]),
        });
    }
    verify_filter(&insns)?;
    Ok(insns)
}

fn install_filter(program_ptr: u64, flags: u64) -> i64 {
    if flags
        & !(SECCOMP_FILTER_FLAG_TSYNC
            | SECCOMP_FILTER_FLAG_LOG
            | SECCOMP_FILTER_FLAG_SPEC_ALLOW
            | SECCOMP_FILTER_FLAG_TSYNC_ESRCH)
        != 0
    {
        return -abi::EOPNOTSUPP;
    }
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    if !task.no_new_privs() && task.credentials().euid != 0 {
        return -abi::EACCES;
    }
    let insns = match parse_program(program_ptr) {
        Ok(insns) => insns,
        Err(error) => return error,
    };
    let filter = Arc::new(Filter {
        insns,
        strict: false,
        log: flags & SECCOMP_FILTER_FLAG_LOG != 0,
    });
    if flags & SECCOMP_FILTER_FLAG_TSYNC != 0 {
        let stats = task.stats();
        for member in sched::tasks_in_group(stats.tgid) {
            member.add_seccomp_filter(filter.clone());
        }
    } else {
        task.add_seccomp_filter(filter);
    }
    0
}

fn install_strict(flags: u64) -> i64 {
    if flags != 0 {
        return -abi::EINVAL;
    }
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    if !task.no_new_privs() && task.credentials().euid != 0 {
        return -abi::EACCES;
    }
    task.add_seccomp_filter(Arc::new(Filter {
        insns: Vec::new(),
        strict: true,
        log: false,
    }));
    0
}

pub fn legacy(mode: u64, program_ptr: u64) -> i64 {
    match mode {
        1 => install_strict(0),
        2 => install_filter(program_ptr, 0),
        _ => -abi::EINVAL,
    }
}

pub fn syscall(operation: u64, flags: u64, argument: u64) -> i64 {
    match operation {
        SECCOMP_SET_MODE_STRICT => install_strict(flags),
        SECCOMP_SET_MODE_FILTER => install_filter(argument, flags),
        SECCOMP_GET_ACTION_AVAIL => {
            let mut raw = [0u8; 4];
            if let Err(error) = uaccess::read(argument, &mut raw) {
                return error.to_errno();
            }
            let action = u32::from_le_bytes(raw) & SECCOMP_RET_ACTION_FULL;
            match action {
                SECCOMP_RET_ALLOW
                | SECCOMP_RET_LOG
                | SECCOMP_RET_ERRNO
                | SECCOMP_RET_TRAP
                | SECCOMP_RET_KILL_THREAD
                | SECCOMP_RET_KILL_PROCESS => 0,
                _ => -abi::EOPNOTSUPP,
            }
        }
        _ => -abi::EINVAL,
    }
}

#[cfg(feature = "boot-self-test")]
pub fn self_test() {
    use crate::syscall::numbers::{SYS_GETPID, SYS_GETPPID};
    let filter = Filter {
        insns: alloc::vec![
            SockFilter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0
            },
            SockFilter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: SYS_GETPID as u32
            },
            SockFilter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x0005_0001
            },
            SockFilter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ALLOW
            },
        ],
        strict: false,
        log: false,
    };
    assert!(verify_filter(&filter.insns).is_ok());
    assert_eq!(run_filter(&filter, SYS_GETPID, &[0; 6]), Action::Errno(1));
    assert_eq!(run_filter(&filter, SYS_GETPPID, &[0; 6]), Action::Allow);
}
