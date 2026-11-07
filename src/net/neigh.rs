/*
 *
 *       src/net/neigh.rs
 *       Neighbour table: resolution, ageing, probing and a per-entry queue
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mirrors `net/core/neighbour.c`. The table is shared by ARP (IPv4) and
//! NDISC (IPv6); each family registers a `solicit` hook that emits its own
//! request packet. The state machine is the Linux NUD one:
//!
//! ```text
//! INCOMPLETE --reply--> REACHABLE --30s--> STALE --use--> DELAY --5s--> PROBE
//!     |                    ^                                          |
//!     +--3 probes--------->|                                    3 probes
//!                          +------------------------------------------+
//!                          v
//!                       FAILED
//! ```

use super::{
    dev::{self, ARPHRD_ETHER, ARPHRD_LOOPBACK, MAX_ADDR_LEN, NetDevice},
    ether::{self, ETH_ALEN},
    ip::{IpAddr, Ipv4Addr, v4_multicast_mac, v6_multicast_mac},
    skb::SkBuff,
};
use crate::{net::timer, sync::SpinLock, vfs::VfsError};
use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const NUD_INCOMPLETE: u16 = 0x01;
pub const NUD_REACHABLE: u16 = 0x02;
pub const NUD_STALE: u16 = 0x04;
pub const NUD_DELAY: u16 = 0x08;
pub const NUD_PROBE: u16 = 0x10;
pub const NUD_FAILED: u16 = 0x20;
pub const NUD_NOARP: u16 = 0x40;
pub const NUD_PERMANENT: u16 = 0x80;

pub const NTF_USE: u8 = 0x01;
pub const NTF_SELF: u8 = 0x02;
pub const NTF_PROXY: u8 = 0x08;
pub const NTF_ROUTER: u8 = 0x80;

/// `NEIGH_VAR_BASE_REACHABLE_TIME` default.
pub const BASE_REACHABLE_US: u64 = 30_000_000;
/// `NEIGH_VAR_DELAY_PROBE_TIME`.
pub const DELAY_PROBE_US: u64 = 5_000_000;
/// Retransmit interval while INCOMPLETE or PROBE.
pub const RETRANS_US: u64 = 1_000_000;
/// `NEIGH_VAR_GC_STALETIME`.
pub const GC_STALE_US: u64 = 60_000_000;
/// `NEIGH_VAR_UCAST_PROBES` / `APP_PROBES`.
pub const UCAST_PROBES: u32 = 3;
pub const APP_PROBES: u32 = 3;
/// `NEIGH_VAR_UNRES_QLEAN`: frames held while a solicitation is in flight.
pub const UNRES_QUEUE: usize = 3;

pub const AF_INET: u8 = 2;
pub const AF_INET6: u8 = 10;

/// One cache entry. `queue` holds packets waiting for resolution.
pub struct Neighbor {
    pub ifindex: u32,
    pub family: u8,
    pub address: IpAddr,
    pub hardware: Vec<u8>,
    pub state: u16,
    pub flags: u8,
    /// Last state transition (`neigh->updated`).
    pub updated_us: u64,
    /// Last successful use.
    pub used_us: u64,
    pub probes: u32,
    pub queue: Vec<SkBuff>,
}

impl Clone for Neighbor {
    fn clone(&self) -> Self {
        Self {
            ifindex: self.ifindex,
            family: self.family,
            address: self.address,
            hardware: self.hardware.clone(),
            state: self.state,
            flags: self.flags,
            updated_us: self.updated_us,
            used_us: self.used_us,
            probes: self.probes,
            queue: Vec::new(),
        }
    }
}

impl Neighbor {
    pub fn is_usable(&self) -> bool {
        self.state & (NUD_REACHABLE | NUD_STALE | NUD_DELAY | NUD_PROBE | NUD_PERMANENT | NUD_NOARP)
            != 0
            && !self.hardware.is_empty()
    }
}

static TABLE: SpinLock<Vec<Neighbor>> = SpinLock::new(Vec::new());
static SOLICIT: SpinLock<BTreeMap<u8, fn(&Arc<NetDevice>, IpAddr)>> =
    SpinLock::new(BTreeMap::new());
static VERSION: AtomicU32 = AtomicU32::new(0);
static TIMER_ARMED: AtomicBool = AtomicBool::new(false);

/// `NEIGH_EVENT_*`: what netlink subscribers see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeighborEvent {
    Add,
    Update,
    Delete,
}

pub type Notifier = fn(NeighborEvent, &Neighbor);

static NOTIFIERS: SpinLock<Vec<Notifier>> = SpinLock::new(Vec::new());

pub fn register_notifier(notifier: Notifier) {
    NOTIFIERS.lock().push(notifier);
}

fn notify(event: NeighborEvent, neighbor: &Neighbor) {
    for notifier in NOTIFIERS.lock().clone() {
        notifier(event, neighbor);
    }
}

pub fn version() -> u32 {
    VERSION.load(Ordering::Acquire)
}

fn bump() {
    VERSION.fetch_add(1, Ordering::AcqRel);
}

/// Register the solicitation hook for an address family.
pub fn register_family(family: u8, solicit: fn(&Arc<NetDevice>, IpAddr)) {
    SOLICIT.lock().insert(family, solicit);
}

pub fn snapshot() -> Vec<Neighbor> {
    TABLE.lock().clone()
}

pub fn for_device(ifindex: u32) -> Vec<Neighbor> {
    TABLE
        .lock()
        .iter()
        .filter(|entry| entry.ifindex == ifindex)
        .cloned()
        .collect()
}

pub fn lookup(ifindex: u32, address: IpAddr) -> Option<Neighbor> {
    TABLE
        .lock()
        .iter()
        .find(|entry| entry.ifindex == ifindex && entry.address == address)
        .cloned()
}

pub fn count() -> usize {
    TABLE.lock().len()
}

fn ethertype(family: u8) -> u16 {
    if family == AF_INET6 {
        ether::ETH_P_IPV6
    } else {
        ether::ETH_P_IP
    }
}

fn group_hardware(destination: IpAddr) -> Option<[u8; ETH_ALEN]> {
    match destination {
        IpAddr::V4(address) => {
            if address.is_multicast() {
                return Some(v4_multicast_mac(address));
            }
            if address.is_broadcast() {
                return Some(ether::BROADCAST);
            }
            None
        }
        IpAddr::V6(address) => address.is_multicast().then(|| v6_multicast_mac(address)),
    }
}

fn is_subnet_broadcast(ifindex: u32, destination: Ipv4Addr) -> bool {
    super::ipv4::addr::for_device(ifindex)
        .iter()
        .any(|entry| entry.broadcast == Some(destination))
}

fn destination_hardware(ifindex: u32, destination: IpAddr) -> Option<[u8; ETH_ALEN]> {
    if let Some(hardware) = group_hardware(destination) {
        return Some(hardware);
    }
    if let IpAddr::V4(address) = destination
        && is_subnet_broadcast(ifindex, address)
    {
        return Some(ether::BROADCAST);
    }
    None
}

/// Frame a network packet and hand it to the device once a hardware address is
/// known (`neigh_resolve_output` / `dev_hard_header`).
fn transmit(dev: &Arc<NetDevice>, mut skb: SkBuff, hardware: &[u8], family: u8) {
    match dev.dev_type {
        ARPHRD_LOOPBACK => {
            skb.protocol = ethertype(family);
            let _ = dev::queue_xmit(dev, skb);
        }
        ARPHRD_ETHER => {
            if hardware.len() != ETH_ALEN {
                return;
            }
            let mut destination = [0u8; ETH_ALEN];
            destination.copy_from_slice(hardware);
            let source = dev.hw_addr();
            if source.len() != ETH_ALEN {
                return;
            }
            let mut source_address = [0u8; ETH_ALEN];
            source_address.copy_from_slice(&source);
            if !ether::push_header(&mut skb, &destination, &source_address, ethertype(family)) {
                return;
            }
            let _ = dev::queue_xmit(dev, skb);
        }
        _ => {}
    }
}

/// `neigh_output`: resolve `next_hop` (the gateway when routing through one)
/// and transmit. A group destination needs no resolution.
pub fn output(
    dev: &Arc<NetDevice>,
    skb: SkBuff,
    family: u8,
    destination: IpAddr,
    next_hop: IpAddr,
) -> Result<(), VfsError> {
    if !dev.is_up() {
        return Err(VfsError::NoDevice);
    }
    if dev.dev_type == ARPHRD_LOOPBACK || dev.dev_type == dev::ARPHRD_NONE {
        transmit(dev, skb, &[], family);
        return Ok(());
    }
    if let Some(hardware) = destination_hardware(dev.ifindex(), destination) {
        transmit(dev, skb, &hardware, family);
        return Ok(());
    }
    let now = crate::time::uptime_micros();
    {
        let mut table = TABLE.lock();
        let ifindex = dev.ifindex();
        if let Some(entry) = table
            .iter_mut()
            .find(|entry| entry.ifindex == ifindex && entry.address == next_hop)
        {
            entry.used_us = now;
            if entry.is_usable() {
                let hardware = entry.hardware.clone();
                drop(table);
                transmit(dev, skb, &hardware, family);
                return Ok(());
            }
            if entry.state == NUD_STALE {
                entry.state = NUD_DELAY;
                entry.updated_us = now;
                arm_timer();
            }
            if entry.queue.len() < UNRES_QUEUE {
                entry.queue.push(skb);
            } else {
                dev.stats.tx_dropped.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        } else {
            table.push(Neighbor {
                ifindex,
                family,
                address: next_hop,
                hardware: Vec::new(),
                state: NUD_INCOMPLETE,
                flags: 0,
                updated_us: now,
                used_us: now,
                probes: 1,
                queue: vec![skb],
            });
            bump();
        }
    }
    solicit(dev, next_hop);
    arm_timer();
    Ok(())
}

fn solicit(dev: &Arc<NetDevice>, target: IpAddr) {
    let hook = SOLICIT.lock().get(&family_of(target)).copied();
    if let Some(hook) = hook {
        hook(dev, target);
    }
}

pub fn family_of(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(_) => AF_INET,
        IpAddr::V6(_) => AF_INET6,
    }
}

/// `neigh_update`: a solicitation was answered (or an entry was configured).
pub fn update(
    dev: &Arc<NetDevice>,
    family: u8,
    address: IpAddr,
    hardware: &[u8],
    state: u16,
) -> bool {
    if hardware.len() > MAX_ADDR_LEN || hardware.len() != dev.addr_len as usize {
        return false;
    }
    let now = crate::time::uptime_micros();
    let flushed: Vec<SkBuff>;
    let created;
    {
        let mut table = TABLE.lock();
        if let Some(entry) = table
            .iter_mut()
            .find(|entry| entry.ifindex == dev.ifindex() && entry.address == address)
        {
            entry.hardware.clear();
            entry.hardware.extend_from_slice(hardware);
            entry.state = state;
            entry.updated_us = now;
            entry.used_us = now;
            entry.probes = 0;
            entry.family = family;
            flushed = core::mem::take(&mut entry.queue);
            created = false;
        } else {
            table.push(Neighbor {
                ifindex: dev.ifindex(),
                family,
                address,
                hardware: hardware.to_vec(),
                state,
                flags: 0,
                updated_us: now,
                used_us: now,
                probes: 0,
                queue: Vec::new(),
            });
            flushed = Vec::new();
            created = true;
        }
        bump();
    }
    notify(
        if created {
            NeighborEvent::Add
        } else {
            NeighborEvent::Update
        },
        &Neighbor {
            ifindex: dev.ifindex(),
            family,
            address,
            hardware: hardware.to_vec(),
            state,
            flags: 0,
            updated_us: now,
            used_us: now,
            probes: 0,
            queue: Vec::new(),
        },
    );
    for skb in flushed {
        transmit(dev, skb, hardware, family);
    }
    created
}

/// Install a static entry (`RTM_NEWNEIGH` without `NTF_USE`).
pub fn set_static(
    ifindex: u32,
    family: u8,
    address: IpAddr,
    hardware: &[u8],
    state: u16,
    flags: u8,
) -> Result<(), VfsError> {
    let dev = dev::by_index(ifindex).ok_or(VfsError::NoDevice)?;
    if hardware.len() != dev.addr_len as usize {
        return Err(VfsError::InvalidArgument);
    }
    update(&dev, family, address, hardware, state | NUD_PERMANENT);
    let mut table = TABLE.lock();
    if let Some(entry) = table
        .iter_mut()
        .find(|entry| entry.ifindex == ifindex && entry.address == address)
    {
        entry.flags = flags;
    }
    Ok(())
}

/// Start resolution for an address (`RTM_NEWNEIGH` with `NTF_USE` or an
/// explicit request). Creates an INCOMPLETE entry when none exists.
pub fn request(ifindex: u32, family: u8, address: IpAddr) -> Result<(), VfsError> {
    let device = dev::by_index(ifindex).ok_or(VfsError::NoDevice)?;
    if device.dev_type == ARPHRD_LOOPBACK {
        return Ok(());
    }
    let now = crate::time::uptime_micros();
    {
        let mut table = TABLE.lock();
        if let Some(entry) = table
            .iter_mut()
            .find(|entry| entry.ifindex == ifindex && entry.address == address)
        {
            entry.state = NUD_INCOMPLETE;
            entry.probes = 1;
            entry.updated_us = now;
            entry.family = family;
        } else {
            table.push(Neighbor {
                ifindex,
                family,
                address,
                hardware: Vec::new(),
                state: NUD_INCOMPLETE,
                flags: 0,
                updated_us: now,
                used_us: now,
                probes: 1,
                queue: Vec::new(),
            });
        }
        bump();
    }
    solicit(&device, address);
    arm_timer();
    Ok(())
}

pub fn delete(ifindex: u32, family: u8, address: IpAddr) -> Result<(), VfsError> {
    let removed = {
        let mut table = TABLE.lock();
        let position = table.iter().position(|entry| {
            entry.ifindex == ifindex && entry.family == family && entry.address == address
        });
        match position {
            Some(position) => table.remove(position),
            None => return Err(VfsError::NotFound),
        }
    };
    bump();
    notify(NeighborEvent::Delete, &removed);
    Ok(())
}

pub fn flush_device(ifindex: u32) -> usize {
    let removed = {
        let mut table = TABLE.lock();
        let before = table.len();
        table.retain(|entry| entry.ifindex != ifindex);
        before - table.len()
    };
    if removed != 0 {
        bump();
    }
    removed
}

/// `pneigh`-style proxy lookup is not provided; a neighbour that fails
/// resolution is removed once its queue is drained.

fn arm_timer() {
    if !TIMER_ARMED.swap(true, Ordering::AcqRel) {
        timer::schedule_in(RETRANS_US, alloc::boxed::Box::new(tick));
    }
}

fn tick() {
    TIMER_ARMED.store(false, Ordering::Release);
    let now = crate::time::uptime_micros();
    let mut solicitations: Vec<(u32, IpAddr)> = Vec::new();
    let mut events: Vec<(NeighborEvent, Neighbor)> = Vec::new();
    let mut failed_queues: Vec<SkBuff> = Vec::new();
    {
        let mut table = TABLE.lock();
        let mut index = 0;
        while index < table.len() {
            let entry = &mut table[index];
            match entry.state {
                NUD_INCOMPLETE => {
                    if entry.probes >= UCAST_PROBES {
                        entry.state = NUD_FAILED;
                        failed_queues.extend(entry.queue.drain(..));
                        events.push((NeighborEvent::Update, entry.clone()));
                        index += 1;
                    } else {
                        entry.probes += 1;
                        entry.updated_us = now;
                        solicitations.push((entry.ifindex, entry.address));
                        index += 1;
                    }
                }
                NUD_DELAY => {
                    if now.saturating_sub(entry.updated_us) >= DELAY_PROBE_US {
                        entry.state = NUD_PROBE;
                        entry.probes = 1;
                        entry.updated_us = now;
                        solicitations.push((entry.ifindex, entry.address));
                    }
                    index += 1;
                }
                NUD_PROBE => {
                    if entry.probes >= UCAST_PROBES {
                        entry.state = NUD_FAILED;
                        entry.hardware.clear();
                        failed_queues.extend(entry.queue.drain(..));
                        events.push((NeighborEvent::Update, entry.clone()));
                    } else {
                        entry.probes += 1;
                        entry.updated_us = now;
                        solicitations.push((entry.ifindex, entry.address));
                    }
                    index += 1;
                }
                NUD_REACHABLE if now.saturating_sub(entry.updated_us) >= BASE_REACHABLE_US => {
                    entry.state = NUD_STALE;
                    events.push((NeighborEvent::Update, entry.clone()));
                    index += 1;
                }
                NUD_STALE if now.saturating_sub(entry.used_us) >= GC_STALE_US => {
                    events.push((NeighborEvent::Delete, entry.clone()));
                    table.remove(index);
                }
                NUD_FAILED if now.saturating_sub(entry.updated_us) >= GC_STALE_US => {
                    events.push((NeighborEvent::Delete, entry.clone()));
                    table.remove(index);
                }
                _ => index += 1,
            }
        }
    }
    for (ifindex, address) in solicitations {
        if let Some(device) = dev::by_index(ifindex) {
            solicit(&device, address);
        }
    }
    drop(failed_queues);
    for (event, neighbor) in events {
        notify(event, &neighbor);
    }
    if !TABLE.lock().is_empty() {
        arm_timer();
    }
}

/// Number of unsent frames currently queued on failed entries (leak check).
pub fn queued_frames() -> usize {
    TABLE.lock().iter().map(|entry| entry.queue.len()).sum()
}
