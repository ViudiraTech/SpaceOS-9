/*
 *
 *       src/drm/gem.rs
 *       GEM objects: page-backed buffers with a kernel mapping and mmap
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux `drm_gem_object` with pluggable backing storage. The core's own
//! backing is [`Shmem`] (`drm_gem_shmem`): individually allocated pages
//! mapped contiguously in kernel space so drivers can read the pixels.
//! Drivers with device memory (virtio-gpu resources, host-visible blobs)
//! supply their own [`GemBacking`]. Objects reach user space through the
//! device file's fake mmap offsets or as PRIME dma-buf fds; handles are per
//! file and objects are freed when the last handle, framebuffer, dma-buf
//! and user mapping are gone.

use crate::{
    mm::{pmm, vmm},
    sync::SpinLock,
    user::UserAddressSpace,
    vfs::{Inode, InodeKind, Stat, VfsError},
};
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicU64, Ordering},
};

const PAGE: u64 = 4096;
/// Each object owns a 4 GiB window of fake mmap offsets.
const OFFSET_SHIFT: u32 = 32;
/// Largest object the core will allocate (a 8K 32-bit frame is ~128 MiB).
const MAX_OBJECT: u64 = 256 << 20;

/// Storage behind a GEM object (Linux `drm_gem_object_funcs`).
pub trait GemBacking: Send + Sync + 'static {
    /// Size in bytes (a multiple of the page size).
    fn size(&self) -> u64;
    /// Contiguous kernel mapping, when the CPU can read the contents.
    fn vaddr(&self) -> Option<*mut u8> {
        None
    }
    /// Map `[offset, offset + length)` (page aligned, validated by the core)
    /// at `address`; `owner` keeps the object alive while mapped.
    fn map_user(
        &self,
        space: &mut UserAddressSpace,
        address: u64,
        length: u64,
        offset: u64,
        prot: u32,
        owner: Arc<dyn Inode>,
    ) -> Result<(), VfsError>;
    /// poll() readiness of a dma-buf of this object (implicit fencing).
    fn poll(&self) -> u32 {
        1 | 4
    }
    fn as_any(&self) -> &dyn Any;
}

pub struct GemObject {
    pub id: u64,
    pub size: u64,
    pub backing: Arc<dyn GemBacking>,
}

impl GemObject {
    pub fn new(backing: Arc<dyn GemBacking>) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Arc::new(Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            size: backing.size(),
            backing,
        })
    }

    /// Zeroed, kernel-mapped shmem object of at least `size` bytes.
    pub fn shmem(size: u64) -> Result<Arc<Self>, VfsError> {
        Ok(Self::new(Arc::new(Shmem::new(size)?)))
    }

    /// Kernel virtual address of the first byte, if CPU accessible.
    pub fn vaddr(&self) -> Option<*mut u8> {
        self.backing.vaddr()
    }

    /// The driver's backing of type `T`, if this object has one.
    pub fn backing<T: 'static>(&self) -> Option<&T> {
        self.backing.as_any().downcast_ref::<T>()
    }

    pub fn mmap_offset(&self) -> u64 {
        self.id << OFFSET_SHIFT
    }

    pub(crate) fn map_user(
        self: &Arc<Self>,
        space: &mut UserAddressSpace,
        address: u64,
        length: u64,
        offset: u64,
        prot: u32,
    ) -> Result<(), VfsError> {
        if offset % PAGE != 0
            || length % PAGE != 0
            || offset.checked_add(length).is_none_or(|end| end > self.size)
            || prot & 4 != 0
        {
            return Err(VfsError::InvalidArgument);
        }
        let owner: Arc<dyn Inode> = Arc::new(Mapping {
            object: self.clone(),
        });
        self.backing
            .map_user(space, address, length, offset, prot, owner)
    }
}

/// Page-backed storage with a contiguous kernel alias (`drm_gem_shmem`).
pub struct Shmem {
    size: u64,
    pages: Vec<u64>,
    kernel: u64,
}

impl Shmem {
    pub fn new(size: u64) -> Result<Self, VfsError> {
        let size = size.div_ceil(PAGE) * PAGE;
        if size == 0 || size > MAX_OBJECT {
            return Err(VfsError::InvalidArgument);
        }
        let count = (size / PAGE) as usize;
        let mut pages = Vec::with_capacity(count);
        let release = |pages: &[u64]| {
            for &page in pages {
                let _ = pmm::free(page, 1);
            }
        };
        for _ in 0..count {
            let Some(page) = pmm::alloc(1) else {
                release(&pages);
                return Err(VfsError::NoMemory);
            };
            // SAFETY: freshly allocated frame, reachable only through HHDM.
            unsafe { core::ptr::write_bytes(pmm::phys_to_virt(page) as *mut u8, 0, PAGE as usize) };
            pages.push(page);
        }
        let Ok(region) = vmm::reserve(count) else {
            release(&pages);
            return Err(VfsError::NoMemory);
        };
        for (index, &page) in pages.iter().enumerate() {
            if vmm::map(region, index, page, false).is_err() {
                let _ = vmm::unmap(region.base(), index);
                release(&pages);
                return Err(VfsError::NoMemory);
            }
        }
        Ok(Self {
            size,
            pages,
            kernel: region.base(),
        })
    }
}

impl GemBacking for Shmem {
    fn size(&self) -> u64 {
        self.size
    }

    fn vaddr(&self) -> Option<*mut u8> {
        Some(self.kernel as *mut u8)
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
        for done in (0..length).step_by(PAGE as usize) {
            let page = self.pages[((offset + done) / PAGE) as usize];
            if space
                .map_external(address + done, page, prot, owner.clone())
                .is_err()
            {
                let _ = space.unmap(address, done);
                return Err(VfsError::NoMemory);
            }
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for Shmem {
    fn drop(&mut self) {
        let _ = vmm::unmap(self.kernel, self.pages.len());
        for &page in &self.pages {
            let _ = pmm::free(page, 1);
        }
    }
}

/// Keeps an object alive while user pages map it.
struct Mapping {
    object: Arc<GemObject>,
}

impl Inode for Mapping {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(0, InodeKind::Regular, 0o600, self.object.size))
    }
}

/// Device-wide index from mmap offset to object (Linux vma offset manager).
pub struct Registry {
    objects: SpinLock<BTreeMap<u64, Weak<GemObject>>>,
}

impl Registry {
    pub const fn new() -> Self {
        Self {
            objects: SpinLock::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, object: &Arc<GemObject>) {
        let mut objects = self.objects.lock();
        objects.retain(|_, weak| weak.strong_count() != 0);
        objects.insert(object.id, Arc::downgrade(object));
    }

    /// Whether `object` belongs to this device (PRIME import check).
    pub fn contains(&self, object: &Arc<GemObject>) -> bool {
        self.objects
            .lock()
            .get(&object.id)
            .is_some_and(|weak| weak.as_ptr() == Arc::as_ptr(object))
    }

    pub fn by_offset(&self, offset: u64) -> Option<(Arc<GemObject>, u64)> {
        let id = offset >> OFFSET_SHIFT;
        let object = self.objects.lock().get(&id)?.upgrade()?;
        Some((object, offset & ((1 << OFFSET_SHIFT) - 1)))
    }
}

/// Per-file handle table (Linux `drm_file::object_idr`).
pub struct Handles {
    next: u32,
    table: BTreeMap<u32, Arc<GemObject>>,
}

impl Handles {
    pub fn new() -> Self {
        Self {
            next: 1,
            table: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn insert(&mut self, object: Arc<GemObject>) -> u32 {
        // Reuse a handle already naming this object, as Linux does for
        // flink/prime imports into the same file.
        if let Some((&handle, _)) = self.table.iter().find(|(_, o)| Arc::ptr_eq(o, &object)) {
            return handle;
        }
        let handle = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        self.table.insert(handle, object);
        handle
    }

    pub fn get(&self, handle: u32) -> Result<Arc<GemObject>, VfsError> {
        self.table.get(&handle).cloned().ok_or(VfsError::NotFound)
    }

    pub fn remove(&mut self, handle: u32) -> Result<(), VfsError> {
        self.table
            .remove(&handle)
            .map(|_| ())
            .ok_or(VfsError::NotFound)
    }

    pub fn contains(&self, object: &Arc<GemObject>) -> bool {
        self.table.values().any(|o| Arc::ptr_eq(o, object))
    }
}
