/*
 *
 *       src/net/netlink/sock_diag.rs
 *       NETLINK_SOCK_DIAG: TCP and UDP socket inspection for ss(8)
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mirrors `net/core/sock_diag.c` and `net/ipv4/inet_diag.c`
//! (`uapi/linux/sock_diag.h`, `uapi/linux/inet_diag.h`).

use super::{
    KernelCfg, NETLINK_SOCK_DIAG, NL_CFG_F_NONROOT_RECV, Request, SKIP_ACK, kernel_create,
    msg::{ExtAck, MsgBuilder, NLM_F_DUMP},
};
use crate::{
    net::{
        ip::{self, Ipv4Addr},
        tcp::{self, Status},
        udp,
    },
    vfs::abi,
};
use alloc::{boxed::Box, vec, vec::Vec};

pub const TCPDIAG_GETSOCK: u16 = 18;
pub const DCCPDIAG_GETSOCK: u16 = 19;
pub const SOCK_DIAG_BY_FAMILY: u16 = 20;

const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;

// Linux `include/net/tcp_states.h` numbers used in `inet_diag_msg.idiag_state`.
pub const TCP_ESTABLISHED: u8 = 1;
pub const TCP_SYN_SENT: u8 = 2;
pub const TCP_SYN_RECV: u8 = 3;
pub const TCP_FIN_WAIT1: u8 = 4;
pub const TCP_FIN_WAIT2: u8 = 5;
pub const TCP_TIME_WAIT: u8 = 6;
pub const TCP_CLOSE: u8 = 7;
pub const TCP_CLOSE_WAIT: u8 = 8;
pub const TCP_LAST_ACK: u8 = 9;
pub const TCP_LISTEN: u8 = 10;
pub const TCP_CLOSING: u8 = 11;

const INET_DIAG_REQ_V2_LEN: usize = 56;
const INET_DIAG_MSG_LEN: usize = 72;

pub fn init() {
    let mut cfg = KernelCfg::new();
    cfg.groups = 32;
    cfg.flags = NL_CFG_F_NONROOT_RECV;
    cfg.input = Some(input);
    if let Err(error) = kernel_create(NETLINK_SOCK_DIAG, cfg) {
        crate::kwarn!("net: NETLINK_SOCK_DIAG registration failed: {error:?}");
        return;
    }
    crate::verbose::log(format_args!("net: NETLINK_SOCK_DIAG registered"));
}

fn linux_tcp_state(internal: u8) -> u8 {
    match internal {
        x if x == Status::Closed as u8 => TCP_CLOSE,
        x if x == Status::Listen as u8 => TCP_LISTEN,
        x if x == Status::SynSent as u8 => TCP_SYN_SENT,
        x if x == Status::SynRecv as u8 => TCP_SYN_RECV,
        x if x == Status::Established as u8 => TCP_ESTABLISHED,
        x if x == Status::FinWait1 as u8 => TCP_FIN_WAIT1,
        x if x == Status::FinWait2 as u8 => TCP_FIN_WAIT2,
        x if x == Status::CloseWait as u8 => TCP_CLOSE_WAIT,
        x if x == Status::Closing as u8 => TCP_CLOSING,
        x if x == Status::LastAck as u8 => TCP_LAST_ACK,
        x if x == Status::TimeWait as u8 => TCP_TIME_WAIT,
        _ => TCP_CLOSE,
    }
}

struct DiagEntry {
    family: u8,
    state: u8,
    sport: u16,
    dport: u16,
    src: Ipv4Addr,
    dst: Ipv4Addr,
    ino: u32,
}

fn encode_diag_msg(entry: &DiagEntry, kind: u16, flags: u16, seq: u32, portid: u32) -> Vec<u8> {
    let mut payload = vec![0u8; INET_DIAG_MSG_LEN];
    payload[0] = entry.family;
    payload[1] = entry.state;
    payload[2] = 0; // idiag_timer
    payload[3] = 0; // idiag_retrans
    payload[4..6].copy_from_slice(&entry.sport.to_be_bytes());
    payload[6..8].copy_from_slice(&entry.dport.to_be_bytes());
    payload[8..12].copy_from_slice(&entry.src.octets());
    payload[24..28].copy_from_slice(&entry.dst.octets());
    // idiag_if = 0 at 40..44
    let cookie = (entry.ino as u64).to_le_bytes();
    payload[44..52].copy_from_slice(&cookie);
    // expires, rqueue, wqueue, uid = 0 at 52..68
    payload[68..72].copy_from_slice(&entry.ino.to_le_bytes());

    let mut builder = MsgBuilder::new(kind, flags, seq, portid);
    builder.put_payload(&payload);
    builder.finish()
}

fn collect_entries(family: u8, protocol: u8, state_mask: u32) -> Result<Vec<DiagEntry>, i32> {
    if family != AF_INET && family != AF_INET6 {
        return Err(-(abi::EAFNOSUPPORT as i32));
    }
    let mut entries = Vec::new();
    if family == AF_INET6 {
        match protocol {
            ip::IPPROTO_TCP | ip::IPPROTO_UDP => return Ok(entries),
            _ => return Err(-(abi::ENOENT as i32)),
        }
    }
    match protocol {
        ip::IPPROTO_TCP => {
            for (ino, src, sport, dst, dport, raw_state) in tcp::diagnostics() {
                let state = linux_tcp_state(raw_state);
                if state_mask != 0 && (state_mask & (1u32 << state)) == 0 {
                    continue;
                }
                entries.push(DiagEntry {
                    family: AF_INET,
                    state,
                    sport,
                    dport,
                    src,
                    dst,
                    ino: ino as u32,
                });
            }
        }
        ip::IPPROTO_UDP => {
            for (ino, src, sport, dst, dport, connected) in udp::diagnostics() {
                let state = if connected {
                    TCP_ESTABLISHED
                } else {
                    TCP_CLOSE
                };
                if state_mask != 0 && (state_mask & (1u32 << state)) == 0 {
                    continue;
                }
                entries.push(DiagEntry {
                    family: AF_INET,
                    state,
                    sport,
                    dport,
                    src,
                    dst,
                    ino: ino as u32,
                });
            }
        }
        _ => return Err(-(abi::ENOENT as i32)),
    }
    Ok(entries)
}

fn input(request: &Request<'_>, _ext: &mut ExtAck) -> Result<(), i32> {
    let (family, protocol, state_mask, sport, dport, src, dst, reply_kind) =
        match request.header.kind {
            SOCK_DIAG_BY_FAMILY => {
                if request.payload.len() < INET_DIAG_REQ_V2_LEN {
                    return Err(-(abi::EINVAL as i32));
                }
                let p = request.payload;
                let family = p[0];
                let protocol = p[1];
                let states = u32::from_le_bytes([p[4], p[5], p[6], p[7]]);
                let sport = u16::from_be_bytes([p[8], p[9]]);
                let dport = u16::from_be_bytes([p[10], p[11]]);
                let src = Ipv4Addr::new(p[12], p[13], p[14], p[15]);
                let dst = Ipv4Addr::new(p[28], p[29], p[30], p[31]);
                (
                    family,
                    protocol,
                    states,
                    sport,
                    dport,
                    src,
                    dst,
                    SOCK_DIAG_BY_FAMILY,
                )
            }
            TCPDIAG_GETSOCK => {
                if request.payload.len() < INET_DIAG_REQ_V2_LEN {
                    return Err(-(abi::EINVAL as i32));
                }
                let p = request.payload;
                let family = p[0];
                let sport = u16::from_be_bytes([p[4], p[5]]);
                let dport = u16::from_be_bytes([p[6], p[7]]);
                let src = Ipv4Addr::new(p[8], p[9], p[10], p[11]);
                let dst = Ipv4Addr::new(p[24], p[25], p[26], p[27]);
                let states = u32::from_le_bytes([p[52], p[53], p[54], p[55]]);
                (
                    family,
                    ip::IPPROTO_TCP,
                    states,
                    sport,
                    dport,
                    src,
                    dst,
                    TCPDIAG_GETSOCK,
                )
            }
            _ => return Err(-(abi::EINVAL as i32)),
        };

    let entries = collect_entries(family, protocol, state_mask)?;
    if request.header.flags & NLM_F_DUMP != 0 {
        let seq = request.header.seq;
        let portid = request.socket.portid();
        let mut next = 0usize;
        request.dump_start(Box::new(move |ctx| {
            while next < entries.len() {
                let msg = encode_diag_msg(
                    &entries[next],
                    reply_kind,
                    super::msg::NLM_F_MULTI,
                    seq,
                    portid,
                );
                if !ctx.try_put(&msg) {
                    return Ok(true);
                }
                next += 1;
            }
            Ok(false)
        }))?;
        return Err(SKIP_ACK);
    }

    let found = entries
        .into_iter()
        .find(|e| {
            e.sport == sport
                && (dport == 0 || e.dport == dport)
                && (src.is_unspecified() || e.src == src)
                && (dst.is_unspecified() || e.dst == dst)
        })
        .ok_or(-(abi::ENOENT as i32))?;
    let reply = encode_diag_msg(
        &found,
        reply_kind,
        0,
        request.header.seq,
        request.socket.portid(),
    );
    request
        .reply(reply)
        .map_err(|error| error.to_errno() as i32)?;
    Ok(())
}
