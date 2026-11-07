/*
 *
 *       src/net/ipv6/icmpv6.rs
 *       ICMPv6 echo, error and NDISC dispatch (RFC 4443)
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::NetDevice,
        ip::{self, Ipv6Addr},
        skb::{DEFAULT_HEADROOM, SkBuff},
    },
    addr,
    header::{IPV6_HLEN, Ip6Meta, Ipv6Header, Tx6Template},
    ndisc, output,
};
use crate::vfs::VfsError;
use alloc::sync::Arc;

pub const ICMPV6_DEST_UNREACH: u8 = 1;
pub const ICMPV6_PKT_TOOBIG: u8 = 2;
pub const ICMPV6_TIME_EXCEED: u8 = 3;
pub const ICMPV6_PARAMPROB: u8 = 4;
pub const ICMPV6_ECHO_REQUEST: u8 = 128;
pub const ICMPV6_ECHO_REPLY: u8 = 129;

pub const ICMPV6_NOROUTE: u8 = 0;
pub const ICMPV6_ADM_PROHIBITED: u8 = 1;
pub const ICMPV6_ADDR_UNREACH: u8 = 3;
pub const ICMPV6_PORT_UNREACH: u8 = 4;

fn is_error_type(kind: u8) -> bool {
    kind < 128
}

fn build_message(
    source: Ipv6Addr,
    destination: Ipv6Addr,
    kind: u8,
    code: u8,
    rest: [u8; 4],
    body: &[u8],
) -> Option<SkBuff> {
    let length = 8usize.checked_add(body.len())?;
    let mut skb = SkBuff::alloc(DEFAULT_HEADROOM, length)?;
    let message = skb.put(length)?;
    message[0] = kind;
    message[1] = code;
    message[2..4].copy_from_slice(&[0, 0]);
    message[4..8].copy_from_slice(&rest);
    message[8..].copy_from_slice(body);
    let seed = ip::pseudo_v6(source, destination, ip::IPPROTO_ICMPV6, length as u32);
    let sum = ip::checksum(seed, message);
    message[2..4].copy_from_slice(&sum.to_be_bytes());
    Some(skb)
}

fn icmp6_template() -> Tx6Template {
    Tx6Template {
        traffic_class: 0,
        flow_label: 0,
        hop_limit: 64,
        protocol: ip::IPPROTO_ICMPV6,
    }
}

pub fn receive(dev: &Arc<NetDevice>, meta: &Ip6Meta, skb: SkBuff) {
    let data = skb.data();
    if data.len() < 8 {
        return;
    }
    let seed = ip::pseudo_v6(
        meta.source,
        meta.destination,
        ip::IPPROTO_ICMPV6,
        data.len() as u32,
    );
    if ip::checksum(seed, data) != 0 {
        return;
    }
    let kind = data[0];
    match kind {
        ICMPV6_ECHO_REQUEST => {
            let source = if !meta.destination.is_multicast() && addr::is_local(meta.destination) {
                meta.destination
            } else {
                let Some(selected) = addr::select_source(dev.ifindex(), meta.source) else {
                    return;
                };
                selected
            };
            let rest = [data[4], data[5], data[6], data[7]];
            let body = &data[8..];
            let Some(reply) = build_message(source, meta.source, ICMPV6_ECHO_REPLY, 0, rest, body)
            else {
                return;
            };
            let _ = output::send(Some(source), meta.source, icmp6_template(), reply);
        }
        133..=137 => ndisc::receive(dev, meta, data),
        _ => {}
    }
}

pub fn send_error(dev: &Arc<NetDevice>, original: &[u8], kind: u8, code: u8, info: u32) {
    let Some(inner) = Ipv6Header::parse(original) else {
        return;
    };
    if inner.source.is_unspecified()
        || inner.source.is_multicast()
        || inner.destination.is_multicast()
    {
        return;
    }
    if inner.next_header == ip::IPPROTO_ICMPV6 && original.len() > IPV6_HLEN {
        let inner_type = original[IPV6_HLEN];
        if is_error_type(inner_type) {
            return;
        }
    }
    let Some(source) = addr::select_source(dev.ifindex(), inner.source) else {
        return;
    };
    let max_quote = (ip::IPV6_MIN_MTU as usize).saturating_sub(IPV6_HLEN + 8);
    let quote = original.len().min(max_quote);
    let Some(message) = build_message(
        source,
        inner.source,
        kind,
        code,
        info.to_be_bytes(),
        &original[..quote],
    ) else {
        return;
    };
    let _ = output::send(Some(source), inner.source, icmp6_template(), message);
}

pub fn send_echo(
    source: Ipv6Addr,
    destination: Ipv6Addr,
    identifier: u16,
    sequence: u16,
    data: &[u8],
) -> Result<(), VfsError> {
    let rest = [
        (identifier >> 8) as u8,
        identifier as u8,
        (sequence >> 8) as u8,
        sequence as u8,
    ];
    let Some(message) = build_message(source, destination, ICMPV6_ECHO_REQUEST, 0, rest, data)
    else {
        return Err(VfsError::NoBuffers);
    };
    output::send(Some(source), destination, icmp6_template(), message)
}
