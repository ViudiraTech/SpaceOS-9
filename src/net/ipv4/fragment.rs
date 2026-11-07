/*
 *
 *       src/net/ipv4/fragment.rs
 *       IPv4 fragmentation on output and reassembly on input
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::NetDevice,
        ip::Ipv4Addr,
        skb::{DEFAULT_HEADROOM, SkBuff},
    },
    header::{IP_MF, IPV4_HLEN, Ipv4Header, TxTemplate},
};
use crate::{net::timer, sync::SpinLock, vfs::VfsError};
use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

const REASSEMBLY_TIMEOUT_US: u64 = 30_000_000;
const MAX_REASSEMBLY_BYTES: usize = 4 * 1024 * 1024;
const MAX_DATAGRAM: usize = u16::MAX as usize;

struct Entry {
    source: Ipv4Addr,
    destination: Ipv4Addr,
    id: u16,
    protocol: u8,
    tos: u8,
    ttl: u8,
    fragments: BTreeMap<usize, Vec<u8>>,
    total: Option<usize>,
    bytes: usize,
    deadline_us: u64,
}

impl Entry {
    fn matches(&self, header: &Ipv4Header) -> bool {
        self.source == header.source
            && self.destination == header.destination
            && self.id == header.id
            && self.protocol == header.protocol
    }

    /// Whether the fragments cover `0..total` with no hole.
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
        let mut skb = SkBuff::alloc(DEFAULT_HEADROOM, IPV4_HLEN + total)?;
        let header = skb.put(IPV4_HLEN)?;
        let template = Ipv4Header {
            version: 4,
            ihl: IPV4_HLEN as u8,
            tos: self.tos,
            total_len: (IPV4_HLEN + total) as u16,
            id: self.id,
            flags_offset: 0,
            ttl: self.ttl,
            protocol: self.protocol,
            checksum: 0,
            source: self.source,
            destination: self.destination,
        };
        template.write(
            header,
            (IPV4_HLEN + total) as u16,
            self.id,
            0,
            self.source,
            self.destination,
        );
        let payload = skb.put(total)?;
        let mut cursor = 0;
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

fn purge_expired(table: &mut Vec<Entry>) {
    let now = crate::time::uptime_micros();
    table.retain(|entry| entry.deadline_us > now);
}

/// `ip_defrag`: feed one fragment; returns the complete packet once every byte
/// has arrived. Overlapping fragments are discarded (first copy wins).
pub fn consume(_dev: &Arc<NetDevice>, skb: SkBuff, header: &Ipv4Header) -> Option<SkBuff> {
    let offset = header.fragment_offset();
    let start = header.header_len();
    let end = header.total_len as usize;
    if end < start || end > skb.len() {
        return None;
    }
    let payload = &skb.data()[start..end];
    let length = payload.len();
    if length == 0 && !header.more_fragments() {
        return None;
    }
    let end_offset = offset.checked_add(length)?;
    if end_offset > MAX_DATAGRAM {
        return None;
    }
    let assembled = {
        let mut table = TABLE.lock();
        purge_expired(&mut table);
        let position = table.iter().position(|entry| entry.matches(header));
        let index = match position {
            Some(index) => index,
            None => {
                table.push(Entry {
                    source: header.source,
                    destination: header.destination,
                    id: header.id,
                    protocol: header.protocol,
                    tos: header.tos,
                    ttl: header.ttl,
                    fragments: BTreeMap::new(),
                    total: None,
                    bytes: 0,
                    deadline_us: crate::time::uptime_micros() + REASSEMBLY_TIMEOUT_US,
                });
                table.len() - 1
            }
        };
        let mut assembled = None;
        let mut complete = false;
        let mut invalid = false;
        {
            let entry = &mut table[index];
            if !header.more_fragments() {
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

/// Fragments waiting for completion (leak check).
pub fn pending_datagrams() -> usize {
    TABLE.lock().len()
}

/// `ip_fragment`: split `payload` so each fragment plus its header fits `mtu`.
pub fn send(
    dev: &Arc<NetDevice>,
    source: Ipv4Addr,
    destination: Ipv4Addr,
    next_hop: Ipv4Addr,
    template: &TxTemplate,
    id: u16,
    payload: SkBuff,
    mtu: usize,
) -> Result<(), VfsError> {
    let capacity = mtu.saturating_sub(IPV4_HLEN) & !7;
    if capacity == 0 || payload.len() > MAX_DATAGRAM {
        return Err(VfsError::MessageTooLong);
    }
    let total = payload.len();
    let bytes = payload.data();
    let mut offset = 0usize;
    while offset < total {
        let length = capacity.min(total - offset);
        let last = offset + length == total;
        let mut fragment = SkBuff::alloc(DEFAULT_HEADROOM, length).ok_or(VfsError::NoBuffers)?;
        let body = fragment.put(length).ok_or(VfsError::NoBuffers)?;
        body.copy_from_slice(&bytes[offset..offset + length]);
        let mut flags = (offset / 8) as u16;
        if !last {
            flags |= IP_MF;
        }
        super::output::send_fragment(
            dev,
            source,
            destination,
            next_hop,
            template,
            id,
            flags,
            fragment,
        )?;
        offset += length;
    }
    Ok(())
}
