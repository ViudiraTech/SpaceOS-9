/*
 *
 *       src/net/ipv4/icmp.rs
 *       ICMP echo and error generation/delivery
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::NetDevice,
        ip::{self, Ipv4Addr},
        skb::SkBuff,
    },
    addr,
    header::{IPV4_HLEN, IpMeta, Ipv4Header, TxTemplate, echo_options_for_reply},
    input, output,
};
use crate::{sync::SpinLock, vfs::VfsError};
use alloc::{sync::Arc, vec::Vec};

pub const ICMP_ECHOREPLY: u8 = 0;
pub const ICMP_DEST_UNREACH: u8 = 3;
pub const ICMP_SOURCE_QUENCH: u8 = 4;
pub const ICMP_REDIRECT: u8 = 5;
pub const ICMP_ECHO: u8 = 8;
pub const ICMP_TIME_EXCEEDED: u8 = 11;
pub const ICMP_PARAMETERPROB: u8 = 12;
pub const ICMP_TIMESTAMP: u8 = 13;
pub const ICMP_TIMESTAMPREPLY: u8 = 14;

pub const ICMP_NET_UNREACH: u8 = 0;
pub const ICMP_HOST_UNREACH: u8 = 1;
pub const ICMP_PROT_UNREACH: u8 = 2;
pub const ICMP_PORT_UNREACH: u8 = 3;
pub const ICMP_FRAG_NEEDED: u8 = 4;

pub const ICMP_EXC_TTL: u8 = 0;
pub const ICMP_EXC_FRAG: u8 = 1;

/// Quoted original datagram carried by an ICMP error.
pub struct IcmpError<'a> {
    pub device: Arc<NetDevice>,
    /// Source of the ICMP error packet (the router or peer).
    pub source: Ipv4Addr,
    pub icmp_type: u8,
    pub code: u8,
    /// Original IP header plus at least eight transport bytes.
    pub quoted: &'a [u8],
}

pub type ErrorHandler = fn(&IcmpError<'_>);

static ERROR_HANDLERS: SpinLock<Vec<(u8, ErrorHandler)>> = SpinLock::new(Vec::new());

/// `icmp_register_rcv`.
pub fn register_error_handler(protocol: u8, handler: ErrorHandler) {
    let mut handlers = ERROR_HANDLERS.lock();
    if let Some(entry) = handlers.iter_mut().find(|entry| entry.0 == protocol) {
        entry.1 = handler;
    } else {
        handlers.push((protocol, handler));
    }
}

fn handler_for(protocol: u8) -> Option<ErrorHandler> {
    ERROR_HANDLERS
        .lock()
        .iter()
        .find(|entry| entry.0 == protocol)
        .map(|entry| entry.1)
}

fn is_error_type(kind: u8) -> bool {
    matches!(
        kind,
        ICMP_DEST_UNREACH
            | ICMP_SOURCE_QUENCH
            | ICMP_REDIRECT
            | ICMP_TIME_EXCEEDED
            | ICMP_PARAMETERPROB
    )
}

fn checksum_of(message: &[u8]) -> u16 {
    ip::checksum(0, message)
}

fn build_message(kind: u8, code: u8, rest: [u8; 4], body: &[u8]) -> Option<SkBuff> {
    let length = 8usize.checked_add(body.len())?;
    let mut skb = SkBuff::alloc(super::super::skb::DEFAULT_HEADROOM, length)?;
    let message = skb.put(length)?;
    message[0] = kind;
    message[1] = code;
    message[2..4].copy_from_slice(&[0, 0]);
    message[4..8].copy_from_slice(&rest);
    message[8..].copy_from_slice(body);
    let sum = checksum_of(&message[..]);
    message[2..4].copy_from_slice(&sum.to_be_bytes());
    Some(skb)
}

fn icmp_template() -> TxTemplate {
    TxTemplate {
        tos: 0,
        ttl: 64,
        protocol: ip::IPPROTO_ICMP,
        dont_fragment: false,
    }
}

/// `icmp_echo`: answer an echo request delivered to a local address, preserving
/// the caller's identifier/sequence and echoing any IPv4 header options.
fn send_echo_reply(
    dev: &Arc<NetDevice>,
    meta: &IpMeta,
    rest: [u8; 4],
    echoed_options: &[u8],
    payload: &[u8],
) {
    let source = if addr::is_local(meta.destination) {
        meta.destination
    } else {
        input::reply_source(dev, meta.destination)
    };
    let Some(reply) = build_message(ICMP_ECHOREPLY, 0, rest, payload) else {
        return;
    };
    let _ = output::send_with_options(
        Some(source),
        meta.source,
        icmp_template(),
        echoed_options,
        reply,
    );
}

/// `icmp_rcv`.
pub fn receive(dev: &Arc<NetDevice>, meta: &IpMeta, skb: SkBuff) {
    if skb.len() < 8 || checksum_of(skb.data()) != 0 {
        return;
    }
    let (kind, code) = {
        let data = skb.data();
        (data[0], data[1])
    };
    match kind {
        ICMP_ECHO => {
            if meta.destination.is_multicast() || meta.destination.is_broadcast() {
                return;
            }
            let data = skb.data();
            let rest = [data[4], data[5], data[6], data[7]];
            let body = data[8..].to_vec();
            let echoed_opts = if meta.header_len > IPV4_HLEN
                && let Some(net) = skb.network_slice()
                && let Some(raw_opts) = net.get(IPV4_HLEN..meta.header_len)
            {
                echo_options_for_reply(raw_opts, meta.source)
            } else {
                Vec::new()
            };
            send_echo_reply(dev, meta, rest, &echoed_opts, &body);
        }
        ICMP_ECHOREPLY | ICMP_TIMESTAMP | ICMP_TIMESTAMPREPLY => {}
        ICMP_DEST_UNREACH | ICMP_SOURCE_QUENCH | ICMP_REDIRECT | ICMP_TIME_EXCEEDED
        | ICMP_PARAMETERPROB => {
            if skb.len() < 8 + 20 {
                return;
            }
            let quoted = skb.data()[8..].to_vec();
            let Some(inner) = Ipv4Header::parse(&quoted) else {
                return;
            };
            let Some(handler) = handler_for(inner.protocol) else {
                return;
            };
            handler(&IcmpError {
                device: dev.clone(),
                source: meta.source,
                icmp_type: kind,
                code,
                quoted: &quoted,
            });
        }
        _ => {}
    }
}

/// `icmp_send`: an ICMP error quoting `original`, which must be a complete IP
/// datagram. Errors are never sent for errors, broadcasts, multicast or a
/// zero source, matching Linux's suppression rules closely enough for the
/// stack's own peers.
pub fn send_error(dev: &Arc<NetDevice>, original: &[u8], kind: u8, code: u8) {
    let Some(inner) = Ipv4Header::parse(original) else {
        return;
    };
    if inner.source.is_unspecified()
        || inner.source.is_multicast()
        || inner.source.is_broadcast()
        || inner.destination.is_multicast()
    {
        return;
    }
    if inner.protocol == ip::IPPROTO_ICMP {
        if original.len() > inner.header_len() {
            let inner_type = original[inner.header_len()];
            if is_error_type(inner_type) {
                return;
            }
        }
    }
    let quote = (inner.header_len() + 8).min(original.len());
    let body = &original[..quote];
    let Some(message) = build_message(kind, code, [0; 4], body) else {
        return;
    };
    let source = addr::first_on(dev.ifindex()).unwrap_or(inner.destination);
    let _ = output::send(Some(source), inner.source, icmp_template(), message);
}

/// Generate an echo request, used by the boot self-test and diagnostics.
pub fn send_echo(
    source: Ipv4Addr,
    destination: Ipv4Addr,
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
    let Some(message) = build_message(ICMP_ECHO, 0, rest, data) else {
        return Err(VfsError::NoBuffers);
    };
    output::send(Some(source), destination, icmp_template(), message)
}
