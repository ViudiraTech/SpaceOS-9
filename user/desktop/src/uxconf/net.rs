/*
 *
 *       user/desktop/src/uxconf/net.rs
 *       NetworkManager core: NETLINK_ROUTE, RFC 2131 DHCPv4 T1/T2 engine, keyfiles & IPC
 *
 *       2026/11/06 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mature user-space NetworkManager core shared by `/usr/bin/NetworkManager`,
//! `/usr/bin/nmcli`, and the SPUI 4.0 shell & System Settings UI.
//!
//! Architecture (modeled after FreeDesktop `NetworkManager` and `systemd-networkd`):
//! - **Per-device state machine** ([`DeviceState`]) matching `NMDeviceState` numeric codes
//!   (`10 unmanaged` .. `100 activated` .. `120 failed`), with a 1500 ms carrier-loss
//!   debounce grace period (`IgnoreCarrierLoss`).
//! - **Non-blocking RFC 2131 DHCPv4 client** ([`DhcpEngine`]) driven inside the daemon's
//!   single `poll(2)` loop: `Selecting` (`DHCPDISCOVER`) -> `Requesting` (`DHCPREQUEST`) ->
//!   `Bound` -> `Renewing` (unicast at `T1`) -> `Rebinding` (broadcast at `T2`), plus
//!   `DHCPRELEASE` on disconnect and automatic QEMU SLIRP fallback (`10.0.2.15/24`).
//! - **Persistent keyfile profiles** under `/etc/NetworkManager/system-connections/`
//!   synchronized bidirectionally with the `uxconfd` `"network"` channel.
//! - **Control plane IPC** over `AF_UNIX` `/run/NetworkManager/nm.sock` (`0666`) plus
//!   atomic `/run/NetworkManager/resolv.conf`, `/etc/resolv.conf`, and `/run/netmgr.status`.

use crate::client::{UxConfClient, UxConfWatch};
use crate::{DesktopConfig, NetMode, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::mem;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const NM_RUN_DIR: &str = "/run/NetworkManager";
pub const NM_SOCKET_PATH: &str = "/run/NetworkManager/nm.sock";
pub const NM_RESOLV_CONF: &str = "/run/NetworkManager/resolv.conf";
pub const NM_PROFILES_DIR: &str = "/etc/NetworkManager/system-connections";
pub const STATUS_FILE: &str = "/run/netmgr.status";
pub const ETC_RESOLV_CONF: &str = "/etc/resolv.conf";

const CARRIER_GRACE_MS: u64 = 1500;

const AF_UNSPEC: u8 = 0;
const AF_INET_U8: u8 = 2;
const AF_INET6_U8: u8 = 10;

const NETLINK_ROUTE: libc::c_int = 0;

const NLM_F_REQUEST: u16 = 0x0001;
const NLM_F_MULTI: u16 = 0x0002;
const NLM_F_ACK: u16 = 0x0004;
const NLM_F_DUMP: u16 = 0x0300;
const NLM_F_REPLACE: u16 = 0x0100;
const NLM_F_CREATE: u16 = 0x0400;

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;

const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_SETLINK: u16 = 19;
const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_GETADDR: u16 = 22;
const RTM_NEWROUTE: u16 = 24;
const RTM_DELROUTE: u16 = 25;
const RTM_GETROUTE: u16 = 26;

const RTNLGRP_LINK_MASK: u32 = 1 << 0;
const RTNLGRP_NEIGH_MASK: u32 = 1 << 2;
const RTNLGRP_IPV4_IFADDR_MASK: u32 = 1 << 4;
const RTNLGRP_IPV4_ROUTE_MASK: u32 = 1 << 6;
const RTNLGRP_IPV6_IFADDR_MASK: u32 = 1 << 8;
const RTNLGRP_IPV6_ROUTE_MASK: u32 = 1 << 10;

const IFF_UP: u32 = 0x1;
const IFF_LOOPBACK: u32 = 0x8;
const IFF_RUNNING: u32 = 0x40;

const IFLA_ADDRESS: u16 = 1;
const IFLA_IFNAME: u16 = 3;
const IFLA_MTU: u16 = 4;
const IFLA_STATS: u16 = 7;
const IFLA_STATS64: u16 = 23;
const IFLA_CARRIER: u16 = 33;

const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;

const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PREFSRC: u16 = 7;

const RT_TABLE_MAIN: u8 = 254;
const RTPROT_BOOT: u8 = 3;
const RTPROT_STATIC: u8 = 4;
const RTPROT_DHCP: u8 = 16;
const RT_SCOPE_UNIVERSE: u8 = 0;
const RTN_UNICAST: u8 = 1;

const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;
const DHCP_NAK: u8 = 6;
const DHCP_RELEASE: u8 = 7;

/// FreeDesktop `NMDeviceState` numeric state codes and human-readable labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceState {
    #[default]
    Unmanaged = 10,
    Unavailable = 20,
    Disconnected = 30,
    Prepare = 40,
    Config = 50,
    IpConfig = 70,
    Activated = 100,
    Deactivating = 110,
    Failed = 120,
}

impl DeviceState {
    pub const fn code(self) -> u32 {
        self as u32
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unmanaged => "unmanaged",
            Self::Unavailable => "unavailable",
            Self::Disconnected => "disconnected",
            Self::Prepare => "prepare",
            Self::Config => "config",
            Self::IpConfig => "ip-config",
            Self::Activated => "activated",
            Self::Deactivating => "deactivating",
            Self::Failed => "failed",
        }
    }

    pub const fn label_zh(self) -> &'static str {
        match self {
            Self::Unmanaged => "未托管",
            Self::Unavailable => "不可用",
            Self::Disconnected => "已断开",
            Self::Prepare => "准备中",
            Self::Config => "配置中",
            Self::IpConfig => "获取地址中",
            Self::Activated => "已连接",
            Self::Deactivating => "正在断开",
            Self::Failed => "连接失败",
        }
    }

    pub fn from_code_or_str(raw: &str) -> Self {
        let trimmed = raw.trim();
        let token = trimmed.split_whitespace().next().unwrap_or(trimmed);
        match token {
            "10" | "unmanaged" => Self::Unmanaged,
            "20" | "unavailable" => Self::Unavailable,
            "30" | "disconnected" => Self::Disconnected,
            "40" | "prepare" => Self::Prepare,
            "50" | "config" => Self::Config,
            "70" | "ip-config" => Self::IpConfig,
            "100" | "activated" => Self::Activated,
            "110" | "deactivating" => Self::Deactivating,
            "120" | "failed" => Self::Failed,
            _ => Self::Unavailable,
        }
    }
}

#[repr(C)]
struct SockAddrNl {
    nl_family: u16,
    nl_pad: u16,
    nl_pid: u32,
    nl_groups: u32,
}

/// Live interface, NetworkManager state, DHCPv4 lease, DNS and counter snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetStatus {
    pub daemon_running: bool,
    pub device_state: DeviceState,
    pub profile_name: String,
    pub has_device: bool,
    pub ifindex: u32,
    pub ifname: String,
    pub mac: String,
    pub up: bool,
    pub carrier: bool,
    pub mtu: u32,
    pub ipv4: Option<(Ipv4Addr, u8)>,
    pub ipv4_all: Vec<(Ipv4Addr, u8)>,
    pub ipv6_ll: Option<(Ipv6Addr, u8)>,
    pub gateway: Option<Ipv4Addr>,
    pub dns: Option<Ipv4Addr>,
    pub dhcp_state: String,
    pub lease_secs: u32,
    pub lease_remaining_secs: u32,
    pub t1_secs: u32,
    pub t2_secs: u32,
    pub server_id: Option<Ipv4Addr>,
    pub mode_note: String,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

impl NetStatus {
    pub fn connected(&self) -> bool {
        self.has_device && self.up && self.carrier && self.ipv4.is_some()
    }

    pub fn panel_label(&self) -> String {
        if let Some((ip, _)) = self.ipv4 {
            if self.up && self.carrier {
                return ip.to_string();
            }
        }
        if !self.has_device {
            "无网卡".to_string()
        } else if !self.up {
            "已禁用".to_string()
        } else if !self.carrier {
            "无载波".to_string()
        } else if matches!(
            self.device_state,
            DeviceState::Prepare | DeviceState::Config | DeviceState::IpConfig
        ) {
            "正在连接".to_string()
        } else {
            "未连接".to_string()
        }
    }

    pub fn format_bytes(bytes: u64) -> String {
        const KIB: f64 = 1024.0;
        const MIB: f64 = 1024.0 * 1024.0;
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        if bytes >= 1024 * 1024 * 1024 {
            format!("{:.2} GiB", bytes as f64 / GIB)
        } else if bytes >= 1024 * 1024 {
            format!("{:.1} MiB", bytes as f64 / MIB)
        } else if bytes >= 1024 {
            format!("{:.1} KiB", bytes as f64 / KIB)
        } else {
            format!("{bytes} B")
        }
    }
}

/// Persistent INI keyfile profile stored in `/etc/NetworkManager/system-connections/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub id: String,
    pub uuid: String,
    pub ifname: String,
    pub autoconnect: bool,
    pub mode: NetMode,
    pub address: String,
    pub prefixlen: u8,
    pub gateway: String,
    pub dns: String,
}

impl ConnectionProfile {
    pub fn from_desktop_config(ifname: &str, cfg: &DesktopConfig) -> Self {
        let safe_if = if ifname.is_empty() { "eth0" } else { ifname };
        Self {
            id: format!("Wired connection ({safe_if})"),
            uuid: deterministic_uuid(safe_if),
            ifname: safe_if.to_string(),
            autoconnect: cfg.net_enabled,
            mode: cfg.net_mode,
            address: cfg.net_address.clone(),
            prefixlen: u8::try_from(cfg.net_prefixlen.clamp(1, 32)).unwrap_or(24),
            gateway: cfg.net_gateway.clone(),
            dns: cfg.net_dns.clone(),
        }
    }

    pub fn encode_keyfile(&self) -> String {
        let method = match self.mode {
            NetMode::Dhcp => "auto",
            NetMode::Static => "manual",
        };
        let mut out = String::new();
        out.push_str("[connection]\n");
        out.push_str(&format!("id={}\n", self.id));
        out.push_str(&format!("uuid={}\n", self.uuid));
        out.push_str("type=802-3-ethernet\n");
        out.push_str(&format!("interface-name={}\n", self.ifname));
        out.push_str(&format!("autoconnect={}\n\n", self.autoconnect));

        out.push_str("[ipv4]\n");
        out.push_str(&format!("method={method}\n"));
        if self.mode == NetMode::Static {
            if self.gateway.trim().is_empty() {
                out.push_str(&format!(
                    "address1={}/{}\n",
                    self.address.trim(),
                    self.prefixlen
                ));
            } else {
                out.push_str(&format!(
                    "address1={}/{},{}\n",
                    self.address.trim(),
                    self.prefixlen,
                    self.gateway.trim()
                ));
            }
            if !self.dns.trim().is_empty() {
                out.push_str(&format!("dns={};\n", self.dns.trim()));
            }
        }
        out.push_str("\n[ipv6]\nmethod=link-local\n");
        out
    }

    pub fn parse_keyfile(raw: &str, default_ifname: &str) -> Self {
        let mut prof = Self::from_desktop_config(default_ifname, &DesktopConfig::default());
        let mut section = "";
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if let Some(sec) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = sec.trim();
                continue;
            }
            let Some((k, v)) = trimmed.split_once('=') else {
                continue;
            };
            let key = k.trim();
            let val = v.trim();
            match (section, key) {
                ("connection", "id") if !val.is_empty() => prof.id = val.to_string(),
                ("connection", "uuid") if !val.is_empty() => prof.uuid = val.to_string(),
                ("connection", "interface-name") if !val.is_empty() => {
                    prof.ifname = val.to_string();
                }
                ("connection", "autoconnect") => {
                    prof.autoconnect = matches!(val, "true" | "1" | "yes");
                }
                ("ipv4", "method") => {
                    prof.mode = if val.eq_ignore_ascii_case("manual") {
                        NetMode::Static
                    } else {
                        NetMode::Dhcp
                    };
                }
                ("ipv4", "address1") => {
                    let mut parts = val.split(',');
                    if let Some(cidr) = parts.next() {
                        if let Some((ip_part, plen_part)) = cidr.split_once('/') {
                            prof.address = ip_part.trim().to_string();
                            if let Ok(p) = plen_part.trim().parse::<u8>() {
                                prof.prefixlen = p.clamp(1, 32);
                            }
                        } else {
                            prof.address = cidr.trim().to_string();
                        }
                    }
                    if let Some(gw) = parts.next() {
                        prof.gateway = gw.trim().to_string();
                    }
                }
                ("ipv4", "dns") => {
                    let first = val
                        .trim_end_matches(';')
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim();
                    prof.dns = first.to_string();
                }
                _ => {}
            }
        }
        prof
    }
}

fn deterministic_uuid(seed: &str) -> String {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    for &b in seed.as_bytes() {
        h1 ^= u64::from(b);
        h1 = h1.wrapping_mul(0x0100_0000_01b3);
        h2 ^= u64::from(b.rotate_left(3));
        h2 = h2.wrapping_mul(0x0100_0000_01b3);
    }
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (h1 >> 32) as u32,
        ((h1 >> 16) & 0xffff) as u16,
        (h1 & 0x0fff) as u16,
        ((h2 >> 48) & 0x0fff) as u16,
        h2 & 0xffff_ffff_ffff
    )
}

#[derive(Clone, Debug)]
struct LinkInfo {
    ifindex: u32,
    name: String,
    flags: u32,
    mtu: u32,
    mac: [u8; 6],
    has_mac: bool,
    carrier: bool,
    rx_packets: u64,
    tx_packets: u64,
    rx_bytes: u64,
    tx_bytes: u64,
}

#[derive(Clone, Debug)]
struct Addr4Info {
    ifindex: u32,
    addr: Ipv4Addr,
    prefixlen: u8,
}

#[derive(Clone, Debug)]
struct Addr6Info {
    ifindex: u32,
    addr: Ipv6Addr,
    prefixlen: u8,
}

#[derive(Clone, Debug)]
struct Route4Info {
    ifindex: u32,
    dst_len: u8,
    table: u8,
    gateway: Option<Ipv4Addr>,
}

/// Open a non-blocking `NETLINK_ROUTE` multicast listener socket for UI / daemon live updates.
pub fn open_event_socket() -> Option<i32> {
    // SAFETY: Standard `AF_NETLINK` `SOCK_RAW` socket creation and `bind(2)` with stack `SockAddrNl`.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            NETLINK_ROUTE,
        );
        if fd < 0 {
            return None;
        }
        let addr = SockAddrNl {
            nl_family: libc::AF_NETLINK as u16,
            nl_pad: 0,
            nl_pid: 0,
            nl_groups: RTNLGRP_LINK_MASK
                | RTNLGRP_NEIGH_MASK
                | RTNLGRP_IPV4_IFADDR_MASK
                | RTNLGRP_IPV4_ROUTE_MASK
                | RTNLGRP_IPV6_IFADDR_MASK
                | RTNLGRP_IPV6_ROUTE_MASK,
        };
        if libc::bind(
            fd,
            &addr as *const SockAddrNl as *const libc::sockaddr,
            mem::size_of::<SockAddrNl>() as libc::socklen_t,
        ) < 0
        {
            libc::close(fd);
            return None;
        }
        Some(fd)
    }
}

/// Drain all pending multicast datagrams from an event socket opened by [`open_event_socket`].
pub fn drain_events(fd: i32) -> bool {
    let mut drained = false;
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: Valid stack buffer passed to non-blocking `recv(2)`.
        let n = unsafe {
            libc::recv(
                fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if n > 0 {
            drained = true;
        } else {
            break;
        }
    }
    drained
}

struct NlClient {
    fd: i32,
    seq: u32,
}

impl Drop for NlClient {
    fn drop(&mut self) {
        if self.fd >= 0 {
            // SAFETY: Owned file descriptor closed once on drop.
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

impl NlClient {
    fn open() -> Result<Self, String> {
        // SAFETY: Standard `AF_NETLINK` `socket(2)` + `bind(2)`.
        unsafe {
            let fd = libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                NETLINK_ROUTE,
            );
            if fd < 0 {
                return Err("无法创建 NETLINK_ROUTE 套接字".to_string());
            }
            let addr = SockAddrNl {
                nl_family: libc::AF_NETLINK as u16,
                nl_pad: 0,
                nl_pid: 0,
                nl_groups: 0,
            };
            if libc::bind(
                fd,
                &addr as *const SockAddrNl as *const libc::sockaddr,
                mem::size_of::<SockAddrNl>() as libc::socklen_t,
            ) < 0
            {
                libc::close(fd);
                return Err("无法绑定 NETLINK_ROUTE 套接字".to_string());
            }
            Ok(Self { fd, seq: 1 })
        }
    }

    fn next_seq(&mut self) -> u32 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1).max(1);
        s
    }

    fn send_and_collect(
        &mut self,
        msg_type: u16,
        flags: u16,
        payload: &[u8],
    ) -> Result<Vec<(u16, Vec<u8>)>, String> {
        let seq = self.next_seq();
        let total_len = 16 + payload.len();
        let mut msg = Vec::with_capacity(nlmsg_align(total_len));
        msg.extend_from_slice(&(total_len as u32).to_le_bytes());
        msg.extend_from_slice(&msg_type.to_le_bytes());
        msg.extend_from_slice(&flags.to_le_bytes());
        msg.extend_from_slice(&seq.to_le_bytes());
        msg.extend_from_slice(&0u32.to_le_bytes());
        msg.extend_from_slice(payload);
        while msg.len() < nlmsg_align(total_len) {
            msg.push(0);
        }

        let dst = SockAddrNl {
            nl_family: libc::AF_NETLINK as u16,
            nl_pad: 0,
            nl_pid: 0,
            nl_groups: 0,
        };
        // SAFETY: Valid Netlink message buffer and kernel destination `SockAddrNl`.
        let sent = unsafe {
            libc::sendto(
                self.fd,
                msg.as_ptr() as *const libc::c_void,
                msg.len(),
                0,
                &dst as *const SockAddrNl as *const libc::sockaddr,
                mem::size_of::<SockAddrNl>() as libc::socklen_t,
            )
        };
        if sent < 0 {
            return Err("发送 Netlink 请求失败".to_string());
        }

        let is_dump = flags & NLM_F_DUMP == NLM_F_DUMP;
        let want_ack = flags & NLM_F_ACK != 0;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 32768];

        loop {
            let mut pfd = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: Single valid `pollfd` polled with bounded timeout.
            let ready = unsafe { libc::poll(&mut pfd, 1, 1500) };
            if ready <= 0 {
                return Err("等待 Netlink 响应超时".to_string());
            }
            // SAFETY: Valid heap buffer passed to `recv(2)`.
            let n =
                unsafe { libc::recv(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
            if n <= 0 {
                return Err("读取 Netlink 响应失败".to_string());
            }
            let raw = &buf[..n as usize];
            let mut off = 0usize;
            let mut finished = false;
            while off + 16 <= raw.len() {
                let len = u32::from_le_bytes(raw[off..off + 4].try_into().unwrap()) as usize;
                if len < 16 || off + len > raw.len() {
                    break;
                }
                let rtype = u16::from_le_bytes(raw[off + 4..off + 6].try_into().unwrap());
                let rflags = u16::from_le_bytes(raw[off + 6..off + 8].try_into().unwrap());
                let rseq = u32::from_le_bytes(raw[off + 8..off + 12].try_into().unwrap());
                let body = &raw[off + 16..off + len];
                off += nlmsg_align(len);

                if rseq != seq {
                    continue;
                }
                if rtype == NLMSG_DONE {
                    finished = true;
                    break;
                }
                if rtype == NLMSG_ERROR {
                    if body.len() >= 4 {
                        let code = i32::from_le_bytes(body[0..4].try_into().unwrap());
                        if code == 0 {
                            return Ok(out);
                        }
                        return Err(format!("内核拒绝网络配置 (errno={})", -code));
                    }
                    return Err("Netlink 返回未知错误".to_string());
                }
                out.push((rtype, body.to_vec()));
                if !is_dump && !want_ack && rflags & NLM_F_MULTI == 0 {
                    finished = true;
                    break;
                }
            }
            if finished {
                break;
            }
        }
        Ok(out)
    }

    fn dump_links(&mut self) -> Result<Vec<LinkInfo>, String> {
        let mut ifi = [0u8; 16];
        ifi[0] = AF_UNSPEC;
        let replies = self.send_and_collect(RTM_GETLINK, NLM_F_REQUEST | NLM_F_DUMP, &ifi)?;
        let mut links = Vec::new();
        for (rtype, body) in replies {
            if rtype != RTM_NEWLINK || body.len() < 16 {
                continue;
            }
            let ifindex = i32::from_le_bytes(body[4..8].try_into().unwrap()).max(0) as u32;
            let flags = u32::from_le_bytes(body[8..12].try_into().unwrap());
            let mut info = LinkInfo {
                ifindex,
                name: String::new(),
                flags,
                mtu: 1500,
                mac: [0u8; 6],
                has_mac: false,
                carrier: flags & IFF_RUNNING != 0,
                rx_packets: 0,
                tx_packets: 0,
                rx_bytes: 0,
                tx_bytes: 0,
            };
            let mut has_stats64 = false;
            parse_attrs(&body[16..], |atype, val| match atype {
                IFLA_IFNAME => {
                    let end = val.iter().position(|&b| b == 0).unwrap_or(val.len());
                    info.name = String::from_utf8_lossy(&val[..end]).into_owned();
                }
                IFLA_ADDRESS if val.len() >= 6 => {
                    info.mac.copy_from_slice(&val[..6]);
                    info.has_mac = info.mac != [0u8; 6];
                }
                IFLA_MTU if val.len() >= 4 => {
                    info.mtu = u32::from_le_bytes(val[0..4].try_into().unwrap());
                }
                IFLA_CARRIER if !val.is_empty() => {
                    info.carrier = val[0] != 0;
                }
                IFLA_STATS64 if val.len() >= 32 => {
                    info.rx_packets = u64::from_le_bytes(val[0..8].try_into().unwrap());
                    info.tx_packets = u64::from_le_bytes(val[8..16].try_into().unwrap());
                    info.rx_bytes = u64::from_le_bytes(val[16..24].try_into().unwrap());
                    info.tx_bytes = u64::from_le_bytes(val[24..32].try_into().unwrap());
                    has_stats64 = true;
                }
                IFLA_STATS if !has_stats64 && val.len() >= 16 => {
                    info.rx_packets = u32::from_le_bytes(val[0..4].try_into().unwrap()) as u64;
                    info.tx_packets = u32::from_le_bytes(val[4..8].try_into().unwrap()) as u64;
                    info.rx_bytes = u32::from_le_bytes(val[8..12].try_into().unwrap()) as u64;
                    info.tx_bytes = u32::from_le_bytes(val[12..16].try_into().unwrap()) as u64;
                }
                _ => {}
            });
            links.push(info);
        }
        Ok(links)
    }

    fn dump_addrs(&mut self) -> Result<(Vec<Addr4Info>, Vec<Addr6Info>), String> {
        let mut ifa = [0u8; 8];
        ifa[0] = AF_UNSPEC;
        let replies = self.send_and_collect(RTM_GETADDR, NLM_F_REQUEST | NLM_F_DUMP, &ifa)?;
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        for (rtype, body) in replies {
            if rtype != RTM_NEWADDR || body.len() < 8 {
                continue;
            }
            let family = body[0];
            let prefixlen = body[1];
            let ifindex = u32::from_le_bytes(body[4..8].try_into().unwrap());
            if family == AF_INET_U8 {
                let mut local_ip = None;
                let mut addr_ip = None;
                parse_attrs(&body[8..], |atype, val| {
                    if val.len() >= 4 {
                        let ip = Ipv4Addr::new(val[0], val[1], val[2], val[3]);
                        if atype == IFA_LOCAL {
                            local_ip = Some(ip);
                        } else if atype == IFA_ADDRESS {
                            addr_ip = Some(ip);
                        }
                    }
                });
                if let Some(addr) = local_ip.or(addr_ip) {
                    v4.push(Addr4Info {
                        ifindex,
                        addr,
                        prefixlen,
                    });
                }
            } else if family == AF_INET6_U8 {
                let mut addr_ip = None;
                parse_attrs(&body[8..], |atype, val| {
                    if (atype == IFA_ADDRESS || atype == IFA_LOCAL) && val.len() >= 16 {
                        let mut oct = [0u8; 16];
                        oct.copy_from_slice(&val[..16]);
                        addr_ip = Some(Ipv6Addr::from(oct));
                    }
                });
                if let Some(addr) = addr_ip {
                    v6.push(Addr6Info {
                        ifindex,
                        addr,
                        prefixlen,
                    });
                }
            }
        }
        Ok((v4, v6))
    }

    fn dump_routes4(&mut self) -> Result<Vec<Route4Info>, String> {
        let mut rtm = [0u8; 12];
        rtm[0] = AF_INET_U8;
        let replies = self.send_and_collect(RTM_GETROUTE, NLM_F_REQUEST | NLM_F_DUMP, &rtm)?;
        let mut routes = Vec::new();
        for (rtype, body) in replies {
            if rtype != RTM_NEWROUTE || body.len() < 12 || body[0] != AF_INET_U8 {
                continue;
            }
            let dst_len = body[1];
            let table = body[4];
            let mut oif = 0u32;
            let mut gw = None;
            parse_attrs(&body[12..], |atype, val| match atype {
                RTA_OIF if val.len() >= 4 => {
                    oif = u32::from_le_bytes(val[0..4].try_into().unwrap());
                }
                RTA_GATEWAY if val.len() >= 4 => {
                    gw = Some(Ipv4Addr::new(val[0], val[1], val[2], val[3]));
                }
                _ => {}
            });
            routes.push(Route4Info {
                ifindex: oif,
                dst_len,
                table,
                gateway: gw,
            });
        }
        Ok(routes)
    }

    fn set_link_up(&mut self, ifindex: u32, up: bool) -> Result<(), String> {
        let mut payload = Vec::with_capacity(16);
        payload.push(AF_UNSPEC);
        payload.push(0);
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(&(ifindex as i32).to_le_bytes());
        let flags = if up { IFF_UP } else { 0 };
        payload.extend_from_slice(&flags.to_le_bytes());
        payload.extend_from_slice(&IFF_UP.to_le_bytes());
        self.send_and_collect(RTM_SETLINK, NLM_F_REQUEST | NLM_F_ACK, &payload)?;
        Ok(())
    }

    fn del_ipv4_addr(&mut self, ifindex: u32, addr: Ipv4Addr, prefixlen: u8) -> Result<(), String> {
        let mut payload = Vec::with_capacity(24);
        payload.push(AF_INET_U8);
        payload.push(prefixlen);
        payload.push(0);
        payload.push(RT_SCOPE_UNIVERSE);
        payload.extend_from_slice(&ifindex.to_le_bytes());
        put_attr(&mut payload, IFA_LOCAL, &addr.octets());
        put_attr(&mut payload, IFA_ADDRESS, &addr.octets());
        self.send_and_collect(RTM_DELADDR, NLM_F_REQUEST | NLM_F_ACK, &payload)?;
        Ok(())
    }

    fn add_ipv4_addr(&mut self, ifindex: u32, addr: Ipv4Addr, prefixlen: u8) -> Result<(), String> {
        let mut payload = Vec::with_capacity(24);
        payload.push(AF_INET_U8);
        payload.push(prefixlen);
        payload.push(0);
        payload.push(RT_SCOPE_UNIVERSE);
        payload.extend_from_slice(&ifindex.to_le_bytes());
        put_attr(&mut payload, IFA_LOCAL, &addr.octets());
        put_attr(&mut payload, IFA_ADDRESS, &addr.octets());
        self.send_and_collect(
            RTM_NEWADDR,
            NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE,
            &payload,
        )?;
        Ok(())
    }

    fn del_default_route4(&mut self, ifindex: u32) -> Result<(), String> {
        let mut payload = Vec::with_capacity(32);
        payload.push(AF_INET_U8);
        payload.push(0); // dst_len = 0
        payload.push(0);
        payload.push(0);
        payload.push(RT_TABLE_MAIN);
        payload.push(0);
        payload.push(RT_SCOPE_UNIVERSE);
        payload.push(RTN_UNICAST);
        payload.extend_from_slice(&0u32.to_le_bytes());
        put_attr(&mut payload, RTA_DST, &[0, 0, 0, 0]);
        if ifindex != 0 {
            put_attr(&mut payload, RTA_OIF, &ifindex.to_le_bytes());
        }
        let _ = self.send_and_collect(RTM_DELROUTE, NLM_F_REQUEST | NLM_F_ACK, &payload);
        Ok(())
    }

    fn replace_default_route4(
        &mut self,
        ifindex: u32,
        gateway: Ipv4Addr,
        prefsrc: Option<Ipv4Addr>,
        protocol: u8,
    ) -> Result<(), String> {
        let mut payload = Vec::with_capacity(48);
        payload.push(AF_INET_U8);
        payload.push(0);
        payload.push(0);
        payload.push(0);
        payload.push(RT_TABLE_MAIN);
        payload.push(protocol);
        payload.push(RT_SCOPE_UNIVERSE);
        payload.push(RTN_UNICAST);
        payload.extend_from_slice(&0u32.to_le_bytes());
        put_attr(&mut payload, RTA_GATEWAY, &gateway.octets());
        put_attr(&mut payload, RTA_OIF, &ifindex.to_le_bytes());
        if let Some(src) = prefsrc {
            put_attr(&mut payload, RTA_PREFSRC, &src.octets());
        }
        self.send_and_collect(
            RTM_NEWROUTE,
            NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE,
            &payload,
        )?;
        Ok(())
    }
}

fn nlmsg_align(len: usize) -> usize {
    (len + 3) & !3
}

fn put_attr(buf: &mut Vec<u8>, atype: u16, data: &[u8]) {
    let rta_len = (4 + data.len()) as u16;
    buf.extend_from_slice(&rta_len.to_le_bytes());
    buf.extend_from_slice(&atype.to_le_bytes());
    buf.extend_from_slice(data);
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

fn parse_attrs<F: FnMut(u16, &[u8])>(mut bytes: &[u8], mut cb: F) {
    while bytes.len() >= 4 {
        let len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        let atype = u16::from_le_bytes([bytes[2], bytes[3]]) & 0x3fff;
        if len < 4 || len > bytes.len() {
            break;
        }
        cb(atype, &bytes[4..len]);
        let step = nlmsg_align(len);
        if step >= bytes.len() {
            break;
        }
        bytes = &bytes[step..];
    }
}

fn select_primary_link(links: &[LinkInfo]) -> Option<&LinkInfo> {
    links
        .iter()
        .find(|l| l.flags & IFF_LOOPBACK == 0 && l.name.starts_with("eth"))
        .or_else(|| {
            links
                .iter()
                .find(|l| l.flags & IFF_LOOPBACK == 0 && l.name != "lo")
        })
}

fn format_mac(mac: &[u8; 6], valid: bool) -> String {
    if !valid {
        return "--".to_string();
    }
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

#[derive(Clone, Debug, Default)]
struct RuntimeStatusFile {
    daemon_running: bool,
    state: DeviceState,
    profile: String,
    dns: Option<Ipv4Addr>,
    dhcp_state: String,
    lease_secs: u32,
    lease_obtained_epoch: u64,
    t1_secs: u32,
    t2_secs: u32,
    server_id: Option<Ipv4Addr>,
    mode_note: String,
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn read_runtime_status() -> RuntimeStatusFile {
    let mut rt = RuntimeStatusFile {
        daemon_running: Path::new(NM_SOCKET_PATH).exists(),
        state: DeviceState::Unavailable,
        ..RuntimeStatusFile::default()
    };
    if let Ok(raw) = fs::read_to_string(STATUS_FILE) {
        for line in raw.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let val = v.trim();
            match k.trim() {
                "daemon" => rt.daemon_running |= val == "NetworkManager",
                "state" => rt.state = DeviceState::from_code_or_str(val),
                "profile" => rt.profile = val.to_string(),
                "dns" => rt.dns = val.parse::<Ipv4Addr>().ok(),
                "dhcp_state" => rt.dhcp_state = val.to_string(),
                "lease_secs" => rt.lease_secs = val.parse().unwrap_or(0),
                "lease_epoch" => rt.lease_obtained_epoch = val.parse().unwrap_or(0),
                "t1_secs" => rt.t1_secs = val.parse().unwrap_or(0),
                "t2_secs" => rt.t2_secs = val.parse().unwrap_or(0),
                "server_id" => rt.server_id = val.parse::<Ipv4Addr>().ok(),
                "mode_note" => rt.mode_note = val.to_string(),
                _ => {}
            }
        }
    }
    if rt.dns.is_none() {
        for path in [NM_RESOLV_CONF, ETC_RESOLV_CONF] {
            if let Ok(raw) = fs::read_to_string(path) {
                for line in raw.lines() {
                    if let Some(rest) = line.trim().strip_prefix("nameserver") {
                        if let Ok(ip) = rest.trim().parse::<Ipv4Addr>() {
                            rt.dns = Some(ip);
                            break;
                        }
                    }
                }
                if rt.dns.is_some() {
                    break;
                }
            }
        }
    }
    rt
}

fn write_runtime_files(
    ifname: &str,
    state: DeviceState,
    profile: &str,
    dns: Option<Ipv4Addr>,
    dhcp_state: &str,
    lease: Option<&DhcpLease>,
    lease_epoch: u64,
    mode_note: &str,
) {
    let _ = fs::create_dir_all(NM_RUN_DIR);
    let tmp = "/run/netmgr.status.tmp";
    let mut body = String::with_capacity(256);
    body.push_str("daemon=NetworkManager\n");
    body.push_str(&format!("ifname={ifname}\n"));
    body.push_str(&format!("state={} ({})\n", state.code(), state.as_str()));
    body.push_str(&format!("profile={profile}\n"));
    if let Some(ip) = dns {
        body.push_str(&format!("dns={ip}\n"));
    }
    body.push_str(&format!("dhcp_state={dhcp_state}\n"));
    if let Some(l) = lease {
        body.push_str(&format!("lease_secs={}\n", l.lease_secs));
        body.push_str(&format!("lease_epoch={lease_epoch}\n"));
        body.push_str(&format!("t1_secs={}\n", l.t1_secs));
        body.push_str(&format!("t2_secs={}\n", l.t2_secs));
        if let Some(sid) = l.server_id {
            body.push_str(&format!("server_id={sid}\n"));
        }
    }
    body.push_str(&format!("mode_note={mode_note}\n"));
    if fs::write(tmp, body).is_ok() {
        let _ = fs::set_permissions(tmp, fs::Permissions::from_mode(0o644));
        let _ = fs::rename(tmp, STATUS_FILE);
    }

    if let Some(ip) = dns {
        let resolv = format!("# Generated by SpaceOS NetworkManager\nnameserver {ip}\n");
        let nm_tmp = "/run/NetworkManager/resolv.conf.tmp";
        if fs::write(nm_tmp, &resolv).is_ok() {
            let _ = fs::set_permissions(nm_tmp, fs::Permissions::from_mode(0o644));
            let _ = fs::rename(nm_tmp, NM_RESOLV_CONF);
        }
        let etc_tmp = "/etc/resolv.conf.tmp";
        if fs::write(etc_tmp, &resolv).is_ok() {
            let _ = fs::set_permissions(etc_tmp, fs::Permissions::from_mode(0o644));
            let _ = fs::rename(etc_tmp, ETC_RESOLV_CONF);
        }
    }
}

/// Query live interface, address, route, NetworkManager state, lease and counter status.
pub fn query_status() -> NetStatus {
    let rt = read_runtime_status();
    let Ok(mut nl) = NlClient::open() else {
        return NetStatus {
            daemon_running: rt.daemon_running,
            device_state: rt.state,
            profile_name: rt.profile,
            dns: rt.dns,
            dhcp_state: rt.dhcp_state,
            mode_note: rt.mode_note,
            ..NetStatus::default()
        };
    };
    let links = nl.dump_links().unwrap_or_default();
    let Some(primary) = select_primary_link(&links).cloned() else {
        return NetStatus {
            daemon_running: rt.daemon_running,
            device_state: DeviceState::Unavailable,
            profile_name: rt.profile,
            dns: rt.dns,
            dhcp_state: rt.dhcp_state,
            mode_note: rt.mode_note,
            ..NetStatus::default()
        };
    };
    let (addrs4, addrs6) = nl.dump_addrs().unwrap_or_default();
    let routes4 = nl.dump_routes4().unwrap_or_default();

    let ipv4_all: Vec<(Ipv4Addr, u8)> = addrs4
        .into_iter()
        .filter(|a| a.ifindex == primary.ifindex)
        .map(|a| (a.addr, a.prefixlen))
        .collect();
    let ipv4 = ipv4_all.first().copied();

    let ipv6_ll = addrs6
        .into_iter()
        .filter(|a| a.ifindex == primary.ifindex)
        .find(|a| (a.addr.segments()[0] & 0xffc0) == 0xfe80)
        .map(|a| (a.addr, a.prefixlen));

    let gateway = routes4
        .iter()
        .find(|r| {
            r.dst_len == 0
                && (r.table == RT_TABLE_MAIN || r.table == 0)
                && (r.ifindex == primary.ifindex || r.ifindex == 0)
                && r.gateway.is_some()
        })
        .and_then(|r| r.gateway);

    let up = primary.flags & IFF_UP != 0;
    let inferred_state = if rt.daemon_running && rt.state != DeviceState::Unavailable {
        rt.state
    } else if !up || !primary.carrier {
        DeviceState::Unavailable
    } else if ipv4.is_some() {
        DeviceState::Activated
    } else {
        DeviceState::Disconnected
    };

    let lease_remaining_secs = if rt.lease_secs > 0 && rt.lease_obtained_epoch > 0 {
        let elapsed = now_epoch_secs().saturating_sub(rt.lease_obtained_epoch);
        u64::from(rt.lease_secs).saturating_sub(elapsed) as u32
    } else {
        0
    };

    NetStatus {
        daemon_running: rt.daemon_running,
        device_state: inferred_state,
        profile_name: if rt.profile.is_empty() {
            format!("Wired connection ({})", primary.name)
        } else {
            rt.profile
        },
        has_device: true,
        ifindex: primary.ifindex,
        ifname: primary.name,
        mac: format_mac(&primary.mac, primary.has_mac),
        up,
        carrier: primary.carrier,
        mtu: primary.mtu,
        ipv4,
        ipv4_all,
        ipv6_ll,
        gateway,
        dns: rt.dns,
        dhcp_state: rt.dhcp_state,
        lease_secs: rt.lease_secs,
        lease_remaining_secs,
        t1_secs: rt.t1_secs,
        t2_secs: rt.t2_secs,
        server_id: rt.server_id,
        mode_note: rt.mode_note,
        rx_packets: primary.rx_packets,
        tx_packets: primary.tx_packets,
        rx_bytes: primary.rx_bytes,
        tx_bytes: primary.tx_bytes,
    }
}

/// Parse a CIDR prefix length (`"24"`, `"/24"`) or dotted IPv4 netmask (`"255.255.255.0"`).
pub fn parse_prefix_or_netmask(input: &str) -> Option<u8> {
    let s = input.trim().trim_start_matches('/');
    if let Ok(p) = s.parse::<u8>() {
        if (1..=32).contains(&p) {
            return Some(p);
        }
        return None;
    }
    let mask = s.parse::<Ipv4Addr>().ok()?;
    let bits = u32::from(mask);
    if bits == 0 {
        return None;
    }
    let ones = bits.leading_ones();
    if ones == 0 || (ones < 32 && (bits << ones) != 0) {
        return None;
    }
    Some(ones as u8)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DhcpLease {
    pub ip: Ipv4Addr,
    pub prefixlen: u8,
    pub gateway: Option<Ipv4Addr>,
    pub dns: Option<Ipv4Addr>,
    pub server_id: Option<Ipv4Addr>,
    pub lease_secs: u32,
    pub t1_secs: u32,
    pub t2_secs: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DhcpReply {
    pub msg_type: u8,
    pub yiaddr: Ipv4Addr,
    pub prefixlen: u8,
    pub gateway: Option<Ipv4Addr>,
    pub dns: Option<Ipv4Addr>,
    pub server_id: Option<Ipv4Addr>,
    pub lease_secs: u32,
    pub t1_secs: Option<u32>,
    pub t2_secs: Option<u32>,
}

/// Build an RFC 2131 BOOTP/DHCPv4 packet (minimum 300 octets).
///
/// - `Selecting` (`DHCPDISCOVER`): `ciaddr = 0`, `broadcast = true`, no Option 50/54.
/// - `Requesting` (`DHCPREQUEST`): `ciaddr = 0`, `broadcast = true`, Option 50 + Option 54.
/// - `Renewing` / `Rebinding` (`DHCPREQUEST`): `ciaddr = lease.ip`, no Option 50/54 per RFC 2131 §4.3.2.
/// - `Release` (`DHCPRELEASE`): `ciaddr = lease.ip`, Option 54 `server_id`.
pub fn build_dhcp_packet(
    xid: u32,
    mac: &[u8; 6],
    msg_type: u8,
    ciaddr: Option<Ipv4Addr>,
    requested_ip: Option<Ipv4Addr>,
    server_id: Option<Ipv4Addr>,
    broadcast_flag: bool,
    hostname: &str,
) -> Vec<u8> {
    let mut pkt = vec![0u8; 320];
    pkt[0] = 1; // BOOTREQUEST
    pkt[1] = 1; // Ethernet (10Mb)
    pkt[2] = 6; // Hardware address length
    pkt[3] = 0; // Hops
    pkt[4..8].copy_from_slice(&xid.to_be_bytes());
    pkt[8..10].copy_from_slice(&0u16.to_be_bytes());
    let flags: u16 = if broadcast_flag { 0x8000 } else { 0x0000 };
    pkt[10..12].copy_from_slice(&flags.to_be_bytes());
    if let Some(ci) = ciaddr {
        pkt[12..16].copy_from_slice(&ci.octets());
    }
    pkt[28..34].copy_from_slice(mac);
    pkt[236..240].copy_from_slice(&[99, 130, 83, 99]); // Magic cookie

    let mut idx = 240usize;
    // Option 53: DHCP Message Type
    pkt[idx..idx + 3].copy_from_slice(&[53, 1, msg_type]);
    idx += 3;
    // Option 61: Client Identifier
    pkt[idx..idx + 9].copy_from_slice(&[61, 7, 1, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]);
    idx += 9;
    // Option 12: Host Name
    let host_bytes = hostname.trim().as_bytes();
    if !host_bytes.is_empty() && msg_type != DHCP_RELEASE {
        let hlen = host_bytes.len().min(32);
        pkt[idx] = 12;
        pkt[idx + 1] = hlen as u8;
        pkt[idx + 2..idx + 2 + hlen].copy_from_slice(&host_bytes[..hlen]);
        idx += 2 + hlen;
    }
    if let Some(req) = requested_ip {
        pkt[idx] = 50;
        pkt[idx + 1] = 4;
        pkt[idx + 2..idx + 6].copy_from_slice(&req.octets());
        idx += 6;
    }
    if let Some(sid) = server_id {
        pkt[idx] = 54;
        pkt[idx + 1] = 4;
        pkt[idx + 2..idx + 6].copy_from_slice(&sid.octets());
        idx += 6;
    }
    if msg_type == DHCP_DISCOVER || msg_type == DHCP_REQUEST {
        // Option 55: Parameter Request List (Subnet, Router, DNS, Domain, Broadcast, Lease, T1, T2)
        let prl = [55u8, 8, 1, 3, 6, 15, 28, 51, 58, 59];
        pkt[idx..idx + prl.len()].copy_from_slice(&prl);
        idx += prl.len();
    }
    pkt[idx] = 255; // End
    idx += 1;
    pkt.truncate(idx.max(300));
    pkt
}

/// Parse an incoming BOOTP/DHCPv4 reply matching `(expected_xid, mac)`.
pub fn parse_dhcp_reply(buf: &[u8], expected_xid: u32, mac: &[u8; 6]) -> Option<DhcpReply> {
    if buf.len() < 244 || buf[0] != 2 {
        return None;
    }
    let xid = u32::from_be_bytes(buf[4..8].try_into().ok()?);
    if xid != expected_xid || &buf[28..34] != mac {
        return None;
    }
    if buf[236..240] != [99, 130, 83, 99] {
        return None;
    }
    let yiaddr = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);

    let mut msg_type = 0u8;
    let mut prefixlen = 24u8;
    let mut gateway = None;
    let mut dns = None;
    let mut server_id = None;
    let mut lease_secs = 3600u32;
    let mut t1_secs = None;
    let mut t2_secs = None;

    let mut i = 240usize;
    while i < buf.len() {
        let code = buf[i];
        if code == 255 {
            break;
        }
        if code == 0 {
            i += 1;
            continue;
        }
        if i + 2 > buf.len() {
            break;
        }
        let olen = buf[i + 1] as usize;
        if i + 2 + olen > buf.len() {
            break;
        }
        let val = &buf[i + 2..i + 2 + olen];
        match code {
            53 if olen >= 1 => msg_type = val[0],
            1 if olen >= 4 => {
                let mask = Ipv4Addr::new(val[0], val[1], val[2], val[3]);
                let ones = u32::from(mask).leading_ones();
                if (1..=32).contains(&ones) {
                    prefixlen = ones as u8;
                }
            }
            3 if olen >= 4 => {
                gateway = Some(Ipv4Addr::new(val[0], val[1], val[2], val[3]));
            }
            6 if olen >= 4 => {
                dns = Some(Ipv4Addr::new(val[0], val[1], val[2], val[3]));
            }
            54 if olen >= 4 => {
                server_id = Some(Ipv4Addr::new(val[0], val[1], val[2], val[3]));
            }
            51 if olen >= 4 => {
                lease_secs = u32::from_be_bytes(val[0..4].try_into().unwrap()).max(60);
            }
            58 if olen >= 4 => {
                t1_secs = Some(u32::from_be_bytes(val[0..4].try_into().unwrap()));
            }
            59 if olen >= 4 => {
                t2_secs = Some(u32::from_be_bytes(val[0..4].try_into().unwrap()));
            }
            _ => {}
        }
        i += 2 + olen;
    }

    if msg_type == 0 {
        return None;
    }
    if msg_type != DHCP_NAK && yiaddr.is_unspecified() {
        return None;
    }
    Some(DhcpReply {
        msg_type,
        yiaddr,
        prefixlen,
        gateway,
        dns,
        server_id,
        lease_secs,
        t1_secs,
        t2_secs,
    })
}

fn reply_to_lease(chosen: &DhcpReply, fallback_offer: Option<&DhcpReply>) -> DhcpLease {
    let lease_secs = chosen.lease_secs.max(60);
    let default_t1 = (lease_secs / 2).max(30);
    let default_t2 = ((u64::from(lease_secs) * 7) / 8) as u32;
    let t1 = chosen
        .t1_secs
        .filter(|&v| v > 0 && v < lease_secs)
        .unwrap_or(default_t1);
    let t2 = chosen
        .t2_secs
        .filter(|&v| v > t1 && v < lease_secs)
        .unwrap_or_else(|| default_t2.max(t1 + 1).min(lease_secs));
    DhcpLease {
        ip: chosen.yiaddr,
        prefixlen: chosen.prefixlen,
        gateway: chosen
            .gateway
            .or_else(|| fallback_offer.and_then(|o| o.gateway)),
        dns: chosen.dns.or_else(|| fallback_offer.and_then(|o| o.dns)),
        server_id: chosen
            .server_id
            .or_else(|| fallback_offer.and_then(|o| o.server_id)),
        lease_secs,
        t1_secs: t1,
        t2_secs: t2,
    }
}

fn open_dhcp_udp_socket(ifname: &str) -> Option<i32> {
    // SAFETY: Standard `AF_INET` UDP socket setup with `SO_REUSEADDR`, `SO_BROADCAST`, `SO_BINDTODEVICE`, and `bind(0.0.0.0:68)`.
    unsafe {
        let fd = libc::socket(
            libc::AF_INET,
            libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            libc::IPPROTO_UDP,
        );
        if fd < 0 {
            return None;
        }
        let one: libc::c_int = 1;
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &one as *const _ as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_BROADCAST,
            &one as *const _ as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        if !ifname.is_empty() {
            let mut dev_buf = Vec::with_capacity(ifname.len() + 1);
            dev_buf.extend_from_slice(ifname.as_bytes());
            dev_buf.push(0);
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_BINDTODEVICE,
                dev_buf.as_ptr() as *const libc::c_void,
                dev_buf.len() as libc::socklen_t,
            );
        }
        let mut bind_addr: libc::sockaddr_in = mem::zeroed();
        bind_addr.sin_family = libc::AF_INET as libc::sa_family_t;
        bind_addr.sin_port = 68u16.to_be();
        bind_addr.sin_addr.s_addr = 0;
        if libc::bind(
            fd,
            &bind_addr as *const libc::sockaddr_in as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ) < 0
        {
            libc::close(fd);
            return None;
        }
        Some(fd)
    }
}

fn send_dhcp_udp(fd: i32, dst_ip: Ipv4Addr, pkt: &[u8]) {
    // SAFETY: Valid `sockaddr_in` and packet slice passed to `sendto(2)`.
    unsafe {
        let mut dst: libc::sockaddr_in = mem::zeroed();
        dst.sin_family = libc::AF_INET as libc::sa_family_t;
        dst.sin_port = 67u16.to_be();
        dst.sin_addr.s_addr = u32::from_ne_bytes(dst_ip.octets());
        let _ = libc::sendto(
            fd,
            pkt.as_ptr() as *const libc::c_void,
            pkt.len(),
            0,
            &dst as *const libc::sockaddr_in as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        );
    }
}

fn gen_xid(mac: &[u8; 6], salt: u32) -> u32 {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0x5350_4b39u32, |d| d.subsec_nanos() ^ (d.as_secs() as u32));
    seed ^ ((mac[4] as u32) << 8) ^ (mac[5] as u32) ^ salt.wrapping_mul(0x9e37_79b9)
}

fn system_hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "spaceos".to_string())
}

#[derive(Clone, Debug)]
enum DhcpPhase {
    Idle,
    Selecting {
        xid: u32,
        attempt: u8,
        next_tx: Instant,
    },
    Requesting {
        xid: u32,
        offer: DhcpReply,
        deadline: Instant,
    },
    Bound {
        lease: DhcpLease,
        lease_epoch: u64,
        t1_at: Instant,
        t2_at: Instant,
        expire_at: Instant,
    },
    Renewing {
        xid: u32,
        lease: DhcpLease,
        lease_epoch: u64,
        next_tx: Instant,
        t2_at: Instant,
        expire_at: Instant,
    },
    Rebinding {
        xid: u32,
        lease: DhcpLease,
        lease_epoch: u64,
        next_tx: Instant,
        expire_at: Instant,
    },
    Fallback {
        lease: DhcpLease,
        retry_at: Instant,
    },
}

enum DhcpEvent {
    None,
    LeaseAcquired {
        lease: DhcpLease,
        epoch: u64,
        renewed: bool,
    },
    FallbackApplied {
        lease: DhcpLease,
    },
    LeaseExpired,
}

/// Non-blocking RFC 2131 DHCPv4 state machine integrated into `NetworkManager`'s `poll(2)` loop.
struct DhcpEngine {
    fd: i32,
    ifname: String,
    mac: [u8; 6],
    hostname: String,
    tx_counter: u32,
    phase: DhcpPhase,
}

impl Drop for DhcpEngine {
    fn drop(&mut self) {
        self.close_socket();
    }
}

impl DhcpEngine {
    fn new() -> Self {
        Self {
            fd: -1,
            ifname: String::new(),
            mac: [0u8; 6],
            hostname: system_hostname(),
            tx_counter: 1,
            phase: DhcpPhase::Idle,
        }
    }

    fn close_socket(&mut self) {
        if self.fd >= 0 {
            // SAFETY: Owned UDP socket descriptor closed once.
            unsafe {
                libc::close(self.fd);
            }
            self.fd = -1;
        }
    }

    fn ensure_socket(&mut self) -> bool {
        if self.fd >= 0 {
            return true;
        }
        if let Some(fd) = open_dhcp_udp_socket(&self.ifname) {
            self.fd = fd;
            true
        } else {
            false
        }
    }

    fn as_raw_fd(&self) -> Option<i32> {
        if self.fd >= 0 {
            Some(self.fd)
        } else {
            None
        }
    }

    fn state_name(&self) -> &'static str {
        match self.phase {
            DhcpPhase::Idle => "none",
            DhcpPhase::Selecting { .. } => "selecting",
            DhcpPhase::Requesting { .. } => "requesting",
            DhcpPhase::Bound { .. } => "bound",
            DhcpPhase::Renewing { .. } => "renewing",
            DhcpPhase::Rebinding { .. } => "rebinding",
            DhcpPhase::Fallback { .. } => "fallback",
        }
    }

    fn active_lease(&self) -> Option<(&DhcpLease, u64)> {
        match &self.phase {
            DhcpPhase::Bound {
                lease, lease_epoch, ..
            }
            | DhcpPhase::Renewing {
                lease, lease_epoch, ..
            }
            | DhcpPhase::Rebinding {
                lease, lease_epoch, ..
            } => Some((lease, *lease_epoch)),
            DhcpPhase::Fallback { lease, .. } => Some((lease, 0)),
            _ => None,
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        match &self.phase {
            DhcpPhase::Idle => None,
            DhcpPhase::Selecting { next_tx, .. } => Some(*next_tx),
            DhcpPhase::Requesting { deadline, .. } => Some(*deadline),
            DhcpPhase::Bound { t1_at, .. } => Some(*t1_at),
            DhcpPhase::Renewing {
                next_tx,
                t2_at,
                expire_at,
                ..
            } => Some((*next_tx).min(*t2_at).min(*expire_at)),
            DhcpPhase::Rebinding {
                next_tx, expire_at, ..
            } => Some((*next_tx).min(*expire_at)),
            DhcpPhase::Fallback { retry_at, .. } => Some(*retry_at),
        }
    }

    fn next_xid(&mut self) -> u32 {
        self.tx_counter = self.tx_counter.wrapping_add(1);
        gen_xid(&self.mac, self.tx_counter)
    }

    fn start_discover(&mut self, ifname: &str, mac: [u8; 6]) {
        self.close_socket();
        self.ifname = ifname.to_string();
        self.mac = mac;
        self.hostname = system_hostname();
        let xid = self.next_xid();
        if self.ensure_socket() {
            let pkt = build_dhcp_packet(
                xid,
                &self.mac,
                DHCP_DISCOVER,
                None,
                None,
                None,
                true,
                &self.hostname,
            );
            send_dhcp_udp(self.fd, Ipv4Addr::BROADCAST, &pkt);
        }
        self.phase = DhcpPhase::Selecting {
            xid,
            attempt: 1,
            next_tx: Instant::now() + Duration::from_millis(650),
        };
    }

    fn trigger_renew(&mut self) {
        let now = Instant::now();
        match self.phase.clone() {
            DhcpPhase::Bound {
                lease,
                lease_epoch,
                t2_at,
                expire_at,
                ..
            } => {
                let xid = self.next_xid();
                if self.ensure_socket() {
                    let dst = lease.server_id.unwrap_or(Ipv4Addr::BROADCAST);
                    let pkt = build_dhcp_packet(
                        xid,
                        &self.mac,
                        DHCP_REQUEST,
                        Some(lease.ip),
                        None,
                        None,
                        dst == Ipv4Addr::BROADCAST,
                        &self.hostname,
                    );
                    send_dhcp_udp(self.fd, dst, &pkt);
                }
                self.phase = DhcpPhase::Renewing {
                    xid,
                    lease,
                    lease_epoch,
                    next_tx: now + Duration::from_secs(4),
                    t2_at: t2_at.max(now + Duration::from_secs(8)),
                    expire_at: expire_at.max(now + Duration::from_secs(16)),
                };
            }
            _ => {
                if !self.ifname.is_empty() {
                    let ifn = self.ifname.clone();
                    let mac = self.mac;
                    self.start_discover(&ifn, mac);
                }
            }
        }
    }

    fn send_release_and_stop(&mut self) {
        if let Some((lease, _)) = self.active_lease() {
            let lease_copy = *lease;
            if lease_copy.lease_secs > 0 && self.ensure_socket() {
                let xid = self.next_xid();
                let dst = lease_copy.server_id.unwrap_or(Ipv4Addr::BROADCAST);
                let pkt = build_dhcp_packet(
                    xid,
                    &self.mac,
                    DHCP_RELEASE,
                    Some(lease_copy.ip),
                    None,
                    lease_copy.server_id,
                    false,
                    &self.hostname,
                );
                send_dhcp_udp(self.fd, dst, &pkt);
            }
        }
        self.close_socket();
        self.phase = DhcpPhase::Idle;
    }

    fn enter_bound(&mut self, lease: DhcpLease, renewed: bool) -> DhcpEvent {
        let now = Instant::now();
        let epoch = now_epoch_secs();
        let t1_at = now + Duration::from_secs(u64::from(lease.t1_secs));
        let t2_at = now + Duration::from_secs(u64::from(lease.t2_secs));
        let expire_at = now + Duration::from_secs(u64::from(lease.lease_secs));
        // Release port 68 while quiescent in BOUND; reopened automatically at T1.
        self.close_socket();
        self.phase = DhcpPhase::Bound {
            lease,
            lease_epoch: epoch,
            t1_at,
            t2_at,
            expire_at,
        };
        DhcpEvent::LeaseAcquired {
            lease,
            epoch,
            renewed,
        }
    }

    fn step(&mut self, readable: bool) -> DhcpEvent {
        if readable && self.fd >= 0 {
            let mut buf = [0u8; 1500];
            loop {
                // SAFETY: Valid stack buffer with non-blocking `MSG_DONTWAIT`.
                let n = unsafe {
                    libc::recv(
                        self.fd,
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                        libc::MSG_DONTWAIT,
                    )
                };
                if n <= 0 {
                    break;
                }
                let slice = &buf[..n as usize];
                match self.phase.clone() {
                    DhcpPhase::Selecting { xid, .. } => {
                        if let Some(rep) = parse_dhcp_reply(slice, xid, &self.mac) {
                            if rep.msg_type == DHCP_OFFER {
                                let req_pkt = build_dhcp_packet(
                                    xid,
                                    &self.mac,
                                    DHCP_REQUEST,
                                    None,
                                    Some(rep.yiaddr),
                                    rep.server_id,
                                    true,
                                    &self.hostname,
                                );
                                send_dhcp_udp(self.fd, Ipv4Addr::BROADCAST, &req_pkt);
                                self.phase = DhcpPhase::Requesting {
                                    xid,
                                    offer: rep,
                                    deadline: Instant::now() + Duration::from_millis(650),
                                };
                            } else if rep.msg_type == DHCP_ACK {
                                let lease = reply_to_lease(&rep, None);
                                return self.enter_bound(lease, false);
                            }
                        }
                    }
                    DhcpPhase::Requesting { xid, offer, .. } => {
                        if let Some(rep) = parse_dhcp_reply(slice, xid, &self.mac) {
                            if rep.msg_type == DHCP_ACK {
                                let lease = reply_to_lease(&rep, Some(&offer));
                                return self.enter_bound(lease, false);
                            } else if rep.msg_type == DHCP_NAK {
                                let ifn = self.ifname.clone();
                                let mac = self.mac;
                                self.start_discover(&ifn, mac);
                                return DhcpEvent::None;
                            }
                        }
                    }
                    DhcpPhase::Renewing { xid, lease, .. }
                    | DhcpPhase::Rebinding { xid, lease, .. } => {
                        if let Some(rep) = parse_dhcp_reply(slice, xid, &self.mac) {
                            if rep.msg_type == DHCP_ACK {
                                let mut updated = reply_to_lease(&rep, None);
                                if updated.gateway.is_none() {
                                    updated.gateway = lease.gateway;
                                }
                                if updated.dns.is_none() {
                                    updated.dns = lease.dns;
                                }
                                if updated.server_id.is_none() {
                                    updated.server_id = lease.server_id;
                                }
                                return self.enter_bound(updated, true);
                            } else if rep.msg_type == DHCP_NAK {
                                let ifn = self.ifname.clone();
                                let mac = self.mac;
                                self.start_discover(&ifn, mac);
                                return DhcpEvent::LeaseExpired;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let now = Instant::now();
        match self.phase.clone() {
            DhcpPhase::Idle => DhcpEvent::None,
            DhcpPhase::Selecting {
                xid,
                attempt,
                next_tx,
            } => {
                if now < next_tx {
                    return DhcpEvent::None;
                }
                if attempt < 2 {
                    if self.ensure_socket() {
                        let pkt = build_dhcp_packet(
                            xid,
                            &self.mac,
                            DHCP_DISCOVER,
                            None,
                            None,
                            None,
                            true,
                            &self.hostname,
                        );
                        send_dhcp_udp(self.fd, Ipv4Addr::BROADCAST, &pkt);
                    }
                    self.phase = DhcpPhase::Selecting {
                        xid,
                        attempt: attempt + 1,
                        next_tx: now + Duration::from_millis(650),
                    };
                    DhcpEvent::None
                } else {
                    self.close_socket();
                    let fallback = DhcpLease {
                        ip: Ipv4Addr::new(10, 0, 2, 15),
                        prefixlen: 24,
                        gateway: Some(Ipv4Addr::new(10, 0, 2, 2)),
                        dns: Some(Ipv4Addr::new(10, 0, 2, 3)),
                        server_id: None,
                        lease_secs: 0,
                        t1_secs: 0,
                        t2_secs: 0,
                    };
                    self.phase = DhcpPhase::Fallback {
                        lease: fallback,
                        retry_at: now + Duration::from_secs(60),
                    };
                    DhcpEvent::FallbackApplied { lease: fallback }
                }
            }
            DhcpPhase::Requesting {
                offer, deadline, ..
            } => {
                if now < deadline {
                    return DhcpEvent::None;
                }
                let lease = reply_to_lease(&offer, None);
                self.enter_bound(lease, false)
            }
            DhcpPhase::Bound {
                lease,
                lease_epoch,
                t1_at,
                t2_at,
                expire_at,
            } => {
                if now < t1_at {
                    return DhcpEvent::None;
                }
                let xid = self.next_xid();
                if self.ensure_socket() {
                    let dst = lease.server_id.unwrap_or(Ipv4Addr::BROADCAST);
                    let pkt = build_dhcp_packet(
                        xid,
                        &self.mac,
                        DHCP_REQUEST,
                        Some(lease.ip),
                        None,
                        None,
                        dst == Ipv4Addr::BROADCAST,
                        &self.hostname,
                    );
                    send_dhcp_udp(self.fd, dst, &pkt);
                }
                self.phase = DhcpPhase::Renewing {
                    xid,
                    lease,
                    lease_epoch,
                    next_tx: now + Duration::from_secs(10),
                    t2_at,
                    expire_at,
                };
                DhcpEvent::None
            }
            DhcpPhase::Renewing {
                xid,
                lease,
                lease_epoch,
                next_tx,
                t2_at,
                expire_at,
            } => {
                if now >= expire_at {
                    let ifn = self.ifname.clone();
                    let mac = self.mac;
                    self.start_discover(&ifn, mac);
                    return DhcpEvent::LeaseExpired;
                }
                if now >= t2_at {
                    let new_xid = self.next_xid();
                    if self.ensure_socket() {
                        let pkt = build_dhcp_packet(
                            new_xid,
                            &self.mac,
                            DHCP_REQUEST,
                            Some(lease.ip),
                            None,
                            None,
                            true,
                            &self.hostname,
                        );
                        send_dhcp_udp(self.fd, Ipv4Addr::BROADCAST, &pkt);
                    }
                    self.phase = DhcpPhase::Rebinding {
                        xid: new_xid,
                        lease,
                        lease_epoch,
                        next_tx: now + Duration::from_secs(10),
                        expire_at,
                    };
                    return DhcpEvent::None;
                }
                if now >= next_tx {
                    if self.ensure_socket() {
                        let dst = lease.server_id.unwrap_or(Ipv4Addr::BROADCAST);
                        let pkt = build_dhcp_packet(
                            xid,
                            &self.mac,
                            DHCP_REQUEST,
                            Some(lease.ip),
                            None,
                            None,
                            dst == Ipv4Addr::BROADCAST,
                            &self.hostname,
                        );
                        send_dhcp_udp(self.fd, dst, &pkt);
                    }
                    self.phase = DhcpPhase::Renewing {
                        xid,
                        lease,
                        lease_epoch,
                        next_tx: now + Duration::from_secs(15),
                        t2_at,
                        expire_at,
                    };
                }
                DhcpEvent::None
            }
            DhcpPhase::Rebinding {
                xid,
                lease,
                lease_epoch,
                next_tx,
                expire_at,
            } => {
                if now >= expire_at {
                    let ifn = self.ifname.clone();
                    let mac = self.mac;
                    self.start_discover(&ifn, mac);
                    return DhcpEvent::LeaseExpired;
                }
                if now >= next_tx {
                    if self.ensure_socket() {
                        let pkt = build_dhcp_packet(
                            xid,
                            &self.mac,
                            DHCP_REQUEST,
                            Some(lease.ip),
                            None,
                            None,
                            true,
                            &self.hostname,
                        );
                        send_dhcp_udp(self.fd, Ipv4Addr::BROADCAST, &pkt);
                    }
                    self.phase = DhcpPhase::Rebinding {
                        xid,
                        lease,
                        lease_epoch,
                        next_tx: now + Duration::from_secs(15),
                        expire_at,
                    };
                }
                DhcpEvent::None
            }
            DhcpPhase::Fallback { retry_at, .. } => {
                if now >= retry_at {
                    let ifn = self.ifname.clone();
                    let mac = self.mac;
                    self.start_discover(&ifn, mac);
                }
                DhcpEvent::None
            }
        }
    }
}

/// Synchronous one-shot apply helper (used as fallback if `NetworkManager.service` is stopped).
pub fn apply_network_config(cfg: &DesktopConfig) -> Result<String, String> {
    let mut nl = NlClient::open()?;
    let links = nl.dump_links()?;
    let Some(primary) = select_primary_link(&links).cloned() else {
        return Err("未检测到可用有线网卡".to_string());
    };
    let prof = ConnectionProfile::from_desktop_config(&primary.name, cfg);

    let (existing_v4, _) = nl.dump_addrs().unwrap_or_default();
    for addr in existing_v4.iter().filter(|a| a.ifindex == primary.ifindex) {
        let _ = nl.del_ipv4_addr(primary.ifindex, addr.addr, addr.prefixlen);
    }
    let _ = nl.del_default_route4(primary.ifindex);

    if !cfg.net_enabled {
        nl.set_link_up(primary.ifindex, false)?;
        write_runtime_files(
            &primary.name,
            DeviceState::Unavailable,
            &prof.id,
            None,
            "none",
            None,
            0,
            "已禁用",
        );
        return Ok(format!("{} 已关闭", primary.name));
    }

    nl.set_link_up(primary.ifindex, true)?;
    for _ in 0..8 {
        if let Ok(cur_links) = nl.dump_links() {
            if cur_links
                .iter()
                .any(|l| l.ifindex == primary.ifindex && l.flags & IFF_UP != 0 && l.carrier)
            {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }

    match cfg.net_mode {
        NetMode::Dhcp => {
            let mut engine = DhcpEngine::new();
            engine.start_discover(&primary.name, primary.mac);
            let deadline = Instant::now() + Duration::from_millis(2100);
            while Instant::now() < deadline {
                let mut readable = false;
                if let Some(fd) = engine.as_raw_fd() {
                    let mut pfd = libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: Valid single `pollfd`.
                    if unsafe { libc::poll(&mut pfd, 1, 100) } > 0 {
                        readable = true;
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(50));
                }
                match engine.step(readable) {
                    DhcpEvent::LeaseAcquired { lease, epoch, .. } => {
                        nl.add_ipv4_addr(primary.ifindex, lease.ip, lease.prefixlen)?;
                        if let Some(gw) = lease.gateway {
                            let _ = nl.replace_default_route4(
                                primary.ifindex,
                                gw,
                                Some(lease.ip),
                                RTPROT_DHCP,
                            );
                        }
                        let note = format!("DHCP 自动获取 (租期 {}s)", lease.lease_secs);
                        write_runtime_files(
                            &primary.name,
                            DeviceState::Activated,
                            &prof.id,
                            lease.dns,
                            "bound",
                            Some(&lease),
                            epoch,
                            &note,
                        );
                        return Ok(format!(
                            "已通过 DHCP 配置 {}：{}/{}",
                            primary.name, lease.ip, lease.prefixlen
                        ));
                    }
                    DhcpEvent::FallbackApplied { lease } => {
                        nl.add_ipv4_addr(primary.ifindex, lease.ip, lease.prefixlen)?;
                        if let Some(gw) = lease.gateway {
                            let _ = nl.replace_default_route4(
                                primary.ifindex,
                                gw,
                                Some(lease.ip),
                                RTPROT_BOOT,
                            );
                        }
                        let note = "DHCP (回退默认地址)";
                        write_runtime_files(
                            &primary.name,
                            DeviceState::Activated,
                            &prof.id,
                            lease.dns,
                            "fallback",
                            Some(&lease),
                            0,
                            note,
                        );
                        return Ok(format!(
                            "DHCP 未响应，已应用默认配置 {}/{}",
                            lease.ip, lease.prefixlen
                        ));
                    }
                    _ => {}
                }
            }
            Err("DHCP 配置超时".to_string())
        }
        NetMode::Static => {
            let (ip, prefix, gw, dns) = validate_static_cfg(cfg)?;
            nl.add_ipv4_addr(primary.ifindex, ip, prefix)?;
            if let Some(gateway) = gw {
                nl.replace_default_route4(primary.ifindex, gateway, Some(ip), RTPROT_STATIC)?;
            }
            write_runtime_files(
                &primary.name,
                DeviceState::Activated,
                &prof.id,
                dns,
                "none",
                None,
                0,
                "手动静态配置",
            );
            Ok(format!("已应用静态配置：{ip}/{prefix}"))
        }
    }
}

fn validate_static_cfg(
    cfg: &DesktopConfig,
) -> Result<(Ipv4Addr, u8, Option<Ipv4Addr>, Option<Ipv4Addr>), String> {
    let ip = cfg
        .net_address
        .trim()
        .parse::<Ipv4Addr>()
        .map_err(|_| "静态 IPv4 地址格式无效".to_string())?;
    let prefix = u8::try_from(cfg.net_prefixlen.clamp(1, 32)).unwrap_or(24);
    let gw_str = cfg.net_gateway.trim();
    let gw = if gw_str.is_empty() {
        None
    } else {
        Some(
            gw_str
                .parse::<Ipv4Addr>()
                .map_err(|_| "默认网关地址格式无效".to_string())?,
        )
    };
    let dns_str = cfg.net_dns.trim();
    let dns = if dns_str.is_empty() {
        None
    } else {
        Some(
            dns_str
                .parse::<Ipv4Addr>()
                .map_err(|_| "DNS 服务器地址格式无效".to_string())?,
        )
    };
    Ok((ip, prefix, gw, dns))
}

/// Send an IPC command to `/run/NetworkManager/nm.sock` and collect response lines before `END`.
pub fn nm_request(cmd: &str) -> Result<Vec<String>, String> {
    let mut stream = UnixStream::connect(NM_SOCKET_PATH)
        .map_err(|e| format!("无法连接 NetworkManager 服务 ({NM_SOCKET_PATH}): {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(4)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    stream
        .write_all(format!("{}\n", cmd.trim()).as_bytes())
        .map_err(|e| format!("发送 NetworkManager 指令失败: {e}"))?;
    let _ = stream.flush();

    let reader = BufReader::new(stream);
    let mut lines = Vec::new();
    for line in reader.lines() {
        let l = line.map_err(|e| format!("读取 NetworkManager 响应失败: {e}"))?;
        if l == "END" {
            break;
        }
        lines.push(l);
    }
    if let Some(first) = lines.first() {
        if let Some(err) = first.strip_prefix("ERR ") {
            return Err(err.to_string());
        }
    }
    Ok(lines)
}

fn parse_ok_message(lines: &[String], fallback: &str) -> String {
    lines
        .first()
        .and_then(|s| s.strip_prefix("OK "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Request `NetworkManager.service` to apply `cfg` immediately over `/run/NetworkManager/nm.sock`.
pub fn request_apply(cfg: &DesktopConfig) -> Result<String, String> {
    validate_if_static(cfg)?;
    match nm_request("APPLY") {
        Ok(lines) => Ok(parse_ok_message(&lines, "NetworkManager 已开始应用配置")),
        Err(err) => {
            // SAFETY: `geteuid` has no preconditions.
            if unsafe { libc::geteuid() } == 0 {
                apply_network_config(cfg)
            } else {
                Err(err)
            }
        }
    }
}

fn validate_if_static(cfg: &DesktopConfig) -> Result<(), String> {
    if cfg.net_enabled && cfg.net_mode == NetMode::Static {
        validate_static_cfg(cfg).map(|_| ())
    } else {
        Ok(())
    }
}

/// Request `NetworkManager.service` to renew or re-discover DHCPv4 lease on `ifname`.
pub fn request_renew(ifname: Option<&str>) -> Result<String, String> {
    let cmd = match ifname.filter(|s| !s.is_empty()) {
        Some(name) => format!("RENEW {name}"),
        None => "RENEW".to_string(),
    };
    let lines = nm_request(&cmd)?;
    Ok(parse_ok_message(&lines, "已发起 DHCP 续租"))
}

/// Request `NetworkManager.service` to connect or disconnect `ifname`.
pub fn request_connect(ifname: Option<&str>, connect: bool) -> Result<String, String> {
    let verb = if connect { "CONNECT" } else { "DISCONNECT" };
    let cmd = match ifname.filter(|s| !s.is_empty()) {
        Some(name) => format!("{verb} {name}"),
        None => verb.to_string(),
    };
    let lines = nm_request(&cmd)?;
    Ok(parse_ok_message(
        &lines,
        if connect {
            "已激活网卡"
        } else {
            "已断开网卡"
        },
    ))
}

/// Send a single ICMPv4 echo request to `target` and return the measured RTT in milliseconds.
pub fn ping_once(target: Ipv4Addr, timeout_ms: i32) -> Result<f64, String> {
    // SAFETY: Standard `AF_INET` `IPPROTO_ICMP` `SOCK_DGRAM` (with `SOCK_RAW` fallback) send/recv.
    unsafe {
        let mut fd = libc::socket(
            libc::AF_INET,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            libc::IPPROTO_ICMP,
        );
        let is_raw = if fd < 0 {
            fd = libc::socket(
                libc::AF_INET,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::IPPROTO_ICMP,
            );
            if fd < 0 {
                return Err("无法创建 ICMP 探测套接字".to_string());
            }
            true
        } else {
            false
        };

        let ident = (std::process::id() as u16) ^ 0x5a39;
        let seq = 1u16;
        let mut pkt = [0u8; 24];
        pkt[0] = 8; // ICMP_ECHO
        pkt[1] = 0;
        pkt[4..6].copy_from_slice(&ident.to_be_bytes());
        pkt[6..8].copy_from_slice(&seq.to_be_bytes());
        pkt[8..16].copy_from_slice(b"SpaceOS9");
        let csum = icmp_checksum(&pkt);
        pkt[2..4].copy_from_slice(&csum.to_be_bytes());

        let mut dst: libc::sockaddr_in = mem::zeroed();
        dst.sin_family = libc::AF_INET as libc::sa_family_t;
        dst.sin_port = 0;
        dst.sin_addr.s_addr = u32::from_ne_bytes(target.octets());

        let start = Instant::now();
        let rc = libc::sendto(
            fd,
            pkt.as_ptr() as *const libc::c_void,
            pkt.len(),
            0,
            &dst as *const libc::sockaddr_in as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        );
        if rc < 0 {
            libc::close(fd);
            return Err("发送 ICMP 探测报文失败（网络不可达）".to_string());
        }

        let deadline = start + Duration::from_millis(timeout_ms.max(100) as u64);
        let mut buf = [0u8; 256];
        while Instant::now() < deadline {
            let rem = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, timeout_ms.max(100) as u128) as i32;
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if libc::poll(&mut pfd, 1, rem) <= 0 {
                break;
            }
            let mut src: libc::sockaddr_in = mem::zeroed();
            let mut slen = mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            let n = libc::recvfrom(
                fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                libc::MSG_DONTWAIT,
                &mut src as *mut libc::sockaddr_in as *mut libc::sockaddr,
                &mut slen,
            );
            if n <= 0 {
                continue;
            }
            let slice = &buf[..n as usize];
            let icmp = if is_raw && slice.len() >= 28 && (slice[0] >> 4) == 4 {
                let ihl = ((slice[0] & 0x0f) as usize) * 4;
                if ihl + 8 <= slice.len() {
                    &slice[ihl..]
                } else {
                    continue;
                }
            } else {
                slice
            };
            if icmp.len() >= 8 && icmp[0] == 0 && icmp[1] == 0 {
                let rseq = u16::from_be_bytes([icmp[6], icmp[7]]);
                if (!is_raw || u16::from_be_bytes([icmp[4], icmp[5]]) == ident) && rseq == seq {
                    let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;
                    libc::close(fd);
                    return Ok(rtt_ms);
                }
            }
        }

        libc::close(fd);
        Err(format!("探测 {target} 超时（无响应）"))
    }
}

fn icmp_checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(&rem) = chunks.remainder().first() {
        sum += (rem as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

// ============================================================================
// Standalone NetworkManager Daemon Engine (`/usr/bin/NetworkManager`)
// ============================================================================

struct ManagedDevice {
    ifindex: u32,
    ifname: String,
    up: bool,
    carrier: bool,
    carrier_lost_since: Option<Instant>,
    state: DeviceState,
    profile: ConnectionProfile,
    dns: Option<Ipv4Addr>,
    mode_note: String,
}

/// Standalone event-driven `NetworkManager` daemon server.
pub struct NetworkManagerServer {
    socket_path: PathBuf,
    profiles_dir: PathBuf,
    listener: UnixListener,
    config: DesktopConfig,
    last_fp: (bool, NetMode, String, i32, String, String, i64),
    watch: Option<UxConfWatch>,
    nl_event_fd: Option<i32>,
    primary: Option<ManagedDevice>,
    dhcp: DhcpEngine,
}

impl Drop for NetworkManagerServer {
    fn drop(&mut self) {
        if let Some(fd) = self.nl_event_fd {
            // SAFETY: Owned Netlink event socket closed on server teardown.
            unsafe {
                libc::close(fd);
            }
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn net_config_fingerprint(
    cfg: &DesktopConfig,
) -> (bool, NetMode, String, i32, String, String, i64) {
    (
        cfg.net_enabled,
        cfg.net_mode,
        cfg.net_address.clone(),
        cfg.net_prefixlen,
        cfg.net_gateway.clone(),
        cfg.net_dns.clone(),
        cfg.net_apply_serial,
    )
}

impl NetworkManagerServer {
    pub fn bind(
        socket_override: Option<PathBuf>,
        profiles_override: Option<PathBuf>,
    ) -> Result<Self, String> {
        let socket_path = socket_override.unwrap_or_else(|| PathBuf::from(NM_SOCKET_PATH));
        let profiles_dir = profiles_override.unwrap_or_else(|| PathBuf::from(NM_PROFILES_DIR));

        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o755));
        }
        let _ = fs::create_dir_all(&profiles_dir);
        let _ = fs::set_permissions(&profiles_dir, fs::Permissions::from_mode(0o755));

        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| format!("failed to bind {}: {e}", socket_path.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("failed to set nonblocking listener: {e}"))?;
        let _ = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o666));

        let mut ux_client = None;
        for _ in 0..20 {
            if let Ok(c) = UxConfClient::connect() {
                ux_client = Some(c);
                break;
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        let config = ux_client
            .as_mut()
            .map(UxConfClient::load_desktop_config)
            .unwrap_or_default();
        let last_fp = net_config_fingerprint(&config);
        let watch = UxConfWatch::subscribe(Some("network")).ok();
        let nl_event_fd = open_event_socket();

        Ok(Self {
            socket_path,
            profiles_dir,
            listener,
            config,
            last_fp,
            watch,
            nl_event_fd,
            primary: None,
            dhcp: DhcpEngine::new(),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn ensure_loopback_up(&self) {
        if let Ok(mut nl) = NlClient::open() {
            if let Ok(links) = nl.dump_links() {
                if let Some(lo) = links
                    .iter()
                    .find(|l| l.flags & IFF_LOOPBACK != 0 || l.name == "lo")
                {
                    if lo.flags & IFF_UP == 0 {
                        let _ = nl.set_link_up(lo.ifindex, true);
                    }
                }
            }
        }
    }

    fn save_keyfile(&self, profile: &ConnectionProfile) {
        let path = self
            .profiles_dir
            .join(format!("{}.nmconnection", profile.ifname));
        let tmp = self
            .profiles_dir
            .join(format!(".{}.nmconnection.tmp", profile.ifname));
        if fs::write(&tmp, profile.encode_keyfile()).is_ok() {
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
            let _ = fs::rename(&tmp, &path);
        }
    }

    fn load_or_init_keyfile(&self, ifname: &str) -> ConnectionProfile {
        let path = self.profiles_dir.join(format!("{ifname}.nmconnection"));
        let prof = ConnectionProfile::from_desktop_config(ifname, &self.config);
        if !path.exists() {
            self.save_keyfile(&prof);
        }
        prof
    }

    fn sync_status_disk(&self) {
        if let Some(dev) = &self.primary {
            let (lease, epoch) = match self.dhcp.active_lease() {
                Some((l, ep)) => (Some(l), ep),
                None => (None, 0),
            };
            write_runtime_files(
                &dev.ifname,
                dev.state,
                &dev.profile.id,
                dev.dns,
                self.dhcp.state_name(),
                lease,
                epoch,
                &dev.mode_note,
            );
        } else {
            write_runtime_files(
                "",
                DeviceState::Unavailable,
                "",
                None,
                "none",
                None,
                0,
                "未检测到网卡",
            );
        }
    }

    fn reload_uxconf(&mut self) {
        if let Ok(mut client) = UxConfClient::connect() {
            self.config = client.load_desktop_config();
            self.last_fp = net_config_fingerprint(&self.config);
        }
    }

    fn set_uxconf_enabled(&mut self, enabled: bool) {
        self.config.net_enabled = enabled;
        self.last_fp = net_config_fingerprint(&self.config);
        if let Ok(mut client) = UxConfClient::connect() {
            let _ = client.set("network", "/enabled", Value::Bool(enabled));
        }
    }

    fn reconcile_links(&mut self, force_reapply: bool) -> Result<String, String> {
        let mut nl = NlClient::open()?;
        let links = nl.dump_links()?;
        let Some(link) = select_primary_link(&links).cloned() else {
            self.dhcp.send_release_and_stop();
            self.primary = None;
            self.sync_status_disk();
            return Err("未检测到可用有线网卡".to_string());
        };

        let is_new_if = self
            .primary
            .as_ref()
            .is_none_or(|d| d.ifindex != link.ifindex);

        if is_new_if {
            let prof = self.load_or_init_keyfile(&link.name);
            eprintln!(
                "NetworkManager: managing ethernet device {} (ifindex={}, mac={})",
                link.name,
                link.ifindex,
                format_mac(&link.mac, link.has_mac)
            );
            self.primary = Some(ManagedDevice {
                ifindex: link.ifindex,
                ifname: link.name.clone(),
                up: link.flags & IFF_UP != 0,
                carrier: link.carrier,
                carrier_lost_since: None,
                state: DeviceState::Disconnected,
                profile: prof,
                dns: None,
                mode_note: String::new(),
            });
        }

        if force_reapply || is_new_if {
            return self.activate_or_deactivate(&mut nl, &link);
        }

        // Carrier state machine with 1500 ms debounce grace window (`IgnoreCarrierLoss`).
        if let Some(dev) = self.primary.as_mut() {
            dev.up = link.flags & IFF_UP != 0;
            let prev_carrier = dev.carrier;
            dev.carrier = link.carrier;

            if !self.config.net_enabled {
                if dev.state != DeviceState::Unavailable {
                    dev.state = DeviceState::Unavailable;
                    self.sync_status_disk();
                }
                return Ok("已禁用".to_string());
            }

            if !link.carrier {
                if dev.carrier_lost_since.is_none() {
                    dev.carrier_lost_since = Some(Instant::now());
                } else if dev
                    .carrier_lost_since
                    .is_some_and(|ts| ts.elapsed() >= Duration::from_millis(CARRIER_GRACE_MS))
                    && dev.state != DeviceState::Unavailable
                {
                    eprintln!(
                        "NetworkManager: {}: carrier lost, state -> unavailable",
                        dev.ifname
                    );
                    dev.state = DeviceState::Unavailable;
                    dev.mode_note = "网线未连接 (无载波)".to_string();
                    self.dhcp.close_socket();
                    self.sync_status_disk();
                }
                return Ok("等待载波".to_string());
            }

            // Carrier is UP!
            dev.carrier_lost_since = None;
            if !prev_carrier
                || matches!(
                    dev.state,
                    DeviceState::Unavailable | DeviceState::Disconnected | DeviceState::Failed
                )
            {
                return self.activate_or_deactivate(&mut nl, &link);
            }
        }

        Ok("就绪".to_string())
    }

    fn activate_or_deactivate(
        &mut self,
        nl: &mut NlClient,
        link: &LinkInfo,
    ) -> Result<String, String> {
        let prof = ConnectionProfile::from_desktop_config(&link.name, &self.config);
        self.save_keyfile(&prof);

        self.dhcp.send_release_and_stop();
        let (existing_v4, _) = nl.dump_addrs().unwrap_or_default();
        for addr in existing_v4.iter().filter(|a| a.ifindex == link.ifindex) {
            let _ = nl.del_ipv4_addr(link.ifindex, addr.addr, addr.prefixlen);
        }
        let _ = nl.del_default_route4(link.ifindex);

        if !self.config.net_enabled {
            if let Some(dev) = self.primary.as_mut() {
                dev.state = DeviceState::Deactivating;
            }
            nl.set_link_up(link.ifindex, false)?;
            if let Some(dev) = self.primary.as_mut() {
                dev.up = false;
                dev.carrier = false;
                dev.profile = prof;
                dev.state = DeviceState::Unavailable;
                dev.dns = None;
                dev.mode_note = "已禁用".to_string();
            }
            self.sync_status_disk();
            eprintln!(
                "NetworkManager: {}: device disabled (state -> unavailable)",
                link.name
            );
            return Ok(format!("{} 已禁用", link.name));
        }

        // Transition: Prepare (40) -> Config (50) -> IpConfig (70)
        if let Some(dev) = self.primary.as_mut() {
            dev.profile = prof;
            dev.state = DeviceState::Prepare;
        }
        nl.set_link_up(link.ifindex, true)?;

        let mut carrier_up = link.carrier;
        for _ in 0..8 {
            if let Ok(cur_links) = nl.dump_links() {
                if let Some(cur) = cur_links.iter().find(|l| l.ifindex == link.ifindex) {
                    if cur.flags & IFF_UP != 0 && cur.carrier {
                        carrier_up = true;
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        match self.config.net_mode {
            NetMode::Dhcp => {
                if let Some(dev) = self.primary.as_mut() {
                    dev.up = true;
                    dev.carrier = carrier_up;
                    dev.carrier_lost_since = None;
                    dev.state = DeviceState::IpConfig;
                    dev.mode_note = "正在通过 DHCP 获取地址...".to_string();
                }
                self.dhcp.start_discover(&link.name, link.mac);
                // Pump fast initial DHCP exchange (up to ~1.4s if server replies right away, as QEMU SLIRP does in <5ms)
                // so CLI `nmcli` / GUI `Apply` immediately sees the bound IP when fast!
                let quick_wait = Instant::now() + Duration::from_millis(1450);
                while Instant::now() < quick_wait {
                    let mut readable = false;
                    if let Some(fd) = self.dhcp.as_raw_fd() {
                        let mut pfd = libc::pollfd {
                            fd,
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        // SAFETY: Valid single `pollfd`.
                        if unsafe { libc::poll(&mut pfd, 1, 60) } > 0 {
                            readable = true;
                        }
                    } else {
                        break;
                    }
                    let ev = self.dhcp.step(readable);
                    if self.handle_dhcp_event(nl, ev)? {
                        if let Some(dev) = &self.primary {
                            return Ok(format!("{} 已连接 ({})", dev.ifname, dev.mode_note));
                        }
                        break;
                    }
                }
                self.sync_status_disk();
                Ok(format!("{} 正在后台完成 DHCP 配置", link.name))
            }
            NetMode::Static => {
                if let Some(dev) = self.primary.as_mut() {
                    dev.up = true;
                    dev.carrier = carrier_up;
                    dev.carrier_lost_since = None;
                    dev.state = DeviceState::IpConfig;
                }
                match validate_static_cfg(&self.config) {
                    Ok((ip, prefix, gw, dns)) => {
                        nl.add_ipv4_addr(link.ifindex, ip, prefix)?;
                        if let Some(gateway) = gw {
                            nl.replace_default_route4(
                                link.ifindex,
                                gateway,
                                Some(ip),
                                RTPROT_STATIC,
                            )?;
                        }
                        if let Some(dev) = self.primary.as_mut() {
                            dev.state = DeviceState::Activated;
                            dev.dns = dns;
                            dev.mode_note = "手动静态配置".to_string();
                        }
                        self.sync_status_disk();
                        eprintln!(
                            "NetworkManager: {}: activated static {ip}/{prefix}",
                            link.name
                        );
                        Ok(format!("已应用静态配置：{ip}/{prefix}"))
                    }
                    Err(err) => {
                        if let Some(dev) = self.primary.as_mut() {
                            dev.state = DeviceState::Failed;
                            dev.mode_note = format!("静态配置无效: {err}");
                        }
                        self.sync_status_disk();
                        Err(err)
                    }
                }
            }
        }
    }

    fn handle_dhcp_event(&mut self, nl: &mut NlClient, ev: DhcpEvent) -> Result<bool, String> {
        match ev {
            DhcpEvent::None => Ok(false),
            DhcpEvent::LeaseAcquired {
                lease,
                epoch: _,
                renewed,
            } => {
                if let Some(dev) = self.primary.as_mut() {
                    nl.add_ipv4_addr(dev.ifindex, lease.ip, lease.prefixlen)?;
                    if let Some(gw) = lease.gateway {
                        let _ =
                            nl.replace_default_route4(dev.ifindex, gw, Some(lease.ip), RTPROT_DHCP);
                    }
                    dev.state = DeviceState::Activated;
                    dev.dns = lease.dns;
                    dev.mode_note = format!(
                        "DHCP 自动获取 (租期 {}s, T1={}s, T2={}s)",
                        lease.lease_secs, lease.t1_secs, lease.t2_secs
                    );
                    eprintln!(
                        "NetworkManager: {}: DHCPv4 {} {}/{} gw={:?} dns={:?} lease={}s",
                        dev.ifname,
                        if renewed { "renewed" } else { "bound" },
                        lease.ip,
                        lease.prefixlen,
                        lease.gateway,
                        lease.dns,
                        lease.lease_secs
                    );
                }
                self.sync_status_disk();
                Ok(true)
            }
            DhcpEvent::FallbackApplied { lease } => {
                if let Some(dev) = self.primary.as_mut() {
                    nl.add_ipv4_addr(dev.ifindex, lease.ip, lease.prefixlen)?;
                    if let Some(gw) = lease.gateway {
                        let _ =
                            nl.replace_default_route4(dev.ifindex, gw, Some(lease.ip), RTPROT_BOOT);
                    }
                    dev.state = DeviceState::Activated;
                    dev.dns = lease.dns;
                    dev.mode_note = "DHCP (SLIRP 默认地址)".to_string();
                    eprintln!(
                        "NetworkManager: {}: no DHCP offer, applied fallback {}/{}",
                        dev.ifname, lease.ip, lease.prefixlen
                    );
                }
                self.sync_status_disk();
                Ok(true)
            }
            DhcpEvent::LeaseExpired => {
                if let Some(dev) = self.primary.as_mut() {
                    let (existing_v4, _) = nl.dump_addrs().unwrap_or_default();
                    for addr in existing_v4.iter().filter(|a| a.ifindex == dev.ifindex) {
                        let _ = nl.del_ipv4_addr(dev.ifindex, addr.addr, addr.prefixlen);
                    }
                    let _ = nl.del_default_route4(dev.ifindex);
                    dev.state = DeviceState::IpConfig;
                    dev.mode_note = "DHCP 租约到期，正在重新获取...".to_string();
                }
                self.sync_status_disk();
                Ok(false)
            }
        }
    }

    fn handle_control_client(&mut self, mut stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(1000)));
        let mut buf = [0u8; 512];
        let Ok(n) = stream.read(&mut buf) else {
            return;
        };
        if n == 0 {
            return;
        }
        let line = String::from_utf8_lossy(&buf[..n]);
        let cmd_line = line.lines().next().unwrap_or("").trim();
        let mut parts = cmd_line.split_whitespace();
        let verb = parts.next().unwrap_or("").to_ascii_uppercase();
        let arg = parts.next();

        let response = self.dispatch_command(&verb, arg);
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }

    fn dispatch_command(&mut self, verb: &str, arg: Option<&str>) -> String {
        match verb {
            "PING" => "OK PONG\nEND\n".to_string(),
            "STATUS" => {
                let st = query_status();
                let connectivity = if st.connected() && st.gateway.is_some() {
                    "full"
                } else if st.connected() {
                    "limited"
                } else {
                    "none"
                };
                let mut out = String::with_capacity(512);
                out.push_str("OK\n");
                out.push_str(&format!("state={}\n", st.device_state.code()));
                out.push_str(&format!("state_name={}\n", st.device_state.as_str()));
                out.push_str(&format!("state_zh={}\n", st.device_state.label_zh()));
                out.push_str(&format!("connectivity={connectivity}\n"));
                out.push_str(&format!("ifindex={}\n", st.ifindex));
                out.push_str(&format!("ifname={}\n", st.ifname));
                out.push_str(&format!("profile={}\n", st.profile_name));
                out.push_str(&format!("mac={}\n", st.mac));
                out.push_str(&format!("mtu={}\n", st.mtu));
                out.push_str(&format!("up={}\n", st.up));
                out.push_str(&format!("carrier={}\n", st.carrier));
                if let Some((ip, p)) = st.ipv4 {
                    out.push_str(&format!("ipv4={ip}/{p}\n"));
                }
                if let Some((ip6, p6)) = st.ipv6_ll {
                    out.push_str(&format!("ipv6={ip6}/{p6}\n"));
                }
                if let Some(gw) = st.gateway {
                    out.push_str(&format!("gateway={gw}\n"));
                }
                if let Some(dns) = st.dns {
                    out.push_str(&format!("dns={dns}\n"));
                }
                out.push_str(&format!("dhcp_state={}\n", st.dhcp_state));
                out.push_str(&format!("lease_secs={}\n", st.lease_secs));
                out.push_str(&format!(
                    "lease_remaining_secs={}\n",
                    st.lease_remaining_secs
                ));
                out.push_str(&format!("t1_secs={}\n", st.t1_secs));
                out.push_str(&format!("t2_secs={}\n", st.t2_secs));
                out.push_str(&format!("mode_note={}\n", st.mode_note));
                out.push_str("END\n");
                out
            }
            "DEVICES" => {
                let mut out = String::from("OK\n");
                if let Ok(mut nl) = NlClient::open() {
                    let links = nl.dump_links().unwrap_or_default();
                    let (addrs4, _) = nl.dump_addrs().unwrap_or_default();
                    for l in links {
                        let is_lo = l.flags & IFF_LOOPBACK != 0 || l.name == "lo";
                        let dtype = if is_lo { "loopback" } else { "ethernet" };
                        let (dstate, conn_name) = if is_lo {
                            (
                                if l.flags & IFF_UP != 0 {
                                    "connected (externally)"
                                } else {
                                    "unmanaged"
                                },
                                "lo".to_string(),
                            )
                        } else if let Some(dev) =
                            self.primary.as_ref().filter(|d| d.ifindex == l.ifindex)
                        {
                            (
                                dev.state.as_str(),
                                if dev.state == DeviceState::Activated {
                                    dev.profile.id.clone()
                                } else {
                                    "--".to_string()
                                },
                            )
                        } else if l.flags & IFF_UP != 0 {
                            ("disconnected", "--".to_string())
                        } else {
                            ("unavailable", "--".to_string())
                        };
                        let v4 = addrs4
                            .iter()
                            .find(|a| a.ifindex == l.ifindex)
                            .map(|a| format!("{}/{}", a.addr, a.prefixlen))
                            .unwrap_or_else(|| "--".to_string());
                        out.push_str(&format!(
                            "DEV\t{}\t{}\t{}\t{}\t{}\n",
                            l.name, dtype, dstate, conn_name, v4
                        ));
                    }
                }
                out.push_str("END\n");
                out
            }
            "SHOW" => {
                let st = query_status();
                if let Some(req_if) = arg {
                    if !req_if.is_empty() && req_if != st.ifname && req_if != "lo" {
                        return format!("ERR 未找到设备 '{req_if}'\nEND\n");
                    }
                }
                let mut out = String::from("OK\n");
                if arg == Some("lo") {
                    out.push_str("GENERAL.DEVICE:                         lo\n");
                    out.push_str("GENERAL.TYPE:                           loopback\n");
                    out.push_str("GENERAL.STATE:                          10 (unmanaged)\n");
                    out.push_str("IP4.ADDRESS[1]:                         127.0.0.1/8\n");
                    out.push_str("IP6.ADDRESS[1]:                         ::1/128\n");
                    out.push_str("END\n");
                    return out;
                }
                if !st.has_device {
                    return "ERR 未检测到以太网设备\nEND\n".to_string();
                }
                out.push_str(&format!(
                    "GENERAL.DEVICE:                         {}\n",
                    st.ifname
                ));
                out.push_str("GENERAL.TYPE:                           ethernet\n");
                out.push_str(&format!(
                    "GENERAL.HWADDR:                         {}\n",
                    st.mac
                ));
                out.push_str(&format!(
                    "GENERAL.MTU:                            {}\n",
                    st.mtu
                ));
                out.push_str(&format!(
                    "GENERAL.STATE:                          {} ({})\n",
                    st.device_state.code(),
                    st.device_state.as_str()
                ));
                out.push_str(&format!(
                    "GENERAL.CONNECTION:                     {}\n",
                    if st.device_state == DeviceState::Activated {
                        st.profile_name.as_str()
                    } else {
                        "--"
                    }
                ));
                out.push_str(&format!(
                    "WIRED-PROPERTIES.CARRIER:               {}\n",
                    if st.carrier { "on" } else { "off" }
                ));
                for (i, (ip, plen)) in st.ipv4_all.iter().enumerate() {
                    out.push_str(&format!(
                        "IP4.ADDRESS[{}]:                         {ip}/{plen}\n",
                        i + 1
                    ));
                }
                if let Some(gw) = st.gateway {
                    out.push_str(&format!("IP4.GATEWAY:                            {gw}\n"));
                }
                if let Some(dns) = st.dns {
                    out.push_str(&format!("IP4.DNS[1]:                             {dns}\n"));
                }
                if st.lease_secs > 0 {
                    out.push_str(&format!(
                        "DHCP4.OPTION[lease_time]:               {}s (剩余 {}s, T1={}s, T2={}s)\n",
                        st.lease_secs, st.lease_remaining_secs, st.t1_secs, st.t2_secs
                    ));
                    if let Some(sid) = st.server_id {
                        out.push_str(&format!("DHCP4.OPTION[dhcp_server_identifier]:   {sid}\n"));
                    }
                }
                if let Some((ip6, p6)) = st.ipv6_ll {
                    out.push_str(&format!(
                        "IP6.ADDRESS[1]:                         {ip6}/{p6}\n"
                    ));
                }
                out.push_str(&format!(
                    "STATS.RX:                               {} ({} pkts)\n",
                    NetStatus::format_bytes(st.rx_bytes),
                    st.rx_packets
                ));
                out.push_str(&format!(
                    "STATS.TX:                               {} ({} pkts)\n",
                    NetStatus::format_bytes(st.tx_bytes),
                    st.tx_packets
                ));
                out.push_str("END\n");
                out
            }
            "CONNECTIONS" => {
                let mut out = String::from("OK\n");
                let mut found = false;
                if let Ok(entries) = fs::read_dir(&self.profiles_dir) {
                    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
                    paths.sort();
                    for p in paths {
                        if p.extension().is_some_and(|ext| ext == "nmconnection") {
                            if let Ok(raw) = fs::read_to_string(&p) {
                                let prof = ConnectionProfile::parse_keyfile(&raw, "eth0");
                                let active_dev = self
                                    .primary
                                    .as_ref()
                                    .filter(|d| {
                                        d.ifname == prof.ifname && d.state == DeviceState::Activated
                                    })
                                    .map(|d| d.ifname.as_str())
                                    .unwrap_or("--");
                                out.push_str(&format!(
                                    "CONN\t{}\t{}\t802-3-ethernet\t{}\n",
                                    prof.id, prof.uuid, active_dev
                                ));
                                found = true;
                            }
                        }
                    }
                }
                if !found {
                    if let Some(dev) = &self.primary {
                        out.push_str(&format!(
                            "CONN\t{}\t{}\t802-3-ethernet\t{}\n",
                            dev.profile.id, dev.profile.uuid, dev.ifname
                        ));
                    }
                }
                out.push_str("END\n");
                out
            }
            "APPLY" => {
                self.reload_uxconf();
                match self.reconcile_links(true) {
                    Ok(msg) => format!("OK {msg}\nEND\n"),
                    Err(err) => format!("ERR {err}\nEND\n"),
                }
            }
            "RENEW" => {
                if !self.config.net_enabled {
                    return "ERR 网络已禁用，无法续租\nEND\n".to_string();
                }
                if self.config.net_mode != NetMode::Dhcp {
                    return "ERR 当前为手动静态模式，非 DHCP 模式\nEND\n".to_string();
                }
                self.dhcp.trigger_renew();
                if let Ok(mut nl) = NlClient::open() {
                    let deadline = Instant::now() + Duration::from_millis(1200);
                    while Instant::now() < deadline {
                        let mut readable = false;
                        if let Some(fd) = self.dhcp.as_raw_fd() {
                            let mut pfd = libc::pollfd {
                                fd,
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            // SAFETY: Valid single `pollfd`.
                            if unsafe { libc::poll(&mut pfd, 1, 60) } > 0 {
                                readable = true;
                            }
                        } else {
                            break;
                        }
                        let ev = self.dhcp.step(readable);
                        if self.handle_dhcp_event(&mut nl, ev).unwrap_or(false) {
                            break;
                        }
                    }
                }
                let st = query_status();
                if let Some((ip, p)) = st.ipv4 {
                    format!("OK DHCP 租约已更新：{ip}/{p}\nEND\n")
                } else {
                    "OK 已发送 DHCP 续租请求\nEND\n".to_string()
                }
            }
            "CONNECT" => {
                self.set_uxconf_enabled(true);
                match self.reconcile_links(true) {
                    Ok(msg) => format!("OK {msg}\nEND\n"),
                    Err(err) => format!("ERR {err}\nEND\n"),
                }
            }
            "DISCONNECT" => {
                self.set_uxconf_enabled(false);
                match self.reconcile_links(true) {
                    Ok(msg) => format!("OK {msg}\nEND\n"),
                    Err(err) => format!("ERR {err}\nEND\n"),
                }
            }
            other => format!("ERR 未知指令 '{other}'\nEND\n"),
        }
    }

    pub fn run(&mut self, stop: &AtomicBool) -> Result<(), String> {
        self.ensure_loopback_up();

        // Wait briefly if `systemg` is finishing PCI NIC `.ko` probe in parallel.
        for _ in 0..20 {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if query_status().has_device {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        match self.reconcile_links(true) {
            Ok(msg) => eprintln!("NetworkManager: {msg}"),
            Err(err) => eprintln!("NetworkManager: waiting for Ethernet link ({err})"),
        }

        while !stop.load(Ordering::Relaxed) {
            if self.watch.is_none() {
                self.reload_uxconf();
                self.watch = UxConfWatch::subscribe(Some("network")).ok();
            }

            let mut fds = [
                libc::pollfd {
                    fd: -1,
                    events: 0,
                    revents: 0,
                },
                libc::pollfd {
                    fd: -1,
                    events: 0,
                    revents: 0,
                },
                libc::pollfd {
                    fd: -1,
                    events: 0,
                    revents: 0,
                },
                libc::pollfd {
                    fd: -1,
                    events: 0,
                    revents: 0,
                },
            ];
            let mut nfds = 0usize;

            let srv_idx = nfds;
            fds[nfds] = libc::pollfd {
                fd: self.listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            nfds += 1;

            let ux_idx = self.watch.as_ref().map(|w| {
                let idx = nfds;
                fds[idx] = libc::pollfd {
                    fd: w.as_raw_fd(),
                    events: libc::POLLIN | libc::POLLHUP,
                    revents: 0,
                };
                nfds += 1;
                idx
            });

            let nl_idx = self.nl_event_fd.map(|fd| {
                let idx = nfds;
                fds[idx] = libc::pollfd {
                    fd,
                    events: libc::POLLIN | libc::POLLHUP,
                    revents: 0,
                };
                nfds += 1;
                idx
            });

            let dhcp_idx = self.dhcp.as_raw_fd().map(|fd| {
                let idx = nfds;
                fds[idx] = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                nfds += 1;
                idx
            });

            let mut timeout_ms: i32 = if self.primary.is_none() || self.watch.is_none() {
                1500
            } else {
                5000
            };
            if let Some(dev) = &self.primary {
                if let Some(lost_at) = dev.carrier_lost_since {
                    let grace_rem = Duration::from_millis(CARRIER_GRACE_MS)
                        .saturating_sub(lost_at.elapsed())
                        .as_millis() as i32;
                    timeout_ms = timeout_ms.min(grace_rem.max(50));
                }
            }
            if let Some(dl) = self.dhcp.next_deadline() {
                let rem = dl
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .clamp(20, 5000) as i32;
                timeout_ms = timeout_ms.min(rem);
            }

            // SAFETY: Valid stack array `fds[..nfds]` polled with bounded `timeout_ms`.
            unsafe {
                libc::poll(fds.as_mut_ptr(), nfds as libc::nfds_t, timeout_ms);
            }

            if stop.load(Ordering::Relaxed) {
                break;
            }

            let mask = libc::POLLIN | libc::POLLHUP | libc::POLLERR;

            // 1. Control socket clients (`nmcli`, `spui settings`, etc.)
            if fds[srv_idx].revents & libc::POLLIN != 0 {
                while let Ok((stream, _)) = self.listener.accept() {
                    self.handle_control_client(stream);
                }
            }

            // 2. Live `uxconfd` `"network"` channel updates
            if ux_idx.is_some_and(|i| fds[i].revents & mask != 0) {
                if let Some(w) = self.watch.as_mut() {
                    let (changed, _) = w.poll_into(&mut self.config);
                    if !changed {
                        self.watch = None;
                    }
                }
            }

            // 3. Kernel `NETLINK_ROUTE` link/carrier/address events
            let mut nl_event = false;
            if let (Some(idx), Some(fd)) = (nl_idx, self.nl_event_fd) {
                if fds[idx].revents & mask != 0 {
                    nl_event = drain_events(fd);
                }
            }

            // 4. Non-blocking RFC 2131 DHCPv4 state machine step (packets + T1/T2 timers)
            let dhcp_readable = dhcp_idx.is_some_and(|i| fds[i].revents & libc::POLLIN != 0);
            if self.config.net_enabled && self.config.net_mode == NetMode::Dhcp {
                let ev = self.dhcp.step(dhcp_readable);
                if !matches!(ev, DhcpEvent::None) {
                    if let Ok(mut nl) = NlClient::open() {
                        let _ = self.handle_dhcp_event(&mut nl, ev);
                    }
                }
            }

            // 5. Re-apply if `uxconf` configuration fingerprint changed
            let fp = net_config_fingerprint(&self.config);
            if fp != self.last_fp {
                self.last_fp = fp;
                match self.reconcile_links(true) {
                    Ok(msg) => eprintln!("NetworkManager: {msg}"),
                    Err(err) => eprintln!("NetworkManager: apply failed: {err}"),
                }
                continue;
            }

            // 6. Reconcile hotplug NIC or carrier grace window expiry
            let grace_due = self
                .primary
                .as_ref()
                .and_then(|d| d.carrier_lost_since)
                .is_some_and(|ts| ts.elapsed() >= Duration::from_millis(CARRIER_GRACE_MS));
            if nl_event || self.primary.is_none() || grace_due {
                let _ = self.reconcile_links(false);
            }
        }

        eprintln!("NetworkManager: shutting down cleanly");
        Ok(())
    }
}

/// Parse key=value lines returned by `nm_request("STATUS")` into a map.
pub fn parse_kv_lines(lines: &[String]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_and_netmask_parser_accepts_cidr_and_dotted_masks() {
        assert_eq!(parse_prefix_or_netmask("24"), Some(24));
        assert_eq!(parse_prefix_or_netmask("/16"), Some(16));
        assert_eq!(parse_prefix_or_netmask("255.255.255.0"), Some(24));
        assert_eq!(parse_prefix_or_netmask("255.255.254.0"), Some(23));
        assert_eq!(parse_prefix_or_netmask("255.255.255.255"), Some(32));
        assert_eq!(parse_prefix_or_netmask("0.0.0.0"), None);
        assert_eq!(parse_prefix_or_netmask("255.0.255.0"), None);
        assert_eq!(parse_prefix_or_netmask("33"), None);
    }

    #[test]
    fn dhcp_packet_and_reply_roundtrip_with_t1_t2_options() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let xid = 0xdead_beef;
        let discover =
            build_dhcp_packet(xid, &mac, DHCP_DISCOVER, None, None, None, true, "spaceos");
        assert!(discover.len() >= 300);
        assert_eq!(discover[0], 1);
        assert_eq!(&discover[28..34], &mac);
        assert_eq!(&discover[236..240], &[99, 130, 83, 99]);

        // Craft a synthetic DHCPACK with Option 1 (mask), 3 (router), 6 (dns), 51 (lease), 54 (sid), 58 (T1), 59 (T2).
        let mut ack = vec![0u8; 300];
        ack[0] = 2; // BOOTREPLY
        ack[4..8].copy_from_slice(&xid.to_be_bytes());
        ack[16..20].copy_from_slice(&[10, 0, 2, 15]);
        ack[28..34].copy_from_slice(&mac);
        ack[236..240].copy_from_slice(&[99, 130, 83, 99]);
        let opts: &[u8] = &[
            53, 1, DHCP_ACK, // Message Type = ACK
            1, 4, 255, 255, 255, 0, // Subnet mask /24
            3, 4, 10, 0, 2, 2, // Router
            6, 4, 10, 0, 2, 3, // DNS
            54, 4, 10, 0, 2, 2, // Server ID
            51, 4, 0, 1, 0x51, 0x80, // Lease = 86400s
            58, 4, 0, 0, 0xa8, 0xc0, // T1 = 43200s
            59, 4, 0, 1, 0x27, 0x50, // T2 = 75600s
            255,
        ];
        ack[240..240 + opts.len()].copy_from_slice(opts);

        let parsed = parse_dhcp_reply(&ack, xid, &mac).expect("valid DHCPACK");
        assert_eq!(parsed.msg_type, DHCP_ACK);
        assert_eq!(parsed.yiaddr, Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(parsed.prefixlen, 24);
        assert_eq!(parsed.gateway, Some(Ipv4Addr::new(10, 0, 2, 2)));
        assert_eq!(parsed.dns, Some(Ipv4Addr::new(10, 0, 2, 3)));
        assert_eq!(parsed.lease_secs, 86400);
        let lease = reply_to_lease(&parsed, None);
        assert_eq!(lease.t1_secs, 43200);
        assert_eq!(lease.t2_secs, 75600);
    }

    #[test]
    fn keyfile_profile_roundtrips_static_and_dhcp_modes() {
        let mut cfg = DesktopConfig::default();
        cfg.net_mode = NetMode::Static;
        cfg.net_address = "192.168.10.42".to_string();
        cfg.net_prefixlen = 24;
        cfg.net_gateway = "192.168.10.1".to_string();
        cfg.net_dns = "1.1.1.1".to_string();

        let prof = ConnectionProfile::from_desktop_config("eth0", &cfg);
        let text = prof.encode_keyfile();
        let parsed = ConnectionProfile::parse_keyfile(&text, "eth0");
        assert_eq!(parsed, prof);
    }
}
