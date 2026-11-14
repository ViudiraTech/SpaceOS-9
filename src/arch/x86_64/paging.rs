/*
 *
 *       src/arch/x86_64/paging.rs
 *       x86-64 page-table descriptors, root activation and TLB maintenance
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::arch::asm;

use super::super::paging::KernelProt;

const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

pub fn paging_geometry() -> super::super::paging::Geometry {
    super::super::paging::Geometry {
        levels: 4,
        virtual_bits: 48,
        physical_bits: crate::cpuid::info()
            .expect("CPU discovery required")
            .physical_bits
            .unwrap_or(36)
            .min(52) as u32,
    }
}

pub fn page_root() -> u64 {
    let cr3: u64;
    // SAFETY: kernel privilege allows reading CR3.
    unsafe { asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) }
    cr3 & ADDRESS_MASK
}

pub fn set_page_root(root: u64) {
    // SAFETY: caller supplies an initialized and mapped PML4 frame.
    unsafe { asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags)) }
}

pub fn pte_present(entry: u64) -> bool {
    entry & 1 != 0
}
pub fn pte_is_table(entry: u64, level: usize) -> bool {
    pte_present(entry) && (level == 3 || entry & (1 << 7) == 0)
}
pub fn pte_phys(entry: u64) -> u64 {
    entry & ADDRESS_MASK
}
pub fn pte_table(physical: u64) -> u64 {
    physical | 0x003
}
pub fn pte_leaf(physical: u64, device: bool) -> u64 {
    let nx = crate::cpuid::has(crate::cpuid::Feature::Nx);
    physical | 0x003 | if nx { 1 << 63 } else { 0 } | if device { 0x018 } else { 0 }
}

/// Kernel-only leaf with explicit W^X permissions. Shadow-stack and SMEP/SMAP
/// are not enabled here, so the user bit stays clear for every kernel page.
pub fn pte_kernel(physical: u64, prot: KernelProt) -> u64 {
    let base = physical | 0x001 | if prot.writable() { 0x002 } else { 0 };
    let nx = crate::cpuid::has(crate::cpuid::Feature::Nx);
    base | if nx && !prot.executable() { 1 << 63 } else { 0 }
}

/// Published instruction bytes are coherent with instruction fetch on x86-64;
/// the compiler fence orders the stores before the indirect call.
pub fn sync_instruction_cache(_address: u64, _length: usize) {
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

pub fn setup_memory_protection() {
    if !crate::cpuid::has(crate::cpuid::Feature::Nx) {
        return;
    }
    let (low, high): (u32, u32);
    // SAFETY: EFER exists in long mode; NXE is set only after CPUID validation.
    unsafe {
        asm!("rdmsr", in("ecx") 0xc000_0080u32, out("eax") low, out("edx") high, options(nomem,nostack));
        asm!("wrmsr", in("ecx") 0xc000_0080u32, in("eax") low | (1 << 11), in("edx") high, options(nostack));
    }
}

pub fn flush_page(virtual_address: u64) {
    // SAFETY: the caller has completed its page-table write.
    unsafe { asm!("invlpg [{}]", in(reg) virtual_address, options(nostack, preserves_flags)) }
}

/// INVLPG also invalidates this PCID's paging-structure caches, so it covers
/// publication of a descendant table as well as the selected leaf translation.
pub fn flush_table(virtual_address: u64) {
    flush_page(virtual_address);
}

use crate::sync::SpinLock;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub const TLB_IPI_VECTOR: u8 = 0xf2;

static TLB_LOCK: SpinLock<()> = SpinLock::new(());
static PENDING_VADDR: AtomicU64 = AtomicU64::new(0);
static PENDING_PAGES: AtomicUsize = AtomicUsize::new(0);
static ACK_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn handle_tlb_ipi(_vector: u32) {
    let vaddr = PENDING_VADDR.load(Ordering::Acquire);
    let pages = PENDING_PAGES.load(Ordering::Acquire);
    flush_local(vaddr, pages);
    ACK_COUNT.fetch_add(1, Ordering::Release);
}

fn flush_local(vaddr: u64, pages: usize) {
    if pages == 0 || pages > 32 {
        set_page_root(page_root());
    } else {
        for i in 0..pages {
            flush_page(vaddr + (i as u64) * 4096);
        }
    }
}

pub fn tlb_shootdown(virtual_address: u64, pages: usize) {
    // Acquire TLB_LOCK before masking local interrupts so CPUs spinning on
    // TLB_LOCK keep interrupts enabled and still service peer TLB IPIs.
    let _guard = TLB_LOCK.lock();
    PENDING_VADDR.store(virtual_address, Ordering::Release);
    PENDING_PAGES.store(pages, Ordering::Release);
    ACK_COUNT.store(0, Ordering::Release);

    let _online = crate::smp::online_count();
    let current = crate::smp::current_cpu();
    let mut sent = 0;

    for cpu in 0..crate::boot::cpu_count() {
        if cpu != current && crate::smp::online(cpu) {
            if let Some(hw_id) = crate::boot::cpu_hardware_id(cpu) {
                if super::apic::send_fixed_ipi(hw_id as u32, TLB_IPI_VECTOR).is_ok() {
                    sent += 1;
                }
            }
        }
    }

    flush_local(virtual_address, pages);

    if sent > 0 {
        let start = crate::time::uptime_micros();
        while ACK_COUNT.load(Ordering::Acquire) < sent {
            if crate::time::uptime_micros().saturating_sub(start) > 50_000 {
                break;
            }
            core::hint::spin_loop();
        }
    }
}
