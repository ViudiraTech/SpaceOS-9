/*
 *
 *       src/net/netlink/route.rs
 *       NETLINK_ROUTE: link, address, route and neighbour configuration
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mirrors the handlers in `net/core/rtnetlink.c` together with the IPv4 parts
//! of `fib_frontend.c` and `arp.c`. Messages use the uapi layouts from
//! `linux/rtnetlink.h`; attributes are host-endian like the rest of netlink.

use super::{
    KernelCfg, NETLINK_ROUTE, NL_CFG_F_NONROOT_RECV, Request, SKIP_ACK, broadcast, kernel_create,
    msg::{Attrs, MsgBuilder},
};
use crate::{
    net::{
        dev::{self, NetDevice, NetdevEvent, StatsSnapshot},
        fib::{self, Route},
        ip::{IpAddr, Ipv4Addr, Ipv6Addr, mask_v4, mask_v6},
        ipv4::{addr, arp},
        ipv6::{addr as addr6, ndisc},
        neigh::{self, Neighbor},
    },
    vfs::{VfsError, abi},
};
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

pub const RTM_NEWLINK: u16 = 16;
pub const RTM_DELLINK: u16 = 17;
pub const RTM_GETLINK: u16 = 18;
pub const RTM_SETLINK: u16 = 19;
pub const RTM_NEWADDR: u16 = 20;
pub const RTM_DELADDR: u16 = 21;
pub const RTM_GETADDR: u16 = 22;
pub const RTM_NEWROUTE: u16 = 24;
pub const RTM_DELROUTE: u16 = 25;
pub const RTM_GETROUTE: u16 = 26;
pub const RTM_NEWNEIGH: u16 = 28;
pub const RTM_DELNEIGH: u16 = 29;
pub const RTM_GETNEIGH: u16 = 30;

pub const RTNLGRP_LINK: u32 = 1;
pub const RTNLGRP_NOTIFY: u32 = 2;
pub const RTNLGRP_NEIGH: u32 = 3;
pub const RTNLGRP_TC: u32 = 4;
pub const RTNLGRP_IPV4_IFADDR: u32 = 5;
pub const RTNLGRP_IPV4_MROUTE: u32 = 6;
pub const RTNLGRP_IPV4_ROUTE: u32 = 7;
pub const RTNLGRP_IPV4_RULE: u32 = 8;
pub const RTNLGRP_IPV6_IFADDR: u32 = 9;
pub const RTNLGRP_IPV6_MROUTE: u32 = 10;
pub const RTNLGRP_IPV6_ROUTE: u32 = 11;
pub const RTNLGRP_IPV6_IFINFO: u32 = 12;

const IFLA_UNSPEC: u16 = 0;
const IFLA_ADDRESS: u16 = 1;
const IFLA_BROADCAST: u16 = 2;
const IFLA_IFNAME: u16 = 3;
const IFLA_MTU: u16 = 4;
const IFLA_QDISC: u16 = 6;
const IFLA_STATS: u16 = 7;
const IFLA_TXQLEN: u16 = 13;
const IFLA_OPERSTATE: u16 = 16;
const IFLA_LINKMODE: u16 = 17;
const IFLA_IFALIAS: u16 = 20;
const IFLA_STATS64: u16 = 23;
const IFLA_GROUP: u16 = 27;
const IFLA_NUM_TX_QUEUES: u16 = 31;
const IFLA_NUM_RX_QUEUES: u16 = 32;
const IFLA_CARRIER: u16 = 33;
const IFLA_PERM_ADDRESS: u16 = 54;

const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_LABEL: u16 = 3;
const IFA_BROADCAST: u16 = 4;
const IFA_CACHEINFO: u16 = 6;
const IFA_FLAGS: u16 = 8;

const RTA_DST: u16 = 1;
const RTA_SRC: u16 = 2;
const RTA_IIF: u16 = 3;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PRIORITY: u16 = 6;
const RTA_PREFSRC: u16 = 7;
const RTA_METRICS: u16 = 8;
const RTA_CACHEINFO: u16 = 12;
const RTA_TABLE: u16 = 15;
const RTA_MARK: u16 = 16;

const NDA_DST: u16 = 1;
const NDA_LLADDR: u16 = 2;
const NDA_CACHEINFO: u16 = 3;
const NDA_PROBES: u16 = 4;
const NDA_IFINDEX: u16 = 6;

const AF_UNSPEC: u8 = 0;

const NLM_F_CREATE: u16 = 0x400;
const NLM_F_EXCL: u16 = 0x200;
const NLM_F_REPLACE: u16 = 0x100;
const NLM_F_APPEND: u16 = 0x800;

pub const NTF_USE: u8 = 0x01;
pub const NTF_SELF: u8 = 0x02;

const RTAX_MTU: u16 = 2;
const RTAX_HOPLIMIT: u16 = 10;

/// Link-layer notification hook installed once by [`init`].
pub fn init() {
    let mut cfg = KernelCfg::new();
    cfg.groups = 32;
    cfg.flags = NL_CFG_F_NONROOT_RECV;
    cfg.input = Some(input);
    if let Err(error) = kernel_create(NETLINK_ROUTE, cfg) {
        crate::kwarn!("net: NETLINK_ROUTE registration failed: {error:?}");
        return;
    }
    dev::register_notifier(device_event);
    addr::register_notifier(address_event);
    addr6::register_notifier(address6_event);
    neigh::register_notifier(neighbor_event);
    crate::verbose::log(format_args!("net: NETLINK_ROUTE registered"));
}

fn errno(error: VfsError) -> i32 {
    error.to_errno() as i32
}

fn need_privilege(request: &Request<'_>) -> Result<(), i32> {
    if request.is_privileged() {
        Ok(())
    } else {
        Err(-(abi::EPERM as i32))
    }
}

fn family_of_message(family: u8) -> Result<u8, i32> {
    match family {
        AF_UNSPEC | 2 | 10 => Ok(family),
        _ => Err(-(abi::EAFNOSUPPORT as i32)),
    }
}

fn parse_ip(family: u8, data: &[u8]) -> Option<IpAddr> {
    match family {
        2 => data
            .get(..4)
            .map(|raw| IpAddr::V4(Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]))),
        10 => {
            let raw = data.get(..16)?;
            let mut octets = [0u8; 16];
            octets.copy_from_slice(raw);
            Some(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        _ => None,
    }
}

fn ip_bytes(address: IpAddr) -> Vec<u8> {
    match address {
        IpAddr::V4(address) => address.octets().to_vec(),
        IpAddr::V6(address) => address.octets().to_vec(),
    }
}

fn family_of(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(_) => 2,
        IpAddr::V6(_) => 10,
    }
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn i32_at(data: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn pad_to(data: &mut Vec<u8>, alignment: usize) {
    while !data.len().is_multiple_of(alignment) {
        data.push(0);
    }
}

// ---- Link ------------------------------------------------------------------

fn link_message(
    device: &Arc<NetDevice>,
    kind: u16,
    flags_nl: u16,
    seq: u32,
    portid: u32,
    change: u32,
) -> Vec<u8> {
    let flags = device.flags();
    let mut family = Vec::with_capacity(16);
    family.push(AF_UNSPEC);
    family.push(0);
    family.extend_from_slice(&device.dev_type.to_le_bytes());
    family.extend_from_slice(&(device.ifindex() as i32).to_le_bytes());
    family.extend_from_slice(&flags.to_le_bytes());
    family.extend_from_slice(&change.to_le_bytes());

    let mut message = MsgBuilder::new(kind, flags_nl, seq, portid);
    message.put_payload(&family);
    message.put_attr(IFLA_ADDRESS, &device.hw_addr());
    message.put_attr(IFLA_BROADCAST, &device.broadcast);
    message.put_str(IFLA_IFNAME, &device.name());
    message.put_u32(IFLA_MTU, device.mtu());
    message.put_u32(
        IFLA_TXQLEN,
        device
            .tx_queue_len
            .load(core::sync::atomic::Ordering::Relaxed),
    );
    message.put_str(IFLA_QDISC, "pfifo_fast");
    message.put_u8(IFLA_OPERSTATE, device.operstate());
    message.put_u8(IFLA_LINKMODE, 0);
    message.put_u8(IFLA_CARRIER, device.carrier_ok() as u8);
    message.put_u32(IFLA_GROUP, 0);
    message.put_u32(IFLA_NUM_TX_QUEUES, 1);
    message.put_u32(IFLA_NUM_RX_QUEUES, 1);
    message.put_attr(IFLA_PERM_ADDRESS, device.perm_addr());
    let stats = device.stats_snapshot();
    message.put_attr(IFLA_STATS64, &stats64(&stats));
    message.put_attr(IFLA_STATS, &stats_legacy(&stats));
    message.finish()
}

fn stats64(stats: &StatsSnapshot) -> Vec<u8> {
    let values = [
        stats.rx_packets,
        stats.tx_packets,
        stats.rx_bytes,
        stats.tx_bytes,
        stats.rx_errors,
        stats.tx_errors,
        stats.rx_dropped,
        stats.tx_dropped,
        stats.multicast,
        stats.collisions,
    ];
    let mut out = Vec::with_capacity(25 * 8);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for _ in values.len()..25 {
        out.extend_from_slice(&0u64.to_le_bytes());
    }
    out
}

fn stats_legacy(stats: &StatsSnapshot) -> Vec<u8> {
    let values = [
        stats.rx_packets as u32,
        stats.tx_packets as u32,
        stats.rx_bytes as u32,
        stats.tx_bytes as u32,
        stats.rx_errors as u32,
        stats.tx_errors as u32,
        stats.rx_dropped as u32,
        stats.tx_dropped as u32,
        stats.multicast as u32,
        stats.collisions as u32,
    ];
    let mut out = Vec::with_capacity(23 * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for _ in values.len()..23 {
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    out
}

fn find_device(payload: &[u8]) -> Option<Arc<NetDevice>> {
    let index = i32_at(payload, 4).unwrap_or(0);
    if index > 0
        && let Some(device) = dev::by_index(index as u32)
    {
        return Some(device);
    }
    let attributes = payload.get(16..).unwrap_or_default();
    if let Some(name) = Attrs::find(attributes, IFLA_IFNAME) {
        return name.string().and_then(dev::by_name);
    }
    None
}

fn link_get(request: &Request<'_>, dump: bool) -> Result<(), i32> {
    if dump || request.header.flags & super::msg::NLM_F_DUMP != 0 {
        let devices = dev::devices();
        let seq = request.header.seq;
        let portid = request.socket.portid();
        let mut next = 0usize;
        request
            .dump_start(Box::new(move |ctx| {
                while next < devices.len() {
                    let message = link_message(
                        &devices[next],
                        RTM_NEWLINK,
                        super::msg::NLM_F_MULTI,
                        seq,
                        portid,
                        0,
                    );
                    if !ctx.try_put(&message) {
                        return Ok(true);
                    }
                    next += 1;
                }
                Ok(false)
            }))
            .map_err(|error| error)?;
        return Err(SKIP_ACK);
    }
    let device = find_device(request.payload).ok_or(-(abi::ENODEV as i32))?;
    let message = link_message(
        &device,
        RTM_NEWLINK,
        0,
        request.header.seq,
        request.socket.portid(),
        0,
    );
    request.reply(message).map_err(errno)?;
    Ok(())
}

fn link_set(request: &Request<'_>) -> Result<(), i32> {
    need_privilege(request)?;
    let device = find_device(request.payload).ok_or(-(abi::ENODEV as i32))?;
    let flags = u32_at(request.payload, 8).unwrap_or(0);
    let change = u32_at(request.payload, 12).unwrap_or(0);
    let attributes = request.payload.get(16..).unwrap_or_default();
    let _rtnl = dev::rtnl_lock();
    if change != 0 {
        dev::change_flags_locked(&device, flags).map_err(errno)?;
    }
    if let Some(mtu) = Attrs::find(attributes, IFLA_MTU).and_then(|attr| attr.u32()) {
        dev::change_mtu_locked(&device, mtu).map_err(errno)?;
    }
    if let Some(address) = Attrs::find(attributes, IFLA_ADDRESS) {
        dev::set_mac_locked(&device, address.data).map_err(errno)?;
    }
    if let Some(name) =
        Attrs::find(attributes, IFLA_IFNAME).and_then(|attr| attr.string().map(String::from))
    {
        dev::rename_locked(&device, &name).map_err(errno)?;
    }
    if let Some(alias) =
        Attrs::find(attributes, IFLA_IFALIAS).and_then(|attr| attr.string().map(String::from))
    {
        let _ = alias;
    }
    Ok(())
}

// ---- Addresses ----------------------------------------------------------------

fn address_message(ifaddr: &addr::IfAddr, kind: u16, flags: u16, seq: u32, portid: u32) -> Vec<u8> {
    let mut family = Vec::with_capacity(8);
    family.push(2u8);
    family.push(ifaddr.prefixlen);
    family.push((ifaddr.flags & 0xff) as u8);
    family.push(ifaddr.scope);
    family.extend_from_slice(&ifaddr.ifindex.to_le_bytes());

    let mut message = MsgBuilder::new(kind, flags, seq, portid);
    message.put_payload(&family);
    message.put_attr(IFA_ADDRESS, &ifaddr.address.octets());
    message.put_attr(IFA_LOCAL, &ifaddr.local.octets());
    message.put_str(IFA_LABEL, &ifaddr.label);
    if let Some(broadcast) = ifaddr.broadcast {
        message.put_attr(IFA_BROADCAST, &broadcast.octets());
    }
    message.put_u32(IFA_FLAGS, ifaddr.flags);
    message.put_attr(IFA_CACHEINFO, &[0u8; 16]);
    message.finish()
}

fn address6_message(
    ifaddr: &addr6::IfAddr6,
    kind: u16,
    flags: u16,
    seq: u32,
    portid: u32,
) -> Vec<u8> {
    let mut family = Vec::with_capacity(8);
    family.push(10u8);
    family.push(ifaddr.prefixlen);
    family.push((ifaddr.flags & 0xff) as u8);
    family.push(ifaddr.scope);
    family.extend_from_slice(&ifaddr.ifindex.to_le_bytes());

    let mut message = MsgBuilder::new(kind, flags, seq, portid);
    message.put_payload(&family);
    message.put_attr(IFA_ADDRESS, &ifaddr.address.octets());
    message.put_u32(IFA_FLAGS, ifaddr.flags);
    message.put_attr(IFA_CACHEINFO, &[0u8; 16]);
    message.finish()
}

fn address_event(event: addr::AddrEvent, ifaddr: &addr::IfAddr) {
    let kind = match event {
        addr::AddrEvent::Add => RTM_NEWADDR,
        addr::AddrEvent::Delete => RTM_DELADDR,
    };
    let mut message = address_message(ifaddr, kind, 0, 0, 0);
    message[4..6].copy_from_slice(&kind.to_le_bytes());
    broadcast(NETLINK_ROUTE, 0, RTNLGRP_IPV4_IFADDR, &message);
    broadcast(NETLINK_ROUTE, 0, RTNLGRP_NOTIFY, &message);
}

fn address6_event(event: addr6::Addr6Event, ifaddr: &addr6::IfAddr6) {
    let kind = match event {
        addr6::Addr6Event::Add => RTM_NEWADDR,
        addr6::Addr6Event::Delete => RTM_DELADDR,
    };
    let mut message = address6_message(ifaddr, kind, 0, 0, 0);
    message[4..6].copy_from_slice(&kind.to_le_bytes());
    broadcast(NETLINK_ROUTE, 0, RTNLGRP_IPV6_IFADDR, &message);
    broadcast(NETLINK_ROUTE, 0, RTNLGRP_NOTIFY, &message);
}

fn address_get(request: &Request<'_>, dump: bool) -> Result<(), i32> {
    let default_family = request.payload.first().copied().unwrap_or(2);
    let filter_index = u32_at(request.payload, 4).unwrap_or(0);
    if dump || request.header.flags & super::msg::NLM_F_DUMP != 0 {
        let family = family_of_message(default_family)?;
        let seq = request.header.seq;
        let portid = request.socket.portid();
        let mut messages: Vec<Vec<u8>> = Vec::new();
        if family == AF_UNSPEC || family == 2 {
            for entry in addr::snapshot() {
                if filter_index == 0 || entry.ifindex == filter_index {
                    messages.push(address_message(
                        &entry,
                        RTM_NEWADDR,
                        super::msg::NLM_F_MULTI,
                        seq,
                        portid,
                    ));
                }
            }
        }
        if family == AF_UNSPEC || family == 10 {
            for entry in addr6::snapshot() {
                if filter_index == 0 || entry.ifindex == filter_index {
                    messages.push(address6_message(
                        &entry,
                        RTM_NEWADDR,
                        super::msg::NLM_F_MULTI,
                        seq,
                        portid,
                    ));
                }
            }
        }
        let mut next = 0usize;
        request
            .dump_start(Box::new(move |ctx| {
                while next < messages.len() {
                    if !ctx.try_put(&messages[next]) {
                        return Ok(true);
                    }
                    next += 1;
                }
                Ok(false)
            }))
            .map_err(|error| error)?;
        return Err(SKIP_ACK);
    }
    let family = family_of_message(default_family)?;
    let parse_fam = if family == 10 { 10 } else { 2 };
    let address = Attrs::find(request.payload.get(8..).unwrap_or_default(), IFA_LOCAL)
        .or_else(|| Attrs::find(request.payload.get(8..).unwrap_or_default(), IFA_ADDRESS))
        .and_then(|attr| parse_ip(parse_fam, attr.data))
        .ok_or(-(abi::EINVAL as i32))?;
    let index = filter_index;
    let payload_bytes = match address {
        IpAddr::V4(address) => {
            let entry = if index != 0 {
                addr::get(index, address)
            } else {
                addr::snapshot()
                    .into_iter()
                    .find(|entry| entry.address == address)
            }
            .ok_or(-(abi::ENOENT as i32))?;
            address_message(&entry, RTM_NEWADDR, 0, 0, 0)[16..].to_vec()
        }
        IpAddr::V6(address) => {
            let entry = if index != 0 {
                addr6::get(index, address)
            } else {
                addr6::snapshot()
                    .into_iter()
                    .find(|entry| entry.address == address)
            }
            .ok_or(-(abi::ENOENT as i32))?;
            address6_message(&entry, RTM_NEWADDR, 0, 0, 0)[16..].to_vec()
        }
    };
    let mut message = MsgBuilder::new(RTM_NEWADDR, 0, request.header.seq, request.socket.portid());
    message.put_payload(&payload_bytes);
    request.reply(message.finish()).map_err(errno)?;
    Ok(())
}

fn address_add(request: &Request<'_>) -> Result<(), i32> {
    need_privilege(request)?;
    let family = family_of_message(request.payload.first().copied().unwrap_or(2))?;
    let ifindex = u32_at(request.payload, 4).ok_or(-(abi::EINVAL as i32))?;
    if ifindex == 0 {
        return Err(-(abi::EINVAL as i32));
    }
    if family == 10 {
        let prefixlen = request.payload.get(1).copied().unwrap_or(128);
        let flags = request.payload.get(2).copied().unwrap_or(0) as u32;
        let scope = request.payload.get(3).copied().unwrap_or(0);
        let attributes = request.payload.get(8..).unwrap_or_default();
        let IpAddr::V6(local) = Attrs::find(attributes, IFA_LOCAL)
            .or_else(|| Attrs::find(attributes, IFA_ADDRESS))
            .and_then(|attr| parse_ip(10, attr.data))
            .ok_or(-(abi::EINVAL as i32))?
        else {
            return Err(-(abi::EINVAL as i32));
        };
        let entry = addr6::IfAddr6 {
            ifindex,
            address: local,
            prefixlen,
            scope,
            flags: flags | addr::IFA_F_PERMANENT,
        };
        let _rtnl = dev::rtnl_lock();
        addr6::add(entry).map_err(errno)?;
        if let Some(device) = dev::by_index(ifindex) {
            ndisc::announce(&device, local);
        }
        return Ok(());
    }
    let prefixlen = request.payload.get(1).copied().unwrap_or(32);
    let flags = request.payload.get(2).copied().unwrap_or(0) as u32;
    let scope = request
        .payload
        .get(3)
        .copied()
        .unwrap_or(fib::RT_SCOPE_UNIVERSE);
    let attributes = request.payload.get(8..).unwrap_or_default();
    let local = Attrs::find(attributes, IFA_LOCAL)
        .or_else(|| Attrs::find(attributes, IFA_ADDRESS))
        .and_then(|attr| attr.data.get(..4))
        .map(|raw| Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]))
        .ok_or(-(abi::EINVAL as i32))?;
    let peer = Attrs::find(attributes, IFA_ADDRESS)
        .and_then(|attr| attr.data.get(..4))
        .map(|raw| Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]))
        .filter(|address| *address != local);
    let broadcast = Attrs::find(attributes, IFA_BROADCAST)
        .and_then(|attr| attr.data.get(..4))
        .map(|raw| Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]))
        .or_else(|| (prefixlen < 31).then(|| fib::broadcast_v4(local, prefixlen)));
    let label = Attrs::find(attributes, IFA_LABEL).and_then(|attr| attr.string().map(String::from));
    let entry = addr::IfAddr {
        ifindex,
        address: local,
        local: peer.unwrap_or(local),
        prefixlen,
        broadcast,
        scope,
        flags: flags | addr::IFA_F_PERMANENT,
        label: label.unwrap_or_default(),
    };
    let _rtnl = dev::rtnl_lock();
    addr::add(entry).map_err(errno)?;
    if let Some(device) = dev::by_index(ifindex) {
        if device.dev_type == dev::ARPHRD_ETHER {
            arp::announce(&device, local);
        }
    }
    Ok(())
}

fn address_delete(request: &Request<'_>) -> Result<(), i32> {
    need_privilege(request)?;
    let family = family_of_message(request.payload.first().copied().unwrap_or(2))?;
    let ifindex = u32_at(request.payload, 4).ok_or(-(abi::EINVAL as i32))?;
    let attributes = request.payload.get(8..).unwrap_or_default();
    if family == 10 {
        let prefixlen = request.payload.get(1).copied().unwrap_or(128);
        let IpAddr::V6(local) = Attrs::find(attributes, IFA_LOCAL)
            .or_else(|| Attrs::find(attributes, IFA_ADDRESS))
            .and_then(|attr| parse_ip(10, attr.data))
            .ok_or(-(abi::EINVAL as i32))?
        else {
            return Err(-(abi::EINVAL as i32));
        };
        let _rtnl = dev::rtnl_lock();
        addr6::delete(ifindex, local, Some(prefixlen)).map_err(errno)?;
        return Ok(());
    }
    let prefixlen = request.payload.get(1).copied().unwrap_or(32);
    let local = Attrs::find(attributes, IFA_LOCAL)
        .or_else(|| Attrs::find(attributes, IFA_ADDRESS))
        .and_then(|attr| attr.data.get(..4))
        .map(|raw| Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]))
        .ok_or(-(abi::EINVAL as i32))?;
    let _rtnl = dev::rtnl_lock();
    addr::delete(ifindex, local, Some(prefixlen)).map_err(errno)?;
    Ok(())
}

// ---- Routes ------------------------------------------------------------------

fn route_message(route: &Route, flags: u16, seq: u32, portid: u32) -> Vec<u8> {
    let dst_len = route.prefixlen;
    let table = if route.table <= 255 {
        route.table as u8
    } else {
        0
    };
    let mut header = Vec::with_capacity(12);
    header.push(route.family);
    header.push(dst_len);
    header.push(0);
    header.push(0);
    header.push(table);
    header.push(route.protocol);
    header.push(route.scope);
    header.push(route.kind);
    header.extend_from_slice(&route.flags.to_le_bytes());

    let mut message = MsgBuilder::new(RTM_NEWROUTE, flags, seq, portid);
    message.put_payload(&header);
    if dst_len != 0 {
        message.put_attr(
            RTA_DST,
            &ip_bytes(route.destination)[..prefix_bytes(route.family, dst_len)],
        );
    }
    if let Some(gateway) = route.gateway {
        message.put_attr(RTA_GATEWAY, &ip_bytes(gateway));
    }
    if let Some(source) = route.source {
        message.put_attr(RTA_PREFSRC, &ip_bytes(source));
    }
    message.put_u32(RTA_OIF, route.ifindex);
    message.put_u32(RTA_PRIORITY, route.metric);
    if route.table > 255 {
        message.put_u32(RTA_TABLE, route.table);
    }
    message.put_attr(RTA_CACHEINFO, &[0u8; 12]);
    message.finish()
}

fn prefix_bytes(family: u8, prefixlen: u8) -> usize {
    let full = if family == 10 { 16 } else { 4 };
    (((prefixlen as usize) + 7) / 8).min(full)
}

fn route_event(route: &Route) {
    let group = if route.family == 10 {
        RTNLGRP_IPV6_ROUTE
    } else {
        RTNLGRP_IPV4_ROUTE
    };
    let message = route_message(route, 0, 0, 0);
    broadcast(NETLINK_ROUTE, 0, group, &message);
    broadcast(NETLINK_ROUTE, 0, RTNLGRP_NOTIFY, &message);
}

fn route_get(request: &Request<'_>, dump: bool) -> Result<(), i32> {
    let family = family_of_message(request.payload.first().copied().unwrap_or(AF_UNSPEC))?;
    let attributes = request.payload.get(12..).unwrap_or_default();
    let rta_table = Attrs::find(attributes, RTA_TABLE).and_then(|attr| attr.u32());
    let table = rta_table.unwrap_or_else(|| {
        let raw = request.payload.get(4).copied().unwrap_or(0) as u32;
        if raw == 0 { fib::RT_TABLE_MAIN } else { raw }
    });
    let destination =
        Attrs::find(attributes, RTA_DST).and_then(|attr| parse_ip(family.max(2), attr.data));

    if dump || request.header.flags & super::msg::NLM_F_DUMP != 0 {
        let routes: Vec<Route> = fib::snapshot()
            .into_iter()
            .filter(|route| family == AF_UNSPEC || route.family == family)
            .filter(|route| rta_table.is_none_or(|table| route.table == table))
            .filter(|route| destination.is_none_or(|destination| route.destination == destination))
            .collect();
        let seq = request.header.seq;
        let portid = request.socket.portid();
        let mut next = 0usize;
        request
            .dump_start(Box::new(move |ctx| {
                while next < routes.len() {
                    let message =
                        route_message(&routes[next], super::msg::NLM_F_MULTI, seq, portid);
                    if !ctx.try_put(&message) {
                        return Ok(true);
                    }
                    next += 1;
                }
                Ok(false)
            }))
            .map_err(|error| error)?;
        return Err(SKIP_ACK);
    }

    let route = match destination {
        Some(destination) => {
            fib::lookup_table(destination, table).or_else(|| fib::lookup(destination))
        }
        None => None,
    }
    .ok_or(-(abi::ENETUNREACH as i32))?;
    let message = route_message(&route, 0, request.header.seq, request.socket.portid());
    request.reply(message).map_err(errno)?;
    Ok(())
}

struct RouteRequest {
    family: u8,
    destination: Option<IpAddr>,
    prefixlen: u8,
    table: u32,
    gateway: Option<IpAddr>,
    ifindex: u32,
    source: Option<IpAddr>,
    metric: u32,
    protocol: u8,
    scope: u8,
    kind: u8,
}

fn parse_route_request(payload: &[u8]) -> Result<RouteRequest, i32> {
    if payload.len() < 12 {
        return Err(-(abi::EINVAL as i32));
    }
    let family = family_of_message(payload[0])?;
    let attributes = &payload[12..];
    let destination =
        Attrs::find(attributes, RTA_DST).and_then(|attr| parse_ip(family.max(2), attr.data));
    let gateway =
        Attrs::find(attributes, RTA_GATEWAY).and_then(|attr| parse_ip(family.max(2), attr.data));
    let source =
        Attrs::find(attributes, RTA_PREFSRC).and_then(|attr| parse_ip(family.max(2), attr.data));
    let table = Attrs::find(attributes, RTA_TABLE)
        .and_then(|attr| attr.u32())
        .unwrap_or_else(|| {
            let raw = payload[4] as u32;
            if raw == 0 { fib::RT_TABLE_MAIN } else { raw }
        });
    let mut scope = payload[6];
    if scope == 0
        && gateway.is_none()
        && destination.is_some_and(|destination| destination != IpAddr::V4(Ipv4Addr::UNSPECIFIED))
    {
        scope = fib::RT_SCOPE_LINK;
    }
    Ok(RouteRequest {
        family,
        destination,
        prefixlen: payload[1],
        table,
        gateway,
        ifindex: Attrs::find(attributes, RTA_OIF)
            .and_then(|attr| attr.u32())
            .unwrap_or(0),
        source,
        metric: Attrs::find(attributes, RTA_PRIORITY)
            .and_then(|attr| attr.u32())
            .unwrap_or(0),
        protocol: payload[5],
        scope,
        kind: if payload[7] == fib::RTN_UNSPEC {
            fib::RTN_UNICAST
        } else {
            payload[7]
        },
    })
}

fn route_add(request: &Request<'_>) -> Result<(), i32> {
    need_privilege(request)?;
    let parsed = parse_route_request(request.payload)?;
    let destination = parsed.destination.unwrap_or(match parsed.family {
        10 => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        _ => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    });
    let max_prefix = if parsed.family == 10 { 128 } else { 32 };
    if parsed.prefixlen > max_prefix {
        return Err(-(abi::EINVAL as i32));
    }
    let ifindex = if parsed.ifindex != 0 {
        parsed.ifindex
    } else if let Some(gateway) = parsed.gateway {
        fib::lookup(gateway).map(|route| route.ifindex).unwrap_or(0)
    } else {
        0
    };
    if ifindex == 0 || dev::by_index(ifindex).is_none() {
        return Err(-(abi::ENODEV as i32));
    }
    let mut route = Route::new(destination, parsed.prefixlen);
    route.table = parsed.table;
    route.gateway = parsed.gateway;
    route.ifindex = ifindex;
    route.source = parsed.source;
    route.protocol = parsed.protocol;
    route.scope = parsed.scope;
    route.kind = parsed.kind;
    route.metric = parsed.metric;
    route.flags = fib::RTF_UP
        | if parsed.gateway.is_some() {
            fib::RTF_GATEWAY
        } else {
            0
        };
    if route.kind == fib::RTN_UNREACHABLE || route.kind == fib::RTN_BLACKHOLE {
        route.ifindex = 0;
    }
    let replace = request.header.flags & NLM_F_REPLACE != 0;
    let _rtnl = dev::rtnl_lock();
    if replace {
        fib::delete(&fib::DeleteFilter {
            table: Some(route.table),
            family: Some(route.family),
            destination: Some(mask_ip(destination, parsed.prefixlen)),
            prefixlen: Some(parsed.prefixlen),
            ..fib::DeleteFilter::default()
        });
    }
    fib::add(route.clone()).map_err(errno)?;
    route_event(&route);
    Ok(())
}

fn mask_ip(address: IpAddr, prefixlen: u8) -> IpAddr {
    match address {
        IpAddr::V4(address) => IpAddr::V4(mask_v4(address, prefixlen)),
        IpAddr::V6(address) => IpAddr::V6(mask_v6(address, prefixlen)),
    }
}

fn route_delete(request: &Request<'_>) -> Result<(), i32> {
    need_privilege(request)?;
    let parsed = parse_route_request(request.payload)?;
    let destination = parsed
        .destination
        .map(|address| mask_ip(address, parsed.prefixlen));
    let filter = fib::DeleteFilter {
        table: Some(parsed.table),
        family: Some(parsed.family.max(2)),
        destination,
        prefixlen: destination.map(|_| parsed.prefixlen),
        gateway: parsed.gateway,
        ifindex: (parsed.ifindex != 0).then_some(parsed.ifindex),
        kind: None,
    };
    let _rtnl = dev::rtnl_lock();
    if !fib::delete(&filter) {
        return Err(-(abi::ESRCH as i32));
    }
    let mut event = Route::new(
        destination.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
        parsed.prefixlen,
    );
    event.table = parsed.table;
    event.family = parsed.family.max(2);
    event.ifindex = parsed.ifindex;
    event.gateway = parsed.gateway;
    let message = route_message(&event, 0, 0, 0);
    let group = if event.family == 10 {
        RTNLGRP_IPV6_ROUTE
    } else {
        RTNLGRP_IPV4_ROUTE
    };
    broadcast(NETLINK_ROUTE, 0, group, &message);
    Ok(())
}

// ---- Neighbours --------------------------------------------------------------

fn neighbor_message(neighbor: &Neighbor, kind: u16, flags: u16, seq: u32, portid: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(12);
    header.push(neighbor.family);
    header.push(0);
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&(neighbor.ifindex as i32).to_le_bytes());
    header.extend_from_slice(&neighbor.state.to_le_bytes());
    header.push(neighbor.flags);
    header.push(0);
    let mut message = MsgBuilder::new(kind, flags, seq, portid);
    message.put_payload(&header);
    message.put_attr(NDA_DST, &ip_bytes(neighbor.address));
    if !neighbor.hardware.is_empty() {
        message.put_attr(NDA_LLADDR, &neighbor.hardware);
    }
    let mut cache = Vec::with_capacity(16);
    cache.extend_from_slice(&0u32.to_le_bytes());
    cache.extend_from_slice(&0u32.to_le_bytes());
    cache.extend_from_slice(&0u32.to_le_bytes());
    cache.extend_from_slice(&0u32.to_le_bytes());
    message.put_attr(NDA_CACHEINFO, &cache);
    message.put_u32(NDA_PROBES, neighbor.probes);
    message.finish()
}

fn neighbor_event(event: neigh::NeighborEvent, neighbor: &Neighbor) {
    let kind = match event {
        neigh::NeighborEvent::Delete => RTM_DELNEIGH,
        _ => RTM_NEWNEIGH,
    };
    let message = neighbor_message(neighbor, kind, 0, 0, 0);
    broadcast(NETLINK_ROUTE, 0, RTNLGRP_NEIGH, &message);
}

fn neighbor_get(request: &Request<'_>, dump: bool) -> Result<(), i32> {
    let family = family_of_message(request.payload.first().copied().unwrap_or(AF_UNSPEC))?;
    let filter_index = i32_at(request.payload, 4).unwrap_or(0) as u32;
    if dump || request.header.flags & super::msg::NLM_F_DUMP != 0 {
        let neighbors: Vec<Neighbor> = neigh::snapshot()
            .into_iter()
            .filter(|entry| family == AF_UNSPEC || entry.family == family)
            .filter(|entry| filter_index == 0 || entry.ifindex == filter_index)
            .collect();
        let seq = request.header.seq;
        let portid = request.socket.portid();
        let mut next = 0usize;
        request
            .dump_start(Box::new(move |ctx| {
                while next < neighbors.len() {
                    let message = neighbor_message(
                        &neighbors[next],
                        RTM_NEWNEIGH,
                        super::msg::NLM_F_MULTI,
                        seq,
                        portid,
                    );
                    if !ctx.try_put(&message) {
                        return Ok(true);
                    }
                    next += 1;
                }
                Ok(false)
            }))
            .map_err(|error| error)?;
        return Err(SKIP_ACK);
    }
    let destination = Attrs::find(request.payload.get(12..).unwrap_or_default(), NDA_DST)
        .and_then(|attr| parse_ip(family.max(2), attr.data))
        .ok_or(-(abi::EINVAL as i32))?;
    let entry = neigh::snapshot()
        .into_iter()
        .find(|entry| {
            entry.address == destination && (filter_index == 0 || entry.ifindex == filter_index)
        })
        .ok_or(-(abi::ENOENT as i32))?;
    let message = neighbor_message(
        &entry,
        RTM_NEWNEIGH,
        0,
        request.header.seq,
        request.socket.portid(),
    );
    request.reply(message).map_err(errno)?;
    Ok(())
}

fn neighbor_add(request: &Request<'_>) -> Result<(), i32> {
    need_privilege(request)?;
    let family = family_of_message(request.payload.first().copied().unwrap_or(2))?;
    if family == AF_UNSPEC {
        return Err(-(abi::EAFNOSUPPORT as i32));
    }
    let ifindex = u32_at(request.payload, 4).ok_or(-(abi::EINVAL as i32))?;
    let state = u16_at(request.payload, 8).unwrap_or(neigh::NUD_PERMANENT);
    let flags = request.payload.get(10).copied().unwrap_or(0);
    let attributes = request.payload.get(12..).unwrap_or_default();
    let destination = Attrs::find(attributes, NDA_DST)
        .and_then(|attr| parse_ip(family, attr.data))
        .ok_or(-(abi::EINVAL as i32))?;
    if let Some(hardware) = Attrs::find(attributes, NDA_LLADDR) {
        neigh::set_static(ifindex, family, destination, hardware.data, state, flags)
            .map_err(errno)?;
    } else {
        neigh::request(ifindex, family, destination).map_err(errno)?;
    }
    Ok(())
}

fn neighbor_delete(request: &Request<'_>) -> Result<(), i32> {
    need_privilege(request)?;
    let family = family_of_message(request.payload.first().copied().unwrap_or(2))?;
    let ifindex = u32_at(request.payload, 4).ok_or(-(abi::EINVAL as i32))?;
    let destination = Attrs::find(request.payload.get(12..).unwrap_or_default(), NDA_DST)
        .and_then(|attr| parse_ip(family.max(2), attr.data))
        .ok_or(-(abi::EINVAL as i32))?;
    neigh::delete(ifindex, family.max(2), destination).map_err(errno)?;
    Ok(())
}

// ---- Dispatch ----------------------------------------------------------------

fn input(request: &Request<'_>, _ext: &mut super::msg::ExtAck) -> Result<(), i32> {
    match request.header.kind {
        RTM_GETLINK => link_get(request, false),
        RTM_SETLINK => link_set(request),
        RTM_GETADDR => address_get(request, false),
        RTM_NEWADDR => address_add(request),
        RTM_DELADDR => address_delete(request),
        RTM_GETROUTE => route_get(request, false),
        RTM_NEWROUTE => route_add(request),
        RTM_DELROUTE => route_delete(request),
        RTM_GETNEIGH => neighbor_get(request, false),
        RTM_NEWNEIGH => neighbor_add(request),
        RTM_DELNEIGH => neighbor_delete(request),
        RTM_NEWLINK | RTM_DELLINK => Err(-(abi::EOPNOTSUPP as i32)),
        _ => Err(-(abi::EOPNOTSUPP as i32)),
    }
}

// ---- Notifications -----------------------------------------------------------

fn device_event(event: NetdevEvent, device: &Arc<NetDevice>) {
    let (kind, change) = match event {
        NetdevEvent::Register => (RTM_NEWLINK, 0xffff_ffff),
        NetdevEvent::Unregister => (RTM_DELLINK, 0xffff_ffff),
        NetdevEvent::Up | NetdevEvent::Down => (RTM_NEWLINK, dev::IFF_UP),
        NetdevEvent::ChangeAddr => (RTM_NEWLINK, dev::IFF_UP),
        NetdevEvent::ChangeMtu => (RTM_NEWLINK, 0),
        NetdevEvent::ChangeName => (RTM_NEWLINK, 0),
        NetdevEvent::ChangeFlags => (RTM_NEWLINK, dev::IFF_UP),
        NetdevEvent::Change => (RTM_NEWLINK, 0),
    };
    let message = link_message(device, kind, 0, 0, 0, change);
    broadcast(NETLINK_ROUTE, 0, RTNLGRP_LINK, &message);
    broadcast(NETLINK_ROUTE, 0, RTNLGRP_NOTIFY, &message);
    if event == NetdevEvent::Unregister {
        let mut gone = message;
        gone[4..6].copy_from_slice(&RTM_DELLINK.to_le_bytes());
        broadcast(NETLINK_ROUTE, 0, RTNLGRP_LINK, &gone);
    }
}
