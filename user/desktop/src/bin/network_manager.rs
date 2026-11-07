/*
 *
 *       user/desktop/src/bin/network_manager.rs
 *       SpaceOS-9 NetworkManager system daemon (/usr/bin/NetworkManager)
 *
 *       2026/11/06 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Standalone network management daemon (`/usr/bin/NetworkManager`) supervised
//! by `systemg` (`NetworkManager.service`).
//!
//! Responsibilities:
//! - Brings `lo` online and tracks Ethernet links via `NETLINK_ROUTE` multicast events.
//! - Runs per-device `NMDeviceState` carrier state machine and non-blocking RFC 2131
//!   DHCPv4 client (`Selecting`, `Requesting`, `Bound`, `Renewing` at `T1`, `Rebinding` at `T2`).
//! - Maintains INI keyfiles in `/etc/NetworkManager/system-connections/` and DNS
//!   configuration in `/run/NetworkManager/resolv.conf` + `/etc/resolv.conf`.
//! - Serves unprivileged CLI (`nmcli`) and desktop (`spui`) control requests over
//!   `/run/NetworkManager/nm.sock` while synchronizing with `uxconfd` channel `"network"`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use uxconf::net::NetworkManagerServer;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_term_signal(_sig: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    // SAFETY: `SIG_IGN` and async-signal-safe `AtomicBool::store` handler are valid POSIX dispositions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(
            libc::SIGTERM,
            handle_term_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            handle_term_signal as *const () as libc::sighandler_t,
        );
    }
}

fn print_usage() {
    println!(
        "Usage: NetworkManager [OPTIONS]\n\n\
         Options:\n  \
           -n, --no-daemon            Run in foreground (default under systemg)\n  \
           --socket <PATH>            Override AF_UNIX control socket (default: /run/NetworkManager/nm.sock)\n  \
           --profiles-dir <DIR>       Override keyfile directory (default: /etc/NetworkManager/system-connections)\n  \
           -V, --version              Print version information\n  \
           -h, --help                 Show this help message"
    );
}

fn main() -> ExitCode {
    // `uxconfd.service` is supervised by `systemg`; never spawn an unmanaged copy.
    std::env::set_var("UXCONF_NO_AUTOSPAWN", "1");
    let mut socket_override: Option<PathBuf> = None;
    let mut profiles_override: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-n" | "--no-daemon" => {}
            "--socket" => {
                let Some(val) = args.next() else {
                    eprintln!("NetworkManager: --socket requires a path");
                    return ExitCode::from(2);
                };
                socket_override = Some(PathBuf::from(val));
            }
            "--profiles-dir" => {
                let Some(val) = args.next() else {
                    eprintln!("NetworkManager: --profiles-dir requires a directory");
                    return ExitCode::from(2);
                };
                profiles_override = Some(PathBuf::from(val));
            }
            "-V" | "--version" => {
                println!("NetworkManager 1.0.0 (SpaceOS-9)");
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("NetworkManager: unknown option {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    install_signal_handlers();

    let mut server = match NetworkManagerServer::bind(socket_override, profiles_override) {
        Ok(srv) => srv,
        Err(err) => {
            eprintln!("NetworkManager: {err}");
            return ExitCode::FAILURE;
        }
    };

    eprintln!(
        "NetworkManager: listening on {}",
        server.socket_path().display()
    );
    if let Err(err) = server.run(&STOP) {
        eprintln!("NetworkManager: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
