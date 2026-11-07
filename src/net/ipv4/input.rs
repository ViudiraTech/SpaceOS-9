/*
 *
 *       src/net/ipv4/input.rs
 *       IPv4 receive path: validation, local delivery, forwarding
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::{self, NetDevice},
        fib::{
            self, RTN_BLACKHOLE, RTN_BROADCAST, RTN_LOCAL, RTN_MULTICAST, RTN_PROHIBIT,
            RTN_UNREACHABLE,
        },
        ip::{IpAddr, Ipv4Addr},
        skb::SkBuff,
    },
    fragment,
    header::{IpMeta, Ipv4Header},
    icmp, output, raw,
};
use crate::sync::SpinLock;
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

/// Transport and control protocol entry points (`inet_add_protocol`).
pub type ProtocolHandler = fn(&Arc<NetDevice>, &IpMeta, SkBuff);

static PROTOCOLS: SpinLock<Vec<(u8, ProtocolHandler)>> = SpinLock::new(Vec::new());
static IN_RECEIVED: AtomicU64 = AtomicU64::new(0);
static IN_DELIVERED: AtomicU64 = AtomicU64::new(0);
static IN_DISCARDED: AtomicU64 = AtomicU64::new(0);

pub fn register_protocol(protocol: u8, handler: ProtocolHandler) {
    let mut protocols = PROTOCOLS.lock();
    if let Some(entry) = protocols.iter_mut().find(|entry| entry.0 == protocol) {
        entry.1 = handler;
    } else {
        protocols.push((protocol, handler));
    }
}

fn handler_for(protocol: u8) -> Option<ProtocolHandler> {
    PROTOCOLS
        .lock()
        .iter()
        .find(|entry| entry.0 == protocol)
        .map(|entry| entry.1)
}

pub fn statistics() -> (u64, u64, u64) {
    (
        IN_RECEIVED.load(Ordering::Relaxed),
        IN_DELIVERED.load(Ordering::Relaxed),
        IN_DISCARDED.load(Ordering::Relaxed),
    )
}

/// `ip_rcv`: the entry point installed for `ETH_P_IP`.
pub fn receive(dev: &Arc<NetDevice>, mut skb: SkBuff) {
    IN_RECEIVED.fetch_add(1, Ordering::Relaxed);
    let Some(header) = Ipv4Header::parse(skb.data()) else {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let total = header.total_len as usize;
    if total < header.header_len() || total > skb.len() || !header.verify(skb.data()) {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    skb.trim(total);
    if header.is_fragment() {
        let Some(packet) = fragment::consume(dev, skb, &header) else {
            return;
        };
        deliver(dev, packet);
    } else {
        skb.mark_network_header();
        deliver(dev, skb);
    }
}

/// `ip_local_deliver` / `ip_forward`: classify a complete datagram.
pub fn deliver(dev: &Arc<NetDevice>, mut skb: SkBuff) {
    let Some(header) = Ipv4Header::parse(skb.data()) else {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let meta = IpMeta::from_header(&header);
    if meta.total_len > skb.len() {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    skb.trim(meta.total_len);
    skb.mark_network_header();
    let route = fib::lookup(IpAddr::V4(meta.destination));
    let local = match route.as_ref().map(|route| route.kind) {
        Some(RTN_LOCAL | RTN_BROADCAST | RTN_MULTICAST) => true,
        Some(RTN_BLACKHOLE | RTN_PROHIBIT | RTN_UNREACHABLE) => false,
        _ => {
            meta.destination.is_multicast()
                || meta.destination.is_broadcast()
                || (meta.protocol == super::super::ip::IPPROTO_UDP
                    && super::addr::for_device(dev.ifindex()).is_empty())
        }
    };
    if !local {
        forward(dev, skb, &meta);
        return;
    }
    IN_DELIVERED.fetch_add(1, Ordering::Relaxed);
    raw::deliver(dev, &meta, &skb);
    let Some(handler) = handler_for(meta.protocol) else {
        // A locally delivered datagram for an unhandled protocol must be
        // answered with ICMP protocol unreachable, never when it is multicast.
        if !meta.destination.is_multicast() && !meta.destination.is_broadcast() {
            icmp::send_error(
                dev,
                skb.network_slice().unwrap_or_default(),
                icmp::ICMP_DEST_UNREACH,
                icmp::ICMP_PROT_UNREACH,
            );
        }
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    // The handler sees the transport header at the front of the data area.
    if skb.pull(meta.header_len).is_none() {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    handler(dev, &meta, skb);
}

/// `ip_forward`. Errors are reported to the source where ICMP defines one.
fn forward(dev: &Arc<NetDevice>, skb: SkBuff, meta: &IpMeta) {
    if !super::forwarding() || meta.destination.is_multicast() {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if meta.ttl <= 1 {
        icmp::send_error(
            dev,
            skb.network_slice().unwrap_or_default(),
            icmp::ICMP_TIME_EXCEEDED,
            icmp::ICMP_EXC_TTL,
        );
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if output::forward(skb).is_err() {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Whether `dev` currently owns `address` (used by ARP and ICMP replies).
pub fn address_on(ifindex: u32, address: Ipv4Addr) -> bool {
    super::addr::get(ifindex, address).is_some()
}

/// Resolve the address of the interface a reply should leave through.
pub fn reply_source(dev: &Arc<NetDevice>, fallback: Ipv4Addr) -> Ipv4Addr {
    super::addr::first_on(dev.ifindex()).unwrap_or(fallback)
}

/// Device lookup helper for handlers that only carry an ifindex.
pub fn device_for(ifindex: u32) -> Option<Arc<NetDevice>> {
    dev::by_index(ifindex)
}

/// Aggregate IP-level counters for diagnostics.
pub fn counters() -> (u64, u64, u64) {
    (
        IN_RECEIVED.load(Ordering::Relaxed),
        IN_DELIVERED.load(Ordering::Relaxed),
        IN_DISCARDED.load(Ordering::Relaxed),
    )
}
