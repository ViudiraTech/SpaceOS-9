/*
 *
 *       user/desktop/src/uxconf/client.rs
 *       Synchronous RPC client and non-blocking event subscription watch
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Client bindings to `uxconfd`:
//! - [`UxConfClient`] performs synchronous `GET`, `GET_ALL`, `SET`, `RESET`
//!   and `LIST_CHANNELS` calls, auto-activating `/usr/bin/uxconfd` if needed.
//! - [`UxConfWatch`] maintains a non-blocking `SUBSCRIBE` socket whose raw
//!   descriptor integrates directly into each GUI application's `poll(2)` loop.

use crate::protocol::{decode_event, resolve_socket_path, Request, MAX_LINE_BYTES};
use crate::schema::{self, BUILTIN_CHANNELS};
use crate::{ChangeEvent, ConfError, DesktopConfig, Value};
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_millis(1500);

fn connect_with_autospawn(socket_path: &Path) -> Result<UnixStream, ConfError> {
    if let Ok(stream) = UnixStream::connect(socket_path) {
        return Ok(stream);
    }

    let autospawn_disabled = std::env::var("UXCONF_NO_AUTOSPAWN")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let mut spawned = false;
    if !autospawn_disabled && Path::new("/usr/bin/uxconfd").exists() {
        spawned = Command::new("/usr/bin/uxconfd")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok();
    }

    if spawned {
        for _ in 0..25 {
            thread::sleep(Duration::from_millis(20));
            if let Ok(stream) = UnixStream::connect(socket_path) {
                return Ok(stream);
            }
        }
    }
    Err(ConfError::Io(format!(
        "unable to connect to uxconfd at {}",
        socket_path.display()
    )))
}

enum TransportError {
    TimedOut(ConfError),
    Disconnected(ConfError),
    Other(ConfError),
}

impl TransportError {
    fn into_conf(self) -> ConfError {
        match self {
            Self::TimedOut(e) | Self::Disconnected(e) | Self::Other(e) => e,
        }
    }
}

/// Wait until `fd` is ready for `events` (or hung up / errored) before `deadline`.
/// SpaceKernel's AF_UNIX sockets ignore `SO_RCVTIMEO`/`SO_SNDTIMEO`, so explicit
/// `poll(2)` bounds are required to guarantee RPC calls never block indefinitely.
fn poll_fd_deadline(fd: i32, events: i16, deadline: Instant) -> Result<(), TransportError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TransportError::TimedOut(ConfError::Io(
                "uxconfd IPC timed out".to_string(),
            )));
        }
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128).max(1) as i32;
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if rc > 0 {
            if (pfd.revents & libc::POLLNVAL) != 0 {
                return Err(TransportError::Disconnected(ConfError::Io(
                    "uxconfd socket fd invalid".to_string(),
                )));
            }
            if (pfd.revents & (events | libc::POLLERR | libc::POLLHUP)) != 0 {
                return Ok(());
            }
        } else if rc == 0 {
            return Err(TransportError::TimedOut(ConfError::Io(
                "uxconfd IPC timed out".to_string(),
            )));
        } else {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(TransportError::Disconnected(ConfError::Io(err.to_string())));
        }
    }
}

fn is_disconnect_io_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::NotConnected
    )
}

fn write_all_deadline(
    stream: &mut UnixStream,
    mut data: &[u8],
    deadline: Instant,
) -> Result<(), TransportError> {
    let fd = stream.as_raw_fd();
    while !data.is_empty() {
        match stream.write(data) {
            Ok(0) => {
                return Err(TransportError::Disconnected(ConfError::Io(
                    "uxconfd write returned 0".to_string(),
                )));
            }
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                poll_fd_deadline(fd, libc::POLLOUT, deadline)?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if is_disconnect_io_error(&e) => {
                return Err(TransportError::Disconnected(ConfError::Io(e.to_string())));
            }
            Err(e) => {
                return Err(TransportError::Other(ConfError::Io(e.to_string())));
            }
        }
    }
    Ok(())
}

fn read_bounded_line_deadline(
    reader: &mut BufReader<UnixStream>,
    deadline: Instant,
) -> Result<String, TransportError> {
    let mut buf = Vec::new();
    let fd = reader.get_ref().as_raw_fd();
    loop {
        if reader.buffer().is_empty() {
            poll_fd_deadline(fd, libc::POLLIN, deadline)?;
        }
        let available = match reader.fill_buf() {
            Ok(avail) => avail,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                poll_fd_deadline(fd, libc::POLLIN, deadline)?;
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if is_disconnect_io_error(&e) => {
                return Err(TransportError::Disconnected(ConfError::Io(e.to_string())));
            }
            Err(e) => {
                return Err(TransportError::Other(ConfError::Io(e.to_string())));
            }
        };
        if available.is_empty() {
            return Err(TransportError::Disconnected(ConfError::Io(
                "uxconfd closed connection".to_string(),
            )));
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            if buf.len() + pos > MAX_LINE_BYTES {
                return Err(TransportError::Other(ConfError::Protocol(
                    "response frame exceeds limit".into(),
                )));
            }
            buf.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            break;
        }
        if buf.len() + available.len() > MAX_LINE_BYTES {
            return Err(TransportError::Other(ConfError::Protocol(
                "response frame exceeds limit".into(),
            )));
        }
        let len = available.len();
        buf.extend_from_slice(available);
        reader.consume(len);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf)
        .map_err(|_| TransportError::Other(ConfError::Protocol("invalid UTF-8 from daemon".into())))
}

fn parse_err_line(line: &str) -> ConfError {
    let rest = line.strip_prefix("ERR ").unwrap_or(line).trim();
    if rest.starts_with("NOT_FOUND") {
        ConfError::NotFound
    } else if let Some(msg) = rest.strip_prefix("INVALID_CHANNEL ") {
        ConfError::InvalidChannel(msg.to_string())
    } else if let Some(msg) = rest.strip_prefix("INVALID_PROPERTY ") {
        ConfError::InvalidProperty(msg.to_string())
    } else if let Some(msg) = rest.strip_prefix("INVALID_VALUE ") {
        ConfError::InvalidValue(msg.to_string())
    } else {
        ConfError::Protocol(rest.to_string())
    }
}

fn configure_client_stream(stream: &UnixStream, timeout: Duration) -> Result<(), ConfError> {
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    stream
        .set_nonblocking(true)
        .map_err(|e| ConfError::Io(e.to_string()))
}

/// Synchronous request/response client for `uxconfd`.
pub struct UxConfClient {
    socket_path: PathBuf,
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    timeout: Duration,
    needs_reconnect: bool,
}

impl UxConfClient {
    pub fn connect() -> Result<Self, ConfError> {
        Self::connect_path(&resolve_socket_path())
    }

    pub fn connect_path(socket_path: &Path) -> Result<Self, ConfError> {
        let timeout = DEFAULT_RPC_TIMEOUT;
        let stream = connect_with_autospawn(socket_path)?;
        configure_client_stream(&stream, timeout)?;
        let reader_stream = stream
            .try_clone()
            .map_err(|e| ConfError::Io(e.to_string()))?;
        Ok(Self {
            socket_path: socket_path.to_path_buf(),
            writer: stream,
            reader: BufReader::new(reader_stream),
            timeout,
            needs_reconnect: false,
        })
    }

    /// Override the bounded RPC deadline used for request writes and reply reads.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout.max(Duration::from_millis(10));
        let _ = self.writer.set_read_timeout(Some(self.timeout));
        let _ = self.writer.set_write_timeout(Some(self.timeout));
    }

    fn reconnect(&mut self) -> Result<(), ConfError> {
        let stream = connect_with_autospawn(&self.socket_path)?;
        configure_client_stream(&stream, self.timeout)?;
        let reader_stream = stream
            .try_clone()
            .map_err(|e| ConfError::Io(e.to_string()))?;
        self.writer = stream;
        self.reader = BufReader::new(reader_stream);
        self.needs_reconnect = false;
        Ok(())
    }

    fn roundtrip_header(
        &mut self,
        frame: &[u8],
        deadline: Instant,
    ) -> Result<String, TransportError> {
        write_all_deadline(&mut self.writer, frame, deadline)?;
        read_bounded_line_deadline(&mut self.reader, deadline)
    }

    fn send_and_read_header_with_deadline(
        &mut self,
        req: &Request,
    ) -> Result<(String, Instant), ConfError> {
        if self.needs_reconnect {
            self.reconnect()?;
        }
        let frame = req.encode();
        let deadline = Instant::now() + self.timeout;
        match self.roundtrip_header(frame.as_bytes(), deadline) {
            Ok(line) => Ok((line, deadline)),
            Err(TransportError::Disconnected(_)) => {
                // Only auto-reconnect when the prior socket cleanly died (EOF /
                // BrokenPipe). Never wait a second timeout when uxconfd stalled.
                self.reconnect()?;
                let retry_deadline = Instant::now() + self.timeout;
                match self.roundtrip_header(frame.as_bytes(), retry_deadline) {
                    Ok(line) => Ok((line, retry_deadline)),
                    Err(err) => {
                        self.needs_reconnect = true;
                        Err(err.into_conf())
                    }
                }
            }
            Err(err) => {
                self.needs_reconnect = true;
                Err(err.into_conf())
            }
        }
    }

    fn send_and_read_header(&mut self, req: &Request) -> Result<String, ConfError> {
        self.send_and_read_header_with_deadline(req)
            .map(|(line, _)| line)
    }

    pub fn ping(&mut self) -> Result<(), ConfError> {
        let line = self.send_and_read_header(&Request::Ping)?;
        if line.starts_with("OK") {
            Ok(())
        } else {
            Err(parse_err_line(&line))
        }
    }

    pub fn list_channels(&mut self) -> Result<Vec<String>, ConfError> {
        let (line, deadline) = self.send_and_read_header_with_deadline(&Request::ListChannels)?;
        let Some(count_str) = line.strip_prefix("OK ") else {
            return Err(parse_err_line(&line));
        };
        let count: usize = count_str
            .trim()
            .parse()
            .map_err(|_| ConfError::Protocol(format!("bad channel count in {line:?}")))?;
        let mut out = Vec::with_capacity(count.min(256));
        for _ in 0..count {
            let entry = read_bounded_line_deadline(&mut self.reader, deadline).map_err(|e| {
                self.needs_reconnect = true;
                e.into_conf()
            })?;
            out.push(entry);
        }
        Ok(out)
    }

    pub fn get(&mut self, channel: &str, property: &str) -> Result<Value, ConfError> {
        let req = Request::Get {
            channel: channel.to_string(),
            property: property.to_string(),
        };
        let line = self.send_and_read_header(&req)?;
        if let Some(val_raw) = line.strip_prefix("OK VALUE ") {
            Value::decode_typed(val_raw)
        } else {
            Err(parse_err_line(&line))
        }
    }

    pub fn get_all(
        &mut self,
        channel: &str,
        prefix: &str,
    ) -> Result<BTreeMap<String, Value>, ConfError> {
        let req = Request::GetAll {
            channel: channel.to_string(),
            prefix: prefix.to_string(),
        };
        let (line, deadline) = self.send_and_read_header_with_deadline(&req)?;
        let Some(count_str) = line.strip_prefix("OK ") else {
            return Err(parse_err_line(&line));
        };
        let count: usize = count_str
            .trim()
            .parse()
            .map_err(|_| ConfError::Protocol(format!("bad property count in {line:?}")))?;
        let mut map = BTreeMap::new();
        for _ in 0..count {
            let entry = read_bounded_line_deadline(&mut self.reader, deadline).map_err(|e| {
                self.needs_reconnect = true;
                e.into_conf()
            })?;
            let Some((prop, val_raw)) = entry.split_once('\t') else {
                self.needs_reconnect = true;
                return Err(ConfError::Protocol(format!("bad GET_ALL entry {entry:?}")));
            };
            map.insert(prop.to_string(), Value::decode_typed(val_raw)?);
        }
        Ok(map)
    }

    pub fn set(&mut self, channel: &str, property: &str, value: Value) -> Result<(), ConfError> {
        let req = Request::Set {
            channel: channel.to_string(),
            property: property.to_string(),
            value,
        };
        let line = self.send_and_read_header(&req)?;
        if line.starts_with("OK") {
            Ok(())
        } else {
            Err(parse_err_line(&line))
        }
    }

    pub fn reset(
        &mut self,
        channel: &str,
        property: &str,
        recursive: bool,
    ) -> Result<usize, ConfError> {
        let req = Request::Reset {
            channel: channel.to_string(),
            property: property.to_string(),
            recursive,
        };
        let line = self.send_and_read_header(&req)?;
        if let Some(rest) = line.strip_prefix("OK") {
            Ok(rest.trim().parse::<usize>().unwrap_or(0))
        } else {
            Err(parse_err_line(&line))
        }
    }

    pub fn factory_reset(&mut self) -> Result<(), ConfError> {
        let line = self.send_and_read_header(&Request::FactoryReset)?;
        if line.starts_with("OK") {
            Ok(())
        } else {
            Err(parse_err_line(&line))
        }
    }

    /// Fetch all built-in desktop channels into a strongly-typed `DesktopConfig`.
    pub fn load_desktop_config(&mut self) -> DesktopConfig {
        let mut cfg = DesktopConfig::default();
        for &ch in &BUILTIN_CHANNELS {
            if let Ok(entries) = self.get_all(ch, "/") {
                for (prop, val) in entries {
                    cfg.apply_property(ch, &prop, &val);
                }
            }
        }
        cfg
    }
}

/// Non-blocking subscription handle receiving live `PropertyChanged` events.
pub struct UxConfWatch {
    stream: UnixStream,
    rx_buf: Vec<u8>,
}

impl UxConfWatch {
    /// Subscribe to a specific `channel` (`Some("panel")`) or all channels (`None`).
    pub fn subscribe(filter: Option<&str>) -> Result<Self, ConfError> {
        Self::subscribe_path(&resolve_socket_path(), filter)
    }

    pub fn subscribe_path(socket_path: &Path, filter: Option<&str>) -> Result<Self, ConfError> {
        let mut stream = connect_with_autospawn(socket_path)?;
        stream
            .set_nonblocking(true)
            .map_err(|e| ConfError::Io(e.to_string()))?;
        let deadline = Instant::now() + DEFAULT_RPC_TIMEOUT;
        let req = Request::Subscribe {
            filter: filter.map(str::to_string),
        };
        write_all_deadline(&mut stream, req.encode().as_bytes(), deadline)
            .map_err(TransportError::into_conf)?;

        let cloned = stream
            .try_clone()
            .map_err(|e| ConfError::Io(e.to_string()))?;
        let mut reader = BufReader::new(cloned);
        let reply = read_bounded_line_deadline(&mut reader, deadline)
            .map_err(TransportError::into_conf)?;
        if !reply.starts_with("OK") {
            return Err(parse_err_line(&reply));
        }

        // Preserve any bytes already buffered right after the OK handshake line.
        let buffered = reader.buffer().to_vec();
        Ok(Self {
            stream,
            rx_buf: buffered,
        })
    }

    /// File descriptor suitable for `libc::poll` (`POLLIN`).
    pub fn as_raw_fd(&self) -> i32 {
        self.stream.as_raw_fd()
    }

    /// Drain all currently available `ChangeEvent` frames without blocking.
    pub fn drain_events(&mut self) -> Vec<ChangeEvent> {
        let mut scratch = [0u8; 4096];
        loop {
            match self.stream.read(&mut scratch) {
                Ok(0) => break,
                Ok(n) => {
                    self.rx_buf.extend_from_slice(&scratch[..n]);
                    if self.rx_buf.len() > MAX_LINE_BYTES * 8 {
                        self.rx_buf.clear();
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }

        let mut events = Vec::new();
        while let Some(pos) = self.rx_buf.iter().position(|&b| b == b'\n') {
            let mut line_bytes: Vec<u8> = self.rx_buf.drain(..=pos).collect();
            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            if let Ok(line) = std::str::from_utf8(&line_bytes) {
                if let Ok(ev) = decode_event(line) {
                    events.push(ev);
                }
            }
        }
        events
    }

    /// Drain pending events and apply them directly onto `config`.
    /// Returns `(changed, events)` where `changed` indicates whether `config` mutated.
    pub fn poll_into(&mut self, config: &mut DesktopConfig) -> (bool, Vec<ChangeEvent>) {
        let events = self.drain_events();
        let mut changed = false;
        for ev in &events {
            if let Some(val) = &ev.value {
                if config.apply_property(&ev.channel, &ev.property, val) {
                    changed = true;
                }
            } else if let Some(default_val) = schema::schema_default(&ev.channel, &ev.property) {
                if config.apply_property(&ev.channel, &ev.property, &default_val) {
                    changed = true;
                }
            }
        }
        (changed, events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_sock_path(tag: &str) -> PathBuf {
        let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "spui_uxconf_client_{tag}_{}_{}.sock",
            std::process::id(),
            n
        ))
    }

    #[test]
    fn unresponsive_server_times_out_once_and_resyncs_on_next_call() {
        let sock_path = temp_sock_path("timeout");
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind unix listener");

        let server = thread::spawn(move || {
            // Connection 1: accept and hold open silently without writing a reply.
            let (stalled, _) = listener.accept().expect("accept stalled");
            // Connection 2: accept after client marks stream stale, read PING, reply OK.
            let (mut healthy, _) = listener.accept().expect("accept healthy");
            let mut r = BufReader::new(healthy.try_clone().expect("clone"));
            let mut line = String::new();
            r.read_line(&mut line).expect("read ping");
            assert_eq!(line.trim_end(), "PING");
            healthy.write_all(b"OK PONG\n").expect("write pong");
            drop(stalled);
        });

        let mut client = UxConfClient::connect_path(&sock_path).expect("connect");
        client.set_timeout(Duration::from_millis(80));

        let start = Instant::now();
        let err = client.ping().expect_err("stalled server must time out");
        let elapsed = start.elapsed();
        assert!(
            matches!(err, ConfError::Io(ref msg) if msg.contains("timed out")),
            "unexpected error: {err:?}"
        );
        // Should return after ~1x timeout (not reconnect+double-wait).
        assert!(
            elapsed < Duration::from_millis(240),
            "elapsed too long (double wait?): {elapsed:?}"
        );

        // Next call should transparently open a fresh socket instead of reading stale state.
        client.ping().expect("fresh reconnect should succeed");
        server.join().expect("server thread");
        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn dead_connection_auto_reconnects_and_retries() {
        let sock_path = temp_sock_path("reconnect");
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind unix listener");

        let server = thread::spawn(move || {
            // Connection 1: immediately drop to simulate daemon restart while idle.
            let (dead, _) = listener.accept().expect("accept dead");
            drop(dead);
            // Connection 2: reply cleanly to retried PING.
            let (mut healthy, _) = listener.accept().expect("accept healthy");
            let mut r = BufReader::new(healthy.try_clone().expect("clone"));
            let mut line = String::new();
            r.read_line(&mut line).expect("read ping");
            assert_eq!(line.trim_end(), "PING");
            healthy.write_all(b"OK PONG\n").expect("write pong");
        });

        let mut client = UxConfClient::connect_path(&sock_path).expect("connect");
        client.set_timeout(Duration::from_millis(200));
        // Give the server thread a brief moment to drop connection 1 cleanly.
        thread::sleep(Duration::from_millis(20));
        client.ping().expect("auto reconnect on EOF/EPIPE should succeed");
        server.join().expect("server thread");
        let _ = std::fs::remove_file(&sock_path);
    }
}
