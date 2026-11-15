/*
 *       src/drm/virtio/transport.rs
 *       Modern VirtIO PCI split queues with retained DMA and IRQ completion
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
//! No sleeping while queue/config locks are held. A timeout resets the device
//! before request memory can be released. DMA never points into a user mapping.
use crate::{
    mm::{pmm, vmm},
    pci::{self, Bar, DeviceInfo},
    sync::SpinLock,
    vfs::VfsError,
};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering, fence};
const N: usize = 128;
const LIMIT: usize = 4 * 1024 * 1024;
pub struct Dma {
    pub physical: u64,
    pub length: usize,
    pages: usize,
}
unsafe impl Send for Dma {}
unsafe impl Sync for Dma {}
impl Dma {
    pub fn new(length: usize) -> Result<Self, VfsError> {
        if length == 0 || length > 256 * 1024 * 1024 {
            return Err(VfsError::InvalidArgument);
        }
        let pages = length.div_ceil(4096);
        let physical = pmm::alloc(pages).ok_or(VfsError::NoMemory)?;
        // SAFETY: fresh, exclusively owned contiguous physical allocation.
        unsafe {
            core::ptr::write_bytes(pmm::phys_to_virt(physical) as *mut u8, 0, pages * 4096);
        }
        Ok(Self {
            physical,
            length,
            pages,
        })
    }
    pub fn pointer(&self) -> *mut u8 {
        pmm::phys_to_virt(self.physical) as *mut u8
    }
    pub fn write(&self, offset: usize, bytes: &[u8]) -> Result<(), VfsError> {
        if offset
            .checked_add(bytes.len())
            .is_none_or(|n| n > self.length)
        {
            return Err(VfsError::InvalidArgument);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.pointer().add(offset), bytes.len());
        }
        Ok(())
    }
    pub fn bytes(&self, length: usize) -> Result<Vec<u8>, VfsError> {
        if length > self.length {
            return Err(VfsError::IoError);
        }
        Ok(unsafe { core::slice::from_raw_parts(self.pointer(), length) }.to_vec())
    }
}
impl Drop for Dma {
    fn drop(&mut self) {
        let _ = pmm::free(self.physical, self.pages);
    }
}
pub struct Request {
    pub done: AtomicBool,
    pub error: AtomicI32,
    pub response_len: AtomicU32,
    pub command: Dma,
    pub response: Dma,
    pub retain: SpinLock<Vec<Arc<dyn crate::vfs::Inode>>>,
}
impl Request {
    pub fn response(&self) -> Result<Vec<u8>, VfsError> {
        if !self.done.load(Ordering::Acquire) {
            return Err(VfsError::WouldBlock);
        }
        if self.error.load(Ordering::Acquire) != 0 {
            return Err(VfsError::IoError);
        }
        let len = self.response_len.load(Ordering::Acquire) as usize;
        if len == 0 {
            return Ok(Vec::new());
        }
        let bytes = self.response.bytes(len)?;
        if bytes.len() < 24 {
            return Err(VfsError::IoError);
        }
        match super::u32_at(&bytes, 0)? {
            0x1100..=0x1106 => Ok(bytes),
            0x1201 => Err(VfsError::NoMemory),
            0x1202..=0x1205 => Err(VfsError::InvalidArgument),
            _ => Err(VfsError::IoError),
        }
    }
}
struct Queue {
    ring: Dma,
    size: usize,
    available: u16,
    used: u16,
    busy: [bool; N],
    pending: Vec<Option<(u16, Arc<Request>)>>,
    notify: u64,
    index: u16,
}
unsafe impl Send for Queue {}
fn rd16(a: u64) -> u16 {
    u16::from_le(unsafe { (a as *const u16).read_volatile() })
}
fn rd32(a: u64) -> u32 {
    u32::from_le(unsafe { (a as *const u32).read_volatile() })
}
fn rd8(a: u64) -> u8 {
    unsafe { (a as *const u8).read_volatile() }
}
fn wr16(a: u64, v: u16) {
    unsafe { (a as *mut u16).write_volatile(v.to_le()) }
}
fn wr32(a: u64, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v.to_le()) }
}
fn wr8(a: u64, v: u8) {
    unsafe { (a as *mut u8).write_volatile(v) }
}
fn wr64(a: u64, v: u64) {
    wr32(a, v as u32);
    wr32(a + 4, (v >> 32) as u32);
}
impl Queue {
    fn new(common: u64, notify: u64, mult: u32, index: u16, msix: bool) -> Result<Self, VfsError> {
        wr16(common + 22, index);
        let max = rd16(common + 24) as usize;
        if max < 2 {
            return Err(VfsError::NoDevice);
        }
        let size = if max >= N { N } else { 1usize << max.ilog2() };
        let ring = Dma::new(3 * 4096)?;
        wr16(common + 24, size as u16);
        wr16(common + 26, if msix { 0 } else { 0xffff });
        if msix && rd16(common + 26) == 0xffff {
            return Err(VfsError::IoError);
        }
        wr64(common + 32, ring.physical);
        wr64(common + 40, ring.physical + 4096);
        wr64(common + 48, ring.physical + 8192);
        let offset = (rd16(common + 30) as u64)
            .checked_mul(mult as u64)
            .ok_or(VfsError::InvalidArgument)?;
        wr16(common + 28, 1);
        Ok(Self {
            ring,
            size,
            available: 0,
            used: 0,
            busy: [false; N],
            pending: (0..N).map(|_| None).collect(),
            notify: notify
                .checked_add(offset)
                .ok_or(VfsError::InvalidArgument)?,
            index,
        })
    }
    fn submit(&mut self, request: Arc<Request>) -> Result<(), VfsError> {
        let mut free = (0..self.size).filter(|&i| !self.busy[i]);
        let head = free.next().ok_or(VfsError::WouldBlock)?;
        let tail = free.next().ok_or(VfsError::WouldBlock)?;
        let base = self.ring.pointer() as u64;
        wr64(base + head as u64 * 16, request.command.physical);
        wr32(base + head as u64 * 16 + 8, request.command.length as u32);
        wr16(base + head as u64 * 16 + 12, 1);
        wr16(base + head as u64 * 16 + 14, tail as u16);
        wr64(base + tail as u64 * 16, request.response.physical);
        wr32(base + tail as u64 * 16 + 8, request.response.length as u32);
        wr16(base + tail as u64 * 16 + 12, 2);
        wr16(base + tail as u64 * 16 + 14, 0);
        self.busy[head] = true;
        self.busy[tail] = true;
        self.pending[head] = Some((tail as u16, request));
        wr16(
            base + 4096 + 4 + (self.available as usize % self.size * 2) as u64,
            head as u16,
        );
        fence(Ordering::Release);
        self.available = self.available.wrapping_add(1);
        wr16(base + 4098, self.available);
        fence(Ordering::SeqCst);
        wr16(self.notify, self.index);
        Ok(())
    }
    fn complete(&mut self) -> Result<usize, VfsError> {
        let base = self.ring.pointer() as u64;
        let index = rd16(base + 8194);
        fence(Ordering::Acquire);
        if index.wrapping_sub(self.used) as usize > self.size {
            return Err(VfsError::IoError);
        }
        let mut count = 0;
        while self.used != index {
            let offset = base + 8196 + (self.used as usize % self.size * 8) as u64;
            let head = rd32(offset) as usize;
            let length = rd32(offset + 4);
            if head >= self.size {
                return Err(VfsError::IoError);
            }
            let (tail, request) = self.pending[head].take().ok_or(VfsError::IoError)?;
            if length as usize > request.response.length {
                request.error.store(-5, Ordering::Relaxed);
            }
            request.response_len.store(length, Ordering::Relaxed);
            request.done.store(true, Ordering::Release);
            request.retain.lock().clear();
            self.busy[head] = false;
            self.busy[tail as usize] = false;
            self.used = self.used.wrapping_add(1);
            count += 1;
        }
        Ok(count)
    }
}
pub struct Transport {
    pub address: pci::Address,
    common: u64,
    isr: u64,
    pub config: u64,
    pub features: u64,
    pub host_visible: Option<(u64, u64)>,
    queues: [SpinLock<Queue>; 2],
    pub failed: AtomicBool,
    pub irq_count: AtomicU32,
    /// Command waiters; woken when `poll` retires used descriptors (normally
    /// from the completion interrupt) and when the device fails.
    completed: crate::sched::WaitQueue,
}
unsafe impl Send for Transport {}
unsafe impl Sync for Transport {}
fn bar(
    cfg: &mut pci::ConfigSpace<'_>,
    device: &DeviceInfo,
    index: usize,
) -> Result<(u64, u64), VfsError> {
    let (base, is_64) = match device.bars.get(index).copied().flatten() {
        Some(Bar::Memory32 { base, .. }) if base != 0 => (base as u64, false),
        Some(Bar::Memory64 { base, .. }) if base != 0 => (base, true),
        _ => return Err(VfsError::NoDevice),
    };
    let offset = 0x10 + index as u16 * 4;
    let orig_low = cfg.read_u32(offset).map_err(|_| VfsError::IoError)?;
    let cmd = cfg.read_u16(4).map_err(|_| VfsError::IoError)?;
    cfg.write_u16(4, cmd & !2).map_err(|_| VfsError::IoError)?;
    cfg.write_u32(offset, 0xffff_ffff)
        .map_err(|_| VfsError::IoError)?;
    let mask_low = cfg.read_u32(offset).map_err(|_| VfsError::IoError)?;
    cfg.write_u32(offset, orig_low)
        .map_err(|_| VfsError::IoError)?;
    let size = if is_64 {
        let orig_high = cfg.read_u32(offset + 4).map_err(|_| VfsError::IoError)?;
        cfg.write_u32(offset + 4, 0xffff_ffff)
            .map_err(|_| VfsError::IoError)?;
        let mask_high = cfg.read_u32(offset + 4).map_err(|_| VfsError::IoError)?;
        cfg.write_u32(offset + 4, orig_high)
            .map_err(|_| VfsError::IoError)?;
        let full_mask = ((mask_high as u64) << 32) | (mask_low as u64 & !0xf);
        (!full_mask).wrapping_add(1)
    } else {
        let full_mask = mask_low as u64 & !0xf;
        (!full_mask & 0xffff_ffff).wrapping_add(1)
    };
    cfg.write_u16(4, cmd).map_err(|_| VfsError::IoError)?;
    if size == 0 {
        return Err(VfsError::IoError);
    }
    Ok((base, size))
}
impl Transport {
    pub fn new(device: &DeviceInfo) -> Result<Self, VfsError> {
        let capabilities =
            pci::standard_capabilities(device.address).map_err(|_| VfsError::IoError)?;
        let mut common = 0;
        let mut notify = 0;
        let mut isr = 0;
        let mut config = 0;
        let mut mult = 0;
        let mut host_visible = None;
        for cap in &capabilities {
            if cap.id != 9 {
                continue;
            }
            let mut cfg = pci::config_for(device.address).map_err(|_| VfsError::IoError)?;
            let o = cap.offset as u16;
            let length = cfg.read_u8(o + 2).map_err(|_| VfsError::IoError)?;
            if length < 16 || o as usize + length as usize > 256 {
                continue;
            }
            let kind = cfg.read_u8(o + 3).map_err(|_| VfsError::IoError)?;
            if !matches!(kind, 1..=4 | 8) {
                continue;
            }
            let bar_idx = cfg.read_u8(o + 4).map_err(|_| VfsError::IoError)? as usize;
            let (base, bar_size) = bar(&mut cfg, device, bar_idx)?;
            let offset = cfg.read_u32(o + 8).map_err(|_| VfsError::IoError)? as u64;
            let size = cfg.read_u32(o + 12).map_err(|_| VfsError::IoError)? as u64;
            if kind == 8 && length >= 24 && cfg.read_u8(o + 5).ok() == Some(1) {
                let offset =
                    offset | ((cfg.read_u32(o + 16).map_err(|_| VfsError::IoError)? as u64) << 32);
                let size =
                    size | ((cfg.read_u32(o + 20).map_err(|_| VfsError::IoError)? as u64) << 32);
                if size != 0
                    && offset.checked_add(size).is_some_and(|end| end <= bar_size)
                    && base
                        .checked_add(offset)
                        .and_then(|a| a.checked_add(size))
                        .is_some()
                {
                    host_visible = Some((base + offset, size));
                }
                continue;
            }
            if !matches!(kind, 1..=4)
                || size == 0
                || size > 1024 * 1024
                || offset.checked_add(size).is_none_or(|end| end > bar_size)
            {
                continue;
            }
            let physical = base.checked_add(offset).ok_or(VfsError::InvalidArgument)?;
            let mapped =
                vmm::map_device_range(physical, size as usize).map_err(|_| VfsError::IoError)?;
            match kind {
                1 if size >= 56 => common = mapped,
                2 if length >= 20 => {
                    notify = mapped;
                    mult = cfg.read_u32(o + 16).map_err(|_| VfsError::IoError)?;
                }
                3 => isr = mapped,
                4 if size >= 16 => config = mapped,
                _ => {}
            }
        }
        if common == 0 || notify == 0 || isr == 0 || config == 0 {
            return Err(VfsError::NoDevice);
        }
        {
            let mut cfg = pci::config_for(device.address).map_err(|_| VfsError::IoError)?;
            let command = cfg.read_u16(4).map_err(|_| VfsError::IoError)?;
            cfg.write_u16(4, (command | 6) & !0x400)
                .map_err(|_| VfsError::IoError)?;
        }
        wr8(common + 20, 0);
        let deadline = crate::time::uptime_micros() + 1_000_000;
        while rd8(common + 20) != 0 {
            if crate::time::uptime_micros() > deadline {
                return Err(VfsError::TimedOut);
            }
            core::hint::spin_loop();
        }
        wr8(common + 20, 3);
        wr32(common, 0);
        let low = rd32(common + 4);
        wr32(common, 1);
        let high = rd32(common + 4);
        if high & 1 == 0 {
            return Err(VfsError::NotSupported);
        }
        let features = (low as u64 & 0x3f) | (1u64 << 32);
        wr32(common + 8, 0);
        wr32(common + 12, features as u32);
        wr32(common + 8, 1);
        wr32(common + 12, (features >> 32) as u32);
        wr8(common + 20, 11);
        if rd8(common + 20) & 8 == 0 {
            return Err(VfsError::NotSupported);
        }
        let msix = configure_irq(device, &capabilities)?;
        wr16(common + 16, if msix { 0 } else { 0xffff });
        let control = Queue::new(common, notify, mult, 0, msix)?;
        let cursor = Queue::new(common, notify, mult, 1, msix)?;
        let result = Self {
            address: device.address,
            common,
            isr,
            config,
            features,
            host_visible,
            queues: [SpinLock::new(control), SpinLock::new(cursor)],
            failed: AtomicBool::new(false),
            irq_count: AtomicU32::new(0),
            completed: crate::sched::WaitQueue::new(),
        };
        wr8(common + 20, 15);
        Ok(result)
    }
    pub fn config32(&self, offset: u64) -> u32 {
        loop {
            let generation = rd8(self.common + 21);
            let value = rd32(self.config + offset);
            fence(Ordering::Acquire);
            if generation == rd8(self.common + 21) {
                return value;
            }
        }
    }
    pub fn submit(
        &self,
        cursor: bool,
        command: &[u8],
        response_size: usize,
        retain: Vec<Arc<dyn crate::vfs::Inode>>,
    ) -> Result<Arc<Request>, VfsError> {
        if self.failed.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        if command.len() < 24
            || command.len() > LIMIT
            || response_size < 24
            || response_size > LIMIT
        {
            return Err(VfsError::InvalidArgument);
        }
        let cmd = Dma::new(command.len())?;
        cmd.write(0, command)?;
        let response = Dma::new(response_size)?;
        let request = Arc::new(Request {
            done: AtomicBool::new(false),
            error: AtomicI32::new(0),
            response_len: AtomicU32::new(0),
            command: cmd,
            response,
            retain: SpinLock::new(retain),
        });
        self.queues[usize::from(cursor)]
            .lock()
            .submit(request.clone())?;
        Ok(request)
    }
    pub fn poll(&self) {
        if self.failed.load(Ordering::Acquire) {
            return;
        }
        let mut retired = 0;
        for queue in &self.queues {
            let result = queue.lock().complete();
            match result {
                Ok(count) => retired += count,
                Err(_) => {
                    self.fail();
                    return;
                }
            }
        }
        if retired != 0 {
            self.completed.wake_all();
            crate::drm::syncobj::fence_event();
        }
    }
    pub fn interrupt(&self) {
        let _cause = rd8(self.isr);
        self.irq_count.fetch_add(1, Ordering::Relaxed);
        self.poll();
    }
    pub fn wait(&self, request: &Request) -> Result<Vec<u8>, VfsError> {
        /// The host usually answers within tens of microseconds; spinning
        /// that long is cheaper than a sleep/wake round trip.
        const SPIN_MICROS: u64 = 50;
        let started = crate::time::uptime_micros();
        let deadline = started.saturating_add(5_000_000);
        loop {
            self.poll();
            if request.done.load(Ordering::Acquire) {
                break;
            }
            let now = crate::time::uptime_micros();
            if now > deadline {
                self.fail();
                return Err(VfsError::TimedOut);
            }
            if now.saturating_sub(started) < SPIN_MICROS || crate::sched::current().is_none() {
                core::hint::spin_loop();
                continue;
            }
            // The completion interrupt polls the rings and wakes this queue.
            let _ = self.completed.wait_event(
                || request.done.load(Ordering::Acquire),
                false,
                Some(deadline),
            );
        }
        request.response()
    }
    pub fn call(&self, command: &[u8], response_size: usize) -> Result<Vec<u8>, VfsError> {
        let request = self.submit(false, command, response_size, Vec::new())?;
        self.wait(&request)
    }
    pub fn fail(&self) {
        if self.failed.swap(true, Ordering::AcqRel) {
            return;
        }
        wr8(self.common + 20, 0);
        let mut spins = 0;
        while rd8(self.common + 20) != 0 && spins < 100000 {
            spins += 1;
            core::hint::spin_loop();
        }
        if let Ok(mut cfg) = pci::config_for(self.address) {
            if let Ok(command) = cfg.read_u16(4) {
                let _ = cfg.write_u16(4, command & !4);
            }
        }
        // Keep pending buffers quarantined in the queue if the reset did not acknowledge.
        let quiesced = rd8(self.common + 20) == 0;
        if !quiesced {
            crate::kwarn!(
                "VirtIO GPU reset at {} did not quiesce; quarantining in-flight DMA buffers",
                self.address
            );
        }
        for queue in &self.queues {
            let mut q = queue.lock();
            for slot in &mut q.pending {
                if let Some((_, request)) = slot {
                    request.error.store(-19, Ordering::Relaxed);
                    request.done.store(true, Ordering::Release);
                }
                if quiesced {
                    if let Some((_, request)) = slot {
                        request.retain.lock().clear();
                    }
                    *slot = None;
                }
            }
        }
        self.completed.wake_all();
        crate::drm::syncobj::fence_event();
    }
}
fn irq(_irq: u32) {
    if let Some(gpu) = super::device() {
        gpu.transport.interrupt();
    }
}
fn configure_irq(device: &DeviceInfo, caps: &[pci::Capability]) -> Result<bool, VfsError> {
    #[cfg(target_arch = "x86_64")]
    {
        let cap = caps
            .iter()
            .find(|c| c.id == 0x11)
            .ok_or(VfsError::NotSupported)?;
        let (control, table) = {
            let cfg = pci::config_for(device.address).map_err(|_| VfsError::IoError)?;
            (
                cfg.read_u16(cap.offset as u16 + 2)
                    .map_err(|_| VfsError::IoError)?,
                cfg.read_u32(cap.offset as u16 + 4)
                    .map_err(|_| VfsError::IoError)?,
            )
        };
        let mut cfg = pci::config_for(device.address).map_err(|_| VfsError::IoError)?;
        let (bar_base, bar_size) = bar(&mut cfg, device, (table & 7) as usize)?;
        let table_offset = (table & !7) as u64;
        if table_offset.checked_add(16).is_none_or(|e| e > bar_size) {
            return Err(VfsError::InvalidArgument);
        }
        let table_phys = bar_base
            .checked_add(table_offset)
            .ok_or(VfsError::InvalidArgument)?;
        let table = vmm::map_device_range(table_phys, 16).map_err(|_| VfsError::IoError)?;
        let message = crate::arch::apic::request_msi(irq).map_err(|_| VfsError::IoError)?;
        cfg.write_u16(cap.offset as u16 + 2, control | 0xc000)
            .map_err(|_| VfsError::IoError)?;
        wr32(table + 12, 1);
        wr64(table, message.address);
        wr32(table + 8, message.data);
        fence(Ordering::SeqCst);
        wr32(table + 12, 0);
        cfg.write_u16(cap.offset as u16 + 2, (control | 0x8000) & !0x4000)
            .map_err(|_| VfsError::IoError)?;
        Ok(true)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = caps;
        let source = pci_irq(device)?;
        #[cfg(target_arch = "aarch64")]
        {
            crate::arch::gic::request(source, irq, false).map_err(|_| VfsError::IoError)?;
            crate::arch::gic::set_mask(source, false).map_err(|_| VfsError::IoError)?;
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::arch::plic::request(source, irq).map_err(|_| VfsError::IoError)?;
            crate::arch::plic::set_mask(source, false).map_err(|_| VfsError::IoError)?;
        }
        Ok(false)
    }
}
#[cfg(not(target_arch = "x86_64"))]
fn pci_irq(device: &DeviceInfo) -> Result<u32, VfsError> {
    let address = crate::boot::dtb_address().ok_or(VfsError::NotSupported)?;
    let fdt =
        crate::hardware::fdt::Fdt::from_boot_address(address).map_err(|_| VfsError::IoError)?;
    let host = fdt
        .find_compatible("pci-host-ecam-generic")
        .ok_or(VfsError::NoDevice)?;
    let mask = host
        .property("interrupt-map-mask")
        .ok_or(VfsError::NotSupported)?
        .value;
    let map = host
        .property("interrupt-map")
        .ok_or(VfsError::NotSupported)?
        .value;
    fn cell(b: &[u8], i: usize) -> Result<u32, VfsError> {
        Ok(u32::from_be_bytes(
            b.get(i * 4..i * 4 + 4)
                .ok_or(VfsError::IoError)?
                .try_into()
                .unwrap(),
        ))
    }
    let key = [
        (device.address.bus() as u32) << 16
            | (device.address.device() as u32) << 11
            | (device.address.function() as u32) << 8,
        0,
        0,
        device.interrupt_pin as u32,
    ];
    let mut index = 0;
    while index * 4 < map.len() {
        let phandle = cell(map, index + 4)?;
        let parent = fdt.find_by_phandle(phandle).ok_or(VfsError::IoError)?;
        let ac = parent.cell("#address-cells").unwrap_or(0) as usize;
        let ic = parent.cell("#interrupt-cells").ok_or(VfsError::IoError)? as usize;
        if ac > 4 || ic == 0 || ic > 4 {
            return Err(VfsError::IoError);
        }
        let matches = (0..4).all(|i| {
            cell(map, index + i)
                .ok()
                .zip(cell(mask, i).ok())
                .is_some_and(|(v, m)| v & m == key[i] & m)
        });
        let start = index + 5 + ac;
        let next = start + ic;
        if next * 4 > map.len() {
            return Err(VfsError::IoError);
        }
        if matches {
            return if ic == 1 {
                cell(map, start)
            } else if ic == 3 {
                match cell(map, start)? {
                    0 => Ok(cell(map, start + 1)? + 32),
                    1 => Ok(cell(map, start + 1)? + 16),
                    _ => Err(VfsError::NotSupported),
                }
            } else {
                Err(VfsError::NotSupported)
            };
        }
        index = next;
    }
    Err(VfsError::NotSupported)
}
