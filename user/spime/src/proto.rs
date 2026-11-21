/*
 *
 *       user/spime/src/proto.rs
 *       Framing-safe Unix Domain Socket binary IPC protocol between SPUI/browser
 *       clients and `spimed`.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use std::io::{self, Read, Write};
use std::path::PathBuf;

pub const MAX_PACKET_BYTES: usize = 64 * 1024;

/// Resolve the Unix domain socket path used by `spimed` and `SpimeClient`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SPIME_SOCKET").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(rt).join("spime.sock");
    }
    let uid = unsafe { libc::geteuid() };
    PathBuf::from(format!("/tmp/spime-{uid}.sock"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientRequest {
    FocusIn {
        client_id: u32,
        window_id: u32,
    },
    FocusOut {
        client_id: u32,
        window_id: u32,
    },
    SetSpot {
        client_id: u32,
        x: i16,
        y: i16,
        h: i16,
    },
    KeyEvent {
        client_id: u32,
        window_id: u32,
        is_press: bool,
        keysym: u32,
        state: u16,
        keycode: u16,
    },
    Reset {
        client_id: u32,
    },
    ToggleMode {
        client_id: u32,
    },
    SetChineseMode {
        client_id: u32,
        chinese: bool,
    },
    QueryStatus,
    PollPending {
        client_id: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreeditInfo {
    pub text: String,
    pub cursor_bytes: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonReply {
    pub consumed: bool,
    pub chinese_mode: bool,
    pub commit_text: String,
    pub preedit: PreeditInfo,
}

impl ClientRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        match *self {
            Self::FocusIn {
                client_id,
                window_id,
            } => {
                out.push(0x01);
                out.extend_from_slice(&client_id.to_le_bytes());
                out.extend_from_slice(&window_id.to_le_bytes());
            }
            Self::FocusOut {
                client_id,
                window_id,
            } => {
                out.push(0x02);
                out.extend_from_slice(&client_id.to_le_bytes());
                out.extend_from_slice(&window_id.to_le_bytes());
            }
            Self::SetSpot { client_id, x, y, h } => {
                out.push(0x03);
                out.extend_from_slice(&client_id.to_le_bytes());
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                out.extend_from_slice(&h.to_le_bytes());
            }
            Self::KeyEvent {
                client_id,
                window_id,
                is_press,
                keysym,
                state,
                keycode,
            } => {
                out.push(0x04);
                out.extend_from_slice(&client_id.to_le_bytes());
                out.extend_from_slice(&window_id.to_le_bytes());
                out.push(u8::from(is_press));
                out.extend_from_slice(&keysym.to_le_bytes());
                out.extend_from_slice(&state.to_le_bytes());
                out.extend_from_slice(&keycode.to_le_bytes());
            }
            Self::Reset { client_id } => {
                out.push(0x05);
                out.extend_from_slice(&client_id.to_le_bytes());
            }
            Self::ToggleMode { client_id } => {
                out.push(0x06);
                out.extend_from_slice(&client_id.to_le_bytes());
            }
            Self::SetChineseMode { client_id, chinese } => {
                out.push(0x07);
                out.extend_from_slice(&client_id.to_le_bytes());
                out.push(u8::from(chinese));
            }
            Self::QueryStatus => {
                out.push(0x08);
            }
            Self::PollPending { client_id } => {
                out.push(0x09);
                out.extend_from_slice(&client_id.to_le_bytes());
            }
        }
        out
    }

    pub fn decode(buf: &[u8]) -> io::Result<Self> {
        let err = || io::Error::new(io::ErrorKind::InvalidData, "malformed spime request");
        let tag = *buf.first().ok_or_else(err)?;
        let u32_at = |off: usize| -> io::Result<u32> {
            buf.get(off..off + 4)
                .and_then(|s| s.try_into().ok())
                .map(u32::from_le_bytes)
                .ok_or_else(err)
        };
        let u16_at = |off: usize| -> io::Result<u16> {
            buf.get(off..off + 2)
                .and_then(|s| s.try_into().ok())
                .map(u16::from_le_bytes)
                .ok_or_else(err)
        };
        let i16_at = |off: usize| -> io::Result<i16> {
            buf.get(off..off + 2)
                .and_then(|s| s.try_into().ok())
                .map(i16::from_le_bytes)
                .ok_or_else(err)
        };
        match tag {
            0x01 => Ok(Self::FocusIn {
                client_id: u32_at(1)?,
                window_id: u32_at(5)?,
            }),
            0x02 => Ok(Self::FocusOut {
                client_id: u32_at(1)?,
                window_id: u32_at(5)?,
            }),
            0x03 => Ok(Self::SetSpot {
                client_id: u32_at(1)?,
                x: i16_at(5)?,
                y: i16_at(7)?,
                h: i16_at(9)?,
            }),
            0x04 => {
                let is_press = *buf.get(9).ok_or_else(err)? != 0;
                Ok(Self::KeyEvent {
                    client_id: u32_at(1)?,
                    window_id: u32_at(5)?,
                    is_press,
                    keysym: u32_at(10)?,
                    state: u16_at(14)?,
                    keycode: u16_at(16)?,
                })
            }
            0x05 => Ok(Self::Reset {
                client_id: u32_at(1)?,
            }),
            0x06 => Ok(Self::ToggleMode {
                client_id: u32_at(1)?,
            }),
            0x07 => {
                let chinese = *buf.get(5).ok_or_else(err)? != 0;
                Ok(Self::SetChineseMode {
                    client_id: u32_at(1)?,
                    chinese,
                })
            }
            0x08 => Ok(Self::QueryStatus),
            0x09 => Ok(Self::PollPending {
                client_id: u32_at(1)?,
            }),
            _ => Err(err()),
        }
    }
}

impl DaemonReply {
    pub fn encode(&self) -> Vec<u8> {
        let cb = self.commit_text.as_bytes();
        let pb = self.preedit.text.as_bytes();
        let clen = cb.len().min(u16::MAX as usize) as u16;
        let plen = pb.len().min(u16::MAX as usize) as u16;
        let mut out = Vec::with_capacity(8 + usize::from(clen) + usize::from(plen));
        out.push(u8::from(self.consumed));
        out.push(u8::from(self.chinese_mode));
        out.extend_from_slice(&self.preedit.cursor_bytes.to_le_bytes());
        out.extend_from_slice(&clen.to_le_bytes());
        out.extend_from_slice(&plen.to_le_bytes());
        out.extend_from_slice(&cb[..usize::from(clen)]);
        out.extend_from_slice(&pb[..usize::from(plen)]);
        out
    }

    pub fn decode(buf: &[u8]) -> io::Result<Self> {
        let err = || io::Error::new(io::ErrorKind::InvalidData, "malformed spime reply");
        if buf.len() < 8 {
            return Err(err());
        }
        let consumed = buf[0] != 0;
        let chinese_mode = buf[1] != 0;
        let cursor_bytes = u16::from_le_bytes([buf[2], buf[3]]);
        let clen = u16::from_le_bytes([buf[4], buf[5]]) as usize;
        let plen = u16::from_le_bytes([buf[6], buf[7]]) as usize;
        if buf.len() < 8 + clen + plen {
            return Err(err());
        }
        let commit_text =
            std::str::from_utf8(&buf[8..8 + clen]).map_err(|_| err())?.to_string();
        let preedit_text = std::str::from_utf8(&buf[8 + clen..8 + clen + plen])
            .map_err(|_| err())?
            .to_string();
        Ok(Self {
            consumed,
            chinese_mode,
            commit_text,
            preedit: PreeditInfo {
                text: preedit_text,
                cursor_bytes: cursor_bytes.min(plen as u16),
            },
        })
    }
}

pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    let len = payload.len();
    if len > MAX_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "spime packet too large",
        ));
    }
    w.write_all(&(len as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > MAX_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid spime frame length",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn proto_request_and_reply_roundtrip() {
        let reqs = [
            ClientRequest::FocusIn {
                client_id: 7,
                window_id: 0x1234,
            },
            ClientRequest::SetSpot {
                client_id: 7,
                x: -10,
                y: 250,
                h: 20,
            },
            ClientRequest::KeyEvent {
                client_id: 7,
                window_id: 0x1234,
                is_press: true,
                keysym: 0x0061,
                state: 4,
                keycode: 38,
            },
            ClientRequest::ToggleMode { client_id: 7 },
            ClientRequest::SetChineseMode {
                client_id: 7,
                chinese: true,
            },
            ClientRequest::QueryStatus,
        ];
        for req in &reqs {
            let mut pipe = Vec::new();
            write_frame(&mut pipe, &req.encode()).unwrap();
            let raw = read_frame(&mut Cursor::new(&pipe)).unwrap();
            let decoded = ClientRequest::decode(&raw).unwrap();
            assert_eq!(req, &decoded);
        }

        let rep = DaemonReply {
            consumed: true,
            chinese_mode: true,
            commit_text: "你好世界".to_string(),
            preedit: PreeditInfo {
                text: "ni'hao".to_string(),
                cursor_bytes: 6,
            },
        };
        let mut pipe = Vec::new();
        write_frame(&mut pipe, &rep.encode()).unwrap();
        let raw = read_frame(&mut Cursor::new(&pipe)).unwrap();
        assert_eq!(DaemonReply::decode(&raw).unwrap(), rep);
    }
}
