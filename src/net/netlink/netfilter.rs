/*
 *
 *       src/net/netlink/netfilter.rs
 *       NETLINK_NETFILTER (nfnetlink): subsystem table and batch framing
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mirrors `net/netfilter/nfnetlink.c` and `uapi/linux/netfilter/nfnetlink.h`.

use super::{KernelCfg, NETLINK_NETFILTER, Request, kernel_create, msg::ExtAck};
use crate::{
    sync::SpinLock,
    vfs::{VfsError, abi},
};
use alloc::vec::Vec;

pub const NFNETLINK_V0: u8 = 0;
pub const NFGENMSG_HDRLEN: usize = 4;

pub const NFNL_SUBSYS_NONE: u8 = 0;
pub const NFNL_SUBSYS_CTNETLINK: u8 = 1;
pub const NFNL_SUBSYS_CTNETLINK_EXP: u8 = 2;
pub const NFNL_SUBSYS_QUEUE: u8 = 3;
pub const NFNL_SUBSYS_ULOG: u8 = 4;
pub const NFNL_SUBSYS_OSF: u8 = 5;
pub const NFNL_SUBSYS_IPSET: u8 = 6;
pub const NFNL_SUBSYS_ACCT: u8 = 7;
pub const NFNL_SUBSYS_CTNETLINK_TIMEOUT: u8 = 8;
pub const NFNL_SUBSYS_CTHELPER: u8 = 9;
pub const NFNL_SUBSYS_NFTABLES: u8 = 10;
pub const NFNL_SUBSYS_NFT_COMPAT: u8 = 11;
pub const NFNL_SUBSYS_HOOK: u8 = 12;
pub const NFNL_SUBSYS_COUNT: usize = 16;

pub const NFNL_MSG_BATCH_BEGIN: u16 = 0x10;
pub const NFNL_MSG_BATCH_END: u16 = 0x11;

pub type NfCallback = fn(&Request<'_>, u8, u16, &[u8], &mut ExtAck) -> Result<(), i32>;

#[derive(Clone)]
pub struct NfSubsys {
    pub subsys_id: u8,
    pub callbacks: Vec<(u8, NfCallback)>,
}

static SUBSYSTEMS: SpinLock<[Option<NfSubsys>; NFNL_SUBSYS_COUNT]> =
    SpinLock::new([const { None }; NFNL_SUBSYS_COUNT]);

pub fn init() {
    let mut cfg = KernelCfg::new();
    cfg.groups = 32;
    cfg.input = Some(input);
    if let Err(error) = kernel_create(NETLINK_NETFILTER, cfg) {
        crate::kwarn!("net: NETLINK_NETFILTER registration failed: {error:?}");
        return;
    }
    crate::verbose::log(format_args!("net: NETLINK_NETFILTER registered"));
}

pub fn register_subsys(subsys: NfSubsys) -> Result<(), VfsError> {
    let idx = subsys.subsys_id as usize;
    if idx == 0 || idx >= NFNL_SUBSYS_COUNT {
        return Err(VfsError::InvalidArgument);
    }
    let mut table = SUBSYSTEMS.lock();
    if table[idx].is_some() {
        return Err(VfsError::AlreadyExists);
    }
    table[idx] = Some(subsys);
    Ok(())
}

pub fn unregister_subsys(subsys_id: u8) {
    let idx = subsys_id as usize;
    if idx < NFNL_SUBSYS_COUNT {
        SUBSYSTEMS.lock()[idx] = None;
    }
}

fn input(request: &Request<'_>, ext: &mut ExtAck) -> Result<(), i32> {
    if !request.is_privileged() {
        return Err(-(abi::EPERM as i32));
    }
    if request.payload.len() < NFGENMSG_HDRLEN {
        return Err(-(abi::EINVAL as i32));
    }
    let nfgen_family = request.payload[0];
    let version = request.payload[1];
    let res_id = u16::from_be_bytes([request.payload[2], request.payload[3]]);
    if version != NFNETLINK_V0 {
        return Err(-(abi::EINVAL as i32));
    }
    let kind = request.header.kind;
    if kind == NFNL_MSG_BATCH_BEGIN || kind == NFNL_MSG_BATCH_END {
        return Ok(());
    }
    let subsys_id = (kind >> 8) as usize;
    let msg_type = (kind & 0xff) as u8;
    if subsys_id == 0 || subsys_id >= NFNL_SUBSYS_COUNT {
        return Err(-(abi::EOPNOTSUPP as i32));
    }
    let callback = {
        let table = SUBSYSTEMS.lock();
        let Some(subsys) = table[subsys_id].as_ref() else {
            return Err(-(abi::EOPNOTSUPP as i32));
        };
        subsys
            .callbacks
            .iter()
            .find(|(id, _)| *id == msg_type)
            .map(|(_, cb)| *cb)
    };
    let Some(callback) = callback else {
        return Err(-(abi::EOPNOTSUPP as i32));
    };
    callback(
        request,
        nfgen_family,
        res_id,
        &request.payload[NFGENMSG_HDRLEN..],
        ext,
    )
}
