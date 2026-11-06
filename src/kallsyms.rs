/*
 *
 *       src/kallsyms.rs
 *       Build-time generated kernel symbol table exposed through /proc/kallsyms
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Full kernel symbol table.
//!
//! `tools/gen_kallsyms.py` rewrites the `.spk_kallsyms` section after the
//! kernel is linked.  Names are stored in address order with front-coding
//! (prefix length, suffix) which cuts the Rust-mangled symbol table to roughly
//! half.  Addresses are link-time relative values; the runtime slide reported
//! by Limine is added when a symbol is printed.

use alloc::{string::String, vec::Vec};

const MAGIC: u32 = 0x534b_5053; // "SPKS"
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 32;
pub const CAPACITY: usize = 4 * 1024 * 1024;

#[repr(align(16))]
struct Blob([u8; CAPACITY]);

// A non-zero initializer keeps the linker from turning the reserved area into
// NOBITS: objcopy/ELF patching needs file-backed contents.
#[used]
#[unsafe(link_section = ".spk_kallsyms")]
static KALLSYMS: Blob = Blob([0x5a; CAPACITY]);

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

struct Table {
    count: usize,
    payload_end: usize,
}

fn table() -> Option<Table> {
    let bytes = &KALLSYMS.0;
    if read_u32(bytes, 0)? != MAGIC || read_u32(bytes, 4)? != VERSION {
        return None;
    }
    let count = read_u32(bytes, 8)? as usize;
    let payload = read_u64(bytes, 16)? as usize;
    let payload_end = HEADER_SIZE.checked_add(payload)?;
    if count == 0 || count > 200_000 || payload_end > CAPACITY {
        return None;
    }
    Some(Table { count, payload_end })
}

pub fn available() -> bool {
    table().is_some()
}

fn slide() -> u64 {
    crate::boot::kernel_virtual()
}

/// Append one `address type name` line per symbol.  Module symbols are handled
/// by the module registry, not by this table.
pub fn append(out: &mut String) {
    let Some(table) = table() else {
        return;
    };
    let bytes = &KALLSYMS.0;
    let base = slide();
    let mut offset = HEADER_SIZE;
    let mut previous: Vec<u8> = Vec::new();
    for _ in 0..table.count {
        let Some(address) = read_u64(bytes, offset) else {
            return;
        };
        let Some(kind) = bytes.get(offset + 8).copied() else {
            return;
        };
        let Some(prefix) = read_u16(bytes, offset + 9) else {
            return;
        };
        let Some(suffix_len) = read_u16(bytes, offset + 11) else {
            return;
        };
        let suffix_start = offset + 13;
        let suffix_end = suffix_start + suffix_len as usize;
        if prefix as usize > previous.len() || suffix_end > table.payload_end {
            return;
        }
        previous.truncate(prefix as usize);
        previous.extend_from_slice(&bytes[suffix_start..suffix_end]);
        let name = core::str::from_utf8(&previous).unwrap_or("?");
        out.push_str(&alloc::format!(
            "{:016x} {} {}\n",
            address.wrapping_add(base),
            kind as char,
            name
        ));
        offset = suffix_end;
    }
}

/// Whether a symbol is present in the generated table.  Used by boot
/// self-tests to prove that build-time patching ran.
pub fn has_symbol(name: &str) -> bool {
    let Some(table) = table() else {
        return false;
    };
    let bytes = &KALLSYMS.0;
    let mut offset = HEADER_SIZE;
    let mut previous: Vec<u8> = Vec::new();
    for _ in 0..table.count {
        let Some(prefix) = read_u16(bytes, offset + 9) else {
            return false;
        };
        let Some(suffix_len) = read_u16(bytes, offset + 11) else {
            return false;
        };
        let suffix_start = offset + 13;
        let suffix_end = suffix_start + suffix_len as usize;
        if prefix as usize > previous.len() || suffix_end > table.payload_end {
            return false;
        }
        previous.truncate(prefix as usize);
        previous.extend_from_slice(&bytes[suffix_start..suffix_end]);
        if previous == name.as_bytes() {
            return true;
        }
        offset = suffix_end;
    }
    false
}
