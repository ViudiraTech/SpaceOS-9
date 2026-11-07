/*
 *
 *       src/net/tcp/mod.rs
 *       Transmission Control Protocol: state machine, windows and retransmission
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Modelled on `net/ipv4/tcp_input.c`, `tcp_output.c`, `tcp_timer.c` and
//! `tcp_cong.c`, reduced to the pieces this kernel needs:
//!
//! * full active/passive open and orderly close, including TIME_WAIT,
//! * receive-side reassembly with a bounded out-of-order map,
//! * an ACK-clocked send queue with one retransmission timer,
//! * Reno slow start / congestion avoidance and fast retransmit,
//! * Karn's rule for RTT sampling and exponential backoff.
//!
//! Window scaling, SACK and timestamps are not negotiated, so peers fall back
//! to an unscaled 64 KiB window. Checksums are always computed in software.

use super::{
    dev::NetDevice,
    ip::{self, IpAddr, Ipv4Addr},
    ipv4::{
        addr,
        header::{IpMeta, TxTemplate},
        icmp, input, output,
    },
    skb::SkBuff,
    socket::{
        self, Ip4SockOpts, SO_ACCEPTCONN, SO_BINDTODEVICE, SO_BROADCAST, SO_DEBUG, SO_DOMAIN,
        SO_DONTROUTE, SO_ERROR, SO_KEEPALIVE, SO_MARK, SO_PRIORITY, SO_PROTOCOL, SO_RCVBUF,
        SO_RCVBUFFORCE, SO_RCVTIMEO_NEW, SO_RCVTIMEO_OLD, SO_REUSEADDR, SO_REUSEPORT, SO_SNDBUF,
        SO_SNDBUFFORCE, SO_SNDTIMEO_NEW, SO_SNDTIMEO_OLD, SO_TIMESTAMP_NEW, SO_TIMESTAMP_OLD,
        SO_TIMESTAMPNS_NEW, SO_TIMESTAMPNS_OLD, SO_TYPE, SOCK_STREAM, SOL_IP, SOL_SOCKET, SOL_TCP,
        TimestampOpt, encode_timeval_us, opt_int_or_byte, parse_timeval_us,
    },
};
use crate::{
    random, sched,
    sync::SpinLock,
    time,
    vfs::{self, Inode, InodeKind, Stat, VfsError, abi},
};
use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ---- Header ---------------------------------------------------------------

pub const TCP_HEADER_LEN: usize = 20;
const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

const OPTION_END: u8 = 0;
const OPTION_NOP: u8 = 1;
const OPTION_MSS: u8 = 2;

const DEFAULT_MSS: usize = 536;
const MAX_MSS: usize = 65_495 - TCP_HEADER_LEN;
const DEFAULT_RTO_US: u64 = 1_000_000;
const MIN_RTO_US: u64 = 200_000;
const MAX_RTO_US: u64 = 60_000_000;
const MAX_RETRIES: u32 = 8;
const MAX_SYN_RETRIES: u32 = 6;
const TIME_WAIT_US: u64 = 60_000_000;
const DEFAULT_RCVBUF: usize = 212_992;
const MIN_BUFFER: usize = 4_608;
const MAX_CWND: usize = 4 * 1024 * 1024;
const EPHEMERAL_START: u16 = 49_152;
const EPHEMERAL_END: u16 = 60_999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Closed = 0,
    Listen = 1,
    SynSent = 2,
    SynRecv = 3,
    Established = 4,
    FinWait1 = 5,
    FinWait2 = 6,
    CloseWait = 7,
    Closing = 8,
    LastAck = 9,
    TimeWait = 10,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub address: Ipv4Addr,
    pub port: u16,
}

impl Endpoint {
    fn unspecified(port: u16) -> Self {
        Self {
            address: Ipv4Addr::UNSPECIFIED,
            port,
        }
    }
}

#[derive(Clone, Debug)]
struct TcpHeader {
    source_port: u16,
    destination_port: u16,
    sequence: u32,
    acknowledgement: u32,
    header_len: usize,
    flags: u8,
    window: u16,
    checksum: u16,
    options: Vec<u8>,
}

impl TcpHeader {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < TCP_HEADER_LEN {
            return None;
        }
        let source_port = u16::from_be_bytes([bytes[0], bytes[1]]);
        let destination_port = u16::from_be_bytes([bytes[2], bytes[3]]);
        let sequence = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let acknowledgement = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let offset_flags = u16::from_be_bytes([bytes[12], bytes[13]]);
        let header_len = ((offset_flags >> 12) as usize) * 4;
        if header_len < TCP_HEADER_LEN || header_len > bytes.len() {
            return None;
        }
        Some(Self {
            source_port,
            destination_port,
            sequence,
            acknowledgement,
            header_len,
            flags: (offset_flags & 0x3f) as u8,
            window: u16::from_be_bytes([bytes[14], bytes[15]]),
            checksum: u16::from_be_bytes([bytes[16], bytes[17]]),
            options: bytes[TCP_HEADER_LEN..header_len].to_vec(),
        })
    }

    fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

// Sequence number arithmetic (RFC 1982).
fn seq_lt(left: u32, right: u32) -> bool {
    (left.wrapping_sub(right) as i32) < 0
}

fn seq_gt(left: u32, right: u32) -> bool {
    (left.wrapping_sub(right) as i32) > 0
}

fn seq_le(left: u32, right: u32) -> bool {
    (left.wrapping_sub(right) as i32) <= 0
}

// ---- State ----------------------------------------------------------------

struct State {
    status: Status,
    local: Endpoint,
    remote: Endpoint,
    bound: bool,
    listening: bool,
    backlog: u32,
    pending: VecDeque<Arc<TcpSocket>>,
    syn_pending: u32,
    parent: Option<Weak<TcpSocket>>,

    snd_una: u32,
    snd_nxt: u32,
    snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    iss: u32,
    snd_buf: VecDeque<u8>,
    sndbuf: usize,
    mss: usize,
    peer_mss: usize,
    cwnd: usize,
    ssthresh: usize,
    srtt_us: u64,
    rttvar_us: u64,
    rto_us: u64,
    retransmit_at_us: u64,
    retransmit_count: u32,
    retransmitted: bool,
    dup_acks: u32,
    in_recovery: bool,
    snd_una_sent_us: u64,
    fin_pending: bool,
    fin_sent: bool,
    fin_seq: u32,
    fin_acked: bool,

    rcv_nxt: u32,
    rcv_buf: VecDeque<u8>,
    ooo: BTreeMap<u32, Vec<u8>>,
    ooo_bytes: usize,
    rcvbuf: usize,
    peer_fin: bool,
    peer_fin_seq: Option<u32>,

    error: Option<i32>,
    read_shutdown: bool,
    write_shutdown: bool,
    reuseaddr: bool,
    reuseport: bool,
    keepalive: bool,
    broadcast: bool,
    dontroute: bool,
    rcvtimeo_us: u64,
    sndtimeo_us: u64,
    bound_ifindex: u32,
    mark: u32,
    priority: u32,
    timestamp: TimestampOpt,
    ip_opts: Ip4SockOpts,
    time_wait_deadline_us: u64,
    need_ack: bool,
}

struct TxCtx {
    local: Endpoint,
    remote: Endpoint,
    template: TxTemplate,
    ip_options: Vec<u8>,
}

impl TxCtx {
    fn plain(local: Endpoint, remote: Endpoint) -> Self {
        Self {
            local,
            remote,
            template: TxTemplate {
                tos: 0,
                ttl: 64,
                protocol: ip::IPPROTO_TCP,
                dont_fragment: false,
            },
            ip_options: Vec::new(),
        }
    }
}

impl State {
    fn tx_ctx(&self, remote: Endpoint) -> TxCtx {
        TxCtx {
            local: self.local,
            remote,
            template: TxTemplate {
                tos: self.ip_opts.tos,
                ttl: self.ip_opts.effective_ttl(),
                protocol: ip::IPPROTO_TCP,
                dont_fragment: self.ip_opts.dont_fragment(),
            },
            ip_options: self.ip_opts.ip_options.clone(),
        }
    }

    fn advertised_window(&self) -> u16 {
        let used = self.rcv_buf.len() + self.ooo_bytes;
        self.rcvbuf.saturating_sub(used).min(u16::MAX as usize) as u16
    }

    fn inflight(&self) -> usize {
        self.snd_nxt.wrapping_sub(self.snd_una) as usize
    }

    fn pending_bytes(&self) -> usize {
        self.snd_buf
            .len()
            .saturating_sub(self.inflight().min(1 << 30))
    }
}

pub struct TcpSocket {
    ino: u64,
    nonblock: AtomicBool,
    state: SpinLock<State>,
    me: Weak<TcpSocket>,
}

struct Connection {
    local: Endpoint,
    remote: Endpoint,
    socket: Weak<TcpSocket>,
}

struct Listener {
    local: Endpoint,
    socket: Weak<TcpSocket>,
}

static CONNECTIONS: SpinLock<Vec<Connection>> = SpinLock::new(Vec::new());
static LISTENERS: SpinLock<Vec<Listener>> = SpinLock::new(Vec::new());
static TIMER_ARMED: AtomicBool = AtomicBool::new(false);
static ESTABLISHED_TOTAL: AtomicU64 = AtomicU64::new(0);

fn privileged() -> bool {
    sched::current().is_none_or(|task| task.credentials().euid == 0)
}

fn random_sequence() -> u32 {
    let mut bytes = [0u8; 4];
    let _ = random::fill(&mut bytes);
    u32::from_le_bytes(bytes) ^ (time::uptime_micros() as u32).rotate_left(7)
}

fn errno_vfs(error: i32) -> VfsError {
    match error {
        111 => VfsError::ConnectionRefused,
        104 => VfsError::ConnectionReset,
        110 => VfsError::TimedOut,
        113 => VfsError::HostUnreachable,
        101 => VfsError::NetworkUnreachable,
        _ => VfsError::IoError,
    }
}

fn errno_of(error: VfsError) -> i32 {
    error.to_errno() as i32
}

impl TcpSocket {
    pub fn create(nonblock: bool) -> Arc<Self> {
        let mss = DEFAULT_MSS;
        let socket = Arc::new_cyclic(|me| Self {
            ino: vfs::tree::ino(),
            nonblock: AtomicBool::new(nonblock),
            state: SpinLock::new(State {
                status: Status::Closed,
                local: Endpoint::unspecified(0),
                remote: Endpoint::unspecified(0),
                bound: false,
                listening: false,
                backlog: 0,
                pending: VecDeque::new(),
                syn_pending: 0,
                parent: None,
                snd_una: 0,
                snd_nxt: 0,
                snd_wnd: 0,
                snd_wl1: 0,
                snd_wl2: 0,
                iss: 0,
                snd_buf: VecDeque::new(),
                sndbuf: DEFAULT_RCVBUF,
                mss,
                peer_mss: DEFAULT_MSS,
                cwnd: 4 * mss,
                ssthresh: u16::MAX as usize,
                srtt_us: 0,
                rttvar_us: 0,
                rto_us: DEFAULT_RTO_US,
                retransmit_at_us: 0,
                retransmit_count: 0,
                retransmitted: false,
                dup_acks: 0,
                in_recovery: false,
                snd_una_sent_us: 0,
                fin_pending: false,
                fin_sent: false,
                fin_seq: 0,
                fin_acked: false,
                rcv_nxt: 0,
                rcv_buf: VecDeque::new(),
                ooo: BTreeMap::new(),
                ooo_bytes: 0,
                rcvbuf: DEFAULT_RCVBUF,
                peer_fin: false,
                peer_fin_seq: None,
                error: None,
                read_shutdown: false,
                write_shutdown: false,
                reuseaddr: false,
                reuseport: false,
                keepalive: false,
                broadcast: false,
                dontroute: false,
                rcvtimeo_us: 0,
                sndtimeo_us: 0,
                bound_ifindex: 0,
                mark: 0,
                priority: 0,
                timestamp: TimestampOpt::Off,
                ip_opts: Ip4SockOpts::default(),
                time_wait_deadline_us: 0,
                need_ack: false,
            }),
            me: me.clone(),
        });
        arm_timer();
        socket
    }

    pub fn ino(&self) -> u64 {
        self.ino
    }

    pub fn kind(&self) -> u32 {
        SOCK_STREAM
    }

    pub fn domain(&self) -> u64 {
        socket::AF_INET
    }

    pub fn protocol(&self) -> u64 {
        ip::IPPROTO_TCP as u64
    }

    pub fn status(&self) -> Status {
        self.state.lock().status
    }

    pub fn is_listening(&self) -> bool {
        let state = self.state.lock();
        state.status == Status::Listen
    }

    pub fn set_nonblock(&self, nonblock: bool) {
        self.nonblock.store(nonblock, Ordering::Release);
    }

    pub fn local_address(&self) -> [u8; socket::SOCKADDR_IN_LEN] {
        let state = self.state.lock();
        socket::encode_sockaddr_in(state.local.address, state.local.port)
    }

    pub fn peer_address(&self) -> Option<[u8; socket::SOCKADDR_IN_LEN]> {
        let state = self.state.lock();
        (state.status != Status::Closed && state.status != Status::Listen && state.remote.port != 0)
            .then(|| socket::encode_sockaddr_in(state.remote.address, state.remote.port))
    }

    /// `inet_bind`.
    pub fn bind(&self, bytes: &[u8]) -> Result<(), VfsError> {
        let (address, port) = socket::decode_sockaddr_in(bytes)?;
        if !address.is_unspecified() && !addr::is_local(address) && !privileged() {
            return Err(VfsError::AddressNotAvailable);
        }
        let mut state = self.state.lock();
        if state.status != Status::Closed {
            return Err(VfsError::InvalidArgument);
        }
        if state.bound {
            return Err(VfsError::InvalidArgument);
        }
        let port = if port == 0 {
            allocate_port(address)?
        } else {
            check_port(address, port, state.reuseaddr)?
        };
        state.local = Endpoint { address, port };
        state.bound = true;
        Ok(())
    }

    /// `inet_listen`.
    pub fn listen(&self, backlog: u32) -> Result<(), VfsError> {
        let mut state = self.state.lock();
        if matches!(state.status, Status::Listen) {
            return Ok(());
        }
        if state.status != Status::Closed {
            return Err(VfsError::InvalidArgument);
        }
        if !state.bound {
            let port = allocate_port(Ipv4Addr::UNSPECIFIED)?;
            state.local = Endpoint::unspecified(port);
            state.bound = true;
        }
        state.status = Status::Listen;
        state.listening = true;
        state.backlog = backlog.max(1);
        let local = state.local;
        drop(state);
        LISTENERS.lock().push(Listener {
            local,
            socket: self.me.clone(),
        });
        arm_timer();
        Ok(())
    }

    /// `inet_accept`: take the next completed connection.
    pub fn accept(&self, nonblock: bool) -> Result<Arc<TcpSocket>, VfsError> {
        loop {
            {
                let mut state = self.state.lock();
                if state.status != Status::Listen {
                    return Err(VfsError::InvalidArgument);
                }
                if let Some(child) = state.pending.pop_front() {
                    return Ok(child);
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

    /// `tcp_v4_connect`. On a blocking socket this waits for the handshake.
    pub fn connect(&self, bytes: &[u8], nonblock_hint: bool) -> Result<(), VfsError> {
        let (address, port) = socket::decode_sockaddr_in(bytes)?;
        if port == 0 {
            return Err(VfsError::AddressNotAvailable);
        }
        let blocking = !(nonblock_hint || self.nonblock.load(Ordering::Acquire));
        let now = time::uptime_micros();
        {
            let mut state = self.state.lock();
            match state.status {
                Status::SynSent => return Err(VfsError::InProgress),
                Status::Closed => {}
                _ => return Err(VfsError::AlreadyConnected),
            }
            if state.write_shutdown || state.read_shutdown {
                return Err(VfsError::InvalidArgument);
            }
            if state.remote.address == address && state.remote.port == port {
                return Err(VfsError::AlreadyConnected);
            }
            if !state.bound {
                let route = super::fib::lookup(IpAddr::V4(address));
                let local_address = route
                    .and_then(|route| match route.source {
                        Some(IpAddr::V4(source)) => Some(source),
                        _ => addr::first_on(route.ifindex),
                    })
                    .ok_or(VfsError::AddressNotAvailable)?;
                let port = allocate_port(local_address)?;
                state.local = Endpoint {
                    address: local_address,
                    port,
                };
                state.bound = true;
            }
            let mss = route_mss(address).unwrap_or(DEFAULT_MSS);
            state.mss = mss.min(MAX_MSS);
            state.iss = random_sequence();
            state.snd_una = state.iss;
            state.snd_nxt = state.iss.wrapping_add(1);
            state.rcv_nxt = 0;
            state.snd_wnd = u16::MAX as u32;
            state.cwnd = 4 * state.mss;
            state.ssthresh = u16::MAX as usize;
            state.rto_us = DEFAULT_RTO_US;
            state.retransmit_at_us = now + state.rto_us;
            state.retransmit_count = 0;
            state.remote = Endpoint { address, port };
            state.status = Status::SynSent;
            state.error = None;
        }
        let (ctx, seq, mss) = {
            let state = self.state.lock();
            (state.tx_ctx(state.remote), state.iss, state.mss)
        };
        register_connection(&self.arc(), ctx.local, ctx.remote);
        let mut options = Vec::with_capacity(4);
        options.extend_from_slice(&[OPTION_MSS, 4]);
        options.extend_from_slice(&(mss as u16).to_be_bytes());
        let _ = transmit_segment(&ctx, seq, 0, SYN, u16::MAX, &options, &[]);
        if !blocking {
            return Ok(());
        }
        loop {
            {
                let mut state = self.state.lock();
                match state.status {
                    Status::Established => return Ok(()),
                    Status::Closed => {
                        return Err(state
                            .error
                            .take()
                            .map(errno_vfs)
                            .unwrap_or(VfsError::ConnectionRefused));
                    }
                    _ => {}
                }
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::ipc::block_briefly();
        }
    }

    /// `tcp_sendmsg`: copy as much as fits into the send buffer.
    pub fn send(&self, data: &[u8], flags: u64) -> Result<usize, VfsError> {
        const MSG_DONTWAIT: u64 = 0x40;
        const MSG_NOSIGNAL: u64 = 0x4000;
        let _ = (MSG_NOSIGNAL, flags);
        if data.is_empty() {
            return Ok(0);
        }
        let mut written = 0usize;
        loop {
            let (accepted, plans) = {
                let mut state = self.state.lock();
                if let Some(error) = state.error.take() {
                    return Err(errno_vfs(error));
                }
                match state.status {
                    Status::Established | Status::CloseWait => {}
                    Status::Closed | Status::Listen | Status::SynSent | Status::SynRecv => {
                        return Err(VfsError::NotConnected);
                    }
                    _ if state.write_shutdown => return Err(VfsError::BrokenPipe),
                    _ => return Err(VfsError::BrokenPipe),
                }
                if state.write_shutdown {
                    return Err(VfsError::BrokenPipe);
                }
                let room = state.sndbuf.saturating_sub(state.snd_buf.len());
                let take = room.min(data.len() - written);
                for byte in &data[written..written + take] {
                    state.snd_buf.push_back(*byte);
                }
                if take != 0 {
                    state.need_ack = true;
                }
                let mut plans = Vec::new();
                flush_locked(&mut state, time::uptime_micros(), &mut plans);
                (take, plans)
            };
            execute(tx_ctx(self), plans);
            written += accepted;
            if written == data.len() {
                return Ok(written);
            }
            if flags & MSG_DONTWAIT != 0 || self.nonblock.load(Ordering::Acquire) {
                return if written == 0 {
                    Err(VfsError::WouldBlock)
                } else {
                    Ok(written)
                };
            }
            if crate::signal::pending_for_current() {
                return if written == 0 {
                    Err(VfsError::Interrupted)
                } else {
                    Ok(written)
                };
            }
            crate::ipc::block_briefly();
        }
    }

    /// `tcp_recvmsg`.
    pub fn receive(&self, max: usize, flags: u64) -> Result<(Vec<u8>, bool), VfsError> {
        const MSG_PEEK: u64 = 2;
        const MSG_DONTWAIT: u64 = 0x40;
        const MSG_WAITALL: u64 = 0x100;
        let peek = flags & MSG_PEEK != 0;
        let mut collected = Vec::new();
        loop {
            let (chunk, eof, plans) = {
                let mut state = self.state.lock();
                if let Some(error) = state.error.take() {
                    return Err(errno_vfs(error));
                }
                if state.status == Status::Listen {
                    return Err(VfsError::NotConnected);
                }
                if state.read_shutdown {
                    return Ok((collected, true));
                }
                if !state.rcv_buf.is_empty() {
                    let take = (max - collected.len()).min(state.rcv_buf.len());
                    let chunk: Vec<u8> = state.rcv_buf.iter().take(take).copied().collect();
                    if !peek {
                        for _ in 0..take {
                            state.rcv_buf.pop_front();
                        }
                        state.need_ack = true;
                    }
                    let mut plans = Vec::new();
                    if !peek {
                        plans.push(Outgoing {
                            flags: ACK,
                            sequence: state.snd_nxt,
                            acknowledgement: state.rcv_nxt,
                            window: state.advertised_window(),
                            options: Vec::new(),
                            payload: Vec::new(),
                        });
                    }
                    let eof = state.peer_fin && state.rcv_buf.is_empty();
                    (chunk, eof, plans)
                } else if state.peer_fin {
                    (Vec::new(), true, Vec::new())
                } else if state.status == Status::Closed
                    || state.status == Status::TimeWait
                    || state.status == Status::LastAck
                {
                    (Vec::new(), true, Vec::new())
                } else {
                    (Vec::new(), false, Vec::new())
                }
            };
            execute(tx_ctx(self), plans);
            let chunk_empty = chunk.is_empty();
            collected.extend_from_slice(&chunk);
            if eof && chunk_empty {
                return Ok((collected, true));
            }
            let want_more = flags & MSG_WAITALL != 0 && collected.len() < max && !eof;
            if collected.len() >= max || !chunk_empty && !want_more {
                return Ok((collected, false));
            }
            if eof {
                return Ok((collected, true));
            }
            if flags & MSG_DONTWAIT != 0 || self.nonblock.load(Ordering::Acquire) {
                if collected.is_empty() {
                    return Err(VfsError::WouldBlock);
                }
                return Ok((collected, false));
            }
            if crate::signal::pending_for_current() {
                if collected.is_empty() {
                    return Err(VfsError::Interrupted);
                }
                return Ok((collected, false));
            }
            crate::ipc::block_briefly();
        }
    }

    pub fn shutdown(&self, how: u32) -> Result<(), VfsError> {
        let now = time::uptime_micros();
        let mut plans = Vec::new();
        {
            // SHUT_RD = 0, SHUT_WR = 1, SHUT_RDWR = 2.
            let mut state = self.state.lock();
            if how == 0 || how == 2 {
                state.read_shutdown = true;
                state.rcv_buf.clear();
                state.ooo.clear();
                state.ooo_bytes = 0;
            }
            if how == 1 || how == 2 {
                state.write_shutdown = true;
                match state.status {
                    Status::Established | Status::CloseWait => {
                        state.fin_pending = true;
                        flush_locked(&mut state, now, &mut plans);
                    }
                    Status::SynSent | Status::Listen | Status::Closed => {}
                    _ => {}
                }
            }
        }
        execute(tx_ctx(self), plans);
        crate::ipc::wake_pollers();
        Ok(())
    }

    pub fn take_error(&self) -> i32 {
        self.state.lock().error.take().unwrap_or(0)
    }

    pub fn setsockopt(&self, level: u64, name: u64, value: &[u8]) -> Result<(), VfsError> {
        match level {
            SOL_IP => self.state.lock().ip_opts.set(SOCK_STREAM, name, value),
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
                    SO_KEEPALIVE => {
                        self.state.lock().keepalive = bool_val()?;
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
                    13 => {
                        // SO_LINGER (struct linger: 8 bytes)
                        if value.len() < 8 {
                            return Err(VfsError::InvalidArgument);
                        }
                        Ok(())
                    }
                    SO_ACCEPTCONN => Ok(()),
                    _ => Err(VfsError::NoProtocolOption),
                }
            }
            SOL_TCP => match name {
                1 => Ok(()),                  // TCP_NODELAY: ACKs are immediate anyway
                2 => Ok(()),                  // TCP_MAXSEG
                3 => Ok(()),                  // TCP_CORK
                13 => Ok(()),                 // TCP_CONGESTION; only Reno exists
                4 | 5 | 6 | 7 | 18 => Ok(()), // keepalive / quickack / defer_accept / user_timeout
                _ => Err(VfsError::NoProtocolOption),
            },
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    pub fn getsockopt(&self, level: u64, name: u64) -> Result<Vec<u8>, VfsError> {
        let int = |value: u32| Ok(value.to_le_bytes().to_vec());
        match level {
            SOL_IP => {
                let state = self.state.lock();
                let peer = (state.status != Status::Closed
                    && state.status != Status::Listen
                    && state.remote.port != 0)
                    .then_some(state.remote.address);
                state.ip_opts.get(SOCK_STREAM, ip::IPPROTO_TCP, peer, name)
            }
            SOL_SOCKET => {
                let mut state = self.state.lock();
                match name {
                    SO_TYPE => int(SOCK_STREAM),
                    SO_DOMAIN => int(socket::AF_INET as u32),
                    SO_PROTOCOL => int(ip::IPPROTO_TCP as u32),
                    SO_SNDBUF => int(state.sndbuf as u32),
                    SO_RCVBUF => int(state.rcvbuf as u32),
                    SO_ACCEPTCONN => int(u32::from(state.status == Status::Listen)),
                    SO_REUSEADDR => int(u32::from(state.reuseaddr)),
                    SO_REUSEPORT => int(u32::from(state.reuseport)),
                    SO_KEEPALIVE => int(u32::from(state.keepalive)),
                    SO_BROADCAST => int(u32::from(state.broadcast)),
                    SO_DONTROUTE => int(u32::from(state.dontroute)),
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
            SOL_TCP => {
                let state = self.state.lock();
                match name {
                    1 => int(1),
                    2 => int(state.mss as u32),
                    13 => Ok(b"reno\0".to_vec()),
                    _ => Err(VfsError::NoProtocolOption),
                }
            }
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    fn arc(&self) -> Arc<Self> {
        self.me.upgrade().expect("tcp socket used after drop")
    }
}

fn tx_ctx(socket: &TcpSocket) -> TxCtx {
    let state = socket.state.lock();
    state.tx_ctx(state.remote)
}

fn route_mss(destination: Ipv4Addr) -> Option<usize> {
    let route = super::fib::lookup(IpAddr::V4(destination))?;
    let device = super::dev::by_index(route.ifindex)?;
    Some((device.mtu() as usize).saturating_sub(40).min(MAX_MSS))
}

// ---- Port and table management --------------------------------------------

fn port_in_use(address: Ipv4Addr, port: u16) -> bool {
    let listeners = LISTENERS.lock();
    let mut used = listeners.iter().any(|listener| {
        listener.socket.strong_count() != 0
            && listener.local.port == port
            && (listener.local.address.is_unspecified()
                || address.is_unspecified()
                || listener.local.address == address)
    });
    if used {
        return true;
    }
    drop(listeners);
    let connections = CONNECTIONS.lock();
    used = connections.iter().any(|connection| {
        connection.socket.strong_count() != 0
            && connection.local.port == port
            && (connection.local.address.is_unspecified()
                || address.is_unspecified()
                || connection.local.address == address)
    });
    used
}

fn check_port(address: Ipv4Addr, port: u16, reuseaddr: bool) -> Result<u16, VfsError> {
    if port_in_use(address, port) && !reuseaddr {
        return Err(VfsError::AddressInUse);
    }
    Ok(port)
}

fn allocate_port(address: Ipv4Addr) -> Result<u16, VfsError> {
    let span = (EPHEMERAL_END - EPHEMERAL_START) as u32 + 1;
    let start = random_sequence() % span;
    for index in 0..span {
        let port = EPHEMERAL_START + ((start + index) % span) as u16;
        if !port_in_use(address, port) {
            return Ok(port);
        }
    }
    Err(VfsError::AddressInUse)
}

fn register_connection(socket: &Arc<TcpSocket>, local: Endpoint, remote: Endpoint) {
    CONNECTIONS.lock().push(Connection {
        local,
        remote,
        socket: Arc::downgrade(socket),
    });
    arm_timer();
}

fn find_connection(
    local_address: Ipv4Addr,
    local_port: u16,
    remote: Endpoint,
) -> Option<Arc<TcpSocket>> {
    let connections = CONNECTIONS.lock();
    connections.iter().find_map(|connection| {
        if connection.local.port == local_port
            && connection.remote == remote
            && (connection.local.address == local_address
                || connection.local.address.is_unspecified())
        {
            connection.socket.upgrade()
        } else {
            None
        }
    })
}

fn find_listener(local_address: Ipv4Addr, local_port: u16) -> Option<Arc<TcpSocket>> {
    let listeners = LISTENERS.lock();
    let mut wildcard = None;
    for listener in listeners.iter() {
        if listener.local.port != local_port || listener.socket.strong_count() == 0 {
            continue;
        }
        if listener.local.address == local_address {
            return listener.socket.upgrade();
        }
        if listener.local.address.is_unspecified() {
            wildcard = listener.socket.upgrade();
        }
    }
    wildcard
}

// ---- Transmission -----------------------------------------------------------

struct Outgoing {
    flags: u8,
    sequence: u32,
    acknowledgement: u32,
    window: u16,
    options: Vec<u8>,
    payload: Vec<u8>,
}

fn execute(ctx: TxCtx, plans: Vec<Outgoing>) {
    for plan in plans {
        let _ = transmit_segment(
            &ctx,
            plan.sequence,
            plan.acknowledgement,
            plan.flags,
            plan.window,
            &plan.options,
            &plan.payload,
        );
    }
}

fn transmit_segment(
    ctx: &TxCtx,
    sequence: u32,
    acknowledgement: u32,
    flags: u8,
    window: u16,
    options: &[u8],
    payload: &[u8],
) -> Result<(), VfsError> {
    let local = ctx.local;
    let remote = ctx.remote;
    if local.port == 0 || remote.port == 0 || local.address.is_unspecified() {
        return Ok(());
    }
    let header_len = TCP_HEADER_LEN + options.len();
    let length = header_len + payload.len();
    let mut skb = SkBuff::alloc(super::skb::DEFAULT_HEADROOM, length).ok_or(VfsError::NoBuffers)?;
    let segment = skb.put(length).ok_or(VfsError::NoBuffers)?;
    segment[0..2].copy_from_slice(&local.port.to_be_bytes());
    segment[2..4].copy_from_slice(&remote.port.to_be_bytes());
    segment[4..8].copy_from_slice(&sequence.to_be_bytes());
    segment[8..12].copy_from_slice(&acknowledgement.to_be_bytes());
    let offset_flags = ((header_len / 4) as u16) << 12 | flags as u16;
    segment[12..14].copy_from_slice(&offset_flags.to_be_bytes());
    segment[14..16].copy_from_slice(&window.to_be_bytes());
    segment[16..18].copy_from_slice(&[0, 0]);
    segment[18..20].copy_from_slice(&[0, 0]);
    segment[20..header_len].copy_from_slice(options);
    segment[header_len..].copy_from_slice(payload);
    let seed = ip::pseudo_v4(
        local.address,
        remote.address,
        ip::IPPROTO_TCP,
        length as u16,
    );
    let sum = ip::checksum(seed, segment);
    segment[16..18].copy_from_slice(&sum.to_be_bytes());
    output::send_with_options(
        Some(local.address),
        remote.address,
        ctx.template,
        &ctx.ip_options,
        skb,
    )
}

/// Fill the congestion window and send any queued bytes.
fn flush_locked(state: &mut State, now: u64, plans: &mut Vec<Outgoing>) {
    if !matches!(
        state.status,
        Status::Established | Status::CloseWait | Status::FinWait1 | Status::FinWait2
    ) {
        return;
    }
    loop {
        let inflight = state.inflight();
        if state.snd_buf.len() <= inflight {
            break;
        }
        let window = if state.snd_wnd > inflight as u32 {
            state.snd_wnd as usize - inflight
        } else {
            0
        };
        let cwnd = state.cwnd.saturating_sub(inflight);
        let available = (state.snd_buf.len() - inflight).min(window).min(cwnd);
        if available == 0 {
            break;
        }
        let size = available.min(state.mss.max(1));
        let payload: Vec<u8> = state
            .snd_buf
            .iter()
            .skip(inflight)
            .take(size)
            .copied()
            .collect();
        plans.push(Outgoing {
            flags: ACK | PSH,
            sequence: state.snd_nxt,
            acknowledgement: state.rcv_nxt,
            window: state.advertised_window(),
            options: Vec::new(),
            payload,
        });
        if state.retransmit_at_us == 0 {
            state.retransmit_at_us = now + state.rto_us;
            state.snd_una_sent_us = now;
        }
        state.snd_nxt = state.snd_nxt.wrapping_add(size as u32);
    }
    if state.fin_pending && !state.fin_sent && state.snd_buf.len() <= state.inflight() {
        let sequence = state.snd_nxt;
        plans.push(Outgoing {
            flags: FIN | ACK,
            sequence,
            acknowledgement: state.rcv_nxt,
            window: state.advertised_window(),
            options: Vec::new(),
            payload: Vec::new(),
        });
        state.snd_nxt = state.snd_nxt.wrapping_add(1);
        state.fin_sent = true;
        state.fin_seq = sequence;
        state.fin_pending = false;
        state.retransmit_at_us = now + state.rto_us;
        match state.status {
            Status::Established => state.status = Status::FinWait1,
            Status::CloseWait => state.status = Status::LastAck,
            _ => {}
        }
    }
}

// ---- Input processing ------------------------------------------------------

fn parse_options(bytes: &[u8]) -> Option<usize> {
    let mut mss = None;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            OPTION_END => break,
            OPTION_NOP => index += 1,
            kind => {
                let Some(length) = bytes.get(index + 1).copied() else {
                    break;
                };
                if length < 2 || index + length as usize > bytes.len() {
                    break;
                }
                if kind == OPTION_MSS && length == 4 {
                    let value = u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]) as usize;
                    if value != 0 {
                        mss = Some(value);
                    }
                }
                index += length as usize;
            }
        }
    }
    let _ = index;
    mss
}

fn process_ack(state: &mut State, source: &TcpHeader, now: u64, plans: &mut Vec<Outgoing>) {
    if !source.has(ACK) || state.status == Status::Listen {
        return;
    }
    if seq_gt(source.acknowledgement, state.snd_una)
        && seq_le(source.acknowledgement, state.snd_nxt)
    {
        let acked = source.acknowledgement.wrapping_sub(state.snd_una) as usize;
        for _ in 0..acked {
            state.snd_buf.pop_front();
        }
        state.snd_una = source.acknowledgement;
        state.dup_acks = 0;
        if !state.retransmitted && state.snd_una_sent_us != 0 {
            let sample = now.saturating_sub(state.snd_una_sent_us).max(1);
            if state.srtt_us == 0 {
                state.srtt_us = sample;
                state.rttvar_us = sample / 2;
            } else {
                state.rttvar_us = (state.rttvar_us * 3 / 4) + (state.srtt_us.abs_diff(sample) / 4);
                state.srtt_us = (state.srtt_us * 7 / 8) + (sample / 8);
            }
            state.rto_us = (state.srtt_us + 4 * state.rttvar_us).clamp(MIN_RTO_US, MAX_RTO_US);
        }
        state.retransmit_count = 0;
        state.retransmitted = false;
        if state.in_recovery {
            // Reno: leave fast recovery once the retransmission is acknowledged.
            state.cwnd = state.ssthresh;
            state.in_recovery = false;
        } else if state.cwnd < state.ssthresh {
            state.cwnd = (state.cwnd + acked).min(MAX_CWND);
        } else {
            state.cwnd =
                (state.cwnd + (state.mss * acked).max(1) / state.cwnd.max(1)).min(MAX_CWND);
        }
        if state.snd_una == state.snd_nxt {
            state.retransmit_at_us = 0;
        } else {
            state.retransmit_at_us = now + state.rto_us;
            state.snd_una_sent_us = now;
        }
        if state.fin_sent && state.snd_una == state.snd_nxt.wrapping_add(1).wrapping_sub(1) {
            // All outstanding bytes including FIN are acknowledged.
            state.fin_acked = true;
            match state.status {
                Status::FinWait1 => state.status = Status::FinWait2,
                Status::Closing => {
                    state.status = Status::TimeWait;
                    state.time_wait_deadline_us = now + TIME_WAIT_US;
                }
                Status::LastAck => state.status = Status::Closed,
                _ => {}
            }
        }
        flush_locked(state, now, plans);
    } else if source.acknowledgement == state.snd_una
        && source.has(ACK)
        && source.header_len == TCP_HEADER_LEN
        && u32::from(source.window) == state.snd_wnd
        && state.inflight() != 0
    {
        state.dup_acks += 1;
        if state.dup_acks == 3 {
            state.ssthresh = (state.inflight() / 2).max(2 * state.mss);
            state.cwnd = state.ssthresh + 3 * state.mss;
            state.in_recovery = true;
            state.retransmitted = true;
            let length = (state.snd_nxt.wrapping_sub(state.snd_una) as usize).min(state.mss);
            if length != 0 {
                let payload: Vec<u8> = state.snd_buf.iter().take(length).copied().collect();
                plans.push(Outgoing {
                    flags: ACK | PSH,
                    sequence: state.snd_una,
                    acknowledgement: state.rcv_nxt,
                    window: state.advertised_window(),
                    options: Vec::new(),
                    payload,
                });
                state.retransmit_at_us = now + state.rto_us;
            }
        }
    }
    if seq_gt(source.sequence, state.snd_wl1)
        || (source.sequence == state.snd_wl1 && seq_gt(source.acknowledgement, state.snd_wl2))
    {
        state.snd_wnd = source.window as u32;
        state.snd_wl1 = source.sequence;
        state.snd_wl2 = source.acknowledgement;
        flush_locked(state, now, plans);
    }
}

fn drain_out_of_order(state: &mut State) {
    while let Some(data) = state.ooo.remove(&state.rcv_nxt) {
        state.ooo_bytes = state.ooo_bytes.saturating_sub(data.len());
        let room = state.rcvbuf.saturating_sub(state.rcv_buf.len());
        let take = room.min(data.len());
        for byte in &data[..take] {
            state.rcv_buf.push_back(*byte);
        }
        state.rcv_nxt = state.rcv_nxt.wrapping_add(take as u32);
        if take < data.len() {
            // Receive buffer is full; drop the tail and let the peer retransmit.
            state.ooo.insert(state.rcv_nxt, data[take..].to_vec());
            state.ooo_bytes += data.len() - take;
            break;
        }
    }
    if let Some(fin_seq) = state.peer_fin_seq
        && state.rcv_nxt == fin_seq
    {
        state.rcv_nxt = state.rcv_nxt.wrapping_add(1);
        state.peer_fin = true;
        state.peer_fin_seq = None;
    }
}

fn queue_data(
    state: &mut State,
    sequence: u32,
    payload: &[u8],
    fin: bool,
    plans: &mut Vec<Outgoing>,
) {
    let end = sequence.wrapping_add(payload.len() as u32);
    if seq_lt(sequence, state.rcv_nxt) {
        let overlap = state.rcv_nxt.wrapping_sub(sequence) as usize;
        if overlap < payload.len() {
            let fresh = &payload[overlap..];
            let room = state.rcvbuf.saturating_sub(state.rcv_buf.len());
            let take = room.min(fresh.len());
            for byte in &fresh[..take] {
                state.rcv_buf.push_back(*byte);
            }
            state.rcv_nxt = state.rcv_nxt.wrapping_add(take as u32);
        }
    } else if sequence == state.rcv_nxt {
        let room = state.rcvbuf.saturating_sub(state.rcv_buf.len());
        let take = room.min(payload.len());
        for byte in &payload[..take] {
            state.rcv_buf.push_back(*byte);
        }
        state.rcv_nxt = state.rcv_nxt.wrapping_add(take as u32);
        if take < payload.len() {
            return;
        }
        drain_out_of_order(state);
    } else if seq_gt(sequence, state.rcv_nxt) {
        let distance = sequence.wrapping_sub(state.rcv_nxt) as usize;
        if distance <= state.rcvbuf
            && state.ooo_bytes + payload.len() <= state.rcvbuf
            && !state.ooo.contains_key(&sequence)
        {
            state.ooo.insert(sequence, payload.to_vec());
            state.ooo_bytes += payload.len();
        } else {
            return;
        }
    }
    if fin && state.peer_fin_seq.is_none() && !state.peer_fin {
        state.peer_fin_seq = Some(end);
        if state.rcv_nxt == end {
            state.rcv_nxt = state.rcv_nxt.wrapping_add(1);
            state.peer_fin = true;
            state.peer_fin_seq = None;
        }
    }
    drain_out_of_order(state);
    plans.push(Outgoing {
        flags: ACK,
        sequence: state.snd_nxt,
        acknowledgement: state.rcv_nxt,
        window: state.advertised_window(),
        options: Vec::new(),
        payload: Vec::new(),
    });
}

fn transition_after_fin(state: &mut State, now: u64) {
    if !state.peer_fin {
        return;
    }
    match state.status {
        Status::Established => state.status = Status::CloseWait,
        Status::FinWait1 => {
            if state.fin_acked {
                state.status = Status::TimeWait;
                state.time_wait_deadline_us = now + TIME_WAIT_US;
            } else {
                state.status = Status::Closing;
            }
        }
        Status::FinWait2 => {
            state.status = Status::TimeWait;
            state.time_wait_deadline_us = now + TIME_WAIT_US;
        }
        _ => {}
    }
}

fn reset_connection(state: &mut State, error: i32) {
    state.error = Some(error);
    state.status = Status::Closed;
    state.retransmit_at_us = 0;
    state.ooo.clear();
    state.ooo_bytes = 0;
    state.snd_buf.clear();
}

/// `tcp_v4_rcv`.
pub fn receive(dev: &Arc<NetDevice>, meta: &IpMeta, mut skb: SkBuff) {
    let Some(header) = TcpHeader::parse(skb.data()) else {
        return;
    };
    let total = header.header_len + skb.data().len().saturating_sub(header.header_len);
    let segment_length = skb.len();
    if header.checksum != 0 {
        let seed = ip::pseudo_v4(
            meta.source,
            meta.destination,
            ip::IPPROTO_TCP,
            segment_length as u16,
        );
        if ip::checksum(seed, skb.data()) != 0 {
            return;
        }
    }
    let _ = total;
    let payload = skb.data()[header.header_len..].to_vec();
    skb.trim(header.header_len);
    let local_address = meta.destination;
    let remote = Endpoint {
        address: meta.source,
        port: header.source_port,
    };
    let Some(socket) = find_connection(local_address, header.destination_port, remote) else {
        if header.has(RST) {
            return;
        }
        if header.has(SYN) && !header.has(ACK) {
            if let Some(listener) = find_listener(local_address, header.destination_port) {
                accept_syn(&listener, local_address, remote, &header, &payload);
                return;
            }
        }
        // No socket: refuse the segment so the peer fails fast.
        send_reset(local_address, remote, &header, payload.len());
        return;
    };
    on_segment(&socket, local_address, remote, &header, &payload);
    let _ = dev;
}

fn send_reset(local: Ipv4Addr, remote: Endpoint, source: &TcpHeader, payload_len: usize) {
    let (sequence, acknowledgement, flags) = if source.has(ACK) {
        (source.acknowledgement, 0, RST)
    } else {
        (
            0,
            source
                .sequence
                .wrapping_add(payload_len as u32)
                .wrapping_add(source.has(SYN) as u32)
                .wrapping_add(source.has(FIN) as u32),
            RST | ACK,
        )
    };
    let local = Endpoint {
        address: local,
        port: source.destination_port,
    };
    let _ = transmit_segment(
        &TxCtx::plain(local, remote),
        sequence,
        acknowledgement,
        flags,
        0,
        &[],
        &[],
    );
}

fn accept_syn(
    listener: &Arc<TcpSocket>,
    local_address: Ipv4Addr,
    remote: Endpoint,
    header: &TcpHeader,
    _payload: &[u8],
) {
    let (local, backlog, parent, inherited_ip_opts) = {
        let state = listener.state.lock();
        (
            Endpoint {
                address: if state.local.address.is_unspecified() {
                    local_address
                } else {
                    state.local.address
                },
                port: state.local.port,
            },
            state.backlog,
            listener.arc(),
            state.ip_opts.clone(),
        )
    };
    {
        let mut parent_state = listener.state.lock();
        if parent_state.syn_pending >= backlog.saturating_add(1) {
            return;
        }
        parent_state.syn_pending += 1;
    }
    let child = TcpSocket::create(true);
    let mss = parse_options(&header.options)
        .unwrap_or(DEFAULT_MSS)
        .min(MAX_MSS);
    let iss = random_sequence();
    let now = time::uptime_micros();
    let ctx = {
        let mut state = child.state.lock();
        state.local = local;
        state.remote = remote;
        state.bound = true;
        state.status = Status::SynRecv;
        state.parent = Some(Arc::downgrade(&parent));
        state.ip_opts = inherited_ip_opts;
        state.iss = iss;
        state.snd_una = iss;
        state.snd_nxt = iss.wrapping_add(1);
        state.peer_mss = mss;
        state.mss = route_mss(remote.address)
            .unwrap_or(DEFAULT_MSS)
            .min(MAX_MSS);
        state.cwnd = 4 * state.mss;
        state.snd_wnd = header.window as u32;
        state.rcv_nxt = header.sequence.wrapping_add(1);
        state.snd_wl1 = header.sequence;
        state.rto_us = DEFAULT_RTO_US;
        state.retransmit_at_us = now + state.rto_us;
        state.tx_ctx(remote)
    };
    register_connection(&child, local, remote);
    let mut options = Vec::with_capacity(4);
    options.extend_from_slice(&[OPTION_MSS, 4]);
    options.extend_from_slice(
        &(route_mss(remote.address).unwrap_or(DEFAULT_MSS) as u16).to_be_bytes(),
    );
    let _ = transmit_segment(
        &ctx,
        iss,
        header.sequence.wrapping_add(1),
        SYN | ACK,
        u16::MAX,
        &options,
        &[],
    );
}

fn on_segment(
    socket: &Arc<TcpSocket>,
    local_address: Ipv4Addr,
    remote: Endpoint,
    header: &TcpHeader,
    payload: &[u8],
) {
    let now = time::uptime_micros();
    let mut plans = Vec::new();
    let mut close_after = false;
    {
        let mut state = socket.state.lock();
        if state.status == Status::TimeWait {
            if header.has(FIN) {
                plans.push(Outgoing {
                    flags: ACK,
                    sequence: state.snd_nxt,
                    acknowledgement: state.rcv_nxt,
                    window: state.advertised_window(),
                    options: Vec::new(),
                    payload: Vec::new(),
                });
            }
            let ctx = state.tx_ctx(remote);
            drop(state);
            execute(ctx, plans);
            return;
        }
        if header.has(RST) {
            let error = match state.status {
                Status::SynSent => abi::ECONNREFUSED as i32,
                Status::CloseWait => abi::EPIPE as i32,
                _ => abi::ECONNRESET as i32,
            };
            reset_connection(&mut state, error);
            drop(state);
            crate::ipc::wake_pollers();
            return;
        }
        let _ = local_address;
        match state.status {
            Status::SynSent => {
                if header.has(SYN) && header.has(ACK) {
                    if header.acknowledgement != state.snd_nxt {
                        plans.push(Outgoing {
                            flags: RST,
                            sequence: header.acknowledgement,
                            acknowledgement: 0,
                            window: 0,
                            options: Vec::new(),
                            payload: Vec::new(),
                        });
                    } else {
                        if let Some(peer_mss) = parse_options(&header.options) {
                            state.peer_mss = peer_mss.min(MAX_MSS);
                            state.mss = state.mss.min(state.peer_mss);
                        }
                        state.snd_una = header.acknowledgement;
                        state.snd_wnd = header.window as u32;
                        state.snd_wl1 = header.sequence;
                        state.snd_wl2 = header.acknowledgement;
                        state.rcv_nxt = header.sequence.wrapping_add(1);
                        state.status = Status::Established;
                        state.retransmit_at_us = 0;
                        state.error = None;
                        ESTABLISHED_TOTAL.fetch_add(1, Ordering::Relaxed);
                        plans.push(Outgoing {
                            flags: ACK,
                            sequence: state.snd_nxt,
                            acknowledgement: state.rcv_nxt,
                            window: state.advertised_window(),
                            options: Vec::new(),
                            payload: Vec::new(),
                        });
                    }
                } else if header.has(SYN) {
                    state.rcv_nxt = header.sequence.wrapping_add(1);
                    state.snd_wnd = header.window as u32;
                    state.status = Status::SynRecv;
                    plans.push(Outgoing {
                        flags: SYN | ACK,
                        sequence: state.iss,
                        acknowledgement: state.rcv_nxt,
                        window: state.advertised_window(),
                        options: vec![OPTION_MSS, 4, (state.mss >> 8) as u8, state.mss as u8],
                        payload: Vec::new(),
                    });
                    state.retransmit_at_us = now + state.rto_us;
                }
            }
            Status::SynRecv => {
                if header.has(SYN) && !header.has(ACK) {
                    plans.push(Outgoing {
                        flags: SYN | ACK,
                        sequence: state.iss,
                        acknowledgement: state.rcv_nxt,
                        window: state.advertised_window(),
                        options: vec![OPTION_MSS, 4, (state.mss >> 8) as u8, state.mss as u8],
                        payload: Vec::new(),
                    });
                } else if header.has(ACK) && header.acknowledgement == state.snd_nxt {
                    state.snd_una = header.acknowledgement;
                    state.snd_wnd = header.window as u32;
                    state.snd_wl1 = header.sequence;
                    state.snd_wl2 = header.acknowledgement;
                    state.status = Status::Established;
                    state.retransmit_at_us = 0;
                    state.error = None;
                    ESTABLISHED_TOTAL.fetch_add(1, Ordering::Relaxed);
                    if let Some(parent) = state.parent.clone() {
                        drop(state);
                        if let Some(parent) = parent.upgrade() {
                            let mut parent_state = parent.state.lock();
                            parent_state.syn_pending = parent_state.syn_pending.saturating_sub(1);
                            parent_state.pending.push_back(socket.clone());
                            drop(parent_state);
                            crate::ipc::wake_pollers();
                        }
                        state = socket.state.lock();
                    }
                }
            }
            Status::Established
            | Status::FinWait1
            | Status::FinWait2
            | Status::CloseWait
            | Status::Closing
            | Status::LastAck => {
                process_ack(&mut state, header, now, &mut plans);
                if !payload.is_empty() || header.has(FIN) {
                    if header.has(SYN) {
                        plans.push(Outgoing {
                            flags: ACK,
                            sequence: state.snd_nxt,
                            acknowledgement: state.rcv_nxt,
                            window: state.advertised_window(),
                            options: Vec::new(),
                            payload: Vec::new(),
                        });
                    } else {
                        queue_data(
                            &mut state,
                            header.sequence,
                            payload,
                            header.has(FIN),
                            &mut plans,
                        );
                        transition_after_fin(&mut state, now);
                    }
                } else if header.has(ACK) && state.need_ack {
                    state.need_ack = false;
                }
            }
            Status::Listen | Status::Closed | Status::TimeWait => {}
        }
        if state.status == Status::Closed {
            close_after = true;
        }
        let ctx = state.tx_ctx(state.remote);
        drop(state);
        execute(ctx, plans);
    }
    if close_after {
        crate::ipc::wake_pollers();
    }
    let _ = remote;
}

// ---- Timer -----------------------------------------------------------------

fn arm_timer() {
    if !TIMER_ARMED.swap(true, Ordering::AcqRel) {
        super::timer::schedule_in(100_000, Box::new(tick));
    }
}

fn tick() {
    TIMER_ARMED.store(false, Ordering::Release);
    let sockets: Vec<Arc<TcpSocket>> = {
        let mut connections = CONNECTIONS.lock();
        connections.retain(|connection| connection.socket.strong_count() != 0);
        connections
            .iter()
            .filter_map(|connection| connection.socket.upgrade())
            .collect()
    };
    let listeners: Vec<Arc<TcpSocket>> = {
        let mut listeners = LISTENERS.lock();
        listeners.retain(|listener| listener.socket.strong_count() != 0);
        listeners
            .iter()
            .filter_map(|listener| listener.socket.upgrade())
            .collect()
    };
    let mut alive = false;
    for socket in sockets {
        alive = true;
        socket.on_timer();
    }
    for _socket in listeners {
        alive = true;
    }
    if alive || !CONNECTIONS.lock().is_empty() || !LISTENERS.lock().is_empty() {
        arm_timer();
    }
}

impl TcpSocket {
    fn on_timer(&self) {
        let now = time::uptime_micros();
        let mut plans = Vec::new();
        let mut failed = None;
        {
            let mut state = self.state.lock();
            match state.status {
                Status::SynSent | Status::SynRecv => {
                    if state.retransmit_at_us != 0 && now >= state.retransmit_at_us {
                        if state.retransmit_count >= MAX_SYN_RETRIES {
                            failed = Some(abi::ETIMEDOUT as i32);
                        } else {
                            state.retransmit_count += 1;
                            state.rto_us = (state.rto_us * 2).min(MAX_RTO_US);
                            state.retransmit_at_us = now + state.rto_us;
                            let options =
                                vec![OPTION_MSS, 4, (state.mss >> 8) as u8, state.mss as u8];
                            if state.status == Status::SynSent {
                                plans.push(Outgoing {
                                    flags: SYN,
                                    sequence: state.iss,
                                    acknowledgement: 0,
                                    window: u16::MAX,
                                    options,
                                    payload: Vec::new(),
                                });
                            } else {
                                plans.push(Outgoing {
                                    flags: SYN | ACK,
                                    sequence: state.iss,
                                    acknowledgement: state.rcv_nxt,
                                    window: state.advertised_window(),
                                    options,
                                    payload: Vec::new(),
                                });
                            }
                        }
                    }
                }
                Status::Established
                | Status::FinWait1
                | Status::FinWait2
                | Status::CloseWait
                | Status::Closing
                | Status::LastAck => {
                    if state.retransmit_at_us != 0 && now >= state.retransmit_at_us {
                        if state.retransmit_count >= MAX_RETRIES {
                            failed = Some(abi::ETIMEDOUT as i32);
                        } else {
                            state.retransmit_count += 1;
                            state.retransmitted = true;
                            state.rto_us = (state.rto_us * 2).min(MAX_RTO_US);
                            state.retransmit_at_us = now + state.rto_us;
                            state.cwnd = state.mss;
                            state.in_recovery = false;
                            state.ssthresh = (state.inflight() / 2).max(2 * state.mss);
                            let transmitted = state.inflight().min(state.mss);
                            if transmitted != 0 {
                                let payload: Vec<u8> =
                                    state.snd_buf.iter().take(transmitted).copied().collect();
                                plans.push(Outgoing {
                                    flags: ACK | PSH,
                                    sequence: state.snd_una,
                                    acknowledgement: state.rcv_nxt,
                                    window: state.advertised_window(),
                                    options: Vec::new(),
                                    payload,
                                });
                            } else if state.fin_sent && state.snd_una == state.fin_seq {
                                plans.push(Outgoing {
                                    flags: FIN | ACK,
                                    sequence: state.fin_seq,
                                    acknowledgement: state.rcv_nxt,
                                    window: state.advertised_window(),
                                    options: Vec::new(),
                                    payload: Vec::new(),
                                });
                            }
                        }
                    } else if state.snd_wnd == 0 && state.pending_bytes() != 0 {
                        // Zero-window probe: prompt the peer for an update.
                        plans.push(Outgoing {
                            flags: ACK,
                            sequence: state.snd_nxt,
                            acknowledgement: state.rcv_nxt,
                            window: state.advertised_window(),
                            options: Vec::new(),
                            payload: Vec::new(),
                        });
                    }
                    if state.status == Status::TimeWait && now >= state.time_wait_deadline_us {
                        state.status = Status::Closed;
                    }
                }
                Status::TimeWait => {
                    if now >= state.time_wait_deadline_us {
                        state.status = Status::Closed;
                    }
                }
                _ => {}
            }
            if let Some(error) = failed {
                reset_connection(&mut state, error);
            }
            let ctx = state.tx_ctx(state.remote);
            drop(state);
            execute(ctx, plans);
        }
        if failed.is_some() {
            crate::ipc::wake_pollers();
        }
    }
}

// ---- ICMP errors -----------------------------------------------------------

pub fn error(event: &icmp::IcmpError<'_>) {
    let Some(header) = super::ipv4::header::Ipv4Header::parse(event.quoted) else {
        return;
    };
    if header.protocol != ip::IPPROTO_TCP || event.quoted.len() < header.header_len() + 4 {
        return;
    }
    let tcp = &event.quoted[header.header_len()..];
    let source_port = u16::from_be_bytes([tcp[0], tcp[1]]);
    let destination_port = u16::from_be_bytes([tcp[2], tcp[3]]);
    let remote = Endpoint {
        address: header.source,
        port: source_port,
    };
    let Some(socket) = find_connection(header.destination, destination_port, remote) else {
        return;
    };
    let error = match (event.icmp_type, event.code) {
        (icmp::ICMP_DEST_UNREACH, icmp::ICMP_PORT_UNREACH) => abi::ECONNREFUSED as i32,
        (icmp::ICMP_DEST_UNREACH, icmp::ICMP_HOST_UNREACH) => abi::EHOSTUNREACH as i32,
        (icmp::ICMP_DEST_UNREACH, _) => abi::ENETUNREACH as i32,
        _ => abi::ENETUNREACH as i32,
    };
    {
        let mut state = socket.state.lock();
        if state.status == Status::SynSent || state.status == Status::Established {
            reset_connection(&mut state, error);
        }
    }
    crate::ipc::wake_pollers();
}

// ---- Socket glue ------------------------------------------------------------

impl Inode for TcpSocket {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::Socket, 0o644, 0))
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        let (data, _eof) = self.receive(buffer.len(), 0)?;
        buffer[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }

    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        self.send(buffer, 0)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(self as *const Self);
    }

    fn poll(&self) -> u32 {
        const POLLIN: u32 = 1;
        const POLLOUT: u32 = 4;
        const POLLERR: u32 = 8;
        const POLLHUP: u32 = 0x10;
        let state = self.state.lock();
        let mut events = 0;
        match state.status {
            Status::Listen => {
                if !state.pending.is_empty() {
                    events |= POLLIN;
                }
            }
            Status::SynSent | Status::SynRecv => {}
            Status::Closed => events |= POLLHUP,
            _ => {
                if !state.rcv_buf.is_empty() || state.peer_fin {
                    events |= POLLIN;
                }
                if state.write_shutdown {
                    events |= POLLHUP;
                } else if state.snd_buf.len() < state.sndbuf {
                    events |= POLLOUT;
                }
            }
        }
        if state.error.is_some() {
            events |= POLLERR;
        }
        events
    }

    fn ioctl(&self, cmd: u64, _argument: u64) -> Result<u64, VfsError> {
        const FIONREAD: u64 = 0x541B;
        match cmd {
            FIONREAD => Ok(self.state.lock().rcv_buf.len() as u64),
            _ => Err(VfsError::InvalidArgument),
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.set_nonblock(nonblock);
        Ok(())
    }
}

impl Drop for TcpSocket {
    fn drop(&mut self) {
        let (local, remote, status, parent, pending) = {
            let state = self.state.lock();
            (
                state.local,
                state.remote,
                state.status,
                state.parent.clone(),
                state.pending.len(),
            )
        };
        let mut plans = Vec::new();
        let ctx = {
            let mut state = self.state.lock();
            if matches!(
                state.status,
                Status::Established | Status::CloseWait | Status::SynRecv | Status::SynSent
            ) {
                if state.status == Status::SynRecv {
                    plans.push(Outgoing {
                        flags: RST,
                        sequence: state.snd_nxt,
                        acknowledgement: state.rcv_nxt,
                        window: 0,
                        options: Vec::new(),
                        payload: Vec::new(),
                    });
                } else if !state.rcv_buf.is_empty() {
                    // Linux aborts a connection closed with unread data.
                    plans.push(Outgoing {
                        flags: RST | ACK,
                        sequence: state.snd_nxt,
                        acknowledgement: state.rcv_nxt,
                        window: 0,
                        options: Vec::new(),
                        payload: Vec::new(),
                    });
                } else if !state.write_shutdown {
                    state.fin_pending = true;
                    flush_locked(&mut state, time::uptime_micros(), &mut plans);
                }
            }
            state.status = Status::Closed;
            state.tx_ctx(remote)
        };
        execute(ctx, plans);
        CONNECTIONS.lock().retain(|connection| {
            connection.socket.strong_count() != 0
                || !(connection.local == local && connection.remote == remote)
        });
        LISTENERS
            .lock()
            .retain(|listener| listener.socket.strong_count() != 0 || !(listener.local == local));
        if status == Status::SynRecv
            && let Some(parent) = parent.and_then(|parent| parent.upgrade())
        {
            let mut parent_state = parent.state.lock();
            parent_state.syn_pending = parent_state.syn_pending.saturating_sub(1);
        }
        let _ = pending;
        crate::ipc::poll_kick();
    }
}

/// Family constructor for `AF_INET`/`SOCK_STREAM`.
pub fn create_stream(
    kind: u32,
    protocol: u32,
    nonblock: bool,
) -> Result<Arc<crate::vfs::OpenFile>, VfsError> {
    if kind != SOCK_STREAM || (protocol != 0 && protocol != ip::IPPROTO_TCP as u32) {
        return Err(VfsError::ProtocolType);
    }
    let socket = TcpSocket::create(nonblock);
    Ok(socket::socket_file(socket, "socket:tcp", nonblock))
}

pub fn init() {
    input::register_protocol(ip::IPPROTO_TCP, receive);
    icmp::register_error_handler(ip::IPPROTO_TCP, error);
}

pub fn established_total() -> u64 {
    ESTABLISHED_TOTAL.load(Ordering::Relaxed)
}

/// Snapshot for `sock_diag`.
pub fn diagnostics() -> Vec<(u64, Ipv4Addr, u16, Ipv4Addr, u16, u8)> {
    let mut list: Vec<(u64, Ipv4Addr, u16, Ipv4Addr, u16, u8)> = LISTENERS
        .lock()
        .iter()
        .filter_map(|listener| {
            let socket = listener.socket.upgrade()?;
            let state = socket.state.lock();
            Some((
                socket.ino,
                state.local.address,
                state.local.port,
                state.remote.address,
                state.remote.port,
                state.status as u8,
            ))
        })
        .collect();
    list.extend(CONNECTIONS.lock().iter().filter_map(|connection| {
        let socket = connection.socket.upgrade()?;
        let state = socket.state.lock();
        Some((
            socket.ino,
            state.local.address,
            state.local.port,
            state.remote.address,
            state.remote.port,
            state.status as u8,
        ))
    }));
    list
}
