/*
 *
 *       src/net/self_test.rs
 *       Boot-time tests for buffers, device registry, NAPI and loopback
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    dev::{self, NetDevice, NetDeviceConfig, NetDeviceOps, TxResult},
    ether,
    skb::SkBuff,
};
use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

const TEST_ETHERTYPE: u16 = 0x88b5; // IEEE 802 local experimental 1

static RECEIVED: AtomicUsize = AtomicUsize::new(0);
static RECEIVED_BYTES: AtomicUsize = AtomicUsize::new(0);

fn receive(_dev: &Arc<NetDevice>, skb: SkBuff) {
    RECEIVED.fetch_add(1, Ordering::SeqCst);
    RECEIVED_BYTES.fetch_add(skb.len(), Ordering::SeqCst);
}

/// Software NIC that records frames instead of sending them.
struct Sink {
    sent: AtomicUsize,
}

impl NetDeviceOps for Sink {
    fn start_xmit(&self, _dev: &NetDevice, _skb: SkBuff) -> TxResult {
        self.sent.fetch_add(1, Ordering::SeqCst);
        TxResult::Ok
    }
}

fn skb_basics() {
    let mut skb = SkBuff::alloc(32, 64).expect("alloc");
    assert_eq!((skb.len(), skb.headroom(), skb.tailroom()), (0, 32, 64));
    skb.put(10).expect("put").copy_from_slice(&[7; 10]);
    skb.push(14).expect("push").fill(1);
    assert_eq!((skb.len(), skb.headroom()), (24, 18));
    assert_eq!(skb.pull(14).expect("pull"), &[1; 14]);
    assert_eq!(skb.data(), &[7; 10]);
    assert!(skb.pull(11).is_none() && skb.push(33).is_none() && skb.put(55).is_none());
    assert!(skb.ensure_headroom(100));
    assert_eq!((skb.len(), skb.headroom()), (10, 100));
    assert_eq!(skb.data(), &[7; 10]);
    let copy = skb.duplicate().expect("duplicate");
    assert_eq!(copy.data(), skb.data());
    skb.trim(4);
    assert_eq!(skb.len(), 4);
    assert!(SkBuff::alloc(0, super::skb::MAX_SKB + 1).is_none());

    let mut frame = SkBuff::alloc(super::skb::DEFAULT_HEADROOM, 4).expect("alloc");
    frame.put(4).expect("put").copy_from_slice(&[9; 4]);
    let mac = [2, 0, 0, 0, 0, 1];
    assert!(ether::push_header(
        &mut frame,
        &ether::BROADCAST,
        &mac,
        0x0800
    ));
    assert!(ether::type_trans(&mut frame, &mac, false));
    assert_eq!(frame.protocol, 0x0800);
    assert_eq!(frame.pkt_type, super::skb::PacketType::Broadcast);
    assert_eq!(frame.data(), &[9; 4]);
}

fn registry_and_sysfs() {
    let lo = dev::by_name("lo").expect("lo missing");
    assert_eq!(lo.ifindex(), 1);
    assert!(lo.is_up() && lo.flags() & dev::IFF_LOOPBACK != 0);
    assert_eq!(lo.operstate(), dev::IF_OPER_UP);
    let mtu = crate::vfs::read_file("/sys/class/net/lo/mtu").expect("sysfs mtu");
    assert_eq!(mtu, b"65536\n");

    let sink = Arc::new(Sink {
        sent: AtomicUsize::new(0),
    });
    let mac = [2, 0x53, 0x50, 0x4b, 0, 1];
    let first = NetDevice::new(NetDeviceConfig::ethernet("test%d", mac), sink.clone()).unwrap();
    let second = NetDevice::new(NetDeviceConfig::ethernet("test%d", mac), sink.clone()).unwrap();
    dev::register_netdev(&first).expect("register first");
    dev::register_netdev(&second).expect("register second");
    assert_eq!(first.name(), "test0");
    assert_eq!(second.name(), "test1");
    assert_ne!(first.ifindex(), second.ifindex());
    assert!(dev::by_index(first.ifindex()).is_some());
    let address = crate::vfs::read_file("/sys/class/net/test0/address").expect("sysfs address");
    assert_eq!(address, b"02:53:50:4b:00:01\n");
    let duplicate = NetDevice::new(NetDeviceConfig::ethernet("test0", mac), sink.clone()).unwrap();
    assert!(dev::register_netdev(&duplicate).is_err());

    // Transmit needs the device up; the sink counts frames.
    let frame = SkBuff::from_slice(&[0u8; 60]).unwrap();
    assert!(dev::queue_xmit(&first, frame).is_err());
    dev::dev_open(&first).expect("open");
    assert_eq!(first.operstate(), dev::IF_OPER_UP);
    dev::queue_xmit(&first, SkBuff::from_slice(&[0u8; 60]).unwrap()).expect("xmit");
    assert_eq!(sink.sent.load(Ordering::SeqCst), 1);

    // A stopped queue holds frames until it is woken.
    first.stop_queue();
    dev::queue_xmit(&first, SkBuff::from_slice(&[0u8; 60]).unwrap()).expect("queued");
    assert_eq!(sink.sent.load(Ordering::SeqCst), 1);
    first.wake_queue();
    dev::process_pending();
    assert_eq!(sink.sent.load(Ordering::SeqCst), 2);

    // Carrier loss reflects in flags and operstate.
    first.carrier_off();
    assert_eq!(first.operstate(), dev::IF_OPER_LOWERLAYERDOWN);
    assert_eq!(first.flags() & dev::IFF_RUNNING, 0);
    first.carrier_on();
    assert_eq!(first.operstate(), dev::IF_OPER_UP);

    {
        let _rtnl = dev::rtnl_lock();
        dev::change_mtu_locked(&first, 1400).expect("mtu");
        assert!(dev::change_mtu_locked(&first, 10).is_err());
        assert!(dev::change_mtu_locked(&first, 9000).is_err());
        assert_eq!(first.mtu(), 1400);
        assert!(dev::rename_locked(&first, "test1").is_err());
        assert!(dev::set_mac_locked(&first, &[1, 2, 3, 4, 5, 6]).is_err()); // multicast bit
        dev::set_mac_locked(&first, &[2, 9, 9, 9, 9, 9]).expect("mac");
    }
    assert_eq!(first.hw_addr(), [2, 9, 9, 9, 9, 9]);

    dev::unregister_netdev(&first).expect("unregister first");
    dev::unregister_netdev(&second).expect("unregister second");
    assert!(dev::by_name("test0").is_none() && dev::by_name("test1").is_none());
    assert!(crate::vfs::read_file("/sys/class/net/test0/address").is_err());
    assert!(dev::unregister_netdev(&first).is_err());
    // Indices are never reused immediately.
    let third = NetDevice::new(NetDeviceConfig::ethernet("test%d", mac), sink).unwrap();
    dev::register_netdev(&third).expect("register third");
    assert!(third.ifindex() > second.ifindex());
    dev::unregister_netdev(&third).expect("unregister third");
}

fn loopback_and_napi() {
    dev::register_packet_type(TEST_ETHERTYPE, receive);
    let lo = dev::by_name("lo").expect("lo");
    let mut skb = SkBuff::from_slice(&[0x5a; 100]).unwrap();
    skb.protocol = TEST_ETHERTYPE;
    dev::queue_xmit(&lo, skb).expect("lo xmit");
    dev::process_pending();
    assert_eq!(RECEIVED.load(Ordering::SeqCst), 1);
    assert_eq!(RECEIVED_BYTES.load(Ordering::SeqCst), 100);
    let stats = lo.stats_snapshot();
    assert_eq!((stats.tx_packets, stats.rx_packets), (1, 1));
    assert_eq!((stats.tx_bytes, stats.rx_bytes), (100, 100));

    // A NAPI instance polls under budget and may be rescheduled after complete.
    let sink = Arc::new(Sink {
        sent: AtomicUsize::new(0),
    });
    let mac = [2, 0, 0, 0, 0, 2];
    let nic = NetDevice::new(NetDeviceConfig::ethernet("napi%d", mac), sink).unwrap();
    static POLLS: AtomicUsize = AtomicUsize::new(0);
    static PENDING: AtomicUsize = AtomicUsize::new(150);
    let napi = nic.napi_add(64, |napi, budget| {
        POLLS.fetch_add(1, Ordering::SeqCst);
        let done = PENDING.load(Ordering::SeqCst).min(budget);
        PENDING.fetch_sub(done, Ordering::SeqCst);
        if done < budget {
            napi.complete();
        }
        done
    });
    dev::register_netdev(&nic).expect("register napi nic");
    assert!(!napi.schedule(), "disabled NAPI must refuse scheduling");
    dev::dev_open(&nic).expect("open napi nic");
    assert!(napi.schedule());
    assert!(!napi.schedule(), "already scheduled");
    dev::process_pending();
    assert_eq!(PENDING.load(Ordering::SeqCst), 0);
    assert_eq!(POLLS.load(Ordering::SeqCst), 3); // 64 + 64 + 22
    assert!(napi.schedule(), "reschedulable after complete");
    dev::process_pending();
    dev::unregister_netdev(&nic).expect("unregister napi nic");

    // Backlog overflow drops instead of growing without bound.
    let before = lo.stats.rx_dropped.load(Ordering::Relaxed);
    for _ in 0..1100 {
        let mut skb = SkBuff::from_slice(&[1; 20]).unwrap();
        skb.protocol = TEST_ETHERTYPE;
        dev::netif_rx(&lo, skb);
    }
    assert_eq!(lo.stats.rx_dropped.load(Ordering::Relaxed) - before, 100);
    dev::process_pending();
    assert_eq!(RECEIVED.load(Ordering::SeqCst), 1001);
}

fn ipv4_loopback() {
    use super::{
        fib,
        ip::{self, IpAddr, Ipv4Addr},
        ipv4::{addr, icmp, raw},
    };
    assert!(addr::is_local(Ipv4Addr::LOCALHOST));
    let route = fib::lookup(IpAddr::V4(Ipv4Addr::LOCALHOST)).expect("loopback route");
    assert_eq!(route.kind, fib::RTN_LOCAL);

    let socket = raw::RawSocket::create(ip::IPPROTO_ICMP, true).expect("raw icmp socket");
    icmp::send_echo(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::LOCALHOST,
        0x5350,
        1,
        b"spacekernel",
    )
    .expect("echo request");
    dev::process_pending();
    let (request, source, _) = socket.receive(2048, true).expect("raw request");
    assert_eq!(source, Ipv4Addr::LOCALHOST);
    let header = super::ipv4::header::Ipv4Header::parse(&request).expect("request header");
    assert_eq!(
        (header.protocol, header.source, header.destination),
        (ip::IPPROTO_ICMP, Ipv4Addr::LOCALHOST, Ipv4Addr::LOCALHOST)
    );
    let body = &request[header.header_len()..];
    assert_eq!(body[0], icmp::ICMP_ECHO);
    assert_eq!(&body[4..6], &0x5350u16.to_be_bytes());
    let (reply, source, _) = socket.receive(2048, true).expect("raw reply");
    assert_eq!(source, Ipv4Addr::LOCALHOST);
    let header = super::ipv4::header::Ipv4Header::parse(&reply).expect("reply header");
    let body = &reply[header.header_len()..];
    assert_eq!((body[0], body[1]), (icmp::ICMP_ECHOREPLY, 0));
    assert_eq!(&body[4..8], &[0x53, 0x50, 0x00, 0x01]);
    assert_eq!(&body[8..], b"spacekernel");
    assert!(socket.receive(2048, true).is_err());
}

fn netlink_route() {
    use super::netlink::{
        self,
        msg::{
            Attrs, Messages, MsgBuilder, NLM_F_ACK, NLM_F_CREATE, NLM_F_DUMP, NLM_F_EXCL,
            NLM_F_REQUEST, NLMSG_DONE, NLMSG_ERROR,
        },
        route::*,
    };
    use super::{
        dev::{self, NetDevice, NetDeviceConfig},
        fib,
        ip::IpAddr,
        ipv4::addr,
        neigh,
    };
    use core::net::Ipv4Addr;

    let socket =
        netlink::NetlinkSocket::create(netlink::NETLINK_ROUTE, true).expect("route netlink socket");
    socket.bind(&netlink::encoded_source(0, 0)).expect("bind");

    let send = |kind: u16, flags: u16, seq: u32, body: &[u8], attrs: &dyn Fn(&mut MsgBuilder)| {
        let mut message = MsgBuilder::new(kind, NLM_F_REQUEST | flags, seq, 0);
        message.put_payload(body);
        attrs(&mut message);
        socket.send(&[], &message.finish()).expect("send");
    };

    // GETLINK dump must report loopback.
    send(RTM_GETLINK, NLM_F_DUMP, 1, &[0u8; 16], &|_| {});
    let mut saw_loopback = false;
    'link: loop {
        let (_, _, payload) = socket.receive(true).expect("link dump");
        for (header, body) in Messages::new(&payload) {
            if header.kind == NLMSG_DONE {
                break 'link;
            }
            assert_eq!(header.kind, RTM_NEWLINK);
            if Attrs::find(&body[16..], 3)
                .and_then(|attr| attr.string().map(String::from))
                .as_deref()
                == Some("lo")
            {
                saw_loopback = true;
            }
        }
    }
    assert!(saw_loopback, "GETLINK dump missed lo");

    // A device to configure, so the loopback routing is untouched.
    struct Sink;
    impl dev::NetDeviceOps for Sink {
        fn start_xmit(&self, _dev: &NetDevice, _skb: SkBuff) -> dev::TxResult {
            dev::TxResult::Ok
        }
    }
    let mac = [2, 0x53, 0x50, 0x4b, 0x00, 0x42];
    let device = NetDevice::new(NetDeviceConfig::ethernet("nltest%d", mac), Arc::new(Sink))
        .expect("nltest device");
    dev::register_netdev(&device).expect("register nltest");
    dev::dev_open(&device).expect("open nltest");
    let ifindex = device.ifindex();

    let address = Ipv4Addr::new(192, 0, 2, 1);
    let mut body = [0u8; 8];
    body[0] = 2;
    body[1] = 24;
    body[4..8].copy_from_slice(&ifindex.to_le_bytes());
    send(
        RTM_NEWADDR,
        NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL,
        2,
        &body,
        &|msg| {
            msg.put_attr(2, &address.octets());
            msg.put_attr(1, &address.octets());
            msg.put_str(3, "nltest");
        },
    );
    let (_, _, ack) = socket.receive(true).expect("addr ack");
    let (ack_header, ack_body) = Messages::new(&ack).next().expect("ack message");
    assert_eq!(ack_header.kind, NLMSG_ERROR);
    assert_eq!(i32::from_le_bytes(ack_body[..4].try_into().unwrap()), 0);
    assert_eq!(
        addr::get(ifindex, address)
            .expect("address installed")
            .prefixlen,
        24
    );

    // The connected and local routes must exist exactly as Linux creates them.
    assert_eq!(
        fib::lookup(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 77)))
            .expect("connected route")
            .prefixlen,
        24
    );
    assert_eq!(
        fib::lookup(IpAddr::V4(address)).expect("local route").kind,
        fib::RTN_LOCAL
    );

    // GETADDR dump filtered by interface.
    let mut query = [0u8; 8];
    query[0] = 2;
    query[4..8].copy_from_slice(&ifindex.to_le_bytes());
    send(RTM_GETADDR, NLM_F_DUMP, 3, &query, &|_| {});
    let mut saw_address = false;
    'addr: loop {
        let (_, _, payload) = socket.receive(true).expect("addr dump");
        for (header, body) in Messages::new(&payload) {
            if header.kind == NLMSG_DONE {
                break 'addr;
            }
            let index = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let local = Attrs::find(&body[8..], 2).and_then(|attr| attr.data.get(..4));
            if index == ifindex && local == Some(&address.octets()[..]) {
                saw_address = true;
            }
        }
    }
    assert!(saw_address, "GETADDR dump missed 192.0.2.1");

    // Add a default route through a gateway on the same link.
    let gateway = Ipv4Addr::new(192, 0, 2, 2);
    let mut rtmsg = [0u8; 12];
    rtmsg[0] = 2;
    rtmsg[4] = fib::RT_TABLE_MAIN as u8;
    rtmsg[5] = fib::RTPROT_STATIC;
    rtmsg[6] = fib::RT_SCOPE_UNIVERSE;
    rtmsg[7] = fib::RTN_UNICAST;
    send(
        RTM_NEWROUTE,
        NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL,
        4,
        &rtmsg,
        &|msg| {
            msg.put_attr(5, &gateway.octets());
            msg.put_u32(4, ifindex);
        },
    );
    let (_, _, ack) = socket.receive(true).expect("route ack");
    assert_eq!(
        i32::from_le_bytes(ack[16..20].try_into().unwrap()),
        0,
        "route add failed"
    );
    let resolved = fib::lookup(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))).expect("default route");
    assert_eq!(resolved.gateway, Some(IpAddr::V4(gateway)));
    assert_eq!(resolved.ifindex, ifindex);

    // Removing it must make the destination unreachable again.
    send(RTM_DELROUTE, NLM_F_ACK, 5, &rtmsg, &|msg| {
        msg.put_attr(5, &gateway.octets());
        msg.put_u32(4, ifindex);
    });
    let (_, _, ack) = socket.receive(true).expect("route delete ack");
    assert_eq!(i32::from_le_bytes(ack[16..20].try_into().unwrap()), 0);
    assert!(fib::lookup(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))).is_none());

    // Static neighbour through RTM_NEWNEIGH, visible in GETNEIGH.
    let hardware = [2u8, 0, 0, 0, 0, 9];
    let mut ndmsg = [0u8; 12];
    ndmsg[0] = 2;
    ndmsg[4..8].copy_from_slice(&(ifindex as i32).to_le_bytes());
    ndmsg[8..10].copy_from_slice(&neigh::NUD_PERMANENT.to_le_bytes());
    send(
        RTM_NEWNEIGH,
        NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL,
        6,
        &ndmsg,
        &|msg| {
            msg.put_attr(1, &gateway.octets());
            msg.put_attr(2, &hardware);
        },
    );
    let (_, _, ack) = socket.receive(true).expect("neigh ack");
    assert_eq!(i32::from_le_bytes(ack[16..20].try_into().unwrap()), 0);
    let entry = neigh::lookup(ifindex, IpAddr::V4(gateway)).expect("neighbour");
    assert_eq!(entry.hardware, hardware);
    assert_eq!(entry.state & neigh::NUD_PERMANENT, neigh::NUD_PERMANENT);

    send(RTM_GETNEIGH, NLM_F_DUMP, 7, &ndmsg, &|_| {});
    let mut saw_neighbor = false;
    'neigh: loop {
        let (_, _, payload) = socket.receive(true).expect("neigh dump");
        for (header, body) in Messages::new(&payload) {
            if header.kind == NLMSG_DONE {
                break 'neigh;
            }
            let index = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let destination = Attrs::find(&body[12..], 1).and_then(|attr| attr.data.get(..4));
            if index == ifindex && destination == Some(&gateway.octets()[..]) {
                saw_neighbor = true;
            }
        }
    }
    assert!(saw_neighbor, "GETNEIGH dump missed the static entry");

    send(RTM_DELNEIGH, NLM_F_ACK, 8, &ndmsg, &|msg| {
        msg.put_attr(1, &gateway.octets());
    });
    let (_, _, ack) = socket.receive(true).expect("neigh delete ack");
    assert_eq!(i32::from_le_bytes(ack[16..20].try_into().unwrap()), 0);
    assert!(neigh::lookup(ifindex, IpAddr::V4(gateway)).is_none());

    send(RTM_DELADDR, NLM_F_ACK, 9, &body, &|msg| {
        msg.put_attr(2, &address.octets());
    });
    let (_, _, ack) = socket.receive(true).expect("addr delete ack");
    assert_eq!(i32::from_le_bytes(ack[16..20].try_into().unwrap()), 0);
    assert!(addr::get(ifindex, address).is_none());

    dev::unregister_netdev(&device).expect("unregister nltest");
}

fn udp_loopback() {
    use super::{
        ip::Ipv4Addr,
        socket::{self, encode_sockaddr_in},
        udp::UdpSocket,
    };
    let receiver = UdpSocket::create(true);
    receiver
        .bind(&encode_sockaddr_in(Ipv4Addr::LOCALHOST, 23_456))
        .expect("udp bind receiver");
    let sender = UdpSocket::create(true);
    sender
        .bind(&encode_sockaddr_in(Ipv4Addr::LOCALHOST, 23_457))
        .expect("udp bind sender");
    let destination = encode_sockaddr_in(Ipv4Addr::LOCALHOST, 23_456);
    sender
        .send_to(b"udp-over-loopback", Some(&destination), 0)
        .expect("udp send");
    dev::process_pending();
    let (payload, source, truncated, full) = receiver.receive(64, 0x40).expect("udp receive");
    assert_eq!(payload, b"udp-over-loopback");
    assert!(!truncated && full == payload.len());
    assert_eq!(
        socket::decode_sockaddr_in(&source).expect("source").0,
        Ipv4Addr::LOCALHOST
    );
    assert_eq!(
        socket::decode_sockaddr_in(&source).expect("source").1,
        23_457
    );
    assert!(receiver.receive(64, 0x40).is_err());

    // A connected socket learns about ICMP port unreachable.
    let probe = UdpSocket::create(true);
    probe
        .bind(&encode_sockaddr_in(Ipv4Addr::LOCALHOST, 23_458))
        .expect("udp bind probe");
    probe
        .connect(&encode_sockaddr_in(Ipv4Addr::LOCALHOST, 9))
        .expect("udp connect");
    probe.send_to(b"?", None, 0).expect("udp probe send");
    dev::process_pending();
    match probe.receive(64, 0x40) {
        Err(crate::vfs::VfsError::ConnectionRefused) => {}
        other => panic!("expected connection refused, got {other:?}"),
    }
}

fn tcp_loopback() {
    use super::{
        ip::Ipv4Addr,
        socket::encode_sockaddr_in,
        tcp::{Status, TcpSocket},
    };
    let listener = TcpSocket::create(true);
    listener
        .bind(&encode_sockaddr_in(Ipv4Addr::LOCALHOST, 23_500))
        .expect("tcp bind");
    listener.listen(4).expect("tcp listen");

    let client = TcpSocket::create(true);
    client
        .connect(&encode_sockaddr_in(Ipv4Addr::LOCALHOST, 23_500), false)
        .expect("tcp connect");
    // Loopback delivery is synchronous: the handshake may already be
    // complete by the time connect() returns.
    assert!(matches!(
        client.status(),
        Status::SynSent | Status::Established
    ));
    dev::process_pending();
    assert_eq!(client.status(), Status::Established, "client handshake");
    let server = listener.accept(true).expect("tcp accept");
    assert_eq!(server.status(), Status::Established, "server handshake");

    client.send(b"hello tcp", 0).expect("tcp send");
    dev::process_pending();
    let (data, eof) = server.receive(64, 0x40).expect("tcp receive");
    assert_eq!(data, b"hello tcp");
    assert!(!eof);

    server.send(b"reply", 0).expect("tcp reply");
    dev::process_pending();
    let (data, eof) = client.receive(64, 0x40).expect("tcp reply receive");
    assert_eq!(data, b"reply");
    assert!(!eof);

    // Orderly shutdown in both directions reaches TIME_WAIT on the client.
    client.shutdown(1).expect("client shutdown");
    dev::process_pending();
    let (data, eof) = server.receive(64, 0x40).expect("server eof");
    assert!(data.is_empty() && eof);
    server.shutdown(1).expect("server shutdown");
    dev::process_pending();
    let (_data, eof) = client.receive(64, 0x40).expect("client eof");
    assert!(eof);
    assert_eq!(client.status(), Status::TimeWait);
    assert_eq!(server.status(), Status::Closed);
}

fn fragmentation_roundtrip() {
    use super::{
        dev::{self, NetDevice, NetDeviceConfig, NetDeviceOps, TxResult},
        ether,
        ip::{IpAddr, Ipv4Addr},
        ipv4::{fragment, header::Ipv4Header},
        neigh,
        skb::SkBuff,
    };
    use crate::sync::SpinLock;

    struct Capture {
        frames: SpinLock<Vec<SkBuff>>,
    }
    impl NetDeviceOps for Capture {
        fn start_xmit(&self, _dev: &NetDevice, skb: SkBuff) -> TxResult {
            if let Some(copy) = skb.duplicate() {
                self.frames.lock().push(copy);
            }
            TxResult::Ok
        }
    }
    let capture = Arc::new(Capture {
        frames: SpinLock::new(Vec::new()),
    });
    let mac = [2, 0x53, 0x50, 0x4b, 0x00, 0x60];
    let config = NetDeviceConfig {
        mtu: 600,
        ..NetDeviceConfig::ethernet("frag%d", mac)
    };
    let device = NetDevice::new(config, capture.clone()).expect("fragment device");
    dev::register_netdev(&device).expect("register fragment device");
    dev::dev_open(&device).expect("open fragment device");
    let gateway = Ipv4Addr::new(10, 1, 1, 2);
    let gateway_mac = [2, 0, 0, 0, 0, 0x22];
    neigh::set_static(
        device.ifindex(),
        neigh::AF_INET,
        IpAddr::V4(gateway),
        &gateway_mac,
        neigh::NUD_PERMANENT,
        0,
    )
    .expect("static neighbour");

    let payload: Vec<u8> = (0..2000u32).map(|index| index as u8).collect();
    let mut skb = SkBuff::alloc(128, payload.len()).expect("payload skb");
    skb.put(payload.len())
        .expect("put")
        .copy_from_slice(&payload);
    fragment::send(
        &device,
        Ipv4Addr::new(10, 1, 1, 1),
        Ipv4Addr::new(10, 2, 0, 5),
        gateway,
        &super::ipv4::header::TxTemplate {
            tos: 0,
            ttl: 64,
            protocol: 17,
            dont_fragment: false,
        },
        7,
        skb,
        600,
    )
    .expect("fragment send");
    dev::process_pending();
    let frames = core::mem::take(&mut *capture.frames.lock());
    assert_eq!(frames.len(), 4, "2000 bytes split into four fragments");
    let mut assembled = None;
    for mut frame in frames {
        assert!(ether::type_trans(&mut frame, &mac, false));
        let header = Ipv4Header::parse(frame.data()).expect("fragment header");
        assert!(header.is_fragment());
        if let Some(packet) = fragment::consume(&device, frame, &header) {
            assembled = Some(packet);
        }
    }
    let mut packet = assembled.expect("reassembled datagram");
    let header = Ipv4Header::parse(packet.data()).expect("assembled header");
    assert_eq!(header.total_len as usize, 20 + payload.len());
    assert!(!header.is_fragment());
    packet.pull(header.header_len());
    assert_eq!(packet.data(), &payload[..]);
    assert_eq!(fragment::pending_datagrams(), 0);
    dev::unregister_netdev(&device).expect("unregister fragment device");
}

fn ipv6_loopback_and_nd() {
    use super::{
        dev::{self, NetDevice, NetDeviceConfig, NetDeviceOps, TxResult},
        fib,
        ip::{self, IpAddr, Ipv6Addr},
        ipv6::{addr as addr6, icmpv6, ndisc, raw as raw6},
        neigh,
    };
    use crate::sync::SpinLock;

    assert!(addr6::is_local(Ipv6Addr::LOCALHOST));
    let route = fib::lookup(IpAddr::V6(Ipv6Addr::LOCALHOST)).expect("ipv6 loopback route");
    assert_eq!(route.kind, fib::RTN_LOCAL);

    let socket = raw6::Raw6Socket::create(ip::IPPROTO_ICMPV6, true).expect("raw6 icmpv6 socket");
    icmpv6::send_echo(
        Ipv6Addr::LOCALHOST,
        Ipv6Addr::LOCALHOST,
        0x5636,
        1,
        b"spacekernel-v6",
    )
    .expect("icmpv6 echo request");
    dev::process_pending();

    let (req_pkt, src, _) = socket.receive(2048, true).expect("raw6 request");
    assert_eq!(src, Ipv6Addr::LOCALHOST);
    assert_eq!(req_pkt[0], icmpv6::ICMPV6_ECHO_REQUEST);
    let (rep_pkt, src, _) = socket.receive(2048, true).expect("raw6 reply");
    assert_eq!(src, Ipv6Addr::LOCALHOST);
    assert_eq!(rep_pkt[0], icmpv6::ICMPV6_ECHO_REPLY);
    assert_eq!(&rep_pkt[8..], b"spacekernel-v6");

    // Verify NDISC Neighbor Solicitation emission and EUI-64 link-local address.
    struct Capture {
        frames: SpinLock<Vec<SkBuff>>,
    }
    impl NetDeviceOps for Capture {
        fn start_xmit(&self, _dev: &NetDevice, skb: SkBuff) -> TxResult {
            if let Some(copy) = skb.duplicate() {
                self.frames.lock().push(copy);
            }
            TxResult::Ok
        }
    }
    let capture = Arc::new(Capture {
        frames: SpinLock::new(Vec::new()),
    });
    let mac = [2, 0x53, 0x50, 0x4b, 0x06, 0x01];
    let device =
        NetDevice::new(NetDeviceConfig::ethernet("v6test%d", mac), capture.clone()).unwrap();
    dev::register_netdev(&device).expect("register v6test");
    dev::dev_open(&device).expect("open v6test");
    let ll = addr6::eui64_link_local(&mac).expect("eui64");
    assert!(
        addr6::get(device.ifindex(), ll).is_some(),
        "link-local auto-configured"
    );

    let peer = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x99);
    ndisc::solicit(&device, IpAddr::V6(peer));
    let frames = core::mem::take(&mut *capture.frames.lock());
    assert!(!frames.is_empty(), "expected NS frame");
    let frame = &frames[0];
    // Multicast MAC for ff02::1:ff00:0099 is 33:33:ff:00:00:99.
    assert_eq!(&frame.data()[0..6], &[0x33, 0x33, 0xff, 0x00, 0x00, 0x99]);
    let _ = neigh::count();
    dev::unregister_netdev(&device).expect("unregister v6test");
}

fn netlink_phase6() {
    use super::{
        ip::{self, Ipv4Addr},
        netlink::{
            self,
            genl::{
                CTRL_ATTR_FAMILY_ID, CTRL_ATTR_FAMILY_NAME, CTRL_CMD_GETFAMILY, CTRL_CMD_NEWFAMILY,
                GENL_ID_CTRL,
            },
            msg::{
                Attrs, Messages, MsgBuilder, NLM_F_ACK, NLM_F_DUMP, NLM_F_REQUEST, NLMSG_DONE,
                NLMSG_ERROR,
            },
            netfilter::{NFNL_MSG_BATCH_BEGIN, NFNL_MSG_BATCH_END, NFNL_SUBSYS_NFTABLES},
            sock_diag::SOCK_DIAG_BY_FAMILY,
        },
        socket::encode_sockaddr_in,
        tcp::TcpSocket,
        udp::UdpSocket,
    };

    // 1. NETLINK_GENERIC: CTRL_CMD_GETFAMILY single lookup + dump for "nlctrl".
    let genl_sock =
        netlink::NetlinkSocket::create(netlink::NETLINK_GENERIC, true).expect("genl socket");
    genl_sock
        .bind(&netlink::encoded_source(0, 0))
        .expect("genl bind");
    let mut msg = MsgBuilder::new(GENL_ID_CTRL, NLM_F_REQUEST, 1, 0);
    msg.put_payload(&[CTRL_CMD_GETFAMILY, 2, 0, 0]);
    msg.put_str(CTRL_ATTR_FAMILY_NAME, "nlctrl");
    genl_sock.send(&[], &msg.finish()).expect("genl send");
    let (_, _, reply) = genl_sock.receive(true).expect("genl reply");
    let (hdr, body) = Messages::new(&reply).next().expect("genl message");
    assert_eq!(hdr.kind, GENL_ID_CTRL);
    assert_eq!(body[0], CTRL_CMD_NEWFAMILY);
    let fam_id = Attrs::find(&body[4..], CTRL_ATTR_FAMILY_ID)
        .and_then(|a| a.u16())
        .expect("family id");
    assert_eq!(fam_id, GENL_ID_CTRL);

    let mut dump_msg = MsgBuilder::new(GENL_ID_CTRL, NLM_F_REQUEST | NLM_F_DUMP, 2, 0);
    dump_msg.put_payload(&[CTRL_CMD_GETFAMILY, 2, 0, 0]);
    genl_sock.send(&[], &dump_msg.finish()).expect("genl dump");
    let mut saw_nlctrl = false;
    'genl_dump: loop {
        let (_, _, chunk) = genl_sock.receive(true).expect("genl dump chunk");
        for (h, b) in Messages::new(&chunk) {
            if h.kind == NLMSG_DONE {
                break 'genl_dump;
            }
            if Attrs::find(&b[4..], CTRL_ATTR_FAMILY_NAME)
                .is_some_and(|a| a.string() == Some("nlctrl"))
            {
                saw_nlctrl = true;
            }
        }
    }
    assert!(saw_nlctrl, "genl dump missed nlctrl");

    // 2. NETLINK_NETFILTER: batch begin/end ACK and unknown subsys rejection.
    let nf_sock =
        netlink::NetlinkSocket::create(netlink::NETLINK_NETFILTER, true).expect("nfnetlink socket");
    nf_sock
        .bind(&netlink::encoded_source(0, 0))
        .expect("nfnetlink bind");
    let mut begin = MsgBuilder::new(NFNL_MSG_BATCH_BEGIN, NLM_F_REQUEST | NLM_F_ACK, 10, 0);
    begin.put_payload(&[0, 0, 0, NFNL_SUBSYS_NFTABLES]);
    nf_sock.send(&[], &begin.finish()).expect("nf batch begin");
    let (_, _, ack) = nf_sock.receive(true).expect("nf batch begin ack");
    let (ack_hdr, ack_body) = Messages::new(&ack).next().expect("nf ack");
    assert_eq!(ack_hdr.kind, NLMSG_ERROR);
    assert_eq!(i32::from_le_bytes(ack_body[..4].try_into().unwrap()), 0);

    let mut end = MsgBuilder::new(NFNL_MSG_BATCH_END, NLM_F_REQUEST | NLM_F_ACK, 11, 0);
    end.put_payload(&[0, 0, 0, NFNL_SUBSYS_NFTABLES]);
    nf_sock.send(&[], &end.finish()).expect("nf batch end");
    let (_, _, ack2) = nf_sock.receive(true).expect("nf batch end ack");
    let (_, ack2_body) = Messages::new(&ack2).next().unwrap();
    assert_eq!(i32::from_le_bytes(ack2_body[..4].try_into().unwrap()), 0);

    let unknown_type = ((NFNL_SUBSYS_NFTABLES as u16) << 8) | 1;
    let mut unreg = MsgBuilder::new(unknown_type, NLM_F_REQUEST, 12, 0);
    unreg.put_payload(&[2, 0, 0, 0]);
    nf_sock.send(&[], &unreg.finish()).expect("nf unreg");
    let (_, _, err_reply) = nf_sock.receive(true).expect("nf unreg err");
    let (_, err_body) = Messages::new(&err_reply).next().unwrap();
    assert_eq!(i32::from_le_bytes(err_body[..4].try_into().unwrap()), -95); // -EOPNOTSUPP

    // 3. NETLINK_SOCK_DIAG: TCP and UDP socket dump via SOCK_DIAG_BY_FAMILY.
    let tcp_listener = TcpSocket::create(true);
    tcp_listener
        .bind(&encode_sockaddr_in(Ipv4Addr::LOCALHOST, 23_600))
        .expect("diag tcp bind");
    tcp_listener.listen(2).expect("diag tcp listen");

    let udp_sock = UdpSocket::create(true);
    udp_sock
        .bind(&encode_sockaddr_in(Ipv4Addr::LOCALHOST, 23_601))
        .expect("diag udp bind");

    let diag_sock =
        netlink::NetlinkSocket::create(netlink::NETLINK_SOCK_DIAG, true).expect("sock_diag socket");
    diag_sock
        .bind(&netlink::encoded_source(0, 0))
        .expect("sock_diag bind");

    let mut req = [0u8; 56];
    req[0] = 2; // AF_INET
    req[1] = ip::IPPROTO_TCP;
    req[4..8].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
    let mut diag_msg = MsgBuilder::new(SOCK_DIAG_BY_FAMILY, NLM_F_REQUEST | NLM_F_DUMP, 20, 0);
    diag_msg.put_payload(&req);
    diag_sock
        .send(&[], &diag_msg.finish())
        .expect("diag tcp send");
    let mut saw_tcp_listener = false;
    'tcp_diag: loop {
        let (_, _, chunk) = diag_sock.receive(true).expect("diag tcp chunk");
        for (h, b) in Messages::new(&chunk) {
            if h.kind == NLMSG_DONE {
                break 'tcp_diag;
            }
            assert_eq!(h.kind, SOCK_DIAG_BY_FAMILY);
            let sport = u16::from_be_bytes([b[4], b[5]]);
            if sport == 23_600 {
                saw_tcp_listener = true;
            }
        }
    }
    assert!(saw_tcp_listener, "SOCK_DIAG missed TCP listener on 23600");

    req[1] = ip::IPPROTO_UDP;
    let mut diag_udp = MsgBuilder::new(SOCK_DIAG_BY_FAMILY, NLM_F_REQUEST | NLM_F_DUMP, 21, 0);
    diag_udp.put_payload(&req);
    diag_sock
        .send(&[], &diag_udp.finish())
        .expect("diag udp send");
    let mut saw_udp = false;
    'udp_diag: loop {
        let (_, _, chunk) = diag_sock.receive(true).expect("diag udp chunk");
        for (h, b) in Messages::new(&chunk) {
            if h.kind == NLMSG_DONE {
                break 'udp_diag;
            }
            let sport = u16::from_be_bytes([b[4], b[5]]);
            if sport == 23_601 {
                saw_udp = true;
            }
        }
    }
    assert!(saw_udp, "SOCK_DIAG missed UDP socket on 23601");
}

fn ip_sockopts_and_cmsg() {
    use super::{
        ip::{self, Ipv4Addr, Ipv6Addr},
        ipv4::{
            header::{IPOPT_NOOP, IPOPT_RR},
            icmp, raw,
        },
        ipv6::{icmpv6, raw as raw6},
        socket::{
            IP_MTU_DISCOVER, IP_OPTIONS, IP_PKTINFO, IP_PMTUDISC_DO, IP_RECVOPTS, IP_RECVTOS,
            IP_RECVTTL, IP_RETOPTS, IP_TOS, IP_TTL, SO_TIMESTAMP_OLD, SOL_IP, SOL_IPV6, SOL_RAW,
            SOL_SOCKET, encode_cmsgs, encode_sockaddr_in, encode_sockaddr_in6,
        },
    };

    // 1. Unprivileged AF_INET SOCK_DGRAM IPPROTO_ICMP (BusyBox / iputils ping flow).
    let ping = raw::RawSocket::create_ping(true).expect("dgram ping socket");
    let hold = 65536i32.to_le_bytes();
    ping.setsockopt(SOL_IP, IP_RECVTTL, &hold)
        .expect("setsockopt IP_RECVTTL");
    ping.setsockopt(SOL_IP, IP_RETOPTS, &hold)
        .expect("setsockopt IP_RETOPTS");
    ping.setsockopt(SOL_IP, IP_RECVOPTS, &1i32.to_le_bytes())
        .expect("setsockopt IP_RECVOPTS");
    ping.setsockopt(SOL_IP, IP_PKTINFO, &1i32.to_le_bytes())
        .expect("setsockopt IP_PKTINFO");
    ping.setsockopt(SOL_IP, IP_RECVTOS, &1i32.to_le_bytes())
        .expect("setsockopt IP_RECVTOS");
    ping.setsockopt(
        SOL_IP,
        IP_MTU_DISCOVER,
        &(IP_PMTUDISC_DO as i32).to_le_bytes(),
    )
    .expect("setsockopt IP_MTU_DISCOVER");
    ping.setsockopt(SOL_IP, IP_TOS, &0x28i32.to_le_bytes())
        .expect("setsockopt IP_TOS");
    ping.setsockopt(SOL_IP, IP_TTL, &42i32.to_le_bytes())
        .expect("setsockopt IP_TTL");
    ping.setsockopt(SOL_SOCKET, SO_TIMESTAMP_OLD, &1i32.to_le_bytes())
        .expect("setsockopt SO_TIMESTAMP_OLD");

    assert_eq!(
        ping.getsockopt(SOL_IP, IP_RETOPTS).expect("get IP_RETOPTS"),
        1i32.to_le_bytes()
    );
    assert_eq!(
        ping.getsockopt(SOL_IP, IP_RECVTTL).expect("get IP_RECVTTL"),
        1i32.to_le_bytes()
    );
    assert_eq!(
        ping.getsockopt(SOL_IP, IP_TTL).expect("get IP_TTL"),
        42i32.to_le_bytes()
    );

    let dst = encode_sockaddr_in(Ipv4Addr::LOCALHOST, 0);
    let mut echo = [0u8; 12];
    echo[0] = icmp::ICMP_ECHO;
    echo[6..8].copy_from_slice(&7u16.to_be_bytes());
    echo[8..12].copy_from_slice(b"ping");
    ping.send_to(&echo, Some(&dst), true)
        .expect("ping dgram send");
    dev::process_pending();

    let rx = ping
        .receive_msg(2048, 0x40)
        .expect("ping dgram reply without options");
    assert_eq!(rx.data.len(), 12);
    assert_eq!(rx.data[0], icmp::ICMP_ECHOREPLY);
    assert_eq!(&rx.data[6..8], &7u16.to_be_bytes());
    assert_eq!(&rx.data[8..12], b"ping");
    assert!(!rx.control_truncated);
    assert!(
        rx.cmsgs.iter().any(|c| c.level == SOL_SOCKET as i32
            && c.kind == SO_TIMESTAMP_OLD as i32
            && c.data.len() == 16),
        "missing SO_TIMESTAMP_OLD cmsg"
    );
    assert!(
        rx.cmsgs.iter().any(|c| c.level == SOL_IP as i32
            && c.kind == IP_PKTINFO as i32
            && &c.data[8..12] == &[127, 0, 0, 1]),
        "missing IP_PKTINFO cmsg"
    );
    assert!(
        rx.cmsgs.iter().any(|c| c.level == SOL_IP as i32
            && c.kind == IP_TTL as i32
            && c.data == 64i32.to_le_bytes()),
        "missing IP_TTL cmsg"
    );
    assert!(
        rx.cmsgs
            .iter()
            .any(|c| c.level == SOL_IP as i32 && c.kind == IP_TOS as i32 && c.data.len() == 1),
        "missing IP_TOS cmsg"
    );
    assert!(
        !rx.cmsgs.iter().any(|c| c.level == SOL_IP as i32
            && (c.kind == IP_RECVOPTS as i32 || c.kind == IP_RETOPTS as i32)),
        "no IP_RECVOPTS/IP_RETOPTS expected when IHL == 20"
    );

    let (encoded, ctrunc) = encode_cmsgs(&rx.cmsgs, false, true, 256);
    assert!(!ctrunc && encoded.len() >= 32 && encoded.len() % 8 == 0);
    let (_, short_ctrunc) = encode_cmsgs(&rx.cmsgs, false, true, 20);
    assert!(
        short_ctrunc,
        "expected MSG_CTRUNC on undersized control buffer"
    );

    // 2. IP_OPTIONS Record Route (ping -R) + IP_RECVOPTS + IP_RETOPTS roundtrip.
    let mut rr = [0u8; 40];
    rr[0] = IPOPT_NOOP;
    rr[1] = IPOPT_RR;
    rr[2] = 39;
    rr[3] = 4;
    ping.setsockopt(SOL_IP, IP_OPTIONS, &rr)
        .expect("setsockopt IP_OPTIONS RR");
    assert_eq!(
        ping.getsockopt(SOL_IP, IP_OPTIONS)
            .expect("getsockopt IP_OPTIONS")
            .len(),
        40
    );

    echo[6..8].copy_from_slice(&8u16.to_be_bytes());
    ping.send_to(&echo, Some(&dst), true)
        .expect("ping dgram send with RR");
    dev::process_pending();

    let rx_rr = ping
        .receive_msg(2048, 0x40)
        .expect("ping dgram reply with RR");
    assert_eq!(rx_rr.data[0], icmp::ICMP_ECHOREPLY);
    let recvopts = rx_rr
        .cmsgs
        .iter()
        .find(|c| c.level == SOL_IP as i32 && c.kind == IP_RECVOPTS as i32)
        .expect("expected IP_RECVOPTS cmsg");
    assert_eq!(recvopts.data.len(), 40);
    assert_eq!(
        (recvopts.data[0], recvopts.data[1], recvopts.data[2]),
        (IPOPT_RR, 39, 12)
    );
    assert_eq!(&recvopts.data[3..7], &[127, 0, 0, 1]);
    assert_eq!(&recvopts.data[7..11], &[127, 0, 0, 1]);
    let retopts = rx_rr
        .cmsgs
        .iter()
        .find(|c| c.level == SOL_IP as i32 && c.kind == IP_RETOPTS as i32)
        .expect("expected IP_RETOPTS cmsg");
    assert_eq!(retopts.data.len(), 40);
    assert_eq!((retopts.data[0], retopts.data[1]), (IPOPT_RR, 39));
    assert_eq!(&retopts.data[3..7], &[127, 0, 0, 1]);

    ping.setsockopt(SOL_IP, IP_OPTIONS, &[])
        .expect("clear IP_OPTIONS");
    assert!(
        ping.getsockopt(SOL_IP, IP_OPTIONS)
            .expect("getsockopt cleared IP_OPTIONS")
            .is_empty()
    );

    // 3. SOCK_RAW SOL_RAW / ICMP_FILTER: block ICMP_ECHO, allow ICMP_ECHOREPLY.
    let raw_sock = raw::RawSocket::create(ip::IPPROTO_ICMP, true).expect("raw icmp filter socket");
    let filter_mask: u32 = 1u32 << icmp::ICMP_ECHO;
    raw_sock
        .setsockopt(SOL_RAW, 1, &filter_mask.to_le_bytes())
        .expect("setsockopt ICMP_FILTER");
    assert_eq!(
        raw_sock
            .getsockopt(SOL_RAW, 1)
            .expect("getsockopt ICMP_FILTER"),
        filter_mask.to_le_bytes()
    );
    icmp::send_echo(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::LOCALHOST,
        0x464c,
        9,
        b"filtered",
    )
    .expect("filtered echo");
    dev::process_pending();
    let (pkt, _, _) = raw_sock.receive(2048, true).expect("filtered reply");
    let hdr = super::ipv4::header::Ipv4Header::parse(&pkt).expect("hdr");
    assert_eq!(pkt[hdr.header_len()], icmp::ICMP_ECHOREPLY);
    assert!(raw_sock.receive(2048, true).is_err());

    // 4. Unprivileged IPv6 ping socket + IPV6_RECVHOPLIMIT (51) & IPV6_2292HOPLIMIT (8).
    let ping6 = raw6::Raw6Socket::create_ping(true).expect("dgram ping6 socket");
    ping6
        .setsockopt(SOL_IPV6, 51, &1i32.to_le_bytes())
        .expect("IPV6_RECVHOPLIMIT");
    ping6
        .setsockopt(SOL_IPV6, 8, &1i32.to_le_bytes())
        .expect("IPV6_2292HOPLIMIT");
    ping6
        .setsockopt(SOL_IPV6, 49, &1i32.to_le_bytes())
        .expect("IPV6_RECVPKTINFO");
    let dst6 = encode_sockaddr_in6(Ipv6Addr::LOCALHOST, 0, 0);
    let mut echo6 = [0u8; 12];
    echo6[0] = icmpv6::ICMPV6_ECHO_REQUEST;
    echo6[6..8].copy_from_slice(&3u16.to_be_bytes());
    echo6[8..12].copy_from_slice(b"v6ok");
    ping6
        .send_to(&echo6, Some(&dst6), true)
        .expect("ping6 send");
    dev::process_pending();
    let rx6 = ping6.receive_msg(2048, 0x40).expect("ping6 reply");
    assert_eq!(rx6.data[0], icmpv6::ICMPV6_ECHO_REPLY);
    assert_eq!(&rx6.data[6..8], &3u16.to_be_bytes());
    assert!(
        rx6.cmsgs
            .iter()
            .any(|c| c.level == SOL_IPV6 as i32 && c.kind == 52 && c.data == 64i32.to_le_bytes()),
        "missing IPV6_HOPLIMIT cmsg"
    );
    assert!(
        rx6.cmsgs
            .iter()
            .any(|c| c.level == SOL_IPV6 as i32 && c.kind == 8 && c.data == 64i32.to_le_bytes()),
        "missing IPV6_2292HOPLIMIT cmsg"
    );
    assert!(
        rx6.cmsgs
            .iter()
            .any(|c| c.level == SOL_IPV6 as i32 && c.kind == 50 && c.data.len() == 20),
        "missing IPV6_PKTINFO cmsg"
    );
}

pub fn run() {
    skb_basics();
    registry_and_sysfs();
    loopback_and_napi();
    ipv4_loopback();
    ipv6_loopback_and_nd();
    ip_sockopts_and_cmsg();
    netlink_route();
    netlink_phase6();
    udp_loopback();
    tcp_loopback();
    fragmentation_roundtrip();
    super::timer::self_test::run();
    crate::mm::dma::self_test::run();
    super::netlink::self_test::run();
    let _ = String::new();
    crate::verbose::log(format_args!("network core self-test passed"));
}
