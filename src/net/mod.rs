/*
 *
 *       src/net/mod.rs
 *       Network stack: devices, buffers, protocols and netlink
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Network stack modelled on Linux `net/`. See `docs/NETWORK_STACK_DESIGN.md`
//! for the layering and the phase plan.

pub mod dev;
pub mod ether;
pub mod fib;
pub mod ip;
pub mod ipv4;
pub mod ipv6;
pub mod loopback;
pub mod module_api;
pub mod neigh;
pub mod netlink;
pub mod skb;
pub mod socket;
pub mod tcp;
pub mod timer;
pub mod udp;

#[cfg(feature = "boot-self-test")]
pub mod self_test;

/// Register built-in devices. Requires the device manager (`vfs::init`).
pub fn init() {
    socket::init();
    netlink::init();
    match loopback::create() {
        Ok(lo) => {
            let _rtnl = dev::rtnl_lock();
            // Linux leaves `lo` down until userspace raises it; the kernel
            // does it here because there may be no network manager at all.
            if let Err(error) = dev::open_locked(&lo) {
                crate::kwarn!("net: lo could not be brought up: {error:?}");
            }
        }
        Err(error) => crate::kwarn!("net: loopback registration failed: {error:?}"),
    }
    crate::kinfo!("net: {} device(s) registered", dev::devices().len());
    ipv4::init();
    ipv6::init();
    netlink::route::init();
}

/// Start the softirq thread. Requires the scheduler.
pub fn start() {
    timer::start();
    dev::start_worker();
}
