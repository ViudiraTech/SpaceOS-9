/*
 *
 *       src/drm/gem.rs
 *       GEM objects: page-backed buffers with a kernel mapping and mmap
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux `drm_gem_shmem` equivalent: an object is a set of individually
//! allocated pages (no large contiguous allocation), mapped contiguously in
//! kernel space (`vmap`) so drivers can read the pixels, and mapped into
//! user space page by page through the device file's fake mmap offsets.
//! Handles are per file; objects are reference counted and freed when the
//! last handle, framebuffer and user mapping are gone.

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
use core::sync::atomic::{AtomicU64, Ordering};

const PAGE: u64 = 4096;
/// Each object owns a 4 GiB window of fake mmap offsets.
const OFFSET_SHIFT: u32 = 32;
/// Largest object the core will allocate (a 8K 32-bit frame is ~128 MiB).
const MAX_OBJECT: u64 = 256 << 20;

pub struct GemObject {
    pub id: u64,
    pub size: u64,
    pages: Vec<u64>,
    kernel: u64,
}

impl GemObject {
    /// Zeroed, kernel-mapped object of at least `size` bytes.
    pub fn new(size: u64) -> Result<Arc<Self>, VfsError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
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
        Ok(Arc::new(Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            size,
            pages,
            kernel: region.base(),
        }))
    }

    /// Kernel virtual address of the first byte; the object is contiguous.
    pub fn vaddr(&self) -> *mut u8 {
        self.kernel as *mut u8
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
}

impl Drop for GemObject {
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
        self.table.remove(&handle).map(|_| ()).ok_or(VfsError::NotFound)
    }

    pub fn contains(&self, object: &Arc<GemObject>) -> bool {
        self.table.values().any(|o| Arc::ptr_eq(o, object))
    }
}
