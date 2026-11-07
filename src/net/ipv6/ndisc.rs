/*
 *
 *       src/net/ipv6/ndisc.rs
 *       IPv6 Neighbor Discovery (RFC 4861): NS, NA, RS, RA
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::{ARPHRD_ETHER, NetDevice},
        ip::{self, IpAddr, Ipv6Addr},
        neigh::{self, NUD_REACHABLE, NUD_STALE},
        skb::{DEFAULT_HEADROOM, SkBuff},
    },
    addr,
    header::{Ip6Meta, Tx6Template},
    output,
};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

pub const ND_ROUTER_SOLICIT: u8 = 133;
pub const ND_ROUTER_ADVERT: u8 = 134;
pub const ND_NEIGHBOR_SOLICIT: u8 = 135;
pub const ND_NEIGHBOR_ADVERT: u8 = 136;
pub const ND_REDIRECT: u8 = 137;

pub const ND_OPT_SOURCE_LL_ADDR: u8 = 1;
pub const ND_OPT_TARGET_LL_ADDR: u8 = 2;
pub const ND_OPT_PREFIX_INFO: u8 = 3;
pub const ND_OPT_MTU: u8 = 5;

pub const NA_FLAG_ROUTER: u32 = 0x8000_0000;
pub const NA_FLAG_SOLICITED: u32 = 0x4000_0000;
pub const NA_FLAG_OVERRIDE: u32 = 0x2000_0000;

static NS_SENT: AtomicU64 = AtomicU64::new(0);
static NA_SENT: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    neigh::register_family(neigh::AF_INET6, solicit);
}

pub fn ns_sent() -> u64 {
    NS_SENT.load(Ordering::Relaxed)
}

pub fn na_sent() -> u64 {
    NA_SENT.load(Ordering::Relaxed)
}

fn build_nd(
    source: Ipv6Addr,
    destination: Ipv6Addr,
    kind: u8,
    flags_or_reserved: u32,
    target: Ipv6Addr,
    option_kind: u8,
    hardware: &[u8],
) -> Option<SkBuff> {
    if hardware.len() != 6 {
        return None;
    }
    let length = 24 + 8;
    let mut skb = SkBuff::alloc(DEFAULT_HEADROOM, length)?;
    let packet = skb.put(length)?;
    packet[0] = kind;
    packet[1] = 0;
    packet[2..4].copy_from_slice(&[0, 0]);
    packet[4..8].copy_from_slice(&flags_or_reserved.to_be_bytes());
    packet[8..24].copy_from_slice(&target.octets());
    packet[24] = option_kind;
    packet[25] = 1; // 8-byte units
    packet[26..32].copy_from_slice(hardware);
    let seed = ip::pseudo_v6(source, destination, ip::IPPROTO_ICMPV6, length as u32);
    let sum = ip::checksum(seed, packet);
    packet[2..4].copy_from_slice(&sum.to_be_bytes());
    Some(skb)
}

fn nd_template() -> Tx6Template {
    Tx6Template {
        traffic_class: 0,
        flow_label: 0,
        hop_limit: 255,
        protocol: ip::IPPROTO_ICMPV6,
    }
}

/// `ndisc_solicit`: emit a Neighbor Solicitation to the solicited-node
/// multicast group of `target`.
pub fn solicit(dev: &Arc<NetDevice>, target: IpAddr) {
    let IpAddr::V6(target) = target else {
        return;
    };
    if dev.dev_type != ARPHRD_ETHER {
        return;
    }
    let Some(source) = addr::select_source(dev.ifindex(), target) else {
        return;
    };
    let destination = addr::solicited_node(target);
    let hardware = dev.hw_addr();
    let Some(skb) = build_nd(
        source,
        destination,
        ND_NEIGHBOR_SOLICIT,
        0,
        target,
        ND_OPT_SOURCE_LL_ADDR,
        &hardware,
    ) else {
        return;
    };
    if output::send_on_device(dev, source, destination, destination, nd_template(), skb).is_ok() {
        NS_SENT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Unsolicited Neighbor Advertisement announcing `address` on link.
pub fn announce(dev: &Arc<NetDevice>, address: Ipv6Addr) {
    if dev.dev_type != ARPHRD_ETHER {
        return;
    }
    let destination = addr::ALL_NODES_LINK_LOCAL;
    let hardware = dev.hw_addr();
    let Some(skb) = build_nd(
        address,
        destination,
        ND_NEIGHBOR_ADVERT,
        NA_FLAG_OVERRIDE,
        address,
        ND_OPT_TARGET_LL_ADDR,
        &hardware,
    ) else {
        return;
    };
    if output::send_on_device(dev, address, destination, destination, nd_template(), skb).is_ok() {
        NA_SENT.fetch_add(1, Ordering::Relaxed);
    }
}

fn find_lladdr_option(options: &[u8], expected_type: u8) -> Option<[u8; 6]> {
    let mut offset = 0usize;
    while offset + 2 <= options.len() {
        let opt_type = options[offset];
        let opt_units = options[offset + 1] as usize;
        if opt_units == 0 {
            break;
        }
        let opt_len = opt_units * 8;
        if offset + opt_len > options.len() {
            break;
        }
        if opt_type == expected_type && opt_len >= 8 {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&options[offset + 2..offset + 8]);
            return Some(mac);
        }
        offset += opt_len;
    }
    None
}

pub fn receive(dev: &Arc<NetDevice>, meta: &Ip6Meta, data: &[u8]) {
    if meta.hop_limit != 255 || data.len() < 8 {
        return;
    }
    match data[0] {
        ND_NEIGHBOR_SOLICIT => receive_ns(dev, meta, data),
        ND_NEIGHBOR_ADVERT => receive_na(dev, meta, data),
        ND_ROUTER_ADVERT => receive_ra(dev, meta, data),
        _ => {}
    }
}

fn receive_ns(dev: &Arc<NetDevice>, meta: &Ip6Meta, data: &[u8]) {
    if data.len() < 24 {
        return;
    }
    let mut target_octets = [0u8; 16];
    target_octets.copy_from_slice(&data[8..24]);
    let target = Ipv6Addr::from(target_octets);
    if target.is_multicast() {
        return;
    }
    let source_mac = find_lladdr_option(&data[24..], ND_OPT_SOURCE_LL_ADDR);
    if !meta.source.is_unspecified()
        && !meta.source.is_multicast()
        && let Some(mac) = source_mac
    {
        neigh::update(
            dev,
            neigh::AF_INET6,
            IpAddr::V6(meta.source),
            &mac,
            NUD_STALE,
        );
    }
    if !addr::is_local(target) {
        return;
    }
    let (destination, flags) = if meta.source.is_unspecified() {
        (addr::ALL_NODES_LINK_LOCAL, NA_FLAG_OVERRIDE)
    } else {
        (meta.source, NA_FLAG_SOLICITED | NA_FLAG_OVERRIDE)
    };
    let hardware = dev.hw_addr();
    let Some(reply) = build_nd(
        target,
        destination,
        ND_NEIGHBOR_ADVERT,
        flags,
        target,
        ND_OPT_TARGET_LL_ADDR,
        &hardware,
    ) else {
        return;
    };
    if output::send_on_device(dev, target, destination, destination, nd_template(), reply).is_ok() {
        NA_SENT.fetch_add(1, Ordering::Relaxed);
    }
}

fn receive_na(dev: &Arc<NetDevice>, _meta: &Ip6Meta, data: &[u8]) {
    if data.len() < 24 {
        return;
    }
    let flags = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let mut target_octets = [0u8; 16];
    target_octets.copy_from_slice(&data[8..24]);
    let target = Ipv6Addr::from(target_octets);
    if target.is_multicast() {
        return;
    }
    if let Some(mac) = find_lladdr_option(&data[24..], ND_OPT_TARGET_LL_ADDR) {
        let state = if flags & NA_FLAG_SOLICITED != 0 {
            NUD_REACHABLE
        } else {
            NUD_STALE
        };
        neigh::update(dev, neigh::AF_INET6, IpAddr::V6(target), &mac, state);
    }
}

fn receive_ra(dev: &Arc<NetDevice>, meta: &Ip6Meta, data: &[u8]) {
    if data.len() < 16 || !addr::is_link_local(meta.source) {
        return;
    }
    if let Some(mac) = find_lladdr_option(&data[16..], ND_OPT_SOURCE_LL_ADDR) {
        neigh::update(
            dev,
            neigh::AF_INET6,
            IpAddr::V6(meta.source),
            &mac,
            NUD_STALE,
        );
    }
}
