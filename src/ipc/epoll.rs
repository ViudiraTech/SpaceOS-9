/*
 *
 *       src/ipc/epoll.rs
 *       Linux epoll interest lists, level/edge triggering and oneshot
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Epoll is implemented as a polled interest list.  Readiness is recomputed
//! from each inode's `poll` on every wait, so no wakeup plumbing is required;
//! level triggering reports every ready wait, edge triggering reports only
//! newly-set bits (tracked per interest), and EPOLLONESHOT disables an
//! interest until `EPOLL_CTL_MOD` re-arms it.  An interest whose fd no longer
//! refers to the registered open file is dropped, matching Linux's
//! file-release cleanup.

use crate::{
    sync::SpinLock,
    user::uaccess,
    vfs::{self, Inode, InodeKind, OpenFile, Stat, VfsError},
};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

pub const EPOLLIN: u32 = 0x001;
pub const EPOLLPRI: u32 = 0x002;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLLEXCLUSIVE: u32 = 1 << 28;
pub const EPOLLWAKEUP: u32 = 1 << 29;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;

pub const EPOLL_CTL_ADD: u32 = 1;
pub const EPOLL_CTL_DEL: u32 = 2;
pub const EPOLL_CTL_MOD: u32 = 3;

const EPOLL_EVENT_SIZE: usize = 12;
const MAX_WAIT_EVENTS: usize = 1024;
const MAX_NESTING: usize = 4;

struct Interest {
    fd: i32,
    file: Arc<OpenFile>,
    events: u32,
    data: u64,
    enabled: bool,
    last_ready: u32,
    last_read_generation: u64,
}

pub struct EpollInstance {
    interests: SpinLock<Vec<Interest>>,
    nonblock: AtomicBool,
    ino: u64,
}

impl EpollInstance {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            interests: SpinLock::new(Vec::new()),
            nonblock: AtomicBool::new(false),
            ino: vfs::tree::ino(),
        })
    }

    pub fn set_nonblock(&self, nonblock: bool) {
        self.nonblock.store(nonblock, Ordering::Release);
    }

    fn file_of(&self, fd: i32) -> Result<Arc<OpenFile>, VfsError> {
        if fd < 0 {
            return Err(VfsError::BadFileDescriptor);
        }
        vfs::current_fd_table()
            .lock()
            .get(fd as usize)
            .map(|entry| entry.file)
            .ok_or(VfsError::BadFileDescriptor)
    }

    fn find(&self, fd: i32, file: &Arc<OpenFile>) -> Option<usize> {
        self.interests
            .lock()
            .iter()
            .position(|interest| interest.fd == fd && Arc::ptr_eq(&interest.file, file))
    }

    pub fn control(&self, op: u32, fd: i32, event: u64) -> Result<(), VfsError> {
        let result = self.apply_control(op, fd, event);
        crate::ipc::wake_object(self as *const Self);
        result
    }

    fn apply_control(&self, op: u32, fd: i32, event: u64) -> Result<(), VfsError> {
        let file = self.file_of(fd)?;
        match op {
            EPOLL_CTL_ADD => {
                let (events, data) = read_event(event)?;
                if inode_ptr(&file.inode) == self as *const Self as *const () {
                    return Err(VfsError::InvalidArgument);
                }
                if let Some(instance) = as_epoll(&file.inode)
                    && instance.transitively_contains(self as *const Self as *const (), 0)
                {
                    return Err(VfsError::TooManySymlinks);
                }
                let mut interests = self.interests.lock();
                if interests
                    .iter()
                    .any(|interest| interest.fd == fd && Arc::ptr_eq(&interest.file, &file))
                {
                    return Err(VfsError::AlreadyExists);
                }
                interests.push(Interest {
                    fd,
                    file,
                    events,
                    data,
                    enabled: true,
                    last_ready: 0,
                    last_read_generation: 0,
                });
                Ok(())
            }
            EPOLL_CTL_MOD => {
                let (events, data) = read_event(event)?;
                let mut interests = self.interests.lock();
                let Some(interest) = interests
                    .iter_mut()
                    .find(|interest| interest.fd == fd && Arc::ptr_eq(&interest.file, &file))
                else {
                    return Err(VfsError::NotFound);
                };
                interest.events = events;
                interest.data = data;
                interest.enabled = true;
                interest.last_ready = 0;
                interest.last_read_generation = 0;
                Ok(())
            }
            EPOLL_CTL_DEL => {
                let mut interests = self.interests.lock();
                let Some(index) = interests
                    .iter()
                    .position(|interest| interest.fd == fd && Arc::ptr_eq(&interest.file, &file))
                else {
                    return Err(VfsError::NotFound);
                };
                interests.remove(index);
                Ok(())
            }
            _ => Err(VfsError::InvalidArgument),
        }
    }

    /// Remove interests whose fd no longer references the registered file.
    fn prune(&self) {
        let table = vfs::current_fd_table();
        let table = table.lock();
        self.interests
            .lock()
            .retain(|interest| fd_table_has(&table, &interest.file));
    }

    fn ready_mask(interest: &Interest) -> u32 {
        let available = interest.file.inode.poll();
        available & (interest.events | EPOLLERR | EPOLLHUP)
    }

    pub fn wait(&self, events_pointer: u64, maxevents: u32, timeout_ms: i64) -> i64 {
        if maxevents == 0 {
            return -crate::vfs::abi::EINVAL;
        }
        let deadline = if timeout_ms < 0 {
            u64::MAX
        } else {
            crate::time::uptime_micros().saturating_add((timeout_ms as u64).saturating_mul(1000))
        };
        let mut wait = crate::ipc::pollwait::PollWait::new();
        loop {
            self.prune();
            if wait.registered() {
                let mut sources = crate::ipc::pollwait::PollSources::new();
                if deadline != u64::MAX {
                    sources.expire_at(deadline);
                }
                self.gather_sources(&mut sources);
                wait.arm(&sources);
            }
            let mut output = Vec::new();
            let limit = (maxevents as usize).min(MAX_WAIT_EVENTS);
            {
                let mut interests = self.interests.lock();
                for interest in interests.iter_mut() {
                    if !interest.enabled {
                        continue;
                    }
                    let ready = Self::ready_mask(interest);
                    if ready == 0 {
                        interest.last_ready = 0;
                        continue;
                    }
                    let report = if interest.events & EPOLLET != 0 {
                        let generation = interest.file.inode.poll_read_generation();
                        let mut novel = ready & !interest.last_ready;
                        if generation != interest.last_read_generation {
                            novel |= ready & EPOLLIN;
                        }
                        interest.last_ready = ready;
                        interest.last_read_generation = generation;
                        novel
                    } else {
                        ready
                    };
                    if report == 0 {
                        continue;
                    }
                    if interest.events & EPOLLONESHOT != 0 {
                        interest.enabled = false;
                    }
                    let mut record = [0u8; EPOLL_EVENT_SIZE];
                    record[0..4].copy_from_slice(&report.to_le_bytes());
                    record[4..12].copy_from_slice(&interest.data.to_le_bytes());
                    output.extend_from_slice(&record);
                    if output.len() >= limit * EPOLL_EVENT_SIZE {
                        break;
                    }
                }
            }
            if !output.is_empty() {
                if let Err(error) = uaccess::write(events_pointer, &output) {
                    return error.to_errno();
                }
                return (output.len() / EPOLL_EVENT_SIZE) as i64;
            }
            if crate::time::uptime_micros() >= deadline {
                return 0;
            }
            if crate::signal::pending_for_current() {
                return -crate::vfs::abi::EINTR;
            }
            wait.sleep();
        }
    }

    fn gather_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        // EPOLL_CTL from another thread announces itself through this object.
        sources.object(self as *const Self);
        for interest in self.interests.lock().iter() {
            if interest.enabled {
                interest.file.inode.poll_sources(sources);
            }
        }
    }

    fn transitively_contains(&self, target: *const (), depth: usize) -> bool {
        if depth > MAX_NESTING {
            return true;
        }
        let interests = self.interests.lock();
        for interest in interests.iter() {
            if inode_ptr(&interest.file.inode) == target {
                return true;
            }
            if let Some(inner) = as_epoll(&interest.file.inode)
                && inner.transitively_contains(target, depth + 1)
            {
                return true;
            }
        }
        false
    }
}

fn inode_ptr(inode: &Arc<dyn Inode>) -> *const () {
    Arc::as_ptr(inode) as *const ()
}

fn as_epoll(inode: &Arc<dyn Inode>) -> Option<Arc<EpollInstance>> {
    let any: Arc<dyn core::any::Any + Send + Sync> = inode.clone();
    any.downcast::<EpollInstance>().ok()
}

fn fd_table_has(table: &crate::vfs::FileDescriptorTable, file: &Arc<OpenFile>) -> bool {
    table
        .snapshot()
        .iter()
        .any(|(_, entry)| Arc::ptr_eq(&entry.file, file))
}

fn read_event(pointer: u64) -> Result<(u32, u64), VfsError> {
    let bytes = uaccess::read_vec(pointer, EPOLL_EVENT_SIZE, EPOLL_EVENT_SIZE)?;
    let events = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let data = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
    Ok((events, data))
}

impl Inode for EpollInstance {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::CharDevice, 0o600, 0))
    }

    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, VfsError> {
        Err(VfsError::InvalidArgument)
    }

    fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::InvalidArgument)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        self.gather_sources(sources);
    }

    fn poll(&self) -> u32 {
        self.prune();
        let interests = self.interests.lock();
        for interest in interests.iter() {
            if !interest.enabled {
                continue;
            }
            if Self::ready_mask(interest) != 0 {
                return EPOLLIN;
            }
        }
        0
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.set_nonblock(nonblock);
        Ok(())
    }
}

/// Resolve an epoll fd to its instance.
pub fn instance(fd: u64) -> Result<Arc<EpollInstance>, VfsError> {
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
    any.downcast::<EpollInstance>()
        .map_err(|_| VfsError::InvalidArgument)
}

pub fn create(flags: u32) -> Result<Arc<EpollInstance>, VfsError> {
    // Linux epoll_create1 accepts only EPOLL_CLOEXEC (= O_CLOEXEC).
    if flags & !crate::vfs::abi::O_CLOEXEC != 0 {
        return Err(VfsError::InvalidArgument);
    }
    Ok(EpollInstance::new())
}
