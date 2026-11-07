/*
 *
 *       src/net/ipv6/mod.rs
 *       IPv6 protocol family (RFC 8200 / RFC 4861 / RFC 4443)
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

pub mod addr;
pub mod fragment;
pub mod header;
pub mod icmpv6;
pub mod input;
pub mod ndisc;
pub mod output;
pub mod raw;

use super::{
    dev, ether,
    ip::IPPROTO_ICMPV6,
    socket::{self, SOCK_DGRAM, SOCK_RAW},
};
use crate::vfs::{OpenFile, VfsError};
use alloc::sync::Arc;

fn inet6_create(kind: u32, protocol: u32, nonblock: bool) -> Result<Arc<OpenFile>, VfsError> {
    match kind {
        SOCK_DGRAM if protocol == IPPROTO_ICMPV6 as u32 => raw::create_ping(nonblock),
        SOCK_RAW => raw::create_raw(kind, protocol, nonblock),
        _ => Err(VfsError::ProtocolType),
    }
}

/// Register the IPv6 packet handlers, NDISC and loopback/link-local addresses.
pub fn init() {
    dev::register_packet_type(ether::ETH_P_IPV6, input::receive);
    ndisc::init();
    input::register_protocol(IPPROTO_ICMPV6, icmpv6::receive);
    if let Err(error) = socket::register_family(socket::AF_INET6, inet6_create) {
        crate::kwarn!("net: AF_INET6 family registration failed: {error:?}");
    }
    if let Some(lo) = dev::by_name("lo")
        && let Err(error) = addr::init_loopback(lo.ifindex())
    {
        crate::kwarn!("net: IPv6 loopback address installation failed: {error:?}");
    }
    for device in dev::devices() {
        addr::ensure_link_local(&device);
    }
    dev::register_notifier(|event, device| match event {
        dev::NetdevEvent::Register | dev::NetdevEvent::Up => {
            addr::ensure_link_local(device);
        }
        dev::NetdevEvent::Unregister => {
            let _ = addr::flush_device(device.ifindex());
        }
        _ => {}
    });
    crate::verbose::log(format_args!(
        "net: IPv6 initialised ({} address(es))",
        addr::snapshot().len()
    ));
}
