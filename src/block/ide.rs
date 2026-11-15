/*
 *
 *       src/block/ide.rs
 *       PCI IDE (PATA) storage driver with Bus Master DMA (PRDT) and LBA28/48 PIO
 *
 *       2026/10/23 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Industrial-grade IDE/PATA driver supporting PCI Bus Master DMA (PRDT)
//! with automatic fallback to LBA48 and LBA28 Programmed I/O (PIO).

use super::{BlockDevice, BlockDeviceInfo, BlockError};
#[cfg(target_arch = "x86_64")]
use super::{BlockDeviceKind, SECTOR_SIZE};
use crate::sync::SpinLock;
#[cfg(target_arch = "x86_64")]
use crate::{kinfo, kwarn, mm::pmm, pci};
#[cfg(target_arch = "x86_64")]
use alloc::format;
use alloc::{string::String, sync::Arc, vec::Vec};

// ATA Register Offsets from Command Base
const ATA_REG_DATA: u16 = 0x00;
const ATA_REG_FEATURES: u16 = 0x01;
const ATA_REG_SECCOUNT0: u16 = 0x02;
const ATA_REG_LBA0: u16 = 0x03;
const ATA_REG_LBA1: u16 = 0x04;
const ATA_REG_LBA2: u16 = 0x05;
const ATA_REG_HDDEVSEL: u16 = 0x06;
const ATA_REG_COMMAND: u16 = 0x07;
const ATA_REG_STATUS: u16 = 0x07;

// ATA Status Bits
const ATA_SR_BSY: u8 = 0x80;
const ATA_SR_DRDY: u8 = 0x40;
const ATA_SR_DF: u8 = 0x20;
const ATA_SR_DRQ: u8 = 0x08;
const ATA_SR_ERR: u8 = 0x01;

// ATA Commands
const ATA_CMD_READ_PIO: u8 = 0x20;
const ATA_CMD_READ_PIO_EXT: u8 = 0x24;
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
const ATA_CMD_WRITE_PIO: u8 = 0x30;
const ATA_CMD_WRITE_PIO_EXT: u8 = 0x34;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_READ_DMA: u8 = 0xC8;
const ATA_CMD_WRITE_DMA: u8 = 0xCA;
const ATA_CMD_CACHE_FLUSH: u8 = 0xE7;
const ATA_CMD_CACHE_FLUSH_EXT: u8 = 0xEA;
const ATA_CMD_IDENTIFY: u8 = 0xEC;

// Bus Master IDE Register Offsets
const BMIDE_REG_CMD: u16 = 0x00;
const BMIDE_REG_STATUS: u16 = 0x02;
const BMIDE_REG_PRDT: u16 = 0x04;

// Bus Master IDE Command & Status Bits
const BMIDE_CMD_START: u8 = 0x01;
const BMIDE_CMD_READ: u8 = 0x08; // Read from disk (write to memory)
const BMIDE_STATUS_ACTIVE: u8 = 0x01;
const BMIDE_STATUS_ERR: u8 = 0x02;
const BMIDE_STATUS_IRQ: u8 = 0x04;

/// Physical Region Descriptor Table (PRDT) entry (8 bytes, 4-byte aligned, cannot cross 64K).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct PrdEntry {
    phys_addr: u32,
    byte_count: u16,
    flags: u16, // Bit 15 = End of Table (0x8000)
}

struct IdeChannelState {
    cmd_base: u16,
    ctrl_base: u16,
    bm_base: u16,
    dma_page_phys: u64,
}

pub struct IdeDisk {
    info: BlockDeviceInfo,
    channel: Arc<SpinLock<IdeChannelState>>,
    drive_select: u8, // 0 = Master, 1 = Slave
    lba48: bool,
    dma_capable: bool,
}

#[cfg(target_arch = "x86_64")]
mod pio {
    #[inline]
    pub unsafe fn inb(port: u16) -> u8 {
        let val: u8;
        unsafe {
            core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
        }
        val
    }

    #[inline]
    pub unsafe fn outb(port: u16, val: u8) {
        unsafe {
            core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
        }
    }

    #[inline]
    pub unsafe fn inw(port: u16) -> u16 {
        let val: u16;
        unsafe {
            core::arch::asm!("in ax, dx", out("ax") val, in("dx") port, options(nomem, nostack, preserves_flags));
        }
        val
    }

    #[inline]
    pub unsafe fn outw(port: u16, val: u16) {
        unsafe {
            core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack, preserves_flags));
        }
    }

    #[inline]
    pub unsafe fn outl(port: u16, val: u32) {
        unsafe {
            core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl IdeChannelState {
    #[inline]
    fn delay_400ns(&self) {
        unsafe {
            for _ in 0..4 {
                let _ = pio::inb(self.ctrl_base);
            }
        }
    }

    fn wait_not_busy(&self, timeout_micros: u64, fallback_polls: usize) -> Result<u8, BlockError> {
        super::poll_until(timeout_micros, fallback_polls, || {
            let status = unsafe { pio::inb(self.cmd_base + ATA_REG_STATUS) };
            if status == 0xFF {
                return Err(BlockError::NotReady);
            }
            if (status & ATA_SR_BSY) == 0 {
                return Ok(Some(status));
            }
            Ok(None)
        })
    }

    fn wait_drq(&self, timeout_micros: u64, fallback_polls: usize) -> Result<(), BlockError> {
        super::poll_until(timeout_micros, fallback_polls, || {
            let status = unsafe { pio::inb(self.cmd_base + ATA_REG_STATUS) };
            if (status & ATA_SR_BSY) == 0 {
                if (status & (ATA_SR_ERR | ATA_SR_DF)) != 0 {
                    return Err(BlockError::DeviceFault);
                }
                if (status & ATA_SR_DRQ) != 0 {
                    return Ok(Some(()));
                }
            }
            Ok(None)
        })
    }

    fn setup_lba(&self, drive: u8, lba: u64, count: u16, lba48: bool) {
        unsafe {
            if lba48 {
                pio::outb(self.cmd_base + ATA_REG_HDDEVSEL, 0x40 | ((drive & 1) << 4));
                pio::outb(self.cmd_base + ATA_REG_SECCOUNT0, (count >> 8) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA0, ((lba >> 24) & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA1, ((lba >> 32) & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA2, ((lba >> 40) & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_SECCOUNT0, (count & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA0, (lba & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA1, ((lba >> 8) & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA2, ((lba >> 16) & 0xFF) as u8);
            } else {
                pio::outb(
                    self.cmd_base + ATA_REG_HDDEVSEL,
                    0xE0 | ((drive & 1) << 4) | (((lba >> 24) & 0x0F) as u8),
                );
                pio::outb(self.cmd_base + ATA_REG_FEATURES, 0);
                pio::outb(self.cmd_base + ATA_REG_SECCOUNT0, (count & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA0, (lba & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA1, ((lba >> 8) & 0xFF) as u8);
                pio::outb(self.cmd_base + ATA_REG_LBA2, ((lba >> 16) & 0xFF) as u8);
            }
        }
    }

    fn transfer_dma_sectors(
        &mut self,
        drive: u8,
        lba: u64,
        sector_count: usize,
        lba48: bool,
        write: bool,
        buf: &mut [u8],
    ) -> Result<(), BlockError> {
        let byte_len = sector_count * SECTOR_SIZE;
        if self.bm_base == 0
            || self.dma_page_phys == 0
            || self.dma_page_phys + 8192 > (1u64 << 32)
            || sector_count == 0
            || byte_len > 4096
        {
            return Err(BlockError::Unsupported);
        }

        let page_virt = pmm::phys_to_virt(self.dma_page_phys);
        let prdt_ptr = page_virt as *mut PrdEntry;
        let data_phys = (self.dma_page_phys + 4096) as u32;
        let data_ptr = (page_virt + 4096) as *mut u8;

        unsafe {
            if write {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), data_ptr, byte_len);
            }
            prdt_ptr.write_volatile(PrdEntry {
                phys_addr: data_phys,
                byte_count: byte_len as u16,
                flags: 0x8000, // End of Table
            });

            // Stop any active DMA, clear status error/IRQ bits, and load PRDT physical address
            pio::outb(self.bm_base + BMIDE_REG_CMD, 0);
            pio::outb(
                self.bm_base + BMIDE_REG_STATUS,
                BMIDE_STATUS_IRQ | BMIDE_STATUS_ERR,
            );
            pio::outl(self.bm_base + BMIDE_REG_PRDT, self.dma_page_phys as u32);
        }

        self.wait_not_busy(1_000_000, 100_000)?;
        self.setup_lba(drive, lba, sector_count as u16, lba48);
        self.delay_400ns();

        let ata_cmd = match (write, lba48) {
            (false, true) => ATA_CMD_READ_DMA_EXT,
            (false, false) => ATA_CMD_READ_DMA,
            (true, true) => ATA_CMD_WRITE_DMA_EXT,
            (true, false) => ATA_CMD_WRITE_DMA,
        };
        let bm_dir = if write { 0 } else { BMIDE_CMD_READ };

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe {
            pio::outb(self.cmd_base + ATA_REG_COMMAND, ata_cmd);
            pio::outb(self.bm_base + BMIDE_REG_CMD, bm_dir | BMIDE_CMD_START);
        }

        let poll_res = super::poll_until(5_000_000, 500_000, || {
            let bm_status = unsafe { pio::inb(self.bm_base + BMIDE_REG_STATUS) };
            let ata_status = unsafe { pio::inb(self.cmd_base + ATA_REG_STATUS) };
            if (bm_status & BMIDE_STATUS_ERR) != 0 || (ata_status & (ATA_SR_ERR | ATA_SR_DF)) != 0 {
                return Err(BlockError::IoError);
            }
            if (bm_status & BMIDE_STATUS_IRQ) != 0
                || ((bm_status & BMIDE_STATUS_ACTIVE) == 0 && (ata_status & ATA_SR_BSY) == 0)
            {
                return Ok(Some(()));
            }
            Ok(None)
        });

        unsafe {
            pio::outb(self.bm_base + BMIDE_REG_CMD, 0);
            pio::outb(
                self.bm_base + BMIDE_REG_STATUS,
                BMIDE_STATUS_IRQ | BMIDE_STATUS_ERR,
            );
        }

        poll_res?;

        if !write {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            unsafe {
                core::ptr::copy_nonoverlapping(data_ptr, buf.as_mut_ptr(), byte_len);
            }
        }
        Ok(())
    }

    fn transfer_pio_sector(
        &mut self,
        drive: u8,
        lba: u64,
        lba48: bool,
        write: bool,
        sector_buf: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), BlockError> {
        self.wait_not_busy(1_000_000, 200_000)?;
        self.setup_lba(drive, lba, 1, lba48);
        self.delay_400ns();

        let cmd = match (write, lba48) {
            (false, true) => ATA_CMD_READ_PIO_EXT,
            (false, false) => ATA_CMD_READ_PIO,
            (true, true) => ATA_CMD_WRITE_PIO_EXT,
            (true, false) => ATA_CMD_WRITE_PIO,
        };

        unsafe {
            pio::outb(self.cmd_base + ATA_REG_COMMAND, cmd);
        }
        self.delay_400ns();
        self.wait_drq(1_000_000, 200_000)?;

        unsafe {
            if write {
                for i in 0..256 {
                    let word = u16::from_le_bytes([sector_buf[i * 2], sector_buf[i * 2 + 1]]);
                    pio::outw(self.cmd_base + ATA_REG_DATA, word);
                }
            } else {
                for i in 0..256 {
                    let word = pio::inw(self.cmd_base + ATA_REG_DATA);
                    let bytes = word.to_le_bytes();
                    sector_buf[i * 2] = bytes[0];
                    sector_buf[i * 2 + 1] = bytes[1];
                }
            }
        }
        self.delay_400ns();
        self.wait_not_busy(1_000_000, 200_000)?;
        Ok(())
    }

    fn flush_drive(&mut self, drive: u8, lba48: bool) -> Result<(), BlockError> {
        self.wait_not_busy(1_000_000, 200_000)?;
        unsafe {
            pio::outb(self.cmd_base + ATA_REG_HDDEVSEL, 0xE0 | ((drive & 1) << 4));
            self.delay_400ns();
            let cmd = if lba48 {
                ATA_CMD_CACHE_FLUSH_EXT
            } else {
                ATA_CMD_CACHE_FLUSH
            };
            pio::outb(self.cmd_base + ATA_REG_COMMAND, cmd);
        }
        self.delay_400ns();
        self.wait_not_busy(5_000_000, 500_000)?;
        Ok(())
    }
}

impl BlockDevice for IdeDisk {
    fn info(&self) -> BlockDeviceInfo {
        self.info.clone()
    }

    #[cfg(target_arch = "x86_64")]
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

        let mut ch = self.channel.lock();
        let mut done = 0usize;
        while done < count {
            let batch = (count - done).min(8);
            let bytes = batch * SECTOR_SIZE;
            let off = done * SECTOR_SIZE;
            let cur_lba = lba + done as u64;
            if self.dma_capable
                && ch
                    .transfer_dma_sectors(
                        self.drive_select,
                        cur_lba,
                        batch,
                        self.lba48,
                        false,
                        &mut buf[off..off + bytes],
                    )
                    .is_ok()
            {
                done += batch;
                continue;
            }
            let mut sector = [0u8; SECTOR_SIZE];
            for i in 0..batch {
                ch.transfer_pio_sector(
                    self.drive_select,
                    cur_lba + i as u64,
                    self.lba48,
                    false,
                    &mut sector,
                )?;
                let sec_off = off + i * SECTOR_SIZE;
                buf[sec_off..sec_off + SECTOR_SIZE].copy_from_slice(&sector);
            }
            done += batch;
        }
        Ok(())
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn read_blocks(&self, _lba: u64, _count: usize, _buf: &mut [u8]) -> Result<(), BlockError> {
        Err(BlockError::Unsupported)
    }

    #[cfg(target_arch = "x86_64")]
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

        let mut ch = self.channel.lock();
        let mut done = 0usize;
        let mut temp = [0u8; 4096];
        while done < count {
            let batch = (count - done).min(8);
            let bytes = batch * SECTOR_SIZE;
            let off = done * SECTOR_SIZE;
            let cur_lba = lba + done as u64;
            temp[..bytes].copy_from_slice(&buf[off..off + bytes]);
            if self.dma_capable
                && ch
                    .transfer_dma_sectors(
                        self.drive_select,
                        cur_lba,
                        batch,
                        self.lba48,
                        true,
                        &mut temp[..bytes],
                    )
                    .is_ok()
            {
                done += batch;
                continue;
            }
            let mut sector = [0u8; SECTOR_SIZE];
            for i in 0..batch {
                let sec_off = off + i * SECTOR_SIZE;
                sector.copy_from_slice(&buf[sec_off..sec_off + SECTOR_SIZE]);
                ch.transfer_pio_sector(
                    self.drive_select,
                    cur_lba + i as u64,
                    self.lba48,
                    true,
                    &mut sector,
                )?;
            }
            done += batch;
        }
        Ok(())
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn write_blocks(&self, _lba: u64, _count: usize, _buf: &[u8]) -> Result<(), BlockError> {
        Err(BlockError::Unsupported)
    }

    #[cfg(target_arch = "x86_64")]
    fn flush(&self) -> Result<(), BlockError> {
        self.channel
            .lock()
            .flush_drive(self.drive_select, self.lba48)
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn flush(&self) -> Result<(), BlockError> {
        Ok(())
    }
}

pub(crate) fn extract_ata_string(words: &[u16]) -> String {
    let mut bytes = Vec::with_capacity(words.len() * 2);
    for &w in words {
        bytes.push((w >> 8) as u8);
        bytes.push((w & 0xFF) as u8);
    }
    let s = core::str::from_utf8(&bytes).unwrap_or("").trim();
    String::from(s)
}

#[cfg(target_arch = "x86_64")]
fn probe_drive(
    channel: &Arc<SpinLock<IdeChannelState>>,
    drive: u8,
    disk_index: usize,
) -> Option<Arc<dyn BlockDevice>> {
    let ch = channel.lock();
    unsafe {
        let status_initial = pio::inb(ch.cmd_base + ATA_REG_STATUS);
        if status_initial == 0xFF {
            return None;
        }
        pio::outb(ch.cmd_base + ATA_REG_HDDEVSEL, 0xA0 | ((drive & 1) << 4));
        ch.delay_400ns();
        pio::outb(ch.cmd_base + ATA_REG_SECCOUNT0, 0);
        pio::outb(ch.cmd_base + ATA_REG_LBA0, 0);
        pio::outb(ch.cmd_base + ATA_REG_LBA1, 0);
        pio::outb(ch.cmd_base + ATA_REG_LBA2, 0);
        pio::outb(ch.cmd_base + ATA_REG_COMMAND, ATA_CMD_IDENTIFY);
        ch.delay_400ns();

        let status = pio::inb(ch.cmd_base + ATA_REG_STATUS);
        if status == 0 || status == 0xFF {
            return None;
        }

        if ch.wait_not_busy(1_000_000, 100_000).is_err() {
            return None;
        }

        let lba1 = pio::inb(ch.cmd_base + ATA_REG_LBA1);
        let lba2 = pio::inb(ch.cmd_base + ATA_REG_LBA2);
        if lba1 != 0 || lba2 != 0 {
            if (lba1 == 0x14 && lba2 == 0xEB) || (lba1 == 0x69 && lba2 == 0x96) {
                kinfo!(
                    "IDE: ATAPI optical device detected on port {:#x} drive {}",
                    ch.cmd_base,
                    drive
                );
            }
            return None;
        }

        if ch.wait_drq(1_000_000, 100_000).is_err() {
            return None;
        }

        let mut words = [0u16; 256];
        for slot in &mut words {
            *slot = pio::inw(ch.cmd_base + ATA_REG_DATA);
        }

        let model = {
            let s = extract_ata_string(&words[27..47]);
            if s.is_empty() {
                String::from("Generic IDE Disk")
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

        let dma_supported = (words[49] & (1 << 8)) != 0
            && ch.bm_base != 0
            && ch.dma_page_phys != 0
            && ch.dma_page_phys < (1u64 << 32);

        if ch.bm_base != 0 {
            // Enable drive bit in Bus Master status register (bit 5 for drive 0, bit 6 for drive 1)
            let bm_stat = pio::inb(ch.bm_base + BMIDE_REG_STATUS);
            pio::outb(
                ch.bm_base + BMIDE_REG_STATUS,
                bm_stat | (1 << (5 + (drive & 1))),
            );
        }

        let letter = (b'a' + (disk_index as u8)) as char;
        let name = format!("hd{}", letter);
        kinfo!(
            "IDE: {} '{}' ({} MiB, {} sectors, LBA48={}, DMA={})",
            name,
            model,
            (total_blocks * SECTOR_SIZE as u64) / (1024 * 1024),
            total_blocks,
            lba48,
            dma_supported
        );

        drop(ch);
        Some(Arc::new(IdeDisk {
            info: BlockDeviceInfo {
                name,
                model,
                serial,
                kind: BlockDeviceKind::Ide,
                block_size: SECTOR_SIZE,
                total_blocks,
                read_only: false,
                removable: false,
            },
            channel: channel.clone(),
            drive_select: drive,
            lba48,
            dma_capable: dma_supported,
        }))
    }
}

/// Discover and initialize PCI IDE controllers and attached PATA drives.
pub fn init() -> Vec<Arc<dyn BlockDevice>> {
    #[allow(unused_mut)]
    let mut found: Vec<Arc<dyn BlockDevice>> = Vec::new();

    #[cfg(target_arch = "x86_64")]
    {
        for dev in pci::devices() {
            if dev.class != 0x01 || dev.subclass != 0x01 {
                continue;
            }
            // Enable PCI I/O Space (bit 0) and Bus Master (bit 2)
            if let Ok(mut cfg) = pci::config_for(dev.address)
                && let Ok(cmd) = cfg.read_u16(0x04)
            {
                let _ = cfg.write_u16(0x04, cmd | 0x0005);
            }

            let bm_bar = match dev.bars[4] {
                Some(pci::Bar::Io { base }) => base as u16,
                _ => 0,
            };

            let pri_native = (dev.programming_interface & 0x01) != 0;
            let sec_native = (dev.programming_interface & 0x04) != 0;

            let pri_cmd = if pri_native {
                match dev.bars[0] {
                    Some(pci::Bar::Io { base }) if base != 0 => base as u16,
                    _ => 0x1F0,
                }
            } else {
                0x1F0
            };
            let pri_ctrl = if pri_native {
                match dev.bars[1] {
                    Some(pci::Bar::Io { base }) if base != 0 => (base + 2) as u16,
                    _ => 0x3F6,
                }
            } else {
                0x3F6
            };

            let sec_cmd = if sec_native {
                match dev.bars[2] {
                    Some(pci::Bar::Io { base }) if base != 0 => base as u16,
                    _ => 0x170,
                }
            } else {
                0x170
            };
            let sec_ctrl = if sec_native {
                match dev.bars[3] {
                    Some(pci::Bar::Io { base }) if base != 0 => (base + 2) as u16,
                    _ => 0x376,
                }
            } else {
                0x376
            };

            kinfo!(
                "IDE: PCI controller {} {:04x}:{:04x} (pri={:#x}/{:#x}, sec={:#x}/{:#x}, bm={:#x})",
                dev.address,
                dev.vendor_id,
                dev.device_id,
                pri_cmd,
                pri_ctrl,
                sec_cmd,
                sec_ctrl,
                bm_bar
            );

            for (ch_idx, (cmd_base, ctrl_base, bm_base)) in [
                (pri_cmd, pri_ctrl, bm_bar),
                (sec_cmd, sec_ctrl, if bm_bar != 0 { bm_bar + 8 } else { 0 }),
            ]
            .into_iter()
            .enumerate()
            {
                let dma_page_phys = pmm::alloc(2).unwrap_or(0);
                if dma_page_phys == 0 {
                    kwarn!(
                        "IDE: failed to allocate DMA bounce page for channel {}",
                        ch_idx
                    );
                } else {
                    unsafe {
                        core::ptr::write_bytes(
                            pmm::phys_to_virt(dma_page_phys) as *mut u8,
                            0,
                            8192,
                        );
                    }
                }
                let channel = Arc::new(SpinLock::new(IdeChannelState {
                    cmd_base,
                    ctrl_base,
                    bm_base,
                    dma_page_phys,
                }));

                for drive in 0..2 {
                    let disk_idx = ch_idx * 2 + (drive as usize);
                    if let Some(disk) = probe_drive(&channel, drive, disk_idx) {
                        found.push(disk);
                    }
                }
            }
        }
    }

    found
}
