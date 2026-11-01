/*
 *
 *       src/ipc/inotify.rs
 *       Linux inotify file change notification
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Watches are keyed by resolved absolute path because the VFS hooks report
//! changes by path. A watched directory receives child events with a name, and
//! a watched file receives its own events without one.

use crate::{
    sync::SpinLock,
    vfs::{self, Inode, InodeKind, Stat, VfsError},
};
use alloc::{
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicUsize, Ordering};

pub const IN_MODIFY: u32 = 0x0000_0002;
pub const IN_CLOSE_WRITE: u32 = 0x0000_0008;
pub const IN_MOVED_FROM: u32 = 0x0000_0040;
pub const IN_MOVED_TO: u32 = 0x0000_0080;
pub const IN_CREATE: u32 = 0x0000_0100;
pub const IN_DELETE: u32 = 0x0000_0200;
pub const IN_DELETE_SELF: u32 = 0x0000_0400;
const IN_ALL_EVENTS: u32 = 0x0000_0FFF;
const IN_Q_OVERFLOW: u32 = 0x0000_4000;
const IN_IGNORED: u32 = 0x0000_8000;
const IN_ONLYDIR: u32 = 0x0100_0000;
const IN_DONT_FOLLOW: u32 = 0x0200_0000;
const IN_EXCL_UNLINK: u32 = 0x0400_0000;
const IN_MASK_CREATE: u32 = 0x1000_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
pub const IN_ISDIR: u32 = 0x4000_0000;
const IN_ONESHOT: u32 = 0x8000_0000;
const WATCH_FLAGS: u32 =
    IN_ONLYDIR | IN_DONT_FOLLOW | IN_EXCL_UNLINK | IN_MASK_CREATE | IN_MASK_ADD | IN_ONESHOT;
const QUEUE_LIMIT: usize = 16384;

struct Watch {
    path: String,
    mask: u32,
}

struct Event {
    wd: i32,
    mask: u32,
    name: String,
}

struct State {
    watches: BTreeMap<i32, Watch>,
    next_wd: i32,
    queue: VecDeque<Event>,
    nonblock: bool,
}

pub struct Inotify {
    state: SpinLock<State>,
    ino: u64,
}

static INSTANCES: SpinLock<Vec<Weak<Inotify>>> = SpinLock::new(Vec::new());
static ACTIVE_WATCHES: AtomicUsize = AtomicUsize::new(0);

impl Inotify {
    pub fn create(nonblock: bool) -> Arc<Self> {
        let instance = Arc::new(Self {
            state: SpinLock::new(State {
                watches: BTreeMap::new(),
                next_wd: 1,
                queue: VecDeque::new(),
                nonblock,
            }),
            ino: vfs::tree::ino(),
        });
        let mut instances = INSTANCES.lock();
        instances.retain(|weak| weak.strong_count() != 0);
        instances.push(Arc::downgrade(&instance));
        instance
    }

    pub fn add_watch(&self, path: &str, mask: u32) -> Result<i32, VfsError> {
        if mask & (IN_ALL_EVENTS | WATCH_FLAGS) != mask || mask & IN_ALL_EVENTS == 0 {
            return Err(VfsError::InvalidArgument);
        }
        if mask & IN_MASK_ADD != 0 && mask & IN_MASK_CREATE != 0 {
            return Err(VfsError::InvalidArgument);
        }
        let (inode, resolved) = vfs::lookup_abs(path, mask & IN_DONT_FOLLOW == 0)?;
        if mask & IN_ONLYDIR != 0 && inode.metadata()?.kind != InodeKind::Directory {
            return Err(VfsError::NotADirectory);
        }
        let events = mask & !WATCH_FLAGS | (mask & IN_ONESHOT);
        let mut state = self.state.lock();
        if let Some((&wd, watch)) = state
            .watches
            .iter_mut()
            .find(|(_, watch)| watch.path == resolved)
        {
            if mask & IN_MASK_CREATE != 0 {
                return Err(VfsError::AlreadyExists);
            }
            watch.mask = if mask & IN_MASK_ADD != 0 {
                watch.mask | events
            } else {
                events
            };
            return Ok(wd);
        }
        let wd = state.next_wd;
        state.next_wd = state.next_wd.checked_add(1).ok_or(VfsError::NoSpace)?;
        state.watches.insert(
            wd,
            Watch {
                path: resolved,
                mask: events,
            },
        );
        ACTIVE_WATCHES.fetch_add(1, Ordering::Relaxed);
        Ok(wd)
    }

    pub fn remove_watch(&self, wd: i32) -> Result<(), VfsError> {
        let mut state = self.state.lock();
        if state.watches.remove(&wd).is_none() {
            return Err(VfsError::InvalidArgument);
        }
        ACTIVE_WATCHES.fetch_sub(1, Ordering::Relaxed);
        state.queue.push_back(Event {
            wd,
            mask: IN_IGNORED,
            name: String::new(),
        });
        Ok(())
    }

    fn record(&self, directory: &str, name: &str, full: &str, mask: u32) {
        let mut state = self.state.lock();
        let mut queued: Vec<Event> = Vec::new();
        let mut removed = Vec::new();
        for (&wd, watch) in state.watches.iter() {
            let (hit, event_name) = if watch.path == directory {
                (true, name)
            } else if watch.path == full {
                (true, "")
            } else {
                (false, "")
            };
            if !hit || watch.mask & mask & IN_ALL_EVENTS == 0 {
                continue;
            }
            queued.push(Event {
                wd,
                mask,
                name: String::from(event_name),
            });
            if watch.mask & IN_ONESHOT != 0 {
                removed.push(wd);
            }
        }
        for event in queued {
            if state.queue.len() >= QUEUE_LIMIT {
                if state
                    .queue
                    .back()
                    .is_none_or(|last| last.mask != IN_Q_OVERFLOW)
                {
                    state.queue.push_back(Event {
                        wd: -1,
                        mask: IN_Q_OVERFLOW,
                        name: String::new(),
                    });
                }
                break;
            }
            state.queue.push_back(event);
        }
        for wd in removed {
            state.watches.remove(&wd);
            ACTIVE_WATCHES.fetch_sub(1, Ordering::Relaxed);
            state.queue.push_back(Event {
                wd,
                mask: IN_IGNORED,
                name: String::new(),
            });
        }
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        let count = self.state.lock().watches.len();
        ACTIVE_WATCHES.fetch_sub(count, Ordering::Relaxed);
    }
}

/// Report a change of `path` to interested watches. Cheap when nothing watches.
pub fn notify(path: &str, mask: u32) {
    if ACTIVE_WATCHES.load(Ordering::Relaxed) == 0 {
        return;
    }
    let Some((directory, name)) = vfs::split_parent_name(path) else {
        return;
    };
    let instances: Vec<Arc<Inotify>> = INSTANCES.lock().iter().filter_map(Weak::upgrade).collect();
    for instance in instances {
        instance.record(&directory, &name, path, mask);
    }
    crate::ipc::wake_pollers();
}

impl Inode for Inotify {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::CharDevice, 0o600, 0))
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        loop {
            {
                let mut state = self.state.lock();
                if let Some(first) = state.queue.front() {
                    if buffer.len() < encoded_len(first) {
                        return Err(VfsError::InvalidArgument);
                    }
                    let mut written = 0;
                    while let Some(event) = state.queue.front() {
                        let size = encoded_len(event);
                        if written + size > buffer.len() {
                            break;
                        }
                        encode(event, &mut buffer[written..written + size]);
                        written += size;
                        state.queue.pop_front();
                    }
                    return Ok(written);
                }
                if state.nonblock {
                    return Err(VfsError::WouldBlock);
                }
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::sched::sleep_micros(1000);
        }
    }

    fn poll_sources(&self, _sources: &mut crate::ipc::pollwait::PollSources) {
        // Delivery announces itself through wake_pollers.
    }

    fn poll(&self) -> u32 {
        u32::from(!self.state.lock().queue.is_empty())
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.state.lock().nonblock = nonblock;
        Ok(())
    }

    /// `FIONREAD` reports the bytes a read would return.
    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        const FIONREAD: u64 = 0x541B;
        if cmd != FIONREAD {
            return Err(VfsError::NotATty);
        }
        let total: usize = self.state.lock().queue.iter().map(encoded_len).sum();
        crate::user::uaccess::write_value(arg, &(total as i32))?;
        Ok(0)
    }
}

fn encoded_len(event: &Event) -> usize {
    if event.name.is_empty() {
        16
    } else {
        16 + ((event.name.len() + 1).div_ceil(16)) * 16
    }
}

fn encode(event: &Event, out: &mut [u8]) {
    out.fill(0);
    out[0..4].copy_from_slice(&event.wd.to_le_bytes());
    out[4..8].copy_from_slice(&event.mask.to_le_bytes());
    let length = out.len() as u32 - 16;
    out[12..16].copy_from_slice(&length.to_le_bytes());
    out[16..16 + event.name.len()].copy_from_slice(event.name.as_bytes());
}

pub fn instance(fd: u64) -> Result<Arc<Inotify>, VfsError> {
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
    any.downcast::<Inotify>()
        .map_err(|_| VfsError::InvalidArgument)
}
