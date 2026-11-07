/*
 *
 *       src/mm/dma.rs
 *       Coherent DMA allocation and streaming address translation
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The kernel maps all of physical memory through the HHDM, and every
//! supported target either is cache-coherent for device DMA (x86_64) or boots
//! with the firmware-described coherent attribute (aarch64, riscv64 on QEMU
//! virt). Coherent allocations therefore return the direct-map virtual address
//! together with the physical address the device programs.
//!
//! Streaming mappings translate an existing kernel pointer; they validate that
//! the range is mapped and return its physical address. No cache maintenance
//! is required under the coherence assumptions above; the helpers exist so the
//! translation, not the cache policy, is the one place drivers depend on.

use super::{pmm, vmm};
use core::sync::atomic::{AtomicU64, Ordering};

pub const PAGE_SIZE: usize = 4096;

/// A coherent allocation: CPU and device see the same bytes with no
/// explicit cache maintenance.
pub struct Coherent {
    /// Start of the physical range; page aligned.
    physical_base: u64,
    /// Direct-map virtual address corresponding to `physical_base`.
    virtual_base: usize,
    /// Address handed to the device (may be offset inside the allocation when
    /// the caller demanded stronger alignment than a page).
    device_address: u64,
    /// Page count obtained from the PMM, including alignment padding.
    pages: usize,
    size: usize,
}

impl Coherent {
    /// Direct-map pointer to the first usable byte.
    pub fn as_ptr<T>(&self) -> *mut T {
        (self.virtual_base + (self.device_address - self.physical_base) as usize) as *mut T
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: the allocation owns `size` zeroed bytes and lives as long as
        // `self`; the pointer is in the direct map.
        unsafe { core::slice::from_raw_parts(self.as_ptr(), self.size) }
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above; `&mut self` proves exclusive ownership.
        unsafe { core::slice::from_raw_parts_mut(self.as_ptr(), self.size) }
    }

    /// Address the device programs into a descriptor.
    pub fn dma_address(&self) -> u64 {
        self.device_address
    }

    pub fn size(&self) -> usize {
        self.size
    }
}

impl Drop for Coherent {
    fn drop(&mut self) {
        COHERENT_BYTES.fetch_sub((self.pages * PAGE_SIZE) as u64, Ordering::AcqRel);
        let _ = pmm::free(self.physical_base, self.pages);
    }
}

static COHERENT_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes currently owned by coherent allocations, for leak checks.
pub fn coherent_bytes() -> u64 {
    COHERENT_BYTES.load(Ordering::Acquire)
}

fn pages_for(size: usize, align: usize) -> Option<usize> {
    if size == 0 {
        return None;
    }
    let extra = align.saturating_sub(PAGE_SIZE) / PAGE_SIZE;
    let pages = size.checked_add(PAGE_SIZE - 1)?.checked_div(PAGE_SIZE)?;
    pages.checked_add(extra)
}

/// `dma_alloc_coherent`: page-backed, zeroed, aligned to at least `align`
/// bytes (a page by default) and never crossing a 64 KiB boundary if `align`
/// is 64 KiB.
pub fn alloc_coherent(size: usize, align: usize) -> Option<Coherent> {
    let align = align.max(PAGE_SIZE).next_power_of_two();
    let pages = pages_for(size, align)?;
    let physical_base = pmm::alloc(pages)?;
    let aligned = (physical_base + align as u64 - 1) & !(align as u64 - 1);
    let virtual_base = pmm::phys_to_virt(physical_base);
    // SAFETY: the PMM handed us `pages` writable frames.
    unsafe {
        core::ptr::write_bytes(virtual_base as *mut u8, 0, pages * PAGE_SIZE);
    }
    COHERENT_BYTES.fetch_add((pages * PAGE_SIZE) as u64, Ordering::AcqRel);
    Some(Coherent {
        physical_base,
        virtual_base,
        device_address: aligned,
        pages,
        size,
    })
}

/// Translate a virtual address to the physical address a device programs.
/// Handles the three mappings the kernel uses: the kernel image, VMM regions
/// and the HHDM direct map.
pub fn virt_to_phys(address: u64) -> Option<u64> {
    let kernel_virtual = crate::boot::kernel_virtual();
    if kernel_virtual != 0 && address >= kernel_virtual {
        return Some(crate::boot::kernel_physical() + (address - kernel_virtual));
    }
    if let Some(physical) = vmm::translate(address) {
        return Some(physical);
    }
    let hhdm = pmm::hhdm() as u64;
    if address >= hhdm {
        let physical = address - hhdm;
        if physical < (pmm::stats().frames * PAGE_SIZE) as u64 {
            return Some(physical);
        }
    }
    None
}

/// Translate a kernel pointer range to a device-visible address
/// (`dma_map_single`). Returns `None` when the range is not fully mapped or not
/// physically contiguous.
pub fn map_single(pointer: *const u8, length: usize) -> Option<u64> {
    if pointer.is_null() || length == 0 {
        return None;
    }
    let start = pointer as u64;
    let end = start.checked_add(length as u64 - 1)?;
    let physical = virt_to_phys(start)?;
    let last = virt_to_phys(end)?;
    (physical.checked_add(end - start)? == last).then_some(physical)
}

/// `dma_unmap_single` has nothing to undo without bounce buffering.
pub fn unmap_single(_device_address: u64, _length: usize) {}

#[cfg(feature = "boot-self-test")]
pub mod self_test {
    use super::*;

    pub fn run() {
        let allocation = alloc_coherent(8192, 65536).expect("dma alloc");
        assert_eq!(allocation.dma_address() % 65536, 0);
        assert!(allocation.as_bytes().iter().all(|byte| *byte == 0));
        assert_eq!(allocation.size(), 8192);
        let address = allocation.dma_address();
        // Coherent memory lives in the direct map; its device address is the
        // physical address, verified through the pointer walk used by drivers.
        assert_eq!(
            crate::mm::pmm::phys_to_virt(address) as *const u8,
            allocation.as_ptr::<u8>()
        );
        drop(allocation);
        let buffer = alloc::vec![0u8; 64];
        assert_eq!(
            map_single(buffer.as_ptr(), buffer.len()),
            Some(virt_to_phys(buffer.as_ptr() as u64).expect("mapped"))
        );
        unmap_single(
            map_single(buffer.as_ptr(), buffer.len()).unwrap(),
            buffer.len(),
        );
        assert!(alloc_coherent(0, 16).is_none());
    }
}
