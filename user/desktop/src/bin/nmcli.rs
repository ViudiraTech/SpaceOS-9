/*
 *
 *       user/desktop/src/bin/nmcli.rs
 *       Command-line client for SpaceOS NetworkManager (/usr/bin/nmcli)
 *
 *       2026/11/06 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Command-line control and inspection utility (`/usr/bin/nmcli`) communicating
//! with `NetworkManager.service` over `/run/NetworkManager/nm.sock`.

use std::process::ExitCode;
use uxconf::net::{
    nm_request, parse_kv_lines, query_status, request_apply, request_connect, request_renew,
};
use uxconf::DesktopConfig;

fn print_usage() {
    println!(
        "Usage: nmcli [OBJECT] [COMMAND] [ARGUMENTS...]\n\n\
         Objects:\n  \
           g[eneral]      [status]                 Show overall NetworkManager state and connectivity\n  \
           n[etworking]   [on | off | connectivity] Enable/disable networking or check connectivity\n  \
           c[onnection]   [show]                   List saved connection profiles (.nmconnection)\n  \
           d[evice]       [status]                 List network interfaces and NMDeviceState\n  \
           d[evice]       show [IFNAME]            Show detailed IP/DHCP/DNS/counter info\n  \
           d[evice]       connect [IFNAME]         Activate interface\n  \
           d[evice]       disconnect [IFNAME]      Deactivate interface (sends DHCPRELEASE)\n  \
           d[evice]       reapply [IFNAME]         Reapply active profile\n  \
           d[evice]       renew [IFNAME]           Renew DHCPv4 lease\n\n\
         Options:\n  \
           -V, --version  Print version\n  \
           -h, --help     Show this help"
    );
}

fn cmd_overview() -> ExitCode {
    let st = query_status();
    if !st.has_device {
        println!("lo: connected (externally) to lo");
        println!("        \"lo\"");
        println!("        loopback (unknown), 00:00:00:00:00:00, sw, mtu 65536");
        println!("        inet4 127.0.0.1/8");
        return ExitCode::SUCCESS;
    }
    let state_desc = if st.connected() {
        format!("connected to {}", st.profile_name)
    } else {
        st.device_state.as_str().to_string()
    };
    println!("{}: {state_desc}", st.ifname);
    println!("        \"{}\"", st.ifname);
    println!("        ethernet, {}, hw, mtu {}", st.mac, st.mtu);
    for (ip, plen) in &st.ipv4_all {
        println!("        inet4 {ip}/{plen}");
    }
    if let Some(gw) = st.gateway {
        println!("        route4 default via {gw}");
    }
    if let Some((ip6, plen6)) = st.ipv6_ll {
        println!("        inet6 {ip6}/{plen6}");
    }
    if let Some(dns) = st.dns {
        println!(
            "\nDNS configuration:\n        servers: {dns}\n        interface: {}",
            st.ifname
        );
    }
    ExitCode::SUCCESS
}

fn cmd_general() -> ExitCode {
    match nm_request("STATUS") {
        Ok(lines) => {
            let kv = parse_kv_lines(&lines);
            let state = kv
                .get("state_name")
                .map(String::as_str)
                .unwrap_or("unknown");
            let conn = kv.get("connectivity").map(String::as_str).unwrap_or("none");
            println!(
                "{:<16} {:<15} {:<10} {:<10}",
                "STATE", "CONNECTIVITY", "WIFI-HW", "WIFI"
            );
            println!(
                "{:<16} {:<15} {:<10} {:<10}",
                state, conn, "missing", "enabled"
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nmcli: {err}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_devices() -> ExitCode {
    match nm_request("DEVICES") {
        Ok(lines) => {
            println!(
                "{:<12} {:<12} {:<24} {:<24} {}",
                "DEVICE", "TYPE", "STATE", "CONNECTION", "IPv4"
            );
            for line in lines {
                if let Some(rest) = line.strip_prefix("DEV\t") {
                    let cols: Vec<&str> = rest.split('\t').collect();
                    if cols.len() >= 5 {
                        println!(
                            "{:<12} {:<12} {:<24} {:<24} {}",
                            cols[0], cols[1], cols[2], cols[3], cols[4]
                        );
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nmcli: {err}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_device_show(ifname: Option<&str>) -> ExitCode {
    let cmd = match ifname {
        Some(name) => format!("SHOW {name}"),
        None => "SHOW".to_string(),
    };
    match nm_request(&cmd) {
        Ok(lines) => {
            for line in lines.into_iter().skip(1) {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nmcli: {err}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_connections() -> ExitCode {
    match nm_request("CONNECTIONS") {
        Ok(lines) => {
            println!("{:<28} {:<38} {:<18} {}", "NAME", "UUID", "TYPE", "DEVICE");
            for line in lines {
                if let Some(rest) = line.strip_prefix("CONN\t") {
                    let cols: Vec<&str> = rest.split('\t').collect();
                    if cols.len() >= 4 {
                        println!(
                            "{:<28} {:<38} {:<18} {}",
                            cols[0], cols[1], cols[2], cols[3]
                        );
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nmcli: {err}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return cmd_overview();
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => {
            print_usage();
            ExitCode::SUCCESS
        }
        "-V" | "--version" => {
            println!("nmcli tool, version 1.0.0 (SpaceOS-9)");
            ExitCode::SUCCESS
        }
        obj if "general".starts_with(obj) => cmd_general(),
        obj if "networking".starts_with(obj) => {
            let sub = args.get(1).map(String::as_str).unwrap_or("connectivity");
            match sub {
                "on" => match request_connect(None, true) {
                    Ok(msg) => {
                        println!("{msg}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("nmcli: {e}");
                        ExitCode::FAILURE
                    }
                },
                "off" => match request_connect(None, false) {
                    Ok(msg) => {
                        println!("{msg}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("nmcli: {e}");
                        ExitCode::FAILURE
                    }
                },
                _ => match nm_request("STATUS") {
                    Ok(lines) => {
                        let kv = parse_kv_lines(&lines);
                        println!(
                            "{}",
                            kv.get("connectivity").map(String::as_str).unwrap_or("none")
                        );
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("nmcli: {e}");
                        ExitCode::FAILURE
                    }
                },
            }
        }
        obj if "connection".starts_with(obj) => cmd_connections(),
        obj if "device".starts_with(obj) => {
            let sub = args.get(1).map(String::as_str).unwrap_or("status");
            let ifname = args.get(2).map(String::as_str);
            match sub {
                s if "status".starts_with(s) => cmd_devices(),
                "show" => cmd_device_show(ifname),
                "connect" | "up" => match request_connect(ifname, true) {
                    Ok(msg) => {
                        println!("{msg}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("nmcli: {e}");
                        ExitCode::FAILURE
                    }
                },
                "disconnect" | "down" => match request_connect(ifname, false) {
                    Ok(msg) => {
                        println!("{msg}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("nmcli: {e}");
                        ExitCode::FAILURE
                    }
                },
                "reapply" => match request_apply(&DesktopConfig::default()) {
                    Ok(msg) => {
                        println!("{msg}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("nmcli: {e}");
                        ExitCode::FAILURE
                    }
                },
                "renew" => match request_renew(ifname) {
                    Ok(msg) => {
                        println!("{msg}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("nmcli: {e}");
                        ExitCode::FAILURE
                    }
                },
                other => {
                    eprintln!("nmcli: unknown device command {other:?}");
                    ExitCode::from(2)
                }
            }
        }
        other => {
            eprintln!("nmcli: unknown object {other:?} (try 'nmcli --help')");
            ExitCode::from(2)
        }
    }
}
