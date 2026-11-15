/*
 *
 *       src/hal/block.rs
 *       HAL block device class for drivers
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A driver supplies read/write/flush callbacks; the kernel names the disk,
//! scans partitions and publishes /dev and sysfs nodes (`block::register_
//! device`). Removal first refuses new I/O, then waits for callbacks in
//! flight, so the driver may free its private data once unregister returns.
//!
//! The block layer's cache and partition scan work in 512-byte sectors, so
//! this version accepts only 512-byte logical blocks.

use crate::block::{self, BlockDevice, BlockDeviceInfo, BlockDeviceKind, BlockError};
use crate::sched::WaitQueue;
use alloc::{boxed::Box, string::String, sync::Arc};
use core::ffi::{c_char, c_int, c_uint, c_ulonglong, c_void};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

type ReadFn = unsafe extern "C" fn(*mut c_void, c_ulonglong, c_uint, *mut c_void) -> c_int;
type WriteFn = unsafe extern "C" fn(*mut c_void, c_ulonglong, c_uint, *const c_void) -> c_int;
type FlushFn = unsafe extern "C" fn(*mut c_void) -> c_int;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlockOps {
    read: Option<ReadFn>,
    write: Option<WriteFn>,
    flush: Option<FlushFn>,
}

struct Disk {
    info: BlockDeviceInfo,
    ops: BlockOps,
    private: *mut c_void,
    gone: AtomicBool,
    inflight: AtomicUsize,
    idle: WaitQueue,
}

// SAFETY: the driver's callbacks and private pointer are thread-safe by the
// HAL contract (callbacks may run concurrently on any CPU).
unsafe impl Send for Disk {}
unsafe impl Sync for Disk {}

impl Disk {
    fn call(&self, operation: impl FnOnce() -> c_int) -> Result<(), BlockError> {
        self.inflight.fetch_add(1, Ordering::AcqRel);
        let result = if self.gone.load(Ordering::Acquire) {
            -(crate::vfs::abi::ENODEV as c_int)
        } else {
            operation()
        };
        if self.inflight.fetch_sub(1, Ordering::AcqRel) == 1 && self.gone.load(Ordering::Acquire) {
            self.idle.wake_all();
        }
        match result {
            0 => Ok(()),
            error if error == -(crate::vfs::abi::ENODEV as c_int) => Err(BlockError::NotReady),
            error if error == -(crate::vfs::abi::ETIMEDOUT as c_int) => Err(BlockError::Timeout),
            error if error == -(crate::vfs::abi::EINVAL as c_int) => Err(BlockError::OutOfRange),
            _ => Err(BlockError::IoError),
        }
    }

    fn check(&self, lba: u64, count: usize, length: usize) -> Result<(), BlockError> {
        let end = lba
            .checked_add(count as u64)
            .ok_or(BlockError::OutOfRange)?;
        if end > self.info.total_blocks || count > u32::MAX as usize {
            return Err(BlockError::OutOfRange);
        }
        if length < count * self.info.block_size {
            return Err(BlockError::InvalidBuffer);
        }
        Ok(())
    }
}

impl BlockDevice for Disk {
    fn info(&self) -> BlockDeviceInfo {
        self.info.clone()
    }

    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), BlockError> {
        self.check(lba, count, buf.len())?;
        let read = self.ops.read.ok_or(BlockError::Unsupported)?;
        // SAFETY: the buffer covers count blocks (checked above).
        self.call(|| unsafe { read(self.private, lba, count as c_uint, buf.as_mut_ptr().cast()) })
    }

    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), BlockError> {
        if self.info.read_only {
            return Err(BlockError::ReadOnly);
        }
        self.check(lba, count, buf.len())?;
        let write = self.ops.write.ok_or(BlockError::Unsupported)?;
        // SAFETY: as for reads.
        self.call(|| unsafe { write(self.private, lba, count as c_uint, buf.as_ptr().cast()) })
    }

    fn flush(&self) -> Result<(), BlockError> {
        match self.ops.flush {
            // SAFETY: the driver's flush takes only its private pointer.
            Some(flush) => self.call(|| unsafe { flush(self.private) }),
            None => Ok(()),
        }
    }
}

pub struct BlockHandle {
    disk: Arc<Disk>,
    id: usize,
}

fn text(pointer: *const c_char) -> String {
    String::from_utf8_lossy(crate::module::hw::c_string(pointer)).into_owned()
}

/// # Safety
/// Strings are NUL-terminated or null; `ops` points to a table that outlives
/// the device; callbacks honour the contract in module.h.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spk_blkdev_register(
    prefix: *const c_char,
    model: *const c_char,
    serial: *const c_char,
    block_size: c_uint,
    blocks: c_ulonglong,
    removable: c_int,
    read_only: c_int,
    ops: *const BlockOps,
    private: *mut c_void,
) -> *mut BlockHandle {
    // SAFETY: caller contract.
    let Some(ops) = (unsafe { ops.as_ref() }).copied() else {
        return core::ptr::null_mut();
    };
    if block_size != block::SECTOR_SIZE as c_uint || blocks == 0 {
        crate::kwarn!(
            "hal: block device with {}-byte blocks not supported (needs {})",
            block_size,
            block::SECTOR_SIZE
        );
        return core::ptr::null_mut();
    }
    let prefix = text(prefix);
    let Some(name) = block::allocate_name(&prefix) else {
        return core::ptr::null_mut();
    };
    let disk = Arc::new(Disk {
        info: BlockDeviceInfo {
            name,
            model: text(model),
            serial: text(serial),
            kind: BlockDeviceKind::Hal,
            block_size: block_size as usize,
            total_blocks: blocks,
            read_only: read_only != 0,
            removable: removable != 0,
        },
        ops,
        private,
        gone: AtomicBool::new(false),
        inflight: AtomicUsize::new(0),
        idle: WaitQueue::new(),
    });
    let id = block::register_device(disk.clone());
    Box::into_raw(Box::new(BlockHandle { disk, id }))
}

/// Copy the kernel-chosen name (e.g. "sdb") into `buffer`, NUL-terminated.
///
/// # Safety
/// `handle` is live; `buffer` has `length` writable bytes.
pub unsafe extern "C" fn spk_blkdev_name(
    handle: *mut BlockHandle,
    buffer: *mut c_char,
    length: c_uint,
) -> c_int {
    // SAFETY: caller contract.
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return -(crate::vfs::abi::EINVAL as c_int);
    };
    let name = handle.disk.info.name.as_bytes();
    if buffer.is_null() || (length as usize) <= name.len() {
        return -(crate::vfs::abi::EINVAL as c_int);
    }
    // SAFETY: bounds checked above.
    unsafe {
        core::ptr::copy_nonoverlapping(name.as_ptr(), buffer.cast(), name.len());
        *buffer.add(name.len()) = 0;
    }
    0
}

/// Remove the disk and its partitions; returns once no callback runs.
///
/// # Safety
/// `handle` came from `spk_blkdev_register`; process context.
pub unsafe extern "C" fn spk_blkdev_unregister(handle: *mut BlockHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller contract.
    let handle = unsafe { Box::from_raw(handle) };
    let disk = &handle.disk;
    disk.gone.store(true, Ordering::Release);
    block::unregister_device(handle.id);
    disk.idle
        .wait_until(|| disk.inflight.load(Ordering::Acquire) == 0);
}
