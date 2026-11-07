/*
 *
 *       src/net/loopback.rs
 *       Loopback device (drivers/net/loopback.c)
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    dev::{
        ARPHRD_LOOPBACK, IFF_LOOPBACK, NetDevice, NetDeviceConfig, NetDeviceOps, TxResult,
        netif_rx, register_netdev,
    },
    skb::{PacketType, SkBuff},
};
use crate::vfs::VfsError;
use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

struct Loopback;

impl NetDeviceOps for Loopback {
    fn start_xmit(&self, dev: &NetDevice, mut skb: SkBuff) -> TxResult {
        let length = skb.len() as u64;
        dev.stats.tx_packets.fetch_add(1, Ordering::Relaxed);
        dev.stats.tx_bytes.fetch_add(length, Ordering::Relaxed);
        // The frame has no link header: protocol was set by the sender.
        skb.pkt_type = PacketType::Host;
        netif_rx(&dev.arc(), skb);
        TxResult::Ok
    }
}

/// Create and register `lo`. It starts administratively down, like Linux;
/// `net::init` brings it up so local traffic works without a userland
/// network manager.
pub fn create() -> Result<Arc<NetDevice>, VfsError> {
    let config = NetDeviceConfig {
        name: String::from("lo"),
        dev_type: ARPHRD_LOOPBACK,
        hw_addr: Vec::from([0u8; 6]),
        broadcast: Vec::from([0u8; 6]),
        mtu: 65536,
        min_mtu: 0,
        max_mtu: 65536,
        flags: IFF_LOOPBACK,
        tx_queue_len: 0,
    };
    let dev = NetDevice::new(config, Arc::new(Loopback))?;
    register_netdev(&dev)?;
    Ok(dev)
}
