/*
 *
 *       user/desktop/src/ddm/wire.rs
 *       Length-prefixed DDM greeter protocol with bounded decoding
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The greeter protocol is intentionally tiny and deterministic.  Every frame
//! is a little-endian `u32` byte count followed by one opcode and typed
//! fields.  Strings carry a `u16` byte length, so a compromised greeter cannot
//! make the daemon allocate unbounded memory.

use std::io;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME: usize = 64 * 1024;
pub const MAX_STRING: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Hello = 1,
    Login = 2,
    OobeCommit = 3,
    Power = 4,
    Ping = 5,
    Goodbye = 6,
    ServerConfig = 100,
    LoginOk = 101,
    OobeOk = 102,
    PowerOk = 103,
    Pong = 104,
    Error = 105,
}

impl Opcode {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Hello,
            2 => Self::Login,
            3 => Self::OobeCommit,
            4 => Self::Power,
            5 => Self::Ping,
            6 => Self::Goodbye,
            100 => Self::ServerConfig,
            101 => Self::LoginOk,
            102 => Self::OobeOk,
            103 => Self::PowerOk,
            104 => Self::Pong,
            105 => Self::Error,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ClientKind {
    Greeter = 1,
    Oobe = 2,
}

impl ClientKind {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Greeter,
            2 => Self::Oobe,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PowerAction {
    Reboot = 0,
    PowerOff = 1,
}

impl PowerAction {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Reboot,
            1 => Self::PowerOff,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    BadRequest = 1,
    AuthFailed = 2,
    AccountLocked = 3,
    AccountDisabled = 4,
    SessionFailed = 5,
    OobeDisabled = 6,
    OobeConflict = 7,
    FieldInvalid = 8,
    Internal = 9,
    Protocol = 10,
}

#[derive(Clone, Debug, Default)]
pub struct Writer {
    data: Vec<u8>,
}

impl Writer {
    pub fn new(opcode: Opcode) -> Self {
        let mut writer = Self {
            data: Vec::with_capacity(64),
        };
        writer.u8(opcode as u8);
        writer
    }

    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.data.push(value);
        self
    }

    pub fn bool(&mut self, value: bool) -> &mut Self {
        self.u8(u8::from(value))
    }

    pub fn u16(&mut self, value: u16) -> &mut Self {
        self.data.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.data.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn string(&mut self, value: &str) -> &mut Self {
        let bytes = value.as_bytes();
        let length = bytes.len().min(u16::MAX as usize);
        self.u16(length as u16);
        self.data.extend_from_slice(&bytes[..length]);
        self
    }

    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        let length = value.len().min(u16::MAX as usize);
        self.u16(length as u16);
        self.data.extend_from_slice(&value[..length]);
        self
    }

    pub fn finish(self) -> Vec<u8> {
        self.data
    }
}

#[derive(Clone, Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub fn opcode(&mut self) -> Result<Opcode, String> {
        let value = self.u8()?;
        Opcode::from_u8(value).ok_or_else(|| format!("unknown opcode {value}"))
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        let value = *self
            .data
            .get(self.position)
            .ok_or_else(|| "truncated frame".to_string())?;
        self.position += 1;
        Ok(value)
    }

    pub fn bool(&mut self) -> Result<bool, String> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(format!("invalid boolean {value}")),
        }
    }

    pub fn u16(&mut self) -> Result<u16, String> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn string(&mut self) -> Result<String, String> {
        let bytes = self.field()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "string is not valid UTF-8".to_string())
    }

    pub fn field(&mut self) -> Result<&'a [u8], String> {
        let length = self.u16()? as usize;
        if length > MAX_STRING {
            return Err(format!("field length {length} exceeds limit"));
        }
        self.take(length)
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.position)
    }

    pub fn finish(self) -> Result<(), String> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(format!("{} trailing bytes", self.remaining()))
        }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| "frame length overflow".to_string())?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or_else(|| "truncated frame".to_string())?;
        self.position = end;
        Ok(bytes)
    }
}

pub fn encode_frame(payload: &[u8]) -> io::Result<Vec<u8>> {
    if payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large",
        ));
    }
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub fn parse_frame(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    if buffer.len() < 4 {
        return Ok(None);
    }
    let length = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(format!("invalid frame length {length}"));
    }
    if buffer.len() < 4 + length {
        return Ok(None);
    }
    let payload = buffer[4..4 + length].to_vec();
    buffer.drain(..4 + length);
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_fields() {
        let mut writer = Writer::new(Opcode::Login);
        writer.string("alice").bytes(b"secret").u16(7);
        let payload = writer.finish();
        let mut reader = Reader::new(&payload);
        assert_eq!(reader.opcode().unwrap(), Opcode::Login);
        assert_eq!(reader.string().unwrap(), "alice");
        assert_eq!(reader.field().unwrap(), b"secret");
        assert_eq!(reader.u16().unwrap(), 7);
        reader.finish().unwrap();
    }

    #[test]
    fn rejects_trailing_garbage() {
        let mut reader = Reader::new(&[1, 0, 0]);
        assert_eq!(reader.opcode().unwrap(), Opcode::Hello);
        assert!(reader.finish().is_err());
    }

    #[test]
    fn parses_partial_frame() {
        let payload = Writer::new(Opcode::Ping).finish();
        let frame = encode_frame(&payload).unwrap();
        let mut buffer = frame[..3].to_vec();
        assert!(parse_frame(&mut buffer).unwrap().is_none());
        buffer.extend_from_slice(&frame[3..]);
        assert_eq!(parse_frame(&mut buffer).unwrap().unwrap(), payload);
    }
}
