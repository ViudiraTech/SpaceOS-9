/*
 *
 *       src/module/hw.rs
 *       Hardware C ABI for loadable modules: PCI, DMA, MMIO, IRQ, timers
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Everything a physical device driver needs that is not part of the network
//! stack itself. Handles are opaque pointers owned by the kernel; modules must
//! not dereference them. Every callback runs with the same context rules as its
//! Linux analogue (probe/remove in task context, interrupt handlers with
//! interrupts disabled).

use crate::{
    arch,
    mm::{dma, vmm},
    pci::{self, Bar, DeviceInfo},
    sync::SpinLock,
    vfs::abi,
};
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::{
    ffi::{c_char, c_int, c_long, c_void},
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};

// ---- DMA --------------------------------------------------------------------

struct CoherentEntry {
    pointer: usize,
    allocation: dma::Coherent,
}

static COHERENT: SpinLock<Vec<CoherentEntry>> = SpinLock::new(Vec::new());

/// # Safety
/// `dma_address` must be null or point to a writable `unsigned long long`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_dma_alloc_coherent(
    size: usize,
    align: usize,
    dma_address: *mut u64,
) -> *mut c_void {
    let Some(allocation) = dma::alloc_coherent(size, align.max(1)) else {
        return ptr::null_mut();
    };
    let address = allocation.dma_address();
    let pointer = allocation.as_ptr::<u8>() as usize;
    COHERENT.lock().push(CoherentEntry {
        pointer,
        allocation,
    });
    if !dma_address.is_null() {
        // SAFETY: the caller guarantees a writable out-parameter.
        unsafe { *dma_address = address };
    }
    pointer as *mut c_void
}

/// # Safety
/// `address` must be a live pointer returned by `spk_dma_alloc_coherent`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_dma_free_coherent(address: *mut c_void, _size: usize) {
    if address.is_null() {
        return;
    }
    let mut entries = COHERENT.lock();
    if let Some(index) = entries
        .iter()
        .position(|entry| entry.pointer == address as usize)
    {
        drop(entries.remove(index));
    }
}

/// # Safety
/// `address` must point to `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_dma_map_single(address: *mut c_void, length: usize) -> u64 {
    dma::map_single(address as *const u8, length).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn spk_dma_unmap_single(_address: u64, _length: usize) {}

// ---- MMIO -------------------------------------------------------------------

/// A device mapping whose lifetime is the mapping itself.
pub struct MmioMap {
    virtual_address: u64,
    physical_address: u64,
    length: usize,
}

impl MmioMap {
    pub fn new(physical: u64, length: usize) -> Option<Self> {
        let virtual_address = vmm::map_device_range(physical, length).ok()?;
        Some(Self {
            virtual_address,
            physical_address: physical,
            length,
        })
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.virtual_address as *mut c_void
    }
}

impl Drop for MmioMap {
    fn drop(&mut self) {
        let offset = (self.physical_address % 4096) as usize;
        let pages = (offset + self.length).div_ceil(4096);
        let base = self.virtual_address - offset as u64;
        let _ = vmm::unmap(base, pages);
    }
}

/// # Safety
/// The physical range must belong to a device and not be RAM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_ioremap(physical: u64, length: usize) -> *mut c_void {
    MmioMap::new(physical, length).map_or(ptr::null_mut(), |map| {
        let pointer = map.as_ptr();
        // Kernel-owned DOM mappings live until module unload; keep them in a
        // registry so `spk_iounmap` can drop the right one.
        MODULE_MAPPINGS.lock().push((pointer as usize, map));
        pointer
    })
}

static MODULE_MAPPINGS: SpinLock<Vec<(usize, MmioMap)>> = SpinLock::new(Vec::new());

/// # Safety
/// `address` must come from `spk_ioremap`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_iounmap(address: *mut c_void, _length: usize) {
    if address.is_null() {
        return;
    }
    let mut mappings = MODULE_MAPPINGS.lock();
    if let Some(index) = mappings
        .iter()
        .position(|(pointer, _)| *pointer == address as usize)
    {
        drop(mappings.remove(index));
    }
}

/// # Safety
/// `address` must be a mapped MMIO register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_readl(address: *const c_void) -> u32 {
    // SAFETY: caller contract; MMIO requires volatile access.
    unsafe { ptr::read_volatile(address as *const u32) }
}

/// # Safety
/// `address` must be a mapped MMIO register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_writel(value: u32, address: *mut c_void) {
    // SAFETY: caller contract; MMIO requires volatile access.
    unsafe { ptr::write_volatile(address as *mut u32, value) };
    core::sync::atomic::fence(Ordering::SeqCst);
}

/// # Safety
/// `address` must be a mapped MMIO register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_readw(address: *const c_void) -> u16 {
    // SAFETY: caller contract.
    unsafe { ptr::read_volatile(address as *const u16) }
}

/// # Safety
/// `address` must be a mapped MMIO register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_writew(value: u16, address: *mut c_void) {
    // SAFETY: caller contract.
    unsafe { ptr::write_volatile(address as *mut u16, value) };
    core::sync::atomic::fence(Ordering::SeqCst);
}

/// # Safety
/// `address` must be a mapped MMIO register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_readb(address: *const c_void) -> u8 {
    // SAFETY: caller contract.
    unsafe { ptr::read_volatile(address as *const u8) }
}

/// # Safety
/// `address` must be a mapped MMIO register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_writeb(value: u8, address: *mut c_void) {
    // SAFETY: caller contract.
    unsafe { ptr::write_volatile(address as *mut u8, value) };
    core::sync::atomic::fence(Ordering::SeqCst);
}

#[unsafe(no_mangle)]
pub extern "C" fn spk_mb() {
    core::sync::atomic::fence(Ordering::SeqCst);
}

// ---- Spin locks ---------------------------------------------------------------

#[repr(C)]
pub struct CSpinLock {
    locked: AtomicU32,
}

/// # Safety
/// `lock` must point to a `spk_spinlock` with static or module-owned storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_spin_lock(lock: *mut CSpinLock) {
    let Some(lock) = (unsafe { lock.as_ref() }) else {
        return;
    };
    loop {
        if lock
            .locked
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        while lock.locked.load(Ordering::Relaxed) != 0 {
            core::hint::spin_loop();
        }
    }
}

/// # Safety
/// The caller must hold the lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_spin_unlock(lock: *mut CSpinLock) {
    if let Some(lock) = unsafe { lock.as_ref() } {
        lock.locked.store(0, Ordering::Release);
    }
}

/// # Safety
/// `lock` must be a valid module lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_spin_lock_irqsave(lock: *mut CSpinLock) -> u64 {
    let flags = arch::irq_save();
    // SAFETY: forwarding the caller's contract.
    unsafe { spk_spin_lock(lock) };
    flags
}

/// # Safety
/// `lock` must be held by `spk_spin_lock_irqsave` with the returned flags.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_spin_unlock_irqrestore(lock: *mut CSpinLock, flags: u64) {
    // SAFETY: forwarding the caller's contract.
    unsafe { spk_spin_unlock(lock) };
    arch::irq_restore(flags);
}

// ---- Timers and kernel threads -------------------------------------------------

/// # Safety
/// `callback` must remain valid for the lifetime of the kernel and accept
/// `data` per the module's own contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_timer_add(
    delay_us: u64,
    callback: extern "C" fn(*mut c_void),
    data: *mut c_void,
) -> u64 {
    let data = data as usize;
    crate::net::timer::schedule_in(delay_us, Box::new(move || callback(data as *mut c_void)))
}

#[unsafe(no_mangle)]
pub extern "C" fn spk_timer_del(id: u64) -> c_int {
    crate::net::timer::cancel(id) as c_int
}

/// # Safety
/// Same contract as `spk_timer_add`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_work_schedule(
    callback: extern "C" fn(*mut c_void),
    data: *mut c_void,
) -> c_int {
    // SAFETY: forwarding the caller's contract.
    let id = unsafe { spk_timer_add(0, callback, data) };
    if id != 0 { 0 } else { -(abi::EAGAIN as c_int) }
}

/// # Safety
/// `name` must be a NUL-terminated string; `function` must be safe to call on
/// a kernel thread with `argument`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_kthread_create(
    name: *const c_char,
    function: extern "C" fn(*mut c_void) -> c_int,
    argument: *mut c_void,
) -> c_long {
    let _ = c_string(name);
    let address = argument as usize;
    match crate::sched::spawn(
        move || {
            function(address as *mut c_void);
        },
        0,
    ) {
        Ok(tid) => tid as c_long,
        Err(_) => -1,
    }
}

// ---- PCI ----------------------------------------------------------------------

#[repr(C)]
pub struct CDeviceId {
    pub vendor: u32,
    pub device: u32,
    pub subvendor: u32,
    pub subdevice: u32,
    pub class: u32,
    pub class_mask: u32,
    pub driver_data: usize,
}

#[repr(C)]
pub struct CDriver {
    pub name: *const c_char,
    pub id_table: *const CDeviceId,
    pub probe: Option<extern "C" fn(*mut CPciDevice) -> c_int>,
    pub remove: Option<extern "C" fn(*mut CPciDevice)>,
}

#[derive(Clone, Copy)]
pub enum ApiIrq {
    #[cfg(target_arch = "x86_64")]
    Legacy(crate::arch::apic::Route),
    #[cfg(target_arch = "x86_64")]
    Msi(crate::arch::apic::MsiMessage),
    #[cfg(not(target_arch = "x86_64"))]
    Unsupported,
}

pub struct CPciDevice {
    pub info: DeviceInfo,
    pub driver: usize,
    pub private: usize,
    pub irq: Option<ApiIrq>,
    pub msix: Option<MmioMap>,
}

struct DriverEntry {
    name: String,
    handle: usize,
    ids: Vec<CDeviceId>,
    probe: extern "C" fn(*mut CPciDevice) -> c_int,
    remove: Option<extern "C" fn(*mut CPciDevice)>,
    devices: Vec<*mut CPciDevice>,
}

unsafe impl Send for DriverEntry {}

static DRIVERS: SpinLock<Vec<DriverEntry>> = SpinLock::new(Vec::new());

fn c_string<'a>(pointer: *const c_char) -> &'a [u8] {
    if pointer.is_null() {
        return b"(null)";
    }
    let mut length = 0usize;
    // SAFETY: module-provided strings are NUL terminated; the scan is bounded.
    while length < 4096 && unsafe { *pointer.add(length) as u8 } != 0 {
        length += 1;
    }
    // SAFETY: the bounded range was just validated.
    unsafe { core::slice::from_raw_parts(pointer as *const u8, length) }
}

fn parse_ids(table: *const CDeviceId) -> Vec<CDeviceId> {
    let mut ids = Vec::new();
    if table.is_null() {
        return ids;
    }
    for index in 0..256usize {
        // SAFETY: the table is module-owned and terminated by a zero entry.
        let id = unsafe { ptr::read(table.add(index)) };
        if id.vendor == 0 && id.device == 0 {
            break;
        }
        ids.push(id);
    }
    ids
}

fn matches(id: &CDeviceId, info: &DeviceInfo) -> bool {
    if id.vendor != 0 && id.vendor != u32::from(info.vendor_id) {
        return false;
    }
    if id.device != 0 && id.device != u32::from(info.device_id) {
        return false;
    }
    if id.subvendor != 0 && Some(id.subvendor as u16) != info.subsystem_vendor_id {
        return false;
    }
    if id.subdevice != 0 && Some(id.subdevice as u16) != info.subsystem_device_id {
        return false;
    }
    if id.class != 0 && id.class_mask != 0 {
        let class = (u32::from(info.class) << 16)
            | (u32::from(info.subclass) << 8)
            | u32::from(info.programming_interface);
        if class & id.class_mask != id.class & id.class_mask {
            return false;
        }
    }
    true
}

/// # Safety
/// `driver` must point to a `struct spk_pci_driver` that lives until
/// `spk_pci_driver_unregister` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_driver_register(driver: *const CDriver) -> c_int {
    if driver.is_null() {
        return -(abi::EINVAL as c_int);
    }
    // SAFETY: the caller guarantees a live descriptor.
    let descriptor = unsafe { &*driver };
    let name = String::from_utf8_lossy(c_string(descriptor.name)).to_string();
    let Some(probe) = descriptor.probe else {
        return -(abi::EINVAL as c_int);
    };
    let ids = parse_ids(descriptor.id_table);
    if ids.is_empty() {
        return -(abi::EINVAL as c_int);
    }
    let handle = driver as usize;
    if DRIVERS.lock().iter().any(|entry| entry.handle == handle) {
        return -(abi::EEXIST as c_int);
    }
    let candidates: Vec<DeviceInfo> = pci::devices()
        .iter()
        .filter(|info| ids.iter().any(|id| matches(id, info)))
        .copied()
        .collect();
    let mut bound = Vec::new();
    for info in candidates {
        let device = Box::into_raw(Box::new(CPciDevice {
            info,
            driver: handle,
            private: 0,
            irq: None,
            msix: None,
        }));
        // SAFETY: the handle is the freshly allocated descriptor; module code
        // treats it as opaque and only passes it back to this API.
        let result = probe(device);
        if result == 0 {
            bound.push(device);
        } else {
            // SAFETY: probe failed and no module state owns the descriptor.
            unsafe { drop(Box::from_raw(device)) };
        }
    }
    DRIVERS.lock().push(DriverEntry {
        name,
        handle,
        ids,
        probe,
        remove: descriptor.remove,
        devices: bound,
    });
    0
}

/// # Safety
/// `driver` must be the same pointer previously passed to register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_driver_unregister(driver: *const CDriver) {
    let handle = driver as usize;
    let entry = {
        let mut drivers = DRIVERS.lock();
        let Some(index) = drivers.iter().position(|entry| entry.handle == handle) else {
            return;
        };
        drivers.remove(index)
    };
    for device in entry.devices {
        // SAFETY: the descriptor is still owned by this registry entry.
        if let Some(remove) = entry.remove {
            remove(device);
        }
        // SAFETY: remove has run; the descriptor is unreachable from here.
        unsafe {
            release_device_irq(&mut *device);
            drop(Box::from_raw(device));
        }
    }
    let _ = &entry.name;
    let _ = &entry.ids;
    let _ = &entry.probe;
}

fn config_error() -> c_int {
    -(abi::EIO as c_int)
}

/// # Safety
/// `device` must be a handle delivered to a registered driver's probe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_read_config_byte(device: *mut CPciDevice, offset: c_int) -> c_int {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return config_error();
    };
    match pci::config_for(device.info.address).and_then(|config| config.read_u8(offset as u16)) {
        Ok(value) => value as c_int,
        Err(_) => config_error(),
    }
}

/// # Safety
/// See `spk_pci_read_config_byte`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_read_config_word(device: *mut CPciDevice, offset: c_int) -> c_int {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return config_error();
    };
    match pci::config_for(device.info.address).and_then(|config| config.read_u16(offset as u16)) {
        Ok(value) => value as c_int,
        Err(_) => config_error(),
    }
}

/// # Safety
/// See `spk_pci_read_config_byte`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_read_config_dword(
    device: *mut CPciDevice,
    offset: c_int,
) -> c_int {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return config_error();
    };
    match pci::config_for(device.info.address).and_then(|config| config.read_u32(offset as u16)) {
        Ok(value) => value as c_int,
        Err(_) => config_error(),
    }
}

/// # Safety
/// See `spk_pci_read_config_byte`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_write_config_byte(
    device: *mut CPciDevice,
    offset: c_int,
    value: u32,
) -> c_int {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return config_error();
    };
    match pci::config_for(device.info.address)
        .and_then(|mut config| config.write_u8(offset as u16, value as u8))
    {
        Ok(()) => 0,
        Err(_) => config_error(),
    }
}

/// # Safety
/// See `spk_pci_read_config_byte`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_write_config_word(
    device: *mut CPciDevice,
    offset: c_int,
    value: u32,
) -> c_int {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return config_error();
    };
    match pci::config_for(device.info.address)
        .and_then(|mut config| config.write_u16(offset as u16, value as u16))
    {
        Ok(()) => 0,
        Err(_) => config_error(),
    }
}

/// # Safety
/// See `spk_pci_read_config_byte`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_write_config_dword(
    device: *mut CPciDevice,
    offset: c_int,
    value: u32,
) -> c_int {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return config_error();
    };
    match pci::config_for(device.info.address)
        .and_then(|mut config| config.write_u32(offset as u16, value))
    {
        Ok(()) => 0,
        Err(_) => config_error(),
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_enable_device(device: *mut CPciDevice) -> c_int {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return config_error();
    };
    let result = pci::config_for(device.info.address).and_then(|mut config| {
        let command = config.read_u16(0x04)? | 0x6; // I/O space | memory space
        config.write_u16(0x04, command)
    });
    match result {
        Ok(()) => 0,
        Err(_) => config_error(),
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_set_master(device: *mut CPciDevice) {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return;
    };
    let _ = pci::config_for(device.info.address).and_then(|mut config| {
        let command = config.read_u16(0x04)? | 0x4;
        config.write_u16(0x04, command)
    });
}

fn bar_base(info: &DeviceInfo, bar: usize) -> Option<u64> {
    match info.bars.get(bar).copied().flatten()? {
        Bar::Memory32 { base, .. } => Some(u64::from(base)),
        Bar::Memory64 { base, .. } => Some(base),
        Bar::Io { .. } | Bar::Unsupported { .. } => None,
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_resource_start(device: *mut CPciDevice, bar: c_int) -> u64 {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return 0;
    };
    bar_base(&device.info, bar.max(0) as usize).unwrap_or(0)
}

/// Determine a BAR's size with the standard probe sequence, restoring the
/// original value. Only memory BARs are supported.
///
/// # Safety
/// `device` must be a valid handle and no DMA may be active on the BAR.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_resource_len(device: *mut CPciDevice, bar: c_int) -> u64 {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return 0;
    };
    let index = bar.max(0) as u16;
    if index >= 6 {
        return 0;
    }
    let result = pci::config_for(device.info.address).and_then(|mut config| {
        let offset = 0x10 + index * 4;
        let original = config.read_u32(offset)?;
        if original & 1 != 0 {
            return Ok(0u64);
        }
        config.write_u32(offset, 0xffff_ffff)?;
        let mask = config.read_u32(offset)?;
        config.write_u32(offset, original)?;
        let mask = mask & !0xf;
        if mask == 0 {
            return Ok(0u64);
        }
        Ok((!(mask as u64) + 1) & 0xffff_ffff)
    });
    result.unwrap_or(0)
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_irq_line(device: *mut CPciDevice) -> c_int {
    let Some(device) = (unsafe { device.as_ref() }) else {
        return 0;
    };
    c_int::from(device.info.interrupt_line)
}

// ---- ISA port I/O and legacy interrupts ---------------------------------------

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn port_inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: caller asserts the port belongs to the device.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn port_outb(port: u16, value: u8) {
    // SAFETY: caller asserts the port belongs to the device.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn port_inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn port_outw(port: u16, value: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn port_inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn port_outl(port: u16, value: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

/// # Safety
/// The module must own the I/O port.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_inb(port: u16) -> u8 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: forwarded caller contract.
        unsafe { port_inb(port) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = port;
        0
    }
}

/// # Safety
/// The module must own the I/O port.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_outb(port: u16, value: u8) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: forwarded caller contract.
        unsafe { port_outb(port, value) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (port, value);
    }
}

/// # Safety
/// The module must own the I/O port.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_inw(port: u16) -> u16 {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { port_inw(port) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = port;
        0
    }
}

/// # Safety
/// The module must own the I/O port.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_outw(port: u16, value: u16) {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { port_outw(port, value) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (port, value);
    }
}

/// # Safety
/// The module must own the I/O port.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_inl(port: u16) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { port_inl(port) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = port;
        0
    }
}

/// # Safety
/// The module must own the I/O port.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_outl(port: u16, value: u32) {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { port_outl(port, value) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (port, value);
    }
}

#[cfg(target_arch = "x86_64")]
static ISA_ROUTES: SpinLock<[Option<crate::arch::apic::Route>; 16]> = SpinLock::new([None; 16]);

/// Register a handler for a legacy ISA IRQ line (0-15).
/// # Safety
/// `handler` must stay valid until `spk_free_isa_irq`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_request_isa_irq(
    irq: u32,
    handler: extern "C" fn(c_int, *mut c_void),
    data: *mut c_void,
) -> c_int {
    #[cfg(target_arch = "x86_64")]
    {
        if irq >= 16 {
            return -(abi::EINVAL as c_int);
        }
        let mut routes = ISA_ROUTES.lock();
        if routes[irq as usize].is_some() {
            return -(abi::EBUSY as c_int);
        }
        match crate::arch::apic::request_isa(irq as u8, irq_trampoline) {
            Ok(route) => {
                install_interrupt(route.vector, handler, data);
                routes[irq as usize] = Some(route);
                0
            }
            Err(_) => -(abi::ENODEV as c_int),
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (irq, handler, data);
        -(abi::ENODEV as c_int)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn spk_free_isa_irq(irq: u32) {
    #[cfg(target_arch = "x86_64")]
    {
        if irq >= 16 {
            return;
        }
        let route = ISA_ROUTES.lock()[irq as usize].take();
        if let Some(route) = route {
            remove_interrupt(route.vector);
            let _ = crate::arch::apic::release_route(route);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = irq;
    }
}

// ---- Interrupts ---------------------------------------------------------------

#[derive(Clone, Copy)]
struct CInterrupt {
    handler: extern "C" fn(c_int, *mut c_void),
    data: usize,
}

static C_INTERRUPTS: SpinLock<[Option<CInterrupt>; crate::irq::MAX_IRQS]> =
    SpinLock::new([None; crate::irq::MAX_IRQS]);

fn irq_trampoline(irq: u32) {
    let entry = C_INTERRUPTS.lock().get(irq as usize).copied().flatten();
    if let Some(entry) = entry {
        (entry.handler)(irq as c_int, entry.data as *mut c_void);
    }
}

fn install_interrupt(vector: u8, handler: extern "C" fn(c_int, *mut c_void), data: *mut c_void) {
    if let Some(slot) = C_INTERRUPTS.lock().get_mut(vector as usize) {
        *slot = Some(CInterrupt {
            handler,
            data: data as usize,
        });
    }
}

fn remove_interrupt(vector: u8) {
    if let Some(slot) = C_INTERRUPTS.lock().get_mut(vector as usize) {
        *slot = None;
    }
}

fn find_capability(info: &DeviceInfo, capability: u8) -> Option<u16> {
    let config = pci::config_for(info.address).ok()?;
    let status = config.read_u16(0x06).ok()?;
    if status & 0x10 == 0 {
        return None;
    }
    let mut pointer = config.read_u8(0x34).ok()? as u16;
    for _ in 0..64 {
        if pointer == 0 || pointer >= 0x100 {
            return None;
        }
        let id = config.read_u8(pointer).ok()?;
        if id == capability {
            return Some(pointer);
        }
        pointer = (config.read_u8(pointer + 1).ok()? & 0xfc) as u16;
    }
    None
}

/// # Safety
/// `device` must be a valid handle; `handler` must remain valid until disable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_enable_msi(
    device: *mut CPciDevice,
    handler: extern "C" fn(c_int, *mut c_void),
    data: *mut c_void,
    message_address: *mut u32,
    message_data: *mut u32,
) -> c_int {
    let Some(device) = (unsafe { device.as_mut() }) else {
        return -(abi::EINVAL as c_int);
    };
    #[cfg(target_arch = "x86_64")]
    {
        let Some(capability) = find_capability(&device.info, 0x05) else {
            return -(abi::ENODEV as c_int);
        };
        let (control, is_64) = {
            let Ok(config) = pci::config_for(device.info.address) else {
                return config_error();
            };
            let control = config.read_u16(capability + 2).unwrap_or(0);
            (control, control & 0x80 != 0)
        };
        let Ok(message) = crate::arch::apic::request_msi(irq_trampoline) else {
            return -(abi::ENODEV as c_int);
        };
        install_interrupt(message.vector, handler, data);
        let address = message.address as u32;
        let payload = message.data;
        let configured = pci::config_for(device.info.address).and_then(|mut config| {
            config.write_u32(capability + 4, address)?;
            if is_64 {
                config.write_u32(capability + 8, (message.address >> 32) as u32)?;
            }
            let data_offset = if is_64 {
                capability + 12
            } else {
                capability + 8
            };
            config.write_u16(data_offset, payload as u16)?;
            config.write_u16(capability + 2, control | 0x1)
        });
        if configured.is_err() {
            remove_interrupt(message.vector);
            let _ = crate::arch::apic::release_msi(message);
            return config_error();
        }
        if !message_address.is_null() {
            // SAFETY: documented out-parameter.
            unsafe { *message_address = address };
        }
        if !message_data.is_null() {
            // SAFETY: documented out-parameter.
            unsafe { *message_data = payload };
        }
        device.irq = Some(ApiIrq::Msi(message));
        0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (device, handler, data, message_address, message_data);
        -(abi::ENODEV as c_int)
    }
}

fn release_device_irq(device: &mut CPciDevice) {
    match device.irq.take() {
        #[cfg(target_arch = "x86_64")]
        Some(ApiIrq::Msi(message)) => {
            remove_interrupt(message.vector);
            let _ = crate::arch::apic::release_msi(message);
        }
        #[cfg(target_arch = "x86_64")]
        Some(ApiIrq::Legacy(route)) => {
            remove_interrupt(route.vector);
            let _ = crate::arch::apic::release_route(route);
        }
        #[cfg(not(target_arch = "x86_64"))]
        Some(ApiIrq::Unsupported) => {}
        None => {}
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_disable_msi(device: *mut CPciDevice) {
    if let Some(device) = unsafe { device.as_mut() } {
        release_device_irq(device);
        if let Some(capability) = find_capability(&device.info, 0x05)
            && let Ok(mut config) = pci::config_for(device.info.address)
        {
            let control = config.read_u16(capability + 2).unwrap_or(0);
            let _ = config.write_u16(capability + 2, control & !0x1);
        }
    }
}

/// # Safety
/// `device` must be a valid handle; `handler` must remain valid until disable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_enable_msix(
    device: *mut CPciDevice,
    vectors: u32,
    handler: extern "C" fn(c_int, *mut c_void),
    data: *mut c_void,
) -> c_int {
    let Some(device) = (unsafe { device.as_mut() }) else {
        return -(abi::EINVAL as c_int);
    };
    #[cfg(target_arch = "x86_64")]
    {
        let Some(capability) = find_capability(&device.info, 0x11) else {
            return -(abi::ENODEV as c_int);
        };
        let (control, table_size, table) = {
            let Ok(config) = pci::config_for(device.info.address) else {
                return config_error();
            };
            let control = config.read_u16(capability + 2).unwrap_or(0);
            let table_size = u32::from(control & 0x7ff) + 1;
            let table = config.read_u32(capability + 4).unwrap_or(0);
            (control, table_size, table)
        };
        let entry_count = vectors.clamp(1, table_size);
        let bar = (table & 0x7) as usize;
        let offset = u64::from(table & !0x7);
        let Some(base) = bar_base(&device.info, bar) else {
            return -(abi::ENODEV as c_int);
        };
        let Some(mapping) = MmioMap::new(base + offset, entry_count as usize * 16) else {
            return -(abi::ENOMEM as c_int);
        };
        let Ok(message) = crate::arch::apic::request_msi(irq_trampoline) else {
            return -(abi::ENODEV as c_int);
        };
        install_interrupt(message.vector, handler, data);
        let table = mapping.as_ptr() as *mut u8;
        for index in 0..entry_count as usize {
            let entry = table.wrapping_add(index * 16) as *mut u32;
            // SAFETY: the mapping covers `entry_count` 16-byte entries.
            unsafe {
                ptr::write_volatile(entry, message.address as u32);
                ptr::write_volatile(entry.add(1), (message.address >> 32) as u32);
                ptr::write_volatile(entry.add(2), message.data);
                ptr::write_volatile(entry.add(3), 0);
            }
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        let _ = pci::config_for(device.info.address).and_then(|mut config| {
            config.write_u16(capability + 2, (control & !0x4000) | 0x8000)?;
            let command = config.read_u16(0x04)? | 0x400;
            config.write_u16(0x04, command)
        });
        device.irq = Some(ApiIrq::Msi(message));
        device.msix = Some(mapping);
        0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (device, vectors, handler, data);
        -(abi::ENODEV as c_int)
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_pci_disable_msix(device: *mut CPciDevice) {
    if let Some(device) = unsafe { device.as_mut() } {
        if let Some(capability) = find_capability(&device.info, 0x11)
            && let Ok(mut config) = pci::config_for(device.info.address)
        {
            let control = config.read_u16(capability + 2).unwrap_or(0);
            let _ = config.write_u16(capability + 2, control & !0x8000);
        }
        release_device_irq(device);
        device.msix = None;
    }
}

/// # Safety
/// `device` must be a valid handle; `handler` must remain valid until free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_request_irq(
    device: *mut CPciDevice,
    handler: extern "C" fn(c_int, *mut c_void),
    data: *mut c_void,
) -> c_int {
    let Some(device) = (unsafe { device.as_mut() }) else {
        return -(abi::EINVAL as c_int);
    };
    #[cfg(target_arch = "x86_64")]
    {
        let line = device.info.interrupt_line;
        if line == 0 || line == 0xff {
            return -(abi::ENODEV as c_int);
        }
        match crate::arch::apic::request_isa(line, irq_trampoline) {
            Ok(route) => {
                install_interrupt(route.vector, handler, data);
                device.irq = Some(ApiIrq::Legacy(route));
                0
            }
            Err(_) => -(abi::ENODEV as c_int),
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (device, handler, data);
        -(abi::ENODEV as c_int)
    }
}

/// # Safety
/// `device` must hold a route from `spk_request_irq`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_free_irq(device: *mut CPciDevice) {
    if let Some(device) = unsafe { device.as_mut() } {
        release_device_irq(device);
    }
}

// Silence unused warnings on non-x86 builds while keeping the constant table
// referenced for size checks.
#[allow(dead_code)]
fn _irq_table_size() -> usize {
    C_INTERRUPTS.lock().len()
}
