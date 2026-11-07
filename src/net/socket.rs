/*
 *
 *       src/net/socket.rs
 *       Address-family registry: net_proto_family and the socket(2) entry
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mirrors `net/socket.c`'s `net_families[]` table. Families register a
//! constructor; `socket(2)` validates the type bits, instantiates the family
//! object and allocates a file descriptor around it. Protocol families that
//! need bespoke syscall paths (netlink) still install their inode here so
//! `socket()` has a single dispatch point.

use crate::{
    ipc::unix,
    net::netlink::{self, NetlinkSocket},
    sync::SpinLock,
    vfs::{self, Inode, OpenFile, VfsError, abi},
};
use alloc::{string::String, sync::Arc, vec::Vec};
use core::net::{Ipv4Addr, Ipv6Addr};

pub const AF_UNIX: u64 = 1;
pub const AF_INET: u64 = 2;
pub const AF_INET6: u64 = 10;
pub const AF_NETLINK: u64 = 16;

/// `SOCK_*` type bits shared by every family.
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const SOCK_RAW: u32 = 3;
pub const SOCK_RDM: u32 = 4;
pub const SOCK_SEQPACKET: u32 = 5;
pub const SOCK_DCCP: u32 = 6;
pub const SOCK_PACKET: u32 = 10;
pub const SOCK_CLOEXEC: u32 = 0o2000000;
pub const SOCK_NONBLOCK: u32 = 0o4000;

// SOL_* levels and SOL_SOCKET / SOL_IP option numbers matching Linux UAPI.
pub const SOL_IP: u64 = 0;
pub const SOL_SOCKET: u64 = 1;
pub const SOL_TCP: u64 = 6;
pub const SOL_IPV6: u64 = 41;
pub const SOL_ICMPV6: u64 = 58;
pub const SOL_RAW: u64 = 255;

pub const SO_DEBUG: u64 = 1;
pub const SO_REUSEADDR: u64 = 2;
pub const SO_TYPE: u64 = 3;
pub const SO_ERROR: u64 = 4;
pub const SO_DONTROUTE: u64 = 5;
pub const SO_BROADCAST: u64 = 6;
pub const SO_SNDBUF: u64 = 7;
pub const SO_RCVBUF: u64 = 8;
pub const SO_KEEPALIVE: u64 = 9;
pub const SO_PRIORITY: u64 = 12;
pub const SO_REUSEPORT: u64 = 15;
pub const SO_RCVTIMEO_OLD: u64 = 20;
pub const SO_SNDTIMEO_OLD: u64 = 21;
pub const SO_BINDTODEVICE: u64 = 25;
pub const SO_TIMESTAMP_OLD: u64 = 29;
pub const SO_ACCEPTCONN: u64 = 30;
pub const SO_SNDBUFFORCE: u64 = 32;
pub const SO_RCVBUFFORCE: u64 = 33;
pub const SO_TIMESTAMPNS_OLD: u64 = 35;
pub const SO_MARK: u64 = 36;
pub const SO_PROTOCOL: u64 = 38;
pub const SO_DOMAIN: u64 = 39;
pub const SO_TIMESTAMP_NEW: u64 = 63;
pub const SO_TIMESTAMPNS_NEW: u64 = 64;
pub const SO_RCVTIMEO_NEW: u64 = 66;
pub const SO_SNDTIMEO_NEW: u64 = 67;

// IPPROTO_IP / SOL_IP socket options (include/uapi/linux/in.h).
pub const IP_TOS: u64 = 1;
pub const IP_TTL: u64 = 2;
pub const IP_HDRINCL: u64 = 3;
pub const IP_OPTIONS: u64 = 4;
pub const IP_ROUTER_ALERT: u64 = 5;
pub const IP_RECVOPTS: u64 = 6;
pub const IP_RETOPTS: u64 = 7;
pub const IP_PKTINFO: u64 = 8;
pub const IP_PKTOPTIONS: u64 = 9;
pub const IP_MTU_DISCOVER: u64 = 10;
pub const IP_RECVERR: u64 = 11;
pub const IP_RECVTTL: u64 = 12;
pub const IP_RECVTOS: u64 = 13;
pub const IP_MTU: u64 = 14;
pub const IP_FREEBIND: u64 = 15;
pub const IP_PASSSEC: u64 = 18;
pub const IP_TRANSPARENT: u64 = 19;
pub const IP_ORIGDSTADDR: u64 = 20;
pub const IP_MINTTL: u64 = 21;
pub const IP_NODEFRAG: u64 = 22;
pub const IP_CHECKSUM: u64 = 23;
pub const IP_BIND_ADDRESS_NO_PORT: u64 = 24;
pub const IP_RECVFRAGSIZE: u64 = 25;
pub const IP_RECVERR_RFC4884: u64 = 26;
pub const IP_MULTICAST_IF: u64 = 32;
pub const IP_MULTICAST_TTL: u64 = 33;
pub const IP_MULTICAST_LOOP: u64 = 34;
pub const IP_ADD_MEMBERSHIP: u64 = 35;
pub const IP_DROP_MEMBERSHIP: u64 = 36;
pub const IP_UNBLOCK_SOURCE: u64 = 37;
pub const IP_BLOCK_SOURCE: u64 = 38;
pub const IP_ADD_SOURCE_MEMBERSHIP: u64 = 39;
pub const IP_DROP_SOURCE_MEMBERSHIP: u64 = 40;
pub const MCAST_JOIN_GROUP: u64 = 42;
pub const MCAST_LEAVE_GROUP: u64 = 45;
pub const IP_MULTICAST_ALL: u64 = 49;
pub const IP_UNICAST_IF: u64 = 50;
pub const IP_LOCAL_PORT_RANGE: u64 = 51;
pub const IP_PROTOCOL: u64 = 52;

pub const IP_PMTUDISC_DONT: u8 = 0;
pub const IP_PMTUDISC_WANT: u8 = 1;
pub const IP_PMTUDISC_DO: u8 = 2;
pub const IP_PMTUDISC_PROBE: u8 = 3;
pub const IP_PMTUDISC_INTERFACE: u8 = 4;
pub const IP_PMTUDISC_OMIT: u8 = 5;

/// Family constructor: `(type without flags, protocol, nonblock)`.
pub type FamilyCreate = fn(u32, u32, bool) -> Result<Arc<OpenFile>, VfsError>;

struct Family {
    domain: u64,
    create: FamilyCreate,
}

static FAMILIES: SpinLock<Vec<Family>> = SpinLock::new(Vec::new());

/// `sock_register`. Replacing an existing family is refused, as in Linux.
pub fn register_family(domain: u64, create: FamilyCreate) -> Result<(), VfsError> {
    let mut families = FAMILIES.lock();
    if families.iter().any(|family| family.domain == domain) {
        return Err(VfsError::AlreadyExists);
    }
    families.push(Family { domain, create });
    Ok(())
}

/// Register the built-in families. Called once from `net::init`.
pub fn init() {
    if let Err(error) = register_family(AF_UNIX, unix_create) {
        crate::kwarn!("net: AF_UNIX family registration failed: {error:?}");
    }
    if let Err(error) = register_family(AF_NETLINK, netlink_create) {
        crate::kwarn!("net: AF_NETLINK family registration failed: {error:?}");
    }
}

fn unix_create(kind: u32, protocol: u32, nonblock: bool) -> Result<Arc<OpenFile>, VfsError> {
    if protocol != 0 {
        return Err(VfsError::ProtocolType);
    }
    let kind = unix::SocketKind::from_raw(kind)?;
    let socket = unix::UnixSocket::new_socket(kind, nonblock);
    socket.set_nonblock(nonblock);
    Ok(unix::open_file(socket, socket_flags(nonblock)))
}

fn netlink_create(kind: u32, protocol: u32, nonblock: bool) -> Result<Arc<OpenFile>, VfsError> {
    if kind != SOCK_RAW && kind != SOCK_DGRAM {
        return Err(VfsError::ProtocolType);
    }
    if protocol as usize >= netlink::MAX_LINKS {
        return Err(VfsError::ProtocolType);
    }
    let socket = NetlinkSocket::create(protocol, nonblock)?;
    Ok(socket_file(socket, "socket:netlink", nonblock))
}

/// Wrap any socket inode in an `OpenFile` with the canonical flags.
pub fn socket_file(inode: Arc<dyn Inode>, name: &str, nonblock: bool) -> Arc<OpenFile> {
    Arc::new(OpenFile::new(
        inode,
        String::from(name),
        socket_flags(nonblock),
    ))
}

pub fn socket_flags(nonblock: bool) -> u32 {
    abi::O_RDWR | if nonblock { abi::O_NONBLOCK } else { 0 }
}

/// The `socket(2)` implementation shared by every family.
pub fn socket(domain: u64, kind: u64, protocol: u64) -> i64 {
    let raw = kind as u32;
    let base = raw & !(SOCK_CLOEXEC | SOCK_NONBLOCK);
    let nonblock = raw & SOCK_NONBLOCK != 0;
    let cloexec = raw & SOCK_CLOEXEC != 0;
    let family = FAMILIES
        .lock()
        .iter()
        .find(|family| family.domain == domain)
        .map(|family| family.create);
    let Some(create) = family else {
        return -abi::EAFNOSUPPORT;
    };
    let file = match create(base, protocol as u32, nonblock) {
        Ok(file) => file,
        Err(error) => return error.to_errno(),
    };
    match vfs::current_fd_table()
        .lock()
        .alloc_fd(0, crate::vfs::FdEntry { file, cloexec })
    {
        Ok(fd) => fd as i64,
        Err(error) => error.to_errno(),
    }
}

/// Downcast the inode behind `fd` to a concrete type, for families with
/// dedicated syscall paths.
pub fn inode_of<T: Inode>(fd: u64) -> Option<Arc<T>> {
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
    any.downcast::<T>().ok()
}

/// `sockaddr_in` on the wire: family, port (big endian), address, padding.
pub const SOCKADDR_IN_LEN: usize = 16;

/// `struct sockaddr_in6`.
pub const SOCKADDR_IN6_LEN: usize = 28;

pub fn decode_sockaddr_in(bytes: &[u8]) -> Result<(Ipv4Addr, u16), VfsError> {
    if bytes.len() < 8 {
        return Err(VfsError::InvalidArgument);
    }
    let family = u16::from_le_bytes([bytes[0], bytes[1]]);
    if family != AF_INET as u16 {
        return Err(VfsError::AddressFamilyNotSupported);
    }
    let port = u16::from_be_bytes([bytes[2], bytes[3]]);
    let address = Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7]);
    Ok((address, port))
}

pub fn encode_sockaddr_in(address: Ipv4Addr, port: u16) -> [u8; SOCKADDR_IN_LEN] {
    let mut raw = [0u8; SOCKADDR_IN_LEN];
    raw[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    raw[2..4].copy_from_slice(&port.to_be_bytes());
    raw[4..8].copy_from_slice(&address.octets());
    raw
}

pub fn decode_sockaddr_in6(bytes: &[u8]) -> Result<(Ipv6Addr, u16, u32), VfsError> {
    if bytes.len() < SOCKADDR_IN6_LEN {
        return Err(VfsError::InvalidArgument);
    }
    let family = u16::from_le_bytes([bytes[0], bytes[1]]);
    if family != AF_INET6 as u16 {
        return Err(VfsError::AddressFamilyNotSupported);
    }
    let port = u16::from_be_bytes([bytes[2], bytes[3]]);
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&bytes[8..24]);
    let scope = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    Ok((Ipv6Addr::from(octets), port, scope))
}

pub fn encode_sockaddr_in6(address: Ipv6Addr, port: u16, scope: u32) -> [u8; SOCKADDR_IN6_LEN] {
    let mut raw = [0u8; SOCKADDR_IN6_LEN];
    raw[0..2].copy_from_slice(&(AF_INET6 as u16).to_le_bytes());
    raw[2..4].copy_from_slice(&port.to_be_bytes());
    raw[8..24].copy_from_slice(&address.octets());
    raw[24..28].copy_from_slice(&scope.to_le_bytes());
    raw
}

// ---- Control messages & shared IPv4 socket options ------------------------

use crate::net::{
    dev, fib,
    ip::IpAddr,
    ipv4::{
        header::{retopts_for_cmsg, validate_options},
        raw::RawSocket,
    },
    ipv6::raw::Raw6Socket,
    tcp::TcpSocket,
    udp::UdpSocket,
};

/// One ancillary data record (`struct cmsghdr` + payload) queued for `recvmsg`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmsgItem {
    pub level: i32,
    pub kind: i32,
    pub data: Vec<u8>,
}

/// Encode `items` into a 64-bit Linux `msg_control` buffer following
/// `put_cmsg` in `net/core/scm.c`. Returns `(bytes, msg_ctrunc)`.
pub fn encode_cmsgs(
    items: &[CmsgItem],
    initial_ctrunc: bool,
    has_control: bool,
    capacity: usize,
) -> (Vec<u8>, bool) {
    const CMSG_HDR_LEN: usize = 16;
    let mut out = Vec::new();
    let mut ctrunc = initial_ctrunc;
    if items.is_empty() {
        return (out, ctrunc);
    }
    if !has_control {
        return (out, true);
    }
    let mut remaining = capacity;
    for item in items {
        if remaining < CMSG_HDR_LEN {
            ctrunc = true;
            break;
        }
        let full_len = CMSG_HDR_LEN + item.data.len();
        let cmlen = if remaining < full_len {
            ctrunc = true;
            remaining
        } else {
            full_len
        };
        let copy_len = cmlen - CMSG_HDR_LEN;
        out.extend_from_slice(&(cmlen as u64).to_le_bytes());
        out.extend_from_slice(&item.level.to_le_bytes());
        out.extend_from_slice(&item.kind.to_le_bytes());
        out.extend_from_slice(&item.data[..copy_len]);
        let space = ((full_len + 7) & !7).min(remaining);
        out.resize(out.len() + (space - cmlen), 0);
        remaining -= space;
    }
    (out, ctrunc)
}

/// `SO_TIMESTAMP*` mode on a socket (`sock_recv_timestamp` in `include/net/sock.h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimestampOpt {
    #[default]
    Off,
    TimevalOld,
    TimevalNew,
    TimespecOld,
    TimespecNew,
}

impl TimestampOpt {
    pub fn is_enabled(self) -> bool {
        self != Self::Off
    }

    pub fn set(&mut self, optname: u64, enabled: bool) {
        if !enabled {
            let matches_current = matches!(
                (*self, optname),
                (Self::TimevalOld, SO_TIMESTAMP_OLD)
                    | (Self::TimevalNew, SO_TIMESTAMP_NEW)
                    | (Self::TimespecOld, SO_TIMESTAMPNS_OLD)
                    | (Self::TimespecNew, SO_TIMESTAMPNS_NEW)
            );
            if matches_current {
                *self = Self::Off;
            }
            return;
        }
        *self = match optname {
            SO_TIMESTAMP_OLD => Self::TimevalOld,
            SO_TIMESTAMP_NEW => Self::TimevalNew,
            SO_TIMESTAMPNS_OLD => Self::TimespecOld,
            SO_TIMESTAMPNS_NEW => Self::TimespecNew,
            _ => *self,
        };
    }

    pub fn get(self, optname: u64) -> u32 {
        u32::from(matches!(
            (self, optname),
            (Self::TimevalOld, SO_TIMESTAMP_OLD)
                | (Self::TimevalNew, SO_TIMESTAMP_NEW)
                | (Self::TimespecOld, SO_TIMESTAMPNS_OLD)
                | (Self::TimespecNew, SO_TIMESTAMPNS_NEW)
        ))
    }

    pub fn push_cmsg(self, timestamp_us: u64, out: &mut Vec<CmsgItem>) {
        if self == Self::Off {
            return;
        }
        let ts_us = if timestamp_us == 0 {
            crate::time::realtime_micros()
        } else {
            timestamp_us
        };
        let sec = (ts_us / 1_000_000) as i64;
        let sub = match self {
            Self::TimevalOld | Self::TimevalNew => (ts_us % 1_000_000) as i64,
            Self::TimespecOld | Self::TimespecNew => ((ts_us % 1_000_000) * 1000) as i64,
            Self::Off => return,
        };
        let kind = match self {
            Self::TimevalOld => SO_TIMESTAMP_OLD as i32,
            Self::TimevalNew => SO_TIMESTAMP_NEW as i32,
            Self::TimespecOld => SO_TIMESTAMPNS_OLD as i32,
            Self::TimespecNew => SO_TIMESTAMPNS_NEW as i32,
            Self::Off => return,
        };
        let mut payload = [0u8; 16];
        payload[0..8].copy_from_slice(&sec.to_le_bytes());
        payload[8..16].copy_from_slice(&sub.to_le_bytes());
        out.push(CmsgItem {
            level: SOL_SOCKET as i32,
            kind,
            data: payload.to_vec(),
        });
    }
}

/// Parse a `struct timeval` (16 bytes on 64-bit) into microseconds.
pub fn parse_timeval_us(value: &[u8]) -> Result<u64, VfsError> {
    if value.len() < 16 {
        return Err(VfsError::InvalidArgument);
    }
    let sec = i64::from_le_bytes(value[0..8].try_into().unwrap());
    let usec = i64::from_le_bytes(value[8..16].try_into().unwrap());
    if sec < 0 || !(0..1_000_000).contains(&usec) {
        return Err(VfsError::InvalidArgument);
    }
    Ok((sec as u64)
        .saturating_mul(1_000_000)
        .saturating_add(usec as u64))
}

pub fn encode_timeval_us(micros: u64) -> Vec<u8> {
    let mut out = [0u8; 16];
    let sec = (micros / 1_000_000) as i64;
    let usec = (micros % 1_000_000) as i64;
    out[0..8].copy_from_slice(&sec.to_le_bytes());
    out[8..16].copy_from_slice(&usec.to_le_bytes());
    out.to_vec()
}

/// Decode an integer or 1-byte boolean/small-int option (`do_ip_setsockopt`).
pub fn opt_int_or_byte(value: &[u8]) -> i32 {
    if value.len() >= 4 {
        i32::from_le_bytes(value[0..4].try_into().unwrap())
    } else if !value.is_empty() {
        i32::from(value[0])
    } else {
        0
    }
}

/// Incoming IPv4 packet metadata retained on queued datagrams so `recvmsg`
/// can construct `SOL_IP` control messages (`ip_cmsg_recv_offset`).
#[derive(Clone, Debug)]
pub struct IpRxMeta {
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub spec_dst: Ipv4Addr,
    pub ifindex: u32,
    pub ttl: u8,
    pub tos: u8,
    pub ip_options: Vec<u8>,
    pub timestamp_us: u64,
}

const CMSG_FLAG_PKTINFO: u32 = 1 << 0;
const CMSG_FLAG_TTL: u32 = 1 << 1;
const CMSG_FLAG_TOS: u32 = 1 << 2;
const CMSG_FLAG_RECVOPTS: u32 = 1 << 3;
const CMSG_FLAG_RETOPTS: u32 = 1 << 4;
const CMSG_FLAG_PASSSEC: u32 = 1 << 5;
const CMSG_FLAG_ORIGDSTADDR: u32 = 1 << 6;
const CMSG_FLAG_CHECKSUM: u32 = 1 << 7;
const CMSG_FLAG_RECVFRAGSIZE: u32 = 1 << 8;

/// Per-socket IPv4 state mirroring `struct inet_sock` (`net/ipv4/ip_sockglue.c`).
#[derive(Clone, Debug)]
pub struct Ip4SockOpts {
    pub cmsg_flags: u32,
    pub tos: u8,
    pub uc_ttl: i16,
    pub min_ttl: u8,
    pub mc_ttl: u8,
    pub mc_loop: bool,
    pub mc_all: bool,
    pub mc_addr: Ipv4Addr,
    pub mc_index: u32,
    pub uc_index: u32,
    pub pmtudisc: u8,
    pub recverr: bool,
    pub recverr_rfc4884: bool,
    pub freebind: bool,
    pub hdrincl: bool,
    pub nodefrag: bool,
    pub bind_address_no_port: bool,
    pub router_alert: bool,
    pub transparent: bool,
    pub local_port_range: u32,
    pub ip_options: Vec<u8>,
}

impl Default for Ip4SockOpts {
    fn default() -> Self {
        Self {
            cmsg_flags: 0,
            tos: 0,
            uc_ttl: -1,
            min_ttl: 0,
            mc_ttl: 1,
            mc_loop: true,
            mc_all: true,
            mc_addr: Ipv4Addr::UNSPECIFIED,
            mc_index: 0,
            uc_index: 0,
            pmtudisc: IP_PMTUDISC_WANT as u8,
            recverr: false,
            recverr_rfc4884: false,
            freebind: false,
            hdrincl: false,
            nodefrag: false,
            bind_address_no_port: false,
            router_alert: false,
            transparent: false,
            local_port_range: 0,
            ip_options: Vec::new(),
        }
    }
}

impl Ip4SockOpts {
    pub fn effective_ttl(&self) -> u8 {
        if self.uc_ttl < 0 {
            64
        } else {
            self.uc_ttl as u8
        }
    }

    pub fn dont_fragment(&self) -> bool {
        matches!(self.pmtudisc, IP_PMTUDISC_DO | IP_PMTUDISC_PROBE)
    }

    pub fn wants_cmsg(&self) -> bool {
        self.cmsg_flags != 0
    }

    fn assign_cmsg(&mut self, bit: u32, enabled: bool) {
        if enabled {
            self.cmsg_flags |= bit;
        } else {
            self.cmsg_flags &= !bit;
        }
    }

    /// `do_ip_setsockopt` (`net/ipv4/ip_sockglue.c`).
    pub fn set(&mut self, sock_type: u32, optname: u64, value: &[u8]) -> Result<(), VfsError> {
        let val = opt_int_or_byte(value);
        match optname {
            IP_PKTINFO => {
                self.assign_cmsg(CMSG_FLAG_PKTINFO, val != 0);
                Ok(())
            }
            IP_RECVTTL => {
                self.assign_cmsg(CMSG_FLAG_TTL, val != 0);
                Ok(())
            }
            IP_RECVTOS => {
                self.assign_cmsg(CMSG_FLAG_TOS, val != 0);
                Ok(())
            }
            IP_RECVOPTS => {
                self.assign_cmsg(CMSG_FLAG_RECVOPTS, val != 0);
                Ok(())
            }
            IP_RETOPTS => {
                self.assign_cmsg(CMSG_FLAG_RETOPTS, val != 0);
                Ok(())
            }
            IP_PASSSEC => {
                self.assign_cmsg(CMSG_FLAG_PASSSEC, val != 0);
                Ok(())
            }
            IP_ORIGDSTADDR => {
                self.assign_cmsg(CMSG_FLAG_ORIGDSTADDR, val != 0);
                Ok(())
            }
            IP_CHECKSUM => {
                self.assign_cmsg(CMSG_FLAG_CHECKSUM, val != 0);
                Ok(())
            }
            IP_RECVFRAGSIZE => {
                if sock_type != SOCK_RAW && sock_type != SOCK_DGRAM {
                    return Err(VfsError::InvalidArgument);
                }
                self.assign_cmsg(CMSG_FLAG_RECVFRAGSIZE, val != 0);
                Ok(())
            }
            IP_RECVERR => {
                self.recverr = val != 0;
                Ok(())
            }
            IP_RECVERR_RFC4884 => {
                if !(0..=1).contains(&val) {
                    return Err(VfsError::InvalidArgument);
                }
                self.recverr_rfc4884 = val != 0;
                Ok(())
            }
            IP_ROUTER_ALERT => {
                self.router_alert = val != 0;
                Ok(())
            }
            IP_FREEBIND => {
                if value.is_empty() {
                    return Err(VfsError::InvalidArgument);
                }
                self.freebind = val != 0;
                Ok(())
            }
            IP_TRANSPARENT => {
                if value.is_empty() {
                    return Err(VfsError::InvalidArgument);
                }
                self.transparent = val != 0;
                Ok(())
            }
            IP_BIND_ADDRESS_NO_PORT => {
                self.bind_address_no_port = val != 0;
                Ok(())
            }
            IP_HDRINCL => {
                if sock_type != SOCK_RAW {
                    return Err(VfsError::NoProtocolOption);
                }
                self.hdrincl = val != 0;
                Ok(())
            }
            IP_NODEFRAG => {
                if sock_type != SOCK_RAW {
                    return Err(VfsError::NoProtocolOption);
                }
                self.nodefrag = val != 0;
                Ok(())
            }
            IP_TOS => {
                if sock_type == SOCK_STREAM {
                    self.tos = (self.tos & 3) | ((val as u8) & !3);
                } else {
                    self.tos = val as u8;
                }
                Ok(())
            }
            IP_TTL => {
                if value.is_empty() || (val != -1 && !(1..=255).contains(&val)) {
                    return Err(VfsError::InvalidArgument);
                }
                self.uc_ttl = val as i16;
                Ok(())
            }
            IP_MINTTL => {
                if value.is_empty() || !(0..=255).contains(&val) {
                    return Err(VfsError::InvalidArgument);
                }
                self.min_ttl = val as u8;
                Ok(())
            }
            IP_MULTICAST_TTL => {
                if sock_type == SOCK_STREAM || value.is_empty() {
                    return Err(VfsError::InvalidArgument);
                }
                let eff = if val == -1 { 1 } else { val };
                if !(0..=255).contains(&eff) {
                    return Err(VfsError::InvalidArgument);
                }
                self.mc_ttl = eff as u8;
                Ok(())
            }
            IP_MULTICAST_LOOP => {
                if value.is_empty() {
                    return Err(VfsError::InvalidArgument);
                }
                self.mc_loop = val != 0;
                Ok(())
            }
            IP_MULTICAST_ALL => {
                if value.is_empty() || (val != 0 && val != 1) {
                    return Err(VfsError::InvalidArgument);
                }
                self.mc_all = val != 0;
                Ok(())
            }
            IP_MTU_DISCOVER => {
                if value.is_empty() || !(0..=5).contains(&val) {
                    return Err(VfsError::InvalidArgument);
                }
                self.pmtudisc = val as u8;
                Ok(())
            }
            IP_OPTIONS => {
                self.ip_options = validate_options(value)?;
                Ok(())
            }
            IP_MULTICAST_IF => {
                if sock_type == SOCK_STREAM || value.len() < 4 {
                    return Err(VfsError::InvalidArgument);
                }
                if value.len() >= 12 {
                    // struct ip_mreqn: imr_multiaddr(4), imr_address(4), imr_ifindex(4)
                    let addr = Ipv4Addr::new(value[4], value[5], value[6], value[7]);
                    let ifindex = i32::from_le_bytes(value[8..12].try_into().unwrap());
                    if ifindex < 0 {
                        return Err(VfsError::InvalidArgument);
                    }
                    if ifindex != 0 && dev::by_index(ifindex as u32).is_none() {
                        return Err(VfsError::NoDevice);
                    }
                    self.mc_addr = addr;
                    self.mc_index = ifindex as u32;
                } else if value.len() >= 8 {
                    // struct ip_mreq or struct sockaddr_in
                    let family = u16::from_le_bytes([value[0], value[1]]);
                    if family == AF_INET as u16 {
                        self.mc_addr = Ipv4Addr::new(value[4], value[5], value[6], value[7]);
                    } else {
                        self.mc_addr = Ipv4Addr::new(value[4], value[5], value[6], value[7]);
                    }
                } else {
                    self.mc_addr = Ipv4Addr::new(value[0], value[1], value[2], value[3]);
                }
                Ok(())
            }
            IP_UNICAST_IF => {
                if value.len() != 4 {
                    return Err(VfsError::InvalidArgument);
                }
                let ifindex = u32::from_be_bytes(value[0..4].try_into().unwrap());
                if ifindex != 0 && dev::by_index(ifindex).is_none() {
                    return Err(VfsError::AddressNotAvailable);
                }
                self.uc_index = ifindex;
                Ok(())
            }
            IP_ADD_MEMBERSHIP
            | IP_DROP_MEMBERSHIP
            | IP_UNBLOCK_SOURCE
            | IP_BLOCK_SOURCE
            | IP_ADD_SOURCE_MEMBERSHIP
            | IP_DROP_SOURCE_MEMBERSHIP
            | MCAST_JOIN_GROUP
            | MCAST_LEAVE_GROUP => {
                if value.len() < 8 {
                    return Err(VfsError::InvalidArgument);
                }
                Ok(())
            }
            IP_LOCAL_PORT_RANGE => {
                if value.len() != 4 {
                    return Err(VfsError::InvalidArgument);
                }
                let raw = val as u32;
                let lo = raw as u16;
                let hi = (raw >> 16) as u16;
                if lo != 0 && hi != 0 && lo > hi {
                    return Err(VfsError::InvalidArgument);
                }
                self.local_port_range = raw;
                Ok(())
            }
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    /// `do_ip_getsockopt` (`net/ipv4/ip_sockglue.c`).
    pub fn get(
        &self,
        sock_type: u32,
        protocol: u8,
        peer: Option<Ipv4Addr>,
        optname: u64,
    ) -> Result<Vec<u8>, VfsError> {
        let int = |v: i32| Ok(v.to_le_bytes().to_vec());
        let flag = |bit: u32| int(i32::from(self.cmsg_flags & bit != 0));
        match optname {
            IP_PKTINFO => flag(CMSG_FLAG_PKTINFO),
            IP_RECVTTL => flag(CMSG_FLAG_TTL),
            IP_RECVTOS => flag(CMSG_FLAG_TOS),
            IP_RECVOPTS => flag(CMSG_FLAG_RECVOPTS),
            IP_RETOPTS => flag(CMSG_FLAG_RETOPTS),
            IP_PASSSEC => flag(CMSG_FLAG_PASSSEC),
            IP_ORIGDSTADDR => flag(CMSG_FLAG_ORIGDSTADDR),
            IP_CHECKSUM => flag(CMSG_FLAG_CHECKSUM),
            IP_RECVFRAGSIZE => flag(CMSG_FLAG_RECVFRAGSIZE),
            IP_RECVERR => int(i32::from(self.recverr)),
            IP_RECVERR_RFC4884 => int(i32::from(self.recverr_rfc4884)),
            IP_ROUTER_ALERT => int(i32::from(self.router_alert)),
            IP_FREEBIND => int(i32::from(self.freebind)),
            IP_TRANSPARENT => int(i32::from(self.transparent)),
            IP_BIND_ADDRESS_NO_PORT => int(i32::from(self.bind_address_no_port)),
            IP_HDRINCL => {
                if sock_type != SOCK_RAW {
                    return Err(VfsError::NoProtocolOption);
                }
                int(i32::from(self.hdrincl))
            }
            IP_NODEFRAG => {
                if sock_type != SOCK_RAW {
                    return Err(VfsError::NoProtocolOption);
                }
                int(i32::from(self.nodefrag))
            }
            IP_TOS => int(i32::from(self.tos)),
            IP_TTL => int(i32::from(self.effective_ttl())),
            IP_MINTTL => int(i32::from(self.min_ttl)),
            IP_MULTICAST_TTL => int(i32::from(self.mc_ttl)),
            IP_MULTICAST_LOOP => int(i32::from(self.mc_loop)),
            IP_MULTICAST_ALL => int(i32::from(self.mc_all)),
            IP_MTU_DISCOVER => int(i32::from(self.pmtudisc)),
            IP_OPTIONS => Ok(self.ip_options.clone()),
            IP_MTU => {
                let dst = peer
                    .filter(|a| !a.is_unspecified())
                    .ok_or(VfsError::NotConnected)?;
                let route = fib::lookup(IpAddr::V4(dst)).ok_or(VfsError::NotConnected)?;
                let dev = dev::by_index(route.ifindex).ok_or(VfsError::NotConnected)?;
                int(dev.mtu() as i32)
            }
            IP_MULTICAST_IF => Ok(self.mc_addr.octets().to_vec()),
            IP_UNICAST_IF => Ok(self.uc_index.to_be_bytes().to_vec()),
            IP_LOCAL_PORT_RANGE => int(self.local_port_range as i32),
            IP_PROTOCOL => int(i32::from(protocol)),
            _ => Err(VfsError::NoProtocolOption),
        }
    }

    /// Emit `SOL_IP` ancillary items in `ip_cmsg_recv_offset` order. Returns
    /// `true` if `MSG_CTRUNC` must be raised (malformed `IP_RETOPTS`).
    pub fn collect_cmsgs(
        &self,
        meta: &IpRxMeta,
        orig_dst_port: u16,
        out: &mut Vec<CmsgItem>,
    ) -> bool {
        let flags = self.cmsg_flags;
        if flags == 0 {
            return false;
        }
        let mut ctrunc = false;
        if flags & CMSG_FLAG_PKTINFO != 0 {
            // struct in_pktinfo { int ipi_ifindex; struct in_addr ipi_spec_dst; struct in_addr ipi_addr; }
            let mut info = [0u8; 12];
            info[0..4].copy_from_slice(&(meta.ifindex as i32).to_le_bytes());
            info[4..8].copy_from_slice(&meta.spec_dst.octets());
            info[8..12].copy_from_slice(&meta.destination.octets());
            out.push(CmsgItem {
                level: SOL_IP as i32,
                kind: IP_PKTINFO as i32,
                data: info.to_vec(),
            });
        }
        if flags & CMSG_FLAG_TTL != 0 {
            out.push(CmsgItem {
                level: SOL_IP as i32,
                kind: IP_TTL as i32,
                data: i32::from(meta.ttl).to_le_bytes().to_vec(),
            });
        }
        if flags & CMSG_FLAG_TOS != 0 {
            out.push(CmsgItem {
                level: SOL_IP as i32,
                kind: IP_TOS as i32,
                data: alloc::vec![meta.tos],
            });
        }
        if flags & CMSG_FLAG_RECVOPTS != 0 && !meta.ip_options.is_empty() {
            out.push(CmsgItem {
                level: SOL_IP as i32,
                kind: IP_RECVOPTS as i32,
                data: meta.ip_options.clone(),
            });
        }
        if flags & CMSG_FLAG_RETOPTS != 0 && !meta.ip_options.is_empty() {
            match retopts_for_cmsg(&meta.ip_options, meta.source) {
                Ok(retopts) => {
                    out.push(CmsgItem {
                        level: SOL_IP as i32,
                        kind: IP_RETOPTS as i32,
                        data: retopts,
                    });
                }
                Err(()) => {
                    ctrunc = true;
                }
            }
        }
        if flags & CMSG_FLAG_ORIGDSTADDR != 0 && orig_dst_port != 0 {
            let sin = encode_sockaddr_in(meta.destination, orig_dst_port);
            out.push(CmsgItem {
                level: SOL_IP as i32,
                kind: IP_ORIGDSTADDR as i32,
                data: sin.to_vec(),
            });
        }
        ctrunc
    }
}

// ---- AF_INET socket dispatch -------------------------------------------------

/// One datagram or stream read result in the form the syscall layer writes out.
pub struct InetReceived {
    pub data: Vec<u8>,
    pub source: Option<Vec<u8>>,
    pub cmsgs: Vec<CmsgItem>,
    pub control_truncated: bool,
    pub truncated: bool,
    pub full_len: usize,
    pub eof: bool,
}

/// Uniform view over the concrete AF_INET/AF_INET6 socket types. The syscall
/// layer resolves an fd to one of these before touching family-specific state.
pub enum InetSocket {
    Tcp(Arc<TcpSocket>),
    Udp(Arc<UdpSocket>),
    Raw(Arc<RawSocket>),
    Raw6(Arc<Raw6Socket>),
}

impl InetSocket {
    pub fn instance(fd: u64) -> Option<InetSocket> {
        if let Some(socket) = inode_of::<TcpSocket>(fd) {
            return Some(InetSocket::Tcp(socket));
        }
        if let Some(socket) = inode_of::<UdpSocket>(fd) {
            return Some(InetSocket::Udp(socket));
        }
        if let Some(socket) = inode_of::<RawSocket>(fd) {
            return Some(InetSocket::Raw(socket));
        }
        inode_of::<Raw6Socket>(fd).map(InetSocket::Raw6)
    }

    pub fn kind(&self) -> u32 {
        match self {
            InetSocket::Tcp(socket) => socket.kind(),
            InetSocket::Udp(socket) => socket.kind(),
            InetSocket::Raw(socket) => socket.kind(),
            InetSocket::Raw6(socket) => socket.kind(),
        }
    }

    pub fn domain(&self) -> u64 {
        match self {
            InetSocket::Raw6(_) => AF_INET6,
            _ => AF_INET,
        }
    }

    pub fn protocol(&self) -> u32 {
        match self {
            InetSocket::Tcp(socket) => socket.protocol() as u32,
            InetSocket::Udp(socket) => socket.protocol() as u32,
            InetSocket::Raw(socket) => socket.protocol() as u32,
            InetSocket::Raw6(socket) => socket.protocol() as u32,
        }
    }

    pub fn bind(&self, address: &[u8]) -> Result<(), VfsError> {
        match self {
            InetSocket::Tcp(socket) => socket.bind(address),
            InetSocket::Udp(socket) => socket.bind(address),
            InetSocket::Raw(socket) => socket.bind(address),
            InetSocket::Raw6(socket) => socket.bind(address),
        }
    }

    pub fn listen(&self, backlog: u32) -> Result<(), VfsError> {
        match self {
            InetSocket::Tcp(socket) => socket.listen(backlog),
            _ => Err(VfsError::MessageNotSupported),
        }
    }

    pub fn accept(&self, nonblock: bool) -> Result<(Arc<dyn Inode>, Option<Vec<u8>>), VfsError> {
        match self {
            InetSocket::Tcp(socket) => {
                let child = socket.accept(nonblock)?;
                let peer = child.peer_address().map(|raw| raw.to_vec());
                Ok((child, peer))
            }
            _ => Err(VfsError::MessageNotSupported),
        }
    }

    pub fn connect(&self, address: &[u8], nonblock: bool) -> Result<(), VfsError> {
        match self {
            InetSocket::Tcp(socket) => socket.connect(address, nonblock),
            InetSocket::Udp(socket) => socket.connect(address),
            InetSocket::Raw(socket) => socket.connect(address),
            InetSocket::Raw6(socket) => socket.connect(address),
        }
    }

    pub fn send(
        &self,
        data: &[u8],
        destination: Option<&[u8]>,
        flags: u64,
    ) -> Result<usize, VfsError> {
        match self {
            InetSocket::Tcp(socket) => socket.send(data, flags),
            InetSocket::Udp(socket) => socket.send_to(data, destination, flags),
            InetSocket::Raw(socket) => socket.send_to(data, destination, flags & 0x40 != 0),
            InetSocket::Raw6(socket) => socket.send_to(data, destination, flags & 0x40 != 0),
        }
    }

    pub fn receive(&self, max: usize, flags: u64) -> Result<InetReceived, VfsError> {
        match self {
            InetSocket::Tcp(socket) => {
                let (data, eof) = socket.receive(max, flags)?;
                Ok(InetReceived {
                    data,
                    source: socket.peer_address().map(|raw| raw.to_vec()),
                    cmsgs: Vec::new(),
                    control_truncated: false,
                    truncated: false,
                    full_len: 0,
                    eof,
                })
            }
            InetSocket::Udp(socket) => socket.receive_msg(max, flags),
            InetSocket::Raw(socket) => socket.receive_msg(max, flags),
            InetSocket::Raw6(socket) => socket.receive_msg(max, flags),
        }
    }

    pub fn local_address(&self) -> Vec<u8> {
        match self {
            InetSocket::Tcp(socket) => socket.local_address().to_vec(),
            InetSocket::Udp(socket) => socket.local_address().to_vec(),
            InetSocket::Raw(socket) => socket.local_address().to_vec(),
            InetSocket::Raw6(socket) => socket.local_address().to_vec(),
        }
    }

    pub fn peer_address(&self) -> Option<Vec<u8>> {
        match self {
            InetSocket::Tcp(socket) => socket.peer_address().map(|raw| raw.to_vec()),
            InetSocket::Udp(socket) => socket.peer_address().map(|raw| raw.to_vec()),
            InetSocket::Raw(socket) => socket.peer_address().map(|raw| raw.to_vec()),
            InetSocket::Raw6(socket) => socket.peer_address().map(|raw| raw.to_vec()),
        }
    }

    pub fn shutdown(&self, how: u32) -> Result<(), VfsError> {
        match self {
            InetSocket::Tcp(socket) => socket.shutdown(how),
            InetSocket::Udp(socket) => socket.shutdown(how),
            InetSocket::Raw(_) | InetSocket::Raw6(_) => Err(VfsError::MessageNotSupported),
        }
    }

    pub fn take_error(&self) -> i32 {
        match self {
            InetSocket::Tcp(socket) => socket.take_error(),
            InetSocket::Udp(socket) => socket.take_error(),
            InetSocket::Raw(socket) => socket.error().unwrap_or(0),
            InetSocket::Raw6(socket) => socket.error().unwrap_or(0),
        }
    }

    pub fn is_listening(&self) -> bool {
        match self {
            InetSocket::Tcp(socket) => socket.is_listening(),
            _ => false,
        }
    }

    pub fn setsockopt(&self, level: u64, name: u64, value: &[u8]) -> Result<(), VfsError> {
        match self {
            InetSocket::Tcp(socket) => socket.setsockopt(level, name, value),
            InetSocket::Udp(socket) => socket.setsockopt(level, name, value),
            InetSocket::Raw(socket) => socket.setsockopt(level, name, value),
            InetSocket::Raw6(socket) => socket.setsockopt(level, name, value),
        }
    }

    pub fn getsockopt(&self, level: u64, name: u64) -> Result<Vec<u8>, VfsError> {
        match self {
            InetSocket::Tcp(socket) => socket.getsockopt(level, name),
            InetSocket::Udp(socket) => socket.getsockopt(level, name),
            InetSocket::Raw(socket) => socket.getsockopt(level, name),
            InetSocket::Raw6(socket) => socket.getsockopt(level, name),
        }
    }
}
