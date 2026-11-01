/*
 *
 *       src/ipc/unix.rs
 *       AF_UNIX single-namespace sockets: stream, datagram and seqpacket
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Single-namespace AF_UNIX sockets with pathname/abstract/unnamed addressing,
//! stream/datagram/seqpacket delivery, SCM_RIGHTS, SCM_CREDENTIALS and
//! SO_PEERCRED.  Endpoint links are weak so a closed endpoint is observed as
//! EOF/EPIPE by its peer instead of keeping both objects alive.
//!
//! Blocking operations poll with a short sleep: the scheduler has no wait
//! channel for socket readiness yet, so a wakeup is at most one millisecond
//! late.  No spin lock is held across a sleep.

use crate::{
    sched,
    sync::SpinLock,
    vfs::{self, Inode, InodeKind, OpenFile, Stat, VfsError},
};
use alloc::{
    collections::{BTreeMap, VecDeque},
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

pub const AF_UNIX: u16 = 1;
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const SOCK_SEQPACKET: u32 = 5;
pub const SOCK_CLOEXEC: u32 = 0o2000000;
pub const SOCK_NONBLOCK: u32 = 0o4000;

pub const SOL_SOCKET: i32 = 1;
pub const SCM_RIGHTS: i32 = 1;
pub const SCM_CREDENTIALS: i32 = 2;

pub const SO_TYPE: i32 = 3;
pub const SO_ERROR: i32 = 4;
pub const SO_SNDBUF: i32 = 7;
pub const SO_RCVBUF: i32 = 8;
pub const SO_PASSCRED: i32 = 16;
pub const SO_PEERCRED: i32 = 17;
pub const SO_ACCEPTCONN: i32 = 30;
pub const SO_PROTOCOL: i32 = 38;
pub const SO_DOMAIN: i32 = 39;

const STREAM_CAPACITY: usize = 64 * 1024;
const DEFAULT_BUFFER: usize = 212_992;
const MAX_BACKLOG: usize = 4096;
const SOCKADDR_UN_SIZE: usize = 110;

// poll(2) event bits shared with eventfd, pipes and epoll.
pub const POLLIN: u32 = 0x001;
pub const POLLOUT: u32 = 0x004;
pub const POLLERR: u32 = 0x008;
pub const POLLHUP: u32 = 0x010;
pub const POLLNVAL: u32 = 0x020;
pub const POLLRDHUP: u32 = 0x2000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SocketKind {
    Stream,
    Dgram,
    Seqpacket,
}

impl SocketKind {
    pub fn from_raw(kind: u32) -> Result<Self, VfsError> {
        match kind {
            SOCK_STREAM => Ok(Self::Stream),
            SOCK_DGRAM => Ok(Self::Dgram),
            SOCK_SEQPACKET => Ok(Self::Seqpacket),
            _ => Err(VfsError::AddressFamilyNotSupported),
        }
    }

    pub fn raw(self) -> u32 {
        match self {
            Self::Stream => SOCK_STREAM,
            Self::Dgram => SOCK_DGRAM,
            Self::Seqpacket => SOCK_SEQPACKET,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UnixAddress {
    Unnamed,
    Pathname(String),
    Abstract(Vec<u8>),
}

impl UnixAddress {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SOCKADDR_UN_SIZE);
        out.extend_from_slice(&AF_UNIX.to_le_bytes());
        match self {
            Self::Unnamed => {}
            Self::Pathname(path) => {
                let bytes = path.as_bytes();
                let length = bytes.len().min(107);
                out.extend_from_slice(&bytes[..length]);
                out.push(0);
            }
            Self::Abstract(name) => {
                out.push(0);
                let space = 107.min(name.len());
                out.extend_from_slice(&name[..space]);
            }
        }
        out
    }

    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Unnamed => 2,
            Self::Pathname(path) => 2 + path.as_bytes().len().min(107) + 1,
            Self::Abstract(name) => 2 + 1 + name.len().min(107),
        }
    }

    pub fn is_unnamed(&self) -> bool {
        matches!(self, Self::Unnamed)
    }
}

/// Decode `sockaddr_un`.  `cwd` resolves relative pathnames at call time.
pub fn decode_sockaddr(bytes: &[u8], cwd: &str) -> Result<UnixAddress, VfsError> {
    if bytes.len() < 2 || bytes.len() > SOCKADDR_UN_SIZE {
        return Err(VfsError::InvalidArgument);
    }
    let family = u16::from_le_bytes([bytes[0], bytes[1]]);
    if family != AF_UNIX {
        return Err(VfsError::AddressFamilyNotSupported);
    }
    let path = &bytes[2..];
    if path.is_empty() {
        return Ok(UnixAddress::Unnamed);
    }
    if path[0] == 0 {
        // Linux abstract names include every byte supplied, embedded NULs and
        // the trailing NUL of the address, in the name.
        return Ok(UnixAddress::Abstract(path[1..].to_vec()));
    }
    let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    let name = core::str::from_utf8(&path[..end]).map_err(|_| VfsError::InvalidArgument)?;
    if name.is_empty() {
        return Err(VfsError::InvalidArgument);
    }
    let absolute = vfs::normalize_path(cwd, name);
    if absolute.as_bytes().len() >= 108 {
        return Err(VfsError::NameTooLong);
    }
    Ok(UnixAddress::Pathname(absolute))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Credentials {
    pub fn encode(self) -> [u8; 12] {
        let mut out = [0u8; 12];
        out[0..4].copy_from_slice(&self.pid.to_le_bytes());
        out[4..8].copy_from_slice(&self.uid.to_le_bytes());
        out[8..12].copy_from_slice(&self.gid.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, VfsError> {
        if bytes.len() < 12 {
            return Err(VfsError::InvalidArgument);
        }
        Ok(Self {
            pid: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            uid: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            gid: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        })
    }
}

pub fn current_credentials() -> Credentials {
    Credentials {
        pid: sched::current()
            .map(|task| task.stats().tgid as u32)
            .unwrap_or(0),
        uid: 0,
        gid: 0,
    }
}

struct Barrier {
    offset: u64,
    descriptors: Vec<Arc<OpenFile>>,
    credentials: Option<Credentials>,
}

#[derive(Default)]
struct StreamQueue {
    data: VecDeque<u8>,
    produced: u64,
    consumed: u64,
    barriers: VecDeque<Barrier>,
}

struct Packet {
    data: Vec<u8>,
    source: UnixAddress,
    credentials: Credentials,
    descriptors: Vec<Arc<OpenFile>>,
}

struct State {
    kind: SocketKind,
    bound: Option<UnixAddress>,
    peer: Option<Weak<UnixSocket>>,
    peer_address: Option<UnixAddress>,
    peer_credentials: Option<Credentials>,
    listening: bool,
    backlog: usize,
    pending: VecDeque<Arc<UnixSocket>>,
    read_generation: u64,
    stream: StreamQueue,
    packets: VecDeque<Packet>,
    packet_bytes: usize,
    read_shutdown: bool,
    write_shutdown: bool,
    peer_read_closed: bool,
    peer_write_closed: bool,
    peer_gone: bool,
    passcred: bool,
    sndbuf: usize,
    rcvbuf: usize,
    error: i32,
}

impl State {
    fn pending_bytes(&self) -> usize {
        self.stream.data.len()
    }
}

pub struct UnixSocket {
    state: SpinLock<State>,
    nonblock: AtomicBool,
    ino: u64,
    owner: Credentials,
    address: SpinLock<Option<UnixAddress>>,
}

impl UnixSocket {
    fn new(kind: SocketKind, nonblock: bool) -> Arc<Self> {
        Arc::new(Self {
            state: SpinLock::new(State {
                kind,
                bound: None,
                peer: None,
                peer_address: None,
                peer_credentials: None,
                listening: false,
                backlog: 0,
                pending: VecDeque::new(),
                read_generation: 0,
                stream: StreamQueue::default(),
                packets: VecDeque::new(),
                packet_bytes: 0,
                read_shutdown: false,
                write_shutdown: false,
                peer_read_closed: false,
                peer_write_closed: false,
                peer_gone: false,
                passcred: false,
                sndbuf: DEFAULT_BUFFER,
                rcvbuf: DEFAULT_BUFFER,
                error: 0,
            }),
            nonblock: AtomicBool::new(nonblock),
            ino: vfs::tree::ino(),
            owner: current_credentials(),
            address: SpinLock::new(None),
        })
    }

    pub fn pair(kind: SocketKind, nonblock: bool) -> Result<(Arc<Self>, Arc<Self>), VfsError> {
        let left = Self::new(kind, nonblock);
        let right = Self::new(kind, nonblock);
        let credentials = current_credentials();
        {
            let mut state = left.state.lock();
            state.peer = Some(Arc::downgrade(&right));
            state.peer_credentials = Some(credentials);
        }
        {
            let mut state = right.state.lock();
            state.peer = Some(Arc::downgrade(&left));
            state.peer_credentials = Some(credentials);
        }
        Ok((left, right))
    }

    pub fn new_socket(kind: SocketKind, nonblock: bool) -> Arc<Self> {
        Self::new(kind, nonblock)
    }

    pub fn kind(&self) -> SocketKind {
        self.state.lock().kind
    }

    pub fn set_nonblock(&self, nonblock: bool) {
        self.nonblock.store(nonblock, Ordering::Release);
    }

    fn nonblocking(&self, flag: bool) -> bool {
        flag || self.nonblock.load(Ordering::Acquire)
    }

    pub fn bind(self: &Arc<Self>, address: UnixAddress) -> Result<(), VfsError> {
        if address.is_unnamed() {
            let address = autobind(self)?;
            let mut state = self.state.lock();
            state.bound = Some(address.clone());
            *self.address.lock() = Some(address);
            return Ok(());
        }
        {
            let state = self.state.lock();
            if state.bound.is_some() {
                return Err(VfsError::InvalidArgument);
            }
        }
        if let UnixAddress::Pathname(path) = &address
            && vfs::lookup_abs(path, false).is_ok()
        {
            return Err(VfsError::AddressInUse);
        }
        register(self, &address)?;
        if let UnixAddress::Pathname(path) = &address
            && let Err(error) = vfs::create_socket_abs(path)
        {
            unregister(&address, self.ino);
            return Err(if error == VfsError::AlreadyExists {
                VfsError::AddressInUse
            } else {
                error
            });
        }
        let mut state = self.state.lock();
        state.bound = Some(address.clone());
        *self.address.lock() = Some(address);
        Ok(())
    }

    pub fn listen(self: &Arc<Self>, backlog: u32) -> Result<(), VfsError> {
        if !matches!(self.kind(), SocketKind::Stream | SocketKind::Seqpacket) {
            return Err(VfsError::NotSupported);
        }
        {
            let state = self.state.lock();
            if state.listening {
                return Err(VfsError::AddressInUse);
            }
            if state.peer.is_some() {
                return Err(VfsError::InvalidArgument);
            }
            if state.bound.is_none() {
                drop(state);
                self.bind(UnixAddress::Unnamed)?;
            }
        }
        let mut state = self.state.lock();
        state.listening = true;
        state.backlog = (backlog as usize).clamp(1, MAX_BACKLOG);
        Ok(())
    }

    pub fn is_listening(&self) -> bool {
        self.state.lock().listening
    }

    pub fn accept(self: &Arc<Self>, nonblock: bool) -> Result<Arc<UnixSocket>, VfsError> {
        {
            let state = self.state.lock();
            if !state.listening {
                return Err(VfsError::InvalidArgument);
            }
        }
        loop {
            {
                let mut state = self.state.lock();
                if let Some(peer) = state.pending.pop_front() {
                    return Ok(peer);
                }
            }
            if nonblock || self.nonblock.load(Ordering::Acquire) {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::ipc::block_briefly();
        }
    }

    pub fn is_nonblock(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    pub fn connect(
        self: &Arc<Self>,
        target: &Arc<UnixSocket>,
        target_address: UnixAddress,
    ) -> Result<(), VfsError> {
        if self.state.lock().listening {
            return Err(VfsError::InvalidArgument);
        }
        let kind = self.kind();
        if kind != target.kind() {
            return Err(VfsError::ProtocolType);
        }
        if matches!(kind, SocketKind::Dgram) {
            let mut state = self.state.lock();
            if state.peer.is_some() {
                return Err(VfsError::AlreadyConnected);
            }
            state.peer = Some(Arc::downgrade(target));
            state.peer_address = Some(target_address);
            state.peer_credentials = Some(target.owner);
            return Ok(());
        }
        if !target.is_listening() {
            return Err(VfsError::ConnectionRefused);
        }
        if self.state.lock().peer.is_some() {
            return Err(VfsError::AlreadyConnected);
        }
        let bound = {
            let state = self.state.lock();
            state.bound.clone()
        };
        let client_address = match bound {
            Some(address) => address,
            None => {
                let address = autobind(self)?;
                let mut state = self.state.lock();
                state.bound = Some(address.clone());
                *self.address.lock() = Some(address.clone());
                address
            }
        };
        let server = Self::new(kind, false);
        let client_credentials = current_credentials();
        {
            let mut server_state = server.state.lock();
            server_state.peer = Some(Arc::downgrade(self));
            server_state.peer_address = Some(client_address);
            server_state.peer_credentials = Some(client_credentials);
        }
        {
            let mut state = self.state.lock();
            state.peer = Some(Arc::downgrade(&server));
            state.peer_address = Some(target_address);
            state.peer_credentials = Some(target.owner);
        }
        let nonblock = self.nonblock.load(Ordering::Acquire);
        loop {
            {
                let mut listener = target.state.lock();
                if !listener.listening {
                    return Err(VfsError::ConnectionRefused);
                }
                if listener.pending.len() < listener.backlog {
                    listener.pending.push_back(server);
                    listener.read_generation = listener.read_generation.wrapping_add(1);
                    return Ok(());
                }
            }
            if nonblock {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::ipc::block_briefly();
        }
    }

    pub fn send(
        &self,
        bytes: &[u8],
        descriptors: &[Arc<OpenFile>],
        credentials: Option<Credentials>,
        dontwait: bool,
    ) -> Result<usize, VfsError> {
        let kind = self.kind();
        match kind {
            SocketKind::Stream => {
                if bytes.is_empty() && !descriptors.is_empty() {
                    return Err(VfsError::InvalidArgument);
                }
                if bytes.is_empty() && credentials.is_some() {
                    return Err(VfsError::InvalidArgument);
                }
                self.send_stream(bytes, descriptors, credentials, dontwait)
            }
            SocketKind::Dgram | SocketKind::Seqpacket => {
                let target = {
                    let state = self.state.lock();
                    state
                        .peer
                        .as_ref()
                        .and_then(Weak::upgrade)
                        .ok_or(VfsError::NotConnected)?
                };
                let source = self
                    .state
                    .lock()
                    .bound
                    .clone()
                    .unwrap_or(UnixAddress::Unnamed);
                target.enqueue_packet(
                    bytes,
                    descriptors,
                    credentials.unwrap_or_else(current_credentials),
                    source,
                    self.state.lock().sndbuf,
                    self.nonblocking(dontwait),
                )?;
                Ok(bytes.len())
            }
        }
    }

    fn send_stream(
        &self,
        bytes: &[u8],
        descriptors: &[Arc<OpenFile>],
        credentials: Option<Credentials>,
        dontwait: bool,
    ) -> Result<usize, VfsError> {
        if bytes.is_empty() {
            return Ok(0);
        }
        {
            let state = self.state.lock();
            if state.peer.is_none() {
                return Err(VfsError::NotConnected);
            }
            if state.write_shutdown {
                return Err(VfsError::BrokenPipe);
            }
        }
        let mut wait = crate::ipc::pollwait::PollWait::new();
        loop {
            wait.arm_object(self as *const Self);
            let peer = {
                let state = self.state.lock();
                state.peer.as_ref().and_then(Weak::upgrade)
            };
            let Some(peer) = peer else {
                // The weak link existed but the peer object is gone.
                return Err(VfsError::BrokenPipe);
            };
            {
                let mut target = peer.state.lock();
                if target.read_shutdown || target.peer_gone {
                    return Err(VfsError::BrokenPipe);
                }
                // Streams have no message-size atomicity requirement. A
                // request larger than the queue must make partial progress,
                // otherwise even an empty queue can never satisfy it.
                let count = bytes.len().min(STREAM_CAPACITY - target.stream.data.len());
                if count != 0 {
                    target.stream.data.extend(bytes[..count].iter().copied());
                    target.read_generation = target.read_generation.wrapping_add(1);
                    target.stream.produced = target.stream.produced.saturating_add(count as u64);
                    if !descriptors.is_empty() || credentials.is_some() {
                        let offset = target.stream.produced;
                        // With SO_PASSCRED the receiver wants credentials on
                        // every message, even when the sender did not pass
                        // SCM_CREDENTIALS explicitly.
                        let credentials = if credentials.is_none() && target.passcred {
                            Some(current_credentials())
                        } else {
                            credentials
                        };
                        target.stream.barriers.push_back(Barrier {
                            offset,
                            descriptors: descriptors.to_vec(),
                            credentials,
                        });
                    }
                    drop(target);
                    crate::ipc::wake_object(Arc::as_ptr(&peer));
                    return Ok(count);
                }
            }
            if self.nonblocking(dontwait) {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    }

    fn enqueue_packet(
        &self,
        bytes: &[u8],
        descriptors: &[Arc<OpenFile>],
        credentials: Credentials,
        source: UnixAddress,
        sndbuf: usize,
        dontwait: bool,
    ) -> Result<(), VfsError> {
        if bytes.len() > sndbuf {
            return Err(VfsError::MessageTooLong);
        }
        let cost = bytes.len() + 32;
        let mut wait = crate::ipc::pollwait::PollWait::new();
        loop {
            wait.arm_object(self as *const Self);
            {
                let mut state = self.state.lock();
                if state.read_shutdown || state.peer_gone {
                    return Err(VfsError::BrokenPipe);
                }
                if state.kind == SocketKind::Dgram || state.kind == SocketKind::Seqpacket {
                    if state.packets.is_empty()
                        || state.packet_bytes.saturating_add(cost) <= state.rcvbuf
                    {
                        state.packet_bytes += cost;
                        state.packets.push_back(Packet {
                            data: bytes.to_vec(),
                            source: source.clone(),
                            credentials,
                            descriptors: descriptors.to_vec(),
                        });
                        state.read_generation = state.read_generation.wrapping_add(1);
                        drop(state);
                        crate::ipc::wake_object(self as *const Self);
                        return Ok(());
                    }
                } else {
                    return Err(VfsError::ProtocolType);
                }
            }
            if dontwait {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    }

    /// Send a datagram to an explicitly named destination.
    pub fn send_to(
        self: &Arc<Self>,
        target: &Arc<UnixSocket>,
        _target_address: UnixAddress,
        bytes: &[u8],
        descriptors: &[Arc<OpenFile>],
        credentials: Option<Credentials>,
        dontwait: bool,
    ) -> Result<usize, VfsError> {
        if self.kind() != SocketKind::Dgram {
            return Err(VfsError::AlreadyConnected);
        }
        if target.kind() != SocketKind::Dgram {
            return Err(VfsError::ProtocolType);
        }
        let source = self
            .state
            .lock()
            .bound
            .clone()
            .unwrap_or(UnixAddress::Unnamed);
        target.enqueue_packet(
            bytes,
            descriptors,
            credentials.unwrap_or_else(current_credentials),
            source,
            self.state.lock().sndbuf,
            self.nonblocking(dontwait),
        )?;
        Ok(bytes.len())
    }

    /// Receive at most `max` bytes.  `peek` leaves the queue untouched.
    pub fn receive(&self, max: usize, peek: bool, dontwait: bool) -> Result<Received, VfsError> {
        match self.kind() {
            SocketKind::Stream => self.receive_stream(max, peek, dontwait),
            SocketKind::Dgram | SocketKind::Seqpacket => self.receive_packet(max, peek, dontwait),
        }
    }

    fn receive_stream(&self, max: usize, peek: bool, dontwait: bool) -> Result<Received, VfsError> {
        if max == 0 {
            return Ok(Received::default());
        }
        if self.state.lock().peer.is_none() {
            return Err(VfsError::NotConnected);
        }
        let mut wait = crate::ipc::pollwait::PollWait::new();
        loop {
            wait.arm_object(self as *const Self);
            {
                let mut state = self.state.lock();
                if state.read_shutdown {
                    return Ok(Received {
                        eof: true,
                        ..Received::default()
                    });
                }
                if !state.stream.data.is_empty() {
                    let mut count = max.min(state.stream.data.len());
                    if let Some(barrier) = state.stream.barriers.front() {
                        let boundary =
                            barrier.offset.saturating_sub(state.stream.consumed) as usize;
                        count = count.min(boundary.max(0));
                    }
                    let bytes: Vec<u8> = state.stream.data.iter().take(count).copied().collect();
                    let mut descriptors = Vec::new();
                    let mut credentials = None;
                    let barrier_ready = state.stream.barriers.front().is_some_and(|barrier| {
                        barrier.offset == state.stream.consumed + count as u64
                    });
                    if barrier_ready && peek {
                        if let Some(barrier) = state.stream.barriers.front() {
                            descriptors = barrier.descriptors.clone();
                            credentials = barrier.credentials;
                        }
                    } else if barrier_ready {
                        if let Some(barrier) = state.stream.barriers.pop_front() {
                            descriptors = barrier.descriptors;
                            credentials = barrier.credentials;
                        }
                    }
                    if !peek {
                        // Only a full queue can have a sender waiting on space.
                        let was_full = state.stream.data.len() >= STREAM_CAPACITY;
                        for _ in 0..count {
                            state.stream.data.pop_front();
                        }
                        state.stream.consumed = state.stream.consumed.saturating_add(count as u64);
                        if was_full {
                            let peer = state.peer.as_ref().and_then(Weak::upgrade);
                            drop(state);
                            if let Some(peer) = peer {
                                crate::ipc::wake_object(Arc::as_ptr(&peer));
                            }
                        }
                    }
                    return Ok(Received {
                        bytes,
                        descriptors,
                        credentials,
                        ..Received::default()
                    });
                }
                if state.peer_write_closed || state.peer_gone {
                    return Ok(Received {
                        eof: true,
                        ..Received::default()
                    });
                }
            }
            if self.nonblocking(dontwait) {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    }

    fn receive_packet(&self, max: usize, peek: bool, dontwait: bool) -> Result<Received, VfsError> {
        if self.kind() == SocketKind::Seqpacket && self.state.lock().peer.is_none() {
            return Err(VfsError::NotConnected);
        }
        let mut wait = crate::ipc::pollwait::PollWait::new();
        loop {
            wait.arm_object(self as *const Self);
            {
                let mut state = self.state.lock();
                if state.read_shutdown {
                    return Ok(Received {
                        eof: true,
                        ..Received::default()
                    });
                }
                if let Some(front) = state.packets.front() {
                    let full_len = front.data.len();
                    let count = max.min(full_len);
                    let bytes = front.data[..count].to_vec();
                    let truncated = count < full_len;
                    if peek {
                        return Ok(Received {
                            bytes,
                            descriptors: front.descriptors.clone(),
                            credentials: Some(front.credentials),
                            source: Some(front.source.clone()),
                            truncated,
                            full_len,
                            ..Received::default()
                        });
                    }
                    let packet = state.packets.pop_front().unwrap();
                    state.packet_bytes = state.packet_bytes.saturating_sub(packet.data.len() + 32);
                    drop(state);
                    crate::ipc::wake_object(self as *const Self);
                    return Ok(Received {
                        bytes,
                        descriptors: packet.descriptors,
                        credentials: Some(packet.credentials),
                        source: Some(packet.source),
                        truncated,
                        full_len,
                        ..Received::default()
                    });
                }
                if state.kind == SocketKind::Seqpacket
                    && (state.peer_write_closed || state.peer_gone)
                {
                    return Ok(Received {
                        eof: true,
                        ..Received::default()
                    });
                }
            }
            if self.nonblocking(dontwait) {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    }

    pub fn shutdown(&self, how: u32) -> Result<(), VfsError> {
        if how > 2 {
            return Err(VfsError::InvalidArgument);
        }
        if self.kind() == SocketKind::Dgram && !matches!(how, 0 | 1 | 2) {
            return Err(VfsError::InvalidArgument);
        }
        let mut state = self.state.lock();
        if how == 0 || how == 2 {
            state.read_shutdown = true;
        }
        if how == 1 || how == 2 {
            state.write_shutdown = true;
        }
        let peer = state.peer.as_ref().and_then(Weak::upgrade);
        drop(state);
        if let Some(peer) = peer {
            let mut peer_state = peer.state.lock();
            if how == 0 || how == 2 {
                peer_state.peer_read_closed = true;
            }
            if how == 1 || how == 2 {
                peer_state.peer_write_closed = true;
            }
        }
        Ok(())
    }

    pub fn peer_address(&self) -> Option<UnixAddress> {
        self.state.lock().peer_address.clone()
    }

    pub fn local_address(&self) -> UnixAddress {
        self.state
            .lock()
            .bound
            .clone()
            .unwrap_or(UnixAddress::Unnamed)
    }

    pub fn peer_credentials(&self) -> Option<Credentials> {
        self.state.lock().peer_credentials
    }

    pub fn has_peer(&self) -> bool {
        self.state.lock().peer_credentials.is_some()
    }

    pub fn set_passcred(&self, enable: bool) -> Result<(), VfsError> {
        self.state.lock().passcred = enable;
        Ok(())
    }

    pub fn passcred(&self) -> bool {
        self.state.lock().passcred
    }

    pub fn set_buffer(&self, sndbuf: Option<usize>, rcvbuf: Option<usize>) {
        let mut state = self.state.lock();
        if let Some(size) = sndbuf {
            state.sndbuf = size.clamp(4096, 4 * 1024 * 1024);
        }
        if let Some(size) = rcvbuf {
            state.rcvbuf = size.clamp(4096, 4 * 1024 * 1024);
        }
    }

    pub fn buffers(&self) -> (usize, usize) {
        let state = self.state.lock();
        (state.sndbuf, state.rcvbuf)
    }

    pub fn take_error(&self) -> i32 {
        let mut state = self.state.lock();
        core::mem::take(&mut state.error)
    }

    pub fn pending_readable(&self) -> bool {
        let state = self.state.lock();
        if state.listening {
            return !state.pending.is_empty();
        }
        match state.kind {
            SocketKind::Stream => {
                !state.stream.data.is_empty()
                    || state.peer_write_closed
                    || state.peer_gone
                    || state.read_shutdown
            }
            SocketKind::Dgram | SocketKind::Seqpacket => {
                !state.packets.is_empty()
                    || (state.kind == SocketKind::Seqpacket
                        && (state.peer_write_closed || state.peer_gone))
                    || state.read_shutdown
            }
        }
    }

    pub fn pending_write_ready(&self) -> bool {
        let state = self.state.lock();
        if state.listening {
            return false;
        }
        if state.write_shutdown
            || state.peer_read_closed
            || state.peer_gone
            || matches!(state.kind, SocketKind::Stream | SocketKind::Seqpacket)
                && state.peer.is_none()
        {
            return false;
        }
        match state.kind {
            SocketKind::Dgram | SocketKind::Seqpacket => true,
            SocketKind::Stream => {
                let Some(peer) = state.peer.as_ref().and_then(Weak::upgrade) else {
                    return false;
                };
                let Some(peer_state) = peer.state.try_lock() else {
                    return true;
                };
                !peer_state.read_shutdown
                    && !peer_state.peer_gone
                    && peer_state.stream.data.len() < STREAM_CAPACITY
            }
        }
    }

    fn readable_bytes(&self) -> usize {
        let state = self.state.lock();
        match state.kind {
            SocketKind::Stream => state.pending_bytes(),
            SocketKind::Dgram | SocketKind::Seqpacket => {
                state.packets.front().map_or(0, |packet| packet.data.len())
            }
        }
    }
}

#[derive(Default)]
pub struct Received {
    pub bytes: Vec<u8>,
    pub descriptors: Vec<Arc<OpenFile>>,
    pub credentials: Option<Credentials>,
    pub source: Option<UnixAddress>,
    pub truncated: bool,
    pub full_len: usize,
    pub eof: bool,
}

impl Inode for UnixSocket {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::Socket, 0o600, 0))
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        let received = self.receive(buf.len(), false, false)?;
        if received.eof {
            return Ok(0);
        }
        let count = received.bytes.len().min(buf.len());
        buf[..count].copy_from_slice(&received.bytes[..count]);
        Ok(count)
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> Result<usize, VfsError> {
        // The inode API has no Arc<Self>; connected sends do not need one
        // because packet queues belong to the peer and stream sends look the
        // peer up through the weak link.
        self.send(buf, &[], None, false)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(self as *const Self);
    }

    fn poll(&self) -> u32 {
        let mut events = 0;
        if self.pending_readable() {
            events |= POLLIN;
        }
        if self.pending_write_ready() {
            events |= POLLOUT;
        }
        let state = self.state.lock();
        if state.error != 0 {
            events |= POLLERR;
        }
        if state.peer_gone {
            events |= POLLHUP | POLLRDHUP;
        } else if state.peer_write_closed {
            events |= POLLRDHUP | POLLIN;
        }
        events
    }

    fn poll_read_generation(&self) -> u64 {
        self.state.lock().read_generation
    }

    fn ioctl(&self, cmd: u64, _arg: u64) -> Result<u64, VfsError> {
        const FIONREAD: u64 = 0x541B;
        match cmd {
            FIONREAD => Ok(self.readable_bytes() as u64),
            _ => Err(VfsError::InvalidArgument),
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.set_nonblock(nonblock);
        Ok(())
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        let bound = self.state.lock().bound.clone();
        let peer = self.state.lock().peer.clone();
        if let Some(peer) = peer.and_then(|weak| weak.upgrade()) {
            let mut state = peer.state.lock();
            state.peer_gone = true;
            state.peer_write_closed = true;
            state.peer_read_closed = true;
        }
        // Runs on the task exit path too, where no close syscall announces it.
        crate::ipc::poll_kick();
        if let Some(address) = bound {
            unregister(&address, self.ino);
        }
    }
}

struct RegistryEntry {
    ino: u64,
    socket: Weak<UnixSocket>,
}

struct Registry {
    paths: BTreeMap<String, RegistryEntry>,
    abstract_names: BTreeMap<Vec<u8>, RegistryEntry>,
    next_autobind: u64,
}

impl Registry {
    const fn new() -> Self {
        Self {
            paths: BTreeMap::new(),
            abstract_names: BTreeMap::new(),
            next_autobind: 1,
        }
    }
}

static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry::new());

fn register(socket: &Arc<UnixSocket>, address: &UnixAddress) -> Result<(), VfsError> {
    let mut registry = REGISTRY.lock();
    registry
        .paths
        .retain(|_, entry| entry.socket.strong_count() != 0);
    registry
        .abstract_names
        .retain(|_, entry| entry.socket.strong_count() != 0);
    let entry = RegistryEntry {
        ino: socket.ino,
        socket: Arc::downgrade(socket),
    };
    match address {
        UnixAddress::Unnamed => Ok(()),
        UnixAddress::Pathname(path) => {
            if registry.paths.contains_key(path) {
                return Err(VfsError::AddressInUse);
            }
            registry.paths.insert(path.clone(), entry);
            Ok(())
        }
        UnixAddress::Abstract(name) => {
            if registry.abstract_names.contains_key(name) {
                return Err(VfsError::AddressInUse);
            }
            registry.abstract_names.insert(name.clone(), entry);
            Ok(())
        }
    }
}

fn unregister(address: &UnixAddress, ino: u64) {
    let mut registry = REGISTRY.lock();
    match address {
        UnixAddress::Unnamed => {}
        UnixAddress::Pathname(path) => {
            if registry
                .paths
                .get(path)
                .is_some_and(|entry| entry.ino == ino)
            {
                registry.paths.remove(path);
            }
        }
        UnixAddress::Abstract(name) => {
            if registry
                .abstract_names
                .get(name)
                .is_some_and(|entry| entry.ino == ino)
            {
                registry.abstract_names.remove(name);
            }
        }
    }
}

fn autobind(socket: &Arc<UnixSocket>) -> Result<UnixAddress, VfsError> {
    let mut registry = REGISTRY.lock();
    for _ in 0..(1 << 20) {
        let value = registry.next_autobind;
        registry.next_autobind = registry.next_autobind.wrapping_add(1).max(1);
        let name = format!("{:05x}", value & 0xf_ffff).into_bytes();
        if !registry.abstract_names.contains_key(&name) {
            registry.abstract_names.insert(
                name.clone(),
                RegistryEntry {
                    ino: socket.ino,
                    socket: Arc::downgrade(socket),
                },
            );
            return Ok(UnixAddress::Abstract(name));
        }
    }
    Err(VfsError::AddressNotAvailable)
}

pub fn lookup(address: &UnixAddress) -> Result<Arc<UnixSocket>, VfsError> {
    let socket = {
        let registry = REGISTRY.lock();
        match address {
            UnixAddress::Unnamed => return Err(VfsError::ConnectionRefused),
            UnixAddress::Pathname(path) => registry
                .paths
                .get(path)
                .and_then(|entry| entry.socket.upgrade()),
            UnixAddress::Abstract(name) => registry
                .abstract_names
                .get(name)
                .and_then(|entry| entry.socket.upgrade()),
        }
    };
    socket.ok_or(match address {
        UnixAddress::Pathname(_) => VfsError::NotFound,
        _ => VfsError::ConnectionRefused,
    })
}

/// Remove a pathname socket binding for `unlink(2)`.
pub fn unbind_path(path: &str) -> bool {
    let mut registry = REGISTRY.lock();
    registry.paths.remove(path).is_some()
}

/// Metadata for `stat` on a bound pathname socket.
pub fn path_metadata(path: &str) -> Option<Stat> {
    let socket = {
        let registry = REGISTRY.lock();
        registry
            .paths
            .get(path)
            .and_then(|entry| entry.socket.upgrade())
    }?;
    socket.metadata().ok()
}

pub fn is_bound_path(path: &str) -> bool {
    let registry = REGISTRY.lock();
    registry.paths.contains_key(path)
}

pub fn open_file(socket: Arc<UnixSocket>, flags: u32) -> Arc<OpenFile> {
    let name = format!("socket:[{}]", socket.metadata().map(|s| s.ino).unwrap_or(0));
    Arc::new(OpenFile::new(socket, name, flags))
}

pub fn ino_of(socket: &UnixSocket) -> u64 {
    socket.ino
}
