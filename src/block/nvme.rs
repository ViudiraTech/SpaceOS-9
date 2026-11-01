/*
 *
 *       src/block/nvme.rs
 *       PCIe NVMe 1.4 controller driver with Admin/IO Queue Pairs and PRP DMA
 *
 *       2026/10/23 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Industrial-grade NVMe (Non-Volatile Memory Express) driver.
//!
//! Implements Admin & I/O Submission/Completion Queue Pairs, Phase Tag tracking,
//! Doorbell strides, Identify Controller/Namespace, and PRP1/PRP2 DMA transfers.

use super::{BlockDevice, BlockDeviceInfo, BlockDeviceKind, BlockError};
use crate::{
    kinfo, kwarn,
    mm::{pmm, vmm},
    pci,
    sync::Mutex,
};
use alloc::{format, string::String, sync::Arc, vec::Vec};

// NVMe Controller Register Offsets (BAR0)
const NVME_REG_CAP: usize = 0x00; // 64-bit Controller Capabilities
const NVME_REG_VS: usize = 0x08; // 32-bit Version
const NVME_REG_INTMS: usize = 0x0C; // 32-bit Interrupt Mask Set
const NVME_REG_CC: usize = 0x14; // 32-bit Controller Configuration
const NVME_REG_CSTS: usize = 0x1C; // 32-bit Controller Status
const NVME_REG_AQA: usize = 0x24; // 32-bit Admin Queue Attributes
const NVME_REG_ASQ: usize = 0x28; // 64-bit Admin Submission Queue Base Address
const NVME_REG_ACQ: usize = 0x30; // 64-bit Admin Completion Queue Base Address
const NVME_REG_DBS: usize = 0x1000; // Doorbell Registers Start

// Controller Configuration (CC) & Status (CSTS) bits
const NVME_CC_EN: u32 = 1 << 0;
const NVME_CC_IOSQES_64: u32 = 6 << 16; // 2^6 = 64 bytes
const NVME_CC_IOCQES_16: u32 = 4 << 20; // 2^4 = 16 bytes
const NVME_CSTS_RDY: u32 = 1 << 0;
const NVME_CSTS_CFS: u32 = 1 << 1;

// Admin Opcodes
const NVME_ADMIN_CREATE_SQ: u8 = 0x01;
const NVME_ADMIN_CREATE_CQ: u8 = 0x05;
const NVME_ADMIN_IDENTIFY: u8 = 0x06;

// NVM I/O Opcodes
const NVME_IO_FLUSH: u8 = 0x00;
const NVME_IO_WRITE: u8 = 0x01;
const NVME_IO_READ: u8 = 0x02;

const COMMAND_TIMEOUT_MICROS: u64 = 30_000_000;
const QUEUE_DEPTH: usize = 64; // 64 entries: SQ = 4096 B (1 page), CQ = 1024 B (1 page)

/// 64-byte NVMe Submission Queue Entry (SQE).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmeCommand {
    cdw0: u32, // [7:0] Opcode, [15:8] Flags, [31:16] Command ID
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    mptr: u64,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
}

/// 16-byte NVMe Completion Queue Entry (CQE).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmeCompletion {
    result: u32,
    rsvd: u32,
    sq_head: u16,
    sq_id: u16,
    cid: u16,
    status: u16, // bit 0 = Phase Tag (P), bits 1..15 = Status Field
}

struct QueuePair {
    qid: u16,
    sq_phys: u64,
    sq_virt: usize,
    cq_phys: u64,
    cq_virt: usize,
    sq_tail: u16,
    cq_head: u16,
    phase: u16,
    next_cid: u16,
}

impl QueuePair {
    fn allocate(qid: u16) -> Option<Self> {
        let sq_phys = pmm::alloc(1)?;
        let cq_phys = pmm::alloc(1)?;
        let sq_virt = pmm::phys_to_virt(sq_phys) as usize;
        let cq_virt = pmm::phys_to_virt(cq_phys) as usize;
        unsafe {
            core::ptr::write_bytes(sq_virt as *mut u8, 0, 4096);
            core::ptr::write_bytes(cq_virt as *mut u8, 0, 4096);
        }
        Some(Self {
            qid,
            sq_phys,
            sq_virt,
            cq_phys,
            cq_virt,
            sq_tail: 0,
            cq_head: 0,
            phase: 1,
            next_cid: 1,
        })
    }

    fn submit_and_wait(
        &mut self,
        mmio_base: usize,
        db_stride: usize,
        mut cmd: NvmeCommand,
    ) -> Result<u32, BlockError> {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1).max(1);
        cmd.cdw0 = (cmd.cdw0 & 0xFFFF) | (u32::from(cid) << 16);

        let sq_ptr = self.sq_virt as *mut NvmeCommand;
        let cq_ptr = self.cq_virt as *const NvmeCompletion;

        unsafe {
            sq_ptr.add(self.sq_tail as usize).write_volatile(cmd);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        self.sq_tail = (self.sq_tail + 1) % (QUEUE_DEPTH as u16);
        let sq_db_offset = NVME_REG_DBS + (2 * (self.qid as usize)) * db_stride;
        unsafe {
            ((mmio_base + sq_db_offset) as *mut u32).write_volatile(u32::from(self.sq_tail));
        }

        // Poll Completion Queue Entry for matching Phase Tag
        // A flush can take as long as the host's fsync, so the limit is wall
        // time. Spin briefly for the fast read case, then let other tasks run.
        let mut cqe = NvmeCompletion::default();
        let mut completed = false;
        let started = crate::time::uptime_micros();
        let mut spins = 0u32;
        while crate::time::uptime_micros().saturating_sub(started) < COMMAND_TIMEOUT_MICROS {
            cqe = unsafe { cq_ptr.add(self.cq_head as usize).read_volatile() };
            if (cqe.status & 1) == self.phase {
                completed = true;
                break;
            }
            spins += 1;
            // Interrupts off means a spin lock is held somewhere up the stack;
            // whoever needs that lock could then stop this CPU from ever
            // scheduling the task back.
            if spins > 2000 && crate::sched::current().is_some() && crate::arch::irqs_enabled() {
                crate::sched::yield_now();
            } else {
                core::hint::spin_loop();
            }
        }
        if !completed {
            return Err(BlockError::Timeout);
        }

        // Advance CQ Head and flip Phase Tag on wrap-around
        self.cq_head += 1;
        if self.cq_head as usize >= QUEUE_DEPTH {
            self.cq_head = 0;
            self.phase ^= 1;
        }

        let cq_db_offset = NVME_REG_DBS + (2 * (self.qid as usize) + 1) * db_stride;
        unsafe {
            ((mmio_base + cq_db_offset) as *mut u32).write_volatile(u32::from(self.cq_head));
        }

        let status_code = cqe.status >> 1;
        if status_code != 0 || cqe.cid != cid {
            kwarn!(
                "NVMe: q{} command {:#x} failed: status={:#x} cid={} expected={} sq_head={} cdw10={:#x} cdw12={:#x}",
                self.qid,
                cmd.cdw0 & 0xFF,
                status_code,
                cqe.cid,
                cid,
                cqe.sq_head,
                cmd.cdw10,
                cmd.cdw12
            );
            return Err(BlockError::IoError);
        }

        Ok(cqe.result)
    }
}

struct NvmeControllerState {
    mmio_base: usize,
    db_stride: usize,
    admin_qp: QueuePair,
    io_qp: QueuePair,
    dma_page_phys: u64,
    dma_page_virt: usize,
}

pub struct NvmeNamespace {
    info: BlockDeviceInfo,
    ctrl: Arc<Mutex<NvmeControllerState>>,
    nsid: u32,
    lba_shift: u8,
}

impl BlockDevice for NvmeNamespace {
    fn info(&self) -> BlockDeviceInfo {
        self.info.clone()
    }

    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), BlockError> {
        let block_size = self.info.block_size;
        let needed = count
            .checked_mul(block_size)
            .ok_or(BlockError::InvalidBuffer)?;
        if buf.len() < needed {
            return Err(BlockError::InvalidBuffer);
        }
        let end = lba
            .checked_add(count as u64)
            .ok_or(BlockError::OutOfRange)?;
        if end > self.info.total_blocks {
            return Err(BlockError::OutOfRange);
        }

        let mut ctrl = self.ctrl.lock();
        let max_blocks_per_page = (4096 / block_size).max(1);
        let mut done = 0usize;

        while done < count {
            let batch = (count - done).min(max_blocks_per_page);
            let bytes = batch * block_size;
            let cur_lba = lba + done as u64;

            let cmd = NvmeCommand {
                cdw0: u32::from(NVME_IO_READ),
                nsid: self.nsid,
                prp1: ctrl.dma_page_phys,
                cdw10: cur_lba as u32,
                cdw11: (cur_lba >> 32) as u32,
                cdw12: (batch as u32) - 1, // 0-based NLB
                ..NvmeCommand::default()
            };

            let mmio = ctrl.mmio_base;
            let stride = ctrl.db_stride;
            ctrl.io_qp.submit_and_wait(mmio, stride, cmd)?;

            let off = done * block_size;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    ctrl.dma_page_virt as *const u8,
                    buf[off..off + bytes].as_mut_ptr(),
                    bytes,
                );
            }
            done += batch;
        }
        Ok(())
    }

    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), BlockError> {
        let block_size = self.info.block_size;
        let needed = count
            .checked_mul(block_size)
            .ok_or(BlockError::InvalidBuffer)?;
        if buf.len() < needed {
            return Err(BlockError::InvalidBuffer);
        }
        let end = lba
            .checked_add(count as u64)
            .ok_or(BlockError::OutOfRange)?;
        if end > self.info.total_blocks {
            return Err(BlockError::OutOfRange);
        }

        let mut ctrl = self.ctrl.lock();
        let max_blocks_per_page = (4096 / block_size).max(1);
        let mut done = 0usize;

        while done < count {
            let batch = (count - done).min(max_blocks_per_page);
            let bytes = batch * block_size;
            let cur_lba = lba + done as u64;
            let off = done * block_size;

            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf[off..off + bytes].as_ptr(),
                    ctrl.dma_page_virt as *mut u8,
                    bytes,
                );
            }

            let cmd = NvmeCommand {
                cdw0: u32::from(NVME_IO_WRITE),
                nsid: self.nsid,
                prp1: ctrl.dma_page_phys,
                cdw10: cur_lba as u32,
                cdw11: (cur_lba >> 32) as u32,
                cdw12: (batch as u32) - 1,
                ..NvmeCommand::default()
            };

            let mmio = ctrl.mmio_base;
            let stride = ctrl.db_stride;
            ctrl.io_qp.submit_and_wait(mmio, stride, cmd)?;
            done += batch;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), BlockError> {
        let mut ctrl = self.ctrl.lock();
        let cmd = NvmeCommand {
            cdw0: u32::from(NVME_IO_FLUSH),
            nsid: self.nsid,
            ..NvmeCommand::default()
        };
        let mmio = ctrl.mmio_base;
        let stride = ctrl.db_stride;
        ctrl.io_qp.submit_and_wait(mmio, stride, cmd)?;
        Ok(())
    }
}

#[inline]
unsafe fn read_u32(mmio: usize, offset: usize) -> u32 {
    unsafe { ((mmio + offset) as *const u32).read_volatile() }
}

#[inline]
unsafe fn write_u32(mmio: usize, offset: usize, val: u32) {
    unsafe { ((mmio + offset) as *mut u32).write_volatile(val) }
}

#[inline]
unsafe fn read_u64(mmio: usize, offset: usize) -> u64 {
    unsafe {
        let lo = read_u32(mmio, offset) as u64;
        let hi = read_u32(mmio, offset + 4) as u64;
        lo | (hi << 32)
    }
}

#[inline]
unsafe fn write_u64(mmio: usize, offset: usize, val: u64) {
    unsafe {
        write_u32(mmio, offset, val as u32);
        write_u32(mmio, offset + 4, (val >> 32) as u32);
    }
}

fn init_controller(dev: &pci::DeviceInfo, ctrl_index: usize) -> Option<Vec<Arc<dyn BlockDevice>>> {
    // Enable PCI Memory Space (bit 1) and Bus Master (bit 2)
    if let Ok(mut cfg) = pci::config_for(dev.address)
        && let Ok(cmd) = cfg.read_u16(0x04)
    {
        let _ = cfg.write_u16(0x04, cmd | 0x0006);
    }

    let bar0_phys = match dev.bars[0] {
        Some(pci::Bar::Memory64 { base, .. }) => base,
        Some(pci::Bar::Memory32 { base, .. }) => u64::from(base),
        _ => return None,
    };
    if bar0_phys == 0 {
        return None;
    }

    let Ok(mmio_u64) = vmm::map_device_range(bar0_phys, 0x4000) else {
        kwarn!("NVMe: failed to map BAR0 at {:#x}", bar0_phys);
        return None;
    };
    let mmio = mmio_u64 as usize;

    unsafe {
        let cap = read_u64(mmio, NVME_REG_CAP);
        let vs = read_u32(mmio, NVME_REG_VS);
        let dstrd = ((cap >> 32) & 0x0F) as usize;
        let db_stride = 4usize << dstrd;
        let mpsmin = ((cap >> 48) & 0x0F) as u32;
        if mpsmin > 0 {
            kwarn!("NVMe: controller requires minimum page size > 4 KiB");
            return None;
        }

        // Mask all interrupts (polling completion queues for deterministic latency)
        write_u32(mmio, NVME_REG_INTMS, u32::MAX);

        // Disable controller if currently enabled
        let cc = read_u32(mmio, NVME_REG_CC);
        if (cc & NVME_CC_EN) != 0 {
            write_u32(mmio, NVME_REG_CC, cc & !NVME_CC_EN);
        }
        let mut disabled = false;
        for _ in 0..1_000_000 {
            if (read_u32(mmio, NVME_REG_CSTS) & NVME_CSTS_RDY) == 0 {
                disabled = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !disabled {
            kwarn!("NVMe: timeout waiting for CSTS.RDY == 0");
            return None;
        }

        let mut admin_qp = QueuePair::allocate(0)?;
        let io_qp = QueuePair::allocate(1)?;
        let dma_page_phys = pmm::alloc(1)?;
        let dma_page_virt = pmm::phys_to_virt(dma_page_phys) as usize;
        core::ptr::write_bytes(dma_page_virt as *mut u8, 0, 4096);

        // Program Admin Queue Attributes (AQA) and Base Addresses (ASQ, ACQ)
        let q_attr = ((QUEUE_DEPTH as u32 - 1) << 16) | (QUEUE_DEPTH as u32 - 1);
        write_u32(mmio, NVME_REG_AQA, q_attr);
        write_u64(mmio, NVME_REG_ASQ, admin_qp.sq_phys);
        write_u64(mmio, NVME_REG_ACQ, admin_qp.cq_phys);

        // Enable controller with 64B IOSQES and 16B IOCQES
        let new_cc = NVME_CC_EN | NVME_CC_IOSQES_64 | NVME_CC_IOCQES_16;
        write_u32(mmio, NVME_REG_CC, new_cc);

        let mut ready = false;
        for _ in 0..1_000_000 {
            let csts = read_u32(mmio, NVME_REG_CSTS);
            if (csts & NVME_CSTS_CFS) != 0 {
                kwarn!("NVMe: controller fatal status (CSTS.CFS) during enable");
                return None;
            }
            if (csts & NVME_CSTS_RDY) != 0 {
                ready = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !ready {
            kwarn!("NVMe: timeout waiting for CSTS.RDY == 1");
            return None;
        }

        // 1. Admin Identify Controller (CNS = 0x01)
        let id_ctrl_cmd = NvmeCommand {
            cdw0: u32::from(NVME_ADMIN_IDENTIFY),
            nsid: 0,
            prp1: dma_page_phys,
            cdw10: 1, // CNS = 0x01 (Identify Controller)
            ..NvmeCommand::default()
        };
        if admin_qp
            .submit_and_wait(mmio, db_stride, id_ctrl_cmd)
            .is_err()
        {
            kwarn!("NVMe: Identify Controller command failed");
            return None;
        }

        let id_data = core::slice::from_raw_parts(dma_page_virt as *const u8, 4096);
        let serial = String::from(core::str::from_utf8(&id_data[4..24]).unwrap_or("").trim());
        let model = {
            let s = core::str::from_utf8(&id_data[24..64]).unwrap_or("").trim();
            if s.is_empty() {
                String::from("NVMe SSD")
            } else {
                String::from(s)
            }
        };
        let nn = u32::from_le_bytes(id_data[516..520].try_into().unwrap());

        kinfo!(
            "NVMe: nvme{} {} {:04x}:{:04x} '{}' serial='{}' version={}.{}.{} namespaces={}",
            ctrl_index,
            dev.address,
            dev.vendor_id,
            dev.device_id,
            model,
            serial,
            vs >> 16,
            (vs >> 8) & 0xFF,
            vs & 0xFF,
            nn
        );

        // 2. Admin Create I/O Completion Queue (QID = 1)
        let create_cq_cmd = NvmeCommand {
            cdw0: u32::from(NVME_ADMIN_CREATE_CQ),
            prp1: io_qp.cq_phys,
            cdw10: ((QUEUE_DEPTH as u32 - 1) << 16) | 1, // QSIZE | QID=1
            cdw11: 1,                                    // PC = 1 (Physically Contiguous)
            ..NvmeCommand::default()
        };
        if admin_qp
            .submit_and_wait(mmio, db_stride, create_cq_cmd)
            .is_err()
        {
            kwarn!("NVMe: Create I/O Completion Queue failed");
            return None;
        }

        // 3. Admin Create I/O Submission Queue (QID = 1, CQID = 1)
        let create_sq_cmd = NvmeCommand {
            cdw0: u32::from(NVME_ADMIN_CREATE_SQ),
            prp1: io_qp.sq_phys,
            cdw10: ((QUEUE_DEPTH as u32 - 1) << 16) | 1, // QSIZE | QID=1
            cdw11: (1 << 16) | 1,                        // CQID = 1 | PC = 1
            ..NvmeCommand::default()
        };
        if admin_qp
            .submit_and_wait(mmio, db_stride, create_sq_cmd)
            .is_err()
        {
            kwarn!("NVMe: Create I/O Submission Queue failed");
            return None;
        }

        let ctrl_state = Arc::new(Mutex::new(NvmeControllerState {
            mmio_base: mmio,
            db_stride,
            admin_qp,
            io_qp,
            dma_page_phys,
            dma_page_virt,
        }));

        let mut namespaces: Vec<Arc<dyn BlockDevice>> = Vec::new();
        let max_ns = nn.min(16);
        for nsid in 1..=max_ns {
            let mut ctrl_guard = ctrl_state.lock();
            core::ptr::write_bytes(ctrl_guard.dma_page_virt as *mut u8, 0, 4096);
            let id_ns_cmd = NvmeCommand {
                cdw0: u32::from(NVME_ADMIN_IDENTIFY),
                nsid,
                prp1: ctrl_guard.dma_page_phys,
                cdw10: 0, // CNS = 0x00 (Identify Namespace)
                ..NvmeCommand::default()
            };
            let m = ctrl_guard.mmio_base;
            let s = ctrl_guard.db_stride;
            if ctrl_guard
                .admin_qp
                .submit_and_wait(m, s, id_ns_cmd)
                .is_err()
            {
                continue;
            }
            let ns_data = core::slice::from_raw_parts(ctrl_guard.dma_page_virt as *const u8, 4096);
            let nsze = u64::from_le_bytes(ns_data[0..8].try_into().unwrap());
            if nsze == 0 {
                continue;
            }
            let flbas = (ns_data[26] & 0x0F) as usize;
            let lbaf_offset = 128 + flbas * 4;
            let lbads = ns_data[lbaf_offset + 2];
            let lba_shift = if (9..=12).contains(&lbads) { lbads } else { 9 };
            let block_size = 1usize << lba_shift;
            drop(ctrl_guard);

            let name = format!("nvme{}n{}", ctrl_index, nsid);
            kinfo!(
                "NVMe: namespace {} ({} MiB, {} blocks @ {} B)",
                name,
                (nsze * block_size as u64) / (1024 * 1024),
                nsze,
                block_size
            );

            let ns_dev: Arc<dyn BlockDevice> = Arc::new(NvmeNamespace {
                info: BlockDeviceInfo {
                    name,
                    model: model.clone(),
                    serial: serial.clone(),
                    kind: BlockDeviceKind::Nvme,
                    block_size,
                    total_blocks: nsze,
                    read_only: false,
                    removable: false,
                },
                ctrl: ctrl_state.clone(),
                nsid,
                lba_shift,
            });
            namespaces.push(ns_dev);
        }

        Some(namespaces)
    }
}

/// Discover and initialize all PCIe NVMe controllers and active namespaces.
pub fn init() -> Vec<Arc<dyn BlockDevice>> {
    let mut found: Vec<Arc<dyn BlockDevice>> = Vec::new();
    let mut ctrl_index = 0usize;

    for dev in pci::devices() {
        if dev.class != 0x01 || dev.subclass != 0x08 || dev.programming_interface != 0x02 {
            continue;
        }
        if let Some(mut namespaces) = init_controller(&dev, ctrl_index) {
            ctrl_index += 1;
            found.append(&mut namespaces);
        }
    }

    found
}
