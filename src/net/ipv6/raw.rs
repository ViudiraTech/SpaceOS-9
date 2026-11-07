/*
 *
 *       src/net/ipv6/raw.rs
 *       Raw IPv6 sockets (AF_INET6 SOCK_RAW), used by ping6 and ICMPv6 tools
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::NetDevice,
        fib,
        ip::{self, IpAddr, Ipv6Addr},
        skb::{DEFAULT_HEADROOM, MAX_SKB, SkBuff},
        socket::{
            self, AF_INET6, CmsgItem, InetReceived, SO_ACCEPTCONN, SO_BINDTODEVICE, SO_BROADCAST,
            SO_DEBUG, SO_DOMAIN, SO_DONTROUTE, SO_ERROR, SO_KEEPALIVE, SO_MARK, SO_PRIORITY,
            SO_PROTOCOL, SO_RCVBUF, SO_RCVBUFFORCE, SO_RCVTIMEO_NEW, SO_RCVTIMEO_OLD, SO_REUSEADDR,
            SO_REUSEPORT, SO_SNDBUF, SO_SNDBUFFORCE, SO_SNDTIMEO_NEW, SO_SNDTIMEO_OLD,
            SO_TIMESTAMP_NEW, SO_TIMESTAMP_OLD, SO_TIMESTAMPNS_NEW, SO_TIMESTAMPNS_OLD, SO_TYPE,
            SOCK_DGRAM, SOCK_RAW, SOL_ICMPV6, SOL_IPV6, SOL_RAW, SOL_SOCKET, TimestampOpt,
            encode_timeval_us, opt_int_or_byte, parse_timeval_us,
        },
    },
    addr,
    header::{Ip6Meta, Tx6Template},
    icmpv6, output,
};
use crate::{
    sched,
    sync::SpinLock,
    vfs::{self, Inode, InodeKind, OpenFile, Stat, VfsError},
};
use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

const DEFAULT_BUFFER: usize = 212_992;
const MIN_BUFFER: usize = 4_608;
const OVERHEAD: usize = 256;

const IPV6_2292PKTINFO: u64 = 2;
const IPV6_CHECKSUM: u64 = 7;
const IPV6_2292HOPLIMIT: u64 = 8;
const IPV6_UNICAST_HOPS: u64 = 16;
const IPV6_MULTICAST_IF: u64 = 17;
const IPV6_MULTICAST_HOPS: u64 = 18;
const IPV6_MULTICAST_LOOP: u64 = 19;
const IPV6_ADD_MEMBERSHIP: u64 = 20;
const IPV6_DROP_MEMBERSHIP: u64 = 21;
const IPV6_MTU_DISCOVER: u64 = 23;
const IPV6_MTU: u64 = 24;
const IPV6_RECVERR: u64 = 25;
const IPV6_V6ONLY: u64 = 26;
const IPV6_RECVPKTINFO: u64 = 49;
const IPV6_PKTINFO: u64 = 50;
const IPV6_RECVHOPLIMIT: u64 = 51;
const IPV6_HOPLIMIT: u64 = 52;
const IPV6_DONTFRAG: u64 = 62;
const IPV6_RECVTCLASS: u64 = 66;
const IPV6_TCLASS: u64 = 67;
const IPV6_AUTOFLOWLABEL: u64 = 70;
const IPV6_ADDR_PREFERENCES: u64 = 72;
const ICMP6_FILTER: u64 = 1;

const RX6_PKTINFO: u32 = 1 << 0;
const RX6_2292PKTINFO: u32 = 1 << 1;
const RX6_HOPLIMIT: u32 = 1 << 2;
const RX6_2292HOPLIMIT: u32 = 1 << 3;
const RX6_TCLASS: u32 = 1 << 4;

struct Datagram {
    packet: Vec<u8>,
    source: Ipv6Addr,
    destination: Ipv6Addr,
    ifindex: u32,
    hop_limit: u8,
    traffic_class: u8,
    timestamp_us: u64,
}

struct State {
    queue: VecDeque<Datagram>,
    rmem: usize,
    rcvbuf: usize,
    sndbuf: usize,
    error: Option<i32>,
    rxopt: u32,
    timestamp: TimestampOpt,
    icmp6_filter: [u32; 8],
    tclass: u8,
    mc_hops: u8,
    mc_loop: bool,
    mc_if: u32,
    pmtudisc: u8,
    recverr: bool,
    v6only: bool,
    dontfrag: bool,
    autoflowlabel: bool,
    addr_prefs: u32,
    csum_offset: i32,
    reuseaddr: bool,
    reuseport: bool,
    broadcast: bool,
    dontroute: bool,
    keepalive: bool,
    sndtimeo_us: u64,
    mark: u32,
    priority: u32,
}

struct Raw6Inner {
    ino: u64,
    protocol: u8,
    is_ping: bool,
    ident: AtomicU16,
    hop_limit: AtomicU32,
    rcvtimeo_us: AtomicU64,
    nonblock: AtomicBool,
    bound_address: SpinLock<Option<Ipv6Addr>>,
    bound_ifindex: AtomicU32,
    peer: SpinLock<Option<(Ipv6Addr, u16, u32)>>,
    state: SpinLock<State>,
}

pub struct Raw6Socket {
    inner: Arc<Raw6Inner>,
}

static SOCKETS: SpinLock<Vec<Weak<Raw6Inner>>> = SpinLock::new(Vec::new());
static DELIVERED: AtomicU64 = AtomicU64::new(0);
static NEXT_PING6_IDENT: AtomicU16 = AtomicU16::new(1);

fn privileged() -> bool {
    sched::current().is_none_or(|task| task.credentials().euid == 0)
}

fn alloc_ping6_ident() -> u16 {
    loop {
        let id = NEXT_PING6_IDENT.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

impl Raw6Socket {
    pub fn create(protocol: u8, nonblock: bool) -> Result<Arc<Self>, VfsError> {
        if !privileged() {
            return Err(VfsError::NotPermitted);
        }
        Self::new_inner(protocol, false, nonblock)
    }

    /// Unprivileged `AF_INET6`/`SOCK_DGRAM`/`IPPROTO_ICMPV6`.
    pub fn create_ping(nonblock: bool) -> Result<Arc<Self>, VfsError> {
        Self::new_inner(ip::IPPROTO_ICMPV6, true, nonblock)
    }

    fn new_inner(protocol: u8, is_ping: bool, nonblock: bool) -> Result<Arc<Self>, VfsError> {
        let inner = Arc::new(Raw6Inner {
            ino: vfs::tree::ino(),
            protocol,
            is_ping,
            ident: AtomicU16::new(0),
            hop_limit: AtomicU32::new(64),
            rcvtimeo_us: AtomicU64::new(0),
            nonblock: AtomicBool::new(nonblock),
            bound_address: SpinLock::new(None),
            bound_ifindex: AtomicU32::new(0),
            peer: SpinLock::new(None),
            state: SpinLock::new(State {
                queue: VecDeque::new(),
                rmem: 0,
                rcvbuf: DEFAULT_BUFFER,
                sndbuf: DEFAULT_BUFFER,
                error: None,
                rxopt: 0,
                timestamp: TimestampOpt::Off,
                icmp6_filter: [0; 8],
                tclass: 0,
                mc_hops: 1,
                mc_loop: true,
                mc_if: 0,
                pmtudisc: 1,
                recverr: false,
                v6only: false,
                dontfrag: false,
                autoflowlabel: true,
                addr_prefs: 0x0100,
                csum_offset: if protocol == ip::IPPROTO_ICMPV6 {
                    2
                } else {
                    -1
                },
                reuseaddr: false,
                reuseport: false,
                broadcast: false,
                dontroute: false,
                keepalive: false,
                sndtimeo_us: 0,
                mark: 0,
                priority: 0,
            }),
        });
        SOCKETS.lock().push(Arc::downgrade(&inner));
        Ok(Arc::new(Self { inner }))
    }

    pub fn kind(&self) -> u32 {
        if self.inner.is_ping {
            SOCK_DGRAM
        } else {
            SOCK_RAW
        }
    }

    pub fn protocol(&self) -> u8 {
        self.inner.protocol
    }

    pub fn ino(&self) -> u64 {
        self.inner.ino
    }

    pub fn set_nonblock(&self, nonblock: bool) {
        self.inner.nonblock.store(nonblock, Ordering::Release);
    }

    pub fn set_hop_limit(&self, hops: u8) {
        self.inner.hop_limit.store(hops as u32, Ordering::Release);
    }

    pub fn set_rcvtimeo_us(&self, micros: u64) {
        self.inner.rcvtimeo_us.store(micros, Ordering::Release);
    }

    pub fn bind_device(&self, raw_name: &[u8]) -> Result<(), VfsError> {
        let end = raw_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(raw_name.len());
        let name = core::str::from_utf8(&raw_name[..end]).map_err(|_| VfsError::InvalidArgument)?;
        if name.is_empty() {
            self.inner.bound_ifindex.store(0, Ordering::Release);
            return Ok(());
        }
        let dev = super::super::dev::by_name(name).ok_or(VfsError::NoDevice)?;
        self.inner
            .bound_ifindex
            .store(dev.ifindex(), Ordering::Release);
        Ok(())
    }

    fn ensure_ping_ident(&self) -> u16 {
        let cur = self.inner.ident.load(Ordering::Acquire);
        if cur != 0 {
            return cur;
        }
        let next = alloc_ping6_ident();
        match self
            .inner
            .ident
            .compare_exchange(0, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => next,
            Err(existing) => existing,
        }
    }

    pub fn bind(&self, address: &[u8]) -> Result<(), VfsError> {
        let (local, port, scope) = socket::decode_sockaddr_in6(address)?;
        if !local.is_unspecified() && !addr::is_local(local) {
            return Err(VfsError::AddressNotAvailable);
        }
        *self.inner.bound_address.lock() = Some(local);
        if scope != 0 {
            self.inner.bound_ifindex.store(scope, Ordering::Release);
        }
        if self.inner.is_ping {
            let id = if port == 0 { alloc_ping6_ident() } else { port };
            self.inner.ident.store(id, Ordering::Release);
        }
        Ok(())
    }

    pub fn connect(&self, address: &[u8]) -> Result<(), VfsError> {
        let (peer, port, scope) = socket::decode_sockaddr_in6(address)?;
        *self.inner.peer.lock() = Some((peer, port, scope));
        Ok(())
    }

    pub fn local_address(&self) -> [u8; socket::SOCKADDR_IN6_LEN] {
        let address = self
            .inner
            .bound_address
            .lock()
            .unwrap_or(Ipv6Addr::UNSPECIFIED);
        let scope = self.inner.bound_ifindex.load(Ordering::Acquire);
        let port = if self.inner.is_ping {
            self.inner.ident.load(Ordering::Acquire)
        } else {
            0
        };
        socket::encode_sockaddr_in6(address, port, scope)
    }

    pub fn peer_address(&self) -> Option<[u8; socket::SOCKADDR_IN6_LEN]> {
        self.inner
            .peer
            .lock()
            .map(|(address, port, scope)| socket::encode_sockaddr_in6(address, port, scope))
    }

    pub fn send_to(
        &self,
        data: &[u8],
        destination: Option<&[u8]>,
        dontwait: bool,
    ) -> Result<usize, VfsError> {
        let _ = dontwait;
        let (address, _port, _scope) = match destination {
            Some(bytes) => socket::decode_sockaddr_in6(bytes)?,
            None => self
                .inner
                .peer
                .lock()
                .ok_or(VfsError::DestinationRequired)?,
        };
        if self.inner.is_ping && (data.len() < 8 || data[0] != icmpv6::ICMPV6_ECHO_REQUEST) {
            return Err(VfsError::InvalidArgument);
        }
        if data.len() > MAX_SKB {
            return Err(VfsError::MessageTooLong);
        }
        let mut skb = SkBuff::alloc(DEFAULT_HEADROOM, data.len()).ok_or(VfsError::NoBuffers)?;
        let payload = skb.put(data.len()).ok_or(VfsError::NoBuffers)?;
        payload.copy_from_slice(data);
        if self.inner.is_ping {
            let ident = self.ensure_ping_ident();
            payload[4..6].copy_from_slice(&ident.to_be_bytes());
        }
        let explicit_source = (*self.inner.bound_address.lock()).filter(|a| !a.is_unspecified());
        if self.inner.protocol == ip::IPPROTO_ICMPV6 && payload.len() >= 4 {
            // Per RFC 3542 section 3.1, the kernel computes the ICMPv6 checksum at offset 2.
            let src = explicit_source
                .or_else(|| {
                    super::super::fib::lookup(ip::IpAddr::V6(address))
                        .and_then(|r| addr::select_source(r.ifindex, address))
                })
                .unwrap_or(Ipv6Addr::LOCALHOST);
            payload[2..4].copy_from_slice(&[0, 0]);
            let seed = ip::pseudo_v6(src, address, ip::IPPROTO_ICMPV6, payload.len() as u32);
            let sum = ip::checksum(seed, payload);
            payload[2..4].copy_from_slice(&sum.to_be_bytes());
        }
        let tclass = self.inner.state.lock().tclass;
        let template = Tx6Template {
            traffic_class: tclass,
            flow_label: 0,
            hop_limit: self.inner.hop_limit.load(Ordering::Acquire) as u8,
            protocol: self.inner.protocol,
        };
        output::send(explicit_source, address, template, skb)?;
        Ok(data.len())
    }

    pub fn receive_msg(&self, max: usize, flags: u64) -> Result<InetReceived, VfsError> {
        const MSG_PEEK: u64 = 2;
        const MSG_DONTWAIT: u64 = 0x40;
        let peek = flags & MSG_PEEK != 0;
        let dontwait = flags & MSG_DONTWAIT != 0;
        let timeout_us = self.inner.rcvtimeo_us.load(Ordering::Acquire);
        let deadline =
            (timeout_us != 0).then(|| crate::time::uptime_micros().saturating_add(timeout_us));
        loop {
            {
                let mut state = self.inner.state.lock();
                if let Some(error) = state.error.take() {
                    return Err(match error {
                        105 => VfsError::NoBuffers,
                        13 => VfsError::PermissionDenied,
                        _ => VfsError::IoError,
                    });
                }
                if let Some(front) = state.queue.front() {
                    let full_len = front.packet.len();
                    let truncated = full_len > max;
                    let data = front.packet[..full_len.min(max)].to_vec();
                    let source =
                        socket::encode_sockaddr_in6(front.source, 0, front.ifindex).to_vec();
                    let mut cmsgs: Vec<CmsgItem> = Vec::new();
                    state.timestamp.push_cmsg(front.timestamp_us, &mut cmsgs);
                    let rxopt = state.rxopt;
                    if rxopt & RX6_PKTINFO != 0 {
                        let mut info = [0u8; 20];
                        info[0..16].copy_from_slice(&front.destination.octets());
                        info[16..20].copy_from_slice(&front.ifindex.to_le_bytes());
                        cmsgs.push(CmsgItem {
                            level: SOL_IPV6 as i32,
                            kind: IPV6_PKTINFO as i32,
                            data: info.to_vec(),
                        });
                    }
                    if rxopt & RX6_HOPLIMIT != 0 {
                        cmsgs.push(CmsgItem {
                            level: SOL_IPV6 as i32,
                            kind: IPV6_HOPLIMIT as i32,
                            data: i32::from(front.hop_limit).to_le_bytes().to_vec(),
                        });
                    }
                    if rxopt & RX6_TCLASS != 0 {
                        cmsgs.push(CmsgItem {
                            level: SOL_IPV6 as i32,
                            kind: IPV6_TCLASS as i32,
                            data: i32::from(front.traffic_class).to_le_bytes().to_vec(),
                        });
                    }
                    if rxopt & RX6_2292PKTINFO != 0 {
                        let mut info = [0u8; 20];
                        info[0..16].copy_from_slice(&front.destination.octets());
                        info[16..20].copy_from_slice(&front.ifindex.to_le_bytes());
                        cmsgs.push(CmsgItem {
                            level: SOL_IPV6 as i32,
                            kind: IPV6_2292PKTINFO as i32,
                            data: info.to_vec(),
                        });
                    }
                    if rxopt & RX6_2292HOPLIMIT != 0 {
                        cmsgs.push(CmsgItem {
                            level: SOL_IPV6 as i32,
                            kind: IPV6_2292HOPLIMIT as i32,
                            data: i32::from(front.hop_limit).to_le_bytes().to_vec(),
                        });
                    }
                    if !peek {
                        let popped = state.queue.pop_front().expect("front");
                        state.rmem = state.rmem.saturating_sub(popped.packet.len() + OVERHEAD);
                    }
                    return Ok(InetReceived {
                        data,
                        source: Some(source),
                        cmsgs,
                        control_truncated: false,
                        truncated,
                        full_len,
                        eof: false,
                    });
                }
            }
            if dontwait || self.inner.nonblock.load(Ordering::Acquire) {
                return Err(VfsError::WouldBlock);
            }
            if let Some(limit) = deadline
                && crate::time::uptime_micros() >= limit
            {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::ipc::block_briefly();
        }
    }

    pub fn setsockopt(&self, level: u64, name: u64, value: &[u8]) -> Result<(), VfsError> {
        let val = opt_int_or_byte(value);
        let assign_rx = |state: &mut State, bit: u32, on: bool| {
            if on {
                state.rxopt |= bit;
            } else {
                state.rxopt &= !bit;
            }
        };
        match level {
            SOL_IPV6 => match name {
                IPV6_RECVPKTINFO => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    assign_rx(&mut self.inner.state.lock(), RX6_PKTINFO, val != 0);
                    Ok(())
                }
                IPV6_2292PKTINFO => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    assign_rx(&mut self.inner.state.lock(), RX6_2292PKTINFO, val != 0);
                    Ok(())
                }
                IPV6_RECVHOPLIMIT => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    assign_rx(&mut self.inner.state.lock(), RX6_HOPLIMIT, val != 0);
                    Ok(())
                }
                IPV6_2292HOPLIMIT => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    assign_rx(&mut self.inner.state.lock(), RX6_2292HOPLIMIT, val != 0);
                    Ok(())
                }
                IPV6_RECVTCLASS => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    assign_rx(&mut self.inner.state.lock(), RX6_TCLASS, val != 0);
                    Ok(())
                }
                IPV6_PKTINFO => {
                    if value.len() >= 20 {
                        let idx = u32::from_le_bytes(value[16..20].try_into().unwrap());
                        if idx != 0 && super::super::dev::by_index(idx).is_none() {
                            return Err(VfsError::NoDevice);
                        }
                        self.inner.bound_ifindex.store(idx, Ordering::Release);
                    } else if !value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    Ok(())
                }
                IPV6_HOPLIMIT => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    let eff = if val < 0 {
                        64
                    } else if (1..=255).contains(&val) {
                        val as u8
                    } else {
                        return Err(VfsError::InvalidArgument);
                    };
                    self.set_hop_limit(eff);
                    Ok(())
                }
                IPV6_UNICAST_HOPS => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    let eff = if val == -1 {
                        64
                    } else if (0..=255).contains(&val) {
                        val.max(1) as u8
                    } else {
                        return Err(VfsError::InvalidArgument);
                    };
                    self.set_hop_limit(eff);
                    Ok(())
                }
                IPV6_MULTICAST_HOPS => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    let eff = if val == -1 {
                        1
                    } else if (0..=255).contains(&val) {
                        val as u8
                    } else {
                        return Err(VfsError::InvalidArgument);
                    };
                    self.inner.state.lock().mc_hops = eff;
                    Ok(())
                }
                IPV6_MULTICAST_IF => {
                    if value.len() < 4 {
                        return Err(VfsError::InvalidArgument);
                    }
                    let idx = u32::from_le_bytes(value[0..4].try_into().unwrap());
                    if idx != 0 && super::super::dev::by_index(idx).is_none() {
                        return Err(VfsError::NoDevice);
                    }
                    self.inner.state.lock().mc_if = idx;
                    Ok(())
                }
                IPV6_MULTICAST_LOOP => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    self.inner.state.lock().mc_loop = val != 0;
                    Ok(())
                }
                IPV6_MTU_DISCOVER => {
                    if value.is_empty() || !(0..=5).contains(&val) {
                        return Err(VfsError::InvalidArgument);
                    }
                    self.inner.state.lock().pmtudisc = val as u8;
                    Ok(())
                }
                IPV6_RECVERR => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    self.inner.state.lock().recverr = val != 0;
                    Ok(())
                }
                IPV6_V6ONLY => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    self.inner.state.lock().v6only = val != 0;
                    Ok(())
                }
                IPV6_DONTFRAG => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    self.inner.state.lock().dontfrag = val != 0;
                    Ok(())
                }
                IPV6_TCLASS => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    let eff = if val == -1 {
                        0
                    } else if (0..=255).contains(&val) {
                        val as u8
                    } else {
                        return Err(VfsError::InvalidArgument);
                    };
                    self.inner.state.lock().tclass = eff;
                    Ok(())
                }
                IPV6_AUTOFLOWLABEL => {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    self.inner.state.lock().autoflowlabel = val != 0;
                    Ok(())
                }
                IPV6_ADDR_PREFERENCES => {
                    if value.len() < 4 {
                        return Err(VfsError::InvalidArgument);
                    }
                    self.inner.state.lock().addr_prefs = val as u32;
                    Ok(())
                }
                IPV6_CHECKSUM => {
                    if self.inner.is_ping {
                        return Err(VfsError::NoProtocolOption);
                    }
                    if self.inner.protocol == ip::IPPROTO_ICMPV6 {
                        return Err(VfsError::InvalidArgument);
                    }
                    self.inner.state.lock().csum_offset = val;
                    Ok(())
                }
                IPV6_ADD_MEMBERSHIP | IPV6_DROP_MEMBERSHIP => {
                    if value.len() < 20 {
                        return Err(VfsError::InvalidArgument);
                    }
                    Ok(())
                }
                _ => Err(VfsError::NoProtocolOption),
            },
            SOL_RAW => match name {
                IPV6_CHECKSUM => {
                    if self.inner.is_ping {
                        return Err(VfsError::NoProtocolOption);
                    }
                    self.inner.state.lock().csum_offset = val;
                    Ok(())
                }
                _ => Err(VfsError::NoProtocolOption),
            },
            SOL_ICMPV6 => {
                if self.inner.is_ping || name != ICMP6_FILTER {
                    return Err(VfsError::NoProtocolOption);
                }
                if self.inner.protocol != ip::IPPROTO_ICMPV6 {
                    return Err(VfsError::MessageNotSupported);
                }
                let mut state = self.inner.state.lock();
                let mut raw_bytes = [0u8; 32];
                for (i, word) in state.icmp6_filter.iter().enumerate() {
                    raw_bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
                }
                let copy = value.len().min(32);
                raw_bytes[..copy].copy_from_slice(&value[..copy]);
                for (i, word) in state.icmp6_filter.iter_mut().enumerate() {
                    *word = u32::from_le_bytes(raw_bytes[i * 4..i * 4 + 4].try_into().unwrap());
                }
                Ok(())
            }
            SOL_SOCKET => {
                let bool_val = || -> Result<bool, VfsError> {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    Ok(val != 0)
                };
                let u32_val = || -> Result<u32, VfsError> {
                    if value.len() < 4 {
                        return Err(VfsError::InvalidArgument);
                    }
                    Ok(u32::from_le_bytes(value[0..4].try_into().unwrap()))
                };
                match name {
                    SO_REUSEADDR => {
                        self.inner.state.lock().reuseaddr = bool_val()?;
                        Ok(())
                    }
                    SO_REUSEPORT => {
                        self.inner.state.lock().reuseport = bool_val()?;
                        Ok(())
                    }
                    SO_BROADCAST => {
                        self.inner.state.lock().broadcast = bool_val()?;
                        Ok(())
                    }
                    SO_DONTROUTE => {
                        self.inner.state.lock().dontroute = bool_val()?;
                        Ok(())
                    }
                    SO_KEEPALIVE => {
                        self.inner.state.lock().keepalive = bool_val()?;
                        Ok(())
                    }
                    SO_DEBUG => {
                        let _ = bool_val()?;
                        Ok(())
                    }
                    SO_RCVBUF => {
                        let size = (u32_val()? as usize).saturating_mul(2).max(MIN_BUFFER);
                        self.inner.state.lock().rcvbuf = size;
                        Ok(())
                    }
                    SO_SNDBUF => {
                        let size = (u32_val()? as usize).saturating_mul(2).max(MIN_BUFFER);
                        self.inner.state.lock().sndbuf = size;
                        Ok(())
                    }
                    SO_SNDBUFFORCE | SO_RCVBUFFORCE => {
                        if !privileged() {
                            return Err(VfsError::NotPermitted);
                        }
                        let size = (u32_val()? as usize).saturating_mul(2).max(MIN_BUFFER);
                        let mut state = self.inner.state.lock();
                        if name == SO_SNDBUFFORCE {
                            state.sndbuf = size;
                        } else {
                            state.rcvbuf = size;
                        }
                        Ok(())
                    }
                    SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW => {
                        self.set_rcvtimeo_us(parse_timeval_us(value)?);
                        Ok(())
                    }
                    SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW => {
                        self.inner.state.lock().sndtimeo_us = parse_timeval_us(value)?;
                        Ok(())
                    }
                    SO_BINDTODEVICE => self.bind_device(value),
                    SO_TIMESTAMP_OLD | SO_TIMESTAMP_NEW | SO_TIMESTAMPNS_OLD
                    | SO_TIMESTAMPNS_NEW => {
                        let enabled = bool_val()?;
                        self.inner.state.lock().timestamp.set(name, enabled);
                        Ok(())
                    }
                    SO_MARK => {
                        if !privileged() {
                            return Err(VfsError::NotPermitted);
                        }
                        self.inner.state.lock().mark = u32_val()?;
                        Ok(())
                    }
                    SO_PRIORITY => {
                        self.inner.state.lock().priority = u32_val()?;
                        Ok(())
                    }
                    _ => Err(VfsError::NoProtocolOption),
                }
            }
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    pub fn getsockopt(&self, level: u64, name: u64) -> Result<Vec<u8>, VfsError> {
        let int = |v: i32| Ok(v.to_le_bytes().to_vec());
        match level {
            SOL_IPV6 => {
                let state = self.inner.state.lock();
                match name {
                    IPV6_RECVPKTINFO => int(i32::from(state.rxopt & RX6_PKTINFO != 0)),
                    IPV6_2292PKTINFO => int(i32::from(state.rxopt & RX6_2292PKTINFO != 0)),
                    IPV6_RECVHOPLIMIT => int(i32::from(state.rxopt & RX6_HOPLIMIT != 0)),
                    IPV6_2292HOPLIMIT => int(i32::from(state.rxopt & RX6_2292HOPLIMIT != 0)),
                    IPV6_RECVTCLASS => int(i32::from(state.rxopt & RX6_TCLASS != 0)),
                    IPV6_UNICAST_HOPS | IPV6_HOPLIMIT => {
                        int(self.inner.hop_limit.load(Ordering::Acquire) as i32)
                    }
                    IPV6_MULTICAST_HOPS => int(i32::from(state.mc_hops)),
                    IPV6_MULTICAST_IF => int(state.mc_if as i32),
                    IPV6_MULTICAST_LOOP => int(i32::from(state.mc_loop)),
                    IPV6_MTU_DISCOVER => int(i32::from(state.pmtudisc)),
                    IPV6_MTU => {
                        drop(state);
                        let (dst, _, _) =
                            (*self.inner.peer.lock()).ok_or(VfsError::NotConnected)?;
                        let route = fib::lookup(IpAddr::V6(dst)).ok_or(VfsError::NotConnected)?;
                        let dev = super::super::dev::by_index(route.ifindex)
                            .ok_or(VfsError::NotConnected)?;
                        int(dev.mtu() as i32)
                    }
                    IPV6_RECVERR => int(i32::from(state.recverr)),
                    IPV6_V6ONLY => int(i32::from(state.v6only)),
                    IPV6_DONTFRAG => int(i32::from(state.dontfrag)),
                    IPV6_TCLASS => int(i32::from(state.tclass)),
                    IPV6_AUTOFLOWLABEL => int(i32::from(state.autoflowlabel)),
                    IPV6_ADDR_PREFERENCES => int(state.addr_prefs as i32),
                    IPV6_CHECKSUM => int(state.csum_offset),
                    _ => Err(VfsError::NoProtocolOption),
                }
            }
            SOL_RAW => match name {
                IPV6_CHECKSUM => int(self.inner.state.lock().csum_offset),
                _ => Err(VfsError::NoProtocolOption),
            },
            SOL_ICMPV6 => {
                if self.inner.is_ping || name != ICMP6_FILTER {
                    return Err(VfsError::NoProtocolOption);
                }
                if self.inner.protocol != ip::IPPROTO_ICMPV6 {
                    return Err(VfsError::MessageNotSupported);
                }
                let state = self.inner.state.lock();
                let mut out = [0u8; 32];
                for (i, word) in state.icmp6_filter.iter().enumerate() {
                    out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
                }
                Ok(out.to_vec())
            }
            SOL_SOCKET => {
                let mut state = self.inner.state.lock();
                match name {
                    SO_TYPE => int(self.kind() as i32),
                    SO_DOMAIN => int(AF_INET6 as i32),
                    SO_PROTOCOL => int(i32::from(self.inner.protocol)),
                    SO_ACCEPTCONN => int(0),
                    SO_ERROR => int(state.error.take().unwrap_or(0)),
                    SO_REUSEADDR => int(i32::from(state.reuseaddr)),
                    SO_REUSEPORT => int(i32::from(state.reuseport)),
                    SO_BROADCAST => int(i32::from(state.broadcast)),
                    SO_DONTROUTE => int(i32::from(state.dontroute)),
                    SO_KEEPALIVE => int(i32::from(state.keepalive)),
                    SO_SNDBUF => int(state.sndbuf as i32),
                    SO_RCVBUF => int(state.rcvbuf as i32),
                    SO_MARK => int(state.mark as i32),
                    SO_PRIORITY => int(state.priority as i32),
                    SO_TIMESTAMP_OLD | SO_TIMESTAMP_NEW | SO_TIMESTAMPNS_OLD
                    | SO_TIMESTAMPNS_NEW => int(state.timestamp.get(name) as i32),
                    SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW => Ok(encode_timeval_us(
                        self.inner.rcvtimeo_us.load(Ordering::Acquire),
                    )),
                    SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW => Ok(encode_timeval_us(state.sndtimeo_us)),
                    _ => Err(VfsError::NoProtocolOption),
                }
            }
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    pub fn receive(
        &self,
        max: usize,
        dontwait: bool,
    ) -> Result<(Vec<u8>, Ipv6Addr, u32), VfsError> {
        let timeout_us = self.inner.rcvtimeo_us.load(Ordering::Acquire);
        let deadline =
            (timeout_us != 0).then(|| crate::time::uptime_micros().saturating_add(timeout_us));
        loop {
            {
                let mut state = self.inner.state.lock();
                if let Some(error) = state.error.take() {
                    return Err(match error {
                        105 => VfsError::NoBuffers,
                        13 => VfsError::PermissionDenied,
                        _ => VfsError::IoError,
                    });
                }
                if let Some(datagram) = state.queue.pop_front() {
                    state.rmem = state.rmem.saturating_sub(datagram.packet.len() + OVERHEAD);
                    let length = max.min(datagram.packet.len());
                    let packet = datagram.packet[..length].to_vec();
                    return Ok((packet, datagram.source, datagram.ifindex));
                }
            }
            if dontwait || self.inner.nonblock.load(Ordering::Acquire) {
                return Err(VfsError::WouldBlock);
            }
            if let Some(limit) = deadline
                && crate::time::uptime_micros() >= limit
            {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::ipc::block_briefly();
        }
    }

    pub fn error(&self) -> Option<i32> {
        self.inner.state.lock().error.take()
    }
}

impl Inode for Raw6Socket {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.inner.ino, InodeKind::Socket, 0o600, 0))
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        let (packet, _source, _ifindex) = self.receive(buffer.len(), false)?;
        buffer[..packet.len()].copy_from_slice(&packet);
        Ok(packet.len())
    }

    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        self.send_to(buffer, None, false)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(self.inner.as_ref() as *const Raw6Inner);
    }

    fn poll(&self) -> u32 {
        const POLLIN: u32 = 1;
        const POLLOUT: u32 = 4;
        let state = self.inner.state.lock();
        let mut events = POLLOUT;
        if !state.queue.is_empty() || state.error.is_some() {
            events |= POLLIN;
        }
        events
    }

    fn ioctl(&self, cmd: u64, _argument: u64) -> Result<u64, VfsError> {
        const FIONREAD: u64 = 0x541B;
        match cmd {
            FIONREAD => Ok(self
                .inner
                .state
                .lock()
                .queue
                .front()
                .map_or(0, |datagram| datagram.packet.len()) as u64),
            _ => Err(VfsError::InvalidArgument),
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.set_nonblock(nonblock);
        Ok(())
    }
}

impl Drop for Raw6Socket {
    fn drop(&mut self) {
        SOCKETS.lock().retain(|weak| weak.strong_count() != 0);
        crate::ipc::poll_kick();
    }
}

/// Deliver a locally received IPv6 transport payload to matching raw6 sockets
/// (per RFC 3542 section 3, IPv6 raw sockets receive payload without the IPv6 header).
pub fn deliver(dev: &Arc<NetDevice>, meta: &Ip6Meta, skb: &SkBuff) {
    let payload = match skb.data().get(meta.header_len..meta.total_len) {
        Some(slice) => slice,
        None => return,
    };
    let targets: Vec<Arc<Raw6Inner>> = {
        let mut list = SOCKETS.lock();
        list.retain(|weak| weak.strong_count() != 0);
        list.iter().filter_map(Weak::upgrade).collect()
    };
    let mut delivered = false;
    for inner in targets {
        if inner.protocol != meta.protocol && inner.protocol != ip::IPPROTO_RAW {
            continue;
        }
        let bound_ifindex = inner.bound_ifindex.load(Ordering::Acquire);
        if bound_ifindex != 0 && bound_ifindex != dev.ifindex() {
            continue;
        }
        if inner.is_ping {
            if payload.len() < 8 || payload[0] != icmpv6::ICMPV6_ECHO_REPLY {
                continue;
            }
            let pkt_id = u16::from_be_bytes([payload[4], payload[5]]);
            let want_id = inner.ident.load(Ordering::Acquire);
            if want_id == 0 || pkt_id != want_id {
                continue;
            }
        }
        if let Some(bound) = *inner.bound_address.lock()
            && !bound.is_unspecified()
            && bound != meta.destination
        {
            continue;
        }
        if let Some((peer, _, _)) = *inner.peer.lock()
            && !peer.is_unspecified()
            && peer != meta.source
        {
            continue;
        }
        if !inner.is_ping && inner.protocol == ip::IPPROTO_ICMPV6 && !payload.is_empty() {
            let icmp_type = payload[0];
            let filter = inner.state.lock().icmp6_filter;
            if (filter[(icmp_type >> 5) as usize] & (1u32 << (icmp_type & 31))) != 0 {
                continue;
            }
        }
        let mut state = inner.state.lock();
        if state.rmem + payload.len() + OVERHEAD <= state.rcvbuf {
            let timestamp_us = if state.timestamp.is_enabled() {
                crate::time::realtime_micros()
            } else {
                0
            };
            state.rmem += payload.len() + OVERHEAD;
            state.queue.push_back(Datagram {
                packet: payload.to_vec(),
                source: meta.source,
                destination: meta.destination,
                ifindex: dev.ifindex(),
                hop_limit: meta.hop_limit,
                traffic_class: meta.traffic_class,
                timestamp_us,
            });
            delivered = true;
        }
    }
    if delivered {
        DELIVERED.fetch_add(1, Ordering::Relaxed);
        crate::ipc::wake_pollers();
    }
}

pub fn create_raw(kind: u32, protocol: u32, nonblock: bool) -> Result<Arc<OpenFile>, VfsError> {
    if kind != SOCK_RAW || protocol > u8::MAX as u32 {
        return Err(VfsError::ProtocolType);
    }
    let socket = Raw6Socket::create(protocol as u8, nonblock)?;
    Ok(socket::socket_file(socket, "socket:raw6", nonblock))
}

pub fn create_ping(nonblock: bool) -> Result<Arc<OpenFile>, VfsError> {
    let socket = Raw6Socket::create_ping(nonblock)?;
    Ok(socket::socket_file(socket, "socket:icmpv6", nonblock))
}
