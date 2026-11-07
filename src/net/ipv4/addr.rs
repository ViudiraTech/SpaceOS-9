/*
 *
 *       src/net/ipv4/addr.rs
 *       IPv4 interface addresses and the routes derived from them
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::super::{
    fib::{self, RTPROT_KERNEL},
    ip::Ipv4Addr,
};
use crate::{
    net::dev::{self, ARPHRD_LOOPBACK},
    sync::SpinLock,
    vfs::VfsError,
};
use alloc::{string::String, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

pub const IFA_F_SECONDARY: u32 = 0x01;
pub const IFA_F_NODAD: u32 = 0x02;
pub const IFA_F_PERMANENT: u32 = 0x80;

/// One `RTM_NEWADDR` record (`struct in_ifaddr`).
#[derive(Clone, Debug)]
pub struct IfAddr {
    pub ifindex: u32,
    /// The address assigned to the interface.
    pub address: Ipv4Addr,
    /// Peer address for point-to-point links, or the address itself.
    pub local: Ipv4Addr,
    pub prefixlen: u8,
    pub broadcast: Option<Ipv4Addr>,
    pub scope: u8,
    pub flags: u32,
    pub label: String,
}

static ADDRS: SpinLock<Vec<IfAddr>> = SpinLock::new(Vec::new());
static VERSION: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrEvent {
    Add,
    Delete,
}

pub type Notifier = fn(AddrEvent, &IfAddr);

static NOTIFIERS: SpinLock<Vec<Notifier>> = SpinLock::new(Vec::new());

pub fn register_notifier(notifier: Notifier) {
    NOTIFIERS.lock().push(notifier);
}

fn notify(event: AddrEvent, address: &IfAddr) {
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

/// `inet_insert_ifa`: install the address and the local/connected/broadcast
/// routes it implies.
pub fn add(mut address: IfAddr) -> Result<(), VfsError> {
    let device = dev::by_index(address.ifindex).ok_or(VfsError::NoDevice)?;
    if address.label.is_empty() {
        address.label = device.name();
    }
    if address.local == Ipv4Addr::UNSPECIFIED {
        address.local = address.address;
    }
    if address.flags == 0 {
        address.flags = IFA_F_PERMANENT;
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
    fib::install_address_routes(
        address.ifindex,
        address.address,
        address.prefixlen,
        (address.local != address.address).then_some(address.local),
        Some(&address.label),
        RTPROT_KERNEL,
    )?;
    bump();
    notify(AddrEvent::Add, &address);
    Ok(())
}

/// `inet_del_ifa`.
pub fn delete(ifindex: u32, address: Ipv4Addr, prefixlen: Option<u8>) -> Result<(), VfsError> {
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
    fib::remove_address_routes(removed.ifindex, removed.address, removed.prefixlen);
    bump();
    notify(AddrEvent::Delete, &removed);
    Ok(())
}

/// Remove every address of a device that is being unregistered.
pub fn flush_device(ifindex: u32) -> Vec<IfAddr> {
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
            fib::remove_address_routes(address.ifindex, address.address, address.prefixlen);
            notify(AddrEvent::Delete, address);
        }
    }
    removed
}

pub fn snapshot() -> Vec<IfAddr> {
    ADDRS.lock().clone()
}

pub fn for_device(ifindex: u32) -> Vec<IfAddr> {
    ADDRS
        .lock()
        .iter()
        .filter(|entry| entry.ifindex == ifindex)
        .cloned()
        .collect()
}

pub fn get(ifindex: u32, address: Ipv4Addr) -> Option<IfAddr> {
    ADDRS
        .lock()
        .iter()
        .find(|entry| entry.ifindex == ifindex && entry.address == address)
        .cloned()
}

/// Whether `address` is configured on any interface (`ipv4_chk_adv_local_v4`).
pub fn is_local(address: Ipv4Addr) -> bool {
    ADDRS.lock().iter().any(|entry| entry.address == address)
}

/// First address of a device, used for source selection when a route carries
/// no preferred source.
pub fn first_on(ifindex: u32) -> Option<Ipv4Addr> {
    ADDRS
        .lock()
        .iter()
        .find(|entry| entry.ifindex == ifindex)
        .map(|entry| entry.address)
}

pub fn primary_on(ifindex: u32) -> Option<IfAddr> {
    ADDRS
        .lock()
        .iter()
        .find(|entry| entry.ifindex == ifindex)
        .cloned()
}

/// Install `127.0.0.1/8` on loopback, matching `ipv4_loopback_init`.
pub fn init_loopback(ifindex: u32) -> Result<(), VfsError> {
    add(IfAddr {
        ifindex,
        address: Ipv4Addr::new(127, 0, 0, 1),
        local: Ipv4Addr::new(127, 0, 0, 1),
        prefixlen: 8,
        broadcast: None,
        scope: fib::RT_SCOPE_HOST,
        flags: IFA_F_PERMANENT,
        label: String::new(),
    })?;
    // The whole 127/8 is host-local, as Linux's loopback route states.
    let mut route = fib::Route::new(crate::net::ip::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0)), 8);
    route.table = fib::RT_TABLE_LOCAL;
    route.ifindex = ifindex;
    route.scope = fib::RT_SCOPE_HOST;
    route.kind = fib::RTN_LOCAL;
    route.protocol = RTPROT_KERNEL;
    fib::add(route)?;
    Ok(())
}

/// Whether the device is loopback, which needs no neighbour resolution.
pub fn is_loopback(ifindex: u32) -> bool {
    dev::by_index(ifindex).is_some_and(|device| device.dev_type == ARPHRD_LOOPBACK)
}
