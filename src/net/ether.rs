/*
 *
 *       src/net/ether.rs
 *       Ethernet constants, header build/parse and address classification
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::skb::{PacketType, SkBuff};

pub const ETH_ALEN: usize = 6;
pub const ETH_HLEN: usize = 14;
pub const ETH_ZLEN: usize = 60;
pub const ETH_DATA_LEN: u32 = 1500;
pub const ETH_FRAME_LEN: usize = 1514;

pub const ETH_P_IP: u16 = 0x0800;
pub const ETH_P_ARP: u16 = 0x0806;
pub const ETH_P_8021Q: u16 = 0x8100;
pub const ETH_P_IPV6: u16 = 0x86dd;
pub const ETH_P_ALL: u16 = 0x0003;

pub const BROADCAST: [u8; ETH_ALEN] = [0xff; ETH_ALEN];

pub fn is_multicast(address: &[u8]) -> bool {
    address.first().is_some_and(|byte| byte & 1 != 0)
}

pub fn is_broadcast(address: &[u8]) -> bool {
    address == BROADCAST
}

pub fn is_zero(address: &[u8]) -> bool {
    address.iter().all(|byte| *byte == 0)
}

/// A usable station address: not zero and not group-addressed
/// (`is_valid_ether_addr`).
pub fn is_valid(address: &[u8]) -> bool {
    address.len() == ETH_ALEN && !is_multicast(address) && !is_zero(address)
}

/// Prepend an Ethernet header to `skb` (`eth_header`).
pub fn push_header(
    skb: &mut SkBuff,
    destination: &[u8; ETH_ALEN],
    source: &[u8; ETH_ALEN],
    ethertype: u16,
) -> bool {
    let Some(header) = skb.push(ETH_HLEN) else {
        return false;
    };
    header[0..6].copy_from_slice(destination);
    header[6..12].copy_from_slice(source);
    header[12..14].copy_from_slice(&ethertype.to_be_bytes());
    skb.protocol = ethertype;
    true
}

/// Parse and strip the Ethernet header of a received frame, setting
/// `protocol` and `pkt_type` (`eth_type_trans`). Frames shorter than a header
/// are rejected.
pub fn type_trans(skb: &mut SkBuff, device_address: &[u8], promiscuous: bool) -> bool {
    if skb.len() < ETH_HLEN {
        return false;
    }
    let (destination, ethertype) = {
        let data = skb.data();
        let mut destination = [0u8; ETH_ALEN];
        destination.copy_from_slice(&data[0..6]);
        (destination, u16::from_be_bytes([data[12], data[13]]))
    };
    skb.pull(ETH_HLEN);
    skb.pkt_type = if is_broadcast(&destination) {
        PacketType::Broadcast
    } else if is_multicast(&destination) {
        PacketType::Multicast
    } else if destination[..] == *device_address {
        PacketType::Host
    } else if promiscuous {
        PacketType::OtherHost
    } else {
        // The NIC filter should have dropped it; deliver as foreign traffic.
        PacketType::OtherHost
    };
    // 802.3 length field (<= 1500) is not Ethernet II; mark it unrecognised.
    skb.protocol = if ethertype >= 0x0600 { ethertype } else { 0 };
    true
}
