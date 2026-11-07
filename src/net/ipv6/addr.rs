/*
 *
 *       src/net/ipv6/addr.rs
 *       IPv6 interface addresses, EUI-64 link-local generation and route setup
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::super::{
    dev::{self, ARPHRD_ETHER, ARPHRD_LOOPBACK, NetDevice},
    fib::{self, RTPROT_KERNEL},
    ip::Ipv6Addr,
    ipv4::addr::IFA_F_PERMANENT,
};
use crate::{sync::SpinLock, vfs::VfsError};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

/// One `RTM_NEWADDR` IPv6 record (`struct inet6_ifaddr`).
#[derive(Clone, Debug)]
pub struct IfAddr6 {
    pub ifindex: u32,
    pub address: Ipv6Addr,
    pub prefixlen: u8,
    pub scope: u8,
    pub flags: u32,
}

static ADDRS: SpinLock<Vec<IfAddr6>> = SpinLock::new(Vec::new());
static VERSION: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addr6Event {
    Add,
    Delete,
}

pub type Notifier = fn(Addr6Event, &IfAddr6);

static NOTIFIERS: SpinLock<Vec<Notifier>> = SpinLock::new(Vec::new());

pub fn register_notifier(notifier: Notifier) {
    NOTIFIERS.lock().push(notifier);
}

fn notify(event: Addr6Event, address: &IfAddr6) {
    for notifier in NOTIFIERS.lock().clone() {
        notifier(event, address);
    }
}

pub fn version() -> u32 {
    VERSION.load(Ordering::Acquire)
}

fn bump() {
    VERSION.fetch_add(1, Ordering::AcqRel);
}

pub fn scope_of(address: Ipv6Addr) -> u8 {
    if address.is_loopback() {
        fib::RT_SCOPE_HOST
    } else if is_link_local(address) {
        fib::RT_SCOPE_LINK
    } else {
        fib::RT_SCOPE_UNIVERSE
    }
}

pub fn is_link_local(address: Ipv6Addr) -> bool {
    let octets = address.octets();
    octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80
}

/// Solicited-node multicast address `ff02::1:ffXX:XXXX` (RFC 4291).
pub fn solicited_node(address: Ipv6Addr) -> Ipv6Addr {
    let octets = address.octets();
    Ipv6Addr::new(
        0xff02,
        0,
        0,
        0,
        0,
        1,
        0xff00 | u16::from(octets[13]),
        u16::from_be_bytes([octets[14], octets[15]]),
    )
}

/// All-nodes link-local multicast `ff02::1`.
pub const ALL_NODES_LINK_LOCAL: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);
/// All-routers link-local multicast `ff02::2`.
pub const ALL_ROUTERS_LINK_LOCAL: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 2);

/// Derive the modified EUI-64 link-local address `fe80::/64` from a 6-byte MAC.
pub fn eui64_link_local(mac: &[u8]) -> Option<Ipv6Addr> {
    if mac.len() != 6 {
        return None;
    }
    Some(Ipv6Addr::from([
        0xfe,
        0x80,
        0,
        0,
        0,
        0,
        0,
        0,
        mac[0] ^ 0x02,
        mac[1],
        mac[2],
        0xff,
        0xfe,
        mac[3],
        mac[4],
        mac[5],
    ]))
}

pub fn add(mut address: IfAddr6) -> Result<(), VfsError> {
    let _device = dev::by_index(address.ifindex).ok_or(VfsError::NoDevice)?;
    if address.flags == 0 {
        address.flags = IFA_F_PERMANENT;
    }
    if address.scope == 0 {
        address.scope = scope_of(address.address);
    }
    {
        let mut list = ADDRS.lock();
        if list
            .iter()
            .any(|entry| entry.ifindex == address.ifindex && entry.address == address.address)
        {
            return Err(VfsError::AlreadyExists);
        }
        list.push(address.clone());
    }
    fib::install_address_routes_v6(
        address.ifindex,
        address.address,
        address.prefixlen,
        RTPROT_KERNEL,
    )?;
    bump();
    notify(Addr6Event::Add, &address);
    Ok(())
}

pub fn delete(ifindex: u32, address: Ipv6Addr, prefixlen: Option<u8>) -> Result<(), VfsError> {
    let removed = {
        let mut list = ADDRS.lock();
        let position = list.iter().position(|entry| {
            entry.ifindex == ifindex
                && entry.address == address
                && prefixlen.is_none_or(|prefixlen| entry.prefixlen == prefixlen)
        });
        match position {
            Some(position) => list.remove(position),
            None => return Err(VfsError::NotFound),
        }
    };
    fib::remove_address_routes_v6(removed.ifindex, removed.address, removed.prefixlen);
    bump();
    notify(Addr6Event::Delete, &removed);
    Ok(())
}

pub fn flush_device(ifindex: u32) -> Vec<IfAddr6> {
    let removed = {
        let mut list = ADDRS.lock();
        let mut removed = Vec::new();
        let mut index = 0;
        while index < list.len() {
            if list[index].ifindex == ifindex {
                removed.push(list.swap_remove(index));
            } else {
                index += 1;
            }
        }
        removed
    };
    if !removed.is_empty() {
        bump();
        for address in &removed {
            fib::remove_address_routes_v6(address.ifindex, address.address, address.prefixlen);
            notify(Addr6Event::Delete, address);
        }
    }
    removed
}

pub fn snapshot() -> Vec<IfAddr6> {
    ADDRS.lock().clone()
}

pub fn for_device(ifindex: u32) -> Vec<IfAddr6> {
    ADDRS
        .lock()
        .iter()
        .filter(|entry| entry.ifindex == ifindex)
        .cloned()
        .collect()
}

pub fn get(ifindex: u32, address: Ipv6Addr) -> Option<IfAddr6> {
    ADDRS
        .lock()
        .iter()
        .find(|entry| entry.ifindex == ifindex && entry.address == address)
        .cloned()
}

pub fn is_local(address: Ipv6Addr) -> bool {
    ADDRS.lock().iter().any(|entry| entry.address == address)
}

pub fn is_local_multicast(ifindex: u32, multicast: Ipv6Addr) -> bool {
    if multicast == ALL_NODES_LINK_LOCAL {
        return true;
    }
    ADDRS
        .lock()
        .iter()
        .filter(|entry| ifindex == 0 || entry.ifindex == ifindex)
        .any(|entry| solicited_node(entry.address) == multicast)
}

pub fn select_source(ifindex: u32, destination: Ipv6Addr) -> Option<Ipv6Addr> {
    let list = ADDRS.lock();
    let want_link_local = is_link_local(destination) || destination.is_multicast();
    if let Some(found) = list
        .iter()
        .find(|entry| entry.ifindex == ifindex && is_link_local(entry.address) == want_link_local)
    {
        return Some(found.address);
    }
    list.iter()
        .find(|entry| entry.ifindex == ifindex)
        .map(|entry| entry.address)
}

pub fn first_on(ifindex: u32) -> Option<Ipv6Addr> {
    ADDRS
        .lock()
        .iter()
        .find(|entry| entry.ifindex == ifindex)
        .map(|entry| entry.address)
}

/// Install `::1/128` on loopback (`addrconf_loopback_init`).
pub fn init_loopback(ifindex: u32) -> Result<(), VfsError> {
    add(IfAddr6 {
        ifindex,
        address: Ipv6Addr::LOCALHOST,
        prefixlen: 128,
        scope: fib::RT_SCOPE_HOST,
        flags: IFA_F_PERMANENT,
    })
}

/// Ensure an Ethernet device has its EUI-64 link-local address configured.
pub fn ensure_link_local(dev: &Arc<NetDevice>) {
    if dev.dev_type != ARPHRD_ETHER {
        return;
    }
    let Some(lladdr) = eui64_link_local(&dev.hw_addr()) else {
        return;
    };
    if get(dev.ifindex(), lladdr).is_some() {
        return;
    }
    let _ = add(IfAddr6 {
        ifindex: dev.ifindex(),
        address: lladdr,
        prefixlen: 64,
        scope: fib::RT_SCOPE_LINK,
        flags: IFA_F_PERMANENT,
    });
}

pub fn is_loopback(ifindex: u32) -> bool {
    dev::by_index(ifindex).is_some_and(|device| device.dev_type == ARPHRD_LOOPBACK)
}
