/*
 *       src/gpu/mod.rs
 *       DRM sessions, GEM resources and VirtIO VirGL/Venus dispatch
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
mod abi;
mod kms;
#[cfg(feature = "boot-self-test")]
pub mod self_test;
mod syncobj;
mod transport;
use crate::{
    sync::SpinLock,
    user::{UserAddressSpace, uaccess},
    vfs::{self, FdEntry, Inode, InodeKind, OpenFile, Stat, VfsError, tree::Attribute},
};
use alloc::{
    collections::{BTreeMap, VecDeque},
    format,
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use transport::{Dma, Request, Transport};
static GPU: SpinLock<Option<Arc<Gpu>>> = SpinLock::new(None);
/// Enabled by the `drmtrace` kernel command line; traces every DRM request.
static DRM_TRACE: AtomicBool = AtomicBool::new(false);
pub fn device() -> Option<Arc<Gpu>> {
    GPU.lock().clone()
}
pub fn u32_at(b: &[u8], o: usize) -> Result<u32, VfsError> {
    Ok(u32::from_le_bytes(
        b.get(o..o + 4)
            .ok_or(VfsError::InvalidArgument)?
            .try_into()
            .unwrap(),
    ))
}
pub fn u64_at(b: &[u8], o: usize) -> Result<u64, VfsError> {
    Ok(u64::from_le_bytes(
        b.get(o..o + 8)
            .ok_or(VfsError::InvalidArgument)?
            .try_into()
            .unwrap(),
    ))
}
pub fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
pub fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
pub fn command(kind: u32, context: u32, words: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0; 24 + words.len() * 4];
    put32(&mut bytes, 0, kind);
    put32(&mut bytes, 16, context);
    for (i, &word) in words.iter().enumerate() {
        put32(&mut bytes, 24 + i * 4, word);
    }
    bytes
}
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
pub fn fd_inode(fd: u32) -> Result<Arc<dyn Inode>, VfsError> {
    Ok(vfs::current_fd_table()
        .lock()
        .get(fd as usize)
        .ok_or(VfsError::BadFileDescriptor)?
        .file
        .inode
        .clone())
}
pub fn array32(address: u64, count: usize) -> Result<Vec<u32>, VfsError> {
    let bytes = uaccess::read_vec(
        address,
        count.checked_mul(4).ok_or(VfsError::InvalidArgument)?,
        1024 * 1024,
    )?;
    bytes.chunks_exact(4).map(|c| u32_at(c, 0)).collect()
}
pub fn array64(address: u64, count: usize) -> Result<Vec<u64>, VfsError> {
    let bytes = uaccess::read_vec(
        address,
        count.checked_mul(8).ok_or(VfsError::InvalidArgument)?,
        1024 * 1024,
    )?;
    bytes.chunks_exact(8).map(|c| u64_at(c, 0)).collect()
}
pub fn write32(address: u64, values: &[u32]) -> Result<(), VfsError> {
    for (i, value) in values.iter().enumerate() {
        uaccess::write_value(
            address.checked_add(i as u64 * 4).ok_or(VfsError::Fault)?,
            value,
        )?;
    }
    Ok(())
}
pub fn write64(address: u64, values: &[u64]) -> Result<(), VfsError> {
    for (i, value) in values.iter().enumerate() {
        uaccess::write_value(
            address.checked_add(i as u64 * 8).ok_or(VfsError::Fault)?,
            value,
        )?;
    }
    Ok(())
}
pub struct Ioctl {
    pub data: Vec<u8>,
    address: u64,
    user_size: usize,
    dir: u32,
}
impl Ioctl {
    fn new(cmd: u64, address: u64) -> Result<Self, VfsError> {
        if (cmd >> 8) & 255 != b'd' as u64 || cmd >> 32 != 0 {
            return Err(VfsError::NotATty);
        }
        let (size, dir) = abi::descriptor(cmd as u8).ok_or(VfsError::NotATty)?;
        let user_size = ((cmd >> 16) & 0x3fff) as usize;
        let user_dir = ((cmd >> 30) & 3) as u32;
        if user_size > 4096 {
            return Err(VfsError::InvalidArgument);
        }
        if (size == 0 && (user_dir != 0 || user_size != 0))
            || (size != 0 && (user_dir & dir == 0 || user_size == 0))
        {
            return Err(VfsError::NotATty);
        }
        let mut data = vec![0; size];
        let copy = user_size.min(size);
        if dir & 1 != 0 {
            uaccess::read(address, &mut data[..copy])?;
            if user_size > size {
                let extra = uaccess::read_vec(
                    address.checked_add(size as u64).ok_or(VfsError::Fault)?,
                    user_size - size,
                    4096,
                )?;
                if extra.iter().any(|&b| b != 0) {
                    return Err(VfsError::InvalidArgument);
                }
            }
        }
        if dir & 2 != 0 {
            uaccess::check(address, user_size, true)?;
        }
        Ok(Self {
            data,
            address,
            user_size,
            dir,
        })
    }
    pub fn r32(&self, o: usize) -> Result<u32, VfsError> {
        u32_at(&self.data, o)
    }
    pub fn r64(&self, o: usize) -> Result<u64, VfsError> {
        u64_at(&self.data, o)
    }
    pub fn w32(&mut self, o: usize, v: u32) {
        put32(&mut self.data, o, v)
    }
    pub fn w64(&mut self, o: usize, v: u64) {
        put64(&mut self.data, o, v)
    }
    fn finish(&self) -> Result<(), VfsError> {
        if self.dir & 2 != 0 {
            let copy = self.user_size.min(self.data.len());
            uaccess::write(self.address, &self.data[..copy])?;
            if self.user_size > copy {
                let zeroes = vec![0u8; self.user_size - copy];
                uaccess::write(
                    self.address
                        .checked_add(copy as u64)
                        .ok_or(VfsError::Fault)?,
                    &zeroes,
                )?;
            }
        }
        Ok(())
    }
}
pub struct Capset {
    pub id: u32,
    pub version: u32,
    pub size: u32,
}
struct Retirement {
    id: u32,
    pages: Vec<Dma>,
    host: Option<(u64, u64, u64)>,
    context: bool,
}
pub struct Gpu {
    transport: Transport,
    next_resource: AtomicU32,
    next_context: AtomicU32,
    next_session: AtomicU64,
    next_fence: AtomicU64,
    pub master: AtomicU64,
    pub capsets: SpinLock<Vec<Capset>>,
    pub outputs: SpinLock<Vec<kms::Output>>,
    host_ranges: SpinLock<Vec<(u64, u64)>>,
    retired: SpinLock<Vec<Retirement>>,
    sessions: SpinLock<Vec<Weak<Session>>>,
    pub displays: SpinLock<kms::DisplayState>,
    console: SpinLock<Option<Arc<ConsoleBacking>>>,
    console_version: AtomicU64,
    console_tick: AtomicU64,
    console_visible: AtomicBool,
    servicing: AtomicBool,
    live_resources: AtomicU32,
}
impl Gpu {
    fn call(
        &self,
        kind: u32,
        context: u32,
        words: &[u32],
        size: usize,
    ) -> Result<Vec<u8>, VfsError> {
        {
            let mut bytes = command(kind, context, words);
            put32(&mut bytes, 4, 1);
            put64(
                &mut bytes,
                8,
                self.next_fence.fetch_add(1, Ordering::Relaxed),
            );
            self.transport.call(&bytes, size)
        }
    }
    fn id(counter: &AtomicU32) -> Result<u32, VfsError> {
        let id = counter.fetch_add(1, Ordering::Relaxed);
        if id == 0 || id == u32::MAX {
            return Err(VfsError::NoSpace);
        }
        Ok(id)
    }
    fn backing(size: u64) -> Result<Vec<Dma>, VfsError> {
        if size > 256 * 1024 * 1024 {
            return Err(VfsError::NoSpace);
        }
        let mut pages = Vec::new();
        for _ in 0..size.div_ceil(4096) {
            pages.push(Dma::new(4096)?);
        }
        Ok(pages)
    }
    fn attach(&self, id: u32, pages: &[Dma]) -> Result<(), VfsError> {
        if pages.is_empty() {
            return Ok(());
        }
        let mut cmd = command(0x106, 0, &[id, pages.len() as u32]);
        for page in pages {
            cmd.extend_from_slice(&page.physical.to_le_bytes());
            cmd.extend_from_slice(&(page.length as u32).to_le_bytes());
            cmd.extend_from_slice(&0u32.to_le_bytes());
        }
        self.transport.call(&cmd, 24)?;
        Ok(())
    }
    pub fn create_2d(self: &Arc<Self>, width: u32, height: u32) -> Result<Arc<Resource>, VfsError> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(VfsError::InvalidArgument);
        }
        let stride = width.checked_mul(4).ok_or(VfsError::InvalidArgument)?;
        let size = stride as u64 * height as u64;
        let aligned = super_align(size)?;
        let pages = Self::backing(size)?;
        let id = Self::id(&self.next_resource)?;
        self.call(0x101, 0, &[id, 2, width, height], 24)?;
        if let Err(e) = self.attach(id, &pages) {
            let _ = self.call(0x102, 0, &[id, 0], 24);
            return Err(e);
        }
        self.live_resources.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(Resource {
            gpu: Arc::downgrade(self),
            id,
            size: aligned,
            pages,
            host: None,
            blob_mem: 0,
            width,
            height,
            stride,
            format: 0x34325258,
            last: SpinLock::new(None),
        }))
    }
    fn create_3d(
        self: &Arc<Self>,
        state: &mut SessionState,
        arg: &mut Ioctl,
    ) -> Result<Arc<Resource>, VfsError> {
        self.ensure_context(state)?;
        if arg.r32(40)? != 0 {
            return Err(VfsError::InvalidArgument);
        }
        let size = arg.r32(48)? as u64;
        let aligned = super_align(size)?;
        let pages = Self::backing(size)?;
        let id = Self::id(&self.next_resource)?;
        let mut words = vec![id];
        for o in (0..40).step_by(4) {
            words.push(arg.r32(o)?);
        }
        words.push(0);
        self.call(0x204, state.context, &words, 24)?;
        if let Err(e) = self.attach(id, &pages) {
            let _ = self.call(0x102, 0, &[id, 0], 24);
            return Err(e);
        }
        if let Err(e) = self.call(0x202, state.context, &[id, 0], 24) {
            let _ = self.call(0x102, 0, &[id, 0], 24);
            return Err(e);
        }
        self.live_resources.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(Resource {
            gpu: Arc::downgrade(self),
            id,
            size: aligned,
            pages,
            host: None,
            blob_mem: 0,
            width: arg.r32(12)?,
            height: arg.r32(16)?,
            stride: arg.r32(52)?,
            format: arg.r32(4)?,
            last: SpinLock::new(None),
        }))
    }
    fn host_allocate(&self, size: u64) -> Result<u64, VfsError> {
        let (_, limit) = self.transport.host_visible.ok_or(VfsError::NotSupported)?;
        let align = if self.transport.features & 32 != 0 {
            self.transport.config32(16).max(4096) as u64
        } else {
            4096
        };
        if !align.is_power_of_two() {
            return Err(VfsError::IoError);
        }
        let mut ranges = self.host_ranges.lock();
        ranges.sort_unstable();
        let mut offset = 0u64;
        for &(start, length) in ranges.iter() {
            if offset.checked_add(size).is_some_and(|end| end <= start) {
                break;
            }
            offset = (start + length)
                .checked_add(align - 1)
                .ok_or(VfsError::NoSpace)?
                & !(align - 1);
        }
        if offset.checked_add(size).is_none_or(|end| end > limit) {
            return Err(VfsError::NoSpace);
        }
        ranges.push((offset, size));
        Ok(offset)
    }
    fn create_blob(
        self: &Arc<Self>,
        state: &mut SessionState,
        arg: &mut Ioctl,
    ) -> Result<Arc<Resource>, VfsError> {
        if self.transport.features & 8 == 0 {
            return Err(VfsError::NotSupported);
        }
        let memory = arg.r32(0)?;
        let flags = arg.r32(4)?;
        let size = super_align(arg.r64(16)?)?;
        if !(1..=3).contains(&memory)
            || flags & !3 != 0
            || size == 0
            || size > 256 * 1024 * 1024
            || arg.r32(24)? != 0
            || arg.r32(48)? != 0
            || arg.r32(52)? != 0
        {
            return Err(VfsError::InvalidArgument);
        }
        if memory != 1 {
            self.ensure_context(state)?;
        } else if arg.r32(28)? != 0 || arg.r64(40)? != 0 {
            return Err(VfsError::InvalidArgument);
        }
        if arg.r32(28)? != 0 {
            let bytes = uaccess::read_vec(arg.r64(32)?, arg.r32(28)? as usize, 1024 * 1024)?;
            self.submit_bytes(state, bytes, 0, Vec::new())?.wait(self)?;
        }
        let pages = if memory == 2 {
            Vec::new()
        } else {
            Self::backing(size)?
        };
        let id = Self::id(&self.next_resource)?;
        let mut cmd = command(
            0x10c,
            if memory == 1 { 0 } else { state.context },
            &[id, memory, flags, pages.len() as u32],
        );
        cmd.extend_from_slice(&arg.r64(40)?.to_le_bytes());
        cmd.extend_from_slice(&size.to_le_bytes());
        for page in &pages {
            cmd.extend_from_slice(&page.physical.to_le_bytes());
            cmd.extend_from_slice(&4096u32.to_le_bytes());
            cmd.extend_from_slice(&0u32.to_le_bytes());
        }
        self.transport.call(&cmd, 24)?;
        let mut host = None;
        if memory != 1 && flags & 1 != 0 {
            let offset = match self.host_allocate(size) {
                Ok(o) => o,
                Err(e) => {
                    let _ = self.call(0x102, 0, &[id, 0], 24);
                    return Err(e);
                }
            };
            let result = self.call(
                0x208,
                state.context,
                &[id, 0, offset as u32, (offset >> 32) as u32],
                32,
            );
            match result {
                Ok(reply) => {
                    let cache = match u32_at(&reply, 24) {
                        Ok(value) => value & 15,
                        Err(e) => {
                            self.abort_blob(id, offset, None, true);
                            return Err(e);
                        }
                    };
                    if !(1..=3).contains(&cache) {
                        self.abort_blob(id, offset, None, true);
                        return Err(VfsError::IoError);
                    }
                    let Some((base, _)) = self.transport.host_visible else {
                        self.abort_blob(id, offset, None, true);
                        return Err(VfsError::NoDevice);
                    };
                    let physical = match base.checked_add(offset) {
                        Some(physical) => physical,
                        None => {
                            self.abort_blob(id, offset, None, true);
                            return Err(VfsError::InvalidArgument);
                        }
                    };
                    let kernel = match crate::mm::vmm::map_device_range(physical, size as usize) {
                        Ok(kernel) => kernel,
                        Err(_) => {
                            self.abort_blob(id, offset, None, true);
                            return Err(VfsError::NoMemory);
                        }
                    };
                    if cache == 1
                        && crate::mm::vmm::protect(
                            kernel,
                            size.div_ceil(4096) as usize,
                            false,
                            true,
                            false,
                        )
                        .is_err()
                    {
                        self.abort_blob(id, offset, Some((kernel, size)), true);
                        return Err(VfsError::IoError);
                    }
                    host = Some(HostMapping {
                        physical,
                        kernel,
                        cache,
                        offset,
                    });
                }
                Err(e) => {
                    self.abort_blob(id, offset, None, false);
                    return Err(e);
                }
            }
        }
        self.live_resources.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(Resource {
            gpu: Arc::downgrade(self),
            id,
            size,
            pages,
            host,
            blob_mem: memory,
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            last: SpinLock::new(None),
        }))
    }
    /// Roll back a blob whose host mapping was partially set up.  `kernel`
    /// is the kernel BAR alias when one was created and `host_mapped` says
    /// whether RESOURCE_MAP_BLOB reached the host and needs RESOURCE_UNMAP.
    fn abort_blob(&self, id: u32, offset: u64, kernel: Option<(u64, u64)>, host_mapped: bool) {
        if let Some((kernel, size)) = kernel {
            let _ = crate::mm::vmm::unmap(kernel, size.div_ceil(4096) as usize);
        }
        if host_mapped {
            let _ = self.call(0x209, 0, &[id, 0], 24);
        }
        self.host_ranges.lock().retain(|&(o, _)| o != offset);
        let _ = self.call(0x102, 0, &[id, 0], 24);
    }
    fn ensure_context(&self, state: &mut SessionState) -> Result<(), VfsError> {
        if self.transport.features & 1 == 0 {
            return Err(VfsError::NotSupported);
        }
        if state.created {
            return Ok(());
        }
        if state.context == 0 {
            state.context = Self::id(&self.next_context)?;
        }
        // `context_init` carries the capset id only (VIRTIO_GPU_CONTEXT_INIT_
        // CAPSET_ID_MASK == 0xff).  virglrenderer rejects every other bit, so
        // num_rings must not be folded in here.  A Venus client (Mesa) binds
        // ring N on the host by creating a VkQueue with
        // VkDeviceQueueTimelineInfoMESA.ringIdx == N; until that happens the
        // host has no sync queue for N and rejects a fence submitted on it.
        let mut cmd = command(0x200, state.context, &[0, state.capset]);
        cmd.resize(96, 0);
        self.transport.call(&cmd, 24)?;
        state.created = true;
        Ok(())
    }
    fn submit_bytes(
        &self,
        state: &mut SessionState,
        bytes: Vec<u8>,
        ring: u32,
        retain: Vec<Arc<dyn Inode>>,
    ) -> Result<Arc<syncobj::Fence>, VfsError> {
        self.ensure_context(state)?;
        if ring >= state.rings || bytes.len() > 1024 * 1024 || bytes.len() % 4 != 0 {
            return Err(VfsError::InvalidArgument);
        }
        let mut cmd = command(0x207, state.context, &[bytes.len() as u32, 0]);
        cmd.extend_from_slice(&bytes);
        // Per-context fences carry VIRTIO_GPU_FLAG_INFO_RING_IDX even for ring
        // 0, matching Linux virtio_gpu_fence_emit(); legacy contexts use the
        // ctx0 fence path instead.  The header field is a u8 at offset 20
        // (ctx_id at 16, then ring_idx + 3 padding bytes) and the validated
        // ring index is < 64, so the low byte is the complete value.
        put32(&mut cmd, 4, 1 | if state.initialized { 2 } else { 0 });
        put64(&mut cmd, 8, self.next_fence.fetch_add(1, Ordering::Relaxed));
        cmd[20] = ring as u8;
        let request = self.transport.submit(false, &cmd, 24, retain)?;
        Ok(syncobj::Fence::request(request))
    }
    fn install_console(self: &Arc<Self>) -> Result<(), VfsError> {
        let output = self
            .outputs
            .lock()
            .first()
            .cloned()
            .ok_or(VfsError::NoDevice)?;
        let width = output.width.min(2048);
        let height = output.height.min(2048);
        let resource = self.create_2d(width, height)?;
        let region =
            crate::mm::vmm::reserve(resource.pages.len()).map_err(|_| VfsError::NoMemory)?;
        for (index, page) in resource.pages.iter().enumerate() {
            if crate::mm::vmm::map(region, index, page.physical, false).is_err() {
                let _ = crate::mm::vmm::unmap(region.base(), index);
                return Err(VfsError::NoMemory);
            }
        }
        let backing = Arc::new(ConsoleBacking {
            resource: resource.clone(),
            base: region.base(),
            pages: resource.pages.len(),
        });
        // SAFETY: ConsoleBacking pins all mapped pages and owns the kernel alias.
        unsafe {
            crate::tty::install_gpu_console(region.base() as usize, width, height, backing.clone())
                .map_err(|_| VfsError::NotSupported)?;
        }
        let transfer = self.call(0x105, 0, &[0, 0, width, height, 0, 0, resource.id, 0], 24)?;
        let flush = self.call(0x104, 0, &[0, 0, width, height, resource.id, 0], 24)?;
        let scanout = self.call(0x103, 0, &[0, 0, width, height, 0, resource.id], 24)?;
        crate::verbose::log(format_args!(
            "GPU console: transfer={:#x} flush={:#x} set_scanout={:#x} resource={} {}x{}",
            u32_at(&transfer, 0).unwrap_or(0),
            u32_at(&flush, 0).unwrap_or(0),
            u32_at(&scanout, 0).unwrap_or(0),
            resource.id,
            width,
            height
        ));
        kms::install_console(self, &resource);
        *self.console.lock() = Some(backing);
        self.console_visible.store(true, Ordering::Release);
        Ok(())
    }
    fn console_service(&self) {
        if self.transport.failed.load(Ordering::Acquire) {
            return;
        }
        if self.master.load(Ordering::Acquire) != 0 {
            self.console_visible.store(false, Ordering::Release);
            return;
        }
        let now = crate::time::uptime_micros();
        if now.saturating_sub(self.console_tick.load(Ordering::Relaxed)) < 16000 {
            return;
        }
        self.console_tick.store(now, Ordering::Relaxed);
        let version = crate::tty::GRAPHICS_DIRTY.load(Ordering::Acquire);
        let visible = self.console_visible.load(Ordering::Acquire);
        if visible && version == self.console_version.load(Ordering::Relaxed) {
            return;
        }
        let Some(backing) = self.console.lock().clone() else {
            return;
        };
        let r = &backing.resource;
        if self
            .call(0x105, 0, &[0, 0, r.width, r.height, 0, 0, r.id, 0], 24)
            .is_ok()
        {
            let _ = self.call(0x104, 0, &[0, 0, r.width, r.height, r.id, 0], 24);
            if !visible {
                let _ = self.call(0x103, 0, &[0, 0, r.width, r.height, 0, r.id], 24);
            }
            if !visible {
                kms::restore_console(self, &backing.resource);
            }
            self.console_visible.store(true, Ordering::Release);
            self.console_version.store(version, Ordering::Relaxed);
        }
    }
    pub fn service(&self) {
        // The periodic service task and a self-test/kernel caller can race.
        // Only one of them may take the retired list or drive the display
        // state machine at a time; the loser simply retries on its next tick.
        if self
            .servicing
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        self.transport.poll();
        let sessions: Vec<_> = {
            let mut list = self.sessions.lock();
            list.retain(|w| w.strong_count() != 0);
            list.iter().filter_map(|w| w.upgrade()).collect()
        };
        for session in sessions {
            session.complete_events();
        }
        kms::service(self);
        self.console_service();
        // Completing an earlier request can drop its retained BufferFile and
        // therefore queue another Resource in `retired`.  Drain until the list
        // stays empty instead of dropping the second generation on the floor.
        for _ in 0..64 {
            let retired = core::mem::take(&mut *self.retired.lock());
            if retired.is_empty() {
                break;
            }
            for item in retired {
                if item.context {
                    let _ = self.call(0x201, item.id, &[], 24);
                } else {
                    if let Some((offset, size, kernel)) = item.host {
                        let _ = crate::mm::vmm::unmap(kernel, size.div_ceil(4096) as usize);
                        let _ = self.call(0x209, 0, &[item.id, 0], 24);
                        self.host_ranges
                            .lock()
                            .retain(|&(o, l)| o != offset || l != size);
                    }
                    let _ = self.call(0x102, 0, &[item.id, 0], 24);
                }
                drop(item.pages);
            }
        }
        self.servicing.store(false, Ordering::Release);
    }
    /// Number of `Resource` objects currently owned by userspace or the
    /// kernel console.  A self-test records the baseline before opening its
    /// sessions and requires the count to return to it after close.
    pub fn live_resources(&self) -> u32 {
        self.live_resources.load(Ordering::Acquire)
    }
    /// Drive the device until every queued resource/context retirement has
    /// been processed and the live resource count is back at `baseline`.
    /// Returns false if the deadline elapsed first.
    pub fn wait_idle(&self, baseline: u32, timeout_micros: u64) -> bool {
        let deadline = crate::time::uptime_micros().saturating_add(timeout_micros);
        loop {
            self.service();
            if self.retired.lock().is_empty()
                && self.host_ranges.lock().is_empty()
                && self.live_resources() == baseline
            {
                return true;
            }
            if crate::time::uptime_micros() >= deadline {
                return false;
            }
            crate::sched::sleep_micros(1000);
        }
    }
}
fn super_align(size: u64) -> Result<u64, VfsError> {
    crate::user::address_space::align_up(size).ok_or(VfsError::InvalidArgument)
}
struct HostMapping {
    physical: u64,
    kernel: u64,
    cache: u32,
    offset: u64,
}
pub struct Resource {
    gpu: Weak<Gpu>,
    pub id: u32,
    pub size: u64,
    pages: Vec<Dma>,
    host: Option<HostMapping>,
    pub blob_mem: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub last: SpinLock<Option<Arc<syncobj::Fence>>>,
}
impl Drop for Resource {
    fn drop(&mut self) {
        if let Some(gpu) = self.gpu.upgrade() {
            gpu.live_resources.fetch_sub(1, Ordering::Relaxed);
            gpu.retired.lock().push(Retirement {
                id: self.id,
                pages: core::mem::take(&mut self.pages),
                host: self.host.as_ref().map(|h| (h.offset, self.size, h.kernel)),
                context: false,
            });
        }
    }
}
pub struct BufferFile {
    pub resource: Arc<Resource>,
}
impl Inode for BufferFile {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(
            0x8000_0000 + self.resource.id as u64,
            InodeKind::Regular,
            0o600,
            self.resource.size,
        ))
    }
    fn mmap(
        &self,
        space: &mut UserAddressSpace,
        address: u64,
        length: u64,
        offset: u64,
        prot: u32,
    ) -> Result<(), VfsError> {
        let resource = &self.resource;
        if offset & 4095 != 0
            || length & 4095 != 0
            || offset.checked_add(length).is_none_or(|e| e > resource.size)
            || prot & 4 != 0
        {
            return Err(VfsError::InvalidArgument);
        }
        let owner: Arc<dyn Inode> = Arc::new(BufferFile {
            resource: resource.clone(),
        });
        for done in (0..length).step_by(4096) {
            let result = if let Some(host) = &resource.host {
                space.map_external_at(
                    address + done,
                    host.physical + offset + done,
                    host.kernel + offset + done,
                    prot,
                    owner.clone(),
                    host.cache != 1,
                )
            } else {
                let page = resource
                    .pages
                    .get(((offset + done) / 4096) as usize)
                    .ok_or(VfsError::InvalidArgument)?;
                space.map_external(address + done, page.physical, prot, owner.clone())
            };
            if result.is_err() {
                let _ = space.unmap(address, done);
                return Err(VfsError::NoMemory);
            }
        }
        Ok(())
    }
    fn poll(&self) -> u32 {
        self.resource
            .last
            .lock()
            .as_ref()
            .map_or(1 | 4, |f| if f.status() != 0 { 1 | 4 } else { 0 })
    }
}
struct ConsoleBacking {
    resource: Arc<Resource>,
    base: u64,
    pages: usize,
}
impl Inode for ConsoleBacking {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(
            0,
            InodeKind::Regular,
            0o600,
            self.resource.size,
        ))
    }
}
impl Drop for ConsoleBacking {
    fn drop(&mut self) {
        let _ = crate::mm::vmm::unmap(self.base, self.pages);
    }
}

struct DeviceNode {
    gpu: Arc<Gpu>,
    render: bool,
}
impl Inode for DeviceNode {
    fn deactivate(&self) -> Result<(), VfsError> {
        self.gpu.transport.fail();
        Ok(())
    }
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut s = Stat::simple(
            0x800000,
            InodeKind::CharDevice,
            if self.render { 0o666 } else { 0o660 },
            0,
        );
        s.rdev = (226 << 8) | if self.render { 128 } else { 0 };
        Ok(s)
    }
    fn open(&self, _flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        let id = self.gpu.next_session.fetch_add(1, Ordering::Relaxed);
        if !self.render {
            let _ = self
                .gpu
                .master
                .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire);
        }
        let session = Arc::new(Session {
            gpu: self.gpu.clone(),
            id,
            render: self.render,
            busy: AtomicBool::new(false),
            state: SpinLock::new(Some(SessionState::new())),
            events: Arc::new(SpinLock::new(VecDeque::new())),
            pending_events: SpinLock::new(Vec::new()),
            map_handles: SpinLock::new(BTreeMap::new()),
        });
        self.gpu.sessions.lock().push(Arc::downgrade(&session));
        Ok(Some(session))
    }
}
pub struct Session {
    pub gpu: Arc<Gpu>,
    pub id: u64,
    pub render: bool,
    busy: AtomicBool,
    state: SpinLock<Option<SessionState>>,
    events: Arc<SpinLock<VecDeque<Vec<u8>>>>,
    pending_events: SpinLock<Vec<Arc<syncobj::Fence>>>,
    map_handles: SpinLock<BTreeMap<u32, Arc<Resource>>>,
}
pub struct SessionState {
    pub handles: BTreeMap<u32, Arc<Resource>>,
    pub syncs: BTreeMap<u32, Arc<syncobj::SyncObject>>,
    pub next_handle: u32,
    pub next_sync: u32,
    pub context: u32,
    pub capset: u32,
    pub rings: u32,
    pub poll_rings: u64,
    pub initialized: bool,
    pub created: bool,
    pub atomic: bool,
    pub universal: bool,
    pub blobs: BTreeMap<u32, Vec<u8>>,
    pub next_blob: u32,
    pub framebuffers: BTreeMap<u32, kms::Framebuffer>,
}
impl SessionState {
    fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            syncs: BTreeMap::new(),
            next_handle: 1,
            next_sync: 1,
            context: 0,
            capset: 0,
            rings: 1,
            poll_rings: 0,
            initialized: false,
            created: false,
            atomic: false,
            universal: false,
            blobs: BTreeMap::new(),
            next_blob: 1000,
            framebuffers: BTreeMap::new(),
        }
    }
    pub fn add(&mut self, resource: Arc<Resource>) -> Result<u32, VfsError> {
        if self.handles.len() >= 4096 {
            return Err(VfsError::NoSpace);
        }
        if let Some((&h, _)) = self.handles.iter().find(|(_, r)| Arc::ptr_eq(r, &resource)) {
            return Ok(h);
        }
        let id = self.next_handle;
        self.next_handle = self.next_handle.checked_add(1).ok_or(VfsError::NoSpace)?;
        self.handles.insert(id, resource);
        Ok(id)
    }
    pub fn resource(&self, handle: u32) -> Result<Arc<Resource>, VfsError> {
        self.handles.get(&handle).cloned().ok_or(VfsError::NotFound)
    }
}
impl Session {
    pub fn unlocked<R>(&self, state: &mut SessionState, work: impl FnOnce() -> R) -> R {
        let local = core::mem::replace(state, SessionState::new());
        *self.state.lock() = Some(local);
        self.busy.store(false, Ordering::Release);
        let result = work();
        while self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            crate::sched::sleep_micros(100);
        }
        *state = self
            .state
            .lock()
            .take()
            .expect("DRM wait reacquired session");
        result
    }
    fn dispatch(&self, state: &mut SessionState, nr: u8, arg: &mut Ioctl) -> Result<(), VfsError> {
        match nr {
            0x00 => {
                arg.w32(0, 0);
                arg.w32(4, 1);
                arg.w32(8, 0);
                for (length, pointer, text) in [
                    (16, 24, "virtio_gpu"),
                    (32, 40, "20261024"),
                    (48, 56, "SpaceKernel VirtIO GPU"),
                ] {
                    let capacity = arg.r64(length)? as usize;
                    let dst = arg.r64(pointer)?;
                    let n = capacity.min(text.len());
                    // Linux copies a field only when the caller supplied a
                    // buffer; a NULL pointer just reports the required length.
                    if n != 0 && dst != 0 {
                        uaccess::write(dst, &text.as_bytes()[..n])?;
                    }
                    arg.w64(length, text.len() as u64);
                }
                Ok(())
            }
            0x01 => {
                arg.w64(0, 0);
                Ok(())
            }
            0x02 => Err(VfsError::NotSupported),
            0x07 => {
                arg.w32(0, 1);
                arg.w32(4, 4);
                arg.w32(8, 0);
                arg.w32(12, 1);
                Ok(())
            }
            0x09 => {
                if arg.r32(4)? != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                state
                    .handles
                    .remove(&arg.r32(0)?)
                    .ok_or(VfsError::NotFound)?;
                Ok(())
            }
            0x0c => {
                let value = match arg.r64(0)? {
                    1 => 1,
                    2 => 0,
                    3 => 24,
                    4 => 1,
                    5 => 3,
                    6 => 1,
                    7 => 0,
                    8 | 9 => 64,
                    0x10 => 0,
                    0x11 => 0,
                    0x12 => 1,
                    0x13 => 1,
                    0x14 => 1,
                    0x15 => 0,
                    _ => return Err(VfsError::InvalidArgument),
                };
                arg.w64(8, value);
                Ok(())
            }
            0x0d => {
                let cap = arg.r64(0)?;
                let value = arg.r64(8)?;
                if value > 1 {
                    return Err(VfsError::InvalidArgument);
                }
                match cap {
                    2 => state.universal = value != 0,
                    3 => {
                        state.atomic = value != 0;
                        state.universal |= value != 0;
                    }
                    1 | 4 | 5 => {}
                    _ => return Err(VfsError::NotSupported),
                }
                Ok(())
            }
            0x1e => {
                if self.render {
                    return Err(VfsError::PermissionDenied);
                }
                match self.gpu.master.compare_exchange(
                    0,
                    self.id,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => Ok(()),
                    Err(id) if id == self.id => Ok(()),
                    _ => Err(VfsError::Busy),
                }
            }
            0x1f => {
                if self.render
                    || self
                        .gpu
                        .master
                        .compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                {
                    return Err(VfsError::PermissionDenied);
                }
                Ok(())
            }
            0x2d => {
                let resource = state.resource(arg.r32(0)?)?;
                let flags = arg.r32(4)?;
                if flags & !(vfs::abi::O_CLOEXEC | vfs::abi::O_RDWR) != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                let fd = install_fd(Arc::new(BufferFile { resource }), flags)?;
                arg.w32(8, fd);
                Ok(())
            }
            0x2e => {
                let inode: Arc<dyn core::any::Any + Send + Sync> = fd_inode(arg.r32(8)?)?;
                let file = inode
                    .downcast::<BufferFile>()
                    .map_err(|_| VfsError::InvalidArgument)?;
                let owner = file.resource.gpu.upgrade().ok_or(VfsError::NoDevice)?;
                if !Arc::ptr_eq(&owner, &self.gpu) {
                    return Err(VfsError::NotSupported);
                }
                let handle = state.add(file.resource.clone())?;
                arg.w32(0, handle);
                Ok(())
            }
            0x41 => {
                let resource = state.resource(arg.r32(8)?)?;
                if arg.r32(12)? != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                arg.w64(0, (resource.id as u64) << 32);
                Ok(())
            }
            0x43 => {
                let value = match arg.r64(0)? {
                    1 => u64::from(self.gpu.transport.features & 1 != 0),
                    2 => 1,
                    3 => u64::from(self.gpu.transport.features & 8 != 0),
                    4 => u64::from(self.gpu.transport.host_visible.is_some()),
                    5 => 0,
                    6 => u64::from(self.gpu.transport.features & 16 != 0),
                    7 => self
                        .gpu
                        .capsets
                        .lock()
                        .iter()
                        .fold(0, |mask, c| mask | if c.id < 64 { 1u64 << c.id } else { 0 }),
                    8 => 0,
                    9 => {
                        if self.gpu.transport.features & 32 != 0 {
                            self.gpu.transport.config32(16) as u64
                        } else {
                            4096
                        }
                    }
                    _ => return Err(VfsError::InvalidArgument),
                };
                uaccess::write_value(arg.r64(8)?, &value)
            }
            0x44 => {
                let resource = self.gpu.create_3d(state, arg)?;
                let id = resource.id;
                let handle = state.add(resource)?;
                arg.w32(40, handle);
                arg.w32(44, id);
                Ok(())
            }
            0x45 => {
                let resource = state.resource(arg.r32(0)?)?;
                arg.w32(4, resource.id);
                arg.w32(8, resource.size.min(u32::MAX as u64) as u32);
                arg.w32(12, resource.blob_mem);
                Ok(())
            }
            0x46 | 0x47 => {
                let resource = state.resource(arg.r32(0)?)?;
                self.gpu.ensure_context(state)?;
                let mut words = Vec::new();
                for o in (4..28).step_by(4) {
                    words.push(arg.r32(o)?);
                }
                words.extend([
                    arg.r32(32)?,
                    0,
                    resource.id,
                    arg.r32(28)?,
                    arg.r32(36)?,
                    arg.r32(40)?,
                ]);
                self.gpu.call(
                    if nr == 0x46 { 0x206 } else { 0x205 },
                    state.context,
                    &words,
                    24,
                )?;
                Ok(())
            }
            0x48 => {
                let resource = state.resource(arg.r32(0)?)?;
                let flags = arg.r32(4)?;
                if flags & !1 != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                let fence = resource.last.lock().clone();
                if let Some(fence) = fence {
                    if flags & 1 != 0 && fence.status() == 0 {
                        return Err(VfsError::Busy);
                    }
                    self.unlocked(state, || fence.wait(&self.gpu))?;
                }
                Ok(())
            }
            0x49 => {
                let (id, version, size) = (arg.r32(0)?, arg.r32(4)?, arg.r32(16)? as usize);
                let maximum = self
                    .gpu
                    .capsets
                    .lock()
                    .iter()
                    .find(|c| c.id == id && version <= c.version)
                    .map(|c| c.size)
                    .ok_or(VfsError::InvalidArgument)? as usize;
                if arg.r32(20)? != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                let response = self.gpu.call(0x109, 0, &[id, version], 24 + maximum)?;
                uaccess::write(arg.r64(8)?, &response[24..response.len().min(24 + size)])
            }
            0x4a => {
                let resource = self.gpu.create_blob(state, arg)?;
                let id = resource.id;
                let handle = state.add(resource)?;
                arg.w32(8, handle);
                arg.w32(12, id);
                Ok(())
            }
            0x4b => {
                if self.gpu.transport.features & 16 == 0
                    || state.initialized
                    || state.created
                    || arg.r32(4)? != 0
                {
                    return Err(VfsError::InvalidArgument);
                }
                let count = arg.r32(0)? as usize;
                if count > 4 {
                    return Err(VfsError::InvalidArgument);
                }
                let params = uaccess::read_vec(arg.r64(8)?, count * 16, 64)?;
                let mut seen = 0;
                let mut capset = 0;
                let mut rings = 1;
                let mut mask = 0;
                for item in params.chunks_exact(16) {
                    let key = u64_at(item, 0)?;
                    let value = u64_at(item, 8)?;
                    if key == 0 || key > 3 || seen & (1 << key) != 0 {
                        return Err(VfsError::InvalidArgument);
                    }
                    seen |= 1 << key;
                    match key {
                        1 => {
                            if value > 255
                                || !self.gpu.capsets.lock().iter().any(|c| c.id == value as u32)
                            {
                                return Err(VfsError::InvalidArgument);
                            }
                            capset = value as u32;
                        }
                        2 => {
                            if value == 0 || value > 64 {
                                return Err(VfsError::InvalidArgument);
                            }
                            rings = value as u32;
                        }
                        3 => mask = value,
                        _ => {}
                    }
                }
                if rings < 64 && mask >> rings != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                state.capset = capset;
                state.rings = rings;
                state.poll_rings = mask;
                state.initialized = true;
                Ok(())
            }
            0x42 => self.exec(state, arg),
            0xbf..=0xc5 | 0xca..=0xcd => syncobj::ioctl(self, state, nr, arg),
            0xa0..=0xbe | 0xce => {
                if self.render {
                    return Err(VfsError::PermissionDenied);
                }
                kms::ioctl(self, state, nr, arg)
            }
            _ => Err(VfsError::NotATty),
        }
    }
    fn exec(&self, state: &mut SessionState, arg: &mut Ioctl) -> Result<(), VfsError> {
        let flags = arg.r32(0)?;
        if flags & !7 != 0 {
            return Err(VfsError::InvalidArgument);
        }
        let ring = if flags & 4 != 0 { arg.r32(32)? } else { 0 };
        if ring >= state.rings || ring >= 64 {
            return Err(VfsError::InvalidArgument);
        }
        let handles = array32(arg.r64(16)?, arg.r32(24)? as usize)?;
        let mut resources = Vec::new();
        for h in handles {
            resources.push(state.resource(h)?);
        }
        if flags & 1 != 0 {
            let fence = syncobj::fence_fd(arg.r32(28)?)?;
            self.unlocked(state, || fence.wait(&self.gpu))?;
        }
        let ins = syncobj::exec_syncs(
            state,
            arg.r64(48)?,
            arg.r32(40)? as usize,
            arg.r32(36)? as usize,
        )?;
        let outs = syncobj::exec_syncs(
            state,
            arg.r64(56)?,
            arg.r32(44)? as usize,
            arg.r32(36)? as usize,
        )?;
        for (sync, point, reset) in &ins {
            if let Some(f) = sync.fence(*point) {
                self.unlocked(state, || f.wait(&self.gpu))?;
            } else {
                return Err(VfsError::InvalidArgument);
            }
            if *reset {
                sync.reset();
            }
        }
        let bytes = uaccess::read_vec(arg.r64(8)?, arg.r32(4)? as usize, 1024 * 1024)?;
        self.gpu.ensure_context(state)?;
        for resource in &resources {
            self.gpu.call(0x202, state.context, &[resource.id, 0], 24)?;
        }
        let retain: Vec<Arc<dyn Inode>> = resources
            .iter()
            .map(|r| {
                Arc::new(BufferFile {
                    resource: r.clone(),
                }) as Arc<dyn Inode>
            })
            .collect();
        let notify = state.poll_rings & (1u64 << ring) != 0;
        if notify && self.pending_events.lock().len() + self.events.lock().len() >= 256 {
            return Err(VfsError::NoSpace);
        }
        let reserved = if flags & 2 != 0 {
            let placeholder = syncobj::Fence::pending();
            let fd = install_fd(
                Arc::new(syncobj::SyncFile {
                    fence: placeholder.clone(),
                }),
                vfs::abi::O_CLOEXEC,
            )?;
            Some((fd, placeholder))
        } else {
            None
        };
        let submitted = self.gpu.submit_bytes(state, bytes, ring, retain);
        let fence = match submitted {
            Ok(f) => f,
            Err(e) => {
                if let Some((fd, placeholder)) = reserved {
                    placeholder.complete(-5);
                    let _ = vfs::current_fd_table().lock().close(fd as usize);
                }
                return Err(e);
            }
        };
        if let Some((fd, placeholder)) = &reserved {
            placeholder.bind(&fence);
            arg.w32(28, *fd);
        }

        if notify {
            self.pending_events.lock().push(fence.clone());
        }
        for resource in resources {
            *resource.last.lock() = Some(fence.clone());
        }
        for (sync, point, _) in outs {
            sync.insert(point, fence.clone())?;
        }
        Ok(())
    }
    fn complete_events(&self) {
        let ready = {
            let mut pending = self.pending_events.lock();
            let count = pending.iter().filter(|f| f.status() != 0).count();
            pending.retain(|f| f.status() == 0);
            count
        };
        for _ in 0..ready {
            let mut event = vec![0; 8];
            put32(&mut event, 0, 0x90000000);
            put32(&mut event, 4, 8);
            let _ = self.event(event);
        }
    }
    pub fn event(&self, bytes: Vec<u8>) -> Result<(), VfsError> {
        let mut queue = self.events.lock();
        if queue.len() >= 256 {
            return Err(VfsError::NoSpace);
        }
        queue.push_back(bytes);
        drop(queue);
        crate::ipc::wake_object(Arc::as_ptr(&self.events));
        Ok(())
    }
}
impl Inode for Session {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut s = Stat::simple(self.id + 0x810000, InodeKind::CharDevice, 0o660, 0);
        s.rdev = (226 << 8) | if self.render { 128 } else { 0 };
        Ok(s)
    }
    fn ioctl(&self, cmd: u64, address: u64) -> Result<u64, VfsError> {
        if self.gpu.transport.failed.load(Ordering::Acquire)
            && !matches!(cmd as u8, 9 | 0xb4 | 0xc0 | 0xbe)
        {
            return Err(VfsError::NoDevice);
        }
        let mut arg = Ioctl::new(cmd, address)?;
        let trace = DRM_TRACE.load(Ordering::Relaxed);
        if trace {
            crate::kinfo!("drm: enter nr={:#04x} arg={:#x}", cmd as u8, address);
        }
        while self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if crate::sched::current().is_some() {
                crate::sched::sleep_micros(100);
            } else {
                core::hint::spin_loop();
            }
        }
        let mut state = self.state.lock().take().expect("DRM session serialized");
        let result = self.dispatch(&mut state, cmd as u8, &mut arg);
        let handles = state.handles.clone();
        *self.map_handles.lock() = handles;
        *self.state.lock() = Some(state);
        self.busy.store(false, Ordering::Release);
        if trace {
            crate::kinfo!("drm: exit nr={:#04x} -> {:?}", cmd as u8, result);
        }
        result?;
        arg.finish()?;
        Ok(0)
    }
    fn mmap(
        &self,
        space: &mut UserAddressSpace,
        address: u64,
        length: u64,
        offset: u64,
        prot: u32,
    ) -> Result<(), VfsError> {
        let resource = self
            .map_handles
            .lock()
            .values()
            .find(|r| offset >= ((r.id as u64) << 32) && offset < ((r.id as u64) << 32) + r.size)
            .cloned()
            .ok_or(VfsError::PermissionDenied)?;
        let file = BufferFile {
            resource: resource.clone(),
        };
        file.mmap(
            space,
            address,
            length,
            offset - ((resource.id as u64) << 32),
            prot,
        )
    }
    fn read_at(&self, _: u64, dst: &mut [u8]) -> Result<usize, VfsError> {
        let mut queue = self.events.lock();
        let Some(first) = queue.front() else {
            return Err(VfsError::WouldBlock);
        };
        if dst.len() < first.len() {
            return Err(VfsError::InvalidArgument);
        }
        let mut written = 0;
        while queue
            .front()
            .is_some_and(|e| written + e.len() <= dst.len())
        {
            let event = queue.pop_front().unwrap();
            dst[written..written + event.len()].copy_from_slice(&event);
            written += event.len();
        }
        Ok(written)
    }
    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(Arc::as_ptr(&self.events));
        // Fence completion only becomes an event when poll collects it.
        if !self.pending_events.lock().is_empty() {
            sources.tick();
        }
    }
    fn poll(&self) -> u32 {
        self.complete_events();
        if self.gpu.transport.failed.load(Ordering::Acquire) {
            8 | 16
        } else if self.events.lock().is_empty() {
            0
        } else {
            1
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        kms::close_session(&self.gpu, self.id);
        self.gpu.console_visible.store(false, Ordering::Release);
        let _ = self
            .gpu
            .master
            .compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Acquire);
        if let Some(state) = self.state.lock().take() {
            if state.created {
                self.gpu.retired.lock().push(Retirement {
                    id: state.context,
                    pages: Vec::new(),
                    host: None,
                    context: true,
                });
            }
        }
    }
}
pub fn init() {
    DRM_TRACE.store(
        crate::boot::cmdline()
            .split_ascii_whitespace()
            .any(|word| word == "drmtrace"),
        Ordering::Relaxed,
    );
    let Some(pci) = crate::pci::devices().iter().find(|d| {
        d.vendor_id == 0x1af4 && matches!(d.device_id, 0x1050 | 0x1040..=0x1040) && d.class == 3
    }) else {
        return;
    };
    let transport = match Transport::new(pci) {
        Ok(t) => t,
        Err(e) => {
            crate::kwarn!("VirtIO GPU transport unavailable: {:?}", e);
            return;
        }
    };
    let gpu = Arc::new(Gpu {
        transport,
        next_resource: AtomicU32::new(1),
        next_context: AtomicU32::new(1),
        next_session: AtomicU64::new(1),
        next_fence: AtomicU64::new(1),
        master: AtomicU64::new(0),
        capsets: SpinLock::new(Vec::new()),
        outputs: SpinLock::new(Vec::new()),
        host_ranges: SpinLock::new(Vec::new()),
        retired: SpinLock::new(Vec::with_capacity(8192)),
        sessions: SpinLock::new(Vec::new()),
        displays: SpinLock::new(kms::DisplayState::new()),
        console: SpinLock::new(None),
        console_version: AtomicU64::new(0),
        console_tick: AtomicU64::new(0),
        console_visible: AtomicBool::new(false),
        servicing: AtomicBool::new(false),
        live_resources: AtomicU32::new(0),
    });
    *GPU.lock() = Some(gpu.clone());
    let result = (|| -> Result<(), VfsError> {
        let count = gpu.transport.config32(12);
        if count > 64 {
            return Err(VfsError::IoError);
        }
        for index in 0..count {
            let reply = gpu.call(0x108, 0, &[index, 0], 40)?;
            let id = u32_at(&reply, 24)?;
            let version = u32_at(&reply, 28)?;
            let size = u32_at(&reply, 32)?;
            if id == 0 || id >= 64 || size > 1024 * 1024 {
                return Err(VfsError::IoError);
            }
            gpu.capsets.lock().push(Capset { id, version, size });
        }
        kms::discover(&gpu)?;
        // The firmware framebuffer usually belongs to the primary display
        // device.  Taking the console over to the virtio-gpu scanout would
        // leave that visible display frozen, so only do it when there is no
        // firmware framebuffer or the user explicitly requests it.
        let gpu_console_requested = crate::boot::cmdline()
            .split_ascii_whitespace()
            .any(|word| word == "fbcon=gpu");
        // The DRM self-test expects the console scanout to participate in
        // request accounting and IRQ delivery, so keep the historical
        // behaviour for self-test builds.
        if cfg!(feature = "boot-self-test")
            || crate::boot::framebuffer().is_none()
            || gpu_console_requested
        {
            gpu.install_console()?;
        }
        for (name, node, render) in [
            ("card0", "dri/card0", false),
            ("renderD128", "dri/renderD128", true),
        ] {
            let backend: Arc<dyn Inode> = Arc::new(DeviceNode {
                gpu: gpu.clone(),
                render,
            });
            crate::device::register(
                name,
                "drm",
                Some(node),
                Some(backend),
                vec![
                    ("vendor", Attribute::text("0x1af4\n")),
                    (
                        "device",
                        Attribute::text(format!("0x{:04x}\n", pci.device_id)),
                    ),
                ],
            )?;
        }
        for record in crate::device::records()
            .into_iter()
            .filter(|d| d.subsystem == "drm")
        {
            let _ = record.directory.insert(
                "device",
                vfs::tree::Symlink::new(&format!("../{}", pci.address)),
            );
        }
        for index in 0..gpu.outputs.lock().len() {
            let weak = Arc::downgrade(&gpu);
            let status = Attribute::new(0o444, move || {
                let gpu = weak.upgrade().ok_or(VfsError::NoDevice)?;
                let connected = gpu
                    .outputs
                    .lock()
                    .get(index)
                    .ok_or(VfsError::NoDevice)?
                    .connected;
                Ok(if connected {
                    b"connected\n".to_vec()
                } else {
                    b"disconnected\n".to_vec()
                })
            });
            let weak = Arc::downgrade(&gpu);
            let modes = Attribute::new(0o444, move || {
                let gpu = weak.upgrade().ok_or(VfsError::NoDevice)?;
                let outputs = gpu.outputs.lock();
                let o = outputs.get(index).ok_or(VfsError::NoDevice)?;
                Ok(format!("{}x{}\n", o.width, o.height).into_bytes())
            });
            let weak = Arc::downgrade(&gpu);
            let edid = Attribute::new(0o444, move || {
                let gpu = weak.upgrade().ok_or(VfsError::NoDevice)?;
                let value = gpu
                    .outputs
                    .lock()
                    .get(index)
                    .ok_or(VfsError::NoDevice)?
                    .edid
                    .clone();
                Ok(value)
            });
            crate::device::register(
                &format!("card0-Virtual-{}", index + 1),
                "drm",
                None,
                None,
                vec![("status", status), ("modes", modes), ("edid", edid)],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => crate::kinfo!(
            "DRM: VirtIO GPU online, {} outputs, {} capsets, features={:#x}",
            gpu.outputs.lock().len(),
            gpu.capsets.lock().len(),
            gpu.transport.features
        ),
        Err(e) => {
            gpu.transport.fail();
            crate::kwarn!("DRM: GPU initialization failed: {:?}", e);
        }
    }
}
pub fn start() {
    if let Some(gpu) = device() {
        let _ = crate::sched::spawn(
            move || loop {
                gpu.service();
                crate::sched::sleep_micros(1000);
            },
            0,
        );
    }
}
