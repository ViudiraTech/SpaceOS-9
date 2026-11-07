/*
 *
 *       src/net/ipv6/input.rs
 *       IPv6 receive path: extension header walk, reassembly, local delivery
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::NetDevice,
        fib::{self, RTN_LOCAL},
        ip::IpAddr,
        skb::SkBuff,
    },
    addr, fragment,
    header::{
        IPV6_HLEN, Ip6Meta, Ipv6Header, NEXTHDR_DEST, NEXTHDR_FRAGMENT, NEXTHDR_HOP, NEXTHDR_NONE,
        NEXTHDR_ROUTING,
    },
    icmpv6, raw,
};
use crate::sync::SpinLock;
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

pub type ProtocolHandler = fn(&Arc<NetDevice>, &Ip6Meta, SkBuff);

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

fn scan_extensions(bytes: &[u8], first_next: u8) -> Option<(u8, usize, Option<usize>)> {
    let mut next = first_next;
    let mut offset = IPV6_HLEN;
    for _ in 0..8 {
        match next {
            NEXTHDR_HOP | NEXTHDR_ROUTING | NEXTHDR_DEST => {
                if offset + 2 > bytes.len() {
                    return None;
                }
                let next_hdr = bytes[offset];
                let ext_len = (bytes[offset + 1] as usize + 1) * 8;
                if offset + ext_len > bytes.len() {
                    return None;
                }
                next = next_hdr;
                offset += ext_len;
            }
            NEXTHDR_FRAGMENT => {
                return Some((next, offset, Some(offset)));
            }
            _ => break,
        }
    }
    Some((next, offset, None))
}

/// `ipv6_rcv`: the entry point installed for `ETH_P_IPV6`.
pub fn receive(dev: &Arc<NetDevice>, mut skb: SkBuff) {
    IN_RECEIVED.fetch_add(1, Ordering::Relaxed);
    let Some(header) = Ipv6Header::parse(skb.data()) else {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let total = IPV6_HLEN + header.payload_len as usize;
    if total > skb.len() {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    skb.trim(total);
    let Some((_, _, frag_offset)) = scan_extensions(skb.data(), header.next_header) else {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if let Some(frag_offset) = frag_offset {
        let Some(packet) = fragment::consume(dev, skb, &header, frag_offset) else {
            return;
        };
        deliver(dev, packet);
    } else {
        skb.mark_network_header();
        deliver(dev, skb);
    }
}

/// `ip6_input`: classify and deliver an unfragmented or reassembled IPv6 packet.
pub fn deliver(dev: &Arc<NetDevice>, mut skb: SkBuff) {
    let Some(header) = Ipv6Header::parse(skb.data()) else {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let total = IPV6_HLEN + header.payload_len as usize;
    if total > skb.len() {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    skb.trim(total);
    skb.mark_network_header();
    let Some((protocol, header_len, _)) = scan_extensions(skb.data(), header.next_header) else {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let local = if header.destination.is_multicast() {
        addr::is_local_multicast(dev.ifindex(), header.destination)
    } else if addr::is_local(header.destination) {
        true
    } else {
        fib::lookup(IpAddr::V6(header.destination)).is_some_and(|route| route.kind == RTN_LOCAL)
    };
    if !local {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let meta = Ip6Meta {
        source: header.source,
        destination: header.destination,
        protocol,
        traffic_class: header.traffic_class,
        hop_limit: header.hop_limit,
        header_len,
        total_len: total,
    };
    IN_DELIVERED.fetch_add(1, Ordering::Relaxed);
    raw::deliver(dev, &meta, &skb);
    if protocol == NEXTHDR_NONE {
        return;
    }
    let Some(handler) = handler_for(protocol) else {
        if !meta.destination.is_multicast() {
            icmpv6::send_error(
                dev,
                skb.network_slice().unwrap_or_default(),
                icmpv6::ICMPV6_PARAMPROB,
                1,
                6,
            );
        }
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if skb.pull(meta.header_len).is_none() {
        IN_DISCARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    handler(dev, &meta, skb);
}
