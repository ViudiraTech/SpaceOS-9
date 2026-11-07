/*
 *
 *       src/net/netlink/uevent.rs
 *       NETLINK_KOBJECT_UEVENT: kernel device events on multicast group 1
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The kernel multicasts to group 1, exactly like Linux; userspace device
//! managers may rebroadcast to other groups, which is how libudev-zero expects
//! to receive events (group 4 by default). Each datagram is
//! `action@devpath\0KEY=VALUE\0...`.

use super::{KernelCfg, NETLINK_KOBJECT_UEVENT, NL_CFG_F_NONROOT_RECV, broadcast, kernel_create};
use alloc::{string::String, vec::Vec};

/// Group the kernel itself publishes on (`UEVENT_SOCK_GROUP`... group 1).
const KERNEL_GROUP: u32 = 1;

pub fn init() {
    let mut cfg = KernelCfg::new();
    cfg.flags = NL_CFG_F_NONROOT_RECV;
    // No input handler: the kernel never consumes uevent requests.
    if let Err(error) = kernel_create(NETLINK_KOBJECT_UEVENT, cfg) {
        crate::kwarn!("netlink: uevent protocol registration failed: {error:?}");
    }
}

/// Kernel uevent multicast to group 1. A no-op before `init`.
pub fn broadcast_uevent(action: &str, devpath: &str, properties: &[(String, String)]) {
    let mut payload = Vec::with_capacity(128);
    payload.extend_from_slice(action.as_bytes());
    payload.push(b'@');
    payload.extend_from_slice(devpath.as_bytes());
    payload.push(0);
    let mut push = |key: &str, value: &str| {
        payload.extend_from_slice(key.as_bytes());
        payload.push(b'=');
        payload.extend_from_slice(value.as_bytes());
        payload.push(0);
    };
    push("ACTION", action);
    push("DEVPATH", devpath);
    for (key, value) in properties {
        push(key, value);
    }
    broadcast(NETLINK_KOBJECT_UEVENT, 0, KERNEL_GROUP, &payload);
}
