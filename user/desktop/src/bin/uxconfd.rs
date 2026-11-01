/*
 *
 *       user/desktop/src/bin/uxconfd.rs
 *       SpaceOS-9 hierarchical configuration daemon (uxconfd)
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Standalone configuration management daemon (`/usr/bin/uxconfd`) modeled
//! after XFCE's `xfconfd`. Serves `AF_UNIX` client requests and broadcasts
//! real-time `PropertyChanged` events across subscribed desktop applications.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use uxconf::daemon::Server;
use uxconf::protocol::resolve_socket_path;
use uxconf::storage::{resolve_system_dir, resolve_user_dir, ConfigStore};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_term_signal(_sig: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    // SAFETY: `SIG_IGN` and async-signal-safe `AtomicBool::store` handler are valid POSIX signal dispositions.
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
        "Usage: uxconfd [OPTIONS]\n\n\
         Options:\n  \
           --socket <PATH>      Override AF_UNIX socket path (default: /run/uxconfd.sock)\n  \
           --system-dir <DIR>   Override system channel directory (default: /etc/uxconf/channels)\n  \
           --user-dir <DIR>     Override persistent user directory (default: /var/lib/uxconf/channels)\n  \
           -V, --version        Print version information\n  \
           -h, --help           Show this help message"
    );
}

fn main() -> ExitCode {
    let mut socket_override: Option<PathBuf> = None;
    let mut system_override: Option<PathBuf> = None;
    let mut user_override: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                let Some(val) = args.next() else {
                    eprintln!("uxconfd: --socket requires a path");
                    return ExitCode::from(2);
                };
                socket_override = Some(PathBuf::from(val));
            }
            "--system-dir" => {
                let Some(val) = args.next() else {
                    eprintln!("uxconfd: --system-dir requires a directory");
                    return ExitCode::from(2);
                };
                system_override = Some(PathBuf::from(val));
            }
            "--user-dir" => {
                let Some(val) = args.next() else {
                    eprintln!("uxconfd: --user-dir requires a directory");
                    return ExitCode::from(2);
                };
                user_override = Some(PathBuf::from(val));
            }
            "-V" | "--version" => {
                println!("uxconfd 1.0.0 (SpaceOS-9)");
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("uxconfd: unknown option {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    install_signal_handlers();

    let socket_path = socket_override.unwrap_or_else(resolve_socket_path);
    let system_dir = resolve_system_dir(system_override.as_deref());
    let user_dir = resolve_user_dir(user_override.as_deref());

    let store = ConfigStore::open(system_dir, user_dir);
    let mut server = match Server::bind(socket_path, store) {
        Ok(srv) => srv,
        Err(err) => {
            eprintln!("uxconfd: {err}");
            return ExitCode::FAILURE;
        }
    };

    eprintln!("uxconfd: listening on {}", server.socket_path().display());
    if let Err(err) = server.run(&STOP) {
        eprintln!("uxconfd: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
