/*
 *
 *       src/module/api.rs
 *       Exported C ABI whitelist available to loadable modules
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Kernel API surface for modules.
//!
//! Only symbols listed in [`EXPORTS`] resolve for module relocations.  The
//! table is deliberately small and every entry is a plain C ABI function so
//! both C and Rust modules can call it without depending on Rust internals.

use alloc::alloc::{alloc, dealloc};
use core::{alloc::Layout, ffi::c_int, mem::size_of, ptr};

use crate::{printk::Level, sync::SpinLock};

#[repr(C)]
struct AllocHeader {
    size: usize,
    magic: u64,
}

const ALLOC_MAGIC: u64 = 0x5350_4b4d_414c_4c4f; // "SPKMALLO"
const MAX_ALLOC: usize = 256 * 1024 * 1024;

static CURRENT_NAME: SpinLock<[u8; 64]> = SpinLock::new([0; 64]);
static RELEASE_C: [u8; 8] = *b"3.0-spk\0";

pub struct Export {
    pub name: &'static str,
    // Function pointers cannot be cast to integers in const context, but they
    // can be stored as raw pointers and converted when the loader looks them
    // up. The table is immutable for the kernel lifetime.
    pub address: *const (),
}

// SAFETY: exports are immutable function pointers only read by the loader.
unsafe impl Sync for Export {}

macro_rules! export {
    ($name:literal, $function:path) => {
        Export {
            name: $name,
            address: $function as *const (),
        }
    };
}

unsafe extern "C" {
    fn memcpy(destination: *mut u8, source: *const u8, count: usize) -> *mut u8;
    fn memmove(destination: *mut u8, source: *const u8, count: usize) -> *mut u8;
    fn memset(destination: *mut u8, value: c_int, count: usize) -> *mut u8;
    fn memcmp(left: *const u8, right: *const u8, count: usize) -> c_int;
}

/// Whitelisted kernel symbols.  Keep this table sorted by name for readability;
/// lookup is linear because it is only used while loading a module.
pub static EXPORTS: &[Export] = &[
    export!("memcmp", memcmp),
    export!("memcpy", memcpy),
    export!("memmove", memmove),
    export!("memset", memset),
    export!("spk_get_cpu", spk_get_cpu),
    export!("spk_getpid", spk_getpid),
    export!("spk_kernel_version", spk_kernel_version),
    export!("spk_kfree", spk_kfree),
    export!("spk_kmalloc", spk_kmalloc),
    export!("spk_krealloc", spk_krealloc),
    export!("spk_kzalloc", spk_kzalloc),
    export!("spk_module_name", spk_module_name),
    export!("spk_msleep", spk_msleep),
    export!("spk_pr_err", spk_pr_err),
    export!("spk_pr_info", spk_pr_info),
    export!("spk_pr_warn", spk_pr_warn),
    export!("spk_printk", spk_printk),
    export!("spk_udelay", spk_udelay),
    export!("spk_uptime_ms", spk_uptime_ms),
    export!("spk_uptime_us", spk_uptime_us),
];

pub fn lookup(name: &str) -> Option<u64> {
    EXPORTS
        .iter()
        .find(|export| export.name == name)
        .map(|export| export.address as usize as u64)
}

pub fn set_current_name(name: &str) {
    let mut guard = CURRENT_NAME.lock();
    guard.fill(0);
    let length = name.len().min(guard.len() - 1);
    guard[..length].copy_from_slice(&name.as_bytes()[..length]);
}

pub fn clear_current_name() {
    CURRENT_NAME.lock().fill(0);
}

struct Line {
    bytes: [u8; 1024],
    length: usize,
}

impl Line {
    fn new() -> Self {
        Self {
            bytes: [0; 1024],
            length: 0,
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.length < self.bytes.len() {
            self.bytes[self.length] = byte;
            self.length += 1;
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.push_byte(byte);
        }
    }

    fn push_str(&mut self, text: &str) {
        self.push_bytes(text.as_bytes());
    }

    fn text(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("<invalid utf-8>")
    }
}

fn unsafe_c_string(address: *const u8, limit: usize) -> &'static [u8] {
    if address.is_null() {
        return b"(null)";
    }
    let mut length = 0;
    // SAFETY: module-provided strings are trusted kernel-mode memory; the
    // scan is capped so a missing terminator cannot run away.
    while length < limit && unsafe { address.add(length).read() } != 0 {
        length += 1;
    }
    // SAFETY: the bounded range is valid for the returned slice lifetime.
    unsafe { core::slice::from_raw_parts(address, length) }
}

fn push_number(
    line: &mut Line,
    value: u64,
    negative: bool,
    base: u32,
    upper: bool,
    width: usize,
    zero_pad: bool,
    left: bool,
    alternate: bool,
) {
    let mut digits = [0u8; 64];
    let mut length = 0usize;
    let alphabet: &[u8; 16] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut remaining = value;
    if remaining == 0 {
        digits[0] = b'0';
        length = 1;
    } else {
        while remaining != 0 {
            digits[length] = alphabet[(remaining % base as u64) as usize];
            remaining /= base as u64;
            length += 1;
        }
    }
    let sign = negative as usize;
    let prefix = if alternate && base == 16 { 2 } else { 0 };
    let total = sign + prefix + length;
    let padding = width.saturating_sub(total);
    if !left && !zero_pad {
        for _ in 0..padding {
            line.push_byte(b' ');
        }
    }
    if negative {
        line.push_byte(b'-');
    }
    if prefix != 0 {
        line.push_byte(b'0');
        line.push_byte(if upper { b'X' } else { b'x' });
    }
    if !left && zero_pad {
        for _ in 0..padding {
            line.push_byte(b'0');
        }
    }
    while length != 0 {
        length -= 1;
        line.push_byte(digits[length]);
    }
    if left {
        for _ in 0..padding {
            line.push_byte(b' ');
        }
    }
}

/// C `printk`-style logging with the common integer, string, character and
/// pointer conversions.  Unknown conversions are emitted literally.
///
/// # Safety
/// `format` must point to a NUL-terminated string and the variadic arguments
/// must match the conversions in it, exactly like C `printf`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_printk(format: *const u8, mut arguments: ...) -> c_int {
    if format.is_null() {
        return -crate::vfs::abi::EINVAL as c_int;
    }
    let specification = unsafe_c_string(format, 1024);
    let mut line = Line::new();
    let mut index = 0usize;
    while index < specification.len() {
        let byte = specification[index];
        if byte != b'%' {
            line.push_byte(byte);
            index += 1;
            continue;
        }
        index += 1;
        if index >= specification.len() {
            line.push_byte(b'%');
            break;
        }
        if specification[index] == b'%' {
            line.push_byte(b'%');
            index += 1;
            continue;
        }
        let mut left = false;
        let mut zero_pad = false;
        let mut alternate = false;
        loop {
            match specification.get(index).copied() {
                Some(b'-') => left = true,
                Some(b'0') => zero_pad = true,
                Some(b'#') => alternate = true,
                Some(b' ') | Some(b'+') => {}
                _ => break,
            }
            index += 1;
        }
        let mut width = 0usize;
        while let Some(digit @ b'0'..=b'9') = specification.get(index).copied() {
            width = width
                .saturating_mul(10)
                .saturating_add((digit - b'0') as usize);
            index += 1;
        }
        if specification.get(index) == Some(&b'.') {
            index += 1;
            while let Some(b'0'..=b'9') = specification.get(index).copied() {
                index += 1;
            }
        }
        let mut long = false;
        while specification.get(index) == Some(&b'l') {
            long = true;
            index += 1;
        }
        if matches!(specification.get(index), Some(b'z') | Some(b't')) {
            long = true;
            index += 1;
        }
        while matches!(specification.get(index), Some(b'h')) {
            index += 1;
        }
        let Some(conversion) = specification.get(index).copied() else {
            line.push_byte(b'%');
            break;
        };
        index += 1;
        match conversion {
            b's' => {
                // SAFETY: the caller supplies a pointer per the C ABI.
                let pointer: *const u8 = unsafe { arguments.next_arg() };
                let text = unsafe_c_string(pointer, 1024);
                let length = text.len();
                let padding = width.saturating_sub(length);
                if !left {
                    for _ in 0..padding {
                        line.push_byte(b' ');
                    }
                }
                line.push_bytes(text);
                if left {
                    for _ in 0..padding {
                        line.push_byte(b' ');
                    }
                }
            }
            b'c' => {
                // SAFETY: characters are promoted to c_int.
                let value: c_int = unsafe { arguments.next_arg() };
                let padding = width.saturating_sub(1);
                if !left {
                    for _ in 0..padding {
                        line.push_byte(b' ');
                    }
                }
                line.push_byte(value as u8);
                if left {
                    for _ in 0..padding {
                        line.push_byte(b' ');
                    }
                }
            }
            b'd' | b'i' => {
                let value: i64 = if long {
                    // SAFETY: the caller passed a long/long long argument.
                    unsafe { arguments.next_arg() }
                } else {
                    // SAFETY: int arguments occupy one variadic slot.
                    unsafe { arguments.next_arg::<i32>() as i64 }
                };
                let negative = value < 0;
                let magnitude = value.unsigned_abs();
                push_number(
                    &mut line, magnitude, negative, 10, false, width, zero_pad, left, false,
                );
            }
            b'u' => {
                let value: u64 = if long {
                    // SAFETY: the caller passed an unsigned long argument.
                    unsafe { arguments.next_arg() }
                } else {
                    // SAFETY: unsigned int occupies one variadic slot.
                    unsafe { arguments.next_arg::<u32>() as u64 }
                };
                push_number(
                    &mut line, value, false, 10, false, width, zero_pad, left, false,
                );
            }
            b'x' | b'X' => {
                let value: u64 = if long {
                    // SAFETY: the caller passed an unsigned long argument.
                    unsafe { arguments.next_arg() }
                } else {
                    // SAFETY: unsigned int occupies one variadic slot.
                    unsafe { arguments.next_arg::<u32>() as u64 }
                };
                push_number(
                    &mut line,
                    value,
                    false,
                    16,
                    conversion == b'X',
                    width,
                    zero_pad,
                    left,
                    alternate,
                );
            }
            b'p' => {
                // SAFETY: pointer arguments occupy one variadic slot.
                let pointer: *const u8 = unsafe { arguments.next_arg() };
                push_number(
                    &mut line,
                    pointer as usize as u64,
                    false,
                    16,
                    false,
                    width.max(2) + 2,
                    zero_pad,
                    left,
                    true,
                );
            }
            _ => {
                line.push_byte(b'%');
                line.push_byte(conversion);
            }
        }
    }
    crate::printk::record(Level::Info, format_args!("{}", line.text()));
    line.length as c_int
}

fn log_c_string(message: *const u8, level: Level) {
    if message.is_null() {
        return;
    }
    let text = unsafe_c_string(message, 2048);
    crate::printk::record(
        level,
        format_args!(
            "{}",
            core::str::from_utf8(text).unwrap_or("<invalid utf-8>")
        ),
    );
}

/// # Safety
/// `message` must be a NUL-terminated string valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pr_info(message: *const u8) {
    log_c_string(message, Level::Info);
}

/// # Safety
/// `message` must be a NUL-terminated string valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pr_warn(message: *const u8) {
    log_c_string(message, Level::Warn);
}

/// # Safety
/// `message` must be a NUL-terminated string valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pr_err(message: *const u8) {
    log_c_string(message, Level::Error);
}

fn header_layout(size: usize) -> Option<(Layout, usize)> {
    if size > MAX_ALLOC {
        return None;
    }
    let total = size.max(1).checked_add(size_of::<AllocHeader>())?;
    Layout::from_size_align(total, 16)
        .ok()
        .map(|layout| (layout, total))
}

/// Allocate zeroed kernel memory.  The returned pointer is 16-byte aligned and
/// must be released with `spk_kfree` or `spk_krealloc`.
#[unsafe(no_mangle)]
pub extern "C" fn spk_kmalloc(size: usize) -> *mut u8 {
    let Some((layout, _)) = header_layout(size) else {
        return ptr::null_mut();
    };
    // SAFETY: the layout has non-zero size.
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: the allocation is at least the header size and 16-byte aligned.
    unsafe {
        ptr::write(
            base as *mut AllocHeader,
            AllocHeader {
                size,
                magic: ALLOC_MAGIC,
            },
        );
    }
    // SAFETY: the header occupies the first 16 bytes of the allocation.
    unsafe { base.add(size_of::<AllocHeader>()) }
}

/// Allocate zero-filled kernel memory.
#[unsafe(no_mangle)]
pub extern "C" fn spk_kzalloc(size: usize) -> *mut u8 {
    let pointer = spk_kmalloc(size);
    if !pointer.is_null() {
        // SAFETY: the allocation holds at least `size` writable bytes.
        unsafe { ptr::write_bytes(pointer, 0, size.max(1)) };
    }
    pointer
}

/// Release memory obtained from `spk_kmalloc`/`spk_kzalloc`.
///
/// # Safety
/// `pointer` must be null or an allocation returned by this API that has not
/// already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_kfree(pointer: *mut u8) {
    if pointer.is_null() {
        return;
    }
    // SAFETY: the header precedes the returned pointer.
    let header = unsafe { &*(pointer.sub(size_of::<AllocHeader>()) as *const AllocHeader) };
    assert_eq!(header.magic, ALLOC_MAGIC, "spk_kfree: bad header");
    let Some((layout, _)) = header_layout(header.size) else {
        return;
    };
    // SAFETY: layout matches the allocation made by spk_kmalloc.
    unsafe { dealloc(pointer.sub(size_of::<AllocHeader>()), layout) };
}

/// Resize a `spk_kmalloc` allocation.  Returns null when the new allocation
/// fails; the original pointer then remains valid.
///
/// # Safety
/// `pointer` must be null or a live allocation from this API.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_krealloc(pointer: *mut u8, size: usize) -> *mut u8 {
    if pointer.is_null() {
        return spk_kmalloc(size);
    }
    if size == 0 {
        // SAFETY: the caller transfers ownership of a live allocation.
        unsafe { spk_kfree(pointer) };
        return ptr::null_mut();
    }
    // SAFETY: the header precedes the returned pointer.
    let header = unsafe { &*(pointer.sub(size_of::<AllocHeader>()) as *const AllocHeader) };
    assert_eq!(header.magic, ALLOC_MAGIC, "spk_krealloc: bad header");
    let replacement = spk_kmalloc(size);
    if replacement.is_null() {
        return ptr::null_mut();
    }
    let copy = header.size.min(size);
    // SAFETY: both allocations are at least `copy` bytes and do not overlap.
    unsafe { ptr::copy_nonoverlapping(pointer, replacement, copy) };
    // SAFETY: the original allocation is still live and uniquely owned here.
    unsafe { spk_kfree(pointer) };
    replacement
}

/// Microseconds since boot.
#[unsafe(no_mangle)]
pub extern "C" fn spk_uptime_us() -> u64 {
    crate::time::uptime_micros()
}

/// Milliseconds since boot.
#[unsafe(no_mangle)]
pub extern "C" fn spk_uptime_ms() -> u64 {
    crate::time::uptime_micros() / 1000
}

fn wait_micros(microseconds: u64) {
    if crate::sched::current().is_some() {
        crate::sched::sleep_micros(microseconds);
        return;
    }
    let deadline = crate::time::uptime_micros().saturating_add(microseconds);
    while crate::time::uptime_micros() < deadline {
        core::hint::spin_loop();
    }
}

/// Sleep in process context.  During early boot, before the scheduler exists,
/// this degrades to a bounded busy wait.
#[unsafe(no_mangle)]
pub extern "C" fn spk_msleep(milliseconds: u64) {
    wait_micros(milliseconds.saturating_mul(1000));
}

/// Busy-wait or sleep for a short microsecond delay.
#[unsafe(no_mangle)]
pub extern "C" fn spk_udelay(microseconds: u64) {
    wait_micros(microseconds);
}

/// UTS release string as a NUL-terminated C string.
#[unsafe(no_mangle)]
pub extern "C" fn spk_kernel_version() -> *const u8 {
    RELEASE_C.as_ptr()
}

/// Name of the module currently executing its init/exit routine.
#[unsafe(no_mangle)]
pub extern "C" fn spk_module_name() -> *const u8 {
    CURRENT_NAME.as_ptr() as *const u8
}

/// Thread-group ID of the caller.
#[unsafe(no_mangle)]
pub extern "C" fn spk_getpid() -> u32 {
    crate::sched::current().map_or(0, |task| task.stats().tgid as u32)
}

/// Logical CPU index of the caller.
#[unsafe(no_mangle)]
pub extern "C" fn spk_get_cpu() -> u32 {
    crate::smp::current_cpu() as u32
}
