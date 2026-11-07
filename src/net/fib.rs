/*
 *
 *       src/net/fib.rs
 *       Forwarding information base: multi-table longest-prefix matching
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Modelled on `net/ipv4/fib_frontend.c` and `fib_trie.c`, with a compact
//! representation: routes are keyed by `(table, family, network, prefixlen)`
//! and looked up by masking the destination once per prefix length. Thirty-
//! three `BTreeMap` probes replace the trie; the table stays small in this
//! kernel and lookups remain O(33 log n) with no allocation.

use super::ip::{IpAddr, Ipv4Addr, Ipv6Addr, mask_v4, mask_v6};
use crate::vfs::{VfsError, abi};
use alloc::{collections::BTreeMap, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

pub const RT_TABLE_UNSPEC: u32 = 0;
pub const RT_TABLE_DEFAULT: u32 = 253;
pub const RT_TABLE_MAIN: u32 = 254;
pub const RT_TABLE_LOCAL: u32 = 255;

pub const RT_SCOPE_UNIVERSE: u8 = 0;
pub const RT_SCOPE_SITE: u8 = 200;
pub const RT_SCOPE_LINK: u8 = 253;
pub const RT_SCOPE_HOST: u8 = 254;
pub const RT_SCOPE_NOWHERE: u8 = 255;

pub const RTN_UNSPEC: u8 = 0;
pub const RTN_UNICAST: u8 = 1;
pub const RTN_LOCAL: u8 = 2;
pub const RTN_BROADCAST: u8 = 3;
pub const RTN_ANYCAST: u8 = 4;
pub const RTN_MULTICAST: u8 = 5;
pub const RTN_BLACKHOLE: u8 = 6;
pub const RTN_UNREACHABLE: u8 = 7;
pub const RTN_PROHIBIT: u8 = 8;
pub const RTN_THROW: u8 = 9;
pub const RTN_NAT: u8 = 10;

pub const RTPROT_UNSPEC: u8 = 0;
pub const RTPROT_REDIRECT: u8 = 1;
pub const RTPROT_KERNEL: u8 = 2;
pub const RTPROT_BOOT: u8 = 3;
pub const RTPROT_STATIC: u8 = 4;
pub const RTPROT_DHCP: u8 = 16;

pub const RTF_UP: u32 = 0x0001;
pub const RTF_GATEWAY: u32 = 0x0002;
pub const RTF_HOST: u32 = 0x0004;

/// One routing entry, the information `rtmsg` and `RTA_*` expose.
#[derive(Clone, Debug)]
pub struct Route {
    pub table: u32,
    pub family: u8,
    /// Masked destination; the prefix length carries the remaining bits.
    pub destination: IpAddr,
    pub prefixlen: u8,
    pub gateway: Option<IpAddr>,
    pub ifindex: u32,
    /// Preferred source address (`RTA_PREFSRC`).
    pub source: Option<IpAddr>,
    pub protocol: u8,
    pub scope: u8,
    pub kind: u8,
    pub metric: u32,
    pub flags: u32,
}

impl Route {
    pub fn family_of(address: IpAddr) -> u8 {
        match address {
            IpAddr::V4(_) => 2,
            IpAddr::V6(_) => 10,
        }
    }

    pub fn new(destination: IpAddr, prefixlen: u8) -> Self {
        Self {
            table: RT_TABLE_MAIN,
            family: Self::family_of(destination),
            destination,
            prefixlen,
            gateway: None,
            ifindex: 0,
            source: None,
            protocol: RTPROT_BOOT,
            scope: RT_SCOPE_UNIVERSE,
            kind: RTN_UNICAST,
            metric: 0,
            flags: 0,
        }
    }

    pub fn is_gateway(&self) -> bool {
        self.gateway.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    table: u32,
    family: u8,
    prefixlen: u8,
    destination: IpAddr,
}

impl Key {
    fn of(route: &Route) -> Self {
        Self {
            table: route.table,
            family: route.family,
            prefixlen: route.prefixlen,
            destination: mask(route.destination, route.prefixlen),
        }
    }
}

fn mask(address: IpAddr, prefixlen: u8) -> IpAddr {
    match address {
        IpAddr::V4(address) => IpAddr::V4(mask_v4(address, prefixlen)),
        IpAddr::V6(address) => IpAddr::V6(mask_v6(address, prefixlen)),
    }
}

fn max_prefix(family: u8) -> u8 {
    if family == 2 { 32 } else { 128 }
}

static ROUTES: crate::sync::SpinLock<BTreeMap<Key, Vec<Route>>> =
    crate::sync::SpinLock::new(BTreeMap::new());
static VERSION: AtomicU32 = AtomicU32::new(0);

/// Monotonic change counter, used to invalidate cached lookups and to tag
/// dumps interrupted by concurrent changes.
pub fn version() -> u32 {
    VERSION.load(Ordering::Acquire)
}

fn bump() {
    VERSION.fetch_add(1, Ordering::AcqRel);
}

/// `fib_table_insert`: replace an entry with the same metric, gateway, output
/// device and protocol, otherwise append.
pub fn add(mut route: Route) -> Result<(), VfsError> {
    if route.prefixlen > max_prefix(route.family) {
        return Err(VfsError::InvalidArgument);
    }
    route.destination = mask(route.destination, route.prefixlen);
    if route.table == RT_TABLE_UNSPEC {
        route.table = RT_TABLE_MAIN;
    }
    if route.kind == RTN_UNSPEC {
        route.kind = RTN_UNICAST;
    }
    let key = Key::of(&route);
    let mut table = ROUTES.lock();
    let entries = table.entry(key).or_default();
    if let Some(existing) = entries.iter_mut().find(|existing| {
        existing.metric == route.metric
            && existing.gateway == route.gateway
            && existing.ifindex == route.ifindex
            && existing.protocol == route.protocol
    }) {
        *existing = route;
    } else {
        entries.push(route);
        entries.sort_by_key(|entry| entry.metric);
    }
    drop(table);
    bump();
    Ok(())
}

/// Filter used by `RTM_DELROUTE`; `None` members match anything.
#[derive(Clone, Copy, Default, Debug)]
pub struct DeleteFilter {
    pub table: Option<u32>,
    pub family: Option<u8>,
    pub destination: Option<IpAddr>,
    pub prefixlen: Option<u8>,
    pub gateway: Option<IpAddr>,
    pub ifindex: Option<u32>,
    pub kind: Option<u8>,
}

/// `fib_table_delete`. Returns whether anything matched.
pub fn delete(filter: &DeleteFilter) -> bool {
    let mut table = ROUTES.lock();
    let mut removed = false;
    let keys: Vec<Key> = table.keys().copied().collect();
    for key in keys {
        if let Some(root_table) = filter.table
            && key.table != root_table
        {
            continue;
        }
        if let Some(family) = filter.family
            && key.family != family
        {
            continue;
        }
        if let Some(destination) = filter.destination
            && key.destination != destination
        {
            continue;
        }
        if let Some(prefixlen) = filter.prefixlen
            && key.prefixlen != prefixlen
        {
            continue;
        }
        let Some(entries) = table.get_mut(&key) else {
            continue;
        };
        let before = entries.len();
        entries.retain(|entry| {
            if let Some(gateway) = filter.gateway
                && entry.gateway != Some(gateway)
            {
                return true;
            }
            if let Some(ifindex) = filter.ifindex
                && entry.ifindex != ifindex
            {
                return true;
            }
            if let Some(kind) = filter.kind
                && entry.kind != kind
            {
                return true;
            }
            false
        });
        removed |= entries.len() != before;
        if entries.is_empty() {
            table.remove(&key);
        }
    }
    drop(table);
    if removed {
        bump();
    }
    removed
}

/// Remove every route that leaves through `ifindex`.
pub fn flush_device(ifindex: u32) -> bool {
    delete(&DeleteFilter {
        ifindex: Some(ifindex),
        ..DeleteFilter::default()
    })
}

/// Longest-prefix match inside one table. `THROW` entries stop the search and
/// let the caller consult the next table, like Linux.
pub fn lookup_table(destination: IpAddr, table: u32) -> Option<Route> {
    let family = Route::family_of(destination);
    let routes = ROUTES.lock();
    for prefixlen in (0..=max_prefix(family)).rev() {
        let key = Key {
            table,
            family,
            prefixlen,
            destination: mask(destination, prefixlen),
        };
        let Some(entries) = routes.get(&key) else {
            continue;
        };
        let Some(best) = entries.first() else {
            continue;
        };
        if best.kind == RTN_THROW {
            return None;
        }
        return Some(best.clone());
    }
    None
}

/// Default policy: the local table answers for host addresses and broadcasts,
/// then the main table is tried. An unreachable route is returned to the
/// caller so the error it describes can be reported.
pub fn lookup(destination: IpAddr) -> Option<Route> {
    if let Some(route) = lookup_table(destination, RT_TABLE_LOCAL) {
        return Some(route);
    }
    lookup_table(destination, RT_TABLE_MAIN)
}

/// Every route, ordered the way `RTM_GETROUTE` dumps present them.
pub fn snapshot() -> Vec<Route> {
    let routes = ROUTES.lock();
    let mut all: Vec<Route> = routes
        .iter()
        .flat_map(|(_, entries)| entries.iter().cloned())
        .collect();
    all.sort_by(|left, right| {
        (
            left.table,
            left.family,
            left.prefixlen,
            left.destination,
            left.metric,
        )
            .cmp(&(
                right.table,
                right.family,
                right.prefixlen,
                right.destination,
                right.metric,
            ))
    });
    all
}

pub fn count() -> usize {
    ROUTES.lock().values().map(Vec::len).sum()
}

/// Convert a route lookup failure to the errno the socket layer reports.
pub fn lookup_error(route: Option<Route>) -> Result<Route, i32> {
    match route {
        Some(route) if route.kind == RTN_UNREACHABLE => Err(-(abi::ENETUNREACH as i32)),
        Some(route) if route.kind == RTN_BLACKHOLE || route.kind == RTN_PROHIBIT => {
            Err(-(abi::EACCES as i32))
        }
        Some(route) => Ok(route),
        None => Err(-(abi::ENETUNREACH as i32)),
    }
}

/// Convenience used by address configuration: install the local, connected and
/// broadcast routes Linux derives from an interface address.
pub fn install_address_routes(
    ifindex: u32,
    address: Ipv4Addr,
    prefixlen: u8,
    peer: Option<Ipv4Addr>,
    label: Option<&str>,
    protocol: u8,
) -> Result<(), VfsError> {
    let _ = label;
    let mut local = Route::new(IpAddr::V4(address), 32);
    local.table = RT_TABLE_LOCAL;
    local.ifindex = ifindex;
    local.scope = RT_SCOPE_HOST;
    local.kind = RTN_LOCAL;
    local.protocol = protocol;
    add(local)?;

    if let Some(peer) = peer {
        let mut host = Route::new(IpAddr::V4(peer), 32);
        host.ifindex = ifindex;
        host.scope = RT_SCOPE_LINK;
        host.source = Some(IpAddr::V4(address));
        host.protocol = protocol;
        add(host)?;
        return Ok(());
    }

    let network = mask_v4(address, prefixlen);
    let mut connected = Route::new(IpAddr::V4(network), prefixlen);
    connected.ifindex = ifindex;
    connected.scope = RT_SCOPE_LINK;
    connected.source = Some(IpAddr::V4(address));
    connected.protocol = protocol;
    add(connected)?;

    if prefixlen < 31 {
        let broadcast = broadcast_v4(address, prefixlen);
        let mut entry = Route::new(IpAddr::V4(broadcast), 32);
        entry.table = RT_TABLE_LOCAL;
        entry.ifindex = ifindex;
        entry.scope = RT_SCOPE_LINK;
        entry.kind = RTN_BROADCAST;
        entry.protocol = protocol;
        add(entry)?;
    }
    Ok(())
}

/// Remove the routes `install_address_routes` created for one address.
pub fn remove_address_routes(ifindex: u32, address: Ipv4Addr, prefixlen: u8) {
    delete(&DeleteFilter {
        table: Some(RT_TABLE_LOCAL),
        family: Some(2),
        destination: Some(IpAddr::V4(address)),
        prefixlen: Some(32),
        ..DeleteFilter::default()
    });
    let network = mask_v4(address, prefixlen);
    delete(&DeleteFilter {
        table: Some(RT_TABLE_MAIN),
        family: Some(2),
        destination: Some(IpAddr::V4(network)),
        prefixlen: Some(prefixlen),
        ifindex: Some(ifindex),
        ..DeleteFilter::default()
    });
    if prefixlen < 31 {
        delete(&DeleteFilter {
            table: Some(RT_TABLE_LOCAL),
            family: Some(2),
            destination: Some(IpAddr::V4(broadcast_v4(address, prefixlen))),
            prefixlen: Some(32),
            ifindex: Some(ifindex),
            ..DeleteFilter::default()
        });
    }
}

pub fn broadcast_v4(address: Ipv4Addr, prefixlen: u8) -> Ipv4Addr {
    if prefixlen >= 31 {
        return address;
    }
    let mask = u32::MAX << (32 - prefixlen as u32);
    Ipv4Addr::from(u32::from(address) | !mask)
}

/// Install the local `/128` and connected prefix routes for an IPv6 address.
pub fn install_address_routes_v6(
    ifindex: u32,
    address: Ipv6Addr,
    prefixlen: u8,
    protocol: u8,
) -> Result<(), VfsError> {
    let mut local = Route::new(IpAddr::V6(address), 128);
    local.table = RT_TABLE_LOCAL;
    local.ifindex = ifindex;
    local.scope = RT_SCOPE_HOST;
    local.kind = RTN_LOCAL;
    local.protocol = protocol;
    add(local)?;

    if prefixlen < 128 {
        let network = mask_v6(address, prefixlen);
        let mut connected = Route::new(IpAddr::V6(network), prefixlen);
        connected.ifindex = ifindex;
        connected.scope = RT_SCOPE_LINK;
        connected.source = Some(IpAddr::V6(address));
        connected.protocol = protocol;
        add(connected)?;
    }
    Ok(())
}

/// Remove the routes `install_address_routes_v6` created for one IPv6 address.
pub fn remove_address_routes_v6(ifindex: u32, address: Ipv6Addr, prefixlen: u8) {
    delete(&DeleteFilter {
        table: Some(RT_TABLE_LOCAL),
        family: Some(10),
        destination: Some(IpAddr::V6(address)),
        prefixlen: Some(128),
        ifindex: Some(ifindex),
        ..DeleteFilter::default()
    });
    if prefixlen < 128 {
        let network = mask_v6(address, prefixlen);
        delete(&DeleteFilter {
            table: Some(RT_TABLE_MAIN),
            family: Some(10),
            destination: Some(IpAddr::V6(network)),
            prefixlen: Some(prefixlen),
            ifindex: Some(ifindex),
            ..DeleteFilter::default()
        });
    }
}

/// `fib_sync_down_dev` equivalent for device teardown.
pub fn device_down(ifindex: u32) {
    let routes = ROUTES.lock();
    let stale: Vec<Key> = routes
        .iter()
        .filter(|(_, entries)| entries.iter().any(|entry| entry.ifindex == ifindex))
        .map(|(key, _)| *key)
        .collect();
    drop(routes);
    for key in stale {
        let mut routes = ROUTES.lock();
        if let Some(entries) = routes.get_mut(&key) {
            entries.retain(|entry| entry.ifindex != ifindex);
            if entries.is_empty() {
                routes.remove(&key);
            }
        }
    }
    bump();
}
