/*
 *
 *       src/net/ipv6/header.rs
 *       IPv6 header encoding, parsing, extension headers and metadata
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::super::ip::Ipv6Addr;

pub const IPV6_HLEN: usize = 40;
pub const FRAG_HLEN: usize = 8;

pub const NEXTHDR_HOP: u8 = 0;
pub const NEXTHDR_ROUTING: u8 = 43;
pub const NEXTHDR_FRAGMENT: u8 = 44;
pub const NEXTHDR_NONE: u8 = 59;
pub const NEXTHDR_DEST: u8 = 60;

pub const IPV6_MF: u16 = 0x0001;
pub const IPV6_OFFSET_MASK: u16 = 0xfff8;

/// Parsed 40-byte fixed IPv6 header.
#[derive(Clone, Copy, Debug)]
pub struct Ipv6Header {
    pub version: u8,
    pub traffic_class: u8,
    pub flow_label: u32,
    pub payload_len: u16,
    pub next_header: u8,
    pub hop_limit: u8,
    pub source: Ipv6Addr,
    pub destination: Ipv6Addr,
}

impl Ipv6Header {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < IPV6_HLEN {
            return None;
        }
        let vtf = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let version = (vtf >> 28) as u8;
        if version != 6 {
            return None;
        }
        let traffic_class = ((vtf >> 20) & 0xff) as u8;
        let flow_label = vtf & 0x000f_ffff;
        let payload_len = u16::from_be_bytes([bytes[4], bytes[5]]);
        let next_header = bytes[6];
        let hop_limit = bytes[7];
        let mut src = [0u8; 16];
        let mut dst = [0u8; 16];
        src.copy_from_slice(&bytes[8..24]);
        dst.copy_from_slice(&bytes[24..40]);
        Some(Self {
            version,
            traffic_class,
            flow_label,
            payload_len,
            next_header,
            hop_limit,
            source: Ipv6Addr::from(src),
            destination: Ipv6Addr::from(dst),
        })
    }

    pub fn write(&self, out: &mut [u8]) {
        let vtf =
            (6u32 << 28) | (u32::from(self.traffic_class) << 20) | (self.flow_label & 0x000f_ffff);
        out[0..4].copy_from_slice(&vtf.to_be_bytes());
        out[4..6].copy_from_slice(&self.payload_len.to_be_bytes());
        out[6] = self.next_header;
        out[7] = self.hop_limit;
        out[8..24].copy_from_slice(&self.source.octets());
        out[24..40].copy_from_slice(&self.destination.octets());
    }
}

/// Parsed 8-byte IPv6 Fragment Extension Header (`NEXTHDR_FRAGMENT = 44`).
#[derive(Clone, Copy, Debug)]
pub struct FragHeader {
    pub next_header: u8,
    pub flags_offset: u16,
    pub id: u32,
}

impl FragHeader {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < FRAG_HLEN {
            return None;
        }
        Some(Self {
            next_header: bytes[0],
            flags_offset: u16::from_be_bytes([bytes[2], bytes[3]]),
            id: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }

    pub fn fragment_offset(&self) -> usize {
        (self.flags_offset & IPV6_OFFSET_MASK) as usize
    }

    pub fn more_fragments(&self) -> bool {
        self.flags_offset & IPV6_MF != 0
    }
}

/// Transport-visible description of a received IPv6 packet.
#[derive(Clone, Copy, Debug)]
pub struct Ip6Meta {
    pub source: Ipv6Addr,
    pub destination: Ipv6Addr,
    pub protocol: u8,
    pub traffic_class: u8,
    pub hop_limit: u8,
    pub header_len: usize,
    pub total_len: usize,
}

impl Ip6Meta {
    pub fn payload_len(&self) -> usize {
        self.total_len.saturating_sub(self.header_len)
    }
}

/// Header template used when building outgoing IPv6 packets.
#[derive(Clone, Copy, Debug)]
pub struct Tx6Template {
    pub traffic_class: u8,
    pub flow_label: u32,
    pub hop_limit: u8,
    pub protocol: u8,
}

impl Default for Tx6Template {
    fn default() -> Self {
        Self {
            traffic_class: 0,
            flow_label: 0,
            hop_limit: 64,
            protocol: 0,
        }
    }
}
