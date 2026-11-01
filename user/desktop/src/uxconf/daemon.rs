/*
 *
 *       user/desktop/src/uxconf/daemon.rs
 *       Single-process non-blocking poll(2) configuration server engine
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Event-driven `AF_UNIX` server engine powering `/usr/bin/uxconfd`.
//! Multiplexes the listening socket and all client/subscriber connections
//! via `poll(2)` without threads or blocking writes.

use crate::protocol::{encode_event, Request, MAX_LINE_BYTES};
use crate::storage::ConfigStore;
use crate::{ChangeEvent, ConfError};
use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_CLIENTS: usize = 256;
const MAX_TX_BUFFER: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Subscription {
    All,
    Channel(String),
}

impl Subscription {
    fn matches(&self, channel: &str) -> bool {
        match self {
            Self::All => true,
            Self::Channel(target) => target == channel,
        }
    }
}

struct ClientConn {
    stream: UnixStream,
    rx_buf: Vec<u8>,
    tx_buf: VecDeque<u8>,
    subscription: Option<Subscription>,
    closed: bool,
}

impl ClientConn {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            rx_buf: Vec::with_capacity(256),
            tx_buf: VecDeque::with_capacity(256),
            subscription: None,
            closed: false,
        }
    }

    fn queue_reply(&mut self, payload: &str) {
        if self.tx_buf.len() + payload.len() > MAX_TX_BUFFER {
            self.closed = true;
            return;
        }
        self.tx_buf.extend(payload.as_bytes());
    }

    fn flush_tx(&mut self) {
        while !self.tx_buf.is_empty() {
            let (head, _) = self.tx_buf.as_slices();
            if head.is_empty() {
                break;
            }
            match self.stream.write(head) {
                Ok(0) => {
                    self.closed = true;
                    break;
                }
                Ok(n) => {
                    self.tx_buf.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.closed = true;
                    break;
                }
            }
        }
    }
}

/// Active `uxconfd` server instance bound to a Unix domain socket.
pub struct Server {
    socket_path: PathBuf,
    listener: UnixListener,
    store: ConfigStore,
    clients: Vec<ClientConn>,
}

impl Server {
    /// Bind `socket_path` (cleaning up any stale socket left by an unclean shutdown).
    pub fn bind(socket_path: PathBuf, store: ConfigStore) -> Result<Self, ConfError> {
        if socket_path.exists() {
            // Check whether another uxconfd instance is actively serving this socket.
            if let Ok(mut probe) = UnixStream::connect(&socket_path) {
                let _ = probe.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = probe.set_write_timeout(Some(Duration::from_millis(200)));
                let mut reply = [0u8; 16];
                if probe.write_all(b"PING\n").is_ok()
                    && probe.read(&mut reply).ok().is_some_and(|n| n > 0)
                    && reply.starts_with(b"OK")
                {
                    return Err(ConfError::Io(format!(
                        "uxconfd is already running on {}",
                        socket_path.display()
                    )));
                }
            }
            let _ = fs::remove_file(&socket_path);
        }

        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ConfError::Io(format!("{}: {e}", parent.display())))?;
        }

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| ConfError::Io(format!("bind {}: {e}", socket_path.display())))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| ConfError::Io(e.to_string()))?;
        let _ = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o666));

        Ok(Self {
            socket_path,
            listener,
            store,
            clients: Vec::new(),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Run the `poll(2)` event loop until `stop` becomes `true`.
    pub fn run(&mut self, stop: &AtomicBool) -> Result<(), ConfError> {
        let mut pollfds = Vec::with_capacity(64);
        while !stop.load(Ordering::Relaxed) {
            self.step(&mut pollfds, 500)?;
        }
        Ok(())
    }

    /// Execute one `poll(2)` cycle with `timeout_ms`.
    pub fn step(
        &mut self,
        pollfds: &mut Vec<libc::pollfd>,
        timeout_ms: i32,
    ) -> Result<(), ConfError> {
        pollfds.clear();
        pollfds.push(libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        for client in &self.clients {
            let mut events = libc::POLLIN;
            if !client.tx_buf.is_empty() {
                events |= libc::POLLOUT;
            }
            pollfds.push(libc::pollfd {
                fd: client.stream.as_raw_fd(),
                events,
                revents: 0,
            });
        }

        // SAFETY: `pollfds` points to a contiguous valid slice of `pollfd` structs for the duration of `poll`.
        let rc = unsafe {
            libc::poll(
                pollfds.as_mut_ptr(),
                pollfds.len() as libc::nfds_t,
                timeout_ms,
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(());
            }
            return Err(ConfError::Io(format!("poll failed: {err}")));
        }
        if rc == 0 {
            return Ok(());
        }

        let listener_ready = pollfds[0].revents & libc::POLLIN != 0;
        let mut pending_broadcasts: Vec<ChangeEvent> = Vec::new();

        for (idx, pfd) in pollfds.iter().enumerate().skip(1) {
            let client_idx = idx - 1;
            if pfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                self.clients[client_idx].closed = true;
                continue;
            }
            if pfd.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                self.read_client(client_idx, &mut pending_broadcasts);
            }
            if !self.clients[client_idx].closed && pfd.revents & libc::POLLOUT != 0 {
                self.clients[client_idx].flush_tx();
            }
        }

        if !pending_broadcasts.is_empty() {
            for ev in &pending_broadcasts {
                let wire = encode_event(ev);
                for client in &mut self.clients {
                    if !client.closed
                        && client
                            .subscription
                            .as_ref()
                            .is_some_and(|sub| sub.matches(&ev.channel))
                    {
                        client.queue_reply(&wire);
                    }
                }
            }
        }

        // Attempt immediate non-blocking flush for newly queued replies & broadcasts.
        for client in &mut self.clients {
            if !client.closed && !client.tx_buf.is_empty() {
                client.flush_tx();
            }
        }

        self.clients.retain(|c| !c.closed);

        if listener_ready {
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => {
                        if self.clients.len() >= MAX_CLIENTS {
                            drop(stream);
                            continue;
                        }
                        if stream.set_nonblocking(true).is_ok() {
                            self.clients.push(ClientConn::new(stream));
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }

        Ok(())
    }

    fn read_client(&mut self, idx: usize, broadcasts: &mut Vec<ChangeEvent>) {
        let mut buf = [0u8; 4096];
        loop {
            match self.clients[idx].stream.read(&mut buf) {
                Ok(0) => {
                    self.clients[idx].closed = true;
                    break;
                }
                Ok(n) => {
                    self.clients[idx].rx_buf.extend_from_slice(&buf[..n]);
                    if self.clients[idx].rx_buf.len() > MAX_LINE_BYTES * 2 {
                        self.clients[idx].queue_reply("ERR PROTOCOL frame too large\n");
                        self.clients[idx].closed = true;
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.clients[idx].closed = true;
                    break;
                }
            }
        }

        while let Some(pos) = self.clients[idx].rx_buf.iter().position(|&b| b == b'\n') {
            let mut raw_line: Vec<u8> = self.clients[idx].rx_buf.drain(..=pos).collect();
            if raw_line.last() == Some(&b'\n') {
                raw_line.pop();
            }
            if raw_line.last() == Some(&b'\r') {
                raw_line.pop();
            }
            let Ok(line) = std::str::from_utf8(&raw_line) else {
                self.clients[idx].queue_reply("ERR PROTOCOL invalid UTF-8\n");
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            let reply = self.dispatch_line(idx, line, broadcasts);
            self.clients[idx].queue_reply(&reply);
        }
    }

    fn dispatch_line(
        &mut self,
        client_idx: usize,
        line: &str,
        broadcasts: &mut Vec<ChangeEvent>,
    ) -> String {
        let req = match Request::decode(line) {
            Ok(req) => req,
            Err(err) => return format_err(&err),
        };
        match req {
            Request::Ping => "OK PONG\n".to_string(),
            Request::ListChannels => {
                let channels = self.store.list_channels();
                let mut out = format!("OK {}\n", channels.len());
                for ch in channels {
                    out.push_str(&ch);
                    out.push('\n');
                }
                out
            }
            Request::Get { channel, property } => match self.store.get(&channel, &property) {
                Ok(val) => format!("OK VALUE {}\n", val.encode_typed()),
                Err(err) => format_err(&err),
            },
            Request::GetAll { channel, prefix } => match self.store.get_all(&channel, &prefix) {
                Ok(entries) => {
                    let mut out = format!("OK {}\n", entries.len());
                    for (prop, val) in entries {
                        out.push_str(&prop);
                        out.push('\t');
                        out.push_str(&val.encode_typed());
                        out.push('\n');
                    }
                    out
                }
                Err(err) => format_err(&err),
            },
            Request::Set {
                channel,
                property,
                value,
            } => match self.store.set(&channel, &property, value) {
                Ok(maybe_change) => {
                    if let Some(ev) = maybe_change {
                        broadcasts.push(ev);
                    }
                    "OK\n".to_string()
                }
                Err(err) => format_err(&err),
            },
            Request::Reset {
                channel,
                property,
                recursive,
            } => match self.store.reset(&channel, &property, recursive) {
                Ok(changes) => {
                    let count = changes.len();
                    broadcasts.extend(changes);
                    format!("OK {count}\n")
                }
                Err(err) => format_err(&err),
            },
            Request::Subscribe { filter } => {
                self.clients[client_idx].subscription = Some(match filter {
                    Some(ch) => Subscription::Channel(ch),
                    None => Subscription::All,
                });
                "OK\n".to_string()
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn format_err(err: &ConfError) -> String {
    match err {
        ConfError::NotFound => "ERR NOT_FOUND property not found\n".to_string(),
        ConfError::InvalidChannel(c) => format!("ERR INVALID_CHANNEL {c}\n"),
        ConfError::InvalidProperty(p) => format!("ERR INVALID_PROPERTY {p}\n"),
        ConfError::InvalidValue(v) => format!("ERR INVALID_VALUE {v}\n"),
        ConfError::TypeMismatch { expected, actual } => format!(
            "ERR INVALID_VALUE expected {}, got {}\n",
            expected.as_str(),
            actual.as_str()
        ),
        ConfError::Io(m) => format!("ERR IO {}\n", m.replace('\n', " ")),
        ConfError::Protocol(m) => format!("ERR PROTOCOL {}\n", m.replace('\n', " ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{UxConfClient, UxConfWatch};
    use crate::{DesktopConfig, ThemeMode, Value};
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn daemon_roundtrip_persistence_reset_and_live_subscription() {
        let base = std::env::temp_dir().join(format!(
            "uxconf_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sys_dir = base.join("sys");
        let usr_dir = base.join("usr");
        let sock_path = base.join("uxconfd.sock");
        fs::create_dir_all(&sys_dir).unwrap();
        fs::create_dir_all(&usr_dir).unwrap();

        let store = ConfigStore::open(sys_dir.clone(), usr_dir.clone());
        let mut server = Server::bind(sock_path.clone(), store).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();

        let handle = thread::spawn(move || {
            let mut fds = Vec::new();
            while !stop_bg.load(Ordering::Relaxed) {
                server.step(&mut fds, 20).unwrap();
            }
        });

        let mut client = UxConfClient::connect_path(&sock_path).unwrap();
        let mut watch_all = UxConfWatch::subscribe_path(&sock_path, None).unwrap();
        let mut watch_panel = UxConfWatch::subscribe_path(&sock_path, Some("panel")).unwrap();

        // Built-in defaults are readable immediately.
        assert_eq!(
            client.get("appearance", "/theme/mode").unwrap(),
            Value::String("dark".to_string())
        );
        assert!(client
            .list_channels()
            .unwrap()
            .contains(&"panel".to_string()));

        // Type mismatch against schema is rejected cleanly.
        assert!(client
            .set("panel", "/size", Value::String("huge".into()))
            .is_err());

        // Valid mutations persist to disk and push live notifications to matching subscribers.
        client
            .set("appearance", "/theme/mode", Value::String("light".into()))
            .unwrap();
        client.set("panel", "/size", Value::Int(54)).unwrap();

        thread::sleep(Duration::from_millis(50));

        let mut cfg = DesktopConfig::default();
        let (changed, all_events) = watch_all.poll_into(&mut cfg);
        assert!(changed);
        assert_eq!(all_events.len(), 2);
        assert_eq!(cfg.mode, ThemeMode::Light);
        assert_eq!(cfg.panel_size, 54);

        let panel_events = watch_panel.drain_events();
        assert_eq!(panel_events.len(), 1);
        assert_eq!(panel_events[0].channel, "panel");
        assert_eq!(panel_events[0].property, "/size");

        // User override file exists on disk and survives a fresh ConfigStore reload.
        let reloaded = ConfigStore::open(sys_dir, usr_dir);
        assert_eq!(reloaded.get("panel", "/size").unwrap(), Value::Int(54));

        // Resetting the channel restores schema default and broadcasts the reverted value.
        let reset_count = client.reset("panel", "/", true).unwrap();
        assert_eq!(reset_count, 1);
        assert_eq!(client.get("panel", "/size").unwrap(), Value::Int(46));

        thread::sleep(Duration::from_millis(50));
        let (reverted, _) = watch_all.poll_into(&mut cfg);
        assert!(reverted);
        assert_eq!(cfg.panel_size, 46);

        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        let _ = fs::remove_dir_all(&base);
    }
}
