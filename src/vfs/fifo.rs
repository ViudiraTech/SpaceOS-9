/*
 *
 *       src/vfs/fifo.rs
 *       Named pipes sharing the pipe buffer implementation
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A FIFO inode is a persistent directory entry; each `open` returns a fresh
//! endpoint that participates in the shared reader/writer counts.  Blocking
//! open is not implemented: `open` with O_WRONLY|O_NONBLOCK returns ENXIO when
//! no reader exists, and other opens proceed immediately.

use super::{
    abi,
    fd::{PIPE_CAPACITY, PipeBuffer, pipe_read, pipe_write},
    inode::{Inode, InodeKind, Stat, VfsError},
};
use crate::sync::SpinLock;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

pub struct FifoInode {
    shared: Arc<SpinLock<PipeBuffer>>,
    ino: u64,
}

impl FifoInode {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(SpinLock::new(PipeBuffer {
                queue: Default::default(),
                readers: 0,
                writers: 0,
            })),
            ino: super::tree::ino(),
        })
    }
}

impl Inode for FifoInode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::Fifo, 0o600, 0))
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        let access = flags & abi::O_ACCMODE;
        let (can_read, can_write) = match access {
            abi::O_RDONLY => (true, false),
            abi::O_WRONLY => (false, true),
            _ => (true, true),
        };
        let nonblock = flags & abi::O_NONBLOCK != 0;
        {
            let mut pipe = self.shared.lock();
            if can_write && !can_read && nonblock && pipe.readers == 0 {
                return Err(VfsError::NoSuchDeviceOrAddress);
            }
            if can_read {
                pipe.readers = pipe.readers.saturating_add(1);
            }
            if can_write {
                pipe.writers = pipe.writers.saturating_add(1);
            }
        }
        Ok(Some(Arc::new(FifoEndInode {
            shared: self.shared.clone(),
            can_read,
            can_write,
            nonblock: AtomicBool::new(nonblock),
            ino: self.ino,
        })))
    }
}

struct FifoEndInode {
    shared: Arc<SpinLock<PipeBuffer>>,
    can_read: bool,
    can_write: bool,
    nonblock: AtomicBool,
    ino: u64,
}

impl Inode for FifoEndInode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let len = self.shared.lock().queue.len() as u64;
        Ok(Stat::simple(self.ino, InodeKind::Fifo, 0o600, len))
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        if !self.can_read {
            return Err(VfsError::BadFileDescriptor);
        }
        pipe_read(&self.shared, self.nonblock.load(Ordering::Acquire), buf)
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> Result<usize, VfsError> {
        if !self.can_write {
            return Err(VfsError::BadFileDescriptor);
        }
        pipe_write(&self.shared, self.nonblock.load(Ordering::Acquire), buf)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(Arc::as_ptr(&self.shared));
    }

    fn poll(&self) -> u32 {
        let pipe = self.shared.lock();
        let mut events = 0;
        if self.can_read {
            if !pipe.queue.is_empty() {
                events |= 0x01;
            }
            if pipe.writers == 0 {
                events |= 0x10;
            }
        }
        if self.can_write {
            if pipe.readers == 0 {
                events |= 0x08;
            } else if pipe.queue.len() < PIPE_CAPACITY {
                events |= 0x04;
            }
        }
        events
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }
}

impl Drop for FifoEndInode {
    fn drop(&mut self) {
        let mut pipe = self.shared.lock();
        if self.can_read {
            pipe.readers = pipe.readers.saturating_sub(1);
        }
        if self.can_write {
            pipe.writers = pipe.writers.saturating_sub(1);
        }
        crate::ipc::poll_kick();
    }
}
