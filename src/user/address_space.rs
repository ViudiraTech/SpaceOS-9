/*
 *
 *       src/user/address_space.rs
 *       Isolated user virtual address space management and translation tables
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Architecture-aware user virtual address space management.
//!
//! Kernel higher-half tables are shared across all address spaces, while the
//! canonical lower-half is private to each user process.

use crate::vfs::{Inode, VfsError};
use crate::{
    arch,
    mm::{pmm, vmm},
    sync::Mutex,
};
use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};

pub const PAGE_SIZE: u64 = 4096;
pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;
const MAX_MAPPING: u64 = 64 * 1024 * 1024 * 1024;

#[derive(Clone)]
struct Mapping {
    physical: u64,
    kernel: u64,
    uncached: bool,
    prot: u32,
    max_prot: u32,
    owned: bool,
    cow: bool,
    owner: Option<Arc<dyn Inode>>,
}

#[derive(Clone)]
pub struct ProcessImage {
    pub executable: String,
    pub argv: Vec<u8>,
    pub environ: Vec<u8>,
    pub auxv: Vec<u8>,
}

/// A reserved range whose pages materialize on first access.
///
/// Anonymous memory is zero-filled. Private file mappings read their page
/// outside the address-space spin lock, because filesystems may sleep.
#[derive(Clone)]
struct LazyRegion {
    end: u64,
    prot: u32,
    max_prot: u32,
    file: Option<Arc<dyn Inode>>,
    /// File offset of the region's start address.
    offset: u64,
    /// When set, faults map the file's shared pages instead of copying them.
    shared: bool,
}

/// What a fault handler must do outside the spin lock.
pub enum FaultWork {
    /// The page was resolved under the lock, or is already present.
    Done,
    /// No mapping allows this access.
    Violation,
    /// Read a page of `file` at `offset`, then call `finish_file_fault`.
    ReadFile { file: Arc<dyn Inode>, offset: u64 },
    /// Ask `file` to install shared pages for this page.
    MapShared {
        file: Arc<dyn Inode>,
        page: u64,
        offset: u64,
        prot: u32,
    },
}

pub struct UserAddressSpace {
    root_phys: u64,
    allocated_tables: Vec<u64>,
    mapped_frames: BTreeMap<u64, Mapping>,
    lazy: BTreeMap<u64, LazyRegion>,
    pub brk_base: u64,
    pub brk: u64,
    pub image: Option<ProcessImage>,
}

unsafe impl Send for UserAddressSpace {}
unsafe impl Sync for UserAddressSpace {}

impl UserAddressSpace {
    pub fn new() -> Result<Self, &'static str> {
        let root_phys = pmm::alloc(1).ok_or("failed to allocate user root page table")?;
        let root_virt = pmm::phys_to_virt(root_phys) as *mut u64;

        // Zero the entire 4 KiB root table frame
        unsafe {
            core::ptr::write_bytes(root_virt, 0, 512);
        }

        // On x86_64 and RISC-V, copy the kernel's top-half mappings into entries 256..512
        #[cfg(not(target_arch = "aarch64"))]
        {
            let kernel_root = vmm::root().ok_or("kernel VMM root unavailable")?;
            let kernel_virt = pmm::phys_to_virt(kernel_root) as *const u64;
            unsafe {
                core::ptr::copy_nonoverlapping(kernel_virt.add(256), root_virt.add(256), 256);
            }
        }

        Ok(Self {
            root_phys,
            allocated_tables: Vec::new(),
            mapped_frames: BTreeMap::new(),
            lazy: BTreeMap::new(),
            brk_base: 0x2000_0000,
            brk: 0x2000_0000,
            image: None,
        })
    }

    /// Copy this address space for `fork`/`clone` without CLONE_VM.
    ///
    /// Every privately owned page is retained and mapped read-only in both
    /// parent and child.  The first write fault copies the page.  Shared
    /// device/file mappings keep their physical page and are not refcounted;
    /// their `Inode` owner already outlives the mapping.
    pub fn fork_copy(&mut self) -> Result<Self, &'static str> {
        let mut child = Self::new()?;
        child.brk_base = self.brk_base;
        child.brk = self.brk;
        child.image = self.image.clone();
        child.lazy = self.lazy.clone();

        for (&address, mapping) in &self.mapped_frames {
            let mut copy = mapping.clone();
            if mapping.owned {
                pmm::retain(mapping.physical, 1)?;
                copy.cow = true;
            } else {
                copy.cow = false;
            }
            if let Err(error) = child.install_mapping(address, copy) {
                if mapping.owned {
                    let _ = pmm::release(mapping.physical, 1);
                }
                return Err(error);
            }
        }

        // Publish the parent side only after the child is complete, so a
        // failed fork never leaves the parent with dangling COW state.
        let owned: Vec<(u64, u64, u32)> = self
            .mapped_frames
            .iter()
            .filter(|(_, mapping)| mapping.owned)
            .map(|(&address, mapping)| (address, mapping.physical, mapping.prot))
            .collect();
        let owned_count = owned.len();
        let first_owned = owned.first().map(|&(address, _, _)| address);
        for (address, physical, prot) in owned {
            let leaf = self.leaf(address, false)?;
            // SAFETY: the address is present in mapped_frames and the leaf
            // belongs to this private address space.
            unsafe { leaf.write_volatile(user_leaf(physical, prot & !PROT_WRITE)) };
            if let Some(mapping) = self.mapped_frames.get_mut(&address) {
                mapping.cow = true;
            }
        }
        if let Some(address) = first_owned {
            if owned_count == 1 {
                arch::tlb_shootdown(address, 1);
            } else {
                arch::tlb_shootdown(0, 0);
            }
        }
        Ok(child)
    }

    /// Resolve a write fault on a COW page.  Returns false when the address is
    /// not a writable COW mapping, so the arch fault path can deliver SIGSEGV.
    pub fn cow_break(&mut self, address: u64) -> Result<bool, VfsError> {
        self.cow_break_inner(address)
    }

    fn lazy_region(&self, address: u64) -> Option<(u64, &LazyRegion)> {
        let (&start, region) = self.lazy.range(..=address).next_back()?;
        (address < region.end).then_some((start, region))
    }

    /// Resolve an access to a non-present or read-only page.
    ///
    /// Anonymous lazy pages and copy-on-write are handled here. File-backed
    /// pages need I/O and return work for the caller to perform unlocked.
    pub fn fault(&mut self, address: u64, write: bool, exec: bool) -> Result<FaultWork, VfsError> {
        let page = address & !4095;
        if let Some(mapping) = self.mapped_frames.get(&page) {
            let prot = mapping.prot;
            if (write && prot & PROT_WRITE == 0)
                || (exec && prot & PROT_EXEC == 0)
                || (!write && !exec && prot == 0)
            {
                return Ok(FaultWork::Violation);
            }
            if write && mapping.cow {
                self.cow_break_inner(page)?;
            }
            return Ok(FaultWork::Done);
        }
        let Some((start, region)) = self.lazy_region(page) else {
            return Ok(FaultWork::Violation);
        };
        let prot = region.prot;
        if prot == 0 || (write && prot & PROT_WRITE == 0) || (exec && prot & PROT_EXEC == 0) {
            return Ok(FaultWork::Violation);
        }
        match &region.file {
            None => {
                let frame = pmm::alloc(1).ok_or(VfsError::NoMemory)?;
                // SAFETY: freshly allocated frame, not yet visible to user code.
                unsafe { core::ptr::write_bytes(pmm::phys_to_virt(frame) as *mut u8, 0, 4096) };
                let max_prot = region.max_prot;
                if self.install(page, frame, prot, true, None).is_err() {
                    let _ = pmm::free(frame, 1);
                    return Err(VfsError::NoMemory);
                }
                if let Some(mapping) = self.mapped_frames.get_mut(&page) {
                    mapping.max_prot = max_prot;
                }
                Ok(FaultWork::Done)
            }
            Some(file) if region.shared => Ok(FaultWork::MapShared {
                file: file.clone(),
                page,
                offset: region.offset + (page - start),
                prot,
            }),
            Some(file) => Ok(FaultWork::ReadFile {
                file: file.clone(),
                offset: region.offset + (page - start),
            }),
        }
    }

    /// Install the page read for `FaultWork::ReadFile`, which takes ownership
    /// of `frame`. A racing fault or unmap simply drops the frame.
    pub fn finish_file_fault(&mut self, address: u64, frame: u64) {
        let page = address & !4095;
        let region = self.lazy_region(page).map(|(_, r)| (r.prot, r.max_prot));
        let present = self.mapped_frames.contains_key(&page);
        match region {
            Some((prot, max_prot)) if !present => {
                if self.install(page, frame, prot, true, None).is_err() {
                    let _ = pmm::free(frame, 1);
                } else if let Some(mapping) = self.mapped_frames.get_mut(&page) {
                    mapping.max_prot = max_prot;
                }
            }
            _ => {
                let _ = pmm::free(frame, 1);
            }
        }
    }

    /// Reserve a lazily populated mapping. The range must already be free.
    pub fn map_lazy(
        &mut self,
        address: u64,
        length: u64,
        prot: u32,
        max_prot: u32,
        file: Option<Arc<dyn Inode>>,
        offset: u64,
        shared: bool,
    ) -> Result<(), VfsError> {
        if address & 4095 != 0 || length & 4095 != 0 || length == 0 {
            return Err(VfsError::InvalidArgument);
        }
        if !Self::valid_range(address, length as usize) {
            return Err(VfsError::NoMemory);
        }
        self.lazy.insert(
            address,
            LazyRegion {
                end: address + length,
                prot,
                max_prot,
                file,
                offset,
                shared,
            },
        );
        Ok(())
    }

    /// Remove `[start, end)` from the lazy map, splitting regions as needed.
    fn lazy_carve(&mut self, start: u64, end: u64) {
        let overlapping: Vec<u64> = self
            .lazy
            .range(..end)
            .filter(|(_, region)| region.end > start)
            .map(|(&key, _)| key)
            .collect();
        for key in overlapping {
            let region = self.lazy.remove(&key).unwrap();
            if key < start {
                let mut left = region.clone();
                left.end = start;
                self.lazy.insert(key, left);
            }
            if region.end > end {
                let mut right = region.clone();
                right.offset = region.offset + (end - key);
                right.end = region.end;
                self.lazy.insert(end, right);
            }
        }
    }

    /// Whether every page in `[address, address + length)` is mapped or reserved.
    pub fn range_mapped(&self, address: u64, length: u64) -> bool {
        let end = address.saturating_add(length);
        let mut cursor = address & !4095;
        while cursor < end {
            if self.mapped_frames.contains_key(&cursor) {
                cursor += PAGE_SIZE;
            } else if let Some((_, region)) = self.lazy_region(cursor) {
                cursor = region.end;
            } else {
                return false;
            }
        }
        true
    }

    /// `MADV_DONTNEED`: drop private pages so the next touch reads zero, or the
    /// file again for private file mappings. Shared pages stay mapped.
    pub fn discard_private(&mut self, address: u64, length: u64) -> Result<(), VfsError> {
        let end = address + length;
        let pages: Vec<u64> = self
            .mapped_frames
            .range(address..end)
            .filter(|(_, mapping)| mapping.owned)
            .map(|(&page, _)| page)
            .collect();
        // Anything no lazy region covers came from an eager mapping, such as
        // the ELF image; keep the range valid as anonymous memory afterwards.
        for &page in &pages {
            if self.lazy_region(page).is_none() {
                let mapping = &self.mapped_frames[&page];
                let (prot, max_prot) = (mapping.prot, mapping.max_prot);
                self.lazy.insert(
                    page,
                    LazyRegion {
                        end: page + PAGE_SIZE,
                        prot,
                        max_prot,
                        file: None,
                        offset: 0,
                        shared: false,
                    },
                );
            }
            let leaf = self.leaf(page, false).map_err(|_| VfsError::Fault)?;
            // SAFETY: the leaf belongs to this exclusively locked address space.
            unsafe { leaf.write_volatile(0) };
        }
        if !pages.is_empty() {
            arch::tlb_shootdown(address, (length / PAGE_SIZE) as usize);
        }
        for page in pages {
            if let Some(mapping) = self.mapped_frames.remove(&page) {
                let _ = pmm::release(mapping.physical, 1);
            }
        }
        Ok(())
    }

    /// Whether any page in `[start, end)` is mapped or reserved.
    pub fn range_in_use(&self, start: u64, end: u64) -> bool {
        self.mapped_frames.range(start..end).next().is_some()
            || self
                .lazy
                .range(..end)
                .next_back()
                .is_some_and(|(_, region)| region.end > start)
    }

    /// Move or resize a mapping (`mremap`). Returns the resulting address.
    ///
    /// Present pages move by rewriting PTEs, so nothing is copied and shared
    /// pages stay shared; reserved ranges move with their file offset intact.
    pub fn remap(
        &mut self,
        old: u64,
        old_size: u64,
        new_size: u64,
        may_move: bool,
        fixed: Option<u64>,
    ) -> Result<u64, VfsError> {
        let old_end = old.checked_add(old_size).ok_or(VfsError::Fault)?;
        // The whole old range has to be mapped by something.
        for page in (old..old_end).step_by(4096) {
            if !self.mapped_frames.contains_key(&page) && self.lazy_region(page).is_none() {
                return Err(VfsError::Fault);
            }
        }
        if let Some(target) = fixed {
            if target & 4095 != 0 || !Self::valid_range(target, new_size as usize) {
                return Err(VfsError::InvalidArgument);
            }
            let target_end = target + new_size;
            if target < old_end && old < target_end {
                return Err(VfsError::InvalidArgument);
            }
            self.unmap(target, new_size)?;
        }
        if fixed.is_none() && new_size <= old_size {
            if new_size < old_size {
                self.unmap(old + new_size, old_size - new_size)?;
            }
            return Ok(old);
        }
        // Region describing the bytes past the old end when growing.
        let tail_template = self
            .lazy_region(old_end - 1)
            .map(|(start, region)| (start, region.clone()))
            .or_else(|| {
                self.mapped_frames.get(&(old_end - 4096)).map(|mapping| {
                    (
                        old,
                        LazyRegion {
                            end: old_end,
                            prot: mapping.prot,
                            max_prot: mapping.max_prot,
                            file: if mapping.owned {
                                None
                            } else {
                                mapping.owner.clone()
                            },
                            offset: 0,
                            shared: !mapping.owned,
                        },
                    )
                })
            })
            .ok_or(VfsError::Fault)?;
        if fixed.is_none()
            && !self.range_in_use(old_end, old + new_size)
            && Self::valid_range(old, new_size as usize)
        {
            let (start, template) = &tail_template;
            let offset = template.offset + (old_end - *start);
            self.map_lazy(
                old_end,
                old + new_size - old_end,
                template.prot,
                template.max_prot,
                template.file.clone(),
                offset,
                template.shared,
            )?;
            return Ok(old);
        }
        if fixed.is_none() && !may_move {
            return Err(VfsError::NoMemory);
        }
        let target = match fixed {
            Some(target) => target,
            None => self.find_free(0, new_size)?,
        };
        let moved_len = old_size.min(new_size);
        let pages: Vec<u64> = self
            .mapped_frames
            .range(old..old + moved_len)
            .map(|(&page, _)| page)
            .collect();
        for &page in &pages {
            let leaf = self.leaf(page, false).map_err(|_| VfsError::Fault)?;
            // SAFETY: the leaf belongs to this exclusively locked address space.
            unsafe { leaf.write_volatile(0) };
        }
        if !pages.is_empty() {
            arch::tlb_shootdown(old, (moved_len / PAGE_SIZE) as usize);
        }
        for page in pages {
            let mapping = self.mapped_frames.remove(&page).unwrap();
            let destination = target + (page - old);
            self.install_mapping(destination, mapping)
                .map_err(|_| VfsError::NoMemory)?;
        }
        let moved_regions: Vec<(u64, LazyRegion)> = self
            .lazy
            .range(..old + moved_len)
            .filter(|(_, region)| region.end > old)
            .map(|(&key, region)| (key, region.clone()))
            .collect();
        self.lazy_carve(old, old_end);
        for (key, region) in moved_regions {
            let start = key.max(old);
            let end = region.end.min(old + moved_len);
            if end <= start {
                continue;
            }
            let mut copy = region;
            copy.offset += start - key;
            copy.end = target + (end - old);
            self.lazy.insert(target + (start - old), copy);
        }
        if moved_len < old_size {
            self.unmap(old + moved_len, old_size - moved_len)?;
        }
        if new_size > old_size {
            let (start, template) = &tail_template;
            let offset = template.offset + (old_end - *start);
            self.map_lazy(
                target + old_size,
                new_size - old_size,
                template.prot,
                template.max_prot,
                template.file.clone(),
                offset,
                template.shared,
            )?;
        }
        Ok(target)
    }

    /// Whether a shared file mapping already exists for `(file, range)`; used by
    /// `mremap(old_size = 0)`, which duplicates a shared mapping.
    pub fn duplicate_shared(
        &mut self,
        old: u64,
        new_size: u64,
        fixed: Option<u64>,
    ) -> Result<u64, VfsError> {
        let (file, offset, prot, max_prot) = if let Some((start, region)) = self.lazy_region(old) {
            if !region.shared {
                return Err(VfsError::InvalidArgument);
            }
            (
                region.file.clone().ok_or(VfsError::InvalidArgument)?,
                region.offset + (old - start),
                region.prot,
                region.max_prot,
            )
        } else {
            return Err(VfsError::InvalidArgument);
        };
        let target = match fixed {
            Some(target) => {
                self.unmap(target, new_size)?;
                target
            }
            None => self.find_free(0, new_size)?,
        };
        self.map_lazy(target, new_size, prot, max_prot, Some(file), offset, true)?;
        Ok(target)
    }

    fn cow_break_inner(&mut self, address: u64) -> Result<bool, VfsError> {
        let page = address & !4095;
        let Some(mapping) = self.mapped_frames.get(&page) else {
            return Err(VfsError::Fault);
        };
        if !mapping.cow || mapping.prot & PROT_WRITE == 0 {
            return Ok(false);
        }
        let old = mapping.physical;
        let prot = mapping.prot;
        let references = pmm::refcount(old).map_err(|_| VfsError::IoError)?;
        let leaf = self.leaf(page, false).map_err(|_| VfsError::Fault)?;
        if references == 1 {
            // The other sharers exited; reuse the frame and restore write.
            // SAFETY: the leaf is present and owned by this address space.
            unsafe { leaf.write_volatile(user_leaf(old, prot)) };
            if let Some(mapping) = self.mapped_frames.get_mut(&page) {
                mapping.cow = false;
            }
            arch::flush_page(page);
            return Ok(true);
        }
        let fresh = pmm::alloc(1).ok_or(VfsError::NoMemory)?;
        // SAFETY: both frames are exclusively referenced for the duration of
        // the copy; the old frame stays readable until the PTE is replaced.
        unsafe {
            core::ptr::copy_nonoverlapping(
                pmm::phys_to_virt(old) as *const u8,
                pmm::phys_to_virt(fresh) as *mut u8,
                PAGE_SIZE as usize,
            );
        }
        if pmm::release(old, 1).is_err() {
            let _ = pmm::free(fresh, 1);
            return Err(VfsError::IoError);
        }
        // SAFETY: as above; the new frame is now owned by this mapping.
        unsafe { leaf.write_volatile(user_leaf(fresh, prot)) };
        if let Some(mapping) = self.mapped_frames.get_mut(&page) {
            mapping.physical = fresh;
            mapping.kernel = pmm::phys_to_virt(fresh) as u64;
            mapping.owned = true;
            mapping.cow = false;
        }
        arch::flush_page(page);
        Ok(true)
    }

    fn ensure_writable(&mut self, address: u64, length: usize) -> Result<(), VfsError> {
        if length == 0 {
            return Ok(());
        }
        let end = address.checked_add(length as u64).ok_or(VfsError::Fault)?;
        let mut page = address & !4095;
        while page < end {
            self.cow_break(page)?;
            page += PAGE_SIZE;
        }
        Ok(())
    }

    /// First page in `[address, address + length)` that is not yet resident.
    pub fn first_absent(&self, address: u64, length: usize) -> Option<u64> {
        if length == 0 {
            return None;
        }
        let end = address.saturating_add(length as u64);
        let mut page = address & !4095;
        while page < end {
            if !self.mapped_frames.contains_key(&page) {
                return Some(page);
            }
            page += PAGE_SIZE;
        }
        None
    }

    pub fn root_physical(&self) -> u64 {
        self.root_phys
    }

    /// Activate this address space on the current CPU.
    pub fn activate(&self) {
        Self::activate_root(self.root_phys);
    }

    /// Activate `space` without taking its lock. The context switch path must
    /// not wait for a lock whose holder may need this CPU to make progress, and
    /// the root table never changes after the space is created.
    pub fn activate_shared(space: &Mutex<Self>) {
        // SAFETY: `root_phys` is immutable after construction, the pointer comes
        // from a live lock, and no reference to the protected value is formed.
        let root = unsafe { core::ptr::addr_of!((*space.as_ptr()).root_phys).read() };
        Self::activate_root(root);
    }

    fn activate_root(root: u64) {
        #[cfg(target_arch = "x86_64")]
        {
            arch::set_page_root(root);
        }
        #[cfg(target_arch = "aarch64")]
        {
            // On AArch64, user addresses (lower half) are translated via TTBR0_EL1
            unsafe {
                let mut tcr: u64;
                core::arch::asm!("mrs {}, tcr_el1", out(reg) tcr, options(nomem, nostack));
                let t1sz = (tcr >> 16) & 0x3f;
                tcr &=
                    !(0x3f | (1 << 7) | (0b11 << 8) | (0b11 << 10) | (0b11 << 12) | (0b11 << 14));
                tcr |= t1sz | (0b01 << 8) | (0b01 << 10) | (0b11 << 12);
                core::arch::asm!(
                    "dsb ishst",
                    "msr tcr_el1, {tcr}",
                    "msr ttbr0_el1, {root}",
                    "isb",
                    "tlbi vmalle1is",
                    "dsb ish",
                    "isb",
                    tcr = in(reg) tcr,
                    root = in(reg) root,
                    options(nostack)
                );
            }
        }
        #[cfg(target_arch = "riscv64")]
        {
            arch::set_page_root(root);
        }
    }

    /// Restore the kernel-only page root on the current CPU.
    pub fn deactivate() {
        if let Some(_kernel_root) = vmm::root() {
            #[cfg(target_arch = "x86_64")]
            {
                arch::set_page_root(_kernel_root);
            }
            #[cfg(target_arch = "aarch64")]
            {
                unsafe {
                    core::arch::asm!(
                        "dsb ishst",
                        "msr ttbr0_el1, {zero}",
                        "isb",
                        "tlbi vmalle1is",
                        "dsb ish",
                        "isb",
                        zero = in(reg) 0u64,
                        options(nostack)
                    );
                }
            }
            #[cfg(target_arch = "riscv64")]
            {
                arch::set_page_root(_kernel_root);
            }
        }
    }

    /// Maximum exclusive user address. Upper-half kernel tables are never walked.
    pub fn user_limit() -> u64 {
        1u64 << (arch::paging_geometry().virtual_bits - 1)
    }

    pub fn valid_range(address: u64, length: usize) -> bool {
        address >= PAGE_SIZE
            && address
                .checked_add(length as u64)
                .is_some_and(|end| end <= Self::user_limit())
    }

    fn leaf(&mut self, address: u64, create: bool) -> Result<*mut u64, &'static str> {
        if address % PAGE_SIZE != 0 || !Self::valid_range(address, PAGE_SIZE as usize) {
            return Err("invalid user page address");
        }
        let mut table = self.root_phys;
        for level in (1..arch::paging_geometry().levels).rev() {
            let index = ((address >> (12 + level * 9)) & 511) as usize;
            // SAFETY: table is an owned page-table frame, index is within 512 entries.
            let ptr = unsafe { (pmm::phys_to_virt(table) as *mut u64).add(index) };
            let value = unsafe { ptr.read_volatile() };
            if arch::pte_present(value) {
                if !arch::pte_is_table(value, level) {
                    return Err("unexpected user huge page");
                }
                table = arch::pte_phys(value);
            } else if create {
                let frame = pmm::alloc(1).ok_or("out of memory for page table")?;
                // SAFETY: newly allocated frame, not yet published to hardware.
                unsafe { core::ptr::write_bytes(pmm::phys_to_virt(frame) as *mut u8, 0, 4096) };
                self.allocated_tables.push(frame);
                unsafe { ptr.write_volatile(make_user_table_entry(frame)) };
                table = frame;
            } else {
                return Err("missing user page table");
            }
        }
        // SAFETY: final table has 512 entries; address was validated above.
        Ok(unsafe { (pmm::phys_to_virt(table) as *mut u64).add(((address >> 12) & 511) as usize) })
    }

    /// Takes ownership of `physical` only on success. Never overwrites a mapping.
    pub fn map_page(
        &mut self,
        address: u64,
        physical: u64,
        writable: bool,
        executable: bool,
    ) -> Result<(), &'static str> {
        self.install(
            address,
            physical,
            PROT_READ
                | if writable { PROT_WRITE } else { 0 }
                | if executable { PROT_EXEC } else { 0 },
            true,
            None,
        )
    }

    fn install(
        &mut self,
        address: u64,
        physical: u64,
        prot: u32,
        owned: bool,
        owner: Option<Arc<dyn Inode>>,
    ) -> Result<(), &'static str> {
        let mapping = Mapping {
            physical,
            kernel: pmm::phys_to_virt(physical) as u64,
            uncached: false,
            prot,
            max_prot: 7,
            owned,
            cow: false,
            owner,
        };
        self.install_mapping(address, mapping)
    }

    fn install_mapping(&mut self, address: u64, mapping: Mapping) -> Result<(), &'static str> {
        if mapping.physical % PAGE_SIZE != 0
            || !arch::paging_geometry().valid_physical(mapping.physical)
            || mapping.prot & !7 != 0
            || self.mapped_frames.contains_key(&address)
        {
            return Err("invalid or overlapping mapping");
        }
        let leaf = self.leaf(address, true)?;
        let pte_prot = if mapping.cow {
            mapping.prot & !PROT_WRITE
        } else {
            mapping.prot
        };
        let value = if mapping.uncached {
            external_leaf(mapping.physical, pte_prot, true)
        } else {
            user_leaf(mapping.physical, pte_prot)
        };
        // SAFETY: exclusively owned leaf; frame is retained in mapped_frames.
        unsafe { leaf.write_volatile(value) };
        self.mapped_frames.insert(address, mapping);
        arch::flush_page(address);
        Ok(())
    }

    /// Retain the backing inode until all corresponding user pages are unmapped.
    pub fn map_external(
        &mut self,
        address: u64,
        physical: u64,
        prot: u32,
        owner: Arc<dyn Inode>,
    ) -> Result<(), &'static str> {
        self.install(address, physical, prot, false, Some(owner))
    }

    /// Whether this exact page is resident; used by shared fault handlers.
    pub fn page_present(&self, page: u64) -> bool {
        self.mapped_frames.contains_key(&page)
    }

    /// Protection of the region covering `page`, if it is still reserved.
    pub fn lazy_prot(&self, page: u64) -> Option<u32> {
        self.lazy_region(page).map(|(_, region)| region.prot)
    }

    pub fn map_external_at(
        &mut self,
        address: u64,
        physical: u64,
        kernel: u64,
        prot: u32,
        owner: Arc<dyn Inode>,
        uncached: bool,
    ) -> Result<(), &'static str> {
        self.install(address, physical, prot, false, Some(owner))?;
        let mapping = self.mapped_frames.get_mut(&address).unwrap();
        mapping.kernel = kernel;
        mapping.uncached = uncached;
        let leaf = self.leaf(address, false)?;
        let value = external_leaf(physical, prot, uncached);
        unsafe { leaf.write_volatile(value) };
        arch::flush_page(address);
        Ok(())
    }

    pub fn contains(&self, address: u64) -> bool {
        let page = address & !4095;
        self.mapped_frames.contains_key(&page) || self.lazy_region(page).is_some()
    }

    pub fn find_free(&self, hint: u64, length: u64) -> Result<u64, VfsError> {
        let length = align_up(length).ok_or(VfsError::InvalidArgument)?;
        if length == 0 || length > MAX_MAPPING {
            return Err(VfsError::NoSpace);
        }
        let mut address = align_up(hint.max(0x4000_0000)).ok_or(VfsError::NoSpace)?;
        loop {
            let end = address.checked_add(length).ok_or(VfsError::NoSpace)?;
            if !Self::valid_range(address, length as usize) {
                return Err(VfsError::NoSpace);
            }
            // Jump past whichever object overlaps the candidate range.
            let mut next = None;
            if let Some((_, region)) = self
                .lazy
                .range(..end)
                .next_back()
                .filter(|(_, region)| region.end > address)
            {
                next = Some(region.end);
            }
            if let Some(mut page) = self
                .mapped_frames
                .range(address..end)
                .next_back()
                .map(|(&p, _)| p)
            {
                page += PAGE_SIZE;
                next = Some(next.map_or(page, |value: u64| value.max(page)));
            }
            match next {
                Some(value) => address = value,
                None => return Ok(address),
            }
        }
    }

    pub fn map_zeroed(&mut self, address: u64, length: u64, prot: u32) -> Result<(), &'static str> {
        let length = align_up(length).ok_or("mapping size overflow")?;
        if length == 0
            || length > MAX_MAPPING
            || prot & !7 != 0
            || address & 4095 != 0
            || !Self::valid_range(address, length as usize)
            || self.range_in_use(address, address + length)
        {
            return Err("invalid or overlapping user mapping");
        }
        let mut done = 0;
        while done < length {
            let Some(frame) = pmm::alloc(1) else {
                let _ = self.unmap(address, done);
                return Err("out of memory for user data");
            };
            // SAFETY: unpublished fresh frame, exactly one page.
            unsafe { core::ptr::write_bytes(pmm::phys_to_virt(frame) as *mut u8, 0, 4096) };
            if let Err(e) = self.install(address + done, frame, prot, true, None) {
                let _ = pmm::free(frame, 1);
                let _ = self.unmap(address, done);
                return Err(e);
            }
            done += PAGE_SIZE;
        }
        Ok(())
    }

    /// Validate the entire range before copying; the caller holds the space lock.
    pub fn validate(&self, address: u64, length: usize, write: bool) -> Result<(), VfsError> {
        if length == 0 {
            return Ok(());
        }
        if !Self::valid_range(address, length) {
            return Err(VfsError::Fault);
        }
        let end = address + length as u64;
        let mut page = address & !4095;
        while page < end {
            let m = self.mapped_frames.get(&page).ok_or(VfsError::Fault)?;
            if m.prot & if write { PROT_WRITE } else { PROT_READ } == 0 {
                return Err(VfsError::Fault);
            }
            page += PAGE_SIZE;
        }
        Ok(())
    }

    pub fn read_bytes(&self, address: u64, dst: &mut [u8]) -> Result<(), VfsError> {
        self.validate(address, dst.len(), false)?;
        self.copy_pages(address, dst.as_mut_ptr(), dst.len(), false)
    }

    pub fn write_bytes(&mut self, address: u64, src: &[u8]) -> Result<(), VfsError> {
        self.ensure_writable(address, src.len())?;
        self.validate(address, src.len(), true)?;
        self.copy_pages(address, src.as_ptr() as *mut u8, src.len(), true)
    }

    /// Loader-only initialization through HHDM, without making RX mappings writable.
    pub fn initialize(&self, address: u64, src: &[u8]) -> Result<(), VfsError> {
        if !Self::valid_range(address, src.len()) {
            return Err(VfsError::Fault);
        }
        self.copy_pages(address, src.as_ptr() as *mut u8, src.len(), true)
    }

    fn copy_pages(
        &self,
        mut address: u64,
        data: *mut u8,
        length: usize,
        write: bool,
    ) -> Result<(), VfsError> {
        let mut done = 0;
        while done < length {
            let m = self
                .mapped_frames
                .get(&(address & !4095))
                .ok_or(VfsError::Fault)?;
            let offset = address as usize & 4095;
            let count = (4096 - offset).min(length - done);
            // SAFETY: each physical page is retained by this locked address space;
            // source/destination is a kernel slice and page bounds are checked.
            unsafe {
                let page = (m.kernel as *mut u8).add(offset);
                if write {
                    core::ptr::copy_nonoverlapping(data.add(done), page, count);
                } else {
                    core::ptr::copy_nonoverlapping(page, data.add(done), count);
                }
            }
            address += count as u64;
            done += count;
        }
        Ok(())
    }

    pub fn unmap(&mut self, address: u64, length: u64) -> Result<(), VfsError> {
        let length = align_up(length).ok_or(VfsError::InvalidArgument)?;
        if address & 4095 != 0 || !Self::valid_range(address, length as usize) {
            return Err(VfsError::InvalidArgument);
        }
        self.lazy_carve(address, address + length);
        let keys: Vec<u64> = self
            .mapped_frames
            .range(address..address + length)
            .map(|(&p, _)| p)
            .collect();
        // Clear all PTEs before shooting down, and free backing only after acknowledgements.
        for &page in &keys {
            if let Ok(leaf) = self.leaf(page, false) {
                unsafe { leaf.write_volatile(0) };
            }
        }
        if !keys.is_empty() {
            arch::tlb_shootdown(address, (length / PAGE_SIZE) as usize);
        }
        for page in keys {
            if let Some(m) = self.mapped_frames.remove(&page) {
                if m.owned {
                    let _ = pmm::release(m.physical, 1);
                }
            }
        }
        Ok(())
    }

    pub fn restrict_permissions(
        &mut self,
        address: u64,
        length: u64,
        maximum: u32,
    ) -> Result<(), VfsError> {
        let end = address
            .checked_add(length)
            .ok_or(VfsError::InvalidArgument)?;
        for (&_, mapping) in self.mapped_frames.range_mut(address..end) {
            mapping.max_prot &= maximum;
        }
        for (_, region) in self.lazy.range_mut(..end) {
            if region.end > address {
                region.max_prot &= maximum;
            }
        }
        Ok(())
    }

    pub fn protect(&mut self, address: u64, length: u64, prot: u32) -> Result<(), VfsError> {
        let length = align_up(length).ok_or(VfsError::InvalidArgument)?;
        if address & 4095 != 0
            || length == 0
            || prot & !7 != 0
            || !Self::valid_range(address, length as usize)
        {
            return Err(VfsError::InvalidArgument);
        }
        let end = address + length;
        let mut cursor = address;
        while cursor < end {
            if self.mapped_frames.contains_key(&cursor) {
                cursor += PAGE_SIZE;
            } else if let Some((_, region)) = self.lazy_region(cursor) {
                cursor = region.end;
            } else {
                return Err(VfsError::NoMemory);
            }
        }
        if self
            .mapped_frames
            .range(address..end)
            .any(|(_, m)| prot & !m.max_prot != 0)
            || self
                .lazy
                .range(..end)
                .any(|(_, r)| r.end > address && prot & !r.max_prot != 0)
        {
            return Err(VfsError::PermissionDenied);
        }
        // Split reserved regions at the boundaries, then retag the middle.
        let touched: Vec<(u64, LazyRegion)> = self
            .lazy
            .range(..end)
            .filter(|(_, region)| region.end > address)
            .map(|(&key, region)| (key, region.clone()))
            .collect();
        self.lazy_carve(address, end);
        for (key, mut region) in touched {
            let start = key.max(address);
            let stop = region.end.min(end);
            region.offset += start - key;
            region.end = stop;
            region.prot = prot;
            self.lazy.insert(start, region);
        }
        let resident: Vec<u64> = self
            .mapped_frames
            .range(address..end)
            .map(|(&page, _)| page)
            .collect();
        for page in resident {
            let m = self.mapped_frames.get_mut(&page).unwrap();
            m.prot = prot;
            let pte_prot = if m.cow { prot & !PROT_WRITE } else { prot };
            let value = external_leaf(m.physical, pte_prot, m.uncached);
            let leaf = self.leaf(page, false).map_err(|_| VfsError::Fault)?;
            unsafe { leaf.write_volatile(value) };
        }
        arch::tlb_shootdown(address, (length / PAGE_SIZE) as usize);
        Ok(())
    }

    pub fn resize_brk(&mut self, requested: u64) -> u64 {
        if requested < self.brk_base || requested >= Self::user_limit() {
            return self.brk;
        }
        let Some(new_end) = align_up(requested) else {
            return self.brk;
        };
        let old_end = align_up(self.brk).unwrap();
        if new_end > old_end {
            if self
                .map_zeroed(old_end, new_end - old_end, PROT_READ | PROT_WRITE)
                .is_err()
            {
                return self.brk;
            }
        } else if new_end < old_end {
            let _ = self.unmap(new_end, old_end - new_end);
        }
        self.brk = requested;
        self.brk
    }

    pub fn maps(&self) -> Vec<(u64, u64, u32)> {
        let mut result: Vec<(u64, u64, u32)> = Vec::new();
        for (&page, m) in &self.mapped_frames {
            if self.lazy_region(page).is_some() {
                continue;
            }
            if let Some(last) = result.last_mut() {
                if last.1 == page && last.2 == m.prot {
                    last.1 += PAGE_SIZE;
                    continue;
                }
            }
            result.push((page, page + PAGE_SIZE, m.prot));
        }
        for (&start, region) in &self.lazy {
            result.push((start, region.end, region.prot));
        }
        result.sort_unstable_by_key(|entry| entry.0);
        result
    }

    /// `(start, end, prot, shared, rdev, ino)` for every mapping, as shown in
    /// `/proc/<pid>/maps`. Takes the lock itself because a filesystem's `metadata`
    /// may sleep and so has to run after it is released.
    pub fn map_entries(space: &Mutex<Self>) -> Vec<(u64, u64, u32, bool, u64, u64)> {
        let raw = space.lock().raw_map_entries();
        let mut result: Vec<(u64, u64, u32, bool, u64, u64)> = Vec::with_capacity(raw.len());
        for (start, end, prot, shared, file) in raw {
            let (rdev, ino) = file
                .and_then(|inode| inode.metadata().ok())
                .map_or((0, 0), |stat| (stat.rdev, stat.ino));
            result.push((start, end, prot, shared, rdev, ino));
        }
        result
    }

    fn raw_map_entries(&self) -> Vec<(u64, u64, u32, bool, Option<Arc<dyn Inode>>)> {
        let mut result: Vec<(u64, u64, u32, bool, Option<Arc<dyn Inode>>)> = Vec::new();
        for (&start, region) in &self.lazy {
            result.push((start, region.end, region.prot, region.shared, region.file.clone()));
        }
        for (&page, m) in &self.mapped_frames {
            if self.lazy_region(page).is_some() {
                continue;
            }
            let shared = !m.owned;
            let same_owner = |other: &Option<Arc<dyn Inode>>| match (other, &m.owner) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if let Some(last) = result.last_mut()
                && last.1 == page
                && last.2 == m.prot
                && last.3 == shared
                && same_owner(&last.4)
            {
                last.1 += PAGE_SIZE;
                continue;
            }
            result.push((page, page + PAGE_SIZE, m.prot, shared, m.owner.clone()));
        }
        result.sort_unstable_by_key(|entry| entry.0);
        result
    }

    /// Futex identity: address-space root plus virtual address.  Threads that
    /// share this space share the key.
    pub fn futex_key(&self, address: u64) -> Option<(u64, u64)> {
        self.contains(address).then_some((self.root_phys, address))
    }

    /// Reserved or resident bytes, as reported by `/proc/<pid>/statm` size.
    pub fn virtual_pages(&self) -> usize {
        let reserved: u64 = self
            .lazy
            .iter()
            .map(|(&start, region)| (region.end - start) / PAGE_SIZE)
            .sum();
        reserved as usize
            + self
                .mapped_frames
                .keys()
                .filter(|&&page| self.lazy_region(page).is_none())
                .count()
    }

    pub fn resident_pages(&self) -> usize {
        self.mapped_frames.len()
    }
}

impl Drop for UserAddressSpace {
    fn drop(&mut self) {
        // Exec and fatal signals leave without shmdt, as does a plain exit.
        crate::ipc::shm::release_address_space(self.root_phys);
        // Drop one reference from privately owned frames; COW pages may
        // still be mapped by another address space.
        for m in self.mapped_frames.values() {
            if m.owned {
                let _ = pmm::release(m.physical, 1);
            }
        }
        // Free intermediate page table frames
        for &frame in &self.allocated_tables {
            let _ = pmm::free(frame, 1);
        }
        // Free root page table frame
        let _ = pmm::free(self.root_phys, 1);
    }
}

fn make_user_table_entry(physical: u64) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // Present (bit 0) | Writable (bit 1) | User (bit 2)
        physical | 0x07
    }
    #[cfg(target_arch = "aarch64")]
    {
        // Table descriptor: bit 0 and bit 1 set (0b11)
        physical | 0b11
    }
    #[cfg(target_arch = "riscv64")]
    {
        // Pointer to next-level page table: V=1 (bit 0)
        ((physical >> 12) << 10) | 0x01
    }
}

fn make_user_leaf_entry(physical: u64, writable: bool, executable: bool) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        let mut entry = physical | 0x05; // Present (1) | User (4)
        if writable {
            entry |= 0x02;
        }
        if !executable && crate::cpuid::has(crate::cpuid::Feature::Nx) {
            entry |= 1u64 << 63;
        }
        entry
    }
    #[cfg(target_arch = "aarch64")]
    {
        // Page descriptor: 0b11, AF=1 (bit 10), IS=0b11 (bits 9:8), AP[1]=1 for EL0 (bit 6)
        let mut entry = physical | 0b11 | (1 << 10) | (0b11 << 8) | (1 << 6) | (1 << 53);
        if !writable {
            entry |= 1 << 7; // AP[2] = 1 (read-only)
        }
        if !executable {
            entry |= (1 << 53) | (1 << 54); // PXN | UXN
        }
        entry
    }
    #[cfg(target_arch = "riscv64")]
    {
        // V(1), R(2), U(16), A(64), D(128)
        let mut entry = ((physical >> 12) << 10) | 0xc3 | 0x10;
        if writable {
            entry |= 0x04;
        }
        if executable {
            entry |= 0x08;
        }
        entry
    }
}

fn user_leaf(physical: u64, prot: u32) -> u64 {
    if prot == 0 {
        return 0;
    }
    #[allow(unused_mut)]
    let mut value = make_user_leaf_entry(physical, prot & PROT_WRITE != 0, prot & PROT_EXEC != 0);
    #[cfg(target_arch = "riscv64")]
    if prot & (PROT_READ | PROT_WRITE) == 0 {
        value &= !2;
    }
    value
}

pub fn align_up(value: u64) -> Option<u64> {
    value.checked_add(4095).map(|n| n & !4095)
}

fn external_leaf(physical: u64, prot: u32, uncached: bool) -> u64 {
    let value = user_leaf(physical, prot);
    if !uncached || prot == 0 {
        return value;
    }
    #[cfg(target_arch = "x86_64")]
    {
        return value | 0x18;
    }
    #[cfg(target_arch = "aarch64")]
    {
        return (value & !(7 << 2)) | (3 << 2);
    }
    #[cfg(target_arch = "riscv64")]
    {
        value
    }
}
