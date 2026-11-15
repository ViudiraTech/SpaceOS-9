/*
 *
 *       src/cpuid/mod.rs
 *       Architecture-independent CPU identity and hardware capability discovery
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Architecture-independent CPU identity and hardware capability discovery.
//!
//! A hardware feature bit does not grant permission to execute an instruction:
//! extended register state must also be enabled and managed by `crate::fpu`.

use crate::arch;
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(target_arch = "x86_64")]
pub use crate::arch::cpuid::{Registers, query as query_x86};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Architecture {
    X86_64,
    Aarch64,
    Riscv64,
}

/// Stable, semantic capability names. Architecture-specific ISA names remain
/// explicit; e.g. AdvSIMD is never advertised as x86 SSE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Feature {
    Fpu,
    Simd,
    DoublePrecision,
    Fxsave,
    Xsave,
    Xsaveopt,
    Mmx,
    Sse,
    Sse2,
    Sse3,
    Ssse3,
    Sse41,
    Sse42,
    Sse4a,
    Avx,
    Avx2,
    Fma,
    F16c,
    Avx512f,
    Avx512dq,
    Avx512cd,
    Avx512bw,
    Avx512vl,
    Aes,
    Pclmul,
    Sha,
    Crc32,
    Atomics,
    Bmi1,
    Bmi2,
    Popcnt,
    Rdrand,
    Rdseed,
    Nx,
    Page1g,
    Apic,
    X2apic,
    Tsc,
    Rdtscp,
    InvariantTsc,
    TscDeadline,
    Hypervisor,
    Smap,
    Smep,
    Pcid,
    Invpcid,
    La57,
    AdvSimd,
    Sve,
    RiscvI,
    RiscvM,
    RiscvA,
    RiscvC,
    RiscvF,
    RiscvD,
    RiscvV,
    /// RISC-V Zkr: entropy source `seed` CSR.
    RiscvZkr,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features(u64);

impl Features {
    pub const fn contains(self, feature: Feature) -> bool {
        self.0 & (1 << feature as u8) != 0
    }
    pub fn insert(&mut self, feature: Feature) {
        self.0 |= 1 << feature as u8;
    }
    pub fn contains_all(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }
}

/// Bounded identity text: no allocation or borrowed firmware lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Text<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Default for Text<N> {
    fn default() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }
}

impl<const N: usize> Text<N> {
    pub fn set(&mut self, source: &[u8]) {
        self.bytes.fill(0);
        let source = source.split(|b| *b == 0).next().unwrap_or_default();
        self.len = source.len().min(N);
        // Firmware/CPUID strings are not trusted UTF-8. Normalize to ASCII.
        for (to, from) in self.bytes[..self.len].iter_mut().zip(source) {
            *to = if from.is_ascii_graphic() || *from == b' ' {
                *from
            } else {
                b'?'
            };
        }
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len])
            .unwrap_or("")
            .trim()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cache {
    pub level: u8,
    pub kind: u8,
    pub bytes: u64,
    pub line_bytes: u32,
    pub sharing: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Topology {
    pub thread_id: u32,
    pub core_id: u32,
    pub package_id: u32,
    pub logical_per_package: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuInfo {
    pub architecture: Architecture,
    pub hardware_id: u64,
    pub vendor: Text<16>,
    pub model: Text<64>,
    /// Native numeric vendor/revision identifiers, where the ISA defines them.
    /// Preserve their full width; an unavailable firmware getter yields None.
    pub vendor_id: Option<u64>,
    pub revision_id: Option<u64>,
    pub signature: u64,
    pub family: u16,
    pub model_id: u16,
    pub stepping: u8,
    pub physical_bits: Option<u8>,
    pub virtual_bits: Option<u8>,
    pub features: Features,
    pub topology: Option<Topology>,
    pub caches: [Option<Cache>; 16],
    pub cache_count: usize,
    pub counter_hz: Option<u64>,
}

impl CpuInfo {
    pub(crate) fn empty(architecture: Architecture) -> Self {
        Self {
            architecture,
            hardware_id: 0,
            vendor: Text::default(),
            model: Text::default(),
            vendor_id: None,
            revision_id: None,
            signature: 0,
            family: 0,
            model_id: 0,
            stepping: 0,
            physical_bits: None,
            virtual_bits: None,
            features: Features::default(),
            topology: None,
            caches: [None; 16],
            cache_count: 0,
            counter_hz: None,
        }
    }
}

struct Snapshot {
    value: UnsafeCell<MaybeUninit<CpuInfo>>,
    initializing: AtomicBool,
    ready: AtomicBool,
}

// SAFETY: a single initializer writes before Release publication; readers
// acquire ready and the snapshot is never mutated afterwards, including NMIs.
unsafe impl Sync for Snapshot {}

static BSP: Snapshot = Snapshot {
    value: UnsafeCell::new(MaybeUninit::uninit()),
    initializing: AtomicBool::new(false),
    ready: AtomicBool::new(false),
};

fn snapshot() -> Option<&'static CpuInfo> {
    if !BSP.ready.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: acquire observes initialized, permanently immutable storage.
    Some(unsafe { (&*BSP.value.get()).assume_init_ref() })
}

/// Discover the BSP before memory management uses architectural capabilities.
pub fn init() {
    assert!(
        BSP.initializing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok(),
        "CPU discovery initialized twice"
    );
    // SAFETY: Limine invokes kmain on the BSP recorded in its MP response.
    let discovered =
        unsafe { detect_current(crate::boot::bsp_cpu_index()) }.expect("BSP identity required");
    // SAFETY: this is the only writer and publication has not happened yet.
    unsafe {
        (*BSP.value.get()).write(discovered);
    }
    BSP.ready.store(true, Ordering::Release);
}

/// Read this CPU's native identity. Does not read privileged machine-mode
/// RISC-V CSRs or speculate about absent firmware properties.
/// # Safety
/// `index` must identify the logical CPU executing this call. RISC-V S mode
/// obtains its hart identity from the boot handoff rather than a machine CSR.
pub unsafe fn detect_current(index: usize) -> Option<CpuInfo> {
    crate::boot::cpu_hardware_id(index)?;
    Some(arch::cpuid::detect(index))
}

pub fn info() -> Option<CpuInfo> {
    snapshot().copied()
}
pub fn has(feature: Feature) -> bool {
    snapshot().is_some_and(|cpu| cpu.features.contains(feature))
}

/// Check a future AP before it joins a system-wide capability policy.
pub fn compatible(cpu: &CpuInfo, required: Features) -> bool {
    info().is_some_and(|bsp| {
        cpu.architecture == bsp.architecture
            && cpu.features.contains_all(required)
            // A narrower AP cannot safely consume the BSP's page-table policy.
            // Unknown widths stay unknown instead of assuming BSP capabilities.
            && bsp.physical_bits.is_none_or(|bits| {
                cpu.physical_bits.is_some_and(|local| local >= bits)
            })
            && bsp.virtual_bits.is_none_or(|bits| {
                cpu.virtual_bits.is_some_and(|local| local >= bits)
            })
    })
}
