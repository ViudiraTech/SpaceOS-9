/*
 *
 *       user/desktop/src/uxconf/protocol.rs
 *       Line-framed AF_UNIX IPC wire protocol for uxconfd and clients
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Text-framed wire protocol transported over `AF_UNIX` `SOCK_STREAM`
//! (`/run/uxconfd.sock`). Every command, reply header and pushed event is a
//! single `\n`-terminated UTF-8 line capped at `MAX_LINE_BYTES` (16 KiB).

use crate::{validate_channel, validate_property, ChangeEvent, ConfError, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_SOCKET_PATH: &str = "/run/uxconfd.sock";
pub const FALLBACK_SOCKET_PATH: &str = "/tmp/uxconfd.sock";
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// Resolve the active `uxconfd` socket path, honoring `UXCONF_SOCKET`.
pub fn resolve_socket_path() -> PathBuf {
    if let Ok(custom) = std::env::var("UXCONF_SOCKET") {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }
    let primary = Path::new(DEFAULT_SOCKET_PATH);
    if primary.exists() {
        return primary.to_path_buf();
    }
    let fallback = Path::new(FALLBACK_SOCKET_PATH);
    if fallback.exists() {
        return fallback.to_path_buf();
    }
    if Path::new("/run").is_dir() && is_dir_writable(Path::new("/run")) {
        primary.to_path_buf()
    } else {
        fallback.to_path_buf()
    }
}

fn is_dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".uxconf_probe_{}", std::process::id()));
    if fs::write(&probe, b"").is_ok() {
        let _ = fs::remove_file(&probe);
        true
    } else {
        false
    }
}

/// Client-to-daemon request frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Ping,
    ListChannels,
    Get {
        channel: String,
        property: String,
    },
    GetAll {
        channel: String,
        prefix: String,
    },
    Set {
        channel: String,
        property: String,
        value: Value,
    },
    Reset {
        channel: String,
        property: String,
        recursive: bool,
    },
    FactoryReset,
    Subscribe {
        filter: Option<String>,
    },
}

impl Request {
    pub fn encode(&self) -> String {
        match self {
            Self::Ping => "PING\n".to_string(),
            Self::ListChannels => "LIST_CHANNELS\n".to_string(),
            Self::Get { channel, property } => format!("GET {channel} {property}\n"),
            Self::GetAll { channel, prefix } => format!("GET_ALL {channel} {prefix}\n"),
            Self::Set {
                channel,
                property,
                value,
            } => format!("SET {channel} {property} {}\n", value.encode_typed()),
            Self::Reset {
                channel,
                property,
                recursive,
            } => format!(
                "RESET {channel} {property} {}\n",
                if *recursive { 1 } else { 0 }
            ),
            Self::FactoryReset => "FACTORY_RESET\n".to_string(),
            Self::Subscribe { filter } => match filter {
                Some(ch) => format!("SUBSCRIBE {ch}\n"),
                None => "SUBSCRIBE *\n".to_string(),
            },
        }
    }

    pub fn decode(line: &str) -> Result<Self, ConfError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err(ConfError::Protocol("empty request".to_string()));
        }
        let (cmd, rest) = match trimmed.split_once(' ') {
            Some((c, r)) => (c, r.trim()),
            None => (trimmed, ""),
        };
        match cmd {
            "PING" => Ok(Self::Ping),
            "LIST_CHANNELS" => Ok(Self::ListChannels),
            "GET" => {
                let (ch_raw, prop_raw) = rest.split_once(' ').ok_or_else(|| {
                    ConfError::Protocol("GET requires <channel> <property>".into())
                })?;
                let channel = validate_channel(ch_raw)?;
                let property = validate_property(prop_raw.trim(), false)?.to_string();
                Ok(Self::Get { channel, property })
            }
            "GET_ALL" => {
                let (ch_raw, pfx_raw) = match rest.split_once(' ') {
                    Some((c, p)) => (c, p.trim()),
                    None => (rest, "/"),
                };
                let channel = validate_channel(ch_raw)?;
                let prefix = validate_property(pfx_raw, true)?.to_string();
                Ok(Self::GetAll { channel, prefix })
            }
            "SET" => {
                let (ch_raw, tail) = rest.split_once(' ').ok_or_else(|| {
                    ConfError::Protocol("SET requires <channel> <property> <value>".into())
                })?;
                let (prop_raw, val_raw) = tail
                    .trim_start()
                    .split_once(' ')
                    .ok_or_else(|| ConfError::Protocol("SET requires <property> <value>".into()))?;
                let channel = validate_channel(ch_raw)?;
                let property = validate_property(prop_raw.trim(), false)?.to_string();
                let value = Value::decode_typed(val_raw.trim())?;
                Ok(Self::Set {
                    channel,
                    property,
                    value,
                })
            }
            "RESET" => {
                let mut parts = rest.split_whitespace();
                let ch_raw = parts
                    .next()
                    .ok_or_else(|| ConfError::Protocol("RESET missing channel".into()))?;
                let prop_raw = parts
                    .next()
                    .ok_or_else(|| ConfError::Protocol("RESET missing property".into()))?;
                let rec_raw = parts.next().unwrap_or("0");
                let recursive = rec_raw == "1" || rec_raw.eq_ignore_ascii_case("true");
                let channel = validate_channel(ch_raw)?;
                let property = validate_property(prop_raw, recursive)?.to_string();
                Ok(Self::Reset {
                    channel,
                    property,
                    recursive,
                })
            }
            "SUBSCRIBE" => {
                if rest.is_empty() || rest == "*" {
                    Ok(Self::Subscribe { filter: None })
                } else {
                    let channel = validate_channel(rest)?;
                    Ok(Self::Subscribe {
                        filter: Some(channel),
                    })
                }
            }
            "FACTORY_RESET" => Ok(Self::FactoryReset),
            other => Err(ConfError::Protocol(format!("unknown command {other:?}"))),
        }
    }
}

/// Encode a `ChangeEvent` into an asynchronous server-push wire frame.
pub fn encode_event(ev: &ChangeEvent) -> String {
    match &ev.value {
        Some(val) => format!(
            "EVENT CHANGED {} {} {}\n",
            ev.channel,
            ev.property,
            val.encode_typed()
        ),
        None => format!("EVENT REMOVED {} {}\n", ev.channel, ev.property),
    }
}

/// Decode a single `EVENT ...` server-push line into a `ChangeEvent`.
pub fn decode_event(line: &str) -> Result<ChangeEvent, ConfError> {
    let rest = line
        .trim()
        .strip_prefix("EVENT ")
        .ok_or_else(|| ConfError::Protocol("not an EVENT frame".to_string()))?;
    if let Some(body) = rest.strip_prefix("CHANGED ") {
        let (ch_raw, tail) = body
            .split_once(' ')
            .ok_or_else(|| ConfError::Protocol("malformed EVENT CHANGED".to_string()))?;
        let (prop_raw, val_raw) = tail
            .trim_start()
            .split_once(' ')
            .ok_or_else(|| ConfError::Protocol("malformed EVENT CHANGED payload".to_string()))?;
        let channel = validate_channel(ch_raw)?;
        let property = validate_property(prop_raw, false)?.to_string();
        let value = Value::decode_typed(val_raw)?;
        Ok(ChangeEvent {
            channel,
            property,
            value: Some(value),
        })
    } else if let Some(body) = rest.strip_prefix("REMOVED ") {
        let (ch_raw, prop_raw) = body
            .split_once(' ')
            .ok_or_else(|| ConfError::Protocol("malformed EVENT REMOVED".to_string()))?;
        let channel = validate_channel(ch_raw)?;
        let property = validate_property(prop_raw.trim(), false)?.to_string();
        Ok(ChangeEvent {
            channel,
            property,
            value: None,
        })
    } else {
        Err(ConfError::Protocol(format!(
            "unknown EVENT kind in {line:?}"
        )))
    }
}
