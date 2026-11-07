/*
 *
 *       src/net/ip.rs
 *       Internet address helpers and checksums shared by IPv4 and IPv6
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! `core::net` supplies the address types and formatting; this module adds the
//! pieces the kernel needs on top: protocol numbers, the Internet checksum
//! (RFC 1071) with pseudo-header seeds, and prefix masking.

pub use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub const IPPROTO_ICMP: u8 = 1;
pub const IPPROTO_IGMP: u8 = 2;
pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;
pub const IPPROTO_ICMPV6: u8 = 58;
pub const IPPROTO_RAW: u8 = 255;

pub const IPV4_MIN_MTU: u32 = 68;
pub const IPV6_MIN_MTU: u32 = 1280;

/// Accumulate 16-bit big-endian words into `seed` without folding.
pub fn sum16(seed: u32, data: &[u8]) -> u32 {
    let mut sum = seed;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
        if sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
    }
    if let [last] = chunks.remainder() {
        sum += u32::from(*last) << 8;
    }
    sum
}

/// Fold carries and return the one's complement, ready to store in a header.
pub fn checksum(seed: u32, data: &[u8]) -> u16 {
    let mut sum = sum16(seed, data);
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn pseudo_v4(source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, length: u16) -> u32 {
    let mut seed = sum16(0, &source.octets());
    seed = sum16(seed, &destination.octets());
    seed += u32::from(protocol);
    seed + u32::from(length)
}

pub fn pseudo_v6(source: Ipv6Addr, destination: Ipv6Addr, protocol: u8, length: u32) -> u32 {
    let mut seed = sum16(0, &source.octets());
    seed = sum16(seed, &destination.octets());
    seed += length >> 16;
    seed += length & 0xffff;
    seed + u32::from(protocol)
}

/// `addr` with the low `prefixlen` bits cleared.
pub fn mask_v4(address: Ipv4Addr, prefixlen: u8) -> Ipv4Addr {
    let bits = u32::from(address);
    let mask = if prefixlen == 0 {
        0
    } else {
        u32::MAX << (32 - prefixlen as u32)
    };
    Ipv4Addr::from(bits & mask)
}

pub fn mask_v6(address: Ipv6Addr, prefixlen: u8) -> Ipv6Addr {
    let bits = u128::from(address);
    let mask = if prefixlen == 0 {
        0
    } else {
        u128::MAX << (128 - prefixlen as u32)
    };
    Ipv6Addr::from(bits & mask)
}

/// `ipv4_is_lbcast`: the limited broadcast address.
pub fn is_limited_broadcast(address: Ipv4Addr) -> bool {
    address.octets() == [255, 255, 255, 255]
}

pub fn is_v4_multicast(address: Ipv4Addr) -> bool {
    address.is_multicast()
}

/// Map an IPv4 multicast group to its Ethernet MAC (`01:00:5e` + low 23 bits).
pub fn v4_multicast_mac(address: Ipv4Addr) -> [u8; 6] {
    let octets = address.octets();
    [0x01, 0x00, 0x5e, octets[1] & 0x7f, octets[2], octets[3]]
}

/// Map an IPv6 multicast group to `33:33` + low 32 bits.
pub fn v6_multicast_mac(address: Ipv6Addr) -> [u8; 6] {
    let octets = address.octets();
    [0x33, 0x33, octets[12], octets[13], octets[14], octets[15]]
}

/// Parse a dotted-quad prefix length in `0..=32`.
pub fn parse_v4_prefix(text: &str) -> Option<u8> {
    text.parse::<u8>().ok().filter(|value| *value <= 32)
}
