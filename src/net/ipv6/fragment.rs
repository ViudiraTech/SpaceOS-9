/*
 *
 *       src/net/ipv6/fragment.rs
 *       IPv6 fragmentation on output and reassembly on input (RFC 8200)
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::NetDevice,
        ip::Ipv6Addr,
        skb::{DEFAULT_HEADROOM, SkBuff},
    },
    header::{
        FRAG_HLEN, FragHeader, IPV6_HLEN, IPV6_MF, Ipv6Header, NEXTHDR_FRAGMENT, Tx6Template,
    },
};
use crate::{net::timer, sync::SpinLock, vfs::VfsError};
use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

const REASSEMBLY_TIMEOUT_US: u64 = 30_000_000;
const MAX_REASSEMBLY_BYTES: usize = 4 * 1024 * 1024;
const MAX_PAYLOAD: usize = u16::MAX as usize;

struct Entry {
    source: Ipv6Addr,
    destination: Ipv6Addr,
    id: u32,
    next_header: u8,
    traffic_class: u8,
    flow_label: u32,
    hop_limit: u8,
    fragments: BTreeMap<usize, Vec<u8>>,
    total: Option<usize>,
    bytes: usize,
    deadline_us: u64,
}

impl Entry {
    fn matches(&self, header: &Ipv6Header, frag: &FragHeader) -> bool {
        self.source == header.source
            && self.destination == header.destination
            && self.id == frag.id
            && self.next_header == frag.next_header
    }

    fn complete(&self) -> Option<usize> {
        let total = self.total?;
        let mut cursor = 0usize;
        for (offset, data) in &self.fragments {
            if *offset != cursor {
                return None;
            }
            cursor += data.len();
        }
        (cursor == total).then_some(total)
    }

    fn assemble(&self) -> Option<SkBuff> {
        let total = self.complete()?;
        if total > MAX_PAYLOAD {
            return None;
        }
        let mut skb = SkBuff::alloc(DEFAULT_HEADROOM, IPV6_HLEN + total)?;
        let header_slice = skb.put(IPV6_HLEN)?;
        let header = Ipv6Header {
            version: 6,
            traffic_class: self.traffic_class,
            flow_label: self.flow_label,
            payload_len: total as u16,
            next_header: self.next_header,
            hop_limit: self.hop_limit,
            source: self.source,
            destination: self.destination,
        };
        header.write(header_slice);
        let payload = skb.put(total)?;
        let mut cursor = 0usize;
        for data in self.fragments.values() {
            payload[cursor..cursor + data.len()].copy_from_slice(data);
            cursor += data.len();
        }
        skb.mark_network_header();
        Some(skb)
    }
}

static TABLE: SpinLock<Vec<Entry>> = SpinLock::new(Vec::new());
static ARMED: AtomicBool = AtomicBool::new(false);

fn arm() {
    if !ARMED.swap(true, Ordering::AcqRel) {
        timer::schedule_in(REASSEMBLY_TIMEOUT_US / 2, Box::new(gc));
    }
}

fn gc() {
    ARMED.store(false, Ordering::Release);
    let now = crate::time::uptime_micros();
    {
        let mut table = TABLE.lock();
        table.retain(|entry| entry.deadline_us > now);
    }
    if !TABLE.lock().is_empty() {
        arm();
    }
}

pub fn consume(
    _dev: &Arc<NetDevice>,
    skb: SkBuff,
    header: &Ipv6Header,
    frag_offset_in_skb: usize,
) -> Option<SkBuff> {
    let total_end = IPV6_HLEN + header.payload_len as usize;
    if total_end > skb.len() || frag_offset_in_skb + FRAG_HLEN > total_end {
        return None;
    }
    let frag = FragHeader::parse(&skb.data()[frag_offset_in_skb..total_end])?;
    let payload = &skb.data()[frag_offset_in_skb + FRAG_HLEN..total_end];
    let offset = frag.fragment_offset();
    let length = payload.len();
    if length == 0 && !frag.more_fragments() {
        return None;
    }
    let end_offset = offset.checked_add(length)?;
    if end_offset > MAX_PAYLOAD {
        return None;
    }
    let assembled = {
        let mut table = TABLE.lock();
        let now = crate::time::uptime_micros();
        table.retain(|entry| entry.deadline_us > now);
        let index = match table.iter().position(|entry| entry.matches(header, &frag)) {
            Some(index) => index,
            None => {
                table.push(Entry {
                    source: header.source,
                    destination: header.destination,
                    id: frag.id,
                    next_header: frag.next_header,
                    traffic_class: header.traffic_class,
                    flow_label: header.flow_label,
                    hop_limit: header.hop_limit,
                    fragments: BTreeMap::new(),
                    total: None,
                    bytes: 0,
                    deadline_us: now + REASSEMBLY_TIMEOUT_US,
                });
                table.len() - 1
            }
        };
        let mut assembled = None;
        let mut complete = false;
        let mut invalid = false;
        {
            let entry = &mut table[index];
            if !frag.more_fragments() {
                match entry.total {
                    Some(existing) if existing != end_offset => invalid = true,
                    _ => entry.total = Some(end_offset),
                }
            }
            if !invalid {
                let overlaps = entry.fragments.iter().any(|(existing_offset, existing)| {
                    let existing_end = existing_offset + existing.len();
                    offset < existing_end && *existing_offset < end_offset
                });
                if overlaps && !entry.fragments.is_empty() {
                    invalid = true;
                } else if entry.bytes + length > MAX_REASSEMBLY_BYTES {
                    invalid = true;
                } else {
                    entry.bytes += length;
                    entry.fragments.insert(offset, payload.to_vec());
                    assembled = entry.assemble();
                    complete = assembled.is_some();
                }
            }
        }
        if invalid {
            table.remove(index);
            return None;
        }
        if complete {
            table.remove(index);
        }
        assembled
    };
    if assembled.is_none() {
        arm();
    }
    assembled
}

pub fn send(
    dev: &Arc<NetDevice>,
    source: Ipv6Addr,
    destination: Ipv6Addr,
    next_hop: Ipv6Addr,
    template: &Tx6Template,
    id: u32,
    payload: SkBuff,
    mtu: usize,
) -> Result<(), VfsError> {
    let capacity = mtu.saturating_sub(IPV6_HLEN + FRAG_HLEN) & !7;
    if capacity == 0 || payload.len() > MAX_PAYLOAD {
        return Err(VfsError::MessageTooLong);
    }
    let total = payload.len();
    let bytes = payload.data();
    let mut offset = 0usize;
    while offset < total {
        let length = capacity.min(total - offset);
        let last = offset + length == total;
        let mut fragment =
            SkBuff::alloc(DEFAULT_HEADROOM, FRAG_HLEN + length).ok_or(VfsError::NoBuffers)?;
        let body = fragment
            .put(FRAG_HLEN + length)
            .ok_or(VfsError::NoBuffers)?;
        let mut flags_offset = (offset as u16) & !7;
        if !last {
            flags_offset |= IPV6_MF;
        }
        body[0] = template.protocol;
        body[1] = 0;
        body[2..4].copy_from_slice(&flags_offset.to_be_bytes());
        body[4..8].copy_from_slice(&id.to_be_bytes());
        body[8..8 + length].copy_from_slice(&bytes[offset..offset + length]);
        let frag_template = Tx6Template {
            traffic_class: template.traffic_class,
            flow_label: template.flow_label,
            hop_limit: template.hop_limit,
            protocol: NEXTHDR_FRAGMENT,
        };
        super::output::send_fragment(dev, source, destination, next_hop, &frag_template, fragment)?;
        offset += length;
    }
    Ok(())
}
