/*
 *
 *       src/arch/riscv64/paging.rs
 *       RISC-V page-table descriptors, root activation and TLB maintenance
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::arch::asm;

use super::super::paging::KernelProt;

pub fn paging_geometry() -> super::super::paging::Geometry {
    let satp: u64;
    // SAFETY: this kernel runs in supervisor mode.
    unsafe { asm!("csrr {}, satp", out(reg) satp, options(nomem, nostack)) }
    let (levels, virtual_bits) = match satp >> 60 {
        8 => (3, 39),
        9 => (4, 48),
        _ => panic!("unsupported SATP paging mode"),
    };
    super::super::paging::Geometry {
        levels,
        virtual_bits,
        physical_bits: 56,
    }
}

pub fn page_root() -> u64 {
    let satp: u64;
    // SAFETY: satp is readable in S mode.
    unsafe { asm!("csrr {}, satp", out(reg) satp, options(nomem, nostack)) }
    (satp & ((1u64 << 44) - 1)) << 12
}

pub fn set_page_root(root: u64) {
    let mode = if paging_geometry().levels == 3 { 8 } else { 9 };
    assert!(root & 0xfff == 0 && root < (1 << 56));
    let satp = (mode << 60) | (root >> 12); // Preserve mode, ASID 0.
    // SAFETY: the root has the active mode's geometry; invalidate all ASIDs.
    unsafe { asm!("csrw satp, {}", "sfence.vma", in(reg) satp, options(nostack)) }
}

pub fn pte_present(entry: u64) -> bool {
    entry & 1 != 0
}
pub fn pte_is_table(entry: u64, _level: usize) -> bool {
    // Non-leaf descriptors must have R/W/X clear, including reserved W-only.
    entry & 0xf == 1
}
pub fn pte_phys(entry: u64) -> u64 {
    ((entry >> 10) & ((1u64 << 44) - 1)) << 12
}
pub fn pte_table(physical: u64) -> u64 {
    ((physical >> 12) << 10) | 1
}
pub fn pte_leaf(physical: u64, _device: bool) -> u64 {
    ((physical >> 12) << 10) | 0xc7 // V,R,W,A,D; platform PMA controls MMIO.
}

/// Kernel-only leaf with no U bit. R is always set because W without R is
/// reserved; X is granted only to executable module sections.
pub fn pte_kernel(physical: u64, prot: KernelProt) -> u64 {
    let mut entry = ((physical >> 12) << 10) | 0xc3; // V,R,A,D
    if prot.writable() {
        entry |= 0x4;
    }
    if prot.executable() {
        entry |= 0x8;
    }
    entry
}

pub fn sync_instruction_cache(_address: u64, _length: usize) {
    // SAFETY: fence.i orders prior stores before instruction fetch.
    unsafe { asm!("fence.i", options(nostack)) };
}

pub fn flush_page(virtual_address: u64) {
    // SAFETY: local hart translation invalidation after a new PTE write.
    unsafe { asm!("sfence.vma {}, x0", in(reg) virtual_address, options(nostack)) }
}

/// A non-leaf update can invalidate cached walks for every descendant address,
/// including invalid PTEs. The privileged ISA requires rs1=x0 in this case.
pub fn flush_table(_virtual_address: u64) {
    // SAFETY: synchronize this hart's walks, all addresses/ASIDs/global entries.
    unsafe { asm!("sfence.vma x0, x0", options(nostack)) }
}

pub fn tlb_shootdown(virtual_address: u64, pages: usize) {
    let size = if pages == 0 { 0 } else { pages * 4096 };
    let (err, _) = super::clint::sbi_call_ext(
        0x5246_4E43, // RFNC
        1,           // remote_sfence_vma
        usize::MAX,
        0,
        virtual_address as usize,
        size,
    );
    if err != 0 {
        // Fallback: local sfence
        unsafe { asm!("sfence.vma x0, x0", options(nostack)) };
    } else {
        unsafe { asm!("sfence.vma x0, x0", options(nostack)) };
    }
}
