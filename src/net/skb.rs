/*
 *
 *       src/net/skb.rs
 *       Socket buffer: contiguous packet storage with headroom and tailroom
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A deliberately linear `sk_buff`: one allocation holds headroom, payload and
//! tailroom, so protocol layers `push` headers in front on transmit and `pull`
//! them off on receive without copying. Scatter/gather fragments are not used;
//! drivers that need them map the linear area.

use alloc::vec::Vec;

/// Largest buffer a single skb may own. Matches the 64 KiB IPv4 datagram limit
/// plus link-layer headroom.
pub const MAX_SKB: usize = 65536 + 1024;

/// Default headroom: Ethernet (14) + VLAN (4) + IPv6 (40) + TCP with options
/// (60), rounded up to keep payload aligned.
pub const DEFAULT_HEADROOM: usize = 128;

/// `skb->pkt_type`, the classification `eth_type_trans` produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketType {
    Host,
    Broadcast,
    Multicast,
    OtherHost,
    Outgoing,
    Loopback,
}

/// `skb->ip_summed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checksum {
    /// Software must verify/compute the checksum.
    None,
    /// The device verified the received checksum.
    Unnecessary,
    /// Transmit: the device completes the checksum at `csum_start`/`csum_offset`.
    Partial,
}

pub struct SkBuff {
    buffer: Vec<u8>,
    head: usize,
    tail: usize,
    /// Ethertype in host byte order once the link layer has been parsed.
    pub protocol: u16,
    pub pkt_type: PacketType,
    pub checksum: Checksum,
    pub csum_start: u16,
    pub csum_offset: u16,
    /// Receiving or outgoing interface index; 0 when unset.
    pub ifindex: u32,
    pub mark: u32,
    pub priority: u32,
    pub timestamp_us: u64,
    /// Offset of the network header relative to the start of the data area at
    /// the time `mark_network_header` was called.
    network_offset: Option<usize>,
    transport_offset: Option<usize>,
}

impl SkBuff {
    /// Allocate an empty skb with `headroom` bytes in front and room for
    /// `capacity` payload bytes. Returns `None` for sizes above [`MAX_SKB`].
    pub fn alloc(headroom: usize, capacity: usize) -> Option<Self> {
        let total = headroom.checked_add(capacity)?;
        if total > MAX_SKB {
            return None;
        }
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(total).ok()?;
        buffer.resize(total, 0);
        Some(Self::from_parts(buffer, headroom, headroom))
    }

    /// Copy `data` into a fresh skb that has [`DEFAULT_HEADROOM`] in front.
    pub fn from_slice(data: &[u8]) -> Option<Self> {
        let mut skb = Self::alloc(DEFAULT_HEADROOM, data.len())?;
        skb.put(data.len())?.copy_from_slice(data);
        Some(skb)
    }

    fn from_parts(buffer: Vec<u8>, head: usize, tail: usize) -> Self {
        Self {
            buffer,
            head,
            tail,
            protocol: 0,
            pkt_type: PacketType::Host,
            checksum: Checksum::None,
            csum_start: 0,
            csum_offset: 0,
            ifindex: 0,
            mark: 0,
            priority: 0,
            timestamp_us: 0,
            network_offset: None,
            transport_offset: None,
        }
    }

    pub fn len(&self) -> usize {
        self.tail - self.head
    }

    pub fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    pub fn headroom(&self) -> usize {
        self.head
    }

    pub fn tailroom(&self) -> usize {
        self.buffer.len() - self.tail
    }

    /// Total allocation, the unit used for receive-buffer accounting
    /// (`skb->truesize`).
    pub fn truesize(&self) -> usize {
        self.buffer.len() + core::mem::size_of::<Self>()
    }

    pub fn data(&self) -> &[u8] {
        &self.buffer[self.head..self.tail]
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.buffer[self.head..self.tail]
    }

    /// Reserve headroom on an empty skb (`skb_reserve`).
    pub fn reserve(&mut self, bytes: usize) -> bool {
        if !self.is_empty() || bytes > self.tailroom() {
            return false;
        }
        self.head += bytes;
        self.tail += bytes;
        true
    }

    /// Append `bytes` at the tail and return the new area (`skb_put`).
    pub fn put(&mut self, bytes: usize) -> Option<&mut [u8]> {
        if bytes > self.tailroom() {
            return None;
        }
        let start = self.tail;
        self.tail += bytes;
        Some(&mut self.buffer[start..self.tail])
    }

    /// Prepend `bytes` in front of the data and return the new area (`skb_push`).
    pub fn push(&mut self, bytes: usize) -> Option<&mut [u8]> {
        if bytes > self.head {
            return None;
        }
        self.head -= bytes;
        let end = self.head + bytes;
        Some(&mut self.buffer[self.head..end])
    }

    /// Remove `bytes` from the front and return them (`skb_pull`).
    pub fn pull(&mut self, bytes: usize) -> Option<&[u8]> {
        if bytes > self.len() {
            return None;
        }
        let start = self.head;
        self.head += bytes;
        Some(&self.buffer[start..self.head])
    }

    /// Drop everything after `len` bytes (`skb_trim`).
    pub fn trim(&mut self, len: usize) {
        if len < self.len() {
            self.tail = self.head + len;
        }
    }

    /// Guarantee at least `headroom` bytes in front by reallocating when
    /// needed (`skb_cow_head` / `pskb_expand_head`).
    pub fn ensure_headroom(&mut self, headroom: usize) -> bool {
        if self.head >= headroom {
            return true;
        }
        let length = self.len();
        let Some(total) = headroom
            .checked_add(length)
            .and_then(|n| n.checked_add(self.tailroom()))
        else {
            return false;
        };
        if total > MAX_SKB {
            return false;
        }
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(total).is_err() {
            return false;
        }
        buffer.resize(total, 0);
        buffer[headroom..headroom + length].copy_from_slice(self.data());
        let shift = |offset: Option<usize>| offset.map(|o| o + headroom - self.head);
        self.network_offset = shift(self.network_offset);
        self.transport_offset = shift(self.transport_offset);
        self.buffer = buffer;
        self.head = headroom;
        self.tail = headroom + length;
        true
    }

    /// Deep copy with all metadata (`skb_copy`).
    pub fn duplicate(&self) -> Option<Self> {
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(self.buffer.len()).ok()?;
        buffer.extend_from_slice(&self.buffer);
        let mut copy = Self::from_parts(buffer, self.head, self.tail);
        copy.protocol = self.protocol;
        copy.pkt_type = self.pkt_type;
        copy.checksum = self.checksum;
        copy.csum_start = self.csum_start;
        copy.csum_offset = self.csum_offset;
        copy.ifindex = self.ifindex;
        copy.mark = self.mark;
        copy.priority = self.priority;
        copy.timestamp_us = self.timestamp_us;
        copy.network_offset = self.network_offset;
        copy.transport_offset = self.transport_offset;
        Some(copy)
    }

    /// Record that the network header starts at the current data pointer.
    pub fn mark_network_header(&mut self) {
        self.network_offset = Some(self.head);
    }

    pub fn mark_transport_header(&mut self) {
        self.transport_offset = Some(self.head);
    }

    /// Bytes from the network header to the tail.
    pub fn network_slice(&self) -> Option<&[u8]> {
        self.network_offset
            .map(|offset| &self.buffer[offset..self.tail])
    }

    pub fn transport_slice(&self) -> Option<&[u8]> {
        self.transport_offset
            .map(|offset| &self.buffer[offset..self.tail])
    }
}
