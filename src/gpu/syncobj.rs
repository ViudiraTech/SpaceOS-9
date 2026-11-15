/*
 *       src/gpu/syncobj.rs
 *       Shared binary/timeline fences, sync-file exports and waits
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
use super::*;
use core::sync::atomic::{AtomicI32, AtomicU64};

/// Bumped whenever any fence may have changed state: a GPU command retired,
/// a fence was signalled or bound, or a point was attached to a syncobj.
/// Fence waiters sleep on FENCE_WAIT until it moves and then re-check.
static FENCE_EVENTS: AtomicU64 = AtomicU64::new(0);
static FENCE_WAIT: crate::sched::WaitQueueHead = crate::sched::WaitQueueHead::new();

pub fn fence_event() {
    FENCE_EVENTS.fetch_add(1, Ordering::AcqRel);
    FENCE_WAIT.wake_all();
}

pub(super) fn fence_events() -> u64 {
    FENCE_EVENTS.load(Ordering::Acquire)
}

/// Sleep until a fence event after `seen` or the deadline (uptime micros).
pub(super) fn wait_fence_event(seen: u64, deadline: Option<u64>) {
    let _ = FENCE_WAIT.wait_event(|| fence_events() != seen, true, deadline);
}

pub struct Fence {
    request: SpinLock<Option<Arc<Request>>>,
    manual: AtomicI32,
}
impl Fence {
    pub fn request(request: Arc<Request>) -> Arc<Self> {
        Arc::new(Self {
            request: SpinLock::new(Some(request)),
            manual: AtomicI32::new(0),
        })
    }
    pub fn pending() -> Arc<Self> {
        Arc::new(Self {
            request: SpinLock::new(None),
            manual: AtomicI32::new(0),
        })
    }
    pub fn bind(&self, other: &Fence) {
        let request = other.request.lock().clone();
        *self.request.lock() = request;
        fence_event();
    }
    pub fn complete(&self, status: i32) {
        self.manual.store(status, Ordering::Release);
        fence_event();
    }
    pub fn signaled() -> Arc<Self> {
        Arc::new(Self {
            request: SpinLock::new(None),
            manual: AtomicI32::new(1),
        })
    }
    pub fn status(&self) -> i32 {
        if let Some(request) = self.request.lock().clone() {
            if !request.done.load(Ordering::Acquire) {
                0
            } else if request.error.load(Ordering::Acquire) != 0 {
                -5
            } else {
                let kind = unsafe { (request.response.pointer() as *const u32).read_volatile() };
                if (0x1100..=0x1106).contains(&u32::from_le(kind)) {
                    1
                } else {
                    -5
                }
            }
        } else {
            self.manual.load(Ordering::Acquire)
        }
    }
    pub fn wait(&self, gpu: &Gpu) -> Result<(), VfsError> {
        let request = self.request.lock().clone();
        if let Some(request) = request {
            gpu.transport.wait(&request)?;
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
struct ObjectFile {
    object: Arc<SyncObject>,
}
impl Inode for ObjectFile {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(0x90000002, InodeKind::Regular, 0o600, 0))
    }
}
pub fn fence_fd(fd: u32) -> Result<Arc<Fence>, VfsError> {
    let inode: Arc<dyn core::any::Any + Send + Sync> = fd_inode(fd)?;
    Ok(inode
        .downcast::<SyncFile>()
        .map_err(|_| VfsError::InvalidArgument)?
        .fence
        .clone())
}
fn get(state: &SessionState, handle: u32) -> Result<Arc<SyncObject>, VfsError> {
    state.syncs.get(&handle).cloned().ok_or(VfsError::NotFound)
}
fn add(state: &mut SessionState, object: Arc<SyncObject>) -> Result<u32, VfsError> {
    if state.syncs.len() >= 4096 {
        return Err(VfsError::NoSpace);
    }
    let h = state.next_sync;
    state.next_sync = h.checked_add(1).ok_or(VfsError::NoSpace)?;
    state.syncs.insert(h, object);
    Ok(h)
}
pub fn exec_syncs(
    state: &SessionState,
    address: u64,
    count: usize,
    stride: usize,
) -> Result<Vec<(Arc<SyncObject>, u64, bool)>, VfsError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > 4096 || stride < 16 || stride > 256 {
        return Err(VfsError::InvalidArgument);
    }
    let bytes = uaccess::read_vec(
        address,
        count.checked_mul(stride).ok_or(VfsError::InvalidArgument)?,
        1024 * 1024,
    )?;
    let mut out = Vec::new();
    for item in bytes.chunks_exact(stride) {
        let flags = u32_at(item, 4)?;
        if flags & !1 != 0 || item[16..].iter().any(|&b| b != 0) {
            return Err(VfsError::InvalidArgument);
        }
        out.push((
            get(state, u32_at(item, 0)?)?,
            u64_at(item, 8)?,
            flags & 1 != 0,
        ));
    }
    Ok(out)
}
pub fn ioctl(
    session: &Session,
    state: &mut SessionState,
    nr: u8,
    arg: &mut Ioctl,
) -> Result<(), VfsError> {
    match nr {
        0xbf => {
            let flags = arg.r32(4)?;
            if flags & !1 != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let handle = add(state, SyncObject::new(flags & 1 != 0))?;
            arg.w32(0, handle);
            Ok(())
        }
        0xc0 => {
            if arg.r32(4)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            state.syncs.remove(&arg.r32(0)?).ok_or(VfsError::NotFound)?;
            Ok(())
        }
        0xc1 => {
            let object = get(state, arg.r32(0)?)?;
            let flags = arg.r32(4)?;
            if flags & !3 != 0 || arg.r32(12)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let inode: Arc<dyn Inode> = if flags & 1 != 0 {
                Arc::new(SyncFile {
                    fence: object
                        .fence(if flags & 2 != 0 { arg.r64(16)? } else { 0 })
                        .ok_or(VfsError::InvalidArgument)?,
                })
            } else {
                Arc::new(ObjectFile { object })
            };
            let fd = install_fd(inode, vfs::abi::O_CLOEXEC)?;
            arg.w32(8, fd);
            Ok(())
        }
        0xc2 => {
            let flags = arg.r32(4)?;
            if flags & !3 != 0 || arg.r32(12)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            if flags & 1 != 0 {
                let fence = fence_fd(arg.r32(8)?)?;
                get(state, arg.r32(0)?)?
                    .insert(if flags & 2 != 0 { arg.r64(16)? } else { 0 }, fence)?;
            } else {
                let inode: Arc<dyn core::any::Any + Send + Sync> = fd_inode(arg.r32(8)?)?;
                let object = inode
                    .downcast::<ObjectFile>()
                    .map_err(|_| VfsError::InvalidArgument)?
                    .object
                    .clone();
                let handle = add(state, object)?;
                arg.w32(0, handle);
            }
            Ok(())
        }
        0xc4 | 0xc5 => {
            if arg.r32(12)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let handles = array32(arg.r64(0)?, arg.r32(8)? as usize)?;
            let objects: Vec<_> = handles
                .into_iter()
                .map(|h| get(state, h))
                .collect::<Result<_, _>>()?;
            for object in objects {
                if nr == 0xc4 {
                    object.reset();
                } else {
                    object.insert(0, Fence::signaled())?;
                }
            }
            Ok(())
        }
        0xcb | 0xcd => {
            let count = arg.r32(16)? as usize;
            let flags = arg.r32(20)?;
            if flags & !if nr == 0xcb { 1 } else { 0 } != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let handles = array32(arg.r64(0)?, count)?;
            let objects: Vec<_> = handles
                .into_iter()
                .map(|h| get(state, h))
                .collect::<Result<_, _>>()?;
            if nr == 0xcb {
                let values: Vec<_> = objects.iter().map(|o| o.query(flags & 1 != 0)).collect();
                write64(arg.r64(8)?, &values)?;
            } else {
                let points = array64(arg.r64(8)?, count)?;
                for (object, point) in objects.into_iter().zip(points) {
                    if point == 0 {
                        return Err(VfsError::InvalidArgument);
                    }
                    object.insert(point, Fence::signaled())?;
                }
            }
            Ok(())
        }
        0xcc => {
            if arg.r32(24)? != 0 || arg.r32(28)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let source = get(state, arg.r32(0)?)?;
            let destination = get(state, arg.r32(4)?)?;
            let fence = source.fence(arg.r64(8)?).ok_or(VfsError::InvalidArgument)?;
            destination.insert(arg.r64(16)?, fence)
        }
        0xc3 | 0xca => {
            let timeline = nr == 0xca;
            let count = arg.r32(if timeline { 24 } else { 16 })? as usize;
            let flags = arg.r32(if timeline { 28 } else { 20 })?;
            if count == 0
                || count > 4096
                || flags & !7 != 0
                || arg.r32(if timeline { 36 } else { 28 })? != 0
            {
                return Err(VfsError::InvalidArgument);
            }
            let handles = array32(arg.r64(0)?, count)?;
            let points = if timeline {
                array64(arg.r64(8)?, count)?
            } else {
                vec![0; count]
            };
            let objects: Vec<_> = handles
                .into_iter()
                .map(|h| get(state, h))
                .collect::<Result<_, _>>()?;
            let timeout = arg.r64(if timeline { 16 } else { 8 })? as i64;
            let first = session.unlocked(state, || {
                loop {
                    let seen = fence_events();
                    let mut ready = 0;
                    let mut first = 0;
                    for (index, (object, point)) in objects.iter().zip(&points).enumerate() {
                        if let Some(fence) = object.fence(*point) {
                            if flags & 4 != 0 || fence.status() != 0 {
                                if ready == 0 {
                                    first = index;
                                }
                                ready += 1;
                            }
                        } else if flags & 2 == 0 {
                            return Err(VfsError::InvalidArgument);
                        }
                    }
                    if flags & 1 != 0 && ready == count || flags & 1 == 0 && ready != 0 {
                        return Ok(first);
                    }
                    if timeout >= 0
                        && crate::time::uptime_micros().saturating_mul(1000) >= timeout as u64
                    {
                        return Err(VfsError::TimedOut);
                    }
                    session.gpu.transport.poll();
                    if crate::signal::pending_for_current() {
                        return Err(VfsError::Interrupted);
                    }
                    // DRM timeouts are absolute CLOCK_MONOTONIC nanoseconds.
                    let deadline = (timeout >= 0).then(|| (timeout as u64).div_ceil(1000));
                    wait_fence_event(seen, deadline);
                }
            })?;
            arg.w32(if timeline { 32 } else { 24 }, first as u32);
            Ok(())
        }
        _ => Err(VfsError::NotATty),
    }
}
