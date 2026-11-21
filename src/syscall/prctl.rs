/*
 *
 *       src/syscall/prctl.rs
 *       Linux prctl(2) process control operations
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The subset of `prctl(2)` backed by real task state.  Options that require
//! an unimplemented kernel facility (seccomp filters, capability bounding-set
//! mutation, LSM controls) return `EINVAL` instead of pretending to succeed.

use crate::{sched, user::uaccess, vfs::abi};

const PR_SET_PDEATHSIG: u64 = 1;
const PR_GET_PDEATHSIG: u64 = 2;
const PR_GET_DUMPABLE: u64 = 3;
const PR_SET_DUMPABLE: u64 = 4;
const PR_SET_NAME: u64 = 15;
const PR_GET_NAME: u64 = 16;
const PR_GET_SECCOMP: u64 = 21;
const PR_SET_SECCOMP: u64 = 22;
const PR_CAPBSET_READ: u64 = 23;
const PR_CAPBSET_DROP: u64 = 24;
const PR_SET_CHILD_SUBREAPER: u64 = 36;
const PR_GET_CHILD_SUBREAPER: u64 = 37;
const PR_SET_NO_NEW_PRIVS: u64 = 38;
const PR_GET_NO_NEW_PRIVS: u64 = 39;
const PR_GET_TID_ADDRESS: u64 = 40;

const CAP_LAST_CAP: u64 = 40;

fn current() -> Result<sched::TaskRef, i64> {
    sched::current().ok_or(-abi::ESRCH)
}

fn write_u32(pointer: u64, value: u32) -> i64 {
    match uaccess::write_value(pointer, &value) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

fn write_u64(pointer: u64, value: u64) -> i64 {
    match uaccess::write_value(pointer, &value) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn prctl(option: u64, argument: u64, _third: u64, _fourth: u64, _fifth: u64) -> i64 {
    let task = match current() {
        Ok(task) => task,
        Err(error) => return error,
    };
    match option {
        PR_SET_PDEATHSIG => {
            let signal = argument as u32;
            if signal >= 65 {
                return -abi::EINVAL;
            }
            task.set_pdeathsig(signal);
            0
        }
        PR_GET_PDEATHSIG => write_u32(argument, task.pdeathsig()),
        PR_GET_DUMPABLE => write_u32(argument, u32::from(task.dumpable())),
        PR_SET_DUMPABLE => match argument {
            0 | 1 => {
                task.set_dumpable(argument == 1);
                0
            }
            _ => -abi::EINVAL,
        },
        PR_SET_NAME => {
            let mut comm = [0u8; 16];
            for (index, slot) in comm.iter_mut().enumerate() {
                match uaccess::read_value::<u8>(argument + index as u64) {
                    Ok(0) => break,
                    Ok(byte) => *slot = byte,
                    Err(error) => return error.to_errno(),
                }
            }
            task.set_comm(&comm);
            0
        }
        PR_GET_NAME => match uaccess::write(argument, &task.comm()) {
            Ok(()) => 0,
            Err(error) => error.to_errno(),
        },
        PR_GET_SECCOMP => 0,
        // SECCOMP_MODE_STRICT/FILTER require a seccomp subsystem.
        PR_SET_SECCOMP => crate::seccomp::legacy(argument, _third),
        PR_CAPBSET_READ => {
            if argument > CAP_LAST_CAP {
                -abi::EINVAL
            } else {
                1
            }
        }
        PR_CAPBSET_DROP => -abi::EINVAL,
        PR_SET_CHILD_SUBREAPER => match argument {
            0 | 1 => {
                task.set_child_subreaper(argument == 1);
                0
            }
            _ => -abi::EINVAL,
        },
        PR_GET_CHILD_SUBREAPER => write_u32(argument, u32::from(task.child_subreaper())),
        PR_SET_NO_NEW_PRIVS => {
            if argument != 1 {
                -abi::EINVAL
            } else {
                task.set_no_new_privs(true);
                0
            }
        }
        PR_GET_NO_NEW_PRIVS => write_u32(argument, u32::from(task.no_new_privs())),
        PR_GET_TID_ADDRESS => write_u64(argument, task.clear_child_tid()),
        _ => -abi::EINVAL,
    }
}
