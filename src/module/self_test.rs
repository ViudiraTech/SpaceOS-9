/*
 *
 *       src/module/self_test.rs
 *       Boot-time loadable module integration test
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Loads the C and Rust hello modules from build artifacts embedded by
//! `build.rs`, checks the procfs/sysfs views, then unloads them.

static HELLO: &[u8] = include_bytes!(env!("SPK_HELLO_KO"));
static HELLO_RUST: &[u8] = include_bytes!(env!("SPK_HELLO_RUST_KO"));
static E1000: &[u8] = include_bytes!(env!("SPK_E1000_KO"));
static E1000E: &[u8] = include_bytes!(env!("SPK_E1000E_KO"));

fn check_module(name: &str) {
    assert!(crate::module::is_loaded(name), "module {name} missing");
    let modules = crate::vfs::read_file("/proc/modules").expect("/proc/modules unavailable");
    assert!(
        modules
            .windows(name.len())
            .any(|window| window == name.as_bytes()),
        "/proc/modules does not list {name}"
    );
    let refcnt = crate::vfs::read_file(&alloc::format!("/sys/module/{name}/refcnt"))
        .expect("/sys/module attribute unavailable");
    assert_eq!(refcnt, b"0\n");
}

pub fn run() {
    if crate::boot::init_path().is_some() {
        return;
    }
    if HELLO.is_empty() {
        crate::kwarn!("module self-test: hello.ko was not built by the Makefile");
        return;
    }
    crate::module::load(HELLO, "").expect("C hello module failed to load");
    check_module("hello");
    #[cfg(feature = "kallsyms")]
    assert!(
        crate::kallsyms::has_symbol("spk_printk"),
        "generated kallsyms is missing an exported symbol"
    );
    #[cfg(feature = "module-unload")]
    {
        crate::module::unload("hello", false).expect("C hello module failed to unload");
        assert!(!crate::module::is_loaded("hello"));
    }

    if HELLO_RUST.is_empty() {
        crate::kwarn!("module self-test: hello-rust.ko was not built by the Makefile");
    } else {
        crate::module::load(HELLO_RUST, "").expect("Rust hello module failed to load");
        check_module("hello-rust");
        #[cfg(feature = "module-unload")]
        {
            crate::module::unload("hello-rust", false).expect("Rust hello module failed to unload");
            assert!(!crate::module::is_loaded("hello-rust"));
        }
    }
    crate::verbose::log(format_args!("loadable module self-test passed"));
}

fn has_pci_device(device_id: u16) -> bool {
    crate::pci::devices()
        .iter()
        .any(|device| device.vendor_id == 0x8086 && device.device_id == device_id)
}

fn load_network_driver(pci_id: u16, name: &str, image: &[u8]) -> bool {
    if image.is_empty() || !has_pci_device(pci_id) || crate::module::is_loaded(name) {
        return false;
    }
    match crate::module::load(image, "") {
        Ok(()) => {
            crate::kinfo!("module: network driver {} attached (self-test)", name);
            true
        }
        Err(error) => {
            crate::kwarn!("module: {} failed to load ({})", name, error);
            false
        }
    }
}

/// End-to-end NIC check for headless kernel-only self-test builds (`init=` absent).
/// When booting userland (`init=/sbin/systemg`), kernel modules are loaded
/// exclusively by `systemg` via `/etc/modules` and `finit_module(2)`.
pub fn network_late() {
    if crate::boot::init_path().is_some() {
        return;
    }
    use crate::net::{
        dev, fib,
        ip::{self, IpAddr, Ipv4Addr},
        ipv4::{addr, arp, icmp, raw},
    };
    #[cfg(feature = "module-unload")]
    let baseline_dma = crate::mm::dma::coherent_bytes();

    let loaded_driver = if load_network_driver(0x10d3, "e1000e", E1000E) {
        Some((0x10d3u16, "e1000e", E1000E))
    } else if load_network_driver(0x100e, "e1000", E1000) {
        Some((0x100eu16, "e1000", E1000))
    } else {
        None
    };

    let Some(device) = dev::devices()
        .into_iter()
        .find(|device| device.name().starts_with("eth"))
    else {
        return;
    };
    let address = Ipv4Addr::new(10, 0, 2, 15);
    let gateway = Ipv4Addr::new(10, 0, 2, 2);

    let configure_and_ping = |device: &alloc::sync::Arc<dev::NetDevice>, rounds: u16| {
        if !addr::is_local(address) {
            let entry = addr::IfAddr {
                ifindex: device.ifindex(),
                address,
                local: address,
                prefixlen: 24,
                broadcast: Some(Ipv4Addr::new(10, 0, 2, 255)),
                scope: fib::RT_SCOPE_UNIVERSE,
                flags: addr::IFA_F_PERMANENT,
                label: alloc::string::String::new(),
            };
            addr::add(entry).expect("network self-test: address");
            let mut route = fib::Route::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
            route.gateway = Some(IpAddr::V4(gateway));
            route.ifindex = device.ifindex();
            route.protocol = fib::RTPROT_STATIC;
            fib::add(route).expect("network self-test: default route");
        }
        dev::dev_open(device).expect("network self-test: device open");
        assert!(
            crate::mm::dma::coherent_bytes() > 0,
            "network self-test: expected coherent DMA rings after dev_open"
        );
        arp::announce(device, address);
        let socket =
            raw::RawSocket::create(ip::IPPROTO_ICMP, true).expect("network self-test: raw socket");
        let mut replies = 0u16;
        for attempt in 0..20u16 {
            let _ = icmp::send_echo(address, gateway, 0x534b, attempt, b"spacekernel");
            let deadline = crate::time::uptime_micros() + 500_000;
            while crate::time::uptime_micros() < deadline {
                if let Ok((packet, source, _)) = socket.receive(2048, true) {
                    if let Some(header) = crate::net::ipv4::header::Ipv4Header::parse(&packet)
                        && header.source == gateway
                        && header.protocol == ip::IPPROTO_ICMP
                    {
                        let body = &packet[header.header_len()..];
                        if body.len() >= 8
                            && body[0] == icmp::ICMP_ECHOREPLY
                            && u16::from_be_bytes([body[6], body[7]]) == attempt
                        {
                            replies += 1;
                            break;
                        }
                    }
                    let _ = source;
                }
                crate::sched::sleep_micros(2000);
            }
            if replies >= rounds {
                break;
            }
        }
        assert!(
            replies >= rounds,
            "network self-test: expected {rounds} ICMP echo replies from {gateway} on {}, got {replies}",
            device.name()
        );
        let stats = device.stats_snapshot();
        assert!(
            stats.tx_packets >= u64::from(rounds) && stats.rx_packets >= u64::from(rounds),
            "network self-test: stats mismatch on {}: tx={} rx={}",
            device.name(),
            stats.tx_packets,
            stats.rx_packets
        );
        assert_eq!(stats.tx_errors, 0);
        assert_eq!(stats.rx_errors, 0);
    };

    configure_and_ping(&device, 4);
    let dev_name = device.name();
    drop(device);

    #[cfg(feature = "module-unload")]
    if let Some((pci_id, name, image)) = loaded_driver {
        crate::module::unload(name, false).expect("network self-test: driver unload");
        assert!(
            dev::by_name(&dev_name).is_none(),
            "network self-test: {dev_name} still registered after {name} unload"
        );
        assert_eq!(
            crate::mm::dma::coherent_bytes(),
            baseline_dma,
            "network self-test: coherent DMA leak after {name} unload"
        );
        assert!(load_network_driver(pci_id, name, image));
        let reloaded = dev::devices()
            .into_iter()
            .find(|d| d.name().starts_with("eth"))
            .expect("network self-test: device missing after reload");
        let reloaded_name = reloaded.name();
        configure_and_ping(&reloaded, 2);
        drop(reloaded);
        crate::module::unload(name, false).expect("network self-test: final driver unload");
        assert_eq!(
            crate::mm::dma::coherent_bytes(),
            baseline_dma,
            "network self-test: coherent DMA leak after second {name} unload"
        );
        crate::kinfo!(
            "NET_PING_OK dev={} address={} gateway={}",
            reloaded_name,
            address,
            gateway
        );
        return;
    }

    let _ = loaded_driver;
    crate::kinfo!(
        "NET_PING_OK dev={} address={} gateway={}",
        dev_name,
        address,
        gateway
    );
}
