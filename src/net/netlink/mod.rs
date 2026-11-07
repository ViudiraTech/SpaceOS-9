/*
 *
 *       src/net/netlink/mod.rs
 *       AF_NETLINK core: protocol table, sockets, delivery, requests and dumps
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mirrors `net/netlink/af_netlink.c`.
//!
//! * Protocols register with [`kernel_create`] (the `netlink_kernel_create`
//!   analogue): a multicast group count, optional per-message `input`
//!   handler, and bind/unbind hooks.
//! * User sockets get a unique portid (`bind` or autobind), join multicast
//!   groups, and receive into a byte-accounted queue; overrun sets `ENOBUFS`.
//! * `sendmsg` to portid 0 runs the protocol's `input` handler synchronously
//!   in the sender's context through [`rcv_skb`], which also sends ACKs and
//!   extended ACKs. Dumps resume lazily from `recvmsg`.
//!
//! Lock order: `TABLE` -> socket `state`. Neither is held while a protocol
//! handler, a dump callback or a bind hook runs.

pub mod genl;
pub mod msg;
pub mod netfilter;
pub mod route;
#[cfg(feature = "boot-self-test")]
pub mod self_test;
pub mod sock_diag;
pub mod uevent;

pub use uevent::broadcast_uevent;

use crate::{
    sched,
    sync::SpinLock,
    vfs::{self, Inode, InodeKind, Stat, VfsError},
};
use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicI32, Ordering};
use msg::{ExtAck, Header, NLM_F_ACK, NLM_F_REQUEST, NLMSG_HDRLEN, NLMSG_MIN_TYPE};

pub const AF_NETLINK: u64 = 16;
pub const MAX_LINKS: usize = 32;

pub const NETLINK_ROUTE: u32 = 0;
pub const NETLINK_USERSOCK: u32 = 2;
pub const NETLINK_FIREWALL: u32 = 3;
pub const NETLINK_SOCK_DIAG: u32 = 4;
pub const NETLINK_NFLOG: u32 = 5;
pub const NETLINK_AUDIT: u32 = 9;
pub const NETLINK_FIB_LOOKUP: u32 = 10;
pub const NETLINK_CONNECTOR: u32 = 11;
pub const NETLINK_NETFILTER: u32 = 12;
pub const NETLINK_KOBJECT_UEVENT: u32 = 15;
pub const NETLINK_GENERIC: u32 = 16;

/// Protocol permits unprivileged group membership (`NL_CFG_F_NONROOT_RECV`).
pub const NL_CFG_F_NONROOT_RECV: u32 = 1;
/// Protocol permits unprivileged multicast sends (`NL_CFG_F_NONROOT_SEND`).
pub const NL_CFG_F_NONROOT_SEND: u32 = 2;

const SOL_SOCKET: u64 = 1;
const SO_TYPE: u64 = 3;
const SO_ERROR: u64 = 4;
const SO_SNDBUF: u64 = 7;
const SO_RCVBUF: u64 = 8;
const SO_PASSCRED: u64 = 16;
const SO_PROTOCOL: u64 = 38;
const SO_DOMAIN: u64 = 39;
const SO_ATTACH_FILTER: u64 = 26;
const SO_DETACH_FILTER: u64 = 27;
const SO_RCVBUFFORCE: u64 = 33;
const SO_SNDBUFFORCE: u64 = 32;
const SOL_NETLINK: u64 = 270;
const NETLINK_ADD_MEMBERSHIP: u64 = 1;
const NETLINK_DROP_MEMBERSHIP: u64 = 2;
const NETLINK_PKTINFO: u64 = 3;
const NETLINK_BROADCAST_ERROR: u64 = 4;
const NETLINK_NO_ENOBUFS: u64 = 5;
const NETLINK_LIST_MEMBERSHIPS: u64 = 9;
const NETLINK_CAP_ACK: u64 = 10;
const NETLINK_EXT_ACK: u64 = 11;
const NETLINK_GET_STRICT_CHK: u64 = 12;

const F_PKTINFO: u32 = 1;
const F_BROADCAST_ERROR: u32 = 2;
const F_NO_ENOBUFS: u32 = 4;
const F_CAP_ACK: u32 = 8;
const F_EXT_ACK: u32 = 16;
const F_STRICT_CHK: u32 = 32;

/// `sysctl net.core.{r,w}mem_{default,max}`.
const DEFAULT_BUFFER: usize = 212_992;
const MIN_BUFFER: usize = 4_608;
/// Per-datagram bookkeeping charged on top of the payload (`skb->truesize`).
const SKB_OVERHEAD: usize = 256;
/// Largest message accepted by `sendmsg`.
const MAX_SEND: usize = 1 << 20;
/// Dump chunk size (`NLMSG_GOODSIZE` scaled up for fewer wakeups).
const DUMP_CHUNK: usize = 16 * 1024;

const EPERM: i32 = 1;
const EINTR: i32 = 4;
const EMSGSIZE: i32 = 90;
const ENOBUFS_ERRNO: i32 = 105;

/// Return this from an `input` handler that has already produced a complete
/// reply (a dump): `rcv_skb` then skips the automatic ACK, like `-EINTR`.
pub const SKIP_ACK: i32 = -EINTR;

// ---- Protocol registration --------------------------------------------------

/// A parsed request handed to a protocol's `input` handler.
pub struct Request<'a> {
    pub socket: &'a Arc<NetlinkSocket>,
    pub header: Header,
    /// Bytes after the 16-byte header (family header and attributes).
    pub payload: &'a [u8],
    /// Whole message including the header, used for error echo.
    pub raw: &'a [u8],
    /// Sender effective uid (0 for kernel-originated requests).
    pub euid: u32,
}

impl Request<'_> {
    /// `netlink_capable(skb, CAP_NET_ADMIN)`; root-only until real
    /// capabilities exist.
    pub fn is_privileged(&self) -> bool {
        self.euid == 0
    }

    /// Queue `message` to the requester (`nlmsg_unicast` back to the sender).
    pub fn reply(&self, message: Vec<u8>) -> Result<(), VfsError> {
        enqueue(self.socket, 0, 0, message, false)
    }

    /// Start a multipart dump (`netlink_dump_start`). On success the handler
    /// should return [`SKIP_ACK`]. The first chunk is produced immediately.
    pub fn dump_start(&self, dump: DumpFn) -> Result<(), i32> {
        self.socket.dump_start(self.header, dump)
    }

    /// Whether the receiver of replies asked for extended ACK TLVs.
    pub fn ext_ack_enabled(&self) -> bool {
        self.socket.state.lock().flags & F_EXT_ACK != 0
    }
}

/// Per-message request handler: `Ok(())` sends an ACK only when
/// `NLM_F_ACK` is set; `Err(-errno)` always sends an error reply.
pub type Input = fn(&Request<'_>, &mut ExtAck) -> Result<(), i32>;
/// Whole-datagram handler for protocols that batch (nfnetlink, genl).
pub type RawInput = fn(&Arc<NetlinkSocket>, &[u8]);
pub type BindHook = fn(group: u32) -> Result<(), i32>;

/// `struct netlink_kernel_cfg`.
#[derive(Clone, Copy)]
pub struct KernelCfg {
    /// Number of multicast groups; at least 32 are always available.
    pub groups: u32,
    pub flags: u32,
    pub input: Option<Input>,
    pub raw_input: Option<RawInput>,
    pub bind: Option<BindHook>,
    pub unbind: Option<fn(group: u32)>,
}

impl KernelCfg {
    pub const fn new() -> Self {
        Self {
            groups: 32,
            flags: 0,
            input: None,
            raw_input: None,
            bind: None,
            unbind: None,
        }
    }
}

impl Default for KernelCfg {
    fn default() -> Self {
        Self::new()
    }
}

struct Proto {
    cfg: Option<KernelCfg>,
    sockets: BTreeMap<u32, Weak<NetlinkSocket>>,
    members: Vec<Weak<NetlinkSocket>>,
}

impl Proto {
    const fn new() -> Self {
        Self {
            cfg: None,
            sockets: BTreeMap::new(),
            members: Vec::new(),
        }
    }
}

static TABLE: SpinLock<[Proto; MAX_LINKS]> = SpinLock::new([const { Proto::new() }; MAX_LINKS]);

/// `netlink_kernel_create`. Fails with `AlreadyExists` if registered.
pub fn kernel_create(protocol: u32, cfg: KernelCfg) -> Result<(), VfsError> {
    let slot = protocol as usize;
    if slot >= MAX_LINKS {
        return Err(VfsError::InvalidArgument);
    }
    let mut table = TABLE.lock();
    if table[slot].cfg.is_some() {
        return Err(VfsError::AlreadyExists);
    }
    table[slot].cfg = Some(cfg);
    Ok(())
}

/// `netlink_kernel_release`: new sockets are refused; open ones stay valid.
pub fn kernel_release(protocol: u32) {
    if let Some(proto) = TABLE.lock().get_mut(protocol as usize) {
        proto.cfg = None;
    }
}

fn config(protocol: u32) -> Option<KernelCfg> {
    TABLE.lock().get(protocol as usize).and_then(|p| p.cfg)
}

/// Register the built-in protocols. Call once from `net::init`.
pub fn init() {
    uevent::init();
    genl::init();
    netfilter::init();
    sock_diag::init();
}

// ---- Socket -----------------------------------------------------------------

struct Datagram {
    sender: u32,
    /// Multicast group number (1-based) or 0 for unicast.
    dst_group: u32,
    payload: Vec<u8>,
}

impl Datagram {
    fn charge(&self) -> usize {
        self.payload.len() + SKB_OVERHEAD
    }
}

/// Resumable dump callback: append messages with [`DumpCtx::try_put`] and
/// return `Ok(true)` while more remain, `Ok(false)` when finished.
pub type DumpFn = Box<dyn FnMut(&mut DumpCtx) -> Result<bool, i32> + Send>;

/// Output side of one dump chunk (`struct netlink_callback` + skb).
pub struct DumpCtx {
    buffer: Vec<u8>,
    limit: usize,
    request: Header,
}

impl DumpCtx {
    /// Header of the request that started this dump (for `seq`/`portid`).
    pub fn request(&self) -> &Header {
        &self.request
    }

    /// Append one finished message if it fits in this chunk. `false` means
    /// "chunk full": keep the cursor on this item and return `Ok(true)`.
    pub fn try_put(&mut self, message: &[u8]) -> bool {
        let padded = msg::align(message.len());
        if self.buffer.len() + padded > self.limit && !self.buffer.is_empty() {
            return false;
        }
        self.buffer.extend_from_slice(message);
        self.buffer
            .resize(self.buffer.len() + padded - message.len(), 0);
        true
    }

    /// New message pre-addressed to the requester with `NLM_F_MULTI`.
    pub fn message(&self, kind: u16) -> msg::MsgBuilder {
        msg::MsgBuilder::new(
            kind,
            msg::NLM_F_MULTI,
            self.request.seq,
            self.request.portid,
        )
    }
}

struct DumpState {
    dump: DumpFn,
    request: Header,
}

struct State {
    portid: u32,
    bound: bool,
    /// Multicast membership bitmap, bit `n-1` for group `n`.
    groups: Vec<u32>,
    queue: VecDeque<Datagram>,
    rmem: usize,
    rcvbuf: usize,
    sndbuf: usize,
    error: Option<i32>,
    flags: u32,
    nonblock: bool,
    dump: Option<DumpState>,
    dumping: bool,
}

pub struct NetlinkSocket {
    protocol: u32,
    ino: u64,
    state: SpinLock<State>,
    me: Weak<NetlinkSocket>,
}

/// Rover for portids taken by autobind (`netlink_autobind`): starts at
/// `-4097` and counts down so it never collides with real pids.
static ROVER: AtomicI32 = AtomicI32::new(-4097);

fn sender_euid() -> u32 {
    sched::current().map_or(0, |task| task.credentials().euid)
}

fn current_tgid() -> u32 {
    sched::current().map_or(0, |task| task.stats().tgid as u32)
}

impl NetlinkSocket {
    /// `netlink_create`.
    pub fn create(protocol: u32, nonblock: bool) -> Result<Arc<Self>, VfsError> {
        if config(protocol).is_none() {
            return Err(VfsError::ProtocolType);
        }
        Ok(Arc::new_cyclic(|me| Self {
            protocol,
            ino: vfs::tree::ino(),
            state: SpinLock::new(State {
                portid: 0,
                bound: false,
                groups: Vec::new(),
                queue: VecDeque::new(),
                rmem: 0,
                rcvbuf: DEFAULT_BUFFER,
                sndbuf: DEFAULT_BUFFER,
                error: None,
                flags: 0,
                nonblock,
                dump: None,
                dumping: false,
            }),
            me: me.clone(),
        }))
    }

    pub fn protocol(&self) -> u32 {
        self.protocol
    }

    pub fn portid(&self) -> u32 {
        self.state.lock().portid
    }

    fn arc(&self) -> Arc<NetlinkSocket> {
        self.me.upgrade().expect("netlink socket used after drop")
    }

    /// Insert into the portid table; `AddressInUse` if taken.
    fn insert(&self, portid: u32) -> Result<(), VfsError> {
        {
            let mut table = TABLE.lock();
            let proto = &mut table[self.protocol as usize];
            if proto
                .sockets
                .get(&portid)
                .is_some_and(|other| other.strong_count() != 0)
            {
                return Err(VfsError::AddressInUse);
            }
            proto.sockets.insert(portid, self.me.clone());
        }
        let mut state = self.state.lock();
        state.portid = portid;
        state.bound = true;
        Ok(())
    }

    /// `netlink_autobind`: try the caller's tgid, then walk the rover down.
    fn autobind(&self) -> Result<(), VfsError> {
        if self.state.lock().bound {
            return Ok(());
        }
        let mut candidate = current_tgid();
        for _ in 0..1 << 16 {
            if candidate != 0 && self.insert(candidate).is_ok() {
                return Ok(());
            }
            candidate = ROVER.fetch_sub(1, Ordering::Relaxed) as u32;
        }
        Err(VfsError::AddressInUse)
    }

    fn ngroups(&self) -> u32 {
        config(self.protocol).map_or(32, |cfg| cfg.groups.max(32))
    }

    fn has_group(state: &State, group: u32) -> bool {
        let (word, bit) = (((group - 1) / 32) as usize, (group - 1) % 32);
        state.groups.get(word).is_some_and(|w| w & (1 << bit) != 0)
    }

    /// Join or leave one group, running the protocol hook (`netlink_update_socket_mc`).
    fn set_membership(&self, group: u32, join: bool) -> Result<(), VfsError> {
        if group == 0 || group > self.ngroups() {
            return Err(VfsError::InvalidArgument);
        }
        let cfg = config(self.protocol);
        let present = Self::has_group(&self.state.lock(), group);
        if present == join {
            return Ok(());
        }
        if join && let Some(bind) = cfg.and_then(|c| c.bind) {
            bind(group).map_err(|_| VfsError::NotPermitted)?;
        }
        {
            let mut state = self.state.lock();
            let word = ((group - 1) / 32) as usize;
            if state.groups.len() <= word {
                state.groups.resize(word + 1, 0);
            }
            let bit = 1u32 << ((group - 1) % 32);
            if join {
                state.groups[word] |= bit;
            } else {
                state.groups[word] &= !bit;
            }
        }
        let mut table = TABLE.lock();
        let members = &mut table[self.protocol as usize].members;
        members.retain(|weak| weak.strong_count() != 0);
        if join && !members.iter().any(|weak| weak.as_ptr() == self.me.as_ptr()) {
            members.push(self.me.clone());
        }
        drop(table);
        if !join && let Some(unbind) = cfg.and_then(|c| c.unbind) {
            unbind(group);
        }
        Ok(())
    }

    fn may_join(&self) -> bool {
        config(self.protocol).is_some_and(|cfg| cfg.flags & NL_CFG_F_NONROOT_RECV != 0)
            || sender_euid() == 0
    }

    /// `netlink_bind`: `address` is a `sockaddr_nl`.
    pub fn bind(&self, address: &[u8]) -> Result<(), VfsError> {
        if address.len() < 12 || u16::from_le_bytes([address[0], address[1]]) as u64 != AF_NETLINK {
            return Err(VfsError::InvalidArgument);
        }
        let pid = u32::from_le_bytes(address[4..8].try_into().unwrap());
        let mask = u32::from_le_bytes(address[8..12].try_into().unwrap());
        let mask = if self.ngroups() < 32 {
            mask & ((1u32 << self.ngroups()) - 1)
        } else {
            mask
        };
        if mask != 0 && !self.may_join() {
            return Err(VfsError::NotPermitted);
        }
        let (bound, current) = {
            let state = self.state.lock();
            (state.bound, state.portid)
        };
        if bound {
            if pid != current {
                return Err(VfsError::InvalidArgument);
            }
        } else if pid != 0 {
            self.insert(pid)?;
        } else {
            self.autobind()?;
        }
        for group in 1..=32u32 {
            if mask & (1 << (group - 1)) != 0 {
                self.set_membership(group, true)?;
            }
        }
        Ok(())
    }

    /// `getsockname`: `sockaddr_nl` with the first 32 groups as a mask.
    pub fn address(&self) -> [u8; 12] {
        let state = self.state.lock();
        encode_address(state.portid, state.groups.first().copied().unwrap_or(0))
    }

    /// `netlink_sendmsg`. `destination` is a `sockaddr_nl` or empty.
    pub fn send(&self, destination: &[u8], payload: &[u8]) -> Result<usize, VfsError> {
        let (dst_portid, dst_mask) = if destination.is_empty() {
            (0, 0)
        } else if destination.len() < 12
            || u16::from_le_bytes([destination[0], destination[1]]) as u64 != AF_NETLINK
        {
            return Err(VfsError::InvalidArgument);
        } else {
            (
                u32::from_le_bytes(destination[4..8].try_into().unwrap()),
                u32::from_le_bytes(destination[8..12].try_into().unwrap()),
            )
        };
        let cfg = config(self.protocol).ok_or(VfsError::ProtocolType)?;
        let euid = sender_euid();
        if dst_mask != 0 && cfg.flags & NL_CFG_F_NONROOT_SEND == 0 && euid != 0 {
            return Err(VfsError::NotPermitted);
        }
        if payload.len() > MAX_SEND || payload.len() > self.state.lock().sndbuf {
            return Err(VfsError::MessageTooLong);
        }
        self.autobind()?;
        let me = self.arc();
        let sender = self.portid();
        // Linux broadcasts to the lowest set group only (`ffs(nl_groups)`).
        if dst_mask != 0 {
            let group = dst_mask.trailing_zeros() + 1;
            broadcast(self.protocol, sender, group, payload);
        }
        if dst_portid != 0 {
            let target = lookup(self.protocol, dst_portid).ok_or(VfsError::ConnectionRefused)?;
            enqueue(&target, sender, 0, payload.to_vec(), false)?;
            return Ok(payload.len());
        }
        if let Some(raw_input) = cfg.raw_input {
            raw_input(&me, payload);
        } else if let Some(input) = cfg.input {
            rcv_skb(&me, payload, euid, input);
        } else if dst_mask == 0 {
            return Err(VfsError::ConnectionRefused);
        }
        // With a group set and no kernel consumer Linux still reports
        // ECONNREFUSED after broadcasting; userspace device managers treat the
        // send as delivered, so success is reported here.
        Ok(payload.len())
    }

    /// `netlink_recvmsg`: `(sender portid, group mask, payload)`.
    pub fn receive(&self, dontwait: bool) -> Result<(u32, u32, Vec<u8>), VfsError> {
        loop {
            let resume = {
                let mut state = self.state.lock();
                if let Some(error) = state.error.take() {
                    return Err(errno_to_error(error));
                }
                if let Some(datagram) = state.queue.pop_front() {
                    state.rmem = state.rmem.saturating_sub(datagram.charge());
                    return Ok((
                        datagram.sender,
                        group_mask(datagram.dst_group),
                        datagram.payload,
                    ));
                }
                state.dump.is_some() && !state.dumping
            };
            if resume {
                self.dump_step();
                continue;
            }
            if dontwait || self.state.lock().nonblock {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::ipc::block_briefly();
        }
    }

    // ---- dumps -------------------------------------------------------------

    fn dump_start(&self, mut request: Header, dump: DumpFn) -> Result<(), i32> {
        {
            let mut state = self.state.lock();
            if state.dump.is_some() || state.dumping {
                return Err(-16); // EBUSY
            }
            request.portid = state.portid;
            state.dump = Some(DumpState { dump, request });
        }
        self.dump_step();
        Ok(())
    }

    /// Produce and queue the next chunk of a pending dump. The callback runs
    /// without any lock held.
    fn dump_step(&self) {
        let Some(mut pending) = ({
            let mut state = self.state.lock();
            if state.dumping {
                None
            } else {
                let taken = state.dump.take();
                state.dumping = taken.is_some();
                taken
            }
        }) else {
            return;
        };
        let mut ctx = DumpCtx {
            buffer: Vec::new(),
            limit: DUMP_CHUNK,
            request: pending.request,
        };
        let outcome = (pending.dump)(&mut ctx);
        let mut done = None;
        let mut again = false;
        match outcome {
            Ok(true) if !ctx.buffer.is_empty() => again = true,
            // A callback that reports "more" without producing anything would
            // spin forever; end the dump with an error instead.
            Ok(true) => done = Some(-EMSGSIZE),
            Ok(false) => done = Some(0),
            Err(error) => done = Some(error),
        }
        if let Some(status) = done {
            let end = msg::build_done(&pending.request, status);
            ctx.buffer.extend_from_slice(&end);
        }
        let _ = enqueue(&self.arc(), 0, 0, ctx.buffer, true);
        let mut state = self.state.lock();
        state.dumping = false;
        if again {
            state.dump = Some(pending);
        }
    }

    // ---- socket options -----------------------------------------------------

    pub fn setsockopt(&self, level: u64, name: u64, value: &[u8]) -> Result<(), VfsError> {
        let int = || -> Result<u32, VfsError> {
            Ok(u32::from_le_bytes(
                value
                    .get(..4)
                    .ok_or(VfsError::InvalidArgument)?
                    .try_into()
                    .unwrap(),
            ))
        };
        match (level, name) {
            (SOL_SOCKET, SO_RCVBUF | SO_SNDBUF) => {
                let size = (int()? as usize).min(DEFAULT_BUFFER).saturating_mul(2);
                self.set_buffer(name == SO_RCVBUF, size.max(MIN_BUFFER));
                Ok(())
            }
            (SOL_SOCKET, SO_RCVBUFFORCE | SO_SNDBUFFORCE) => {
                if sender_euid() != 0 {
                    return Err(VfsError::NotPermitted);
                }
                let size = (int()? as usize).saturating_mul(2).max(MIN_BUFFER);
                self.set_buffer(name == SO_RCVBUFFORCE, size);
                Ok(())
            }
            (SOL_SOCKET, SO_PASSCRED | SO_ATTACH_FILTER | SO_DETACH_FILTER) => Ok(()),
            (SOL_NETLINK, NETLINK_ADD_MEMBERSHIP | NETLINK_DROP_MEMBERSHIP) => {
                if !self.may_join() {
                    return Err(VfsError::NotPermitted);
                }
                self.set_membership(int()?, name == NETLINK_ADD_MEMBERSHIP)
            }
            (SOL_NETLINK, NETLINK_PKTINFO) => self.set_flag(F_PKTINFO, int()? != 0),
            (SOL_NETLINK, NETLINK_BROADCAST_ERROR) => self.set_flag(F_BROADCAST_ERROR, int()? != 0),
            (SOL_NETLINK, NETLINK_NO_ENOBUFS) => {
                let on = int()? != 0;
                self.set_flag(F_NO_ENOBUFS, on)?;
                if on {
                    self.state.lock().error = None;
                }
                Ok(())
            }
            (SOL_NETLINK, NETLINK_CAP_ACK) => self.set_flag(F_CAP_ACK, int()? != 0),
            (SOL_NETLINK, NETLINK_EXT_ACK) => self.set_flag(F_EXT_ACK, int()? != 0),
            (SOL_NETLINK, NETLINK_GET_STRICT_CHK) => self.set_flag(F_STRICT_CHK, int()? != 0),
            (SOL_NETLINK, _) => Err(VfsError::NoProtocolOption),
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    fn set_buffer(&self, receive: bool, size: usize) {
        let mut state = self.state.lock();
        if receive {
            state.rcvbuf = size;
        } else {
            state.sndbuf = size;
        }
    }

    fn set_flag(&self, flag: u32, on: bool) -> Result<(), VfsError> {
        let mut state = self.state.lock();
        if on {
            state.flags |= flag;
        } else {
            state.flags &= !flag;
        }
        Ok(())
    }

    pub fn getsockopt(&self, level: u64, name: u64) -> Result<Vec<u8>, VfsError> {
        let state = self.state.lock();
        let int = |value: u32| Ok(value.to_le_bytes().to_vec());
        let flag = |bit: u32| int((state.flags & bit != 0) as u32);
        match (level, name) {
            (SOL_SOCKET, SO_TYPE) => int(3),
            (SOL_SOCKET, SO_DOMAIN) => int(AF_NETLINK as u32),
            (SOL_SOCKET, SO_PROTOCOL) => int(self.protocol),
            (SOL_SOCKET, SO_SNDBUF) => int(state.sndbuf as u32),
            (SOL_SOCKET, SO_RCVBUF) => int(state.rcvbuf as u32),
            (SOL_SOCKET, SO_ERROR) => {
                drop(state);
                let error = self.state.lock().error.take().unwrap_or(0);
                int(error as u32)
            }
            (SOL_NETLINK, NETLINK_PKTINFO) => flag(F_PKTINFO),
            (SOL_NETLINK, NETLINK_BROADCAST_ERROR) => flag(F_BROADCAST_ERROR),
            (SOL_NETLINK, NETLINK_NO_ENOBUFS) => flag(F_NO_ENOBUFS),
            (SOL_NETLINK, NETLINK_CAP_ACK) => flag(F_CAP_ACK),
            (SOL_NETLINK, NETLINK_EXT_ACK) => flag(F_EXT_ACK),
            (SOL_NETLINK, NETLINK_GET_STRICT_CHK) => flag(F_STRICT_CHK),
            (SOL_NETLINK, NETLINK_LIST_MEMBERSHIPS) => {
                let words = (self.ngroups() as usize).div_ceil(32);
                let mut out = Vec::with_capacity(words * 4);
                for word in 0..words {
                    out.extend_from_slice(
                        &state.groups.get(word).copied().unwrap_or(0).to_le_bytes(),
                    );
                }
                Ok(out)
            }
            _ => Err(VfsError::NoProtocolOption),
        }
    }
}

impl Drop for NetlinkSocket {
    fn drop(&mut self) {
        let portid = self.state.lock().portid;
        let cfg_unbind;
        let groups: Vec<u32> = {
            let state = self.state.lock();
            (1..=self.ngroups())
                .filter(|group| Self::has_group(&state, *group))
                .collect()
        };
        {
            let mut table = TABLE.lock();
            let proto = &mut table[self.protocol as usize];
            if proto
                .sockets
                .get(&portid)
                .is_some_and(|weak| weak.as_ptr() == self.me.as_ptr())
            {
                proto.sockets.remove(&portid);
            }
            proto.members.retain(|weak| weak.strong_count() != 0);
            cfg_unbind = proto.cfg.and_then(|cfg| cfg.unbind);
        }
        if let Some(unbind) = cfg_unbind {
            for group in groups {
                unbind(group);
            }
        }
    }
}

fn errno_to_error(errno: i32) -> VfsError {
    match errno {
        ENOBUFS_ERRNO => VfsError::NoBuffers,
        EPERM => VfsError::NotPermitted,
        _ => VfsError::IoError,
    }
}

/// `sockaddr_nl.nl_groups` for a delivered datagram.
fn group_mask(group: u32) -> u32 {
    if (1..=32).contains(&group) {
        1 << (group - 1)
    } else {
        0
    }
}

fn encode_address(portid: u32, groups: u32) -> [u8; 12] {
    let mut raw = [0u8; 12];
    raw[0..2].copy_from_slice(&(AF_NETLINK as u16).to_le_bytes());
    raw[4..8].copy_from_slice(&portid.to_le_bytes());
    raw[8..12].copy_from_slice(&groups.to_le_bytes());
    raw
}

/// Source address for `recvfrom`/`recvmsg`.
pub fn encoded_source(portid: u32, groups: u32) -> [u8; 12] {
    encode_address(portid, groups)
}

// ---- Delivery -----------------------------------------------------------------

fn lookup(protocol: u32, portid: u32) -> Option<Arc<NetlinkSocket>> {
    TABLE.lock()[protocol as usize]
        .sockets
        .get(&portid)
        .and_then(Weak::upgrade)
}

/// Queue a datagram on `socket`'s receive queue with `rcvbuf` accounting
/// (`netlink_attachskb`). Overrun sets `ENOBUFS` unless `NETLINK_NO_ENOBUFS`.
/// `force` bypasses the limit, used for dump chunks the reader is waiting on.
fn enqueue(
    socket: &Arc<NetlinkSocket>,
    sender: u32,
    dst_group: u32,
    payload: Vec<u8>,
    force: bool,
) -> Result<(), VfsError> {
    let datagram = Datagram {
        sender,
        dst_group,
        payload,
    };
    let charge = datagram.charge();
    {
        let mut state = socket.state.lock();
        if !force && state.rmem + charge > state.rcvbuf {
            if state.flags & F_NO_ENOBUFS == 0 {
                state.error = Some(ENOBUFS_ERRNO);
            }
            drop(state);
            crate::ipc::wake_pollers();
            return Err(VfsError::NoBuffers);
        }
        state.rmem += charge;
        state.queue.push_back(datagram);
    }
    crate::ipc::wake_pollers();
    Ok(())
}

/// `netlink_unicast` from kernel code to a bound user socket.
pub fn unicast(protocol: u32, portid: u32, payload: Vec<u8>) -> Result<(), VfsError> {
    let target = lookup(protocol, portid).ok_or(VfsError::ConnectionRefused)?;
    enqueue(&target, 0, 0, payload, false)
}

/// `netlink_broadcast`: deliver to every member of `group` except the socket
/// bound to `exclude_portid`. Returns how many sockets received it. A
/// receiver whose queue is full gets `ENOBUFS` and the others are unaffected.
pub fn broadcast(protocol: u32, exclude_portid: u32, group: u32, payload: &[u8]) -> usize {
    if group == 0 || protocol as usize >= MAX_LINKS {
        return 0;
    }
    let listeners: Vec<Arc<NetlinkSocket>> = {
        let table = TABLE.lock();
        table[protocol as usize]
            .members
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    };
    let mut delivered = 0;
    for socket in listeners {
        {
            let state = socket.state.lock();
            if !NetlinkSocket::has_group(&state, group)
                || (exclude_portid != 0 && state.bound && state.portid == exclude_portid)
            {
                continue;
            }
        }
        if enqueue(&socket, 0, group, payload.to_vec(), false).is_ok() {
            delivered += 1;
        }
    }
    delivered
}

/// `netlink_has_listeners`: lets producers skip building events nobody reads.
pub fn has_listeners(protocol: u32, group: u32) -> bool {
    if group == 0 || protocol as usize >= MAX_LINKS {
        return false;
    }
    let members: Vec<Arc<NetlinkSocket>> = TABLE.lock()[protocol as usize]
        .members
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    members
        .iter()
        .any(|socket| NetlinkSocket::has_group(&socket.state.lock(), group))
}

// ---- Request processing ---------------------------------------------------------

/// `netlink_rcv_skb`: split a datagram into messages, call `handler` for each
/// valid request, and send ACK / error replies.
pub fn rcv_skb(socket: &Arc<NetlinkSocket>, buffer: &[u8], euid: u32, handler: Input) {
    let mut offset = 0;
    while let Some(header) = Header::parse(&buffer[offset..]) {
        let length = header.length as usize;
        if length < NLMSG_HDRLEN || length > buffer.len() - offset {
            return;
        }
        let raw = &buffer[offset..offset + length];
        let mut ext = ExtAck::default();
        let result = if header.flags & NLM_F_REQUEST == 0 || header.kind < NLMSG_MIN_TYPE {
            Ok(())
        } else {
            handler(
                &Request {
                    socket,
                    header,
                    payload: &raw[NLMSG_HDRLEN..],
                    raw,
                    euid,
                },
                &mut ext,
            )
        };
        let error = match result {
            Ok(()) => 0,
            Err(error) => error,
        };
        if error != SKIP_ACK && (header.flags & NLM_F_ACK != 0 || error != 0) {
            ack(socket, &header, raw, error, &ext);
        }
        offset += msg::align(length);
        if offset >= buffer.len() {
            return;
        }
    }
}

/// `netlink_ack`. A lost ACK (queue full) is reported as `ENOBUFS`.
pub fn ack(socket: &Arc<NetlinkSocket>, header: &Header, raw: &[u8], error: i32, ext: &ExtAck) {
    let (cap_ack, want_ext, portid) = {
        let state = socket.state.lock();
        (
            state.flags & F_CAP_ACK != 0,
            state.flags & F_EXT_ACK != 0,
            state.portid,
        )
    };
    let mut reply_header = *header;
    reply_header.portid = portid;
    let has_ext = ext.message.is_some() || ext.bad_attr_offset.is_some() || ext.cookie.is_some();
    let message = msg::build_error(
        &reply_header,
        raw,
        error,
        if want_ext && has_ext { Some(ext) } else { None },
        cap_ack,
    );
    if enqueue(socket, 0, 0, message, false).is_err() {
        let mut state = socket.state.lock();
        if state.flags & F_NO_ENOBUFS == 0 {
            state.error = Some(ENOBUFS_ERRNO);
        }
    }
}

// ---- VFS glue ---------------------------------------------------------------------

impl Inode for NetlinkSocket {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::Socket, 0o777, 0))
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        let (_sender, _groups, payload) = self.receive(false)?;
        let copy_len = buffer.len().min(payload.len());
        buffer[..copy_len].copy_from_slice(&payload[..copy_len]);
        Ok(copy_len)
    }

    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        self.send(&[], buffer)
    }

    fn poll_sources(&self, _sources: &mut crate::ipc::pollwait::PollSources) {
        // Delivery announces itself through wake_pollers.
    }

    fn poll(&self) -> u32 {
        const POLLIN: u32 = 1;
        const POLLOUT: u32 = 4;
        const POLLERR: u32 = 8;
        let state = self.state.lock();
        let mut events = POLLOUT;
        if !state.queue.is_empty() || state.dump.is_some() {
            events |= POLLIN;
        }
        if state.error.is_some() {
            events |= POLLIN | POLLERR;
        }
        events
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.state.lock().nonblock = nonblock;
        Ok(())
    }
}

/// Resolve a file descriptor to a netlink socket, if it is one.
pub fn instance(fd: u64) -> Option<Arc<NetlinkSocket>> {
    if fd > i32::MAX as u64 {
        return None;
    }
    let inode = vfs::current_fd_table()
        .lock()
        .get(fd as usize)?
        .file
        .inode
        .clone();
    let any: Arc<dyn core::any::Any + Send + Sync> = inode;
    any.downcast::<NetlinkSocket>().ok()
}
