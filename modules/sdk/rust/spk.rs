/*
 *
 *       modules/sdk/rust/spk.rs
 *       Minimal no_std SDK for Rust SpaceKernel modules
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Include this file with `#[path = "..."] mod spk;` and place one
//! [`SpkModule`] in `.spk_module`.  The Rust module is compiled to ET_REL by
//! `tools/build_modules.sh` and linked by the kernel exactly like a C module.

#![allow(dead_code)]

use core::ffi::{c_char, c_int};

pub const SPK_MODULE_MAGIC: u32 = 0x4D4B_5053;
pub const SPK_MODULE_ABI_VERSION: u32 = 2;

pub const VERMAGIC_C: &[u8] = concat!(env!("SPK_VERMAGIC"), "\0").as_bytes();

#[repr(C)]
pub struct SpkModule {
    pub magic: u32,
    pub abi_version: u32,
    pub name: *const c_char,
    pub version: *const c_char,
    pub license: *const c_char,
    pub vermagic: *const c_char,
    pub init: Option<unsafe extern "C" fn() -> c_int>,
    pub exit: Option<unsafe extern "C" fn()>,
}

// SAFETY: all fields are immutable for the module lifetime; the kernel reads
// the record after relocations and before calling init.
unsafe impl Sync for SpkModule {}

unsafe extern "C" {
    fn spk_pr_info(message: *const c_char);
    fn spk_pr_warn(message: *const c_char);
    fn spk_pr_err(message: *const c_char);
    fn spk_kmalloc(size: usize) -> *mut u8;
    fn spk_kzalloc(size: usize) -> *mut u8;
    fn spk_kfree(pointer: *mut u8);
    fn spk_uptime_us() -> u64;
    fn spk_msleep(milliseconds: u64);
}

pub fn print(text: &str) {
    let mut buffer = [0u8; 256];
    let bytes = text.as_bytes();
    let length = bytes.len().min(buffer.len() - 1);
    buffer[..length].copy_from_slice(&bytes[..length]);
    // SAFETY: buffer is NUL-terminated for the duration of the call.
    unsafe { spk_pr_info(buffer.as_ptr() as *const c_char) };
}

pub fn warn(text: &str) {
    let mut buffer = [0u8; 256];
    let bytes = text.as_bytes();
    let length = bytes.len().min(buffer.len() - 1);
    buffer[..length].copy_from_slice(&bytes[..length]);
    // SAFETY: buffer is NUL-terminated for the duration of the call.
    unsafe { spk_pr_warn(buffer.as_ptr() as *const c_char) };
}

pub fn error(text: &str) {
    let mut buffer = [0u8; 256];
    let bytes = text.as_bytes();
    let length = bytes.len().min(buffer.len() - 1);
    buffer[..length].copy_from_slice(&bytes[..length]);
    // SAFETY: buffer is NUL-terminated for the duration of the call.
    unsafe { spk_pr_err(buffer.as_ptr() as *const c_char) };
}

pub fn kmalloc(size: usize) -> *mut u8 {
    // SAFETY: the kernel allocator may return null; callers must check.
    unsafe { spk_kmalloc(size) }
}

pub fn kzalloc(size: usize) -> *mut u8 {
    // SAFETY: the kernel allocator may return null; callers must check.
    unsafe { spk_kzalloc(size) }
}

pub fn kfree(pointer: *mut u8) {
    // SAFETY: the caller passes a live allocation from this SDK.
    unsafe { spk_kfree(pointer) };
}

pub fn uptime_us() -> u64 {
    // SAFETY: plain kernel query.
    unsafe { spk_uptime_us() }
}

pub fn msleep(milliseconds: u64) {
    // SAFETY: plain kernel call; valid in init/exit process context.
    unsafe { spk_msleep(milliseconds) };
}
