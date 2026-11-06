/*
 *
 *       src/arch/aarch64/paging.rs
 *       AArch64 page-table descriptors, root activation and TLB maintenance
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::arch::asm;

use super::super::paging::KernelProt;

const ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;

pub fn paging_geometry() -> super::super::paging::Geometry {
    super::super::paging::Geometry {
        levels: 4,
        virtual_bits: 48,
        physical_bits: crate::cpuid::info()
            .expect("CPU discovery required")
            .physical_bits
            .unwrap_or(32)
            .min(48) as u32,
    }
}

pub fn page_root() -> u64 {
    let root: u64;
    // SAFETY: Limine enters with TTBR1_EL1 configured and readable.
    unsafe { asm!("mrs {}, ttbr1_el1", out(reg) root, options(nomem, nostack)) }
    root & ADDRESS_MASK
}

pub fn set_page_root(root: u64) {
    // SAFETY: caller supplies a valid root copied from the active table.
    unsafe {
        asm!("dsb ishst", "msr ttbr1_el1, {}", "isb", "tlbi vmalle1is", "dsb ish", "isb", in(reg) root, options(nostack))
    }
}

pub fn pte_present(entry: u64) -> bool {
    entry & 1 != 0
}
pub fn pte_is_table(entry: u64, _level: usize) -> bool {
    entry & 3 == 3
}
pub fn pte_phys(entry: u64) -> u64 {
    entry & ADDRESS_MASK
}
pub fn pte_table(physical: u64) -> u64 {
    physical | 0b11
}
pub fn pte_leaf(physical: u64, device: bool) -> u64 {
    // Limine rev 6 fixes Attr0 to Normal WB; Attr2 is configured as Device.
    physical
        | 0b11
        | (1 << 10)
        | (0b11 << 8)
        | (1 << 53)
        | (1 << 54)
        | if device { 2 << 2 } else { 0 }
}

/// Kernel-only Normal WB leaf. UXN stays set, PXN is clear only for executable
/// sections, and AP[2] selects read-only without permitting EL0 access.
pub fn pte_kernel(physical: u64, prot: KernelProt) -> u64 {
    physical
        | 0b11
        | (1 << 10)
        | (0b11 << 8)
        | (1 << 54)
        | if prot.writable() { 0 } else { 1 << 7 }
        | if prot.executable() { 0 } else { 1 << 53 }
}

/// Make freshly written module text visible to instruction fetch: clean data
/// cache to the point of unification, then invalidate the matching I-cache.
pub fn sync_instruction_cache(address: u64, length: usize) {
    if length == 0 {
        return;
    }
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL1 and describes the cache geometry.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let dline = 4u64 << ((ctr >> 16) & 0xf);
    let iline = 4u64 << (ctr & 0xf);
    let end = address.saturating_add(length as u64);
    let mut cursor = address & !(dline - 1);
    while cursor < end {
        // SAFETY: the range is mapped Normal memory owned by the module image.
        unsafe { asm!("dc cvau, {}", in(reg) cursor, options(nostack)) };
        cursor += dline;
    }
    // SAFETY: required barrier between data cleaning and instruction invalidation.
    unsafe { asm!("dsb ish", options(nostack)) };
    let mut cursor = address & !(iline - 1);
    while cursor < end {
        // SAFETY: the range is mapped Normal memory owned by the module image.
        unsafe { asm!("ic ivau, {}", in(reg) cursor, options(nostack)) };
        cursor += iline;
    }
    // SAFETY: complete invalidation before the module entry is called.
    unsafe { asm!("dsb ish", "isb", options(nostack)) };
}

pub fn setup_device_memory() {
    let mut mair: u64;
    // SAFETY: Limine rev 6 leaves Attr2 unused, and the caller runs before
    // any kernel device mapping uses that index.
    unsafe {
        asm!("mrs {}, mair_el1", out(reg) mair, options(nomem, nostack));
        mair &= !((0xff << 16) | (0xff << 24));
        mair |= 0x44 << 24;
        asm!("msr mair_el1, {}", "isb", in(reg) mair, options(nostack));
    }
}

pub fn flush_page(virtual_address: u64) {
    let page = virtual_address >> 12;
    // SAFETY: required barriers surround invalidation of the new mapping.
    unsafe {
        asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb", in(reg) page, options(nostack))
    }
}

/// Complete publication of a new descendant table at every cached walk level.
pub fn flush_table(_virtual_address: u64) {
    // SAFETY: EL1 context invalidation, ordered after the parent descriptor.
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            options(nostack)
        )
    }
}

pub fn tlb_shootdown(virtual_address: u64, pages: usize) {
    if pages == 0 || pages > 32 {
        flush_table(virtual_address);
    } else {
        for i in 0..pages {
            flush_page(virtual_address + (i as u64) * 4096);
        }
    }
}
