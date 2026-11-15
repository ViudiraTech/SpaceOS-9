/*
 *
 *       src/drm/syncobj.rs
 *       DRM fences, sync files and binary/timeline sync objects
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux `dma_fence` + `sync_file` + `drm_syncobj`, driver independent. A
//! [`Fence`] is signalled either by hand (`complete`) or by a driver
//! [`FenceSource`] (a submitted GPU command). Every state change bumps a
//! global event counter; waiters sleep on one wait queue and re-check, so a
//! wait over many fences of different drivers needs no per-fence callbacks.

use super::{file::File, ioctl::Ioctl};
use crate::{
    sync::SpinLock,
    user::uaccess,
    vfs::{self, FdEntry, Inode, InodeKind, OpenFile, Stat, VfsError},
};
use alloc::{collections::BTreeMap, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};

/// Bumped whenever any fence may have changed state: a command retired, a
/// fence was signalled or bound, or a point was attached to a syncobj.
static FENCE_EVENTS: AtomicU64 = AtomicU64::new(0);
static FENCE_WAIT: crate::sched::WaitQueueHead = crate::sched::WaitQueueHead::new();

pub fn fence_event() {
    FENCE_EVENTS.fetch_add(1, Ordering::AcqRel);
    FENCE_WAIT.wake_all();
}

pub fn fence_events() -> u64 {
    FENCE_EVENTS.load(Ordering::Acquire)
}

/// Sleep until a fence event after `seen` or the deadline (uptime micros).
pub fn wait_fence_event(seen: u64, deadline: Option<u64>) {
    let _ = FENCE_WAIT.wait_event(|| fence_events() != seen, true, deadline);
}

/// What signals a driver fence (Linux `dma_fence_ops`).
pub trait FenceSource: Send + Sync {
    /// 0 pending, 1 signalled, negative errno-like failure.
    fn status(&self) -> i32;
    /// Block until the source completed (it may also be polled).
    fn wait(&self) -> Result<(), VfsError>;
}

pub struct Fence {
    source: SpinLock<Option<Arc<dyn FenceSource>>>,
    manual: AtomicI32,
}

impl Fence {
    pub fn from_source(source: Arc<dyn FenceSource>) -> Arc<Self> {
        Arc::new(Self {
            source: SpinLock::new(Some(source)),
            manual: AtomicI32::new(0),
        })
    }
    pub fn pending() -> Arc<Self> {
        Arc::new(Self {
            source: SpinLock::new(None),
            manual: AtomicI32::new(0),
        })
    }
    pub fn signaled() -> Arc<Self> {
        Arc::new(Self {
            source: SpinLock::new(None),
            manual: AtomicI32::new(1),
        })
    }
    /// Make a placeholder follow `other` (an out-fence fd reserved before the
    /// command was submitted).
    pub fn bind(&self, other: &Fence) {
        let source = other.source.lock().clone();
        *self.source.lock() = source;
        fence_event();
    }
    pub fn complete(&self, status: i32) {
        self.manual.store(status, Ordering::Release);
        fence_event();
    }
    pub fn status(&self) -> i32 {
        match self.source.lock().clone() {
            Some(source) => source.status(),
            None => self.manual.load(Ordering::Acquire),
        }
    }
    pub fn wait(&self) -> Result<(), VfsError> {
        let source = self.source.lock().clone();
        if let Some(source) = source {
            source.wait()?;
        }
        let deadline = crate::time::uptime_micros().saturating_add(5_000_000);
        loop {
            let seen = fence_events();
            if self.status() != 0 {
                break;
            }
            if crate::time::uptime_micros() >= deadline {
                return Err(VfsError::TimedOut);
            }
            wait_fence_event(seen, Some(deadline));
        }
        if self.status() < 0 {
            Err(VfsError::IoError)
        } else {
            Ok(())
        }
    }
}

pub struct SyncObject {
    points: SpinLock<BTreeMap<u64, Arc<Fence>>>,
}

impl SyncObject {
    pub fn new(signaled: bool) -> Arc<Self> {
        let mut points = BTreeMap::new();
        if signaled {
            points.insert(0, Fence::signaled());
        }
        Arc::new(Self {
            points: SpinLock::new(points),
        })
    }
    pub fn fence(&self, point: u64) -> Option<Arc<Fence>> {
        let points = self.points.lock();
        if point == 0 {
            points.get(&0).cloned()
        } else {
            points.range(point..).next().map(|(_, f)| f.clone())
        }
    }
    pub fn insert(&self, point: u64, fence: Arc<Fence>) -> Result<(), VfsError> {
        let mut points = self.points.lock();
        if points.len() >= 4096 {
            let newest = points
                .iter()
                .rev()
                .find(|(_, f)| f.status() != 0)
                .map(|(&p, _)| p);
            if let Some(newest) = newest {
                points.retain(|&p, _| p == 0 || p >= newest);
            }
        }
        if points.len() >= 4096 {
            return Err(VfsError::NoSpace);
        }
        points.insert(point, fence);
        drop(points);
        // WAIT_FOR_SUBMIT waiters may be waiting for this point to appear.
        fence_event();
        Ok(())
    }
    pub fn reset(&self) {
        self.points.lock().remove(&0);
    }
    fn query(&self, submitted: bool) -> u64 {
        self.points
            .lock()
            .iter()
            .rev()
            .find(|(_, f)| submitted || f.status() != 0)
            .map(|(&p, _)| p)
            .unwrap_or(0)
    }
}

/// A sync_file fd.
pub struct SyncFile {
    pub fence: Arc<Fence>,
}

impl Inode for SyncFile {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(0x90000001, InodeKind::Regular, 0o600, 0))
    }
    fn poll(&self) -> u32 {
        match self.fence.status() {
            0 => 0,
            n if n < 0 => 1 | 8,
            _ => 1,
        }
    }
}

/// An exported syncobj fd (opaque transfer between processes).
struct ObjectFile {
    object: Arc<SyncObject>,
}

impl Inode for ObjectFile {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(0x90000002, InodeKind::Regular, 0o600, 0))
    }
}

/// Per-file syncobj handle table.
pub struct Table {
    next: u32,
    objects: BTreeMap<u32, Arc<SyncObject>>,
}

impl Table {
    pub fn new() -> Self {
        Self {
            next: 1,
            objects: BTreeMap::new(),
        }
    }
    pub fn get(&self, handle: u32) -> Result<Arc<SyncObject>, VfsError> {
        self.objects.get(&handle).cloned().ok_or(VfsError::NotFound)
    }
    fn add(&mut self, object: Arc<SyncObject>) -> Result<u32, VfsError> {
        if self.objects.len() >= 4096 {
            return Err(VfsError::NoSpace);
        }
        let handle = self.next;
        self.next = handle.checked_add(1).ok_or(VfsError::NoSpace)?;
        self.objects.insert(handle, object);
        Ok(handle)
    }
}

// ---- fd helpers ----------------------------------------------------------------

/// Install `inode` as a new fd of the current process (anon inode).
pub fn install_fd(inode: Arc<dyn Inode>, flags: u32) -> Result<u32, VfsError> {
    let file = Arc::new(OpenFile::new(
        inode,
        String::from("anon_inode:[drm]"),
        flags & 3,
    ));
    let fd = vfs::current_fd_table().lock().alloc_fd(
        0,
        FdEntry {
            file,
            cloexec: flags & vfs::abi::O_CLOEXEC != 0,
        },
    )?;
    Ok(fd as u32)
}

pub fn close_fd(fd: u32) {
    let _ = vfs::current_fd_table().lock().close(fd as usize);
}

pub fn fd_inode(fd: u32) -> Result<Arc<dyn Inode>, VfsError> {
    Ok(vfs::current_fd_table()
        .lock()
        .get(fd as usize)
        .ok_or(VfsError::BadFileDescriptor)?
        .file
        .inode
        .clone())
}

/// The fence of a sync_file fd.
pub fn fence_fd(fd: u32) -> Result<Arc<Fence>, VfsError> {
    let inode: Arc<dyn core::any::Any + Send + Sync> = fd_inode(fd)?;
    Ok(inode
        .downcast::<SyncFile>()
        .map_err(|_| VfsError::InvalidArgument)?
        .fence
        .clone())
}

/// `struct drm_virtgpu_execbuffer_syncobj`-style arrays: handle, flags
/// (bit 0: reset after wait), point, padding.
pub fn exec_syncs(
    file: &File,
    address: u64,
    count: usize,
    stride: usize,
) -> Result<Vec<(Arc<SyncObject>, u64, bool)>, VfsError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > 4096 || !(16..=256).contains(&stride) {
        return Err(VfsError::InvalidArgument);
    }
    let bytes = uaccess::read_vec(
        address,
        count.checked_mul(stride).ok_or(VfsError::InvalidArgument)?,
        1024 * 1024,
    )?;
    let table = file.syncobjs();
    let mut out = Vec::new();
    for item in bytes.chunks_exact(stride) {
        let word = |o: usize| u32::from_le_bytes(item[o..o + 4].try_into().unwrap());
        let flags = word(4);
        if flags & !1 != 0 || item[16..].iter().any(|&b| b != 0) {
            return Err(VfsError::InvalidArgument);
        }
        let point = u64::from_le_bytes(item[8..16].try_into().unwrap());
        out.push((table.get(word(0))?, point, flags & 1 != 0));
    }
    Ok(out)
}

fn read_u32s(address: u64, count: usize) -> Result<Vec<u32>, VfsError> {
    super::ioctl::read_u32s(address, count)
}

fn read_u64s(address: u64, count: usize) -> Result<Vec<u64>, VfsError> {
    super::ioctl::read_u64s(address, count)
}

fn objects(file: &File, handles: Vec<u32>) -> Result<Vec<Arc<SyncObject>>, VfsError> {
    let table = file.syncobjs();
    handles.into_iter().map(|h| table.get(h)).collect()
}

/// DRM_IOCTL_SYNCOBJ_*.
pub(crate) fn ioctl(file: &File, arg: &mut Ioctl) -> Result<(), VfsError> {
    use super::ioctl::*;
    match arg.nr {
        SYNCOBJ_CREATE => {
            let flags = arg.u32(4);
            if flags & !1 != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let handle = file.syncobjs().add(SyncObject::new(flags & 1 != 0))?;
            arg.set_u32(0, handle);
            Ok(())
        }
        SYNCOBJ_DESTROY => {
            if arg.u32(4) != 0 {
                return Err(VfsError::InvalidArgument);
            }
            file.syncobjs()
                .objects
                .remove(&arg.u32(0))
                .ok_or(VfsError::NotFound)?;
            Ok(())
        }
        SYNCOBJ_HANDLE_TO_FD => {
            let object = file.syncobjs().get(arg.u32(0))?;
            let flags = arg.u32(4);
            if flags & !3 != 0 || arg.u32(12) != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let inode: Arc<dyn Inode> = if flags & 1 != 0 {
                Arc::new(SyncFile {
                    fence: object
                        .fence(if flags & 2 != 0 { arg.u64(16) } else { 0 })
                        .ok_or(VfsError::InvalidArgument)?,
                })
            } else {
                Arc::new(ObjectFile { object })
            };
            let fd = install_fd(inode, vfs::abi::O_CLOEXEC)?;
            arg.set_u32(8, fd);
            Ok(())
        }
        SYNCOBJ_FD_TO_HANDLE => {
            let flags = arg.u32(4);
            if flags & !3 != 0 || arg.u32(12) != 0 {
                return Err(VfsError::InvalidArgument);
            }
            if flags & 1 != 0 {
                let fence = fence_fd(arg.u32(8))?;
                let object = file.syncobjs().get(arg.u32(0))?;
                object.insert(if flags & 2 != 0 { arg.u64(16) } else { 0 }, fence)?;
            } else {
                let inode: Arc<dyn core::any::Any + Send + Sync> = fd_inode(arg.u32(8))?;
                let object = inode
                    .downcast::<ObjectFile>()
                    .map_err(|_| VfsError::InvalidArgument)?
                    .object
                    .clone();
                let handle = file.syncobjs().add(object)?;
                arg.set_u32(0, handle);
            }
            Ok(())
        }
        SYNCOBJ_RESET | SYNCOBJ_SIGNAL => {
            if arg.u32(12) != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let objects = objects(file, read_u32s(arg.u64(0), arg.u32(8) as usize)?)?;
            for object in objects {
                if arg.nr == SYNCOBJ_RESET {
                    object.reset();
                } else {
                    object.insert(0, Fence::signaled())?;
                }
            }
            Ok(())
        }
        SYNCOBJ_QUERY | SYNCOBJ_TIMELINE_SIGNAL => {
            let count = arg.u32(16) as usize;
            let flags = arg.u32(20);
            let allowed = if arg.nr == SYNCOBJ_QUERY { 1 } else { 0 };
            if flags & !allowed != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let objects = objects(file, read_u32s(arg.u64(0), count)?)?;
            if arg.nr == SYNCOBJ_QUERY {
                let values: Vec<u8> = objects
                    .iter()
                    .flat_map(|o| o.query(flags & 1 != 0).to_le_bytes())
                    .collect();
                uaccess::write(arg.u64(8), &values)?;
            } else {
                let points = read_u64s(arg.u64(8), count)?;
                for (object, point) in objects.into_iter().zip(points) {
                    if point == 0 {
                        return Err(VfsError::InvalidArgument);
                    }
                    object.insert(point, Fence::signaled())?;
                }
            }
            Ok(())
        }
        SYNCOBJ_TRANSFER => {
            if arg.u32(24) != 0 || arg.u32(28) != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let (source, destination) = {
                let table = file.syncobjs();
                (table.get(arg.u32(0))?, table.get(arg.u32(4))?)
            };
            let fence = source.fence(arg.u64(8)).ok_or(VfsError::InvalidArgument)?;
            destination.insert(arg.u64(16), fence)
        }
        SYNCOBJ_WAIT | SYNCOBJ_TIMELINE_WAIT => {
            let timeline = arg.nr == SYNCOBJ_TIMELINE_WAIT;
            let count = arg.u32(if timeline { 24 } else { 16 }) as usize;
            let flags = arg.u32(if timeline { 28 } else { 20 });
            if count == 0
                || count > 4096
                || flags & !7 != 0
                || arg.u32(if timeline { 36 } else { 28 }) != 0
            {
                return Err(VfsError::InvalidArgument);
            }
            let objects = objects(file, read_u32s(arg.u64(0), count)?)?;
            let points = if timeline {
                read_u64s(arg.u64(8), count)?
            } else {
                vec![0; count]
            };
            let timeout = arg.u64(if timeline { 16 } else { 8 }) as i64;
            let device = file.device().clone();
            let first = loop {
                let seen = fence_events();
                let mut ready = 0;
                let mut first = 0;
                for (index, (object, point)) in objects.iter().zip(&points).enumerate() {
                    if let Some(fence) = object.fence(*point) {
                        // WAIT_AVAILABLE (4): a fence being attached is enough.
                        if flags & 4 != 0 || fence.status() != 0 {
                            if ready == 0 {
                                first = index;
                            }
                            ready += 1;
                        }
                    } else if flags & 2 == 0 {
                        // No fence and not WAIT_FOR_SUBMIT.
                        return Err(VfsError::InvalidArgument);
                    }
                }
                // WAIT_ALL (1) or any.
                if flags & 1 != 0 && ready == count || flags & 1 == 0 && ready != 0 {
                    break first;
                }
                if timeout >= 0
                    && crate::time::uptime_micros().saturating_mul(1000) >= timeout as u64
                {
                    return Err(VfsError::TimedOut);
                }
                device.driver.kick();
                if crate::signal::pending_for_current() {
                    return Err(VfsError::Interrupted);
                }
                // DRM timeouts are absolute CLOCK_MONOTONIC nanoseconds.
                let deadline = (timeout >= 0).then(|| (timeout as u64).div_ceil(1000));
                wait_fence_event(seen, deadline);
            };
            arg.set_u32(if timeline { 32 } else { 24 }, first as u32);
            Ok(())
        }
        _ => Err(VfsError::InvalidArgument),
    }
}
