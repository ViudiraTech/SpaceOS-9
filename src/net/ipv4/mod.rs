/*
 *
 *       src/net/ipv4/mod.rs
 *       IPv4 protocol family
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

pub mod addr;
pub mod arp;
pub mod fragment;
pub mod header;
pub mod icmp;
pub mod input;
pub mod output;
pub mod raw;

use super::{
    dev, ether,
    ip::IPPROTO_ICMP,
    socket::{self, SOCK_DGRAM, SOCK_RAW, SOCK_STREAM},
    tcp, udp,
};
use crate::vfs::{OpenFile, VfsError};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

static FORWARDING: AtomicBool = AtomicBool::new(false);

/// `sysctl net.ipv4.ip_forward`.
pub fn forwarding() -> bool {
    FORWARDING.load(Ordering::Acquire)
}

pub fn set_forwarding(enabled: bool) {
    FORWARDING.store(enabled, Ordering::Release);
}

/// `inet_create` for `AF_INET`. UDP and TCP register their protocol types in
/// their own initialisers; raw sockets cover everything else.
fn inet_create(kind: u32, protocol: u32, nonblock: bool) -> Result<Arc<OpenFile>, VfsError> {
    match kind {
        SOCK_STREAM => tcp::create_stream(kind, protocol, nonblock),
        SOCK_DGRAM if protocol == IPPROTO_ICMP as u32 => raw::create_ping(nonblock),
        SOCK_DGRAM => udp::create_dgram(kind, protocol, nonblock),
        SOCK_RAW => raw::create_raw(kind, protocol, nonblock),
        _ => Err(VfsError::ProtocolType),
    }
}

/// Register the IPv4 packet handlers and bring up loopback addressing.
pub fn init() {
    dev::register_packet_type(ether::ETH_P_IP, input::receive);
    arp::init();
    input::register_protocol(IPPROTO_ICMP, icmp::receive);
    tcp::init();
    udp::init();
    if let Err(error) = socket::register_family(socket::AF_INET, inet_create) {
        crate::kwarn!("net: AF_INET family registration failed: {error:?}");
    }
    if let Some(lo) = dev::by_name("lo")
        && let Err(error) = addr::init_loopback(lo.ifindex())
    {
        crate::kwarn!("net: loopback address installation failed: {error:?}");
    }
    dev::register_notifier(|event, device| {
        if event == dev::NetdevEvent::Unregister {
            let removed = addr::flush_device(device.ifindex());
            let _ = removed.len();
            super::fib::flush_device(device.ifindex());
            super::neigh::flush_device(device.ifindex());
        }
    });
    crate::verbose::log(format_args!(
        "net: IPv4 initialised ({} address(es))",
        addr::snapshot().len()
    ));
}
