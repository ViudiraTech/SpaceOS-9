/*
 *
 *       src/net/netlink/msg.rs
 *       Netlink wire format: nlmsghdr, nlattr, builders and parsers
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! `include/uapi/linux/netlink.h` and `include/net/netlink.h`. All integers are
//! host-endian (little-endian on every supported target), as in Linux.

use alloc::vec::Vec;

pub const NLMSG_HDRLEN: usize = 16;
pub const NLMSG_ALIGNTO: usize = 4;

pub const NLMSG_NOOP: u16 = 1;
pub const NLMSG_ERROR: u16 = 2;
pub const NLMSG_DONE: u16 = 3;
pub const NLMSG_OVERRUN: u16 = 4;
pub const NLMSG_MIN_TYPE: u16 = 0x10;

pub const NLM_F_REQUEST: u16 = 0x01;
pub const NLM_F_MULTI: u16 = 0x02;
pub const NLM_F_ACK: u16 = 0x04;
pub const NLM_F_ECHO: u16 = 0x08;
pub const NLM_F_DUMP_INTR: u16 = 0x10;
pub const NLM_F_DUMP_FILTERED: u16 = 0x20;
// Modifiers to GET requests.
pub const NLM_F_ROOT: u16 = 0x100;
pub const NLM_F_MATCH: u16 = 0x200;
pub const NLM_F_ATOMIC: u16 = 0x400;
pub const NLM_F_DUMP: u16 = NLM_F_ROOT | NLM_F_MATCH;
// Modifiers to NEW requests.
pub const NLM_F_REPLACE: u16 = 0x100;
pub const NLM_F_EXCL: u16 = 0x200;
pub const NLM_F_CREATE: u16 = 0x400;
pub const NLM_F_APPEND: u16 = 0x800;
// Flags on NLMSG_ERROR replies.
pub const NLM_F_CAPPED: u16 = 0x100;
pub const NLM_F_ACK_TLVS: u16 = 0x200;

pub const NLMSGERR_ATTR_MSG: u16 = 1;
pub const NLMSGERR_ATTR_OFFS: u16 = 2;
pub const NLMSGERR_ATTR_COOKIE: u16 = 3;

pub const NLA_HDRLEN: usize = 4;
pub const NLA_F_NESTED: u16 = 0x8000;
pub const NLA_F_NET_BYTEORDER: u16 = 0x4000;
pub const NLA_TYPE_MASK: u16 = !(NLA_F_NESTED | NLA_F_NET_BYTEORDER);

pub const fn align(length: usize) -> usize {
    (length + NLMSG_ALIGNTO - 1) & !(NLMSG_ALIGNTO - 1)
}

/// `struct nlmsghdr`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub length: u32,
    pub kind: u16,
    pub flags: u16,
    pub seq: u32,
    pub portid: u32,
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < NLMSG_HDRLEN {
            return None;
        }
        Some(Self {
            length: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            kind: u16::from_le_bytes(bytes[4..6].try_into().ok()?),
            flags: u16::from_le_bytes(bytes[6..8].try_into().ok()?),
            seq: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            portid: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
        })
    }

    pub fn write(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.length.to_le_bytes());
        out[4..6].copy_from_slice(&self.kind.to_le_bytes());
        out[6..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..12].copy_from_slice(&self.seq.to_le_bytes());
        out[12..16].copy_from_slice(&self.portid.to_le_bytes());
    }
}

/// Iterates the messages packed in one datagram (`nlmsg_next` /
/// `NLMSG_OK`): stops at the first header whose length is inconsistent.
pub struct Messages<'a> {
    rest: &'a [u8],
}

impl<'a> Messages<'a> {
    pub fn new(buffer: &'a [u8]) -> Self {
        Self { rest: buffer }
    }
}

impl<'a> Iterator for Messages<'a> {
    /// Header plus the payload after it (without trailing padding).
    type Item = (Header, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let header = Header::parse(self.rest)?;
        let length = header.length as usize;
        if length < NLMSG_HDRLEN || length > self.rest.len() {
            return None;
        }
        let payload = &self.rest[NLMSG_HDRLEN..length];
        self.rest = &self.rest[align(length).min(self.rest.len())..];
        Some((header, payload))
    }
}

/// One parsed attribute.
#[derive(Clone, Copy, Debug)]
pub struct Attr<'a> {
    pub kind: u16,
    pub nested: bool,
    pub data: &'a [u8],
    /// Offset of the attribute header inside the buffer handed to [`Attrs`],
    /// reported back through `NLMSGERR_ATTR_OFFS`.
    pub offset: usize,
}

impl Attr<'_> {
    pub fn u8(&self) -> Option<u8> {
        self.data.first().copied()
    }

    pub fn u16(&self) -> Option<u16> {
        Some(u16::from_le_bytes(self.data.get(..2)?.try_into().ok()?))
    }

    pub fn u32(&self) -> Option<u32> {
        Some(u32::from_le_bytes(self.data.get(..4)?.try_into().ok()?))
    }

    pub fn u64(&self) -> Option<u64> {
        Some(u64::from_le_bytes(self.data.get(..8)?.try_into().ok()?))
    }

    /// NUL-terminated string without the terminator (`nla_strdup` semantics).
    pub fn string(&self) -> Option<&str> {
        let end = self
            .data
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(self.data.len());
        core::str::from_utf8(&self.data[..end]).ok()
    }

    pub fn nested(&self) -> Attrs<'_> {
        Attrs::new(self.data)
    }
}

/// Iterator over an attribute stream (`nla_for_each_attr`).
pub struct Attrs<'a> {
    buffer: &'a [u8],
    position: usize,
}

impl<'a> Attrs<'a> {
    pub fn new(buffer: &'a [u8]) -> Self {
        Self {
            buffer,
            position: 0,
        }
    }

    /// Validate the whole stream up front (`nla_validate_deprecated`).
    pub fn valid(buffer: &[u8]) -> bool {
        let mut position = 0;
        while position < buffer.len() {
            if buffer.len() - position < NLA_HDRLEN {
                return false;
            }
            let length = u16::from_le_bytes([buffer[position], buffer[position + 1]]) as usize;
            if length < NLA_HDRLEN || position + length > buffer.len() {
                return false;
            }
            position += align(length);
        }
        true
    }

    /// First attribute of `kind`, or `None`.
    pub fn find(buffer: &'a [u8], kind: u16) -> Option<Attr<'a>> {
        Self::new(buffer).find(|attr| attr.kind == kind)
    }
}

impl<'a> Iterator for Attrs<'a> {
    type Item = Attr<'a>;

    fn next(&mut self) -> Option<Attr<'a>> {
        let rest = self.buffer.get(self.position..)?;
        if rest.len() < NLA_HDRLEN {
            return None;
        }
        let length = u16::from_le_bytes([rest[0], rest[1]]) as usize;
        let raw_kind = u16::from_le_bytes([rest[2], rest[3]]);
        if length < NLA_HDRLEN || length > rest.len() {
            return None;
        }
        let attr = Attr {
            kind: raw_kind & NLA_TYPE_MASK,
            nested: raw_kind & NLA_F_NESTED != 0,
            data: &rest[NLA_HDRLEN..length],
            offset: self.position,
        };
        self.position += align(length);
        Some(attr)
    }
}

/// Builds one netlink message in a growable buffer (`nlmsg_put` family).
pub struct MsgBuilder {
    buffer: Vec<u8>,
}

impl MsgBuilder {
    pub fn new(kind: u16, flags: u16, seq: u32, portid: u32) -> Self {
        let mut buffer = alloc::vec![0u8; NLMSG_HDRLEN];
        Header {
            length: NLMSG_HDRLEN as u32,
            kind,
            flags,
            seq,
            portid,
        }
        .write(&mut buffer);
        Self { buffer }
    }

    /// Append a fixed-size family header (`ifinfomsg`, `ifaddrmsg`, ...),
    /// padded to alignment.
    pub fn put_payload(&mut self, bytes: &[u8]) -> &mut Self {
        self.buffer.extend_from_slice(bytes);
        self.pad();
        self
    }

    pub fn put_attr(&mut self, kind: u16, data: &[u8]) -> &mut Self {
        let length = (NLA_HDRLEN + data.len()) as u16;
        self.buffer.extend_from_slice(&length.to_le_bytes());
        self.buffer.extend_from_slice(&kind.to_le_bytes());
        self.buffer.extend_from_slice(data);
        self.pad();
        self
    }

    pub fn put_u8(&mut self, kind: u16, value: u8) -> &mut Self {
        self.put_attr(kind, &[value])
    }

    pub fn put_u16(&mut self, kind: u16, value: u16) -> &mut Self {
        self.put_attr(kind, &value.to_le_bytes())
    }

    pub fn put_u32(&mut self, kind: u16, value: u32) -> &mut Self {
        self.put_attr(kind, &value.to_le_bytes())
    }

    pub fn put_u64(&mut self, kind: u16, value: u64) -> &mut Self {
        self.put_attr(kind, &value.to_le_bytes())
    }

    /// NUL-terminated string attribute.
    pub fn put_str(&mut self, kind: u16, value: &str) -> &mut Self {
        let mut data = Vec::with_capacity(value.len() + 1);
        data.extend_from_slice(value.as_bytes());
        data.push(0);
        self.put_attr(kind, &data)
    }

    /// Open a nested attribute (`nla_nest_start`); pass the token to
    /// [`nest_end`](Self::nest_end).
    pub fn nest_start(&mut self, kind: u16) -> usize {
        let start = self.buffer.len();
        self.buffer
            .extend_from_slice(&(NLA_HDRLEN as u16).to_le_bytes());
        self.buffer
            .extend_from_slice(&(kind | NLA_F_NESTED).to_le_bytes());
        start
    }

    pub fn nest_end(&mut self, start: usize) {
        let length = (self.buffer.len() - start) as u16;
        self.buffer[start..start + 2].copy_from_slice(&length.to_le_bytes());
    }

    /// Drop a partially built nest (`nla_nest_cancel`).
    pub fn nest_cancel(&mut self, start: usize) {
        self.buffer.truncate(start);
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.len() <= NLMSG_HDRLEN
    }

    fn pad(&mut self) {
        let aligned = align(self.buffer.len());
        self.buffer.resize(aligned, 0);
    }

    /// Finalise the length field and return the bytes (`nlmsg_end`).
    pub fn finish(mut self) -> Vec<u8> {
        let length = self.buffer.len() as u32;
        self.buffer[0..4].copy_from_slice(&length.to_le_bytes());
        self.buffer
    }
}

/// Extended-ACK information attached to an error or ACK reply
/// (`struct netlink_ext_ack`).
#[derive(Default, Debug)]
pub struct ExtAck {
    pub message: Option<&'static str>,
    pub bad_attr_offset: Option<u32>,
    pub cookie: Option<u64>,
}

impl ExtAck {
    pub fn msg(message: &'static str) -> Self {
        Self {
            message: Some(message),
            ..Self::default()
        }
    }
}

/// Build an `NLMSG_ERROR` message answering `request` (`netlink_ack`).
///
/// `error` is a negative errno or 0 for a plain ACK. Like Linux, the full
/// request is echoed only for a failure when `cap_ack` is off; otherwise just
/// its header is returned and `NLM_F_CAPPED` is set. `ext` TLVs are appended
/// when the receiver enabled `NETLINK_EXT_ACK`.
pub fn build_error(
    request_header: &Header,
    request_bytes: &[u8],
    error: i32,
    ext: Option<&ExtAck>,
    cap_ack: bool,
) -> Vec<u8> {
    let full = error != 0 && !cap_ack;
    let echo_len = if full {
        request_bytes.len().min(request_header.length as usize)
    } else {
        NLMSG_HDRLEN
    };
    let mut flags = if full { 0 } else { NLM_F_CAPPED };
    if ext.is_some() {
        flags |= NLM_F_ACK_TLVS;
    }
    let mut message = MsgBuilder::new(
        NLMSG_ERROR,
        flags,
        request_header.seq,
        request_header.portid,
    );
    message.buffer.extend_from_slice(&error.to_le_bytes());
    message
        .buffer
        .extend_from_slice(&request_bytes[..echo_len.min(request_bytes.len())]);
    message.pad();
    if let Some(ext) = ext {
        if let Some(text) = ext.message {
            message.put_str(NLMSGERR_ATTR_MSG, text);
        }
        if let Some(offset) = ext.bad_attr_offset {
            message.put_u32(NLMSGERR_ATTR_OFFS, offset);
        }
        if let Some(cookie) = ext.cookie {
            message.put_u64(NLMSGERR_ATTR_COOKIE, cookie);
        }
    }
    message.finish()
}

/// `NLMSG_DONE` terminating a multipart reply; payload is the int status.
pub fn build_done(request: &Header, status: i32) -> Vec<u8> {
    let mut message = MsgBuilder::new(NLMSG_DONE, NLM_F_MULTI, request.seq, request.portid);
    message.put_payload(&status.to_le_bytes());
    message.finish()
}
