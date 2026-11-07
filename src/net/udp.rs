/*
 *
 *       src/net/udp.rs
 *       User Datagram Protocol
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Port table, checksums and the receive queue. Sending hands the datagram to
//! the IPv4 output path; the kernel selects the source address unless the
//! socket is bound.

use super::{
    dev::NetDevice,
    ip::{self, Ipv4Addr},
    ipv4::{
        addr,
        header::{IPV4_HLEN, IpMeta, TxTemplate},
        icmp, input, output,
    },
    skb::SkBuff,
    socket::{
        self, CmsgItem, InetReceived, Ip4SockOpts, IpRxMeta, SO_ACCEPTCONN, SO_BINDTODEVICE,
        SO_BROADCAST, SO_DEBUG, SO_DOMAIN, SO_DONTROUTE, SO_ERROR, SO_KEEPALIVE, SO_MARK,
        SO_PRIORITY, SO_PROTOCOL, SO_RCVBUF, SO_RCVBUFFORCE, SO_RCVTIMEO_NEW, SO_RCVTIMEO_OLD,
        SO_REUSEADDR, SO_REUSEPORT, SO_SNDBUF, SO_SNDBUFFORCE, SO_SNDTIMEO_NEW, SO_SNDTIMEO_OLD,
        SO_TIMESTAMP_NEW, SO_TIMESTAMP_OLD, SO_TIMESTAMPNS_NEW, SO_TIMESTAMPNS_OLD, SO_TYPE,
        SOCK_DGRAM, SOL_IP, SOL_SOCKET, TimestampOpt, encode_timeval_us, opt_int_or_byte,
        parse_timeval_us,
    },
};
use crate::{
    sched,
    sync::SpinLock,
    vfs::{self, Inode, InodeKind, Stat, VfsError},
};
use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const UDP_HEADER_LEN: usize = 8;
const DEFAULT_BUFFER: usize = 212_992;
const MIN_BUFFER: usize = 4_608;
const OVERHEAD: usize = 256;
const MAX_PAYLOAD: usize = u16::MAX as usize - 20 - UDP_HEADER_LEN;
const EPHEMERAL_START: u16 = 32_768;
const EPHEMERAL_END: u16 = 60_999;

struct Datagram {
    payload: Vec<u8>,
    source_port: u16,
    destination_port: u16,
    meta: IpRxMeta,
}

struct State {
    local_address: Ipv4Addr,
    local_port: u16,
    peer_address: Ipv4Addr,
    peer_port: u16,
    connected: bool,
    queue: VecDeque<Datagram>,
    rmem: usize,
    rcvbuf: usize,
    sndbuf: usize,
    error: Option<i32>,
    reuseaddr: bool,
    reuseport: bool,
    broadcast: bool,
    dontroute: bool,
    keepalive: bool,
    ip_opts: Ip4SockOpts,
    timestamp: TimestampOpt,
    bound: bool,
    bound_ifindex: u32,
    rcvtimeo_us: u64,
    sndtimeo_us: u64,
    mark: u32,
    priority: u32,
}

pub struct UdpSocket {
    ino: u64,
    nonblock: AtomicBool,
    state: SpinLock<State>,
    me: Weak<UdpSocket>,
}

struct Binding {
    port: u16,
    address: Ipv4Addr,
    socket: Weak<UdpSocket>,
}

static BINDINGS: SpinLock<Vec<Binding>> = SpinLock::new(Vec::new());
static NEXT_EPHEMERAL: AtomicU64 = AtomicU64::new(EPHEMERAL_START as u64);
static IN_DATAGRAMS: AtomicU64 = AtomicU64::new(0);
static OUT_DATAGRAMS: AtomicU64 = AtomicU64::new(0);

fn privileged() -> bool {
    sched::current().is_none_or(|task| task.credentials().euid == 0)
}

impl UdpSocket {
    pub fn create(nonblock: bool) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            ino: vfs::tree::ino(),
            nonblock: AtomicBool::new(nonblock),
            state: SpinLock::new(State {
                local_address: Ipv4Addr::UNSPECIFIED,
                local_port: 0,
                peer_address: Ipv4Addr::UNSPECIFIED,
                peer_port: 0,
                connected: false,
                queue: VecDeque::new(),
                rmem: 0,
                rcvbuf: DEFAULT_BUFFER,
                sndbuf: DEFAULT_BUFFER,
                error: None,
                reuseaddr: false,
                reuseport: false,
                broadcast: false,
                dontroute: false,
                keepalive: false,
                ip_opts: Ip4SockOpts::default(),
                timestamp: TimestampOpt::Off,
                bound: false,
                bound_ifindex: 0,
                rcvtimeo_us: 0,
                sndtimeo_us: 0,
                mark: 0,
                priority: 0,
            }),
            me: me.clone(),
        })
    }

    pub fn ino(&self) -> u64 {
        self.ino
    }

    fn arc(&self) -> Arc<Self> {
        self.me.upgrade().expect("udp socket used after drop")
    }

    pub fn kind(&self) -> u32 {
        SOCK_DGRAM
    }

    pub fn domain(&self) -> u64 {
        socket::AF_INET
    }

    pub fn protocol(&self) -> u64 {
        0
    }

    pub fn is_connected(&self) -> bool {
        self.state.lock().connected
    }

    pub fn local_address(&self) -> [u8; socket::SOCKADDR_IN_LEN] {
        let state = self.state.lock();
        socket::encode_sockaddr_in(state.local_address, state.local_port)
    }

    pub fn peer_address(&self) -> Option<[u8; socket::SOCKADDR_IN_LEN]> {
        let state = self.state.lock();
        state
            .connected
            .then(|| socket::encode_sockaddr_in(state.peer_address, state.peer_port))
    }

    pub fn set_nonblock(&self, nonblock: bool) {
        self.nonblock.store(nonblock, Ordering::Release);
    }

    /// `inet_bind`: claim a local address/port pair.
    pub fn bind(&self, address: &[u8]) -> Result<(), VfsError> {
        let (local, port) = socket::decode_sockaddr_in(address)?;
        let freebind = self.state.lock().ip_opts.freebind;
        if !local.is_unspecified() && !addr::is_local(local) && !freebind && !privileged() {
            return Err(VfsError::AddressNotAvailable);
        }
        let mut state = self.state.lock();
        if state.bound {
            return Err(VfsError::InvalidArgument);
        }
        let port = if port == 0 {
            allocate_port(local)?
        } else {
            check_port_available(local, port, state.reuseaddr)?
        };
        state.local_address = local;
        state.local_port = port;
        state.bound = true;
        drop(state);
        register_binding(&self.arc())?;
        Ok(())
    }

    fn bind_ephemeral(&self) -> Result<(), VfsError> {
        let mut state = self.state.lock();
        if state.bound {
            return Ok(());
        }
        let port = allocate_port(Ipv4Addr::UNSPECIFIED)?;
        state.local_address = Ipv4Addr::UNSPECIFIED;
        state.local_port = port;
        state.bound = true;
        drop(state);
        register_binding(&self.arc())
    }

    /// `udp_connect`: record the peer; no packet is exchanged.
    pub fn connect(&self, address: &[u8]) -> Result<(), VfsError> {
        let (peer, port) = socket::decode_sockaddr_in(address)?;
        if port == 0 {
            return Err(VfsError::AddressNotAvailable);
        }
        if peer.is_unspecified() {
            return Err(VfsError::InvalidArgument);
        }
        self.bind_ephemeral()?;
        let mut state = self.state.lock();
        state.peer_address = peer;
        state.peer_port = port;
        state.connected = true;
        Ok(())
    }

    pub fn send_to(
        &self,
        payload: &[u8],
        destination: Option<&[u8]>,
        flags: u64,
    ) -> Result<usize, VfsError> {
        let _ = flags;
        if payload.len() > MAX_PAYLOAD {
            return Err(VfsError::MessageTooLong);
        }
        let (state_address, state_port, connected) = {
            let state = self.state.lock();
            (state.peer_address, state.peer_port, state.connected)
        };
        let (destination_address, destination_port) = match destination {
            Some(bytes) => socket::decode_sockaddr_in(bytes)?,
            None if connected => (state_address, state_port),
            None => return Err(VfsError::DestinationRequired),
        };
        if destination_port == 0 {
            return Err(VfsError::AddressNotAvailable);
        }
        {
            let state = self.state.lock();
            if destination_address.is_broadcast() && !state.broadcast {
                return Err(VfsError::PermissionDenied);
            }
        }
        self.bind_ephemeral()?;
        let (source, ttl, tos, dont_fragment, ip_options, local_port) = {
            let state = self.state.lock();
            (
                (!state.local_address.is_unspecified()).then_some(state.local_address),
                state.ip_opts.effective_ttl(),
                state.ip_opts.tos,
                state.ip_opts.dont_fragment(),
                state.ip_opts.ip_options.clone(),
                state.local_port,
            )
        };
        send_with_options(
            source,
            destination_address,
            local_port,
            destination_port,
            tos,
            ttl,
            dont_fragment,
            &ip_options,
            payload,
        )?;
        OUT_DATAGRAMS.fetch_add(1, Ordering::Relaxed);
        Ok(payload.len())
    }

    pub fn receive_msg(&self, max: usize, flags: u64) -> Result<InetReceived, VfsError> {
        const MSG_PEEK: u64 = 2;
        const MSG_DONTWAIT: u64 = 0x40;
        let peek = flags & MSG_PEEK != 0;
        let timeout_us = self.state.lock().rcvtimeo_us;
        let deadline =
            (timeout_us != 0).then(|| crate::time::uptime_micros().saturating_add(timeout_us));
        loop {
            {
                let mut state = self.state.lock();
                if let Some(error) = state.error.take() {
                    return Err(errno_error(error));
                }
                if let Some(datagram) = state.queue.front() {
                    let source =
                        socket::encode_sockaddr_in(datagram.meta.source, datagram.source_port);
                    let full = datagram.payload.len();
                    let truncated = full > max;
                    let payload = datagram.payload[..full.min(max)].to_vec();
                    let mut cmsgs: Vec<CmsgItem> = Vec::new();
                    state
                        .timestamp
                        .push_cmsg(datagram.meta.timestamp_us, &mut cmsgs);
                    let control_truncated = state.ip_opts.collect_cmsgs(
                        &datagram.meta,
                        datagram.destination_port,
                        &mut cmsgs,
                    );
                    if !peek {
                        let datagram = state.queue.pop_front().expect("front");
                        state.rmem = state.rmem.saturating_sub(datagram.payload.len() + OVERHEAD);
                    }
                    return Ok(InetReceived {
                        data: payload,
                        source: Some(source.to_vec()),
                        cmsgs,
                        control_truncated,
                        truncated,
                        full_len: full,
                        eof: false,
                    });
                }
            }
            if flags & MSG_DONTWAIT != 0 || self.nonblock.load(Ordering::Acquire) {
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
        flags: u64,
    ) -> Result<(Vec<u8>, [u8; socket::SOCKADDR_IN_LEN], bool, usize), VfsError> {
        let received = self.receive_msg(max, flags)?;
        let mut source = [0u8; socket::SOCKADDR_IN_LEN];
        if let Some(raw) = received.source {
            source.copy_from_slice(&raw[..socket::SOCKADDR_IN_LEN]);
        }
        Ok((received.data, source, received.truncated, received.full_len))
    }

    pub fn shutdown(&self, _how: u32) -> Result<(), VfsError> {
        let mut state = self.state.lock();
        state.connected = false;
        Ok(())
    }

    pub fn take_error(&self) -> i32 {
        self.state.lock().error.take().unwrap_or(0)
    }

    pub fn is_bound(&self) -> bool {
        self.state.lock().bound
    }

    pub fn setsockopt(&self, level: u64, name: u64, value: &[u8]) -> Result<(), VfsError> {
        match level {
            SOL_IP => self.state.lock().ip_opts.set(SOCK_DGRAM, name, value),
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
                        self.state.lock().reuseaddr = bool_val()?;
                        Ok(())
                    }
                    SO_REUSEPORT => {
                        self.state.lock().reuseport = bool_val()?;
                        Ok(())
                    }
                    SO_BROADCAST => {
                        self.state.lock().broadcast = bool_val()?;
                        Ok(())
                    }
                    SO_DONTROUTE => {
                        self.state.lock().dontroute = bool_val()?;
                        Ok(())
                    }
                    SO_KEEPALIVE => {
                        self.state.lock().keepalive = bool_val()?;
                        Ok(())
                    }
                    SO_DEBUG => {
                        let _ = bool_val()?;
                        Ok(())
                    }
                    SO_RCVBUF => {
                        let size = (u32_val()? as usize).saturating_mul(2).max(MIN_BUFFER);
                        self.state.lock().rcvbuf = size;
                        Ok(())
                    }
                    SO_SNDBUF => {
                        let size = (u32_val()? as usize).saturating_mul(2).max(MIN_BUFFER);
                        self.state.lock().sndbuf = size;
                        Ok(())
                    }
                    SO_SNDBUFFORCE | SO_RCVBUFFORCE => {
                        if !privileged() {
                            return Err(VfsError::NotPermitted);
                        }
                        let size = (u32_val()? as usize).saturating_mul(2).max(MIN_BUFFER);
                        if name == SO_SNDBUFFORCE {
                            self.state.lock().sndbuf = size;
                        } else {
                            self.state.lock().rcvbuf = size;
                        }
                        Ok(())
                    }
                    SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW => {
                        self.state.lock().rcvtimeo_us = parse_timeval_us(value)?;
                        Ok(())
                    }
                    SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW => {
                        self.state.lock().sndtimeo_us = parse_timeval_us(value)?;
                        Ok(())
                    }
                    SO_BINDTODEVICE => {
                        let end = value.iter().position(|&b| b == 0).unwrap_or(value.len());
                        let dev_name = core::str::from_utf8(&value[..end])
                            .map_err(|_| VfsError::InvalidArgument)?;
                        let idx = if dev_name.is_empty() {
                            0
                        } else {
                            super::dev::by_name(dev_name)
                                .ok_or(VfsError::NoDevice)?
                                .ifindex()
                        };
                        self.state.lock().bound_ifindex = idx;
                        Ok(())
                    }
                    SO_TIMESTAMP_OLD | SO_TIMESTAMP_NEW | SO_TIMESTAMPNS_OLD
                    | SO_TIMESTAMPNS_NEW => {
                        let enabled = bool_val()?;
                        self.state.lock().timestamp.set(name, enabled);
                        Ok(())
                    }
                    SO_MARK => {
                        if !privileged() {
                            return Err(VfsError::NotPermitted);
                        }
                        self.state.lock().mark = u32_val()?;
                        Ok(())
                    }
                    SO_PRIORITY => {
                        self.state.lock().priority = u32_val()?;
                        Ok(())
                    }
                    _ => Err(VfsError::NoProtocolOption),
                }
            }
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    pub fn getsockopt(&self, level: u64, name: u64) -> Result<Vec<u8>, VfsError> {
        let int = |value: u32| Ok(value.to_le_bytes().to_vec());
        match level {
            SOL_IP => {
                let state = self.state.lock();
                let peer = state.connected.then_some(state.peer_address);
                state.ip_opts.get(SOCK_DGRAM, ip::IPPROTO_UDP, peer, name)
            }
            SOL_SOCKET => {
                let mut state = self.state.lock();
                match name {
                    SO_TYPE => int(SOCK_DGRAM),
                    SO_DOMAIN => int(socket::AF_INET as u32),
                    SO_PROTOCOL => int(ip::IPPROTO_UDP as u32),
                    SO_SNDBUF => int(state.sndbuf as u32),
                    SO_RCVBUF => int(state.rcvbuf as u32),
                    SO_ACCEPTCONN => int(0),
                    SO_REUSEADDR => int(u32::from(state.reuseaddr)),
                    SO_REUSEPORT => int(u32::from(state.reuseport)),
                    SO_BROADCAST => int(u32::from(state.broadcast)),
                    SO_DONTROUTE => int(u32::from(state.dontroute)),
                    SO_KEEPALIVE => int(u32::from(state.keepalive)),
                    SO_MARK => int(state.mark),
                    SO_PRIORITY => int(state.priority),
                    SO_TIMESTAMP_OLD | SO_TIMESTAMP_NEW | SO_TIMESTAMPNS_OLD
                    | SO_TIMESTAMPNS_NEW => int(state.timestamp.get(name)),
                    SO_RCVTIMEO_OLD | SO_RCVTIMEO_NEW => Ok(encode_timeval_us(state.rcvtimeo_us)),
                    SO_SNDTIMEO_OLD | SO_SNDTIMEO_NEW => Ok(encode_timeval_us(state.sndtimeo_us)),
                    SO_ERROR => int(state.error.take().unwrap_or(0) as u32),
                    _ => Err(VfsError::NoProtocolOption),
                }
            }
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    fn enqueue(&self, mut datagram: Datagram) {
        let mut state = self.state.lock();
        let charge = datagram.payload.len() + OVERHEAD;
        if state.rmem + charge > state.rcvbuf {
            return;
        }
        if state.timestamp.is_enabled() && datagram.meta.timestamp_us == 0 {
            datagram.meta.timestamp_us = crate::time::realtime_micros();
        }
        state.rmem += charge;
        state.queue.push_back(datagram);
        drop(state);
        crate::ipc::wake_pollers();
    }
}

fn errno_error(error: i32) -> VfsError {
    match error {
        111 => VfsError::ConnectionRefused,
        101 => VfsError::NetworkUnreachable,
        113 => VfsError::HostUnreachable,
        105 => VfsError::NoBuffers,
        _ => VfsError::IoError,
    }
}

fn check_port_available(address: Ipv4Addr, port: u16, reuseaddr: bool) -> Result<u16, VfsError> {
    let mut bindings = BINDINGS.lock();
    bindings.retain(|binding| binding.socket.strong_count() != 0);
    let conflict = bindings.iter().any(|binding| {
        binding.port == port
            && (binding.address.is_unspecified()
                || address.is_unspecified()
                || binding.address == address)
    });
    if conflict && !reuseaddr {
        return Err(VfsError::AddressInUse);
    }
    Ok(port)
}

fn allocate_port(address: Ipv4Addr) -> Result<u16, VfsError> {
    for _ in 0..(EPHEMERAL_END - EPHEMERAL_START) as usize + 1 {
        let candidate = NEXT_EPHEMERAL.fetch_add(1, Ordering::Relaxed);
        let port = EPHEMERAL_START as u64
            + (candidate - EPHEMERAL_START as u64) % (EPHEMERAL_END - EPHEMERAL_START) as u64;
        if check_port_available(address, port as u16, false).is_ok() {
            return Ok(port as u16);
        }
    }
    Err(VfsError::AddressInUse)
}

fn register_binding(socket: &Arc<UdpSocket>) -> Result<(), VfsError> {
    let (address, port) = {
        let state = socket.state.lock();
        (state.local_address, state.local_port)
    };
    BINDINGS.lock().push(Binding {
        port,
        address,
        socket: Arc::downgrade(socket),
    });
    Ok(())
}

fn lookup(port: u16, address: Ipv4Addr) -> Option<Arc<UdpSocket>> {
    let bindings = BINDINGS.lock();
    let mut wildcard = None;
    for binding in bindings.iter() {
        if binding.port != port {
            continue;
        }
        let Some(socket) = binding.socket.upgrade() else {
            continue;
        };
        if binding.address == address {
            return Some(socket);
        }
        if binding.address.is_unspecified() {
            wildcard = Some(socket);
        }
    }
    wildcard
}

/// `udp_sendmsg` for sockets: build and transmit one datagram.
pub fn send(
    source: Option<Ipv4Addr>,
    destination: Ipv4Addr,
    source_port: u16,
    destination_port: u16,
    tos: u8,
    ttl: u8,
    payload: &[u8],
) -> Result<(), VfsError> {
    send_with_options(
        source,
        destination,
        source_port,
        destination_port,
        tos,
        ttl,
        false,
        &[],
        payload,
    )
}

fn send_with_options(
    source: Option<Ipv4Addr>,
    destination: Ipv4Addr,
    source_port: u16,
    destination_port: u16,
    tos: u8,
    ttl: u8,
    dont_fragment: bool,
    ip_options: &[u8],
    payload: &[u8],
) -> Result<(), VfsError> {
    let source = source.unwrap_or_else(|| {
        super::fib::lookup(super::ip::IpAddr::V4(destination))
            .and_then(|route| match route.source {
                Some(super::ip::IpAddr::V4(address)) => Some(address),
                _ => addr::first_on(route.ifindex),
            })
            .unwrap_or(Ipv4Addr::UNSPECIFIED)
    });
    let length = UDP_HEADER_LEN + payload.len();
    if length > u16::MAX as usize {
        return Err(VfsError::MessageTooLong);
    }
    let mut skb = SkBuff::alloc(super::skb::DEFAULT_HEADROOM, length).ok_or(VfsError::NoBuffers)?;
    let segment = skb.put(length).ok_or(VfsError::NoBuffers)?;
    segment[0..2].copy_from_slice(&source_port.to_be_bytes());
    segment[2..4].copy_from_slice(&destination_port.to_be_bytes());
    segment[4..6].copy_from_slice(&(length as u16).to_be_bytes());
    segment[6..8].copy_from_slice(&[0, 0]);
    segment[8..].copy_from_slice(payload);
    let seed = ip::pseudo_v4(source, destination, ip::IPPROTO_UDP, length as u16);
    let mut sum = ip::checksum(seed, segment);
    if sum == 0 {
        sum = 0xffff;
    }
    segment[6..8].copy_from_slice(&sum.to_be_bytes());
    let template = TxTemplate {
        tos,
        ttl,
        protocol: ip::IPPROTO_UDP,
        dont_fragment,
    };
    output::send_with_options(Some(source), destination, template, ip_options, skb)
}

/// `udp_rcv`: demultiplex a datagram by port.
pub fn receive(dev: &Arc<NetDevice>, meta: &IpMeta, mut skb: SkBuff) {
    if skb.len() < UDP_HEADER_LEN {
        return;
    }
    let (source_port, destination_port, length, checksum) = {
        let data = skb.data();
        (
            u16::from_be_bytes([data[0], data[1]]),
            u16::from_be_bytes([data[2], data[3]]),
            u16::from_be_bytes([data[4], data[5]]) as usize,
            u16::from_be_bytes([data[6], data[7]]),
        )
    };
    if length < UDP_HEADER_LEN || length > skb.len() {
        return;
    }
    skb.trim(length);
    if checksum != 0 {
        let seed = ip::pseudo_v4(
            meta.source,
            meta.destination,
            ip::IPPROTO_UDP,
            length as u16,
        );
        if ip::checksum(seed, skb.data()) != 0 {
            return;
        }
    }
    let Some(socket) = lookup(destination_port, meta.destination) else {
        // No listener: ICMP port unreachable, quoting ip+udp headers.
        icmp::send_error(
            dev,
            skb.network_slice().unwrap_or_default(),
            icmp::ICMP_DEST_UNREACH,
            icmp::ICMP_PORT_UNREACH,
        );
        return;
    };
    {
        let state = socket.state.lock();
        if state.bound_ifindex != 0 && state.bound_ifindex != dev.ifindex() {
            return;
        }
        if state.connected && (state.peer_port != source_port || state.peer_address != meta.source)
        {
            return;
        }
    }
    let ip_options = if meta.header_len > IPV4_HLEN
        && let Some(net) = skb.network_slice()
        && let Some(raw_opts) = net.get(IPV4_HLEN..meta.header_len)
    {
        raw_opts.to_vec()
    } else {
        Vec::new()
    };
    let payload = skb.data()[UDP_HEADER_LEN..].to_vec();
    socket.enqueue(Datagram {
        payload,
        source_port,
        destination_port,
        meta: IpRxMeta {
            source: meta.source,
            destination: meta.destination,
            spec_dst: addr::first_on(dev.ifindex()).unwrap_or(meta.destination),
            ifindex: dev.ifindex(),
            ttl: meta.ttl,
            tos: meta.tos,
            ip_options,
            timestamp_us: 0,
        },
    });
    IN_DATAGRAMS.fetch_add(1, Ordering::Relaxed);
}

/// ICMP errors for UDP sockets (`udp_err`).
pub fn error(event: &icmp::IcmpError<'_>) {
    let Some(header) = ipv4_header(event.quoted) else {
        return;
    };
    if header.protocol != ip::IPPROTO_UDP || event.quoted.len() < header.header_len() + 8 {
        return;
    }
    let udp = &event.quoted[header.header_len()..];
    let source_port = u16::from_be_bytes([udp[0], udp[1]]);
    let destination_port = u16::from_be_bytes([udp[2], udp[3]]);
    // The socket is identified by the *source* port of the quoted datagram.
    let Some(socket) = lookup(source_port, header.source) else {
        return;
    };
    let errno = match (event.icmp_type, event.code) {
        (icmp::ICMP_DEST_UNREACH, icmp::ICMP_PORT_UNREACH) => 111,
        (icmp::ICMP_DEST_UNREACH, icmp::ICMP_HOST_UNREACH) => 113,
        (icmp::ICMP_DEST_UNREACH, icmp::ICMP_NET_UNREACH) => 101,
        _ => 101,
    };
    let mut state = socket.state.lock();
    if state.connected && state.peer_port == destination_port {
        state.error = Some(errno);
    }
    drop(state);
    crate::ipc::wake_pollers();
}

fn ipv4_header(bytes: &[u8]) -> Option<super::ipv4::header::Ipv4Header> {
    super::ipv4::header::Ipv4Header::parse(bytes)
}

/// Statistics for diagnostics and the `sock_diag` implementation.
pub fn socket_count() -> usize {
    BINDINGS
        .lock()
        .iter()
        .filter(|binding| binding.socket.strong_count() != 0)
        .count()
}

pub fn in_datagrams() -> u64 {
    IN_DATAGRAMS.load(Ordering::Relaxed)
}

pub fn out_datagrams() -> u64 {
    OUT_DATAGRAMS.load(Ordering::Relaxed)
}

/// Snapshot used by `sock_diag`.
pub fn diagnostics() -> Vec<(u64, Ipv4Addr, u16, Ipv4Addr, u16, bool)> {
    BINDINGS
        .lock()
        .iter()
        .filter_map(|binding| {
            let socket = binding.socket.upgrade()?;
            let state = socket.state.lock();
            Some((
                socket.ino,
                state.local_address,
                state.local_port,
                state.peer_address,
                state.peer_port,
                state.connected,
            ))
        })
        .collect()
}

impl Inode for UdpSocket {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::Socket, 0o644, 0))
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        let (payload, _source, _truncated, _full) = self.receive(buffer.len(), 0)?;
        buffer[..payload.len()].copy_from_slice(&payload);
        Ok(payload.len())
    }

    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        self.send_to(buffer, None, 0)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(self as *const Self);
    }

    fn poll(&self) -> u32 {
        const POLLIN: u32 = 1;
        const POLLOUT: u32 = 4;
        const POLLERR: u32 = 8;
        let state = self.state.lock();
        let mut events = POLLOUT;
        if !state.queue.is_empty() || state.error.is_some() {
            events |= POLLIN;
        }
        if state.error.is_some() {
            events |= POLLERR;
        }
        events
    }

    fn ioctl(&self, cmd: u64, _argument: u64) -> Result<u64, VfsError> {
        const FIONREAD: u64 = 0x541B;
        match cmd {
            FIONREAD => Ok(self
                .state
                .lock()
                .queue
                .front()
                .map_or(0, |datagram| datagram.payload.len()) as u64),
            _ => Err(VfsError::InvalidArgument),
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.set_nonblock(nonblock);
        Ok(())
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        let port = self.state.lock().local_port;
        BINDINGS.lock().retain(|binding| {
            if binding.port == port && binding.socket.as_ptr() == self.me.as_ptr() {
                false
            } else {
                binding.socket.strong_count() != 0
            }
        });
        crate::ipc::poll_kick();
    }
}

/// Family constructor used by `AF_INET`/`SOCK_DGRAM`.
pub fn create_dgram(
    kind: u32,
    protocol: u32,
    nonblock: bool,
) -> Result<Arc<crate::vfs::OpenFile>, VfsError> {
    if kind != SOCK_DGRAM || (protocol != 0 && protocol != ip::IPPROTO_UDP as u32) {
        return Err(VfsError::ProtocolType);
    }
    let socket = UdpSocket::create(nonblock);
    Ok(socket::socket_file(socket, "socket:udp", nonblock))
}

/// Registration called from `ipv4::init`.
pub fn init() {
    input::register_protocol(ip::IPPROTO_UDP, receive);
    icmp::register_error_handler(ip::IPPROTO_UDP, error);
}
