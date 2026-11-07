/*
 *
 *       src/net/ipv4/header.rs
 *       IPv4 header encoding, parsing and metadata
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::super::ip::{Ipv4Addr, checksum};
use crate::vfs::VfsError;
use alloc::vec::Vec;

pub const IPV4_HLEN: usize = 20;
pub const IPV4_MAX_HEADER: usize = 60;
pub const IPV4_MAX_OPTLEN: usize = 40;

pub const IP_CE: u8 = 0x01;
pub const IP_DF: u16 = 0x4000;
pub const IP_MF: u16 = 0x2000;
pub const IP_OFFSET_MASK: u16 = 0x1fff;

pub const IPOPT_END: u8 = 0;
pub const IPOPT_NOOP: u8 = 1;
pub const IPOPT_RR: u8 = 7;
pub const IPOPT_TIMESTAMP: u8 = 68;
pub const IPOPT_LSRR: u8 = 131;
pub const IPOPT_CIPSO: u8 = 134;
pub const IPOPT_SSRR: u8 = 137;
pub const IPOPT_RA: u8 = 148;

/// Parsed on-wire header.
#[derive(Clone, Copy, Debug)]
pub struct Ipv4Header {
    pub version: u8,
    pub ihl: u8,
    pub tos: u8,
    pub total_len: u16,
    pub id: u16,
    /// Flags and fragment offset as carried in the header.
    pub flags_offset: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16,
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
}

impl Ipv4Header {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < IPV4_HLEN {
            return None;
        }
        let version = bytes[0] >> 4;
        let ihl = (bytes[0] & 0x0f) * 4;
        if version != 4 || ihl < IPV4_HLEN as u8 || ihl as usize > bytes.len() {
            return None;
        }
        Some(Self {
            version,
            ihl,
            tos: bytes[1],
            total_len: u16::from_be_bytes([bytes[2], bytes[3]]),
            id: u16::from_be_bytes([bytes[4], bytes[5]]),
            flags_offset: u16::from_be_bytes([bytes[6], bytes[7]]),
            ttl: bytes[8],
            protocol: bytes[9],
            checksum: u16::from_be_bytes([bytes[10], bytes[11]]),
            source: Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]),
            destination: Ipv4Addr::new(bytes[16], bytes[17], bytes[18], bytes[19]),
        })
    }

    pub fn header_len(&self) -> usize {
        self.ihl as usize
    }

    pub fn dont_fragment(&self) -> bool {
        self.flags_offset & IP_DF != 0
    }

    pub fn more_fragments(&self) -> bool {
        self.flags_offset & IP_MF != 0
    }

    /// Fragment offset in bytes.
    pub fn fragment_offset(&self) -> usize {
        (self.flags_offset & IP_OFFSET_MASK) as usize * 8
    }

    pub fn is_fragment(&self) -> bool {
        self.more_fragments() || self.fragment_offset() != 0
    }

    /// Verify the header checksum.
    pub fn verify(&self, bytes: &[u8]) -> bool {
        checksum(0, &bytes[..self.header_len()]) == 0
    }

    pub fn write(
        &self,
        out: &mut [u8],
        total_len: u16,
        id: u16,
        flags_offset: u16,
        source: Ipv4Addr,
        destination: Ipv4Addr,
    ) {
        out[0] = 0x45;
        out[1] = self.tos;
        out[2..4].copy_from_slice(&total_len.to_be_bytes());
        out[4..6].copy_from_slice(&id.to_be_bytes());
        out[6..8].copy_from_slice(&flags_offset.to_be_bytes());
        out[8] = self.ttl;
        out[9] = self.protocol;
        out[10..12].copy_from_slice(&[0, 0]);
        out[12..16].copy_from_slice(&source.octets());
        out[16..20].copy_from_slice(&destination.octets());
        let sum = checksum(0, &out[..IPV4_HLEN]);
        out[10..12].copy_from_slice(&sum.to_be_bytes());
    }
}

/// Transport-visible description of a received datagram.
#[derive(Clone, Copy, Debug)]
pub struct IpMeta {
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub protocol: u8,
    pub tos: u8,
    pub ttl: u8,
    pub id: u16,
    pub header_len: usize,
    /// Total datagram length including the header.
    pub total_len: usize,
}

impl IpMeta {
    pub fn from_header(header: &Ipv4Header) -> Self {
        Self {
            source: header.source,
            destination: header.destination,
            protocol: header.protocol,
            tos: header.tos,
            ttl: header.ttl,
            id: header.id,
            header_len: header.header_len(),
            total_len: header.total_len as usize,
        }
    }

    /// Payload length after the IPv4 header.
    pub fn payload_len(&self) -> usize {
        self.total_len.saturating_sub(self.header_len)
    }
}

/// Header template used when building outgoing packets.
#[derive(Clone, Copy, Debug)]
pub struct TxTemplate {
    pub tos: u8,
    pub ttl: u8,
    pub protocol: u8,
    pub dont_fragment: bool,
}

impl Default for TxTemplate {
    fn default() -> Self {
        Self {
            tos: 0,
            ttl: 64,
            protocol: 0,
            dont_fragment: false,
        }
    }
}

/// Validate user-supplied `IP_OPTIONS` bytes (`ip_options_get` +
/// `__ip_options_compile` in `net/ipv4/ip_options.c`) and pad to a 4-byte
/// multiple with `IPOPT_END`.
pub fn validate_options(raw: &[u8]) -> Result<Vec<u8>, VfsError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.len() > IPV4_MAX_OPTLEN {
        return Err(VfsError::InvalidArgument);
    }
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            IPOPT_END => break,
            IPOPT_NOOP => {
                index += 1;
                continue;
            }
            code => {
                if index + 1 >= raw.len() {
                    return Err(VfsError::InvalidArgument);
                }
                let olen = raw[index + 1] as usize;
                if olen < 2 || index + olen > raw.len() {
                    return Err(VfsError::InvalidArgument);
                }
                match code {
                    IPOPT_RR | IPOPT_LSRR | IPOPT_SSRR => {
                        if olen < 3 || raw[index + 2] < 4 {
                            return Err(VfsError::InvalidArgument);
                        }
                    }
                    IPOPT_TIMESTAMP => {
                        if olen < 4 || raw[index + 2] < 5 {
                            return Err(VfsError::InvalidArgument);
                        }
                    }
                    IPOPT_RA => {
                        if olen != 4 {
                            return Err(VfsError::InvalidArgument);
                        }
                    }
                    _ => {}
                }
                index += olen;
            }
        }
    }
    let padded_len = (raw.len() + 3) & !3;
    if padded_len > IPV4_MAX_OPTLEN {
        return Err(VfsError::InvalidArgument);
    }
    let mut out = Vec::with_capacity(padded_len);
    out.extend_from_slice(raw);
    out.resize(padded_len, IPOPT_END);
    Ok(out)
}

/// Stamp outgoing per-hop fields into a 4-byte-aligned IPv4 options buffer
/// (`ip_options_build`: fills the next `IPOPT_RR` slot with `source`).
pub fn stamp_tx_options(opts: &mut [u8], source: Ipv4Addr) {
    let mut index = 0;
    while index < opts.len() {
        match opts[index] {
            IPOPT_END => break,
            IPOPT_NOOP => {
                index += 1;
            }
            code => {
                if index + 1 >= opts.len() {
                    break;
                }
                let olen = opts[index + 1] as usize;
                if olen < 2 || index + olen > opts.len() {
                    break;
                }
                if code == IPOPT_RR && olen >= 3 {
                    let ptr = opts[index + 2] as usize;
                    if ptr >= 4 && ptr - 1 + 4 <= olen {
                        let slot = index + ptr - 1;
                        opts[slot..slot + 4].copy_from_slice(&source.octets());
                        opts[index + 2] = (ptr + 4) as u8;
                    }
                }
                index += olen;
            }
        }
    }
}

/// Build echoed IPv4 options (`__ip_options_echo` in `net/ipv4/ip_options.c`).
/// When `undo` is true (`ip_options_undo`, used by `ip_cmsg_recv_retopts` for
/// `IP_RETOPTS`), any freshly reserved RR/TS slot is left unadvanced so the
/// caller sees the received option state ready for retransmission; when `undo`
/// is false (used by `icmp_echo`), `stamp_tx_options` will fill our local IP.
fn echo_options_inner(raw: &[u8], orig_source: Ipv4Addr) -> Result<Vec<u8>, ()> {
    if raw.is_empty() || raw.len() > IPV4_MAX_OPTLEN {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen_rr = false;
    let mut seen_ts = false;
    let mut seen_srr = false;
    let mut seen_cipso = false;
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            IPOPT_END => break,
            IPOPT_NOOP => {
                index += 1;
            }
            code => {
                if index + 1 >= raw.len() {
                    return Err(());
                }
                let olen = raw[index + 1] as usize;
                if olen < 2 || index + olen > raw.len() {
                    return Err(());
                }
                let slice = &raw[index..index + olen];
                match code {
                    IPOPT_RR if !seen_rr => {
                        if olen < 3 || slice[2] < 4 {
                            return Err(());
                        }
                        let soffset = slice[2] as usize;
                        if soffset <= olen && soffset + 3 > olen {
                            return Err(());
                        }
                        if out.len() + olen <= IPV4_MAX_OPTLEN {
                            out.extend_from_slice(slice);
                            seen_rr = true;
                        }
                    }
                    IPOPT_TIMESTAMP if !seen_ts => {
                        if olen < 4 || slice[2] < 5 {
                            return Err(());
                        }
                        let soffset = slice[2] as usize;
                        if soffset <= olen && soffset + 3 > olen {
                            return Err(());
                        }
                        if out.len() + olen <= IPV4_MAX_OPTLEN {
                            out.extend_from_slice(slice);
                            seen_ts = true;
                        }
                    }
                    IPOPT_LSRR | IPOPT_SSRR if !seen_srr => {
                        if olen < 3 || slice[2] < 4 {
                            return Err(());
                        }
                        let mut soffset = slice[2] as usize;
                        if soffset > olen {
                            soffset = olen + 1;
                        }
                        soffset = soffset.saturating_sub(4);
                        if soffset > 3 {
                            let mut reversed = Vec::new();
                            let mut cur = soffset;
                            while cur > 7 {
                                cur -= 4;
                                reversed.extend_from_slice(&slice[cur - 1..cur + 3]);
                            }
                            if reversed.len() >= 4
                                && reversed[reversed.len() - 4..] == orig_source.octets()
                            {
                                reversed.truncate(reversed.len() - 4);
                            } else {
                                reversed.extend_from_slice(&orig_source.octets());
                            }
                            let total_srr = 3 + reversed.len();
                            if !reversed.is_empty() && out.len() + total_srr <= IPV4_MAX_OPTLEN {
                                out.push(code);
                                out.push(total_srr as u8);
                                out.push(4);
                                out.extend_from_slice(&reversed);
                                seen_srr = true;
                            }
                        }
                    }
                    IPOPT_CIPSO if !seen_cipso => {
                        if out.len() + olen <= IPV4_MAX_OPTLEN {
                            out.extend_from_slice(slice);
                            seen_cipso = true;
                        }
                    }
                    _ => {}
                }
                index += olen;
            }
        }
    }
    while out.len() & 3 != 0 {
        out.push(IPOPT_END);
    }
    Ok(out)
}

/// Echo incoming IPv4 header options onto an outgoing `ICMP_ECHOREPLY`.
pub fn echo_options_for_reply(raw: &[u8], orig_source: Ipv4Addr) -> Vec<u8> {
    echo_options_inner(raw, orig_source).unwrap_or_default()
}

/// Compute the `SOL_IP` / `IP_RETOPTS` control message payload
/// (`ip_cmsg_recv_retopts` in `net/ipv4/ip_sockglue.c`).
pub fn retopts_for_cmsg(raw: &[u8], orig_source: Ipv4Addr) -> Result<Vec<u8>, ()> {
    echo_options_inner(raw, orig_source)
}
