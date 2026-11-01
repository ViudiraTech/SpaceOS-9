/*
 *
 *       user/desktop/src/bin/uxconf_query.rs
 *       Command-line query, mutation and live monitor utility for uxconfd
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Command-line tool (`/usr/bin/uxconf-query`) modeled after XFCE's `xfconf-query`
//! for inspecting channels, reading/setting/resetting properties and monitoring
//! real-time configuration updates.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use uxconf::client::{UxConfClient, UxConfWatch};
use uxconf::schema;
use uxconf::{Value, ValueKind};

static STOP_MONITOR: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_int(_sig: libc::c_int) {
    STOP_MONITOR.store(true, Ordering::Relaxed);
}

fn print_usage() {
    println!(
        "Usage: uxconf-query [OPTIONS]\n\n\
         Channel & Property Operations:\n  \
           -l, --list                 List all channels (or properties if -c is given)\n  \
           -c, --channel <NAME>       Select configuration channel\n  \
           -p, --property <PATH>      Select property path (e.g. /theme/mode)\n  \
           -s, --set <VALUE>          Set property value\n  \
           -t, --type <TYPE>          Explicit value type: bool | int | float | string\n  \
           -r, --reset                Reset property back to default\n  \
           -R, --recursive            Reset entire property subtree recursively\n  \
           -v, --verbose              Show types and values when listing properties\n  \
           -m, --monitor              Monitor channel (or all channels) for live changes\n  \
           -V, --version              Print version information\n  \
           -h, --help                 Show this help message"
    );
}

fn main() -> ExitCode {
    let mut channel: Option<String> = None;
    let mut property: Option<String> = None;
    let mut set_val: Option<String> = None;
    let mut explicit_kind: Option<ValueKind> = None;
    let mut list = false;
    let mut reset = false;
    let mut recursive = false;
    let mut verbose = false;
    let mut monitor = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-l" | "--list" => list = true,
            "-v" | "--verbose" => verbose = true,
            "-r" | "--reset" => reset = true,
            "-R" | "--recursive" => recursive = true,
            "-m" | "--monitor" => monitor = true,
            "-c" | "--channel" => {
                let Some(val) = args.next() else {
                    eprintln!("uxconf-query: -c/--channel requires a channel name");
                    return ExitCode::from(2);
                };
                channel = Some(val);
            }
            "-p" | "--property" => {
                let Some(val) = args.next() else {
                    eprintln!("uxconf-query: -p/--property requires a property path");
                    return ExitCode::from(2);
                };
                property = Some(val);
            }
            "-s" | "--set" => {
                let Some(val) = args.next() else {
                    eprintln!("uxconf-query: -s/--set requires a value");
                    return ExitCode::from(2);
                };
                set_val = Some(val);
            }
            "-t" | "--type" => {
                let Some(val) = args.next() else {
                    eprintln!("uxconf-query: -t/--type requires a type");
                    return ExitCode::from(2);
                };
                match ValueKind::parse(&val) {
                    Ok(k) => explicit_kind = Some(k),
                    Err(e) => {
                        eprintln!("uxconf-query: {e}");
                        return ExitCode::from(2);
                    }
                }
            }
            "-V" | "--version" => {
                println!("uxconf-query 1.0.0 (SpaceOS-9)");
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("uxconf-query: unknown argument {other:?} (try --help)");
                return ExitCode::from(2);
            }
        }
    }

    if monitor {
        return run_monitor(channel.as_deref());
    }

    let mut client = match UxConfClient::connect() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("uxconf-query: {err}");
            return ExitCode::FAILURE;
        }
    };

    if list && channel.is_none() {
        match client.list_channels() {
            Ok(channels) => {
                for ch in channels {
                    println!("{ch}");
                }
                return ExitCode::SUCCESS;
            }
            Err(err) => {
                eprintln!("uxconf-query: {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    let Some(ch) = channel.as_deref() else {
        print_usage();
        return ExitCode::from(2);
    };

    if list {
        let prefix = property.as_deref().unwrap_or("/");
        match client.get_all(ch, prefix) {
            Ok(entries) => {
                let max_w = entries.keys().map(String::len).max().unwrap_or(0);
                for (prop, val) in entries {
                    if verbose {
                        println!(
                            "{prop:<width$}  ({:<6})  {}",
                            val.kind().as_str(),
                            val.display_plain(),
                            width = max_w
                        );
                    } else {
                        println!("{prop}");
                    }
                }
                return ExitCode::SUCCESS;
            }
            Err(err) => {
                eprintln!("uxconf-query: {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    if reset {
        let prop = match property.as_deref() {
            Some(p) => p,
            None if recursive => "/",
            None => {
                eprintln!("uxconf-query: --reset requires -p <property> (or -R to reset channel)");
                return ExitCode::from(2);
            }
        };
        match client.reset(ch, prop, recursive) {
            Ok(_) => return ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("uxconf-query: {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    let Some(prop) = property.as_deref() else {
        eprintln!("uxconf-query: specify -l to list or -p <property> to query/modify");
        return ExitCode::from(2);
    };

    if let Some(raw_val) = set_val {
        let inferred_kind = explicit_kind
            .or_else(|| schema::schema_kind(ch, prop))
            .or_else(|| client.get(ch, prop).ok().map(|v| v.kind()));

        let parsed = match inferred_kind {
            Some(kind) => match Value::parse_with_kind(kind, &raw_val) {
                Ok(v) => v,
                Err(err) => {
                    eprintln!("uxconf-query: {err}");
                    return ExitCode::from(2);
                }
            },
            None => Value::infer_from_str(&raw_val),
        };

        match client.set(ch, prop, parsed) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("uxconf-query: {err}");
                ExitCode::FAILURE
            }
        }
    } else {
        match client.get(ch, prop) {
            Ok(val) => {
                println!("{}", val.display_plain());
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("uxconf-query: {err}");
                ExitCode::FAILURE
            }
        }
    }
}

fn run_monitor(channel: Option<&str>) -> ExitCode {
    let mut watch = match UxConfWatch::subscribe(channel) {
        Ok(w) => w,
        Err(err) => {
            eprintln!("uxconf-query: {err}");
            return ExitCode::FAILURE;
        }
    };

    // SAFETY: Async-signal-safe `AtomicBool::store` handler for clean Ctrl+C exit.
    unsafe {
        libc::signal(libc::SIGINT, handle_int as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_int as *const () as libc::sighandler_t);
    }

    match channel {
        Some(ch) => println!("Start monitoring channel {ch:?}:"),
        None => println!("Start monitoring all channels:"),
    }

    while !STOP_MONITOR.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd {
            fd: watch.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: Single valid `pollfd` passed to `poll(2)`.
        let rc = unsafe { libc::poll(&mut pfd, 1, 250) };
        if rc > 0 {
            for ev in watch.drain_events() {
                match ev.value {
                    Some(val) => {
                        println!(
                            "set: [{}] {} = {}",
                            ev.channel,
                            ev.property,
                            val.display_plain()
                        )
                    }
                    None => println!("reset: [{}] {}", ev.channel, ev.property),
                }
            }
            if pfd.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                break;
            }
        }
    }
    ExitCode::SUCCESS
}
