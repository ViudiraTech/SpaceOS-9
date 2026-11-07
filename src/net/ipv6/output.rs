/*
 *
 *       src/net/ipv6/output.rs
 *       IPv6 transmit path: routing, source selection, header construction
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
        ip::{IpAddr, Ipv6Addr},
        neigh,
        skb::SkBuff,
    },
    addr,
    header::{IPV6_HLEN, Ipv6Header, Tx6Template},
    input,
};
use crate::vfs::VfsError;
use alloc::{sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

static NEXT_FRAG_ID: AtomicU32 = AtomicU32::new(1);

fn next_frag_id() -> u32 {
    let id = NEXT_FRAG_ID.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        NEXT_FRAG_ID.store(1, Ordering::Relaxed);
        1
    } else {
        id
    }
}

fn select_source(
    route: &fib::Route,
    destination: Ipv6Addr,
    explicit: Option<Ipv6Addr>,
) -> Option<Ipv6Addr> {
    if let Some(address) = explicit {
        return Some(address);
    }
    if let Some(IpAddr::V6(address)) = route.source {
        return Some(address);
    }
    addr::select_source(route.ifindex, destination)
}

fn build_header(
    template: &Tx6Template,
    source: Ipv6Addr,
    destination: Ipv6Addr,
    payload_len: usize,
) -> Option<Vec<u8>> {
    if payload_len > u16::MAX as usize {
        return None;
    }
    let header = Ipv6Header {
        version: 6,
        traffic_class: template.traffic_class,
        flow_label: template.flow_label,
        payload_len: payload_len as u16,
        next_header: template.protocol,
        hop_limit: template.hop_limit,
        source,
        destination,
    };
    let mut bytes = vec![0u8; IPV6_HLEN];
    header.write(&mut bytes);
    Some(bytes)
}

fn prepend_header(skb: &mut SkBuff, header: &[u8]) -> bool {
    if !skb.ensure_headroom(header.len()) {
        return false;
    }
    let Some(destination) = skb.push(header.len()) else {
        return false;
    };
    destination.copy_from_slice(header);
    skb.mark_network_header();
    true
}

fn transmit(
    dev: &Arc<NetDevice>,
    skb: SkBuff,
    destination: Ipv6Addr,
    next_hop: Ipv6Addr,
) -> Result<(), VfsError> {
    neigh::output(
        dev,
        skb,
        neigh::AF_INET6,
        IpAddr::V6(destination),
        IpAddr::V6(next_hop),
    )
}

/// Send an IPv6 packet directly out a specific device (used by NDISC).
pub fn send_on_device(
    dev: &Arc<NetDevice>,
    source: Ipv6Addr,
    destination: Ipv6Addr,
    next_hop: Ipv6Addr,
    template: Tx6Template,
    mut payload: SkBuff,
) -> Result<(), VfsError> {
    let Some(header) = build_header(&template, source, destination, payload.len()) else {
        return Err(VfsError::MessageTooLong);
    };
    if !prepend_header(&mut payload, &header) {
        return Err(VfsError::NoBuffers);
    }
    if addr::is_local(destination) || dev.dev_type == dev::ARPHRD_LOOPBACK {
        input::deliver(dev, payload);
        return Ok(());
    }
    transmit(dev, payload, destination, next_hop)
}

pub fn send(
    source: Option<Ipv6Addr>,
    destination: Ipv6Addr,
    template: Tx6Template,
    payload: SkBuff,
) -> Result<(), VfsError> {
    if payload.len() > u16::MAX as usize {
        return Err(VfsError::MessageTooLong);
    }
    if destination.is_multicast() {
        let dev = dev::devices()
            .into_iter()
            .find(|d| d.dev_type == dev::ARPHRD_ETHER && d.is_up())
            .or_else(|| dev::by_name("lo"))
            .ok_or(VfsError::NoDevice)?;
        let Some(src) = source.or_else(|| addr::select_source(dev.ifindex(), destination)) else {
            return Err(VfsError::AddressNotAvailable);
        };
        return send_on_device(&dev, src, destination, destination, template, payload);
    }
    let route = match fib::lookup(IpAddr::V6(destination)) {
        Some(route) => route,
        None => return Err(VfsError::NetworkUnreachable),
    };
    match route.kind {
        RTN_LOCAL | RTN_BROADCAST | RTN_MULTICAST => {
            let dev = dev::by_index(route.ifindex).ok_or(VfsError::NoDevice)?;
            let Some(source) = select_source(&route, destination, source) else {
                return Err(VfsError::AddressNotAvailable);
            };
            let Some(header) = build_header(&template, source, destination, payload.len()) else {
                return Err(VfsError::MessageTooLong);
            };
            let mut skb = payload;
            if !prepend_header(&mut skb, &header) {
                return Err(VfsError::NoBuffers);
            }
            input::deliver(&dev, skb);
            Ok(())
        }
        RTN_UNREACHABLE => Err(VfsError::NetworkUnreachable),
        RTN_BLACKHOLE | RTN_PROHIBIT => Err(VfsError::NotPermitted),
        _ => {
            let dev = dev::by_index(route.ifindex).ok_or(VfsError::NoDevice)?;
            if !dev.is_up() {
                return Err(VfsError::NoDevice);
            }
            let Some(source) = select_source(&route, destination, source) else {
                return Err(VfsError::AddressNotAvailable);
            };
            let next_hop = match route.gateway {
                Some(IpAddr::V6(gateway)) => gateway,
                _ => destination,
            };
            let mtu = dev.mtu() as usize;
            if payload.len() + IPV6_HLEN > mtu {
                let id = next_frag_id();
                return super::fragment::send(
                    &dev,
                    source,
                    destination,
                    next_hop,
                    &template,
                    id,
                    payload,
                    mtu,
                );
            }
            let Some(header) = build_header(&template, source, destination, payload.len()) else {
                return Err(VfsError::MessageTooLong);
            };
            let mut skb = payload;
            if !prepend_header(&mut skb, &header) {
                return Err(VfsError::NoBuffers);
            }
            transmit(&dev, skb, destination, next_hop)
        }
    }
}

pub(crate) fn send_fragment(
    dev: &Arc<NetDevice>,
    source: Ipv6Addr,
    destination: Ipv6Addr,
    next_hop: Ipv6Addr,
    template: &Tx6Template,
    mut payload: SkBuff,
) -> Result<(), VfsError> {
    let Some(header) = build_header(template, source, destination, payload.len()) else {
        return Err(VfsError::MessageTooLong);
    };
    if !prepend_header(&mut payload, &header) {
        return Err(VfsError::NoBuffers);
    }
    transmit(dev, payload, destination, next_hop)
}
