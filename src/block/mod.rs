/*
 *
 *       src/block/mod.rs
 *       Unified block device subsystem, LRU sector cache and hardware driver registry
 *
 *       2026/10/23 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Block device layer unifying IDE (PATA), AHCI (SATA), NVMe, RAMDisk,
//! MBR/GPT partitions, and an LRU sector cache.

pub mod accounting;
pub mod ahci;
pub mod bench;
pub mod cache;
pub mod ide;
pub mod nvme;
pub mod partition;
pub mod ramdisk;

use crate::{kinfo, sync::SpinLock};
use alloc::{string::String, sync::Arc, vec::Vec};

pub const SECTOR_SIZE: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockError {
    IoError,
    OutOfRange,
    ReadOnly,
    NotReady,
    InvalidBuffer,
    Unsupported,
    Timeout,
    DeviceFault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockDeviceKind {
    Ide,
    Ahci,
    Nvme,
    RamDisk,
    Partition,
    /// A disk registered by a loadable driver through the HAL block class.
    Hal,
}

impl BlockDeviceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ide => "ide",
            Self::Ahci => "ahci",
            Self::Nvme => "nvme",
            Self::RamDisk => "ram",
            Self::Partition => "part",
            Self::Hal => "hal",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BlockDeviceInfo {
    pub name: String,
    pub model: String,
    pub serial: String,
    pub kind: BlockDeviceKind,
    pub block_size: usize,
    pub total_blocks: u64,
    pub read_only: bool,
    pub removable: bool,
}

impl BlockDeviceInfo {
    pub fn capacity_bytes(&self) -> u64 {
        self.total_blocks.saturating_mul(self.block_size as u64)
    }
}

pub trait BlockDevice: Send + Sync {
    fn info(&self) -> BlockDeviceInfo;
    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), BlockError>;
    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), BlockError>;
    fn flush(&self) -> Result<(), BlockError>;
}

struct Entry {
    device: Arc<dyn BlockDevice>,
    /// The whole disk a partition belongs to.
    parent: Option<usize>,
}

/// Slot index is the device id used by accounting, the sector cache and
/// devfs inode numbers. Removed devices leave an empty slot and ids are never
/// reused, so nothing keyed by an old id can reach a new device.
static REGISTRY: SpinLock<Vec<Option<Entry>>> = SpinLock::new(Vec::new());

/// Register a block device and automatically discover any MBR/GPT partitions
/// on it. Devices registered after boot are published to /dev and sysfs at
/// once. Returns the device id.
pub fn register_device(dev: Arc<dyn BlockDevice>) -> usize {
    let is_whole_disk = dev.info().kind != BlockDeviceKind::Partition;
    let id = {
        let mut reg = REGISTRY.lock();
        reg.push(Some(Entry {
            device: dev.clone(),
            parent: None,
        }));
        reg.len() - 1
    };
    let mut names = alloc::vec![dev.info().name];
    if is_whole_disk {
        let parts = partition::probe_partitions(&dev);
        if !parts.is_empty() {
            let mut reg = REGISTRY.lock();
            for p in parts {
                names.push(p.info().name);
                reg.push(Some(Entry {
                    device: p,
                    parent: Some(id),
                }));
            }
        }
    }
    for name in names {
        crate::device::publish_block(&name);
    }
    id
}

/// Remove a whole disk and its partitions (hot-unplug). Mounted filesystems
/// keep their `Arc` and see the driver's errors from now on.
pub fn unregister_device(id: usize) {
    let removed: Vec<(usize, Arc<dyn BlockDevice>)> = {
        let mut reg = REGISTRY.lock();
        let mut removed = Vec::new();
        for (index, slot) in reg.iter_mut().enumerate() {
            let matches = index == id || slot.as_ref().is_some_and(|e| e.parent == Some(id));
            if matches && let Some(entry) = slot.take() {
                removed.push((index, entry.device));
            }
        }
        removed
    };
    for (index, device) in removed.iter().rev() {
        crate::device::unpublish(&device.info().name);
        cache::invalidate_device(*index);
    }
}

/// Return a snapshot of all registered block devices and partitions.
pub fn devices() -> Vec<Arc<dyn BlockDevice>> {
    devices_with_ids().into_iter().map(|(_, dev)| dev).collect()
}

/// Registered devices with their stable ids.
pub fn devices_with_ids() -> Vec<(usize, Arc<dyn BlockDevice>)> {
    REGISTRY
        .lock()
        .iter()
        .enumerate()
        .filter_map(|(id, slot)| slot.as_ref().map(|e| (id, e.device.clone())))
        .collect()
}

/// Look up a block device by name (`sda`, `nvme0n1`, `ram0p1`, etc.) and return `(dev_id, device)`.
pub fn find_device(name: &str) -> Option<(usize, Arc<dyn BlockDevice>)> {
    let reg = REGISTRY.lock();
    for (idx, slot) in reg.iter().enumerate() {
        if let Some(entry) = slot
            && entry.device.info().name == name
        {
            return Some((idx, entry.device.clone()));
        }
    }
    None
}

/// First unused `<prefix><letter>` disk name (`sda`, `sdb`, ...).
pub fn allocate_name(prefix: &str) -> Option<String> {
    (b'a'..=b'z')
        .map(|letter| alloc::format!("{}{}", prefix, letter as char))
        .find(|name| find_device(name).is_none())
}

/// Byte-granular cached read from a block device.
pub fn read_at(
    dev_id: usize,
    dev: &Arc<dyn BlockDevice>,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, BlockError> {
    let mut request = accounting::Request::start(dev_id, false);
    let result = read_at_inner(dev_id, dev, offset, buf);
    if let Ok(bytes) = result {
        request.complete(bytes);
    }
    result
}

fn read_at_inner(
    dev_id: usize,
    dev: &Arc<dyn BlockDevice>,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, BlockError> {
    let info = dev.info();
    let capacity = info.capacity_bytes();
    if offset >= capacity || buf.is_empty() {
        return Ok(0);
    }
    let to_read = (capacity - offset).min(buf.len() as u64) as usize;
    let mut done = 0usize;
    let mut sector_buf = [0u8; SECTOR_SIZE];

    while done < to_read {
        let cur_off = offset + done as u64;
        let lba = cur_off / (SECTOR_SIZE as u64);
        let sec_off = (cur_off % (SECTOR_SIZE as u64)) as usize;
        let chunk = (SECTOR_SIZE - sec_off).min(to_read - done);

        cache::read_sector(dev_id, dev, lba, &mut sector_buf)?;
        buf[done..done + chunk].copy_from_slice(&sector_buf[sec_off..sec_off + chunk]);
        done += chunk;
    }

    Ok(done)
}

/// Byte-granular cached write to a block device.
pub fn write_at(
    dev_id: usize,
    dev: &Arc<dyn BlockDevice>,
    offset: u64,
    buf: &[u8],
) -> Result<usize, BlockError> {
    let mut request = accounting::Request::start(dev_id, true);
    let result = write_at_inner(dev_id, dev, offset, buf);
    if let Ok(bytes) = result {
        request.complete(bytes);
    }
    result
}

fn write_at_inner(
    dev_id: usize,
    dev: &Arc<dyn BlockDevice>,
    offset: u64,
    buf: &[u8],
) -> Result<usize, BlockError> {
    let info = dev.info();
    if info.read_only {
        return Err(BlockError::ReadOnly);
    }
    let capacity = info.capacity_bytes();
    if offset >= capacity {
        return Err(BlockError::OutOfRange);
    }
    if buf.is_empty() {
        return Ok(0);
    }
    let to_write = (capacity - offset).min(buf.len() as u64) as usize;
    let mut done = 0usize;
    let mut sector_buf = [0u8; SECTOR_SIZE];

    while done < to_write {
        let cur_off = offset + done as u64;
        let lba = cur_off / (SECTOR_SIZE as u64);
        let sec_off = (cur_off % (SECTOR_SIZE as u64)) as usize;
        let chunk = (SECTOR_SIZE - sec_off).min(to_write - done);

        if sec_off != 0 || chunk != SECTOR_SIZE {
            cache::read_sector(dev_id, dev, lba, &mut sector_buf)?;
        }
        sector_buf[sec_off..sec_off + chunk].copy_from_slice(&buf[done..done + chunk]);
        cache::write_sector(dev_id, dev, lba, &sector_buf)?;
        done += chunk;
    }

    Ok(done)
}

/// Provision an unformatted whole-disk block device with an MBR + FAT32 partition if LBA 0 is blank.
fn ensure_disk_partitioned(dev: &Arc<dyn BlockDevice>) {
    let info = dev.info();
    if info.block_size != SECTOR_SIZE || info.total_blocks < 256 || info.read_only {
        return;
    }
    let mut mbr = [0u8; SECTOR_SIZE];
    if dev.read_blocks(0, 1, &mut mbr).is_err() {
        return;
    }
    if mbr[510] == 0x55 && mbr[511] == 0xAA {
        return;
    }
    // Only auto-partition if LBA 0 is completely zeroed
    if !mbr.iter().all(|&b| b == 0) {
        return;
    }
    let start_lba = 64u32;
    let part_sectors = (info.total_blocks as u32).saturating_sub(start_lba);
    if part_sectors < 128 {
        return;
    }
    let _ = partition::write_mbr_single_partition(dev, 0x0C, start_lba, part_sectors);
}

/// Whether boot may write partition tables and filesystems to hardware disks.
///
/// Never on by default: an unpartitioned disk may hold a whole-disk filesystem
/// such as the ext4 root. Self-test builds use throwaway blank images, and the
/// `block.autoformat` command-line word enables it explicitly.
fn autoformat_allowed() -> bool {
    cfg!(feature = "boot-self-test")
        || crate::boot::cmdline()
            .split_ascii_whitespace()
            .any(|word| word == "block.autoformat")
}

/// Initialize all storage controllers (IDE, AHCI, NVMe, and boot RAMDisk) and probe partitions.
pub fn init() {
    // 1. Create and format the built-in 2 MiB RAMDisk (`ram0` -> `ram0p1` with FAT32)
    let ram0: Arc<dyn BlockDevice> = ramdisk::RamDisk::new("ram0", 4096);
    ensure_disk_partitioned(&ram0);
    register_device(ram0);
    if let Some((_, ram0p1)) = find_device("ram0p1") {
        let _ = crate::vfs::fat32::format_fat32(&ram0p1, "SPACEFAT");
    }

    // 2. Discover PCI IDE (PATA) drives
    for disk in ide::init() {
        if autoformat_allowed() {
            ensure_disk_partitioned(&disk);
        }
        let name = disk.info().name;
        register_device(disk);
        let part_name = alloc::format!("{}1", name);
        if autoformat_allowed()
            && let Some((_, part)) = find_device(&part_name)
        {
            let mut boot_sec = [0u8; SECTOR_SIZE];
            if part.read_blocks(0, 1, &mut boot_sec).is_ok() && boot_sec[510] != 0x55 {
                let _ = crate::vfs::fat32::format_fat32(&part, "IDEDISK");
            }
        }
    }

    // 3. Discover PCI AHCI (SATA) drives
    for disk in ahci::init() {
        if autoformat_allowed() {
            ensure_disk_partitioned(&disk);
        }
        let name = disk.info().name;
        register_device(disk);
        let part_name = alloc::format!("{}1", name);
        if autoformat_allowed()
            && let Some((_, part)) = find_device(&part_name)
        {
            let mut boot_sec = [0u8; SECTOR_SIZE];
            if part.read_blocks(0, 1, &mut boot_sec).is_ok() && boot_sec[510] != 0x55 {
                let _ = crate::vfs::fat32::format_fat32(&part, "SATADISK");
            }
        }
    }

    // 4. Discover PCIe NVMe namespaces
    for disk in nvme::init() {
        if autoformat_allowed() {
            ensure_disk_partitioned(&disk);
        }
        let name = disk.info().name;
        register_device(disk);
        let part_name = alloc::format!("{}p1", name);
        if autoformat_allowed()
            && let Some((_, part)) = find_device(&part_name)
        {
            let mut boot_sec = [0u8; SECTOR_SIZE];
            if part.read_blocks(0, 1, &mut boot_sec).is_ok() && boot_sec[510] != 0x55 {
                let _ = crate::vfs::fat32::format_fat32(&part, "NVMEDISK");
            }
        }
    }

    let total = REGISTRY.lock().len();
    kinfo!(
        "block: initialized ({} block devices/partitions online)",
        total
    );
}

#[cfg(feature = "boot-self-test")]
pub fn run_self_test() {
    let devs = devices();
    assert!(!devs.is_empty(), "block: no devices registered");

    // Verify sector write/flush/read on LBA 16 (reserved area between MBR and Partition 1 at LBA 64)
    // across every whole-disk device (RAMDisk, IDE, AHCI, NVMe).
    for (dev_id, dev) in devs.iter().enumerate() {
        let info = dev.info();
        if info.kind == BlockDeviceKind::Partition
            || info.read_only
            || info.block_size != SECTOR_SIZE
            || info.total_blocks <= 32
        {
            continue;
        }
        let test_lba = 16u64;
        let mut orig = [0u8; SECTOR_SIZE];
        dev.read_blocks(test_lba, 1, &mut orig)
            .expect("block self-test initial read failed");

        let mut pattern = [0u8; SECTOR_SIZE];
        for (i, b) in pattern.iter_mut().enumerate() {
            *b = ((i * 7 + dev_id * 13 + 0x5A) & 0xFF) as u8;
        }

        cache::write_sector(dev_id, dev, test_lba, &pattern)
            .expect("block self-test cached write failed");
        dev.flush().expect("block self-test flush failed");

        // Invalidate cache to force a real hardware DMA/PIO readback from the disk
        cache::invalidate_device(dev_id);

        let mut readback = [0u8; SECTOR_SIZE];
        cache::read_sector(dev_id, dev, test_lba, &mut readback)
            .expect("block self-test hardware readback failed");
        assert_eq!(
            readback, pattern,
            "block self-test data mismatch on {}",
            info.name
        );

        // Restore original sector
        cache::write_sector(dev_id, dev, test_lba, &orig).expect("block self-test restore failed");
        dev.flush().expect("block self-test restore flush failed");
    }

    crate::verbose::log(format_args!("block & storage drivers self-test passed"));
}
