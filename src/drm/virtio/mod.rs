/*
 *
 *       src/drm/virtio/mod.rs
 *       virtio-gpu DRM driver: resources, 3D contexts, blobs and execbuffer
 *
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux `drivers/gpu/drm/virtio` on top of the generic DRM core. The core
//! owns /dev/dri/card0 and renderD128, KMS, GEM handles, PRIME, syncobj and
//! events; this driver provides:
//!
//! * GEM storage: virtio resources ([`Resource`]) — 2D dumb buffers, 3D
//!   resources and blobs (guest pages or host-visible BAR memory);
//! * the VIRTGPU ioctls (MAP, EXECBUFFER, GETPARAM, RESOURCE_CREATE[_BLOB],
//!   RESOURCE_INFO, TRANSFER_{TO,FROM}_HOST, WAIT, GET_CAPS, CONTEXT_INIT)
//!   for VirGL and Venus (per-file 3D context, rings, fence events);
//! * display programming (`display.rs`): scanouts, transfers/flushes of
//!   damage and the hardware cursor;
//! * the text console on a virtio scanout when there is no firmware
//!   framebuffer, refreshed by the service thread while no DRM client owns
//!   the display.

mod display;
#[cfg(feature = "boot-self-test")]
pub mod self_test;
mod transport;

use super::{
    self as drm, DeviceConfig, DriverFile, File, GemBacking, Ioctl,
    syncobj::{self, Fence, FenceSource},
};
use crate::{
    sync::SpinLock,
    user::{UserAddressSpace, uaccess},
    vfs::{self, Inode, InodeKind, Stat, VfsError},
};
use alloc::{
    format,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};
use transport::{Dma, Request, Transport};

static GPU: SpinLock<Option<Arc<Gpu>>> = SpinLock::new(None);

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
/// A control-queue command: header (type, flags, fence, ctx) and words.
pub fn command(kind: u32, context: u32, words: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0; 24 + words.len() * 4];
    put32(&mut bytes, 0, kind);
    put32(&mut bytes, 16, context);
    for (i, &word) in words.iter().enumerate() {
        put32(&mut bytes, 24 + i * 4, word);
    }
    bytes
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
    next_fence: AtomicU64,
    pub capsets: SpinLock<Vec<Capset>>,
    pub outputs: SpinLock<Vec<display::Output>>,
    host_ranges: SpinLock<Vec<(u64, u64)>>,
    retired: SpinLock<Vec<Retirement>>,
    files: SpinLock<Vec<Weak<VirtFile>>>,
    console: SpinLock<Option<Arc<ConsoleBacking>>>,
    console_version: AtomicU64,
    console_tick: AtomicU64,
    console_visible: AtomicBool,
    /// A DRM client programmed a scanout: the console stays hidden until
    /// lastclose.
    display_owned: AtomicBool,
    servicing: AtomicBool,
    live_resources: AtomicU32,
    /// What each scanout shows, to reprogram only on change.
    scanouts: SpinLock<Vec<Option<display::Scanout>>>,
    /// Framebuffer id of each pipe's hardware cursor image.
    cursors: SpinLock<Vec<u32>>,
    /// The DRM device registered for this GPU.
    drm: SpinLock<Weak<drm::Device>>,
}

/// A submitted command as a DRM fence source.
struct RequestFence {
    gpu: Weak<Gpu>,
    request: Arc<Request>,
}

impl FenceSource for RequestFence {
    fn status(&self) -> i32 {
        let request = &self.request;
        if !request.done.load(Ordering::Acquire) {
            0
        } else if request.error.load(Ordering::Acquire) != 0 {
            -5
        } else {
            // SAFETY: the response buffer is owned by the completed request.
            let kind = unsafe { (request.response.pointer() as *const u32).read_volatile() };
            if (0x1100..=0x1106).contains(&u32::from_le(kind)) {
                1
            } else {
                -5
            }
        }
    }

    fn wait(&self) -> Result<(), VfsError> {
        match self.gpu.upgrade() {
            Some(gpu) => gpu.transport.wait(&self.request).map(|_| ()),
            None => Ok(()),
        }
    }
}

impl Gpu {
    fn request_fence(self: &Arc<Self>, request: Arc<Request>) -> Arc<Fence> {
        Fence::from_source(Arc::new(RequestFence {
            gpu: Arc::downgrade(self),
            request,
        }))
    }

    /// The registered DRM device (for tests and diagnostics).
    pub fn drm_device(&self) -> Option<Arc<drm::Device>> {
        self.drm.lock().upgrade()
    }

    /// Whether the transport has failed (device reset or timeout).
    pub fn failed(&self) -> bool {
        self.transport.failed.load(Ordering::Acquire)
    }
    pub(super) fn submit_call(
        &self,
        kind: u32,
        context: u32,
        words: &[u32],
        size: usize,
    ) -> Result<Arc<transport::Request>, VfsError> {
        let mut bytes = command(kind, context, words);
        put32(&mut bytes, 4, 1);
        put64(
            &mut bytes,
            8,
            self.next_fence.fetch_add(1, Ordering::Relaxed),
        );
        self.transport.submit(false, &bytes, size, Vec::new())
    }
    pub(super) fn call(
        &self,
        kind: u32,
        context: u32,
        words: &[u32],
        size: usize,
    ) -> Result<Vec<u8>, VfsError> {
        let request = self.submit_call(kind, context, words, size)?;
        self.transport.wait(&request)
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
    pub(super) fn create_2d(
        self: &Arc<Self>,
        width: u32,
        height: u32,
    ) -> Result<Arc<Resource>, VfsError> {
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
        state: &mut VirtState,
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
        state: &mut VirtState,
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
            self.submit_bytes(state, bytes, 0, Vec::new())?.wait()?;
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
    fn ensure_context(&self, state: &mut VirtState) -> Result<(), VfsError> {
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
        self: &Arc<Self>,
        state: &mut VirtState,
        bytes: Vec<u8>,
        ring: u32,
        retain: Vec<Arc<dyn Inode>>,
    ) -> Result<Arc<Fence>, VfsError> {
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
        Ok(self.request_fence(request))
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
        *self.console.lock() = Some(backing);
        self.console_visible.store(true, Ordering::Release);
        Ok(())
    }
    /// While the text console owns the display, its framebuffer is flushed to
    /// the host at display refresh rate (deferred I/O); once a DRM client has
    /// programmed a scanout the service thread needs no timer at all.
    fn console_refresh_deadline(&self) -> Option<u64> {
        if self.transport.failed.load(Ordering::Acquire)
            || self.display_owned.load(Ordering::Acquire)
        {
            return None;
        }
        Some(
            self.console_tick
                .load(Ordering::Relaxed)
                .saturating_add(16_000)
                .max(crate::time::uptime_micros().saturating_add(1_000)),
        )
    }

    fn console_service(&self) {
        if self.transport.failed.load(Ordering::Acquire) {
            return;
        }
        if self.display_owned.load(Ordering::Acquire) {
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
        let files: Vec<_> = {
            let mut list = self.files.lock();
            list.retain(|w| w.strong_count() != 0);
            list.iter().filter_map(|w| w.upgrade()).collect()
        };
        for file in files {
            file.complete_events();
        }
        self.console_service();
        // Completing an earlier request can drop its retained resources and
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

/// A virtio-gpu resource (Linux `virtio_gpu_object`).
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
    /// Fence of the last command that used this resource (implicit sync).
    pub last: SpinLock<Option<Arc<Fence>>>,
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
            // The service thread releases host resources.
            syncobj::fence_event();
        }
    }
}

/// GEM storage of a resource.
pub struct ResourceObject(pub Arc<Resource>);

impl GemBacking for ResourceObject {
    fn size(&self) -> u64 {
        self.0.size
    }

    fn map_user(
        &self,
        space: &mut UserAddressSpace,
        address: u64,
        length: u64,
        offset: u64,
        prot: u32,
        owner: Arc<dyn Inode>,
    ) -> Result<(), VfsError> {
        let resource = &self.0;
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
        self.0
            .last
            .lock()
            .as_ref()
            .map_or(1 | 4, |f| if f.status() != 0 { 1 | 4 } else { 0 })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Keeps resources alive while the host may still use them (a submitted
/// command's retain list).
struct Retain(Arc<Resource>);

impl Inode for Retain {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(
            0x8000_0000 + self.0.id as u64,
            InodeKind::Regular,
            0o600,
            self.0.size,
        ))
    }
}

/// The resource behind a GEM handle of `file`.
fn resource(file: &File, handle: u32) -> Result<Arc<Resource>, VfsError> {
    file.object(handle)?
        .backing::<ResourceObject>()
        .map(|r| r.0.clone())
        .ok_or(VfsError::InvalidArgument)
}

/// Give `file` a handle for `resource`.
fn add(file: &File, resource: Arc<Resource>) -> Result<u32, VfsError> {
    Ok(file.add_object(Arc::new(ResourceObject(resource)))?.0)
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

// ---- per-file state -------------------------------------------------------------

/// 3D context state of one DRM file.
pub struct VirtState {
    pub context: u32,
    pub capset: u32,
    pub rings: u32,
    pub poll_rings: u64,
    pub initialized: bool,
    pub created: bool,
}

impl VirtState {
    fn new() -> Self {
        Self {
            context: 0,
            capset: 0,
            rings: 1,
            poll_rings: 0,
            initialized: false,
            created: false,
        }
    }
}

/// Driver side of one open DRM file (Linux `virtio_gpu_fpriv`).
pub struct VirtFile {
    gpu: Arc<Gpu>,
    device: Weak<drm::Device>,
    file: u64,
    /// Serializes this file's driver ioctls. A contender sleeps on
    /// `busy_wait`; `unlocked` drops and retakes it around blocking work.
    busy: AtomicBool,
    busy_wait: crate::sched::WaitQueue,
    state: SpinLock<Option<VirtState>>,
    pending_events: SpinLock<Vec<Arc<Fence>>>,
}

impl VirtFile {
    fn acquire_busy(&self) {
        while self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if crate::sched::current().is_some() {
                self.busy_wait
                    .wait_until(|| !self.busy.load(Ordering::Acquire));
            } else {
                core::hint::spin_loop();
            }
        }
    }

    fn release_busy(&self) {
        self.busy.store(false, Ordering::Release);
        self.busy_wait.wake_one();
    }

    pub fn unlocked<R>(&self, state: &mut VirtState, work: impl FnOnce() -> R) -> R {
        let local = core::mem::replace(state, VirtState::new());
        *self.state.lock() = Some(local);
        self.release_busy();
        let result = work();
        self.acquire_busy();
        *state = self
            .state
            .lock()
            .take()
            .expect("virtio-gpu file state reacquired");
        result
    }
    fn dispatch(
        &self,
        file: &File,
        state: &mut VirtState,
        arg: &mut Ioctl,
    ) -> Result<(), VfsError> {
        match arg.nr {
            0x41 => {
                // VIRTGPU_MAP: the object's fake offset on this file.
                let object = file.object(arg.r32(8)?)?;
                if arg.r32(12)? != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                arg.w64(0, object.mmap_offset());
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
                let handle = add(file, resource)?;
                arg.w32(40, handle);
                arg.w32(44, id);
                Ok(())
            }
            0x45 => {
                let resource = resource(file, arg.r32(0)?)?;
                arg.w32(4, resource.id);
                arg.w32(8, resource.size.min(u32::MAX as u64) as u32);
                arg.w32(12, resource.blob_mem);
                Ok(())
            }
            0x46 | 0x47 => {
                let resource = resource(file, arg.r32(0)?)?;
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
                    if arg.nr == 0x46 { 0x206 } else { 0x205 },
                    state.context,
                    &words,
                    24,
                )?;
                Ok(())
            }
            0x48 => {
                let resource = resource(file, arg.r32(0)?)?;
                let flags = arg.r32(4)?;
                if flags & !1 != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                let fence = resource.last.lock().clone();
                if let Some(fence) = fence {
                    if flags & 1 != 0 && fence.status() == 0 {
                        return Err(VfsError::Busy);
                    }
                    self.unlocked(state, || fence.wait())?;
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
                let handle = add(file, resource)?;
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
            0x42 => self.exec(file, state, arg),
            _ => Err(VfsError::InvalidArgument),
        }
    }

    fn exec(&self, file: &File, state: &mut VirtState, arg: &mut Ioctl) -> Result<(), VfsError> {
        let flags = arg.r32(0)?;
        if flags & !7 != 0 {
            return Err(VfsError::InvalidArgument);
        }
        let ring = if flags & 4 != 0 { arg.r32(32)? } else { 0 };
        if ring >= state.rings || ring >= 64 {
            return Err(VfsError::InvalidArgument);
        }
        let handles = drm::ioctl::read_u32s(arg.r64(16)?, arg.r32(24)? as usize)?;
        let mut resources = Vec::new();
        for h in handles {
            resources.push(resource(file, h)?);
        }
        if flags & 1 != 0 {
            let fence = syncobj::fence_fd(arg.r32(28)?)?;
            self.unlocked(state, || fence.wait())?;
        }
        let ins = syncobj::exec_syncs(
            file,
            arg.r64(48)?,
            arg.r32(40)? as usize,
            arg.r32(36)? as usize,
        )?;
        let outs = syncobj::exec_syncs(
            file,
            arg.r64(56)?,
            arg.r32(44)? as usize,
            arg.r32(36)? as usize,
        )?;
        for (sync, point, reset) in &ins {
            if let Some(f) = sync.fence(*point) {
                self.unlocked(state, || f.wait())?;
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
            .map(|r| Arc::new(Retain(r.clone())) as Arc<dyn Inode>)
            .collect();
        let notify = state.poll_rings & (1u64 << ring) != 0;
        if notify && self.pending_events.lock().len() + file.pending_events() >= 256 {
            return Err(VfsError::NoSpace);
        }
        let reserved = if flags & 2 != 0 {
            let placeholder = Fence::pending();
            let fd = syncobj::install_fd(
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
                    syncobj::close_fd(fd);
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
    /// Turn signalled ring fences into VIRTGPU_EVENT_FENCE_SIGNALED events
    /// for rings the client asked to poll (CONTEXT_INIT poll_rings_mask).
    fn complete_events(&self) {
        let ready = {
            let mut pending = self.pending_events.lock();
            let count = pending.iter().filter(|f| f.status() != 0).count();
            pending.retain(|f| f.status() == 0);
            count
        };
        let Some(device) = self.device.upgrade() else {
            return;
        };
        for _ in 0..ready {
            let mut event = [0u8; 8];
            put32(&mut event, 0, 0x9000_0000);
            put32(&mut event, 4, 8);
            drm::file::deliver(&device, self.file, &event);
        }
    }
}

impl DriverFile for VirtFile {
    fn ioctl(&self, file: &File, arg: &mut Ioctl) -> Result<(), VfsError> {
        if self.gpu.failed() {
            return Err(VfsError::NoDevice);
        }
        self.acquire_busy();
        let mut state = self
            .state
            .lock()
            .take()
            .expect("virtio-gpu file serialized");
        let result = self.dispatch(file, &mut state, arg);
        *self.state.lock() = Some(state);
        self.release_busy();
        result
    }

    fn poll(&self, _file: &File) {
        self.complete_events();
    }

    fn needs_tick(&self) -> bool {
        // Fence completion only becomes an event when poll collects it.
        !self.pending_events.lock().is_empty()
    }
}

impl Drop for VirtFile {
    fn drop(&mut self) {
        if let Some(state) = self.state.lock().take()
            && state.created
        {
            self.gpu.retired.lock().push(Retirement {
                id: state.context,
                pages: Vec::new(),
                host: None,
                context: true,
            });
            syncobj::fence_event();
        }
    }
}

impl Gpu {
    fn open_file(self: &Arc<Self>, file: &Arc<File>) -> Arc<VirtFile> {
        let private = Arc::new(VirtFile {
            gpu: self.clone(),
            device: Arc::downgrade(file.device()),
            file: file.id(),
            busy: AtomicBool::new(false),
            busy_wait: crate::sched::WaitQueue::new(),
            state: SpinLock::new(Some(VirtState::new())),
            pending_events: SpinLock::new(Vec::new()),
        });
        self.files.lock().push(Arc::downgrade(&private));
        private
    }
}

/// Size and direction of the VIRTGPU ioctls (Linux `virtgpu_drm.h`).
fn ioctl_descriptor(nr: u8) -> Option<(usize, u32)> {
    let rw = drm::ioctl::READ | drm::ioctl::WRITE;
    Some(match nr {
        0x41 => (16, rw), // MAP
        0x42 => (64, rw), // EXECBUFFER
        0x43 => (16, rw), // GETPARAM
        0x44 => (56, rw), // RESOURCE_CREATE
        0x45 => (16, rw), // RESOURCE_INFO
        0x46 => (44, rw), // TRANSFER_FROM_HOST
        0x47 => (44, rw), // TRANSFER_TO_HOST
        0x48 => (8, rw),  // WAIT
        0x49 => (24, rw), // GET_CAPS
        0x4a => (56, rw), // RESOURCE_CREATE_BLOB
        0x4b => (16, rw), // CONTEXT_INIT
        _ => return None,
    })
}

pub fn init() {
    drm::file::init_trace();
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
        next_fence: AtomicU64::new(1),
        capsets: SpinLock::new(Vec::new()),
        outputs: SpinLock::new(Vec::new()),
        host_ranges: SpinLock::new(Vec::new()),
        retired: SpinLock::new(Vec::with_capacity(8192)),
        files: SpinLock::new(Vec::new()),
        console: SpinLock::new(None),
        console_version: AtomicU64::new(0),
        console_tick: AtomicU64::new(0),
        console_visible: AtomicBool::new(false),
        display_owned: AtomicBool::new(false),
        servicing: AtomicBool::new(false),
        live_resources: AtomicU32::new(0),
        scanouts: SpinLock::new(Vec::new()),
        cursors: SpinLock::new(Vec::new()),
        drm: SpinLock::new(Weak::new()),
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
        let outputs = display::discover(&gpu)?;
        *gpu.scanouts.lock() = vec![None; outputs.len()];
        *gpu.cursors.lock() = vec![0; outputs.len()];
        *gpu.outputs.lock() = outputs;
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
        let mut config = DeviceConfig::new(
            "virtio_gpu",
            "SpaceKernel VirtIO GPU",
            "20261024",
            display::pipes(&gpu.outputs.lock()),
        );
        config.version = (0, 1, 0);
        // Like Linux virtio-gpu: dumb buffers live in guest memory and are
        // transferred to the host on DIRTYFB, so clients should shadow.
        config.prefer_shadow = true;
        config.claims_firmware_fb = true;
        config.render = true;
        config.prime = true;
        config.syncobj = true;
        config.parent = Some(format!("{}", pci.address));
        config.attributes = vec![
            ("vendor", alloc::string::String::from("0x1af4\n")),
            ("device", format!("0x{:04x}\n", pci.device_id)),
        ];
        let registered = drm::register(config, gpu.clone())?;
        *gpu.drm.lock() = Arc::downgrade(&registered);
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
                // Fence completions, queued KMS commits and retired resources
                // all post a fence event; read it before servicing so none is
                // missed.
                let seen = syncobj::fence_events();
                gpu.service();
                syncobj::wait_fence_event(seen, gpu.console_refresh_deadline());
            },
            0,
        );
    }
}
