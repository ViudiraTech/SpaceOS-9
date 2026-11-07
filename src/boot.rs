/*
 *
 *       src/boot.rs
 *       Validated Limine boot handoff and request declarations
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use limine::{
    BaseRevision, RequestsEndMarker, RequestsStartMarker,
    paging::PagingMode,
    request::{
        DtbRequest, ExecutableAddressRequest, ExecutableCmdlineRequest, FramebufferRequest,
        HhdmRequest, MemmapRequest, ModulesRequest, MpRequest, PagingModeRequest, RsdpRequest,
        StackSizeRequest, TscFrequencyRequest,
    },
};

#[cfg(target_arch = "x86_64")]
const MODE: PagingMode = PagingMode::X86_64_4LVL;
#[cfg(target_arch = "aarch64")]
const MODE: PagingMode = PagingMode::AARCH64_4LVL;
#[cfg(all(target_arch = "riscv64", not(feature = "riscv-sv48")))]
const MODE: PagingMode = PagingMode::RISCV_SV39;
#[cfg(all(target_arch = "riscv64", feature = "riscv-sv48"))]
const MODE: PagingMode = PagingMode::RISCV_SV48;

#[used]
#[unsafe(link_section = ".limine_requests_start")]
static START: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static REVISION: BaseRevision = BaseRevision::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM: HhdmRequest = HhdmRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMMAP: MemmapRequest = MemmapRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static MP: MpRequest = MpRequest::new(0);
#[used]
#[unsafe(link_section = ".limine_requests")]
static ADDRESS: ExecutableAddressRequest = ExecutableAddressRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static PAGING: PagingModeRequest = PagingModeRequest::new_exact(MODE);
#[used]
#[unsafe(link_section = ".limine_requests")]
static RSDP: RsdpRequest = RsdpRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static DTB: DtbRequest = DtbRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static CMDLINE: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static COUNTER_FREQUENCY: TscFrequencyRequest = TscFrequencyRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static MODULES: ModulesRequest = ModulesRequest::new();
#[used]
#[unsafe(link_section = ".limine_requests")]
static STACK: StackSizeRequest = StackSizeRequest::new(1024 * 1024);
#[used]
#[unsafe(link_section = ".limine_requests_end")]
static END: RequestsEndMarker = RequestsEndMarker::new();

pub struct BootInfo {
    pub hhdm: u64,
    pub kernel_virtual: u64,
    pub kernel_physical: u64,
}

pub fn read() -> BootInfo {
    assert!(REVISION.is_supported(), "unsupported Limine base revision");
    assert_eq!(PAGING.response().expect("paging mode required").mode, MODE);
    let hhdm = HHDM.response().expect("HHDM required");
    let address = ADDRESS.response().expect("executable address required");
    assert!(MEMMAP.response().is_some(), "memory map required");
    assert!(MP.response().is_some(), "MP required");
    let _ = RSDP.response();
    let _ = DTB.response();
    let _ = STACK.response();
    BootInfo {
        hhdm: hhdm.offset,
        kernel_virtual: address.virtual_base,
        kernel_physical: address.physical_base,
    }
}

pub fn acpi_disabled() -> bool {
    cmdline()
        .split_ascii_whitespace()
        .any(|arg| arg == "acpi=none" || arg == "acpi=off")
}

pub fn rsdp_address() -> Option<usize> {
    if acpi_disabled() {
        return None;
    }
    RSDP.response().map(|response| response.address as usize)
}

pub fn dtb_address() -> Option<usize> {
    DTB.response().map(|response| response.dtb_ptr as usize)
}

pub fn memory_map() -> &'static [&'static limine::memmap::Entry] {
    MEMMAP.response().expect("memory map required").entries()
}

pub fn cmdline() -> &'static str {
    CMDLINE.response().map_or("", |response| response.cmdline())
}

/// Link-time request base reported by Limine. The kernel ELF is relocatable
/// and the symbol table in the kallsyms blob is relative to this base.
pub fn kernel_virtual() -> u64 {
    ADDRESS
        .response()
        .map_or(0, |response| response.virtual_base)
}

/// Physical base of the kernel image, the anchor for virtual-to-physical
/// translation of kernel-image addresses.
pub fn kernel_physical() -> u64 {
    ADDRESS
        .response()
        .map_or(0, |response| response.physical_base)
}

pub fn framebuffer() -> Option<&'static limine::framebuffer::Framebuffer> {
    FRAMEBUFFER.response()?.framebuffers().first().copied()
}

pub fn counter_frequency() -> Option<u64> {
    COUNTER_FREQUENCY
        .response()
        .map(|response| response.frequency)
}

pub fn cpu_count() -> usize {
    MP.response().expect("MP required").cpus().len()
}

/// Firmware hardware ID for an indexed logical CPU (APIC ID, MPIDR or hart ID).
pub fn cpu_hardware_id(index: usize) -> Option<u64> {
    let cpu = *MP.response()?.cpus().get(index)?;
    #[cfg(target_arch = "x86_64")]
    return Some(u64::from(cpu.lapic_id));
    #[cfg(target_arch = "aarch64")]
    return Some(cpu.mpidr);
    #[cfg(target_arch = "riscv64")]
    return Some(cpu.hartid);
}

pub fn bsp_cpu_index() -> usize {
    let response = MP.response().expect("MP required");
    #[cfg(target_arch = "x86_64")]
    let index = response
        .cpus()
        .iter()
        .position(|cpu| cpu.lapic_id == response.bsp_lapic_id);
    #[cfg(target_arch = "aarch64")]
    let index = response
        .cpus()
        .iter()
        .position(|cpu| cpu.mpidr == response.bsp_mpidr);
    #[cfg(target_arch = "riscv64")]
    let index = response
        .cpus()
        .iter()
        .position(|cpu| cpu.hartid == response.bsp_hartid);
    index.expect("BSP absent from Limine CPU list")
}

/// Release the AP only after all kernel-owned startup state is published.
pub fn start_cpu(index: usize, entry: limine::mp::MpGotoFunction) {
    assert_ne!(index, bsp_cpu_index());
    MP.response().expect("MP required").cpus()[index].bootstrap(entry, index as u64);
}

pub fn init_path() -> Option<alloc::string::String> {
    cmdline()
        .split_ascii_whitespace()
        .find_map(|word| word.strip_prefix("init="))
        .filter(|path| !path.is_empty())
        .map(alloc::string::String::from)
}

pub fn modules() -> &'static [&'static limine::file::File] {
    if let Some(resp) = MODULES.response() {
        resp.modules()
    } else {
        &[]
    }
}
