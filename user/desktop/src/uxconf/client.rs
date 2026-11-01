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
use std::time::Duration;

fn connect_with_autospawn(socket_path: &Path) -> Result<UnixStream, ConfError> {
    if let Ok(stream) = UnixStream::connect(socket_path) {
        return Ok(stream);
    }

    let autospawn_disabled = std::env::var("UXCONF_NO_AUTOSPAWN")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    if !autospawn_disabled && Path::new("/usr/bin/uxconfd").exists() {
        let _ = Command::new("/usr/bin/uxconfd")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    for _ in 0..25 {
        thread::sleep(Duration::from_millis(20));
        if let Ok(stream) = UnixStream::connect(socket_path) {
            return Ok(stream);
        }
    }
    Err(ConfError::Io(format!(
        "unable to connect to uxconfd at {}",
        socket_path.display()
    )))
}

fn read_bounded_line(reader: &mut BufReader<UnixStream>) -> Result<String, ConfError> {
    let mut buf = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|e| ConfError::Io(e.to_string()))?;
        if available.is_empty() {
            return Err(ConfError::Io("uxconfd closed connection".to_string()));
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            if buf.len() + pos > MAX_LINE_BYTES {
                return Err(ConfError::Protocol("response frame exceeds limit".into()));
            }
            buf.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            break;
        }
        if buf.len() + available.len() > MAX_LINE_BYTES {
            return Err(ConfError::Protocol("response frame exceeds limit".into()));
        }
        let len = available.len();
        buf.extend_from_slice(available);
        reader.consume(len);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|_| ConfError::Protocol("invalid UTF-8 from daemon".into()))
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

/// Synchronous request/response client for `uxconfd`.
pub struct UxConfClient {
    socket_path: PathBuf,
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl UxConfClient {
    pub fn connect() -> Result<Self, ConfError> {
        Self::connect_path(&resolve_socket_path())
    }

    pub fn connect_path(socket_path: &Path) -> Result<Self, ConfError> {
        let stream = connect_with_autospawn(socket_path)?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let reader_stream = stream
            .try_clone()
            .map_err(|e| ConfError::Io(e.to_string()))?;
        Ok(Self {
            socket_path: socket_path.to_path_buf(),
            writer: stream,
            reader: BufReader::new(reader_stream),
        })
    }

    fn reconnect(&mut self) -> Result<(), ConfError> {
        let stream = connect_with_autospawn(&self.socket_path)?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let reader_stream = stream
            .try_clone()
            .map_err(|e| ConfError::Io(e.to_string()))?;
        self.writer = stream;
        self.reader = BufReader::new(reader_stream);
        Ok(())
    }

    fn send_and_read_header(&mut self, req: &Request) -> Result<String, ConfError> {
        let frame = req.encode();
        if self.writer.write_all(frame.as_bytes()).is_err() || self.writer.flush().is_err() {
            self.reconnect()?;
            self.writer
                .write_all(frame.as_bytes())
                .and_then(|_| self.writer.flush())
                .map_err(|e| ConfError::Io(e.to_string()))?;
        }
        match read_bounded_line(&mut self.reader) {
            Ok(line) => Ok(line),
            Err(_) => {
                self.reconnect()?;
                self.writer
                    .write_all(frame.as_bytes())
                    .and_then(|_| self.writer.flush())
                    .map_err(|e| ConfError::Io(e.to_string()))?;
                read_bounded_line(&mut self.reader)
            }
        }
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
        let line = self.send_and_read_header(&Request::ListChannels)?;
        let Some(count_str) = line.strip_prefix("OK ") else {
            return Err(parse_err_line(&line));
        };
        let count: usize = count_str
            .trim()
            .parse()
            .map_err(|_| ConfError::Protocol(format!("bad channel count in {line:?}")))?;
        let mut out = Vec::with_capacity(count.min(256));
        for _ in 0..count {
            out.push(read_bounded_line(&mut self.reader)?);
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
        let line = self.send_and_read_header(&req)?;
        let Some(count_str) = line.strip_prefix("OK ") else {
            return Err(parse_err_line(&line));
        };
        let count: usize = count_str
            .trim()
            .parse()
            .map_err(|_| ConfError::Protocol(format!("bad property count in {line:?}")))?;
        let mut map = BTreeMap::new();
        for _ in 0..count {
            let entry = read_bounded_line(&mut self.reader)?;
            let Some((prop, val_raw)) = entry.split_once('\t') else {
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
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let req = Request::Subscribe {
            filter: filter.map(str::to_string),
        };
        stream
            .write_all(req.encode().as_bytes())
            .and_then(|_| stream.flush())
            .map_err(|e| ConfError::Io(e.to_string()))?;

        let cloned = stream
            .try_clone()
            .map_err(|e| ConfError::Io(e.to_string()))?;
        let mut reader = BufReader::new(cloned);
        let reply = read_bounded_line(&mut reader)?;
        if !reply.starts_with("OK") {
            return Err(parse_err_line(&reply));
        }

        // Preserve any bytes already buffered right after the OK handshake line.
        let buffered = reader.buffer().to_vec();
        let _ = stream.set_read_timeout(None);
        stream
            .set_nonblocking(true)
            .map_err(|e| ConfError::Io(e.to_string()))?;

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
