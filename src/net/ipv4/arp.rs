/*
 *
 *       src/net/ipv4/arp.rs
 *       Address Resolution Protocol
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::{self, ARPHRD_ETHER, NetDevice},
        ether,
        ip::{IpAddr, Ipv4Addr},
        neigh::{self, NUD_REACHABLE},
        skb::SkBuff,
    },
    addr,
};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

const ARP_REQUEST: u16 = 1;
const ARP_REPLY: u16 = 2;
const ARP_PACKET_LEN: usize = 28;

/// Send a gratuitous ARP announcing `address`, called after address
/// configuration on a broadcast link.
pub fn announce(dev: &Arc<NetDevice>, address: Ipv4Addr) {
    if dev.dev_type != ARPHRD_ETHER {
        return;
    }
    let _ = send_frame(
        dev,
        ether::BROADCAST,
        ARP_REQUEST,
        address,
        &[0u8; 6],
        address,
    );
}

static REQUESTS_SENT: AtomicU64 = AtomicU64::new(0);
static REPLIES_SENT: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    dev::register_packet_type(ether::ETH_P_ARP, receive);
    neigh::register_family(neigh::AF_INET, solicit);
}

pub fn requests_sent() -> u64 {
    REQUESTS_SENT.load(Ordering::Relaxed)
}

pub fn replies_sent() -> u64 {
    REPLIES_SENT.load(Ordering::Relaxed)
}

fn build(
    operation: u16,
    sender_hardware: &[u8],
    sender_address: Ipv4Addr,
    target_hardware: &[u8],
    target_address: Ipv4Addr,
) -> Option<SkBuff> {
    if sender_hardware.len() != 6 || target_hardware.len() != 6 {
        return None;
    }
    let mut skb = SkBuff::alloc(super::super::skb::DEFAULT_HEADROOM, ARP_PACKET_LEN)?;
    let packet = skb.put(ARP_PACKET_LEN)?;
    packet[0..2].copy_from_slice(&(ARPHRD_ETHER).to_be_bytes());
    packet[2..4].copy_from_slice(&ether::ETH_P_IP.to_be_bytes());
    packet[4] = 6;
    packet[5] = 4;
    packet[6..8].copy_from_slice(&operation.to_be_bytes());
    packet[8..14].copy_from_slice(sender_hardware);
    packet[14..18].copy_from_slice(&sender_address.octets());
    packet[18..24].copy_from_slice(target_hardware);
    packet[24..28].copy_from_slice(&target_address.octets());
    Some(skb)
}

fn send_frame(
    dev: &Arc<NetDevice>,
    destination: [u8; 6],
    operation: u16,
    sender_address: Ipv4Addr,
    target_hardware: &[u8],
    target_address: Ipv4Addr,
) -> bool {
    if dev.dev_type != ARPHRD_ETHER {
        return false;
    }
    let hardware = dev.hw_addr();
    let Some(mut skb) = build(
        operation,
        &hardware,
        sender_address,
        target_hardware,
        target_address,
    ) else {
        return false;
    };
    let mut source = [0u8; 6];
    if hardware.len() == 6 {
        source.copy_from_slice(&hardware);
    }
    if !ether::push_header(&mut skb, &destination, &source, ether::ETH_P_ARP) {
        return false;
    }
    dev::queue_xmit(dev, skb).is_ok()
}

/// `arp_solicit`: broadcaster who owns `target` is asked for its hardware
/// address.
fn solicit(dev: &Arc<NetDevice>, target: IpAddr) {
    let IpAddr::V4(target) = target else {
        return;
    };
    if dev.dev_type != ARPHRD_ETHER {
        return;
    }
    let source = addr::first_on(dev.ifindex()).unwrap_or(Ipv4Addr::UNSPECIFIED);
    if send_frame(
        dev,
        ether::BROADCAST,
        ARP_REQUEST,
        source,
        &[0u8; 6],
        target,
    ) {
        REQUESTS_SENT.fetch_add(1, Ordering::Relaxed);
    }
}

/// `arp_process`.
pub fn receive(dev: &Arc<NetDevice>, mut skb: SkBuff) {
    if dev.dev_type != ARPHRD_ETHER || skb.len() < ARP_PACKET_LEN {
        return;
    }
    let data = skb.data();
    let hardware_type = u16::from_be_bytes([data[0], data[1]]);
    let protocol_type = u16::from_be_bytes([data[2], data[3]]);
    let hardware_len = data[4] as usize;
    let protocol_len = data[5] as usize;
    let operation = u16::from_be_bytes([data[6], data[7]]);
    if hardware_type != ARPHRD_ETHER
        || protocol_type != ether::ETH_P_IP
        || hardware_len != 6
        || protocol_len != 4
    {
        return;
    }
    let mut sender_hardware = [0u8; 6];
    sender_hardware.copy_from_slice(&data[8..14]);
    let sender_address = Ipv4Addr::new(data[14], data[15], data[16], data[17]);
    let mut target_hardware = [0u8; 6];
    target_hardware.copy_from_slice(&data[18..24]);
    let target_address = Ipv4Addr::new(data[24], data[25], data[26], data[27]);
    skb.trim(ARP_PACKET_LEN);

    let _ = skb;
    if !sender_address.is_unspecified()
        && !sender_address.is_multicast()
        && !sender_address.is_broadcast()
    {
        neigh::update(
            dev,
            neigh::AF_INET,
            IpAddr::V4(sender_address),
            &sender_hardware,
            NUD_REACHABLE,
        );
    }
    if operation != ARP_REQUEST {
        return;
    }
    // Reply only for an address that is genuinely ours. `is_local` consults
    // the configured address list, which includes addresses of other
    // interfaces (Linux's weak host model).
    if !addr::is_local(target_address) {
        return;
    }
    if send_frame(
        dev,
        sender_hardware,
        ARP_REPLY,
        target_address,
        &sender_hardware,
        sender_address,
    ) {
        REPLIES_SENT.fetch_add(1, Ordering::Relaxed);
    }
}
