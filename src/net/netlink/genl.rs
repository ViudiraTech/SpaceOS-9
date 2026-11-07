/*
 *
 *       src/net/netlink/genl.rs
 *       NETLINK_GENERIC: family registry, nlctrl and multicast groups
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mirrors `net/netlink/genetlink.c` and `uapi/linux/genetlink.h`.

use super::{
    KernelCfg, NETLINK_GENERIC, NL_CFG_F_NONROOT_RECV, Request, SKIP_ACK, broadcast, kernel_create,
    msg::{Attrs, ExtAck, MsgBuilder, NLM_F_DUMP},
};
use crate::{
    sync::SpinLock,
    vfs::{VfsError, abi},
};
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};

pub const GENL_ID_CTRL: u16 = 0x10;
pub const GENL_MIN_ID: u16 = GENL_ID_CTRL;
pub const GENL_MAX_ID: u16 = 1023;
pub const GENL_HDRLEN: usize = 4;

pub const GENL_ADMIN_PERM: u32 = 0x01;
pub const GENL_CMD_CAP_DO: u32 = 0x02;
pub const GENL_CMD_CAP_DUMP: u32 = 0x04;

pub const CTRL_CMD_UNSPEC: u8 = 0;
pub const CTRL_CMD_NEWFAMILY: u8 = 1;
pub const CTRL_CMD_DELFAMILY: u8 = 2;
pub const CTRL_CMD_GETFAMILY: u8 = 3;
pub const CTRL_CMD_NEWOPS: u8 = 4;
pub const CTRL_CMD_DELOPS: u8 = 5;
pub const CTRL_CMD_GETOPS: u8 = 6;
pub const CTRL_CMD_NEWMCAST_GRP: u8 = 7;
pub const CTRL_CMD_DELMCAST_GRP: u8 = 8;
pub const CTRL_CMD_GETMCAST_GRP: u8 = 9;

pub const CTRL_ATTR_UNSPEC: u16 = 0;
pub const CTRL_ATTR_FAMILY_ID: u16 = 1;
pub const CTRL_ATTR_FAMILY_NAME: u16 = 2;
pub const CTRL_ATTR_VERSION: u16 = 3;
pub const CTRL_ATTR_HDRSIZE: u16 = 4;
pub const CTRL_ATTR_MAXATTR: u16 = 5;
pub const CTRL_ATTR_OPS: u16 = 6;
pub const CTRL_ATTR_MCAST_GROUPS: u16 = 7;

pub const CTRL_ATTR_OP_ID: u16 = 1;
pub const CTRL_ATTR_OP_FLAGS: u16 = 2;

pub const CTRL_ATTR_MCAST_GRP_NAME: u16 = 1;
pub const CTRL_ATTR_MCAST_GRP_ID: u16 = 2;

const CTRL_NOTIFY_GRP_ID: u32 = 16;

pub type GenlHandler = fn(&Request<'_>, u8, u8, &[u8], &mut ExtAck) -> Result<(), i32>;

#[derive(Clone)]
pub struct GenlOp {
    pub cmd: u8,
    pub flags: u32,
    pub doit: Option<GenlHandler>,
    pub dumpit: Option<GenlHandler>,
}

#[derive(Clone)]
pub struct GenlMulticastGroup {
    pub name: String,
    pub id: u32,
}

#[derive(Clone)]
pub struct GenlFamily {
    pub id: u16,
    pub name: String,
    pub version: u32,
    pub hdrsize: u32,
    pub maxattr: u32,
    pub ops: Vec<GenlOp>,
    pub mcgrps: Vec<GenlMulticastGroup>,
}

struct Registry {
    next_id: u16,
    next_grp_id: u32,
    families: Vec<GenlFamily>,
}

static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry {
    next_id: GENL_ID_CTRL + 1,
    next_grp_id: CTRL_NOTIFY_GRP_ID + 1,
    families: Vec::new(),
});

pub fn init() {
    {
        let mut reg = REGISTRY.lock();
        if reg.families.is_empty() {
            reg.families.push(GenlFamily {
                id: GENL_ID_CTRL,
                name: String::from("nlctrl"),
                version: 2,
                hdrsize: 0,
                maxattr: 10,
                ops: alloc::vec![GenlOp {
                    cmd: CTRL_CMD_GETFAMILY,
                    flags: GENL_CMD_CAP_DO | GENL_CMD_CAP_DUMP,
                    doit: Some(ctrl_getfamily),
                    dumpit: Some(ctrl_getfamily),
                }],
                mcgrps: alloc::vec![GenlMulticastGroup {
                    name: String::from("notify"),
                    id: CTRL_NOTIFY_GRP_ID,
                }],
            });
        }
    }
    let mut cfg = KernelCfg::new();
    cfg.groups = 64;
    cfg.flags = NL_CFG_F_NONROOT_RECV;
    cfg.input = Some(input);
    if let Err(error) = kernel_create(NETLINK_GENERIC, cfg) {
        crate::kwarn!("net: NETLINK_GENERIC registration failed: {error:?}");
        return;
    }
    crate::verbose::log(format_args!("net: NETLINK_GENERIC registered"));
}

/// Register a Generic Netlink family (`genl_register_family`). Assigns a
/// dynamic family ID if `family.id == 0` and dynamic multicast group IDs.
pub fn register_family(mut family: GenlFamily) -> Result<u16, VfsError> {
    if family.name.is_empty() || family.name.len() >= 16 {
        return Err(VfsError::InvalidArgument);
    }
    let assigned_id;
    let snapshot;
    {
        let mut reg = REGISTRY.lock();
        if reg
            .families
            .iter()
            .any(|existing| existing.name == family.name)
        {
            return Err(VfsError::AlreadyExists);
        }
        if family.id == 0 {
            family.id = reg.next_id;
            reg.next_id = reg.next_id.saturating_add(1);
        }
        for grp in &mut family.mcgrps {
            if grp.id == 0 {
                grp.id = reg.next_grp_id;
                reg.next_grp_id = reg.next_grp_id.saturating_add(1);
            }
        }
        assigned_id = family.id;
        snapshot = family.clone();
        reg.families.push(family);
    }
    let msg = build_family_msg(&snapshot, CTRL_CMD_NEWFAMILY, 0, 0, 0);
    broadcast(NETLINK_GENERIC, 0, CTRL_NOTIFY_GRP_ID, &msg);
    Ok(assigned_id)
}

pub fn unregister_family(name: &str) -> Result<(), VfsError> {
    if name == "nlctrl" {
        return Err(VfsError::InvalidArgument);
    }
    let removed = {
        let mut reg = REGISTRY.lock();
        let Some(pos) = reg.families.iter().position(|f| f.name == name) else {
            return Err(VfsError::NotFound);
        };
        reg.families.remove(pos)
    };
    let msg = build_family_msg(&removed, CTRL_CMD_DELFAMILY, 0, 0, 0);
    broadcast(NETLINK_GENERIC, 0, CTRL_NOTIFY_GRP_ID, &msg);
    Ok(())
}

pub fn family_id_by_name(name: &str) -> Option<u16> {
    REGISTRY
        .lock()
        .families
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.id)
}

fn build_family_msg(family: &GenlFamily, cmd: u8, flags: u16, seq: u32, portid: u32) -> Vec<u8> {
    let mut builder = MsgBuilder::new(GENL_ID_CTRL, flags, seq, portid);
    builder.put_payload(&[cmd, 2, 0, 0]);
    builder.put_str(CTRL_ATTR_FAMILY_NAME, &family.name);
    builder.put_u16(CTRL_ATTR_FAMILY_ID, family.id);
    builder.put_u32(CTRL_ATTR_VERSION, family.version);
    builder.put_u32(CTRL_ATTR_HDRSIZE, family.hdrsize);
    builder.put_u32(CTRL_ATTR_MAXATTR, family.maxattr);
    if !family.ops.is_empty() {
        let mut ops_buf = MsgBuilder::new(0, 0, 0, 0);
        for (idx, op) in family.ops.iter().enumerate() {
            let mut one = MsgBuilder::new(0, 0, 0, 0);
            one.put_u32(CTRL_ATTR_OP_ID, u32::from(op.cmd));
            let mut op_flags = op.flags;
            if op.doit.is_some() {
                op_flags |= GENL_CMD_CAP_DO;
            }
            if op.dumpit.is_some() {
                op_flags |= GENL_CMD_CAP_DUMP;
            }
            one.put_u32(CTRL_ATTR_OP_FLAGS, op_flags);
            let nested = &one.finish()[16..];
            ops_buf.put_attr((idx + 1) as u16, nested);
        }
        let ops_bytes = &ops_buf.finish()[16..];
        builder.put_attr(CTRL_ATTR_OPS, ops_bytes);
    }
    if !family.mcgrps.is_empty() {
        let mut grp_buf = MsgBuilder::new(0, 0, 0, 0);
        for (idx, grp) in family.mcgrps.iter().enumerate() {
            let mut one = MsgBuilder::new(0, 0, 0, 0);
            one.put_str(CTRL_ATTR_MCAST_GRP_NAME, &grp.name);
            one.put_u32(CTRL_ATTR_MCAST_GRP_ID, grp.id);
            let nested = &one.finish()[16..];
            grp_buf.put_attr((idx + 1) as u16, nested);
        }
        let grp_bytes = &grp_buf.finish()[16..];
        builder.put_attr(CTRL_ATTR_MCAST_GROUPS, grp_bytes);
    }
    builder.finish()
}

fn ctrl_getfamily(
    request: &Request<'_>,
    _cmd: u8,
    _version: u8,
    attrs: &[u8],
    _ext: &mut ExtAck,
) -> Result<(), i32> {
    if request.header.flags & NLM_F_DUMP != 0 {
        let families: Vec<GenlFamily> = REGISTRY.lock().families.clone();
        let seq = request.header.seq;
        let portid = request.socket.portid();
        let mut next = 0usize;
        request.dump_start(Box::new(move |ctx| {
            while next < families.len() {
                let msg = build_family_msg(
                    &families[next],
                    CTRL_CMD_NEWFAMILY,
                    super::msg::NLM_F_MULTI,
                    seq,
                    portid,
                );
                if !ctx.try_put(&msg) {
                    return Ok(true);
                }
                next += 1;
            }
            Ok(false)
        }))?;
        return Err(SKIP_ACK);
    }

    let by_id = Attrs::find(attrs, CTRL_ATTR_FAMILY_ID).and_then(|a| a.u16());
    let by_name =
        Attrs::find(attrs, CTRL_ATTR_FAMILY_NAME).and_then(|a| a.string().map(ToString::to_string));
    if by_id.is_none() && by_name.is_none() {
        return Err(-(abi::EINVAL as i32));
    }
    let found = {
        let reg = REGISTRY.lock();
        reg.families
            .iter()
            .find(|f| {
                by_id.is_some_and(|id| f.id == id)
                    || by_name.as_deref().is_some_and(|name| f.name == name)
            })
            .cloned()
    }
    .ok_or(-(abi::ENOENT as i32))?;

    let reply = build_family_msg(
        &found,
        CTRL_CMD_NEWFAMILY,
        0,
        request.header.seq,
        request.socket.portid(),
    );
    request
        .reply(reply)
        .map_err(|error| error.to_errno() as i32)?;
    Ok(())
}

fn input(request: &Request<'_>, ext: &mut ExtAck) -> Result<(), i32> {
    let family_id = request.header.kind;
    if family_id < GENL_MIN_ID || request.payload.len() < GENL_HDRLEN {
        return Err(-(abi::EINVAL as i32));
    }
    let cmd = request.payload[0];
    let version = request.payload[1];
    let family = {
        let reg = REGISTRY.lock();
        reg.families.iter().find(|f| f.id == family_id).cloned()
    }
    .ok_or(-(abi::ENOENT as i32))?;

    let hdr_offset = GENL_HDRLEN + (family.hdrsize as usize);
    if request.payload.len() < hdr_offset {
        return Err(-(abi::EINVAL as i32));
    }
    let attrs = &request.payload[hdr_offset..];
    let Some(op) = family.ops.iter().find(|op| op.cmd == cmd) else {
        return Err(-(abi::EOPNOTSUPP as i32));
    };
    if op.flags & GENL_ADMIN_PERM != 0 && !request.is_privileged() {
        return Err(-(abi::EPERM as i32));
    }
    if request.header.flags & NLM_F_DUMP != 0 {
        if let Some(dumpit) = op.dumpit {
            return dumpit(request, cmd, version, attrs, ext);
        }
        return Err(-(abi::EOPNOTSUPP as i32));
    }
    if let Some(doit) = op.doit {
        return doit(request, cmd, version, attrs, ext);
    }
    if let Some(dumpit) = op.dumpit {
        return dumpit(request, cmd, version, attrs, ext);
    }
    Err(-(abi::EOPNOTSUPP as i32))
}
