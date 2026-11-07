/*
 *
 *       src/net/ipv4/raw.rs
 *       Raw IPv4 sockets (SOCK_RAW), used by ping and diagnostic tools
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    super::{
        dev::NetDevice,
        ip::{self, IpAddr, Ipv4Addr},
        skb::SkBuff,
        socket::{
            self, AF_INET, CmsgItem, InetReceived, Ip4SockOpts, IpRxMeta, SO_ACCEPTCONN,
            SO_BINDTODEVICE, SO_BROADCAST, SO_DEBUG, SO_DOMAIN, SO_DONTROUTE, SO_ERROR,
            SO_KEEPALIVE, SO_MARK, SO_PRIORITY, SO_PROTOCOL, SO_RCVBUF, SO_RCVBUFFORCE,
            SO_RCVTIMEO_NEW, SO_RCVTIMEO_OLD, SO_REUSEADDR, SO_REUSEPORT, SO_SNDBUF,
            SO_SNDBUFFORCE, SO_SNDTIMEO_NEW, SO_SNDTIMEO_OLD, SO_TIMESTAMP_NEW, SO_TIMESTAMP_OLD,
            SO_TIMESTAMPNS_NEW, SO_TIMESTAMPNS_OLD, SO_TYPE, SOCK_DGRAM, SOCK_RAW, SOL_IP, SOL_RAW,
            SOL_SOCKET, TimestampOpt, encode_timeval_us, opt_int_or_byte, parse_timeval_us,
        },
    },
    header::{IPV4_HLEN, IpMeta, TxTemplate},
    output,
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
const ICMP_FILTER: u64 = 1;

struct Datagram {
    packet: Vec<u8>,
    meta: IpRxMeta,
}

struct State {
    queue: VecDeque<Datagram>,
    rmem: usize,
    rcvbuf: usize,
    sndbuf: usize,
    error: Option<i32>,
    ip_opts: Ip4SockOpts,
    timestamp: TimestampOpt,
    icmp_filter: u32,
    reuseaddr: bool,
    reuseport: bool,
    broadcast: bool,
    dontroute: bool,
    keepalive: bool,
    sndtimeo_us: u64,
    mark: u32,
    priority: u32,
}

struct RawInner {
    ino: u64,
    protocol: u8,
    is_ping: bool,
    ident: AtomicU16,
    rcvtimeo_us: AtomicU64,
    nonblock: AtomicBool,
    bound_address: SpinLock<Option<Ipv4Addr>>,
    bound_ifindex: AtomicU32,
    peer: SpinLock<Option<(Ipv4Addr, u16)>>,
    state: SpinLock<State>,
}

pub struct RawSocket {
    inner: Arc<RawInner>,
}

static SOCKETS: SpinLock<Vec<Weak<RawInner>>> = SpinLock::new(Vec::new());
static DELIVERED: AtomicU64 = AtomicU64::new(0);
static NEXT_PING_IDENT: AtomicU16 = AtomicU16::new(1);

fn privileged() -> bool {
    sched::current().is_none_or(|task| task.credentials().euid == 0)
}

fn alloc_ping_ident() -> u16 {
    loop {
        let id = NEXT_PING_IDENT.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

impl RawSocket {
    pub fn create(protocol: u8, nonblock: bool) -> Result<Arc<Self>, VfsError> {
        if !privileged() {
            return Err(VfsError::NotPermitted);
        }
        Self::new_inner(protocol, false, nonblock)
    }

    /// `SOCK_DGRAM` + `IPPROTO_ICMP` (`net/ipv4/ping.c`), usable without root.
    pub fn create_ping(nonblock: bool) -> Result<Arc<Self>, VfsError> {
        Self::new_inner(ip::IPPROTO_ICMP, true, nonblock)
    }

    fn new_inner(protocol: u8, is_ping: bool, nonblock: bool) -> Result<Arc<Self>, VfsError> {
        let mut ip_opts = Ip4SockOpts::default();
        if !is_ping && protocol == ip::IPPROTO_RAW {
            ip_opts.hdrincl = true;
        }
        let inner = Arc::new(RawInner {
            ino: vfs::tree::ino(),
            protocol,
            is_ping,
            ident: AtomicU16::new(0),
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
                ip_opts,
                timestamp: TimestampOpt::Off,
                icmp_filter: 0,
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

    pub fn set_ttl(&self, ttl: u8) {
        self.inner.state.lock().ip_opts.uc_ttl = i16::from(ttl);
    }

    pub fn set_tos(&self, tos: u8) {
        self.inner.state.lock().ip_opts.tos = tos;
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

    fn bound_device_bytes(&self) -> Vec<u8> {
        let idx = self.inner.bound_ifindex.load(Ordering::Acquire);
        if idx == 0 {
            return Vec::new();
        }
        match super::super::dev::by_index(idx) {
            Some(dev) => {
                let mut out = dev.name().as_bytes().to_vec();
                out.push(0);
                out
            }
            None => Vec::new(),
        }
    }

    fn ensure_ping_ident(&self) -> u16 {
        let cur = self.inner.ident.load(Ordering::Acquire);
        if cur != 0 {
            return cur;
        }
        let next = alloc_ping_ident();
        match self
            .inner
            .ident
            .compare_exchange(0, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => next,
            Err(existing) => existing,
        }
    }

    /// `raw_bind` / `ping_bind`: remember the local address (and ICMP ident for `SOCK_DGRAM`).
    pub fn bind(&self, address: &[u8]) -> Result<(), VfsError> {
        let (local, port) = socket::decode_sockaddr_in(address)?;
        let freebind = self.inner.state.lock().ip_opts.freebind;
        if !local.is_unspecified() && !super::addr::is_local(local) && !freebind {
            return Err(VfsError::AddressNotAvailable);
        }
        *self.inner.bound_address.lock() = Some(local);
        if self.inner.is_ping {
            let id = if port == 0 { alloc_ping_ident() } else { port };
            self.inner.ident.store(id, Ordering::Release);
        }
        Ok(())
    }

    pub fn connect(&self, address: &[u8]) -> Result<(), VfsError> {
        let (peer, port) = socket::decode_sockaddr_in(address)?;
        *self.inner.peer.lock() = Some((peer, port));
        Ok(())
    }

    pub fn local_address(&self) -> [u8; socket::SOCKADDR_IN_LEN] {
        let address = self
            .inner
            .bound_address
            .lock()
            .unwrap_or(Ipv4Addr::UNSPECIFIED);
        let port = if self.inner.is_ping {
            self.inner.ident.load(Ordering::Acquire)
        } else {
            0
        };
        socket::encode_sockaddr_in(address, port)
    }

    pub fn peer_address(&self) -> Option<[u8; socket::SOCKADDR_IN_LEN]> {
        self.inner
            .peer
            .lock()
            .map(|(address, port)| socket::encode_sockaddr_in(address, port))
    }

    /// Send a transport message. The kernel prepends the IPv4 header unless
    /// `IP_HDRINCL` or `IPPROTO_RAW` is active on a `SOCK_RAW` socket.
    pub fn send_to(
        &self,
        data: &[u8],
        destination: Option<&[u8]>,
        dontwait: bool,
    ) -> Result<usize, VfsError> {
        let _ = dontwait;
        let (address, _port) = match destination {
            Some(bytes) => socket::decode_sockaddr_in(bytes)?,
            None => self
                .inner
                .peer
                .lock()
                .map(|peer| (peer.0, peer.1))
                .ok_or(VfsError::DestinationRequired)?,
        };
        if self.inner.is_ping && (data.len() < 8 || data[0] != super::icmp::ICMP_ECHO) {
            return Err(VfsError::InvalidArgument);
        }
        if data.len() > super::super::skb::MAX_SKB {
            return Err(VfsError::MessageTooLong);
        }
        let (tos, ttl, dont_fragment, hdrincl, ip_options, broadcast) = {
            let state = self.inner.state.lock();
            (
                state.ip_opts.tos,
                state.ip_opts.effective_ttl(),
                state.ip_opts.dont_fragment(),
                state.ip_opts.hdrincl,
                state.ip_opts.ip_options.clone(),
                state.broadcast,
            )
        };
        if address.is_broadcast() && !broadcast && !privileged() {
            return Err(VfsError::PermissionDenied);
        }
        if !self.inner.is_ping && (hdrincl || self.inner.protocol == ip::IPPROTO_RAW) {
            output::send_hdrincl(address, data)?;
            return Ok(data.len());
        }
        let mut skb = SkBuff::alloc(super::super::skb::DEFAULT_HEADROOM, data.len())
            .ok_or(VfsError::NoBuffers)?;
        let payload = skb.put(data.len()).ok_or(VfsError::NoBuffers)?;
        payload.copy_from_slice(data);
        if self.inner.is_ping {
            let ident = self.ensure_ping_ident();
            payload[2..4].copy_from_slice(&[0, 0]);
            payload[4..6].copy_from_slice(&ident.to_be_bytes());
            let sum = ip::checksum(0, payload);
            payload[2..4].copy_from_slice(&sum.to_be_bytes());
        }
        let source = *self.inner.bound_address.lock();
        let template = TxTemplate {
            tos,
            ttl,
            protocol: self.inner.protocol,
            dont_fragment,
        };
        output::send_with_options(
            source.filter(|address| !address.is_unspecified()),
            address,
            template,
            &ip_options,
            skb,
        )?;
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
                    return Err(errno_error(error));
                }
                if let Some(front) = state.queue.front() {
                    let full_len = front.packet.len();
                    let truncated = full_len > max;
                    let data = front.packet[..full_len.min(max)].to_vec();
                    let source = socket::encode_sockaddr_in(front.meta.source, 0).to_vec();
                    let mut cmsgs: Vec<CmsgItem> = Vec::new();
                    state
                        .timestamp
                        .push_cmsg(front.meta.timestamp_us, &mut cmsgs);
                    let control_truncated = state.ip_opts.collect_cmsgs(&front.meta, 0, &mut cmsgs);
                    if !peek {
                        let popped = state.queue.pop_front().expect("front");
                        state.rmem = state.rmem.saturating_sub(popped.packet.len() + OVERHEAD);
                    }
                    return Ok(InetReceived {
                        data,
                        source: Some(source),
                        cmsgs,
                        control_truncated,
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

    pub fn receive(
        &self,
        max: usize,
        dontwait: bool,
    ) -> Result<(Vec<u8>, Ipv4Addr, u32), VfsError> {
        let timeout_us = self.inner.rcvtimeo_us.load(Ordering::Acquire);
        let deadline =
            (timeout_us != 0).then(|| crate::time::uptime_micros().saturating_add(timeout_us));
        loop {
            {
                let mut state = self.inner.state.lock();
                if let Some(error) = state.error.take() {
                    return Err(errno_error(error));
                }
                if let Some(datagram) = state.queue.pop_front() {
                    state.rmem = state.rmem.saturating_sub(datagram.packet.len() + OVERHEAD);
                    let length = max.min(datagram.packet.len());
                    let packet = datagram.packet[..length].to_vec();
                    return Ok((packet, datagram.meta.source, datagram.meta.ifindex));
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
        match level {
            SOL_IP => self
                .inner
                .state
                .lock()
                .ip_opts
                .set(self.kind(), name, value),
            SOL_RAW => {
                if self.inner.is_ping || name != ICMP_FILTER {
                    return Err(VfsError::NoProtocolOption);
                }
                if self.inner.protocol != ip::IPPROTO_ICMP {
                    return Err(VfsError::MessageNotSupported);
                }
                let mut state = self.inner.state.lock();
                let mut bytes = state.icmp_filter.to_le_bytes();
                let copy = value.len().min(4);
                bytes[..copy].copy_from_slice(&value[..copy]);
                state.icmp_filter = u32::from_le_bytes(bytes);
                Ok(())
            }
            SOL_SOCKET => {
                let bool_val = || -> Result<bool, VfsError> {
                    if value.is_empty() {
                        return Err(VfsError::InvalidArgument);
                    }
                    Ok(opt_int_or_byte(value) != 0)
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
        let int = |v: u32| Ok(v.to_le_bytes().to_vec());
        match level {
            SOL_IP => {
                let peer = self.inner.peer.lock().map(|(addr, _)| addr);
                self.inner
                    .state
                    .lock()
                    .ip_opts
                    .get(self.kind(), self.inner.protocol, peer, name)
            }
            SOL_RAW => {
                if self.inner.is_ping || name != ICMP_FILTER {
                    return Err(VfsError::NoProtocolOption);
                }
                if self.inner.protocol != ip::IPPROTO_ICMP {
                    return Err(VfsError::MessageNotSupported);
                }
                int(self.inner.state.lock().icmp_filter)
            }
            SOL_SOCKET => {
                let mut state = self.inner.state.lock();
                match name {
                    SO_TYPE => int(self.kind()),
                    SO_DOMAIN => int(AF_INET as u32),
                    SO_PROTOCOL => int(u32::from(self.inner.protocol)),
                    SO_ACCEPTCONN => int(0),
                    SO_ERROR => int(state.error.take().unwrap_or(0) as u32),
                    SO_REUSEADDR => int(u32::from(state.reuseaddr)),
                    SO_REUSEPORT => int(u32::from(state.reuseport)),
                    SO_BROADCAST => int(u32::from(state.broadcast)),
                    SO_DONTROUTE => int(u32::from(state.dontroute)),
                    SO_KEEPALIVE => int(u32::from(state.keepalive)),
                    SO_SNDBUF => int(state.sndbuf as u32),
                    SO_RCVBUF => int(state.rcvbuf as u32),
                    SO_MARK => int(state.mark),
                    SO_PRIORITY => int(state.priority),
                    SO_TIMESTAMP_OLD | SO_TIMESTAMP_NEW | SO_TIMESTAMPNS_OLD
                    | SO_TIMESTAMPNS_NEW => int(state.timestamp.get(name)),
                    SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW => Ok(encode_timeval_us(
                        self.inner.rcvtimeo_us.load(Ordering::Acquire),
                    )),
                    SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW => Ok(encode_timeval_us(state.sndtimeo_us)),
                    SO_BINDTODEVICE => {
                        drop(state);
                        Ok(self.bound_device_bytes())
                    }
                    _ => Err(VfsError::NoProtocolOption),
                }
            }
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    pub fn error(&self) -> Option<i32> {
        self.inner.state.lock().error.take()
    }

    fn enqueue(&self, packet: Vec<u8>, mut rx_meta: IpRxMeta) {
        let mut state = self.inner.state.lock();
        if state.rmem + packet.len() + OVERHEAD > state.rcvbuf {
            return;
        }
        if state.timestamp.is_enabled() && rx_meta.timestamp_us == 0 {
            rx_meta.timestamp_us = crate::time::realtime_micros();
        }
        state.rmem += packet.len() + OVERHEAD;
        state.queue.push_back(Datagram {
            packet,
            meta: rx_meta,
        });
        drop(state);
        crate::ipc::wake_pollers();
    }
}

fn errno_error(error: i32) -> VfsError {
    match error {
        105 => VfsError::NoBuffers,
        13 => VfsError::PermissionDenied,
        _ => VfsError::IoError,
    }
}

impl Inode for RawSocket {
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
        sources.object(self.inner.as_ref() as *const RawInner);
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
                .iter()
                .map(|d| d.packet.len())
                .sum::<usize>() as u64),
            _ => Err(VfsError::InvalidArgument),
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.set_nonblock(nonblock);
        Ok(())
    }
}

/// `raw_v4_input`: copy the complete IP datagram to every interested raw
/// socket before the transport layer consumes it.
pub fn deliver(dev: &Arc<NetDevice>, meta: &IpMeta, full: &SkBuff) {
    let Some(packet) = full.network_slice() else {
        return;
    };
    let mut targets = Vec::new();
    {
        let mut sockets = SOCKETS.lock();
        sockets.retain(|weak| weak.strong_count() != 0);
        for weak in sockets.iter() {
            if let Some(inner) = weak.upgrade()
                && (inner.protocol == meta.protocol || inner.protocol == ip::IPPROTO_RAW)
            {
                targets.push(inner);
            }
        }
    }
    for inner in targets {
        if let Some(address) = *inner.bound_address.lock()
            && !address.is_unspecified()
            && address != meta.destination
        {
            continue;
        }
        let bound_ifindex = inner.bound_ifindex.load(Ordering::Acquire);
        if bound_ifindex != 0 && bound_ifindex != dev.ifindex() {
            continue;
        }
        let socket = RawSocket {
            inner: inner.clone(),
        };
        let ip_options = if meta.header_len > IPV4_HLEN {
            packet
                .get(IPV4_HLEN..meta.header_len)
                .unwrap_or_default()
                .to_vec()
        } else {
            Vec::new()
        };
        let rx_meta = IpRxMeta {
            source: meta.source,
            destination: meta.destination,
            spec_dst: super::addr::first_on(dev.ifindex()).unwrap_or(meta.destination),
            ifindex: dev.ifindex(),
            ttl: meta.ttl,
            tos: meta.tos,
            ip_options,
            timestamp_us: 0,
        };
        if inner.is_ping {
            let Some(icmp_payload) = packet.get(meta.header_len..) else {
                continue;
            };
            if icmp_payload.len() < 8 || icmp_payload[0] != super::icmp::ICMP_ECHOREPLY {
                continue;
            }
            let pkt_id = u16::from_be_bytes([icmp_payload[4], icmp_payload[5]]);
            let want_id = inner.ident.load(Ordering::Acquire);
            if want_id == 0 || pkt_id != want_id {
                continue;
            }
            socket.enqueue(icmp_payload.to_vec(), rx_meta);
        } else {
            if inner.protocol == ip::IPPROTO_ICMP
                && let Some(icmp_hdr) = packet.get(meta.header_len..)
                && !icmp_hdr.is_empty()
            {
                let icmp_type = icmp_hdr[0];
                let filter = inner.state.lock().icmp_filter;
                if icmp_type < 32 && (filter & (1u32 << icmp_type)) != 0 {
                    continue;
                }
            }
            socket.enqueue(packet.to_vec(), rx_meta);
        }
        DELIVERED.fetch_add(1, Ordering::Relaxed);
    }
}

/// `raw_protocol_count`: raw deliveries, for self-tests.
pub fn delivered() -> u64 {
    DELIVERED.load(Ordering::Relaxed)
}

/// Family constructor for `AF_INET`/`SOCK_RAW`.
pub fn create_raw(kind: u32, protocol: u32, nonblock: bool) -> Result<Arc<OpenFile>, VfsError> {
    if kind != SOCK_RAW {
        return Err(VfsError::ProtocolType);
    }
    let protocol = u8::try_from(protocol).map_err(|_| VfsError::ProtocolType)?;
    if protocol == 0 {
        return Err(VfsError::ProtocolType);
    }
    let socket = RawSocket::create(protocol, nonblock)?;
    Ok(crate::net::socket::socket_file(
        socket,
        "socket:raw",
        nonblock,
    ))
}

/// Family constructor for unprivileged `AF_INET`/`SOCK_DGRAM`/`IPPROTO_ICMP`.
pub fn create_ping(nonblock: bool) -> Result<Arc<OpenFile>, VfsError> {
    let socket = RawSocket::create_ping(nonblock)?;
    Ok(crate::net::socket::socket_file(
        socket,
        "socket:icmp",
        nonblock,
    ))
}

/// Resolve an fd to a raw socket, if it is one.
pub fn instance(fd: u64) -> Option<Arc<RawSocket>> {
    super::super::socket::inode_of::<RawSocket>(fd)
}

/// Destination address helper for syscall glue.
pub fn destination(bytes: &[u8]) -> Result<Ipv4Addr, VfsError> {
    socket::decode_sockaddr_in(bytes).map(|(address, _port)| address)
}

/// Source selection shared with diagnostics.
pub fn route_source(destination: Ipv4Addr) -> Option<Ipv4Addr> {
    super::super::fib::lookup(IpAddr::V4(destination)).and_then(|route| match route.source {
        Some(IpAddr::V4(address)) => Some(address),
        _ => super::addr::first_on(route.ifindex),
    })
}
