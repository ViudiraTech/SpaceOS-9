/*
 *
 *       src/net/netlink/self_test.rs
 *       Boot-time tests for the netlink core
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    KernelCfg, NETLINK_KOBJECT_UEVENT, NETLINK_USERSOCK, NetlinkSocket, Request, SKIP_ACK,
    broadcast_uevent, has_listeners, kernel_create, kernel_release,
    msg::{
        Attrs, ExtAck, Messages, MsgBuilder, NLM_F_ACK, NLM_F_DUMP, NLM_F_MULTI, NLM_F_REQUEST,
        NLMSG_DONE, NLMSG_ERROR, NLMSG_MIN_TYPE,
    },
};
use crate::vfs::VfsError;
use alloc::{string::String, vec::Vec};

const TYPE_ECHO: u16 = NLMSG_MIN_TYPE;
const TYPE_DUMP: u16 = NLMSG_MIN_TYPE + 1;
const TYPE_FAIL: u16 = NLMSG_MIN_TYPE + 2;
const TYPE_EXT: u16 = NLMSG_MIN_TYPE + 3;
const ITEMS: u32 = 400;

fn sockaddr(portid: u32, groups: u32) -> [u8; 12] {
    super::encoded_source(portid, groups)
}

fn request(kind: u16, flags: u16, seq: u32, body: &[u8]) -> Vec<u8> {
    let mut message = MsgBuilder::new(kind, NLM_F_REQUEST | flags, seq, 0);
    message.put_payload(body);
    message.finish()
}

fn handler(request: &Request<'_>, ext: &mut ExtAck) -> Result<(), i32> {
    match request.header.kind {
        TYPE_ECHO => {
            let mut reply =
                MsgBuilder::new(TYPE_ECHO, 0, request.header.seq, request.socket.portid());
            reply.put_payload(request.payload);
            request.reply(reply.finish()).map_err(|_| -105)
        }
        TYPE_DUMP => {
            let mut next = 0u32;
            request.dump_start(alloc::boxed::Box::new(move |ctx| {
                while next < ITEMS {
                    let mut item = ctx.message(TYPE_DUMP);
                    item.put_u32(1, next).put_attr(2, &[0xab; 100]);
                    if !ctx.try_put(&item.finish()) {
                        return Ok(true);
                    }
                    next += 1;
                }
                Ok(false)
            }))?;
            Err(SKIP_ACK)
        }
        TYPE_FAIL => Err(-22),
        TYPE_EXT => {
            ext.message = Some("bad attribute");
            ext.bad_attr_offset = Some(20);
            Err(-22)
        }
        _ => Err(-95),
    }
}

fn drain(socket: &NetlinkSocket) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Ok((_, _, payload)) = socket.receive(true) {
        out.push(payload);
    }
    out
}

pub fn run() {
    let mut cfg = KernelCfg::new();
    cfg.input = Some(handler);
    cfg.groups = 40;
    kernel_create(NETLINK_USERSOCK, cfg).expect("register test protocol");
    assert_eq!(
        kernel_create(NETLINK_USERSOCK, cfg).err(),
        Some(VfsError::AlreadyExists)
    );

    // Unique portids: explicit bind, collision, and autobind.
    let a = NetlinkSocket::create(NETLINK_USERSOCK, true).unwrap();
    let b = NetlinkSocket::create(NETLINK_USERSOCK, true).unwrap();
    let c = NetlinkSocket::create(NETLINK_USERSOCK, true).unwrap();
    a.bind(&sockaddr(1000, 0)).unwrap();
    assert_eq!(b.bind(&sockaddr(1000, 0)), Err(VfsError::AddressInUse));
    assert_eq!(a.bind(&sockaddr(1001, 0)), Err(VfsError::InvalidArgument));
    b.bind(&sockaddr(0, 0)).unwrap();
    c.bind(&sockaddr(0, 0)).unwrap();
    assert!(b.portid() != 0 && b.portid() != c.portid() && b.portid() != 1000);
    assert_eq!(a.address()[4..8], 1000u32.to_le_bytes());
    assert!(
        NetlinkSocket::create(31, true).is_err(),
        "unregistered protocol"
    );

    // Request -> reply, then the ACK.
    let message = request(TYPE_ECHO, NLM_F_ACK, 7, b"ping");
    assert_eq!(a.send(&sockaddr(0, 0), &message).unwrap(), message.len());
    let replies = drain(&a);
    assert_eq!(replies.len(), 2);
    let (header, payload) = Messages::new(&replies[0]).next().unwrap();
    assert_eq!((header.kind, header.seq), (TYPE_ECHO, 7));
    assert_eq!(payload, b"ping");
    let (ack, body) = Messages::new(&replies[1]).next().unwrap();
    assert_eq!((ack.kind, ack.seq), (NLMSG_ERROR, 7));
    assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 0);
    assert_eq!(body.len(), 4 + 16, "success ACK echoes only the header");

    // Errors echo the whole request; CAP_ACK trims it.
    let message = request(TYPE_FAIL, 0, 8, &[1, 2, 3, 4, 5, 6, 7, 8]);
    a.send(&sockaddr(0, 0), &message).unwrap();
    let replies = drain(&a);
    let (_, body) = Messages::new(&replies[0]).next().unwrap();
    assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), -22);
    assert_eq!(body.len(), 4 + message.len());
    a.setsockopt(270, 10, &1u32.to_le_bytes()).unwrap();
    a.send(&sockaddr(0, 0), &message).unwrap();
    let replies = drain(&a);
    let (header, body) = Messages::new(&replies[0]).next().unwrap();
    assert_eq!(body.len(), 4 + 16);
    assert_ne!(header.flags & super::msg::NLM_F_CAPPED, 0);
    a.setsockopt(270, 10, &0u32.to_le_bytes()).unwrap();

    // Extended ACK TLVs only when the socket opted in.
    let message = request(TYPE_EXT, 0, 9, &[]);
    a.send(&sockaddr(0, 0), &message).unwrap();
    let replies = drain(&a);
    let (header, _) = Messages::new(&replies[0]).next().unwrap();
    assert_eq!(header.flags & super::msg::NLM_F_ACK_TLVS, 0);
    a.setsockopt(270, 11, &1u32.to_le_bytes()).unwrap();
    a.send(&sockaddr(0, 0), &message).unwrap();
    let replies = drain(&a);
    let (header, body) = Messages::new(&replies[0]).next().unwrap();
    assert_ne!(header.flags & super::msg::NLM_F_ACK_TLVS, 0);
    let tlvs = &body[4 + 16..];
    assert!(Attrs::valid(tlvs));
    assert_eq!(
        Attrs::find(tlvs, 1).unwrap().string(),
        Some("bad attribute")
    );
    assert_eq!(Attrs::find(tlvs, 2).unwrap().u32(), Some(20));

    // Unknown message types and non-requests behave like Linux.
    a.send(&sockaddr(0, 0), &request(0x40, 0, 10, &[])).unwrap();
    let replies = drain(&a);
    let (_, body) = Messages::new(&replies[0]).next().unwrap();
    assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), -95);
    let mut control = MsgBuilder::new(NLMSG_DONE, 0, 11, 0);
    control.put_payload(&[0; 4]);
    a.send(&sockaddr(0, 0), &control.finish()).unwrap();
    assert!(
        drain(&a).is_empty(),
        "control messages without ACK are ignored"
    );

    // Multipart dump resumes lazily and finishes with NLMSG_DONE.
    let message = request(TYPE_DUMP, NLM_F_DUMP, 12, &[]);
    a.send(&sockaddr(0, 0), &message).unwrap();
    let mut seen = 0u32;
    let mut chunks = 0;
    let mut done = false;
    for chunk in drain(&a) {
        chunks += 1;
        for (header, payload) in Messages::new(&chunk) {
            assert_eq!(header.seq, 12);
            assert_ne!(header.flags & NLM_F_MULTI, 0);
            if header.kind == NLMSG_DONE {
                assert_eq!(i32::from_le_bytes(payload[0..4].try_into().unwrap()), 0);
                done = true;
            } else {
                assert_eq!(u32::from_le_bytes(payload[4..8].try_into().unwrap()), seen);
                seen += 1;
            }
        }
    }
    assert!(
        done && seen == ITEMS && chunks > 1,
        "dump {seen} items in {chunks} chunks"
    );

    // Unicast between user sockets by portid.
    let to_b = request(TYPE_ECHO, 0, 1, b"x");
    a.send(&sockaddr(b.portid(), 0), &to_b).unwrap();
    let (sender, groups, payload) = b.receive(true).unwrap();
    assert_eq!((sender, groups, &payload), (1000, 0, &to_b));
    assert_eq!(
        a.send(&sockaddr(55555, 0), &to_b),
        Err(VfsError::ConnectionRefused)
    );

    // Multicast: membership, delivery, sender exclusion, drop, overrun.
    assert!(!has_listeners(NETLINK_USERSOCK, 5));
    b.bind(&sockaddr(b.portid(), 1 << 4)).unwrap(); // group 5
    c.setsockopt(270, 1, &39u32.to_le_bytes()).unwrap(); // group 39 (> 32)
    assert!(c.setsockopt(270, 1, &41u32.to_le_bytes()).is_err());
    assert!(has_listeners(NETLINK_USERSOCK, 5) && has_listeners(NETLINK_USERSOCK, 39));
    a.setsockopt(270, 1, &5u32.to_le_bytes()).unwrap();
    // Not a valid message, so the kernel-side input handler ignores it.
    a.send(&sockaddr(0, 1 << 4), b"mc").unwrap();
    let (sender, groups, _) = b.receive(true).unwrap();
    assert_eq!((sender, groups), (0, 1 << 4));
    assert!(
        a.receive(true).is_err(),
        "the sender is excluded from its own broadcast"
    );
    assert!(c.receive(true).is_err());
    assert_eq!(super::broadcast(NETLINK_USERSOCK, 0, 39, b"hi"), 1);
    assert_eq!(c.receive(true).unwrap().2, b"hi");
    assert_eq!(
        c.getsockopt(270, 9).unwrap()[4..8],
        (1u32 << 6).to_le_bytes(),
        "LIST_MEMBERSHIPS reports group 39"
    );
    b.setsockopt(270, 2, &5u32.to_le_bytes()).unwrap();
    assert_eq!(super::broadcast(NETLINK_USERSOCK, 0, 5, b"x"), 1); // only `a`
    drain(&a);

    // Receive buffer accounting: ENOBUFS once, then recovery; opt-out works.
    b.setsockopt(270, 1, &6u32.to_le_bytes()).unwrap();
    b.setsockopt(1, 8, &1u32.to_le_bytes()).unwrap(); // clamps to the minimum
    let big = [0u8; 1500];
    let mut delivered = 0;
    for _ in 0..20 {
        delivered += super::broadcast(NETLINK_USERSOCK, 0, 6, &big);
    }
    assert!(
        delivered > 0 && delivered < 20,
        "queue limit hit after {delivered}"
    );
    assert_eq!(b.receive(true), Err(VfsError::NoBuffers));
    let mut queued = 0;
    while b.receive(true).is_ok() {
        queued += 1;
    }
    assert_eq!(queued, delivered);
    b.setsockopt(270, 5, &1u32.to_le_bytes()).unwrap(); // NO_ENOBUFS
    for _ in 0..20 {
        super::broadcast(NETLINK_USERSOCK, 0, 6, &big);
    }
    assert!(
        b.receive(true).is_ok(),
        "no error is reported with NO_ENOBUFS"
    );

    // Dropping a socket releases its portid and membership.
    let portid = c.portid();
    drop(c);
    assert!(!has_listeners(NETLINK_USERSOCK, 39));
    let again = NetlinkSocket::create(NETLINK_USERSOCK, true).unwrap();
    again
        .bind(&sockaddr(portid, 0))
        .expect("portid reusable after close");

    // Kernel uevents reach group-1 listeners of the uevent protocol.
    let listener = NetlinkSocket::create(NETLINK_KOBJECT_UEVENT, true).unwrap();
    listener.bind(&sockaddr(0, 1)).unwrap();
    broadcast_uevent(
        "add",
        "/devices/test",
        &[(String::from("SUBSYSTEM"), String::from("net"))],
    );
    let (sender, groups, payload) = listener.receive(true).unwrap();
    assert_eq!((sender, groups), (0, 1));
    assert!(payload.starts_with(b"add@/devices/test\0ACTION=add\0DEVPATH=/devices/test\0"));
    assert!(payload.ends_with(b"SUBSYSTEM=net\0"));
    // Userspace may rebroadcast to another group (libudev-zero style).
    let monitor = NetlinkSocket::create(NETLINK_KOBJECT_UEVENT, true).unwrap();
    monitor.bind(&sockaddr(0, 1 << 3)).unwrap();
    listener.send(&sockaddr(0, 1 << 3), &payload).unwrap();
    assert_eq!(monitor.receive(true).unwrap().2, payload);

    drop((a, b, again, listener, monitor));
    kernel_release(NETLINK_USERSOCK);
    assert!(NetlinkSocket::create(NETLINK_USERSOCK, true).is_err());
    crate::verbose::log(format_args!("netlink core self-test passed"));
}
