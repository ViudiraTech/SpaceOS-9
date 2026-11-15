/*
 *
 *       src/block/ahci.rs
 *       PCI AHCI 1.0 (SATA) HBA storage driver with Command List, CFIS and PRDT DMA
 *
 *       2026/10/23 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Industrial-grade AHCI 1.0 SATA Host Bus Adapter (HBA) driver.
//!
//! Uses memory-mapped ABAR (`BAR5`), per-port Command Lists, Received FIS tables,
//! H2D Register FIS (`0x27`), and Physical Region Descriptor Table (PRDT) DMA.

use super::{
    BlockDevice, BlockDeviceInfo, BlockDeviceKind, BlockError, SECTOR_SIZE, ide::extract_ata_string,
};
use crate::{
    kinfo, kwarn,
    mm::{pmm, vmm},
    pci,
    sync::SpinLock,
};
use alloc::{format, string::String, sync::Arc, vec::Vec};

// HBA Generic Host Control Register Offsets
const HBA_CAP: usize = 0x00;
const HBA_GHC: usize = 0x04;
const HBA_IS: usize = 0x08;
const HBA_PI: usize = 0x0C;
const HBA_VS: usize = 0x10;
const HBA_CAP2: usize = 0x24;
const HBA_BOHC: usize = 0x28;

const HBA_GHC_AE: u32 = 1 << 31;
const HBA_CAP2_BOH: u32 = 1 << 0;
const HBA_BOHC_BOS: u32 = 1 << 0;
const HBA_BOHC_OOS: u32 = 1 << 1;

// Per-Port Register Offsets (relative to ABAR + 0x100 + port * 0x80)
const PORT_CLB: usize = 0x00;
const PORT_CLBU: usize = 0x04;
const PORT_FB: usize = 0x08;
const PORT_FBU: usize = 0x0C;
const PORT_IS: usize = 0x10;
const PORT_IE: usize = 0x14;
const PORT_CMD: usize = 0x18;
const PORT_TFD: usize = 0x20;
const PORT_SIG: usize = 0x24;
const PORT_SSTS: usize = 0x28;
const PORT_SERR: usize = 0x30;
const PORT_CI: usize = 0x38;

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;
const PORT_CMD_FR: u32 = 1 << 14;
const PORT_CMD_CR: u32 = 1 << 15;
const PORT_IS_TFES: u32 = 1 << 30;

// Device Signatures
const SATA_SIG_ATA: u32 = 0x0000_0101;
const SATA_SIG_ATAPI: u32 = 0xEB14_0101;

// FIS Types & ATA Commands
const FIS_TYPE_REG_H2D: u8 = 0x27;
const ATA_CMD_IDENTIFY: u8 = 0xEC;
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xEA;

const ATA_DEV_BUSY: u32 = 0x80;
const ATA_DEV_DRQ: u32 = 0x08;

/// AHCI Command Header (32 bytes, 32 slots = 1024 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct HbaCmdHeader {
    flags: u16, // bits 0..4: CFL (in DWORDs), bit 6: Write (W), bit 10: Clear Busy on R_OK
    prdtl: u16, // PRDT entry count
    prdbc: u32, // PRD byte count transferred
    ctba: u32,  // Command Table Base Address (128-byte aligned)
    ctbau: u32, // Command Table Base Address Upper 32-bits
    reserved: [u32; 4],
}

/// AHCI PRDT Entry (16 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct HbaPrdtEntry {
    dba: u32,  // Data Base Address
    dbau: u32, // Data Base Address Upper 32-bits
    reserved: u32,
    dbc: u32, // Byte count (bit 0..21 = bytes - 1, bit 31 = Interrupt on Completion)
}

/// Host-to-Device Register FIS (20 bytes inside 64-byte CFIS area).
#[repr(C)]
#[derive(Clone, Copy)]
struct FisRegH2D {
    fis_type: u8,
    pmport_c: u8, // bit 7 = 1 for Command, 0 for Control
    command: u8,
    featurel: u8,
    lba0: u8,
    lba1: u8,
    lba2: u8,
    device: u8,
    lba3: u8,
    lba4: u8,
    lba5: u8,
    featureh: u8,
    countl: u8,
    counth: u8,
    icc: u8,
    control: u8,
    reserved: [u8; 4],
}

/// AHCI Command Table (CFIS + ACMD + Reserved + PRDT).
#[repr(C, align(128))]
struct HbaCmdTable {
    cfis: [u8; 64],
    acmd: [u8; 16],
    reserved: [u8; 48],
    prdt: [HbaPrdtEntry; 8],
}

struct AhciPortState {
    port_mmio: usize,
    cmd_list_virt: usize,
    cmd_table_virt: usize,
    dma_buf_phys: u64,
    dma_buf_virt: usize,
}

pub struct AhciDisk {
    info: BlockDeviceInfo,
    port: SpinLock<AhciPortState>,
}

impl AhciPortState {
    #[inline]
    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { ((self.port_mmio + offset) as *const u32).read_volatile() }
    }

    #[inline]
    fn write_reg(&self, offset: usize, val: u32) {
        unsafe { ((self.port_mmio + offset) as *mut u32).write_volatile(val) }
    }

    fn stop_cmd(&self) {
        // Clear PxCMD.ST and wait for CR=0 before clearing PxCMD.FRE (AHCI 1.3.1 §10.1.2).
        let cmd = self.read_reg(PORT_CMD);
        self.write_reg(PORT_CMD, cmd & !PORT_CMD_ST);
        let _ = super::poll_until(1_000_000, 500_000, || {
            Ok(((self.read_reg(PORT_CMD) & PORT_CMD_CR) == 0).then_some(()))
        });

        let cmd = self.read_reg(PORT_CMD);
        self.write_reg(PORT_CMD, cmd & !PORT_CMD_FRE);
        let _ = super::poll_until(1_000_000, 500_000, || {
            Ok(((self.read_reg(PORT_CMD) & PORT_CMD_FR) == 0).then_some(()))
        });
    }

    fn start_cmd(&self) {
        let _ = super::poll_until(1_000_000, 500_000, || {
            Ok(((self.read_reg(PORT_CMD) & PORT_CMD_CR) == 0).then_some(()))
        });
        let cmd = self.read_reg(PORT_CMD);
        self.write_reg(PORT_CMD, cmd | PORT_CMD_FRE);
        let cmd = self.read_reg(PORT_CMD);
        self.write_reg(PORT_CMD, cmd | PORT_CMD_ST);
    }

    fn wait_idle(&self) -> Result<(), BlockError> {
        super::poll_until(1_000_000, 500_000, || {
            let tfd = self.read_reg(PORT_TFD);
            Ok(((tfd & (ATA_DEV_BUSY | ATA_DEV_DRQ)) == 0).then_some(()))
        })
    }

    fn exec_command(
        &mut self,
        ata_cmd: u8,
        lba: u64,
        sector_count: u16,
        write: bool,
        byte_len: usize,
    ) -> Result<(), BlockError> {
        self.wait_idle()?;

        // Clear pending port interrupt status and SATA error register
        self.write_reg(PORT_IS, u32::MAX);
        self.write_reg(PORT_SERR, u32::MAX);

        let hdr_ptr = self.cmd_list_virt as *mut HbaCmdHeader;
        let tbl_ptr = self.cmd_table_virt as *mut HbaCmdTable;

        unsafe {
            core::ptr::write_bytes(tbl_ptr as *mut u8, 0, core::mem::size_of::<HbaCmdTable>());

            let cfl = (core::mem::size_of::<FisRegH2D>() / 4) as u16;
            let w_flag = if write { 1 << 6 } else { 0 };
            let prdtl = if byte_len > 0 { 1 } else { 0 };

            let mut hdr = hdr_ptr.read_volatile();
            hdr.flags = cfl | w_flag;
            hdr.prdtl = prdtl;
            hdr.prdbc = 0;
            hdr_ptr.write_volatile(hdr);

            if byte_len > 0 {
                (*tbl_ptr).prdt.as_mut_ptr().write_volatile(HbaPrdtEntry {
                    dba: self.dma_buf_phys as u32,
                    dbau: (self.dma_buf_phys >> 32) as u32,
                    reserved: 0,
                    dbc: (byte_len as u32 - 1) | (1 << 31),
                });
            }

            let fis_ptr = (*tbl_ptr).cfis.as_mut_ptr() as *mut FisRegH2D;
            fis_ptr.write_volatile(FisRegH2D {
                fis_type: FIS_TYPE_REG_H2D,
                pmport_c: 0x80, // Command bit set
                command: ata_cmd,
                featurel: 0,
                lba0: (lba & 0xFF) as u8,
                lba1: ((lba >> 8) & 0xFF) as u8,
                lba2: ((lba >> 16) & 0xFF) as u8,
                device: 1 << 6, // LBA mode
                lba3: ((lba >> 24) & 0xFF) as u8,
                lba4: ((lba >> 32) & 0xFF) as u8,
                lba5: ((lba >> 40) & 0xFF) as u8,
                featureh: 0,
                countl: (sector_count & 0xFF) as u8,
                counth: ((sector_count >> 8) & 0xFF) as u8,
                icc: 0,
                control: 0,
                reserved: [0; 4],
            });
        }

        // Order descriptor and bounce-buffer writes before ringing PxCI.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Issue command in slot 0
        self.write_reg(PORT_CI, 1);

        super::poll_until(5_000_000, 2_000_000, || {
            let ci = self.read_reg(PORT_CI);
            let is = self.read_reg(PORT_IS);
            if (is & PORT_IS_TFES) != 0 {
                return Err(BlockError::IoError);
            }
            if (ci & 1) == 0 {
                let tfd = self.read_reg(PORT_TFD);
                if (tfd & 0x01) != 0 {
                    return Err(BlockError::DeviceFault);
                }
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                return Ok(Some(()));
            }
            Ok(None)
        })
    }
}

impl BlockDevice for AhciDisk {
    fn info(&self) -> BlockDeviceInfo {
        self.info.clone()
    }

    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), BlockError> {
        let needed = count
            .checked_mul(SECTOR_SIZE)
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

        let mut port = self.port.lock();
        let max_sectors_per_batch = 8usize; // 4 KiB DMA buffer = 8 * 512B
        let mut done = 0usize;
        while done < count {
            let batch = (count - done).min(max_sectors_per_batch);
            let bytes = batch * SECTOR_SIZE;
            port.exec_command(
                ATA_CMD_READ_DMA_EXT,
                lba + done as u64,
                batch as u16,
                false,
                bytes,
            )?;
            let off = done * SECTOR_SIZE;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    port.dma_buf_virt as *const u8,
                    buf[off..off + bytes].as_mut_ptr(),
                    bytes,
                );
            }
            done += batch;
        }
        Ok(())
    }

    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), BlockError> {
        let needed = count
            .checked_mul(SECTOR_SIZE)
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

        let mut port = self.port.lock();
        let max_sectors_per_batch = 8usize;
        let mut done = 0usize;
        while done < count {
            let batch = (count - done).min(max_sectors_per_batch);
            let bytes = batch * SECTOR_SIZE;
            let off = done * SECTOR_SIZE;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf[off..off + bytes].as_ptr(),
                    port.dma_buf_virt as *mut u8,
                    bytes,
                );
            }
            port.exec_command(
                ATA_CMD_WRITE_DMA_EXT,
                lba + done as u64,
                batch as u16,
                true,
                bytes,
            )?;
            done += batch;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), BlockError> {
        self.port
            .lock()
            .exec_command(ATA_CMD_FLUSH_CACHE_EXT, 0, 0, false, 0)
    }
}

fn init_port(abar_virt: usize, port_idx: usize, disk_index: usize) -> Option<Arc<dyn BlockDevice>> {
    let port_mmio = abar_virt + 0x100 + port_idx * 0x80;
    let ssts = unsafe { ((port_mmio + PORT_SSTS) as *const u32).read_volatile() };
    let det = ssts & 0x0F;
    let ipm = (ssts >> 8) & 0x0F;
    if det != 3 || ipm != 1 {
        return None;
    }

    let sig = unsafe { ((port_mmio + PORT_SIG) as *const u32).read_volatile() };
    if sig == SATA_SIG_ATAPI {
        kinfo!(
            "AHCI: port {} SATAPI optical drive detected (sig={:#010x})",
            port_idx,
            sig
        );
        return None;
    }
    if sig != SATA_SIG_ATA {
        return None;
    }

    // Allocate 2 physically contiguous pages:
    // Page 0:
    //   0x000..0x400 (1024 B): Command List (32 headers * 32 B)
    //   0x400..0x500 (256 B) : Received FIS
    //   0x800..0xA00 (512 B) : Command Table 0 (CFIS + PRDT)
    // Page 1:
    //   0x000..0x1000 (4096 B): DMA Bounce Buffer
    let ctrl_phys = pmm::alloc(1)?;
    let dma_buf_phys = pmm::alloc(1)?;
    let ctrl_virt = pmm::phys_to_virt(ctrl_phys) as usize;
    let dma_buf_virt = pmm::phys_to_virt(dma_buf_phys) as usize;

    unsafe {
        core::ptr::write_bytes(ctrl_virt as *mut u8, 0, 4096);
        core::ptr::write_bytes(dma_buf_virt as *mut u8, 0, 4096);
    }

    let clb_phys = ctrl_phys;
    let fb_phys = ctrl_phys + 0x400;
    let ctb_phys = ctrl_phys + 0x800;

    let mut port_state = AhciPortState {
        port_mmio,
        cmd_list_virt: ctrl_virt,
        cmd_table_virt: ctrl_virt + 0x800,
        dma_buf_phys,
        dma_buf_virt,
    };

    port_state.stop_cmd();

    port_state.write_reg(PORT_CLB, clb_phys as u32);
    port_state.write_reg(PORT_CLBU, (clb_phys >> 32) as u32);
    port_state.write_reg(PORT_FB, fb_phys as u32);
    port_state.write_reg(PORT_FBU, (fb_phys >> 32) as u32);

    // Point Command Header 0 to Command Table 0
    unsafe {
        let hdr0 = port_state.cmd_list_virt as *mut HbaCmdHeader;
        hdr0.write_volatile(HbaCmdHeader {
            flags: 0,
            prdtl: 0,
            prdbc: 0,
            ctba: ctb_phys as u32,
            ctbau: (ctb_phys >> 32) as u32,
            reserved: [0; 4],
        });
    }

    port_state.write_reg(PORT_SERR, u32::MAX);
    port_state.write_reg(PORT_IS, u32::MAX);
    port_state.write_reg(PORT_IE, 0);

    port_state.start_cmd();

    // Send ATA IDENTIFY DEVICE (0xEC)
    if port_state
        .exec_command(ATA_CMD_IDENTIFY, 0, 1, false, SECTOR_SIZE)
        .is_err()
    {
        kwarn!("AHCI: port {} IDENTIFY DEVICE failed", port_idx);
        return None;
    }

    let mut words = [0u16; 256];
    unsafe {
        let ptr = dma_buf_virt as *const u16;
        for (i, slot) in words.iter_mut().enumerate() {
            *slot = u16::from_le(ptr.add(i).read_volatile());
        }
    }

    let model = {
        let s = extract_ata_string(&words[27..47]);
        if s.is_empty() {
            String::from("AHCI SATA Disk")
        } else {
            s
        }
    };
    let serial = extract_ata_string(&words[10..20]);
    let lba48 = (words[83] & (1 << 10)) != 0;
    let sectors_48 = (words[100] as u64)
        | ((words[101] as u64) << 16)
        | ((words[102] as u64) << 32)
        | ((words[103] as u64) << 48);
    let sectors_28 = (words[60] as u64) | ((words[61] as u64) << 16);
    let total_blocks = if lba48 && sectors_48 != 0 {
        sectors_48
    } else {
        sectors_28
    };
    if total_blocks == 0 {
        return None;
    }

    let letter = (b'a' + (disk_index as u8)) as char;
    let name = format!("sd{}", letter);
    kinfo!(
        "AHCI: port {} -> {} '{}' ({} MiB, {} sectors)",
        port_idx,
        name,
        model,
        (total_blocks * SECTOR_SIZE as u64) / (1024 * 1024),
        total_blocks
    );

    Some(Arc::new(AhciDisk {
        info: BlockDeviceInfo {
            name,
            model,
            serial,
            kind: BlockDeviceKind::Ahci,
            block_size: SECTOR_SIZE,
            total_blocks,
            read_only: false,
            removable: false,
        },
        port: SpinLock::new(port_state),
    }))
}

/// Discover and initialize all PCI AHCI 1.0 controllers and attached SATA drives.
pub fn init() -> Vec<Arc<dyn BlockDevice>> {
    let mut found: Vec<Arc<dyn BlockDevice>> = Vec::new();
    let mut disk_index = 0usize;

    for dev in pci::devices() {
        if dev.class != 0x01 || dev.subclass != 0x06 || dev.programming_interface != 0x01 {
            continue;
        }

        // Enable PCI Memory Space (bit 1) and Bus Master (bit 2)
        if let Ok(mut cfg) = pci::config_for(dev.address)
            && let Ok(cmd) = cfg.read_u16(0x04)
        {
            let _ = cfg.write_u16(0x04, cmd | 0x0006);
        }

        let abar_phys = match dev.bars[5] {
            Some(pci::Bar::Memory32 { base, .. }) => u64::from(base),
            Some(pci::Bar::Memory64 { base, .. }) => base,
            _ => continue,
        };
        if abar_phys == 0 {
            continue;
        }

        let Ok(abar_virt_u64) = vmm::map_device_range(abar_phys, 0x2000) else {
            kwarn!("AHCI: failed to map ABAR at {:#x}", abar_phys);
            continue;
        };
        let abar_virt = abar_virt_u64 as usize;

        unsafe {
            // Perform BIOS/OS handoff if supported
            let cap2 = ((abar_virt + HBA_CAP2) as *const u32).read_volatile();
            if (cap2 & HBA_CAP2_BOH) != 0 {
                let bohc = ((abar_virt + HBA_BOHC) as *const u32).read_volatile();
                ((abar_virt + HBA_BOHC) as *mut u32).write_volatile(bohc | HBA_BOHC_OOS);
                let _ = super::poll_until(500_000, 100_000, || {
                    let cur = ((abar_virt + HBA_BOHC) as *const u32).read_volatile();
                    Ok(((cur & HBA_BOHC_BOS) == 0).then_some(()))
                });
            }

            // Enable AHCI mode in Global Host Control (GHC.AE)
            let ghc = ((abar_virt + HBA_GHC) as *const u32).read_volatile();
            ((abar_virt + HBA_GHC) as *mut u32).write_volatile(ghc | HBA_GHC_AE);
            ((abar_virt + HBA_IS) as *mut u32).write_volatile(u32::MAX);

            let cap = ((abar_virt + HBA_CAP) as *const u32).read_volatile();
            let vs = ((abar_virt + HBA_VS) as *const u32).read_volatile();
            let pi = ((abar_virt + HBA_PI) as *const u32).read_volatile();

            kinfo!(
                "AHCI: controller {} {:04x}:{:04x} ABAR={:#x} version={}.{:02x} slots={} ports_mask={:#x}",
                dev.address,
                dev.vendor_id,
                dev.device_id,
                abar_phys,
                vs >> 16,
                vs & 0xFFFF,
                ((cap >> 8) & 0x1F) + 1,
                pi
            );

            for port_idx in 0..32 {
                if (pi & (1 << port_idx)) != 0 {
                    if let Some(disk) = init_port(abar_virt, port_idx, disk_index) {
                        disk_index += 1;
                        found.push(disk);
                    }
                }
            }
        }
    }

    found
}
