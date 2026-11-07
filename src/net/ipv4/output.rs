/*
 *
 *       src/net/ipv4/output.rs
 *       IPv4 transmit path: routing, header construction and fragmentation
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::{self, NetDevice},
        ether,
        fib::{
            self, RTN_BLACKHOLE, RTN_BROADCAST, RTN_LOCAL, RTN_MULTICAST, RTN_PROHIBIT,
            RTN_UNREACHABLE,
        },
        ip::{self, IpAddr, Ipv4Addr},
        neigh,
        skb::SkBuff,
    },
    addr,
    header::{IP_DF, IPV4_HLEN, IPV4_MAX_OPTLEN, Ipv4Header, TxTemplate, stamp_tx_options},
    input,
};
use crate::vfs::VfsError;
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU16, Ordering};

static NEXT_ID: AtomicU16 = AtomicU16::new(1);

fn next_id() -> u16 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        NEXT_ID.store(1, Ordering::Relaxed);
    }
    id
}

/// Address selection for an outgoing datagram (`inet_select_addr`). An explicit
/// `Some(0.0.0.0)` is kept, which is what a DHCP client needs.
fn select_source(route: &fib::Route, explicit: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
    if let Some(address) = explicit {
        return Some(address);
    }
    if let Some(IpAddr::V4(address)) = route.source {
        return Some(address);
    }
    addr::first_on(route.ifindex)
}

fn build_header_with_options(
    template: &TxTemplate,
    options: &[u8],
    source: Ipv4Addr,
    destination: Ipv4Addr,
    payload_len: usize,
    id: u16,
    flags_offset: u16,
) -> Option<Vec<u8>> {
    let opt_len = if options.is_empty() {
        0
    } else {
        (options.len() + 3) & !3
    };
    if opt_len > IPV4_MAX_OPTLEN {
        return None;
    }
    let hlen = IPV4_HLEN + opt_len;
    let total = hlen.checked_add(payload_len)?;
    if total > u16::MAX as usize {
        return None;
    }
    let mut header = alloc::vec![0u8; hlen];
    header[0] = 0x40 | ((hlen / 4) as u8);
    header[1] = template.tos;
    header[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    header[4..6].copy_from_slice(&id.to_be_bytes());
    header[6..8].copy_from_slice(&flags_offset.to_be_bytes());
    header[8] = template.ttl;
    header[9] = template.protocol;
    header[10..12].copy_from_slice(&[0, 0]);
    header[12..16].copy_from_slice(&source.octets());
    header[16..20].copy_from_slice(&destination.octets());
    if opt_len != 0 {
        header[IPV4_HLEN..IPV4_HLEN + options.len()].copy_from_slice(options);
        stamp_tx_options(&mut header[IPV4_HLEN..hlen], source);
    }
    let sum = ip::checksum(0, &header[..hlen]);
    header[10..12].copy_from_slice(&sum.to_be_bytes());
    Some(header)
}

fn build_header(
    template: &TxTemplate,
    source: Ipv4Addr,
    destination: Ipv4Addr,
    payload_len: usize,
    id: u16,
    flags_offset: u16,
) -> Option<Vec<u8>> {
    build_header_with_options(
        template,
        &[],
        source,
        destination,
        payload_len,
        id,
        flags_offset,
    )
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
    destination: Ipv4Addr,
    next_hop: Ipv4Addr,
) -> Result<(), VfsError> {
    neigh::output(
        dev,
        skb,
        neigh::AF_INET,
        IpAddr::V4(destination),
        IpAddr::V4(next_hop),
    )
}

/// Send a transport payload with a kernel-built IPv4 header
/// (`ip_queue_xmit` plus `ip_local_out`).
pub fn send(
    source: Option<Ipv4Addr>,
    destination: Ipv4Addr,
    template: TxTemplate,
    payload: SkBuff,
) -> Result<(), VfsError> {
    send_with_options(source, destination, template, &[], payload)
}

/// Send a transport payload with optional IPv4 header options (`IP_OPTIONS`).
pub fn send_with_options(
    source: Option<Ipv4Addr>,
    destination: Ipv4Addr,
    template: TxTemplate,
    options: &[u8],
    payload: SkBuff,
) -> Result<(), VfsError> {
    let opt_len = if options.is_empty() {
        0
    } else {
        (options.len() + 3) & !3
    };
    let hlen = IPV4_HLEN + opt_len;
    if opt_len > IPV4_MAX_OPTLEN || payload.len() > u16::MAX as usize - hlen {
        return Err(VfsError::MessageTooLong);
    }
    let df_flags = if template.dont_fragment { IP_DF } else { 0 };
    let route = match fib::lookup(IpAddr::V4(destination)) {
        Some(route) => route,
        None if destination.is_broadcast() || destination.is_multicast() => {
            let dev = dev::devices()
                .into_iter()
                .find(|d| d.is_up() && d.flags() & dev::IFF_LOOPBACK == 0)
                .ok_or(VfsError::NetworkUnreachable)?;
            let src = source
                .or_else(|| addr::first_on(dev.ifindex()))
                .unwrap_or(Ipv4Addr::UNSPECIFIED);
            let id = next_id();
            let Some(header) = build_header_with_options(
                &template,
                options,
                src,
                destination,
                payload.len(),
                id,
                df_flags,
            ) else {
                return Err(VfsError::MessageTooLong);
            };
            let mut skb = payload;
            if !prepend_header(&mut skb, &header) {
                return Err(VfsError::NoBuffers);
            }
            return transmit(&dev, skb, destination, destination);
        }
        None => return Err(VfsError::NetworkUnreachable),
    };
    match route.kind {
        RTN_LOCAL => {
            let dev = dev::by_index(route.ifindex).ok_or(VfsError::NoDevice)?;
            let Some(source) = select_source(&route, source) else {
                return Err(VfsError::AddressNotAvailable);
            };
            let id = next_id();
            let Some(header) = build_header_with_options(
                &template,
                options,
                source,
                destination,
                payload.len(),
                id,
                df_flags,
            ) else {
                return Err(VfsError::MessageTooLong);
            };
            let mut skb = payload;
            if !prepend_header(&mut skb, &header) {
                return Err(VfsError::NoBuffers);
            }
            input::deliver(&dev, skb);
            Ok(())
        }
        RTN_BROADCAST | RTN_MULTICAST => {
            let dev = dev::by_index(route.ifindex).ok_or(VfsError::NoDevice)?;
            let src = select_source(&route, source).unwrap_or(Ipv4Addr::UNSPECIFIED);
            let id = next_id();
            let Some(header) = build_header_with_options(
                &template,
                options,
                src,
                destination,
                payload.len(),
                id,
                df_flags,
            ) else {
                return Err(VfsError::MessageTooLong);
            };
            let mut skb = payload;
            if !prepend_header(&mut skb, &header) {
                return Err(VfsError::NoBuffers);
            }
            if dev.flags() & dev::IFF_LOOPBACK == 0
                && dev.is_up()
                && let Some(wire_copy) = skb.duplicate()
            {
                let _ = transmit(&dev, wire_copy, destination, destination);
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
            let src = match select_source(&route, source) {
                Some(s) => s,
                None if destination.is_broadcast() => Ipv4Addr::UNSPECIFIED,
                None => return Err(VfsError::AddressNotAvailable),
            };
            let next_hop = if destination.is_broadcast() || destination.is_multicast() {
                destination
            } else {
                match route.gateway {
                    Some(IpAddr::V4(gateway)) => gateway,
                    _ => destination,
                }
            };
            let mtu = dev.mtu() as usize;
            let id = next_id();
            if payload.len() + hlen > mtu {
                if template.dont_fragment {
                    return Err(VfsError::MessageTooLong);
                }
                return super::fragment::send(
                    &dev,
                    src,
                    destination,
                    next_hop,
                    &template,
                    id,
                    payload,
                    mtu,
                );
            }
            let Some(header) = build_header_with_options(
                &template,
                options,
                src,
                destination,
                payload.len(),
                id,
                df_flags,
            ) else {
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

/// Transmit a complete user-supplied IPv4 datagram (`raw_send_hdrinc` in
/// `net/ipv4/raw.c`, used when `IP_HDRINCL` or `IPPROTO_RAW` is active).
pub fn send_hdrincl(destination: Ipv4Addr, packet: &[u8]) -> Result<(), VfsError> {
    if packet.len() < IPV4_HLEN {
        return Err(VfsError::InvalidArgument);
    }
    if packet.len() > u16::MAX as usize {
        return Err(VfsError::MessageTooLong);
    }
    let parsed = Ipv4Header::parse(packet).ok_or(VfsError::InvalidArgument)?;
    let dst = if parsed.destination.is_unspecified() {
        destination
    } else {
        parsed.destination
    };
    let route = fib::lookup(IpAddr::V4(dst)).ok_or(VfsError::NetworkUnreachable)?;
    let dev = dev::by_index(route.ifindex).ok_or(VfsError::NoDevice)?;
    let mut skb = SkBuff::alloc(super::super::skb::DEFAULT_HEADROOM, packet.len())
        .ok_or(VfsError::NoBuffers)?;
    let bytes = skb.put(packet.len()).ok_or(VfsError::NoBuffers)?;
    bytes.copy_from_slice(packet);
    let hlen = parsed.header_len();
    bytes[2..4].copy_from_slice(&(packet.len() as u16).to_be_bytes());
    if parsed.id == 0 {
        bytes[4..6].copy_from_slice(&next_id().to_be_bytes());
    }
    if parsed.source.is_unspecified()
        && let Some(src) = select_source(&route, None)
    {
        bytes[12..16].copy_from_slice(&src.octets());
    }
    bytes[16..20].copy_from_slice(&dst.octets());
    bytes[10..12].copy_from_slice(&[0, 0]);
    let sum = ip::checksum(0, &bytes[..hlen]);
    bytes[10..12].copy_from_slice(&sum.to_be_bytes());
    skb.mark_network_header();
    match route.kind {
        RTN_LOCAL => {
            input::deliver(&dev, skb);
            Ok(())
        }
        RTN_BROADCAST | RTN_MULTICAST => {
            if dev.flags() & dev::IFF_LOOPBACK == 0
                && dev.is_up()
                && let Some(wire_copy) = skb.duplicate()
            {
                let _ = transmit(&dev, wire_copy, dst, dst);
            }
            input::deliver(&dev, skb);
            Ok(())
        }
        RTN_UNREACHABLE => Err(VfsError::NetworkUnreachable),
        RTN_BLACKHOLE | RTN_PROHIBIT => Err(VfsError::NotPermitted),
        _ => {
            if !dev.is_up() {
                return Err(VfsError::NoDevice);
            }
            let next_hop = match route.gateway {
                Some(IpAddr::V4(gateway)) => gateway,
                _ => dst,
            };
            transmit(&dev, skb, dst, next_hop)
        }
    }
}

/// One fragment produced by [`send`] or the forwarding path.
pub(crate) fn send_fragment(
    dev: &Arc<NetDevice>,
    source: Ipv4Addr,
    destination: Ipv4Addr,
    next_hop: Ipv4Addr,
    template: &TxTemplate,
    id: u16,
    flags_offset: u16,
    payload: SkBuff,
) -> Result<(), VfsError> {
    let Some(header) = build_header(
        template,
        source,
        destination,
        payload.len(),
        id,
        flags_offset,
    ) else {
        return Err(VfsError::MessageTooLong);
    };
    let mut skb = payload;
    if !prepend_header(&mut skb, &header) {
        return Err(VfsError::NoBuffers);
    }
    transmit(dev, skb, destination, next_hop)
}

/// Build a complete packet for a locally generated payload and return it, used
/// by forwarding and raw injection paths that already carry a transport header.
pub fn wrap_payload(
    template: &TxTemplate,
    source: Ipv4Addr,
    destination: Ipv4Addr,
    id: u16,
    flags_offset: u16,
    mut payload: SkBuff,
) -> Result<SkBuff, VfsError> {
    let Some(header) = build_header(
        template,
        source,
        destination,
        payload.len(),
        id,
        flags_offset,
    ) else {
        return Err(VfsError::MessageTooLong);
    };
    if !prepend_header(&mut payload, &header) {
        return Err(VfsError::NoBuffers);
    }
    payload.protocol = ether::ETH_P_IP;
    Ok(payload)
}

/// `ip_forward`: decrement the TTL and pass the unchanged packet onward.
pub fn forward(skb: SkBuff) -> Result<(), VfsError> {
    let Some(header) = Ipv4Header::parse(skb.data()) else {
        return Err(VfsError::InvalidArgument);
    };
    if header.ttl <= 1 {
        return Err(VfsError::TimedOut);
    }
    let destination = header.destination;
    let route = fib::lookup(IpAddr::V4(destination)).ok_or(VfsError::NetworkUnreachable)?;
    if matches!(route.kind, RTN_LOCAL | RTN_BROADCAST) {
        return Err(VfsError::InvalidArgument);
    }
    let dev = dev::by_index(route.ifindex).ok_or(VfsError::NoDevice)?;
    if !dev.is_up() {
        return Err(VfsError::NoDevice);
    }
    let next_hop = match route.gateway {
        Some(IpAddr::V4(gateway)) => gateway,
        _ => destination,
    };
    let mut skb = skb;
    {
        let Some(bytes) = skb.data_mut().get_mut(..IPV4_HLEN) else {
            return Err(VfsError::InvalidArgument);
        };
        bytes[8] -= 1;
        bytes[10..12].copy_from_slice(&[0, 0]);
        let sum = ip::checksum(0, bytes);
        bytes[10..12].copy_from_slice(&sum.to_be_bytes());
    }
    skb.protocol = ether::ETH_P_IP;
    transmit(&dev, skb, destination, next_hop)
}

/// Whether a datagram to `destination` is delivered locally.
pub fn is_local(destination: Ipv4Addr) -> bool {
    if destination.is_multicast() || ip::is_limited_broadcast(destination) {
        return true;
    }
    matches!(
        fib::lookup(IpAddr::V4(destination)).map(|route| route.kind),
        Some(RTN_LOCAL | RTN_BROADCAST | RTN_MULTICAST)
    )
}
