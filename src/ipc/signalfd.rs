/*
 *
 *       src/ipc/signalfd.rs
 *       Linux signalfd signal queue reader
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A signalfd consumes pending signals matching its mask from the calling
//! thread and its thread group.  Each read returns whole 128-byte
//! `signalfd_siginfo` records.

use crate::{
    sched,
    signal::{self, Signal},
    sync::SpinLock,
    vfs::{self, Inode, InodeKind, Stat, VfsError},
};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

const SIGNALFD_SIGINFO_SIZE: usize = 128;

pub struct SignalFd {
    mask: SpinLock<u64>,
    nonblock: AtomicBool,
    ino: u64,
}

impl SignalFd {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            mask: SpinLock::new(0),
            nonblock: AtomicBool::new(false),
            ino: vfs::tree::ino(),
        })
    }

    pub fn mask(&self) -> u64 {
        *self.mask.lock()
    }

    pub fn set_mask(&self, mask: u64) {
        *self.mask.lock() = mask & !(signal::SIGKILL_MASK | signal::SIGSTOP_MASK);
    }

    pub fn set_nonblock(&self, nonblock: bool) {
        self.nonblock.store(nonblock, Ordering::Release);
    }
}

fn encode(info: &Signal) -> [u8; SIGNALFD_SIGINFO_SIZE] {
    let mut bytes = [0u8; SIGNALFD_SIGINFO_SIZE];
    bytes[0..4].copy_from_slice(&info.number.to_le_bytes());
    bytes[4..8].copy_from_slice(&0i32.to_le_bytes());
    bytes[8..12].copy_from_slice(&info.code.to_le_bytes());
    bytes[12..16].copy_from_slice(&info.pid.to_le_bytes());
    bytes[16..20].copy_from_slice(&info.uid.to_le_bytes());
    if info.number == signal::SIGCHLD {
        bytes[40..44].copy_from_slice(&(info.value as u32 as i32).to_le_bytes());
    } else {
        bytes[44..48].copy_from_slice(&(info.value as u32 as i32).to_le_bytes());
        bytes[48..56].copy_from_slice(&info.value.to_le_bytes());
    }
    bytes
}

impl Inode for SignalFd {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::CharDevice, 0o600, 0))
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        if buf.len() < SIGNALFD_SIGINFO_SIZE {
            return Err(VfsError::InvalidArgument);
        }
        loop {
            let mut written = 0usize;
            while written + SIGNALFD_SIGINFO_SIZE <= buf.len() {
                let mask = self.mask();
                let Some(info) = signal::take_pending_masked(mask) else {
                    break;
                };
                let bytes = encode(&info);
                buf[written..written + SIGNALFD_SIGINFO_SIZE].copy_from_slice(&bytes);
                written += SIGNALFD_SIGINFO_SIZE;
            }
            if written != 0 {
                return Ok(written);
            }
            if self.nonblock.load(Ordering::Acquire) {
                return Err(VfsError::WouldBlock);
            }
            if signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            sched::sleep_micros(1000);
        }
    }

    fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::InvalidArgument)
    }

    fn poll_sources(&self, _sources: &mut crate::ipc::pollwait::PollSources) {
        // Queueing a signal kicks every waiter.
    }

    fn poll(&self) -> u32 {
        if signal::pending_masked(self.mask()) {
            crate::ipc::unix::POLLIN
        } else {
            0
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.set_nonblock(nonblock);
        Ok(())
    }
}

pub fn instance(fd: u64) -> Result<Arc<SignalFd>, VfsError> {
    if fd > i32::MAX as u64 {
        return Err(VfsError::BadFileDescriptor);
    }
    let inode = vfs::current_fd_table()
        .lock()
        .get(fd as usize)
        .ok_or(VfsError::BadFileDescriptor)?
        .file
        .inode
        .clone();
    let any: Arc<dyn core::any::Any + Send + Sync> = inode;
    any.downcast::<SignalFd>()
        .map_err(|_| VfsError::InvalidArgument)
}
