/*
 *
 *       src/vfs/ext4/mod.rs
 *       Read-write ext4 filesystem with extents, metadata checksums and jbd2
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! ext4 driver.
//!
//! Supported on disk: 4 KiB or smaller blocks, extents, 64bit, flex_bg,
//! metadata_csum (including the seed feature), huge_file, dir_nlink,
//! extra_isize, the jbd2 journal and the orphan list. Unknown INCOMPAT features
//! refuse the mount, unknown RO_COMPAT features force read-only.
//!
//! Directories created with `dir_index` are readable because htree blocks are
//! also valid linear directories. Modifying one is refused with `ReadOnlyFs`
//! since this driver would leave the hash index stale; images for SpaceOS are
//! built without the feature.
//!
//! Locking: one `Mutex` serializes all filesystem state. Journal commit runs
//! under it, in `data=ordered` mode. A filesystem operation may not fail after
//! it has started mutating metadata unless the mutation is self-consistent,
//! because the running transaction always commits as a whole.

mod format;
mod journal;

use self::format::*;
use self::journal::{Journal, JournalMap};
use crate::{
    block::{BlockDevice, SECTOR_SIZE},
    kinfo, kwarn,
    sync::{Mutex, MutexGuard, SpinLock},
    vfs::{DirEntry, FileSystem, Inode, InodeKind, Stat, StatFs, VfsError},
};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

const EXT4_SUPER_MAGIC: u64 = 0xEF53;
const READAHEAD_BLOCKS: u64 = 32;
const MAX_NAME: usize = 255;
/// Metadata blocks one operation may dirty before the transaction must commit.
const MAX_WRITE_CHUNK_BLOCKS: u64 = 2048;
const COMMIT_INTERVAL_MICROS: u64 = 5_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extent {
    logical: u32,
    physical: u64,
    length: u32,
    unwritten: bool,
}

struct CacheEntry {
    data: alloc::boxed::Box<[u8]>,
    tick: u64,
    /// Modified by the running transaction: must not reach its final location.
    running: bool,
    /// Committed to the log: must reach its final location before a checkpoint.
    checkpoint: bool,
    /// File data waiting for write-out (`data=ordered`).
    data_dirty: bool,
}

struct BlockCache {
    entries: BTreeMap<u64, CacheEntry>,
    lru: BTreeMap<u64, u64>,
    tick: u64,
    limit: usize,
    hits: u64,
    misses: u64,
}

struct MemInode {
    raw: Vec<u8>,
    extents: Option<Vec<Extent>>,
    tree_blocks: Vec<u64>,
}

struct State {
    device: Arc<dyn BlockDevice>,
    geometry: Geometry,
    read_only: bool,
    aborted: bool,
    cache: BlockCache,
    journal: Option<Journal>,
    /// Metadata blocks of the running transaction.
    tx_blocks: BTreeSet<u64>,
    /// Committed metadata not yet written to its final location.
    checkpoint_blocks: BTreeSet<u64>,
    /// Dirty file data that must reach disk before the next commit.
    ordered: BTreeSet<u64>,
    revokes: BTreeSet<u64>,
    /// Blocks freed by the running transaction. Reusing one before commit could
    /// overwrite data a crash would still need.
    pending_free: BTreeSet<u64>,
    dirty_block_bitmaps: BTreeSet<u32>,
    dirty_inode_bitmaps: BTreeSet<u32>,
    dirty_groups: BTreeSet<u32>,
    super_dirty: bool,
    inodes: BTreeMap<u32, MemInode>,
    free_blocks: u64,
    free_inodes: u32,
    last_group: u32,
    tx_started_micros: u64,
    commits: u64,
}

pub struct Ext4Fs {
    state: Mutex<State>,
    geometry: Geometry,
    handles: SpinLock<BTreeMap<u32, Weak<Ext4Inode>>>,
    /// Inodes whose final handle dropped with no links left. Freed from task
    /// context because `Drop` may run where sleeping is not allowed.
    reap: SpinLock<Vec<u32>>,
    self_ref: SpinLock<Weak<Ext4Fs>>,
    flusher_started: AtomicBool,
}

pub struct Ext4Inode {
    fs: Arc<Ext4Fs>,
    ino: u32,
    kind: InodeKind,
}

fn now_seconds() -> u64 {
    crate::time::realtime_micros() / 1_000_000
}

fn file_type_of(kind: InodeKind) -> u8 {
    match kind {
        InodeKind::Regular => FT_REG,
        InodeKind::Directory => FT_DIR,
        InodeKind::CharDevice => FT_CHR,
        InodeKind::BlockDevice => FT_BLK,
        InodeKind::Fifo => FT_FIFO,
        InodeKind::Socket => FT_SOCK,
        InodeKind::Symlink => FT_SYMLINK,
    }
}

fn kind_from_mode(mode: u16) -> Result<InodeKind, VfsError> {
    Ok(match mode & S_IFMT {
        S_IFREG => InodeKind::Regular,
        S_IFDIR => InodeKind::Directory,
        S_IFCHR => InodeKind::CharDevice,
        S_IFBLK => InodeKind::BlockDevice,
        S_IFIFO => InodeKind::Fifo,
        S_IFSOCK => InodeKind::Socket,
        S_IFLNK => InodeKind::Symlink,
        _ => return Err(VfsError::IoError),
    })
}

fn mode_type(kind: InodeKind) -> u16 {
    match kind {
        InodeKind::Regular => S_IFREG,
        InodeKind::Directory => S_IFDIR,
        InodeKind::CharDevice => S_IFCHR,
        InodeKind::BlockDevice => S_IFBLK,
        InodeKind::Fifo => S_IFIFO,
        InodeKind::Socket => S_IFSOCK,
        InodeKind::Symlink => S_IFLNK,
    }
}

impl BlockCache {
    fn touch(&mut self, block: u64) {
        if let Some(entry) = self.entries.get_mut(&block) {
            self.lru.remove(&entry.tick);
            self.tick += 1;
            entry.tick = self.tick;
            self.lru.insert(self.tick, block);
        }
    }

    fn insert_clean(&mut self, block: u64, data: alloc::boxed::Box<[u8]>) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(old) = self.entries.insert(
            block,
            CacheEntry {
                data,
                tick,
                running: false,
                checkpoint: false,
                data_dirty: false,
            },
        ) {
            self.lru.remove(&old.tick);
        }
        self.lru.insert(tick, block);
    }

    fn remove(&mut self, block: u64) {
        if let Some(entry) = self.entries.remove(&block) {
            self.lru.remove(&entry.tick);
        }
    }
}

impl State {
    fn sectors_per_block(&self) -> usize {
        self.geometry.block_size / SECTOR_SIZE
    }

    fn device_read(&self, block: u64, count: usize, buffer: &mut [u8]) -> Result<(), VfsError> {
        let per = self.sectors_per_block();
        self.device
            .read_blocks(block * per as u64, count * per, buffer)
            .map_err(|_| VfsError::IoError)
    }

    fn device_write(&self, block: u64, count: usize, buffer: &[u8]) -> Result<(), VfsError> {
        let per = self.sectors_per_block();
        self.device
            .write_blocks(block * per as u64, count * per, buffer)
            .map_err(|_| VfsError::IoError)
    }

    fn check_block(&self, block: u64) -> Result<(), VfsError> {
        if block >= self.geometry.blocks_count {
            kwarn!("ext4: block {} beyond device", block);
            return Err(VfsError::IoError);
        }
        Ok(())
    }

    /// Drop clean blocks until the cache is back under its limit, writing dirty
    /// file data out where necessary. Metadata pinned by the journal stays.
    fn evict(&mut self) -> Result<(), VfsError> {
        while self.cache.entries.len() > self.cache.limit {
            let mut victim = None;
            let mut scanned = 0;
            for (&tick, &block) in self.cache.lru.iter() {
                scanned += 1;
                let entry = &self.cache.entries[&block];
                if !entry.running && !entry.checkpoint {
                    victim = Some((tick, block, entry.data_dirty));
                    break;
                }
                if scanned > 4096 {
                    break;
                }
            }
            let Some((_, block, dirty)) = victim else {
                return Ok(());
            };
            if dirty {
                let data = self.cache.entries[&block].data.clone();
                self.device_write(block, 1, &data)?;
                self.ordered.remove(&block);
            }
            self.cache.remove(block);
        }
        Ok(())
    }

    fn load(&mut self, block: u64) -> Result<(), VfsError> {
        self.check_block(block)?;
        if self.cache.entries.contains_key(&block) {
            self.cache.hits += 1;
            self.cache.touch(block);
            return Ok(());
        }
        self.cache.misses += 1;
        self.evict()?;
        let mut data = vec![0u8; self.geometry.block_size].into_boxed_slice();
        self.device_read(block, 1, &mut data)?;
        self.cache.insert_clean(block, data);
        Ok(())
    }

    /// Load a run of file blocks in one device request where possible.
    fn load_run(&mut self, block: u64, count: u64) -> Result<(), VfsError> {
        self.check_block(block)?;
        if self.cache.entries.contains_key(&block) {
            self.cache.hits += 1;
            self.cache.touch(block);
            return Ok(());
        }
        self.cache.misses += 1;
        self.evict()?;
        let mut run = 1;
        while run < count
            && block + run < self.geometry.blocks_count
            && !self.cache.entries.contains_key(&(block + run))
        {
            run += 1;
        }
        let size = self.geometry.block_size;
        let mut buffer = vec![0u8; size * run as usize];
        self.device_read(block, run as usize, &mut buffer)?;
        for index in 0..run as usize {
            let slice = &buffer[index * size..(index + 1) * size];
            self.cache
                .insert_clean(block + index as u64, slice.to_vec().into_boxed_slice());
        }
        Ok(())
    }

    fn read_block(&mut self, block: u64) -> Result<&[u8], VfsError> {
        self.load(block)?;
        Ok(&self.cache.entries[&block].data)
    }

    /// Writable view of a metadata block that joins the running transaction.
    fn modify(&mut self, block: u64) -> Result<&mut [u8], VfsError> {
        if self.read_only || self.aborted {
            return Err(VfsError::ReadOnlyFs);
        }
        self.load(block)?;
        self.pin(block);
        Ok(&mut self.cache.entries.get_mut(&block).unwrap().data)
    }

    /// Like `modify` for a newly allocated block whose old contents are garbage.
    fn modify_fresh(&mut self, block: u64) -> Result<&mut [u8], VfsError> {
        if self.read_only || self.aborted {
            return Err(VfsError::ReadOnlyFs);
        }
        self.check_block(block)?;
        self.cache.remove(block);
        let data = vec![0u8; self.geometry.block_size].into_boxed_slice();
        self.cache.insert_clean(block, data);
        self.pin(block);
        Ok(&mut self.cache.entries.get_mut(&block).unwrap().data)
    }

    fn pin(&mut self, block: u64) {
        let entry = self.cache.entries.get_mut(&block).unwrap();
        entry.running = true;
        // Journaling a block supersedes any previous revocation of it.
        self.revokes.remove(&block);
        if self.tx_blocks.is_empty() {
            self.tx_started_micros = crate::time::uptime_micros();
        }
        self.tx_blocks.insert(block);
    }

    // ------------------------------------------------------------------ groups

    fn desc_location(&self, group: u32) -> (u64, usize) {
        let geometry = &self.geometry;
        let block = geometry.first_data_block + 1 + u64::from(group / geometry.desc_per_block);
        let offset = (group % geometry.desc_per_block) as usize * geometry.desc_size;
        (block, offset)
    }

    fn desc(&mut self, group: u32) -> Result<Vec<u8>, VfsError> {
        let (block, offset) = self.desc_location(group);
        let size = self.geometry.desc_size;
        let raw = self.read_block(block)?;
        Ok(raw[offset..offset + size].to_vec())
    }

    fn desc_store(&mut self, group: u32, record: &[u8]) -> Result<(), VfsError> {
        let (block, offset) = self.desc_location(group);
        let raw = self.modify(block)?;
        raw[offset..offset + record.len()].copy_from_slice(record);
        self.dirty_groups.insert(group);
        Ok(())
    }

    fn desc_hi(&self) -> bool {
        self.geometry.desc_size >= 64
    }

    fn block_bitmap_of(&self, record: &[u8]) -> u64 {
        let high = if self.desc_hi() {
            le32(record, 0x20)
        } else {
            0
        };
        (u64::from(high) << 32) | u64::from(le32(record, 0))
    }

    fn inode_bitmap_of(&self, record: &[u8]) -> u64 {
        let high = if self.desc_hi() {
            le32(record, 0x24)
        } else {
            0
        };
        (u64::from(high) << 32) | u64::from(le32(record, 4))
    }

    fn inode_table_of(&self, record: &[u8]) -> u64 {
        let high = if self.desc_hi() {
            le32(record, 0x28)
        } else {
            0
        };
        (u64::from(high) << 32) | u64::from(le32(record, 8))
    }

    fn free_blocks_of(&self, record: &[u8]) -> u32 {
        let high = if self.desc_hi() {
            le16(record, 0x2C)
        } else {
            0
        };
        (u32::from(high) << 16) | u32::from(le16(record, 0x0C))
    }

    fn set_free_blocks(&self, record: &mut [u8], value: u32) {
        put16(record, 0x0C, value as u16);
        if self.desc_hi() {
            put16(record, 0x2C, (value >> 16) as u16);
        }
    }

    fn free_inodes_of(&self, record: &[u8]) -> u32 {
        let high = if self.desc_hi() {
            le16(record, 0x2E)
        } else {
            0
        };
        (u32::from(high) << 16) | u32::from(le16(record, 0x0E))
    }

    fn set_free_inodes(&self, record: &mut [u8], value: u32) {
        put16(record, 0x0E, value as u16);
        if self.desc_hi() {
            put16(record, 0x2E, (value >> 16) as u16);
        }
    }

    fn used_dirs_of(&self, record: &[u8]) -> u32 {
        let high = if self.desc_hi() {
            le16(record, 0x30)
        } else {
            0
        };
        (u32::from(high) << 16) | u32::from(le16(record, 0x10))
    }

    fn set_used_dirs(&self, record: &mut [u8], value: u32) {
        put16(record, 0x10, value as u16);
        if self.desc_hi() {
            put16(record, 0x30, (value >> 16) as u16);
        }
    }

    fn itable_unused_of(&self, record: &[u8]) -> u32 {
        let high = if self.desc_hi() {
            le16(record, 0x32)
        } else {
            0
        };
        (u32::from(high) << 16) | u32::from(le16(record, 0x1C))
    }

    fn set_itable_unused(&self, record: &mut [u8], value: u32) {
        put16(record, 0x1C, value as u16);
        if self.desc_hi() {
            put16(record, 0x32, (value >> 16) as u16);
        }
    }

    /// Synthesize the bitmap of a `BLOCK_UNINIT` group: only the superblock
    /// backup, group descriptors and this group's own metadata are in use.
    fn init_block_bitmap(&mut self, group: u32, record: &[u8], bitmap: &mut [u8]) {
        bitmap.fill(0);
        let geometry = self.geometry.clone();
        let first = geometry.group_first_block(group);
        let count = geometry.group_block_count(group);
        let set = |block: u64, map: &mut [u8]| {
            if block >= first && block < first + u64::from(count) {
                let bit = (block - first) as usize;
                map[bit / 8] |= 1 << (bit % 8);
            }
        };
        if geometry.group_has_super(group) {
            let mut block = first;
            set(block, bitmap);
            block += 1;
            for _ in 0..geometry.gdt_blocks() + u64::from(geometry.reserved_gdt) {
                set(block, bitmap);
                block += 1;
            }
        }
        set(self.block_bitmap_of(record), bitmap);
        set(self.inode_bitmap_of(record), bitmap);
        let table = self.inode_table_of(record);
        let table_blocks = (u64::from(geometry.inodes_per_group) * geometry.inode_size as u64)
            .div_ceil(geometry.block_size as u64);
        for index in 0..table_blocks {
            set(table + index, bitmap);
        }
        for bit in count as usize..geometry.block_size * 8 {
            bitmap[bit / 8] |= 1 << (bit % 8);
        }
    }

    fn block_bitmap(&mut self, group: u32) -> Result<(u64, Vec<u8>), VfsError> {
        let mut record = self.desc(group)?;
        let location = self.block_bitmap_of(&record);
        if le16(&record, 0x12) & BG_BLOCK_UNINIT != 0 {
            let mut fresh = vec![0u8; self.geometry.block_size];
            self.init_block_bitmap(group, &record, &mut fresh);
            let flags = le16(&record, 0x12) & !BG_BLOCK_UNINIT;
            put16(&mut record, 0x12, flags);
            self.desc_store(group, &record)?;
            self.modify_fresh(location)?.copy_from_slice(&fresh);
            self.dirty_block_bitmaps.insert(group);
            return Ok((location, fresh));
        }
        let data = self.read_block(location)?.to_vec();
        Ok((location, data))
    }

    fn inode_bitmap(&mut self, group: u32) -> Result<(u64, Vec<u8>), VfsError> {
        let mut record = self.desc(group)?;
        let location = self.inode_bitmap_of(&record);
        if le16(&record, 0x12) & BG_INODE_UNINIT != 0 {
            let size = self.geometry.block_size;
            let mut fresh = vec![0u8; size];
            for bit in self.geometry.inodes_per_group as usize..size * 8 {
                fresh[bit / 8] |= 1 << (bit % 8);
            }
            let flags = le16(&record, 0x12) & !BG_INODE_UNINIT;
            put16(&mut record, 0x12, flags);
            self.desc_store(group, &record)?;
            self.modify_fresh(location)?.copy_from_slice(&fresh);
            self.dirty_inode_bitmaps.insert(group);
            return Ok((location, fresh));
        }
        let data = self.read_block(location)?.to_vec();
        Ok((location, data))
    }

    /// Allocate up to `want` contiguous blocks close to `goal`.
    fn alloc_blocks(&mut self, goal: u64, want: u32) -> Result<(u64, u32), VfsError> {
        if self.free_blocks == 0 {
            return Err(VfsError::NoSpace);
        }
        let geometry = self.geometry.clone();
        let goal = goal
            .max(geometry.first_data_block)
            .min(geometry.blocks_count - 1);
        let start_group =
            ((goal - geometry.first_data_block) / u64::from(geometry.blocks_per_group)) as u32;
        for attempt in 0..geometry.group_count {
            let group = (start_group + attempt) % geometry.group_count;
            let record = self.desc(group)?;
            if self.free_blocks_of(&record) == 0 {
                continue;
            }
            let (location, bitmap) = self.block_bitmap(group)?;
            let first = geometry.group_first_block(group);
            let count = geometry.group_block_count(group) as usize;
            let begin = if attempt == 0 {
                (goal - first) as usize
            } else {
                0
            };
            for pass in 0..2 {
                let (low, high) = if pass == 0 {
                    (begin, count)
                } else {
                    (0, begin)
                };
                let mut bit = low;
                while bit < high {
                    if bitmap[bit / 8] == 0xFF && bit % 8 == 0 {
                        bit += 8;
                        continue;
                    }
                    let block = first + bit as u64;
                    if bitmap[bit / 8] & (1 << (bit % 8)) != 0 || self.pending_free.contains(&block)
                    {
                        bit += 1;
                        continue;
                    }
                    let mut run = 1u32;
                    while run < want
                        && bit + (run as usize) < count
                        && bitmap[(bit + run as usize) / 8] & (1 << ((bit + run as usize) % 8)) == 0
                        && !self.pending_free.contains(&(block + u64::from(run)))
                    {
                        run += 1;
                    }
                    let map = self.modify(location)?;
                    for index in bit..bit + run as usize {
                        map[index / 8] |= 1 << (index % 8);
                    }
                    self.dirty_block_bitmaps.insert(group);
                    let mut record = self.desc(group)?;
                    let free = self.free_blocks_of(&record) - run;
                    self.set_free_blocks(&mut record, free);
                    self.desc_store(group, &record)?;
                    self.free_blocks -= u64::from(run);
                    self.super_dirty = true;
                    for index in 0..u64::from(run) {
                        self.cache.remove(block + index);
                        self.ordered.remove(&(block + index));
                    }
                    return Ok((block, run));
                }
            }
        }
        Err(VfsError::NoSpace)
    }

    /// Return blocks to the allocator inside the running transaction.
    ///
    /// `metadata` blocks may have journaled copies, so a revoke record prevents
    /// replay from clobbering their next use.
    fn free_blocks_range(
        &mut self,
        start: u64,
        count: u32,
        metadata: bool,
    ) -> Result<(), VfsError> {
        let geometry = self.geometry.clone();
        let mut remaining = u64::from(count);
        let mut block = start;
        while remaining > 0 {
            if block < geometry.first_data_block || block >= geometry.blocks_count {
                return Err(VfsError::IoError);
            }
            let group =
                ((block - geometry.first_data_block) / u64::from(geometry.blocks_per_group)) as u32;
            let first = geometry.group_first_block(group);
            let bit = (block - first) as usize;
            let here = (u64::from(geometry.group_block_count(group)) - bit as u64).min(remaining);
            let (location, _) = self.block_bitmap(group)?;
            let map = self.modify(location)?;
            let mut freed = 0u32;
            for index in bit..bit + here as usize {
                if map[index / 8] & (1 << (index % 8)) != 0 {
                    map[index / 8] &= !(1 << (index % 8));
                    freed += 1;
                }
            }
            self.dirty_block_bitmaps.insert(group);
            let mut record = self.desc(group)?;
            let free = self.free_blocks_of(&record) + freed;
            self.set_free_blocks(&mut record, free);
            self.desc_store(group, &record)?;
            self.free_blocks += u64::from(freed);
            self.super_dirty = true;
            for index in 0..here {
                let target = block + index;
                self.pending_free.insert(target);
                self.ordered.remove(&target);
                self.tx_blocks.remove(&target);
                self.checkpoint_blocks.remove(&target);
                self.cache.remove(target);
                if metadata {
                    self.revokes.insert(target);
                }
            }
            block += here;
            remaining -= here;
        }
        Ok(())
    }

    fn alloc_inode(&mut self, parent: u32, directory: bool) -> Result<u32, VfsError> {
        if self.free_inodes == 0 {
            return Err(VfsError::NoSpace);
        }
        let geometry = self.geometry.clone();
        let start = (parent.max(1) - 1) / geometry.inodes_per_group;
        for attempt in 0..geometry.group_count {
            let group = (start + attempt) % geometry.group_count;
            let record = self.desc(group)?;
            if self.free_inodes_of(&record) == 0 {
                continue;
            }
            let (location, bitmap) = self.inode_bitmap(group)?;
            let reserved = if group == 0 {
                // First non-reserved inode is s_first_ino, always within group 0.
                self.first_ino()
            } else {
                0
            };
            for bit in reserved as usize..geometry.inodes_per_group as usize {
                if bitmap[bit / 8] & (1 << (bit % 8)) != 0 {
                    continue;
                }
                self.modify(location)?[bit / 8] |= 1 << (bit % 8);
                self.dirty_inode_bitmaps.insert(group);
                let mut record = self.desc(group)?;
                let free = self.free_inodes_of(&record) - 1;
                self.set_free_inodes(&mut record, free);
                if directory {
                    let used = self.used_dirs_of(&record) + 1;
                    self.set_used_dirs(&mut record, used);
                }
                // Inodes past "unused" are assumed uninitialized by fsck.
                let high_water = geometry.inodes_per_group - self.itable_unused_of(&record);
                if bit as u32 >= high_water {
                    let unused = geometry.inodes_per_group - bit as u32 - 1;
                    self.set_itable_unused(&mut record, unused);
                }
                self.desc_store(group, &record)?;
                self.free_inodes -= 1;
                self.super_dirty = true;
                return Ok(group * geometry.inodes_per_group + bit as u32 + 1);
            }
        }
        Err(VfsError::NoSpace)
    }

    fn first_ino(&mut self) -> u32 {
        let (block, offset) = self.super_location();
        match self.read_block(block) {
            Ok(raw) => le32(raw, offset + 0x54).max(11) - 1,
            Err(_) => 10,
        }
    }

    fn free_inode_bit(&mut self, ino: u32, directory: bool) -> Result<(), VfsError> {
        let geometry = self.geometry.clone();
        let group = (ino - 1) / geometry.inodes_per_group;
        let bit = ((ino - 1) % geometry.inodes_per_group) as usize;
        let (location, _) = self.inode_bitmap(group)?;
        let map = self.modify(location)?;
        if map[bit / 8] & (1 << (bit % 8)) == 0 {
            return Err(VfsError::IoError);
        }
        map[bit / 8] &= !(1 << (bit % 8));
        self.dirty_inode_bitmaps.insert(group);
        let mut record = self.desc(group)?;
        let free = self.free_inodes_of(&record) + 1;
        self.set_free_inodes(&mut record, free);
        if directory {
            let used = self.used_dirs_of(&record).saturating_sub(1);
            self.set_used_dirs(&mut record, used);
        }
        self.desc_store(group, &record)?;
        self.free_inodes += 1;
        self.super_dirty = true;
        Ok(())
    }

    fn super_location(&self) -> (u64, usize) {
        let size = self.geometry.block_size as u64;
        (SUPER_OFFSET / size, (SUPER_OFFSET % size) as usize)
    }

    // ------------------------------------------------------------------ inodes

    fn inode_location(&mut self, ino: u32) -> Result<(u64, usize), VfsError> {
        let geometry = self.geometry.clone();
        if ino == 0 || ino > geometry.inodes_count {
            return Err(VfsError::IoError);
        }
        let group = (ino - 1) / geometry.inodes_per_group;
        let index = ((ino - 1) % geometry.inodes_per_group) as usize;
        let record = self.desc(group)?;
        let table = self.inode_table_of(&record);
        let byte = index * geometry.inode_size;
        Ok((
            table + (byte / geometry.block_size) as u64,
            byte % geometry.block_size,
        ))
    }

    fn inode_mut(&mut self, ino: u32) -> Result<&mut MemInode, VfsError> {
        if !self.inodes.contains_key(&ino) {
            let (block, offset) = self.inode_location(ino)?;
            let size = self.geometry.inode_size;
            let raw = self.read_block(block)?[offset..offset + size].to_vec();
            self.inodes.insert(
                ino,
                MemInode {
                    raw,
                    extents: None,
                    tree_blocks: Vec::new(),
                },
            );
        }
        Ok(self.inodes.get_mut(&ino).unwrap())
    }

    fn inode_raw(&mut self, ino: u32) -> Result<&[u8], VfsError> {
        Ok(&self.inode_mut(ino)?.raw)
    }

    /// Persist an inode's raw record into the running transaction.
    fn inode_store(&mut self, ino: u32) -> Result<(), VfsError> {
        let geometry = self.geometry.clone();
        let (block, offset) = self.inode_location(ino)?;
        let memory = self.inodes.get_mut(&ino).ok_or(VfsError::IoError)?;
        inode_checksum_set(&geometry, ino, &mut memory.raw);
        let record = memory.raw.clone();
        let table = self.modify(block)?;
        table[offset..offset + record.len()].copy_from_slice(&record);
        Ok(())
    }

    fn inode_size(raw: &[u8]) -> u64 {
        (u64::from(le32(raw, 0x6C)) << 32) | u64::from(le32(raw, 4))
    }

    fn set_inode_size(raw: &mut [u8], size: u64) {
        put32(raw, 4, size as u32);
        put32(raw, 0x6C, (size >> 32) as u32);
    }

    /// `i_blocks` in filesystem-independent 512-byte units.
    fn inode_sectors(&self, raw: &[u8]) -> u64 {
        let value = (u64::from(le16(raw, 0x74)) << 32) | u64::from(le32(raw, 0x1C));
        if le32(raw, 0x20) & FL_HUGE_FILE != 0 {
            value * (self.geometry.block_size as u64 / 512)
        } else {
            value
        }
    }

    fn adjust_blocks(&mut self, ino: u32, delta_blocks: i64) -> Result<(), VfsError> {
        let per = (self.geometry.block_size / 512) as i64;
        let raw_view = self.inode_raw(ino)?.to_vec();
        let current = self.inode_sectors(&raw_view) as i64;
        let next = (current + delta_blocks * per).max(0) as u64;
        let raw = &mut self.inode_mut(ino)?.raw;
        let flags = le32(raw, 0x20) & !FL_HUGE_FILE;
        put32(raw, 0x20, flags);
        put32(raw, 0x1C, next as u32);
        put16(raw, 0x74, (next >> 32) as u16);
        Ok(())
    }

    fn set_times(raw: &mut [u8], atime: bool, mtime: bool, ctime: bool, now: u64) {
        let extra = (now >> 32) as u32 & 3;
        let has_extra = raw.len() > 128 && usize::from(le16(raw, 0x80)) + 128 >= 0x90;
        if atime {
            put32(raw, 0x08, now as u32);
            if has_extra {
                put32(raw, 0x8C, extra);
            }
        }
        if ctime {
            put32(raw, 0x0C, now as u32);
            if has_extra {
                put32(raw, 0x84, extra);
            }
        }
        if mtime {
            put32(raw, 0x10, now as u32);
            if has_extra {
                put32(raw, 0x88, extra);
            }
        }
    }

    fn decode_time(raw: &[u8], base: usize, extra: usize) -> (i64, i64) {
        let seconds = i64::from(le32(raw, base) as i32);
        if raw.len() > 128 && usize::from(le16(raw, 0x80)) + 128 >= extra + 4 {
            let bits = le32(raw, extra);
            (seconds + (i64::from(bits & 3) << 32), i64::from(bits >> 2))
        } else {
            (seconds, 0)
        }
    }

    fn stat(&mut self, ino: u32, dev: u64) -> Result<Stat, VfsError> {
        let raw = self.inode_raw(ino)?.to_vec();
        let mode = le16(&raw, 0);
        let kind = kind_from_mode(mode)?;
        let mut rdev = 0;
        if matches!(kind, InodeKind::CharDevice | InodeKind::BlockDevice) {
            let old = le32(&raw, INODE_BLOCK_OFFSET);
            let new = le32(&raw, INODE_BLOCK_OFFSET + 4);
            let encoded = if old != 0 { old } else { new };
            let major = u64::from((encoded >> 8) & 0xFFF);
            let minor = u64::from((encoded & 0xFF) | ((encoded >> 12) & 0xFFF00));
            rdev = crate::vfs::abi::makedev(major, minor);
        }
        let uid = u32::from(le16(&raw, 0x02)) | (u32::from(le16(&raw, 0x78)) << 16);
        let gid = u32::from(le16(&raw, 0x18)) | (u32::from(le16(&raw, 0x7A)) << 16);
        let (mtime, mtime_nsec) = Self::decode_time(&raw, 0x10, 0x88);
        let (atime, atime_nsec) = Self::decode_time(&raw, 0x08, 0x8C);
        let (ctime, ctime_nsec) = Self::decode_time(&raw, 0x0C, 0x84);
        Ok(Stat {
            dev,
            ino: u64::from(ino),
            kind,
            mode: u32::from(mode & 0o7777),
            nlink: u32::from(le16(&raw, 0x1A)),
            uid,
            gid,
            rdev,
            size: Self::inode_size(&raw),
            blksize: self.geometry.block_size as u64,
            blocks: self.inode_sectors(&raw),
            mtime,
            mtime_nsec,
            atime,
            atime_nsec,
            ctime,
            ctime_nsec,
        })
    }

    // ----------------------------------------------------------------- extents

    /// Parse the extent tree of `ino` into a sorted in-memory list.
    fn load_extents(&mut self, ino: u32) -> Result<(), VfsError> {
        let memory = self.inode_mut(ino)?;
        if memory.extents.is_some() {
            return Ok(());
        }
        let raw = memory.raw.clone();
        let flags = le32(&raw, 0x20);
        if flags & FL_INLINE_DATA != 0 {
            return Err(VfsError::NotSupported);
        }
        let mut extents = Vec::new();
        let mut tree_blocks = Vec::new();
        if flags & FL_EXTENTS != 0 {
            let root = raw[INODE_BLOCK_OFFSET..INODE_BLOCK_OFFSET + INODE_BLOCK_LEN].to_vec();
            self.walk_extents(&root, 0, &mut extents, &mut tree_blocks)?;
        } else {
            // Legacy block map from ext2/ext3 images: read-only access.
            self.load_block_map(&raw, &mut extents)?;
        }
        let memory = self.inode_mut(ino)?;
        memory.extents = Some(extents);
        memory.tree_blocks = tree_blocks;
        Ok(())
    }

    fn walk_extents(
        &mut self,
        node: &[u8],
        depth_guard: u32,
        extents: &mut Vec<Extent>,
        tree_blocks: &mut Vec<u64>,
    ) -> Result<(), VfsError> {
        if depth_guard > 5 || le16(node, 0) != EXTENT_MAGIC {
            kwarn!("ext4: corrupt extent node");
            return Err(VfsError::IoError);
        }
        let entries = usize::from(le16(node, 2));
        let depth = le16(node, 6);
        if 12 + entries * 12 > node.len() {
            return Err(VfsError::IoError);
        }
        for index in 0..entries {
            let offset = 12 + index * 12;
            if depth == 0 {
                let logical = le32(node, offset);
                let raw_length = u32::from(le16(node, offset + 4));
                let high = u64::from(le16(node, offset + 6));
                let low = u64::from(le32(node, offset + 8));
                let unwritten = raw_length > 32768;
                extents.push(Extent {
                    logical,
                    physical: (high << 32) | low,
                    length: if unwritten {
                        raw_length - 32768
                    } else {
                        raw_length
                    },
                    unwritten,
                });
            } else {
                let low = u64::from(le32(node, offset + 4));
                let high = u64::from(le16(node, offset + 8));
                let child = (high << 32) | low;
                tree_blocks.push(child);
                let block = self.read_block(child)?.to_vec();
                self.walk_extents(&block, depth_guard + 1, extents, tree_blocks)?;
            }
        }
        Ok(())
    }

    fn load_block_map(&mut self, raw: &[u8], extents: &mut Vec<Extent>) -> Result<(), VfsError> {
        let size = self.geometry.block_size;
        let per = size / 4;
        let total = Self::inode_size(raw).div_ceil(size as u64);
        let mut logical = 0u64;
        let push = |extents: &mut Vec<Extent>, logical: u64, physical: u64| {
            if physical == 0 {
                return;
            }
            if let Some(last) = extents.last_mut()
                && u64::from(last.logical) + u64::from(last.length) == logical
                && last.physical + u64::from(last.length) == physical
                && last.length < 32768
            {
                last.length += 1;
                return;
            }
            extents.push(Extent {
                logical: logical as u32,
                physical,
                length: 1,
                unwritten: false,
            });
        };
        for index in 0..12 {
            if logical >= total {
                return Ok(());
            }
            push(
                extents,
                logical,
                u64::from(le32(raw, INODE_BLOCK_OFFSET + index * 4)),
            );
            logical += 1;
        }
        fn descend(
            state: &mut State,
            block: u64,
            level: u32,
            logical: &mut u64,
            total: u64,
            per: usize,
            out: &mut dyn FnMut(u64, u64),
        ) -> Result<(), VfsError> {
            let span = (per as u64).pow(level);
            if block == 0 {
                *logical += span * per as u64;
                return Ok(());
            }
            let table = state.read_block(block)?.to_vec();
            for index in 0..per {
                if *logical >= total {
                    return Ok(());
                }
                let child = u64::from(le32(&table, index * 4));
                if level == 0 {
                    out(*logical, child);
                    *logical += 1;
                } else {
                    descend(state, child, level - 1, logical, total, per, out)?;
                }
            }
            Ok(())
        }
        for (slot, level) in [(12, 0u32), (13, 1), (14, 2)] {
            if logical >= total {
                break;
            }
            let root = u64::from(le32(raw, INODE_BLOCK_OFFSET + slot * 4));
            let mut sink = |at: u64, physical: u64| push(extents, at, physical);
            descend(self, root, level, &mut logical, total, per, &mut sink)?;
        }
        Ok(())
    }

    /// Map a logical block: `(physical start, blocks left in the extent)`.
    /// Holes and unwritten extents return `None` with the hole's known length.
    fn map_block(&mut self, ino: u32, logical: u32) -> Result<(Option<u64>, u32), VfsError> {
        self.load_extents(ino)?;
        let extents = self.inodes[&ino].extents.as_ref().unwrap();
        let index = extents.partition_point(|extent| extent.logical <= logical);
        if index > 0 {
            let extent = &extents[index - 1];
            let delta = logical - extent.logical;
            if delta < extent.length {
                let left = extent.length - delta;
                return Ok(if extent.unwritten {
                    (None, left)
                } else {
                    (Some(extent.physical + u64::from(delta)), left)
                });
            }
        }
        let gap = extents
            .get(index)
            .map(|extent| extent.logical - logical)
            .unwrap_or(u32::MAX);
        Ok((None, gap))
    }

    /// Serialize the in-memory extent list back into the inode and tree blocks.
    ///
    /// The tree is rebuilt whole and reuses its existing blocks, which keeps
    /// the code path for splits, merges and collapses single. Contiguous
    /// allocation keeps the extent count, and so the cost, small in practice.
    fn store_extents(&mut self, ino: u32) -> Result<(), VfsError> {
        let geometry = self.geometry.clone();
        let size = geometry.block_size;
        let tail = if geometry.metadata_csum() { 4 } else { 0 };
        let leaf_capacity = (size - 12 - tail) / 12;
        let index_capacity = leaf_capacity;
        let memory = self.inodes.get(&ino).ok_or(VfsError::IoError)?;
        let extents = memory.extents.clone().ok_or(VfsError::IoError)?;
        let generation = le32(&memory.raw, 0x64);
        let mut old_blocks = memory.tree_blocks.clone();
        let seed = inode_seed(&geometry, ino, generation);

        // Levels from the leaves upward: each entry is (first logical, block).
        let mut new_blocks: Vec<u64> = Vec::new();
        let mut root = vec![0u8; INODE_BLOCK_LEN];

        let write_header = |node: &mut [u8], entries: usize, max: usize, depth: u16| {
            put16(node, 0, EXTENT_MAGIC);
            put16(node, 2, entries as u16);
            put16(node, 4, max as u16);
            put16(node, 6, depth);
            put32(node, 8, 0);
        };
        let put_extent = |node: &mut [u8], index: usize, extent: &Extent| {
            let offset = 12 + index * 12;
            put32(node, offset, extent.logical);
            let length = if extent.unwritten {
                extent.length + 32768
            } else {
                extent.length
            };
            put16(node, offset + 4, length as u16);
            put16(node, offset + 6, (extent.physical >> 32) as u16);
            put32(node, offset + 8, extent.physical as u32);
        };

        if extents.len() <= 4 {
            write_header(&mut root, extents.len(), 4, 0);
            for (index, extent) in extents.iter().enumerate() {
                put_extent(&mut root, index, extent);
            }
        } else {
            let mut level: Vec<(u32, u64)> = Vec::new();
            for chunk in extents.chunks(leaf_capacity) {
                let block = self.tree_block(ino, &mut old_blocks, &mut new_blocks)?;
                let mut node = vec![0u8; size];
                write_header(&mut node, chunk.len(), leaf_capacity, 0);
                for (index, extent) in chunk.iter().enumerate() {
                    put_extent(&mut node, index, extent);
                }
                extent_checksum_set(&geometry, seed, &mut node);
                self.modify_fresh(block)?.copy_from_slice(&node);
                level.push((chunk[0].logical, block));
            }
            let mut depth = 1u16;
            while level.len() > 4 {
                let mut upper = Vec::new();
                for chunk in level.chunks(index_capacity) {
                    let block = self.tree_block(ino, &mut old_blocks, &mut new_blocks)?;
                    let mut node = vec![0u8; size];
                    write_header(&mut node, chunk.len(), index_capacity, depth);
                    for (index, &(logical, child)) in chunk.iter().enumerate() {
                        let offset = 12 + index * 12;
                        put32(&mut node, offset, logical);
                        put32(&mut node, offset + 4, child as u32);
                        put16(&mut node, offset + 8, (child >> 32) as u16);
                    }
                    extent_checksum_set(&geometry, seed, &mut node);
                    self.modify_fresh(block)?.copy_from_slice(&node);
                    upper.push((chunk[0].0, block));
                }
                level = upper;
                depth += 1;
            }
            write_header(&mut root, level.len(), 4, depth);
            for (index, &(logical, child)) in level.iter().enumerate() {
                let offset = 12 + index * 12;
                put32(&mut root, offset, logical);
                put32(&mut root, offset + 4, child as u32);
                put16(&mut root, offset + 8, (child >> 32) as u16);
            }
        }
        for block in old_blocks {
            self.free_blocks_range(block, 1, true)?;
            self.adjust_blocks(ino, -1)?;
        }
        let memory = self.inodes.get_mut(&ino).ok_or(VfsError::IoError)?;
        memory.raw[INODE_BLOCK_OFFSET..INODE_BLOCK_OFFSET + INODE_BLOCK_LEN].copy_from_slice(&root);
        memory.tree_blocks = new_blocks;
        Ok(())
    }

    fn tree_block(
        &mut self,
        ino: u32,
        old: &mut Vec<u64>,
        new: &mut Vec<u64>,
    ) -> Result<u64, VfsError> {
        let block = if let Some(block) = old.pop() {
            block
        } else {
            let goal = self.inode_goal(ino);
            let (block, _) = self.alloc_blocks(goal, 1)?;
            self.adjust_blocks(ino, 1)?;
            block
        };
        new.push(block);
        Ok(block)
    }

    fn inode_goal(&self, ino: u32) -> u64 {
        let geometry = &self.geometry;
        let group = (ino.max(1) - 1) / geometry.inodes_per_group;
        geometry.group_first_block(group)
    }

    /// Insert a mapping for a hole, merging with its neighbours where possible.
    fn extent_insert(&mut self, ino: u32, extent: Extent) -> Result<(), VfsError> {
        let memory = self.inodes.get_mut(&ino).ok_or(VfsError::IoError)?;
        let list = memory.extents.as_mut().ok_or(VfsError::IoError)?;
        let index = list.partition_point(|other| other.logical < extent.logical);
        list.insert(index, extent);
        let mut position = index.saturating_sub(1);
        while position + 1 < list.len() {
            let next = list[position + 1];
            let current = list[position];
            let total = current.length + next.length;
            if current.logical + current.length == next.logical
                && current.physical + u64::from(current.length) == next.physical
                && current.unwritten == next.unwritten
                && total <= 32768
            {
                list[position].length = total;
                list.remove(position + 1);
            } else if position >= index {
                break;
            } else {
                position += 1;
            }
        }
        Ok(())
    }

    /// Release every mapped block at or past logical block `from`.
    fn extent_truncate(&mut self, ino: u32, from: u32) -> Result<(), VfsError> {
        self.load_extents(ino)?;
        // Directory data blocks are journaled metadata. Freeing one without a
        // revoke lets a stale directory block be replayed over its next user.
        let metadata = le16(self.inode_raw(ino)?, 0) & S_IFMT == S_IFDIR;
        let memory = self.inodes.get_mut(&ino).ok_or(VfsError::IoError)?;
        let list = memory.extents.as_mut().ok_or(VfsError::IoError)?;
        let mut freed: Vec<(u64, u32)> = Vec::new();
        let mut kept = Vec::with_capacity(list.len());
        for extent in list.iter() {
            if extent.logical >= from {
                freed.push((extent.physical, extent.length));
            } else if extent.logical + extent.length > from {
                let keep = from - extent.logical;
                freed.push((extent.physical + u64::from(keep), extent.length - keep));
                let mut trimmed = *extent;
                trimmed.length = keep;
                kept.push(trimmed);
            } else {
                kept.push(*extent);
            }
        }
        *list = kept;
        let mut total = 0i64;
        for (start, count) in freed {
            self.free_blocks_range(start, count, metadata)?;
            total += i64::from(count);
        }
        self.adjust_blocks(ino, -total)?;
        self.store_extents(ino)
    }

    // ----------------------------------------------------------------- file I/O

    fn read_file(&mut self, ino: u32, offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        let size = Self::inode_size(self.inode_raw(ino)?);
        if offset >= size || buffer.is_empty() {
            return Ok(0);
        }
        let want = (size - offset).min(buffer.len() as u64) as usize;
        let block_size = self.geometry.block_size;
        let mut done = 0;
        while done < want {
            let position = offset + done as u64;
            let logical =
                u32::try_from(position / block_size as u64).map_err(|_| VfsError::IoError)?;
            let within = (position % block_size as u64) as usize;
            let chunk = (block_size - within).min(want - done);
            let (physical, left) = self.map_block(ino, logical)?;
            if let Some(physical) = physical {
                self.load_run(physical, u64::from(left).min(READAHEAD_BLOCKS))?;
                let data = &self.cache.entries[&physical].data;
                buffer[done..done + chunk].copy_from_slice(&data[within..within + chunk]);
            } else {
                buffer[done..done + chunk].fill(0);
            }
            done += chunk;
        }
        Ok(done)
    }

    fn write_file(&mut self, ino: u32, offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        if self.read_only || self.aborted {
            return Err(VfsError::ReadOnlyFs);
        }
        if buffer.is_empty() {
            return Ok(0);
        }
        let block_size = self.geometry.block_size;
        let chunk_limit = MAX_WRITE_CHUNK_BLOCKS as usize * block_size;
        let want = buffer.len().min(chunk_limit);
        let end = offset.checked_add(want as u64).ok_or(VfsError::NoSpace)?;
        let last_logical = (end - 1) / block_size as u64;
        if last_logical >= u64::from(u32::MAX) {
            return Err(VfsError::NoSpace);
        }
        self.load_extents(ino)?;
        let flags = le32(self.inode_raw(ino)?, 0x20);
        if flags & FL_EXTENTS == 0 {
            return Err(VfsError::NotSupported);
        }
        let old_size = Self::inode_size(self.inode_raw(ino)?);

        // Reserve all blocks first so ENOSPC leaves no half-applied write. A
        // short allocation still yields a valid, shorter write.
        let mut logical = (offset / block_size as u64) as u32;
        let mut allocated_any = false;
        let mut allocated_up_to = last_logical as u32 + 1;
        while u64::from(logical) <= last_logical {
            let (physical, left) = self.map_block(ino, logical)?;
            let remaining = (last_logical - u64::from(logical) + 1) as u32;
            if physical.is_some() {
                logical = logical.saturating_add(left.min(remaining).max(1));
                continue;
            }
            if self.extent_at(ino, logical).is_some() {
                self.split_unwritten(ino, logical)?;
                continue;
            }
            let want_run = left.min(remaining).min(32768).max(1);
            let goal = self.write_goal(ino, logical);
            let (block, got) = match self.alloc_blocks(goal, want_run) {
                Ok(value) => value,
                Err(VfsError::NoSpace) if u64::from(logical) * block_size as u64 > offset => {
                    allocated_up_to = logical;
                    break;
                }
                Err(error) => return Err(error),
            };
            self.extent_insert(
                ino,
                Extent {
                    logical,
                    physical: block,
                    length: got,
                    unwritten: false,
                },
            )?;
            self.adjust_blocks(ino, i64::from(got))?;
            for index in 0..u64::from(got) {
                // A partial first or last write must not expose old disk data,
                // even if this block is evicted before the copy below.
                let fresh = vec![0u8; block_size].into_boxed_slice();
                self.evict()?;
                self.cache.insert_clean(block + index, fresh);
                self.cache
                    .entries
                    .get_mut(&(block + index))
                    .unwrap()
                    .data_dirty = true;
                self.ordered.insert(block + index);
            }
            allocated_any = true;
            logical += got;
        }
        if allocated_any {
            self.store_extents(ino)?;
        }

        let writable_end = (u64::from(allocated_up_to) * block_size as u64).min(end);
        let total = (writable_end - offset) as usize;
        let mut done = 0;
        while done < total {
            let position = offset + done as u64;
            let logical = (position / block_size as u64) as u32;
            let within = (position % block_size as u64) as usize;
            let chunk = (block_size - within).min(total - done);
            let (physical, _) = self.map_block(ino, logical)?;
            let physical = physical.ok_or(VfsError::IoError)?;
            if chunk == block_size && !self.cache.entries.contains_key(&physical) {
                self.evict()?;
                self.cache
                    .insert_clean(physical, vec![0u8; block_size].into_boxed_slice());
            } else {
                self.load(physical)?;
            }
            self.cache.touch(physical);
            let entry = self.cache.entries.get_mut(&physical).unwrap();
            entry.data[within..within + chunk].copy_from_slice(&buffer[done..done + chunk]);
            entry.data_dirty = true;
            self.ordered.insert(physical);
            done += chunk;
        }
        let new_size = old_size.max(offset + done as u64);
        let now = now_seconds();
        let memory = self.inodes.get_mut(&ino).ok_or(VfsError::IoError)?;
        Self::set_inode_size(&mut memory.raw, new_size);
        Self::set_times(&mut memory.raw, false, true, true, now);
        self.inode_store(ino)?;
        Ok(done)
    }

    fn extent_at(&self, ino: u32, logical: u32) -> Option<Extent> {
        let extents = self.inodes.get(&ino)?.extents.as_ref()?;
        let index = extents.partition_point(|extent| extent.logical <= logical);
        (index > 0)
            .then(|| extents[index - 1])
            .filter(|extent| logical - extent.logical < extent.length)
    }

    /// Convert one block of a preallocated extent to written, zeroing it.
    fn split_unwritten(&mut self, ino: u32, logical: u32) -> Result<(), VfsError> {
        let extent = self.extent_at(ino, logical).ok_or(VfsError::IoError)?;
        let delta = logical - extent.logical;
        let list = self
            .inodes
            .get_mut(&ino)
            .ok_or(VfsError::IoError)?
            .extents
            .as_mut()
            .ok_or(VfsError::IoError)?;
        let index = list
            .iter()
            .position(|e| *e == extent)
            .ok_or(VfsError::IoError)?;
        list.remove(index);
        let mut insert_at = index;
        if delta > 0 {
            list.insert(
                insert_at,
                Extent {
                    length: delta,
                    ..extent
                },
            );
            insert_at += 1;
        }
        let physical = extent.physical + u64::from(delta);
        list.insert(
            insert_at,
            Extent {
                logical,
                physical,
                length: 1,
                unwritten: false,
            },
        );
        insert_at += 1;
        if delta + 1 < extent.length {
            list.insert(
                insert_at,
                Extent {
                    logical: logical + 1,
                    physical: physical + 1,
                    length: extent.length - delta - 1,
                    unwritten: true,
                },
            );
        }
        let size = self.geometry.block_size;
        self.evict()?;
        self.cache
            .insert_clean(physical, vec![0u8; size].into_boxed_slice());
        self.store_extents(ino)
    }

    fn write_goal(&self, ino: u32, logical: u32) -> u64 {
        if let Some(memory) = self.inodes.get(&ino)
            && let Some(extents) = memory.extents.as_ref()
        {
            let index = extents.partition_point(|extent| extent.logical < logical);
            if index > 0 {
                let previous = &extents[index - 1];
                return previous.physical + u64::from(previous.length);
            }
        }
        self.inode_goal(ino)
    }

    fn truncate_file(&mut self, ino: u32, size: u64) -> Result<(), VfsError> {
        if self.read_only || self.aborted {
            return Err(VfsError::ReadOnlyFs);
        }
        let block_size = self.geometry.block_size as u64;
        let old = Self::inode_size(self.inode_raw(ino)?);
        let flags = le32(self.inode_raw(ino)?, 0x20);
        if size < old {
            if flags & FL_EXTENTS == 0 {
                return Err(VfsError::NotSupported);
            }
            let within = (size % block_size) as usize;
            if within != 0 {
                let (physical, _) = self.map_block(ino, (size / block_size) as u32)?;
                if let Some(physical) = physical {
                    self.load(physical)?;
                    let entry = self.cache.entries.get_mut(&physical).unwrap();
                    entry.data[within..].fill(0);
                    entry.data_dirty = true;
                    self.ordered.insert(physical);
                }
            }
            self.extent_truncate(ino, size.div_ceil(block_size) as u32)?;
        }
        let now = now_seconds();
        let memory = self.inode_mut(ino)?;
        Self::set_inode_size(&mut memory.raw, size);
        Self::set_times(&mut memory.raw, false, true, true, now);
        self.inode_store(ino)
    }

    // -------------------------------------------------------------- directories

    /// Calls `visit(block index, physical, offset, inode, rec_len, name_len, ftype, name)`.
    fn dir_scan(
        &mut self,
        ino: u32,
        mut visit: impl FnMut(u32, u64, usize, u32, usize, u8, &[u8]) -> bool,
    ) -> Result<(), VfsError> {
        let block_size = self.geometry.block_size as u64;
        let size = Self::inode_size(self.inode_raw(ino)?);
        for index in 0..size / block_size {
            let (physical, _) = self.map_block(ino, index as u32)?;
            let Some(physical) = physical else {
                continue;
            };
            let raw = self.read_block(physical)?;
            let mut offset = 0usize;
            while offset + 8 <= raw.len() {
                let inode = le32(raw, offset);
                let rec_len = usize::from(le16(raw, offset + 4));
                if rec_len < 8 || rec_len % 4 != 0 || offset + rec_len > raw.len() {
                    kwarn!("ext4: corrupt directory {} block {}", ino, index);
                    return Err(VfsError::IoError);
                }
                let name_len = usize::from(raw[offset + 6]);
                let ftype = raw[offset + 7];
                if inode != 0
                    && ftype != FT_DIR_CSUM
                    && offset + 8 + name_len <= raw.len()
                    && visit(
                        index as u32,
                        physical,
                        offset,
                        inode,
                        rec_len,
                        ftype,
                        &raw[offset + 8..offset + 8 + name_len],
                    )
                {
                    return Ok(());
                }
                offset += rec_len;
            }
        }
        Ok(())
    }

    fn dir_lookup(&mut self, ino: u32, name: &str) -> Result<Option<(u32, u8)>, VfsError> {
        let mut found = None;
        self.dir_scan(ino, |_, _, _, inode, _, ftype, entry| {
            if entry == name.as_bytes() {
                found = Some((inode, ftype));
                true
            } else {
                false
            }
        })?;
        Ok(found)
    }

    fn dir_seed(&mut self, ino: u32) -> Result<u32, VfsError> {
        let generation = le32(self.inode_raw(ino)?, 0x64);
        Ok(inode_seed(&self.geometry, ino, generation))
    }

    fn dir_writable_check(&mut self, ino: u32) -> Result<(), VfsError> {
        if le32(self.inode_raw(ino)?, 0x20) & FL_INDEX != 0 {
            return Err(VfsError::ReadOnlyFs);
        }
        Ok(())
    }

    fn dir_add(&mut self, dir: u32, name: &str, target: u32, ftype: u8) -> Result<(), VfsError> {
        self.dir_writable_check(dir)?;
        let block_size = self.geometry.block_size;
        let tail = if self.geometry.metadata_csum() { 12 } else { 0 };
        let usable = block_size - tail;
        let need = (8 + name.len() + 3) & !3;
        let seed = self.dir_seed(dir)?;
        let geometry = self.geometry.clone();
        let size = Self::inode_size(self.inode_raw(dir)?);
        for index in 0..size / block_size as u64 {
            let (physical, _) = self.map_block(dir, index as u32)?;
            let Some(physical) = physical else {
                continue;
            };
            let raw = self.read_block(physical)?;
            let mut offset = 0usize;
            let mut slot = None;
            while offset < usable {
                let inode = le32(raw, offset);
                let rec_len = usize::from(le16(raw, offset + 4));
                if rec_len < 8 || offset + rec_len > usable {
                    return Err(VfsError::IoError);
                }
                let name_len = usize::from(raw[offset + 6]);
                let used = if inode == 0 {
                    0
                } else {
                    (8 + name_len + 3) & !3
                };
                if rec_len - used >= need {
                    slot = Some((offset, used, rec_len));
                    break;
                }
                offset += rec_len;
            }
            if let Some((offset, used, rec_len)) = slot {
                let raw = self.modify(physical)?;
                let mut entry_offset = offset;
                let mut entry_len = rec_len;
                if used != 0 {
                    put16(raw, offset + 4, used as u16);
                    entry_offset += used;
                    entry_len -= used;
                }
                put32(raw, entry_offset, target);
                put16(raw, entry_offset + 4, entry_len as u16);
                raw[entry_offset + 6] = name.len() as u8;
                raw[entry_offset + 7] = ftype;
                raw[entry_offset + 8..entry_offset + 8 + name.len()]
                    .copy_from_slice(name.as_bytes());
                dirblock_checksum_set(&geometry, seed, raw);
                return Ok(());
            }
        }
        // No room: extend the directory by one block.
        let logical = (size / block_size as u64) as u32;
        let goal = self.write_goal(dir, logical);
        let (physical, _) = self.alloc_blocks(goal, 1)?;
        self.extent_insert(
            dir,
            Extent {
                logical,
                physical,
                length: 1,
                unwritten: false,
            },
        )?;
        self.adjust_blocks(dir, 1)?;
        self.store_extents(dir)?;
        let raw = self.modify_fresh(physical)?;
        put32(raw, 0, target);
        put16(raw, 4, usable as u16);
        raw[6] = name.len() as u8;
        raw[7] = ftype;
        raw[8..8 + name.len()].copy_from_slice(name.as_bytes());
        if tail != 0 {
            dirblock_tail_init(raw);
            dirblock_checksum_set(&geometry, seed, raw);
        }
        let memory = self.inode_mut(dir)?;
        Self::set_inode_size(&mut memory.raw, size + block_size as u64);
        Ok(())
    }

    fn dir_remove(&mut self, dir: u32, name: &str) -> Result<(), VfsError> {
        self.dir_writable_check(dir)?;
        let block_size = self.geometry.block_size;
        let tail = if self.geometry.metadata_csum() { 12 } else { 0 };
        let usable = block_size - tail;
        let seed = self.dir_seed(dir)?;
        let geometry = self.geometry.clone();
        let size = Self::inode_size(self.inode_raw(dir)?);
        for index in 0..size / block_size as u64 {
            let (physical, _) = self.map_block(dir, index as u32)?;
            let Some(physical) = physical else {
                continue;
            };
            let raw = self.read_block(physical)?;
            let mut offset = 0usize;
            let mut previous = None;
            let mut hit = None;
            while offset < usable {
                let inode = le32(raw, offset);
                let rec_len = usize::from(le16(raw, offset + 4));
                if rec_len < 8 || offset + rec_len > usable {
                    return Err(VfsError::IoError);
                }
                let name_len = usize::from(raw[offset + 6]);
                if inode != 0 && &raw[offset + 8..offset + 8 + name_len] == name.as_bytes() {
                    hit = Some((offset, rec_len));
                    break;
                }
                previous = Some(offset);
                offset += rec_len;
            }
            if let Some((offset, rec_len)) = hit {
                let raw = self.modify(physical)?;
                if let Some(previous) = previous {
                    let merged = usize::from(le16(raw, previous + 4)) + rec_len;
                    put16(raw, previous + 4, merged as u16);
                } else {
                    put32(raw, offset, 0);
                }
                dirblock_checksum_set(&geometry, seed, raw);
                return Ok(());
            }
        }
        Err(VfsError::NotFound)
    }

    fn dir_is_empty(&mut self, ino: u32) -> Result<bool, VfsError> {
        let mut empty = true;
        self.dir_scan(ino, |_, _, _, _, _, _, name| {
            if name != b"." && name != b".." {
                empty = false;
                true
            } else {
                false
            }
        })?;
        Ok(empty)
    }

    /// Rewrite the `..` entry of `dir`, used when a directory changes parent.
    fn dir_set_parent(&mut self, dir: u32, parent: u32) -> Result<(), VfsError> {
        let seed = self.dir_seed(dir)?;
        let geometry = self.geometry.clone();
        let (physical, _) = self.map_block(dir, 0)?;
        let physical = physical.ok_or(VfsError::IoError)?;
        let raw = self.modify(physical)?;
        let dot_len = usize::from(le16(raw, 4));
        if dot_len + 12 > raw.len()
            || raw[dot_len + 6] != 2
            || &raw[dot_len + 8..dot_len + 10] != b".."
        {
            return Err(VfsError::IoError);
        }
        put32(raw, dot_len, parent);
        dirblock_checksum_set(&geometry, seed, raw);
        Ok(())
    }

    fn links(&mut self, ino: u32) -> Result<u16, VfsError> {
        Ok(le16(self.inode_raw(ino)?, 0x1A))
    }

    fn set_links(&mut self, ino: u32, links: u16) -> Result<(), VfsError> {
        put16(&mut self.inode_mut(ino)?.raw, 0x1A, links);
        Ok(())
    }

    /// `dir_nlink`: a count of 1 means "too many to track", and stays there.
    fn dir_links_inc(&mut self, ino: u32) -> Result<(), VfsError> {
        let links = self.links(ino)?;
        if links >= 64999 {
            self.set_links(ino, 1)?;
        } else if links != 1 {
            self.set_links(ino, links + 1)?;
        }
        Ok(())
    }

    fn dir_links_dec(&mut self, ino: u32) -> Result<(), VfsError> {
        let links = self.links(ino)?;
        if links > 2 {
            self.set_links(ino, links - 1)?;
        }
        Ok(())
    }

    fn touch_dir(&mut self, ino: u32) -> Result<(), VfsError> {
        let now = now_seconds();
        Self::set_times(&mut self.inode_mut(ino)?.raw, false, true, true, now);
        self.inode_store(ino)
    }

    /// Allocate and initialize a new inode record, still without links.
    fn new_inode(
        &mut self,
        parent: u32,
        kind: InodeKind,
        permissions: u32,
    ) -> Result<u32, VfsError> {
        let directory = kind == InodeKind::Directory;
        let ino = self.alloc_inode(parent, directory)?;
        let size = self.geometry.inode_size;
        let now = now_seconds();
        let (uid, gid) = crate::syscall::identity::fs_ids();
        let mut raw = vec![0u8; size];
        let mode = mode_type(kind) | (permissions & 0o7777) as u16;
        put16(&mut raw, 0x00, mode);
        put16(&mut raw, 0x02, uid as u16);
        put16(&mut raw, 0x78, (uid >> 16) as u16);
        put16(&mut raw, 0x18, gid as u16);
        put16(&mut raw, 0x7A, (gid >> 16) as u16);
        if size > 128 {
            put16(&mut raw, 0x80, 32);
        }
        put32(&mut raw, 0x64, (crate::time::uptime_micros() as u32) ^ ino);
        let uses_extents = matches!(
            kind,
            InodeKind::Regular | InodeKind::Directory | InodeKind::Symlink
        );
        if uses_extents {
            put32(&mut raw, 0x20, FL_EXTENTS);
            put16(&mut raw, INODE_BLOCK_OFFSET, EXTENT_MAGIC);
            put16(&mut raw, INODE_BLOCK_OFFSET + 4, 4);
        }
        Self::set_times(&mut raw, true, true, true, now);
        if size >= 0x98 {
            put32(&mut raw, 0x90, now as u32);
            put32(&mut raw, 0x94, (now >> 32) as u32 & 3);
        }
        self.inodes.insert(
            ino,
            MemInode {
                raw,
                extents: Some(Vec::new()),
                tree_blocks: Vec::new(),
            },
        );
        Ok(ino)
    }

    /// Add `ino` to the on-disk orphan list; a crash then cannot leak it.
    fn orphan_add(&mut self, ino: u32) -> Result<(), VfsError> {
        let (block, offset) = self.super_location();
        let head = le32(self.read_block(block)?, offset + 0xE8);
        put32(&mut self.inode_mut(ino)?.raw, 0x14, head);
        self.inode_store(ino)?;
        let raw = self.modify(block)?;
        put32(raw, offset + 0xE8, ino);
        self.super_dirty = true;
        Ok(())
    }

    fn orphan_remove(&mut self, ino: u32) -> Result<(), VfsError> {
        let (block, offset) = self.super_location();
        let next = le32(self.inode_raw(ino)?, 0x14);
        let head = le32(self.read_block(block)?, offset + 0xE8);
        if head == ino {
            let raw = self.modify(block)?;
            put32(raw, offset + 0xE8, next);
            self.super_dirty = true;
            return Ok(());
        }
        let mut cursor = head;
        let mut guard = 0;
        while cursor != 0 && guard < 1_000_000 {
            let following = le32(self.inode_raw(cursor)?, 0x14);
            if following == ino {
                put32(&mut self.inode_mut(cursor)?.raw, 0x14, next);
                return self.inode_store(cursor);
            }
            cursor = following;
            guard += 1;
        }
        Err(VfsError::IoError)
    }

    /// Free an unlinked inode: data, extent tree, record and bitmap bit.
    fn destroy_inode(&mut self, ino: u32) -> Result<(), VfsError> {
        let raw = self.inode_raw(ino)?.to_vec();
        let mode = le16(&raw, 0);
        let directory = mode & S_IFMT == S_IFDIR;
        self.orphan_remove(ino)?;
        let uses_blocks = le32(&raw, 0x20) & FL_EXTENTS != 0;
        if uses_blocks {
            self.extent_truncate(ino, 0)?;
            for block in core::mem::take(&mut self.inode_mut(ino)?.tree_blocks) {
                self.free_blocks_range(block, 1, true)?;
            }
        }
        let now = now_seconds() as u32;
        let memory = self.inode_mut(ino)?;
        put32(&mut memory.raw, 0x14, now);
        Self::set_inode_size(&mut memory.raw, 0);
        put32(&mut memory.raw, 0x1C, 0);
        self.inode_store(ino)?;
        self.free_inode_bit(ino, directory)?;
        self.inodes.remove(&ino);
        Ok(())
    }

    /// Walk the crash-time orphan list and finish its deletions.
    fn process_orphans(&mut self) -> Result<u32, VfsError> {
        let (block, offset) = self.super_location();
        let mut cursor = le32(self.read_block(block)?, offset + 0xE8);
        let mut count = 0;
        while cursor != 0 && count < 1_000_000 {
            let next = le32(self.inode_raw(cursor)?, 0x14);
            if self.links(cursor)? == 0 {
                self.destroy_inode(cursor)?;
            } else {
                self.orphan_remove(cursor)?;
                put32(&mut self.inode_mut(cursor)?.raw, 0x14, 0);
                self.inode_store(cursor)?;
            }
            cursor = next;
            count += 1;
        }
        Ok(count)
    }

    // -------------------------------------------------------------- transactions

    /// Compute every deferred checksum so the running transaction is final.
    fn finalize_metadata(&mut self) -> Result<(), VfsError> {
        let geometry = self.geometry.clone();
        for group in core::mem::take(&mut self.dirty_block_bitmaps) {
            let mut record = self.desc(group)?;
            let location = self.block_bitmap_of(&record);
            if geometry.metadata_csum() {
                let bytes = geometry.blocks_per_group as usize / 8;
                let bitmap = self.read_block(location)?;
                let crc = crc32c(geometry.csum_seed, &bitmap[..bytes]);
                put16(&mut record, 0x18, crc as u16);
                if self.desc_hi() {
                    put16(&mut record, 0x38, (crc >> 16) as u16);
                }
                self.desc_store(group, &record)?;
            }
        }
        for group in core::mem::take(&mut self.dirty_inode_bitmaps) {
            let mut record = self.desc(group)?;
            let location = self.inode_bitmap_of(&record);
            if geometry.metadata_csum() {
                let bytes = geometry.inodes_per_group as usize / 8;
                let bitmap = self.read_block(location)?;
                let crc = crc32c(geometry.csum_seed, &bitmap[..bytes]);
                put16(&mut record, 0x1A, crc as u16);
                if self.desc_hi() {
                    put16(&mut record, 0x3A, (crc >> 16) as u16);
                }
                self.desc_store(group, &record)?;
            }
        }
        for group in core::mem::take(&mut self.dirty_groups) {
            let (block, offset) = self.desc_location(group);
            let size = geometry.desc_size;
            let raw = self.modify(block)?;
            desc_checksum_set(&geometry, group, &mut raw[offset..offset + size]);
        }
        if self.super_dirty {
            self.super_dirty = false;
            let (block, offset) = self.super_location();
            let free_blocks = self.free_blocks;
            let free_inodes = self.free_inodes;
            let raw = self.modify(block)?;
            let record = &mut raw[offset..offset + 1024];
            put32(record, 0x0C, free_blocks as u32);
            put32(record, 0x158, (free_blocks >> 32) as u32);
            put32(record, 0x10, free_inodes);
            super_checksum_set(&geometry, record);
        }
        Ok(())
    }

    fn write_ordered(&mut self) -> Result<(), VfsError> {
        let blocks: Vec<u64> = self.ordered.iter().copied().collect();
        let size = self.geometry.block_size;
        let mut index = 0;
        while index < blocks.len() {
            let mut end = index + 1;
            while end < blocks.len() && blocks[end] == blocks[end - 1] + 1 && end - index < 256 {
                end += 1;
            }
            let mut buffer = Vec::with_capacity((end - index) * size);
            for &block in &blocks[index..end] {
                if let Some(entry) = self.cache.entries.get(&block) {
                    buffer.extend_from_slice(&entry.data);
                } else {
                    return Err(VfsError::IoError);
                }
            }
            self.device_write(blocks[index], end - index, &buffer)?;
            for &block in &blocks[index..end] {
                if let Some(entry) = self.cache.entries.get_mut(&block) {
                    entry.data_dirty = false;
                }
            }
            index = end;
        }
        self.ordered.clear();
        Ok(())
    }

    /// Commit the running transaction to the log in `data=ordered` order.
    fn commit(&mut self) -> Result<(), VfsError> {
        if self.read_only || self.aborted {
            return Ok(());
        }
        self.finalize_metadata()?;
        self.write_ordered()?;
        if self.tx_blocks.is_empty() && self.revokes.is_empty() {
            self.pending_free.clear();
            return Ok(());
        }
        let result = self.commit_log();
        if let Err(error) = result {
            kwarn!(
                "ext4: journal commit failed ({:?}); remounting read-only",
                error
            );
            self.aborted = true;
            return Err(error);
        }
        self.commits += 1;
        Ok(())
    }

    fn commit_log(&mut self) -> Result<(), VfsError> {
        let blocks: Vec<u64> = self.tx_blocks.iter().copied().collect();
        let revokes: Vec<u64> = self.revokes.iter().copied().collect();
        if self.journal.is_some() {
            let (cost, used, capacity, fresh) = {
                let journal = self.journal.as_ref().unwrap();
                (
                    journal.commit_cost(blocks.len(), revokes.len()),
                    journal.used(),
                    journal.capacity(),
                    journal.starting_fresh(),
                )
            };
            if used + cost >= capacity {
                return Err(VfsError::NoSpace);
            }
            if fresh {
                self.set_recover_flag(true)?;
            }
            let mut pairs: Vec<(u64, &[u8])> = Vec::with_capacity(blocks.len());
            for &block in &blocks {
                pairs.push((block, &self.cache.entries[&block].data));
            }
            self.journal
                .as_mut()
                .unwrap()
                .commit(&pairs, &revokes, now_seconds())?;
        } else {
            // No journal on this volume: write metadata straight through.
            let size = self.geometry.block_size;
            for &block in &blocks {
                let data = self.cache.entries[&block].data.clone();
                debug_assert_eq!(data.len(), size);
                self.device_write(block, 1, &data)?;
            }
            self.device.flush().map_err(|_| VfsError::IoError)?;
        }
        for &block in &blocks {
            if let Some(entry) = self.cache.entries.get_mut(&block) {
                entry.running = false;
                entry.checkpoint = self.journal.is_some();
            }
            if self.journal.is_some() {
                self.checkpoint_blocks.insert(block);
            }
        }
        self.tx_blocks.clear();
        self.revokes.clear();
        self.pending_free.clear();
        // The running transaction is empty here, so a checkpoint can always
        // drain the log. Keeping usage under half leaves room for the next
        // commit, which `operation_end` bounds to a quarter of the log.
        let drain = self
            .journal
            .as_ref()
            .is_some_and(|journal| journal.used() >= journal.capacity() / 2);
        if drain {
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Write committed metadata home and empty the log.
    fn checkpoint(&mut self) -> Result<(), VfsError> {
        let blocks: Vec<u64> = core::mem::take(&mut self.checkpoint_blocks)
            .into_iter()
            .collect();
        for &block in &blocks {
            let running = self.cache.entries.get(&block).is_some_and(|e| e.running);
            if let Some(entry) = self.cache.entries.get_mut(&block) {
                entry.checkpoint = false;
            }
            if running {
                // The block holds uncommitted changes: its last committed
                // contents remain in the log until after the next commit.
                self.checkpoint_blocks.insert(block);
                continue;
            }
            if let Some(entry) = self.cache.entries.get(&block) {
                let data = entry.data.clone();
                self.device_write(block, 1, &data)?;
            }
        }
        if !self.checkpoint_blocks.is_empty() {
            // Blocks in the running transaction force a commit first, so the
            // log can only be emptied by the caller's next commit + checkpoint.
            let pending: Vec<u64> = self.checkpoint_blocks.iter().copied().collect();
            for block in pending {
                if let Some(entry) = self.cache.entries.get_mut(&block) {
                    entry.checkpoint = true;
                }
            }
            return self.device.flush().map_err(|_| VfsError::IoError);
        }
        self.device.flush().map_err(|_| VfsError::IoError)?;
        if let Some(journal) = self.journal.as_mut() {
            journal.mark_empty()?;
        }
        if self.tx_blocks.is_empty() {
            self.set_recover_flag(false)?;
        }
        Ok(())
    }

    /// Persist the needs-recovery flag out of band, as Linux does at mount.
    ///
    /// Works from the on-disk superblock: the cached copy can hold uncommitted
    /// fields such as the orphan list head, which must not reach disk early.
    fn set_recover_flag(&mut self, on: bool) -> Result<(), VfsError> {
        let geometry = self.geometry.clone();
        let (block, offset) = self.super_location();
        let apply = |record: &mut [u8]| {
            let incompat = le32(record, 0x60);
            let updated = if on {
                incompat | INCOMPAT_RECOVER
            } else {
                incompat & !INCOMPAT_RECOVER
            };
            put32(record, 0x60, updated);
            super_checksum_set(&geometry, record);
        };
        let mut disk = vec![0u8; geometry.block_size];
        self.device_read(block, 1, &mut disk)?;
        apply(&mut disk[offset..offset + 1024]);
        self.device_write(block, 1, &disk)?;
        self.device.flush().map_err(|_| VfsError::IoError)?;
        self.load(block)?;
        let entry = self.cache.entries.get_mut(&block).unwrap();
        apply(&mut entry.data[offset..offset + 1024]);
        Ok(())
    }

    fn sync_all(&mut self) -> Result<(), VfsError> {
        self.commit()?;
        if self.read_only || self.aborted {
            return Ok(());
        }
        self.checkpoint()?;
        Ok(())
    }

    /// Commit at operation boundaries once the transaction is big or old.
    fn operation_end(&mut self) -> Result<(), VfsError> {
        if self.read_only || self.aborted {
            return Ok(());
        }
        let limit = self
            .journal
            .as_ref()
            .map(|journal| (journal.capacity() / 4) as usize)
            .unwrap_or(1024);
        if self.tx_blocks.len() + self.ordered.len() / 8 >= limit
            || self.ordered.len() >= self.cache.limit / 2
        {
            self.commit()?;
        }
        Ok(())
    }

    fn operation_begin(&mut self, reap: Vec<u32>) -> Result<(), VfsError> {
        // Only safe between operations: every cached inode is stored by now.
        if self.inodes.len() >= 65536 {
            self.inodes.clear();
        }
        if self.read_only || self.aborted {
            return Ok(());
        }
        for ino in reap {
            if self.inode_raw(ino).is_ok() && self.links(ino).unwrap_or(1) == 0 {
                self.destroy_inode(ino)?;
            }
        }
        Ok(())
    }
}

impl Ext4Fs {
    fn begin(&self) -> Result<MutexGuard<'_, State>, VfsError> {
        let mut reap = core::mem::take(&mut *self.reap.lock());
        if !reap.is_empty() {
            // A new handle may have appeared after the old one queued this ino.
            let handles = self.handles.lock();
            reap.retain(|ino| !handles.get(ino).is_some_and(|weak| weak.strong_count() > 0));
            drop(handles);
            reap.sort_unstable();
            reap.dedup();
        }
        let mut state = self.state.lock();
        state.operation_begin(reap)?;
        Ok(state)
    }

    fn handle(self: &Arc<Self>, ino: u32, kind: InodeKind) -> Arc<Ext4Inode> {
        let mut handles = self.handles.lock();
        if let Some(existing) = handles.get(&ino).and_then(Weak::upgrade) {
            return existing;
        }
        let handle = Arc::new(Ext4Inode {
            fs: self.clone(),
            ino,
            kind,
        });
        handles.insert(ino, Arc::downgrade(&handle));
        handle
    }

    fn strong(&self) -> Result<Arc<Ext4Fs>, VfsError> {
        self.self_ref.lock().upgrade().ok_or(VfsError::IoError)
    }

    /// Mount a volume, replaying its journal first when needed.
    pub fn mount(device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, VfsError> {
        if device.info().block_size != SECTOR_SIZE {
            return Err(VfsError::InvalidArgument);
        }
        let device_ro = device.info().read_only;
        let mut geometry = read_geometry(&device)?;
        let mut read_only = device_ro;
        let (ro_only_reason, recovering) = feature_check(&device)?;
        if let Some(reason) = ro_only_reason {
            kwarn!("ext4: read-only mount: {}", reason);
            read_only = true;
        }

        // Recovery and journal setup happen before any cached metadata exists.
        let mut journal = None;
        if geometry.journal_inum != 0 {
            let map = load_journal_map(&device, &geometry)?;
            match Journal::open(
                device.clone(),
                geometry.block_size,
                map,
                geometry.metadata_csum(),
            ) {
                Ok(mut log) => {
                    if !log.is_empty() && !read_only {
                        let report = log.recover()?;
                        kinfo!(
                            "ext4: journal replayed: {} transactions, {} blocks, {} revoked",
                            report.transactions,
                            report.blocks,
                            report.revoked
                        );
                        geometry = read_geometry(&device)?;
                    } else if !log.is_empty() {
                        return Err(VfsError::ReadOnlyFs);
                    }
                    if !read_only {
                        log.adopt_features(&geometry.uuid)?;
                    }
                    journal = Some(log);
                }
                Err(VfsError::NotSupported) => {
                    if recovering {
                        return Err(VfsError::NotSupported);
                    }
                    kwarn!("ext4: journal format unsupported; read-only mount");
                    read_only = true;
                }
                Err(error) => return Err(error),
            }
        }

        let frames = crate::mm::pmm::stats().free as usize;
        let limit = (frames / 4).clamp(1024, 256 * 1024 * 1024 / geometry.block_size);
        let mut state = State {
            device,
            geometry: geometry.clone(),
            read_only,
            aborted: false,
            cache: BlockCache {
                entries: BTreeMap::new(),
                lru: BTreeMap::new(),
                tick: 0,
                limit,
                hits: 0,
                misses: 0,
            },
            journal,
            tx_blocks: BTreeSet::new(),
            checkpoint_blocks: BTreeSet::new(),
            ordered: BTreeSet::new(),
            revokes: BTreeSet::new(),
            pending_free: BTreeSet::new(),
            dirty_block_bitmaps: BTreeSet::new(),
            dirty_inode_bitmaps: BTreeSet::new(),
            dirty_groups: BTreeSet::new(),
            super_dirty: false,
            inodes: BTreeMap::new(),
            free_blocks: 0,
            free_inodes: 0,
            last_group: 0,
            tx_started_micros: 0,
            commits: 0,
        };

        // On-disk totals are advisory after a crash, so derive them from groups.
        for group in 0..geometry.group_count {
            let record = state.desc(group)?;
            state.free_blocks += u64::from(state.free_blocks_of(&record));
            state.free_inodes += state.free_inodes_of(&record);
        }
        if !read_only {
            let orphans = state.process_orphans()?;
            if orphans != 0 {
                kinfo!("ext4: cleaned up {} orphan inodes", orphans);
                state.commit()?;
            }
        }
        kinfo!(
            "ext4: mounted {} blocks of {} bytes, {} free, journal={}, csum={}, {}",
            geometry.blocks_count,
            geometry.block_size,
            state.free_blocks,
            state.journal.is_some(),
            geometry.metadata_csum(),
            if read_only { "ro" } else { "rw" }
        );
        let filesystem = Arc::new(Self {
            state: Mutex::new(state),
            geometry,
            handles: SpinLock::new(BTreeMap::new()),
            reap: SpinLock::new(Vec::new()),
            self_ref: SpinLock::new(Weak::new()),
            flusher_started: AtomicBool::new(false),
        });
        *filesystem.self_ref.lock() = Arc::downgrade(&filesystem);
        Ok(filesystem)
    }

    /// Start the periodic commit thread once the scheduler can run it.
    pub fn start_flusher(self: &Arc<Self>) {
        if self.flusher_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let filesystem = self.clone();
        let _ = crate::sched::spawn(
            move || {
                loop {
                    crate::sched::sleep_micros(COMMIT_INTERVAL_MICROS);
                    if let Ok(mut state) = filesystem.begin() {
                        let age =
                            crate::time::uptime_micros().saturating_sub(state.tx_started_micros);
                        let idle = state.tx_blocks.is_empty() && state.ordered.is_empty();
                        if !idle && age >= COMMIT_INTERVAL_MICROS {
                            let _ = state.commit();
                        }
                    }
                }
            },
            0,
        );
    }
}

fn read_geometry(device: &Arc<dyn BlockDevice>) -> Result<Geometry, VfsError> {
    let mut raw = vec![0u8; 1024];
    device
        .read_blocks(SUPER_OFFSET / SECTOR_SIZE as u64, 2, &mut raw)
        .map_err(|_| VfsError::IoError)?;
    if le16(&raw, 0x38) != SUPER_MAGIC {
        return Err(VfsError::InvalidArgument);
    }
    let shift = le32(&raw, 0x18);
    if shift > 2 {
        return Err(VfsError::NotSupported);
    }
    let block_size = 1024usize << shift;
    let incompat = le32(&raw, 0x60);
    let ro_compat = le32(&raw, 0x64);
    let has_64 = incompat & INCOMPAT_64BIT != 0;
    let blocks_low = u64::from(le32(&raw, 0x04));
    let blocks_high = if has_64 {
        u64::from(le32(&raw, 0x150))
    } else {
        0
    };
    let blocks_count = (blocks_high << 32) | blocks_low;
    let first_data_block = u64::from(le32(&raw, 0x14));
    let blocks_per_group = le32(&raw, 0x20);
    let inodes_per_group = le32(&raw, 0x28);
    let inodes_count = le32(&raw, 0x00);
    let revision = le32(&raw, 0x4C);
    let inode_size = if revision >= 1 {
        usize::from(le16(&raw, 0x58))
    } else {
        128
    };
    let desc_size = if has_64 {
        usize::from(le16(&raw, 0xFE)).max(32)
    } else {
        32
    };
    if blocks_per_group == 0
        || inodes_per_group == 0
        || blocks_count <= first_data_block
        || inode_size < 128
        || inode_size > block_size
        || !inode_size.is_power_of_two()
        || desc_size > block_size
        || (blocks_per_group as usize) > block_size * 8
    {
        return Err(VfsError::InvalidArgument);
    }
    let group_count = (blocks_count - first_data_block).div_ceil(u64::from(blocks_per_group));
    let group_count = u32::try_from(group_count).map_err(|_| VfsError::InvalidArgument)?;
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&raw[0x68..0x78]);
    let csum_seed = if incompat & INCOMPAT_CSUM_SEED != 0 {
        le32(&raw, 0x270)
    } else {
        crc32c(!0, &uuid)
    };
    let journal_inum = if le32(&raw, 0x5C) & COMPAT_HAS_JOURNAL != 0 {
        le32(&raw, 0xE0)
    } else {
        0
    };
    Ok(Geometry {
        block_size,
        first_data_block,
        blocks_count,
        blocks_per_group,
        inodes_per_group,
        inode_size,
        inodes_count,
        desc_size,
        group_count,
        reserved_gdt: u32::from(le16(&raw, 0xCE)),
        incompat,
        ro_compat,
        csum_seed,
        journal_inum,
        desc_per_block: (block_size / desc_size) as u32,
        sparse_super: ro_compat & RO_SPARSE_SUPER != 0,
        uuid,
    })
}

/// Returns `(reason to force read-only, needs journal recovery)`.
fn feature_check(device: &Arc<dyn BlockDevice>) -> Result<(Option<String>, bool), VfsError> {
    let mut raw = vec![0u8; 1024];
    device
        .read_blocks(SUPER_OFFSET / SECTOR_SIZE as u64, 2, &mut raw)
        .map_err(|_| VfsError::IoError)?;
    let incompat = le32(&raw, 0x60);
    let ro_compat = le32(&raw, 0x64);
    if incompat & !INCOMPAT_SUPPORTED != 0 {
        kwarn!(
            "ext4: unsupported INCOMPAT features {:#x}",
            incompat & !INCOMPAT_SUPPORTED
        );
        return Err(VfsError::NotSupported);
    }
    let reason = (ro_compat & !RO_SUPPORTED != 0).then(|| {
        alloc::format!(
            "unsupported RO_COMPAT features {:#x}",
            ro_compat & !RO_SUPPORTED
        )
    });
    Ok((reason, incompat & INCOMPAT_RECOVER != 0))
}

/// Read the journal inode's extents directly from disk, without any cache.
fn load_journal_map(
    device: &Arc<dyn BlockDevice>,
    geometry: &Geometry,
) -> Result<JournalMap, VfsError> {
    let size = geometry.block_size;
    let per = size / SECTOR_SIZE;
    let read = |block: u64, out: &mut [u8]| -> Result<(), VfsError> {
        device
            .read_blocks(block * per as u64, per, out)
            .map_err(|_| VfsError::IoError)
    };
    let ino = geometry.journal_inum;
    let group = (ino - 1) / geometry.inodes_per_group;
    let index = ((ino - 1) % geometry.inodes_per_group) as usize;
    let mut block = vec![0u8; size];
    let desc_block = geometry.first_data_block + 1 + u64::from(group / geometry.desc_per_block);
    read(desc_block, &mut block)?;
    let record = (group % geometry.desc_per_block) as usize * geometry.desc_size;
    let table_high = if geometry.desc_size >= 64 {
        le32(&block, record + 0x28)
    } else {
        0
    };
    let table = (u64::from(table_high) << 32) | u64::from(le32(&block, record + 8));
    let byte = index * geometry.inode_size;
    read(table + (byte / size) as u64, &mut block)?;
    let offset = byte % size;
    let raw = block[offset..offset + geometry.inode_size].to_vec();
    if le32(&raw, 0x20) & FL_EXTENTS == 0 {
        return Err(VfsError::NotSupported);
    }
    let mut map = JournalMap::new();
    let root = raw[INODE_BLOCK_OFFSET..INODE_BLOCK_OFFSET + INODE_BLOCK_LEN].to_vec();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if le16(&node, 0) != EXTENT_MAGIC {
            return Err(VfsError::IoError);
        }
        let entries = usize::from(le16(&node, 2));
        let depth = le16(&node, 6);
        for entry in 0..entries {
            let at = 12 + entry * 12;
            if depth == 0 {
                let length = u32::from(le16(&node, at + 4));
                let physical =
                    (u64::from(le16(&node, at + 6)) << 32) | u64::from(le32(&node, at + 8));
                map.push((le32(&node, at), physical, length.min(32768)));
            } else {
                let child = (u64::from(le16(&node, at + 8)) << 32) | u64::from(le32(&node, at + 4));
                let mut child_block = vec![0u8; size];
                read(child, &mut child_block)?;
                stack.push(child_block);
            }
        }
    }
    Ok(map)
}

impl Drop for Ext4Inode {
    fn drop(&mut self) {
        let mut handles = self.fs.handles.lock();
        if let Some(weak) = handles.get(&self.ino)
            && weak.strong_count() == 0
        {
            handles.remove(&self.ino);
        }
        drop(handles);
        // Liveness is decided under the state lock in task context.
        self.fs.reap.lock().push(self.ino);
    }
}

impl Ext4Inode {
    fn name_ok(name: &str) -> Result<(), VfsError> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(VfsError::InvalidArgument);
        }
        if name.len() > MAX_NAME {
            return Err(VfsError::NameTooLong);
        }
        Ok(())
    }

    fn as_directory(&self) -> Result<(), VfsError> {
        if self.kind == InodeKind::Directory {
            Ok(())
        } else {
            Err(VfsError::NotADirectory)
        }
    }

    fn create_node(
        &self,
        name: &str,
        kind: InodeKind,
        permissions: u32,
        rdev: Option<u64>,
    ) -> Result<Arc<dyn Inode>, VfsError> {
        self.as_directory()?;
        Self::name_ok(name)?;
        let mut state = self.fs.begin()?;
        if state.read_only {
            return Err(VfsError::ReadOnlyFs);
        }
        if state.dir_lookup(self.ino, name)?.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        state.dir_writable_check(self.ino)?;
        let ino = state.new_inode(self.ino, kind, permissions)?;
        if let Some(rdev) = rdev {
            let major = crate::vfs::abi::major(rdev);
            let minor = crate::vfs::abi::minor(rdev);
            let encoded = ((minor & 0xFF) | (major << 8) | ((minor & !0xFF) << 12)) as u32;
            put32(
                &mut state.inode_mut(ino)?.raw,
                INODE_BLOCK_OFFSET + 4,
                encoded,
            );
        }
        if kind == InodeKind::Directory {
            state.set_links(ino, 2)?;
            state.init_directory(ino, self.ino)?;
            state.dir_links_inc(self.ino)?;
        } else {
            state.set_links(ino, 1)?;
        }
        state.inode_store(ino)?;
        if let Err(error) = state.dir_add(self.ino, name, ino, file_type_of(kind)) {
            state.abandon_new_inode(ino, kind, self.ino)?;
            return Err(error);
        }
        state.touch_dir(self.ino)?;
        state.operation_end()?;
        drop(state);
        let filesystem = self.fs.strong()?;
        Ok(filesystem.handle(ino, kind))
    }
}

impl State {
    /// Undo `new_inode` after the parent entry could not be added.
    fn abandon_new_inode(
        &mut self,
        ino: u32,
        kind: InodeKind,
        parent: u32,
    ) -> Result<(), VfsError> {
        if self.inode_raw(ino)?.len() >= 0x24 && le32(self.inode_raw(ino)?, 0x20) & FL_EXTENTS != 0
        {
            self.extent_truncate(ino, 0)?;
        }
        let directory = kind == InodeKind::Directory;
        if directory {
            self.dir_links_dec(parent)?;
            self.inode_store(parent)?;
        }
        let memory = self.inode_mut(ino)?;
        put16(&mut memory.raw, 0x1A, 0);
        put32(&mut memory.raw, 0x14, now_seconds() as u32);
        self.inode_store(ino)?;
        self.free_inode_bit(ino, directory)?;
        self.inodes.remove(&ino);
        Ok(())
    }

    fn init_directory(&mut self, ino: u32, parent: u32) -> Result<(), VfsError> {
        let geometry = self.geometry.clone();
        let goal = self.inode_goal(ino);
        let (physical, _) = self.alloc_blocks(goal, 1)?;
        self.extent_insert(
            ino,
            Extent {
                logical: 0,
                physical,
                length: 1,
                unwritten: false,
            },
        )?;
        self.adjust_blocks(ino, 1)?;
        self.store_extents(ino)?;
        let size = geometry.block_size;
        let seed = self.dir_seed(ino)?;
        let tail = if geometry.metadata_csum() { 12 } else { 0 };
        let raw = self.modify_fresh(physical)?;
        put32(raw, 0, ino);
        put16(raw, 4, 12);
        raw[6] = 1;
        raw[7] = FT_DIR;
        raw[8] = b'.';
        put32(raw, 12, parent);
        put16(raw, 16, (size - tail - 12) as u16);
        raw[18] = 2;
        raw[19] = FT_DIR;
        raw[20] = b'.';
        raw[21] = b'.';
        if tail != 0 {
            dirblock_tail_init(raw);
            dirblock_checksum_set(&geometry, seed, raw);
        }
        let memory = self.inode_mut(ino)?;
        Self::set_inode_size(&mut memory.raw, size as u64);
        Ok(())
    }
}

impl Inode for Ext4Inode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut state = self.fs.state.lock();
        state.stat(self.ino, self.fs.device_id())
    }

    fn chmod(&self, mode: u32) -> Result<(), VfsError> {
        let mut state = self.fs.begin()?;
        let now = now_seconds();
        let memory = state.inode_mut(self.ino)?;
        let current = le16(&memory.raw, 0);
        put16(
            &mut memory.raw,
            0,
            (current & S_IFMT) | (mode & 0o7777) as u16,
        );
        State::set_times(&mut memory.raw, false, false, true, now);
        state.inode_store(self.ino)?;
        state.operation_end()
    }

    fn chown(&self, uid: u32, gid: u32) -> Result<(), VfsError> {
        let mut state = self.fs.begin()?;
        let now = now_seconds();
        let memory = state.inode_mut(self.ino)?;
        if uid != u32::MAX {
            put16(&mut memory.raw, 0x02, uid as u16);
            put16(&mut memory.raw, 0x78, (uid >> 16) as u16);
        }
        if gid != u32::MAX {
            put16(&mut memory.raw, 0x18, gid as u16);
            put16(&mut memory.raw, 0x7A, (gid >> 16) as u16);
        }
        State::set_times(&mut memory.raw, false, false, true, now);
        state.inode_store(self.ino)?;
        state.operation_end()
    }

    fn set_times(
        &self,
        atime: Option<(i64, i64)>,
        mtime: Option<(i64, i64)>,
    ) -> Result<(), VfsError> {
        let mut state = self.fs.begin()?;
        let now = now_seconds();
        let memory = state.inode_mut(self.ino)?;
        let raw = &mut memory.raw;
        let has_extra = raw.len() > 128 && usize::from(le16(raw, 0x80)) + 128 >= 0x90;
        let encode = |seconds: i64, nanos: i64| {
            let epoch = ((seconds - i64::from(seconds as i32)) >> 32) as u32 & 3;
            epoch | ((nanos as u32) << 2)
        };
        if let Some((seconds, nanos)) = atime {
            put32(raw, 0x08, seconds as u32);
            if has_extra {
                put32(raw, 0x8C, encode(seconds, nanos));
            }
        }
        if let Some((seconds, nanos)) = mtime {
            put32(raw, 0x10, seconds as u32);
            if has_extra {
                put32(raw, 0x88, encode(seconds, nanos));
            }
        }
        State::set_times(raw, false, false, true, now);
        state.inode_store(self.ino)?;
        state.operation_end()
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        if self.kind == InodeKind::Directory {
            return Err(VfsError::IsADirectory);
        }
        let mut state = self.fs.state.lock();
        state.read_file(self.ino, offset, buffer)
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        if self.kind == InodeKind::Directory {
            return Err(VfsError::IsADirectory);
        }
        let mut done = 0;
        while done < buffer.len() {
            let mut state = self.fs.begin()?;
            let written = match state.write_file(self.ino, offset + done as u64, &buffer[done..]) {
                Ok(written) => written,
                Err(VfsError::NoSpace) if done > 0 => return Ok(done),
                Err(error) => return Err(error),
            };
            state.operation_end()?;
            if written == 0 {
                break;
            }
            done += written;
        }
        Ok(done)
    }

    fn truncate(&self, size: u64) -> Result<(), VfsError> {
        if self.kind == InodeKind::Directory {
            return Err(VfsError::IsADirectory);
        }
        let mut state = self.fs.begin()?;
        state.truncate_file(self.ino, size)?;
        state.operation_end()
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        self.as_directory()?;
        let mut state = self.fs.state.lock();
        let (ino, _) = state
            .dir_lookup(self.ino, name)?
            .ok_or(VfsError::NotFound)?;
        let mode = le16(state.inode_raw(ino)?, 0);
        let kind = kind_from_mode(mode)?;
        drop(state);
        Ok(self.fs.strong()?.handle(ino, kind))
    }

    fn create(&self, name: &str, kind: InodeKind, mode: u32) -> Result<Arc<dyn Inode>, VfsError> {
        match kind {
            InodeKind::Regular | InodeKind::Fifo | InodeKind::Socket => {
                self.create_node(name, kind, mode, None)
            }
            _ => Err(VfsError::NotSupported),
        }
    }

    fn mkdir(&self, name: &str, mode: u32) -> Result<Arc<dyn Inode>, VfsError> {
        self.create_node(name, InodeKind::Directory, mode, None)
    }

    fn mknod_device(
        &self,
        name: &str,
        kind: InodeKind,
        mode: u32,
        rdev: u64,
    ) -> Result<Arc<dyn Inode>, VfsError> {
        self.create_node(name, kind, mode, Some(rdev))
    }

    fn symlink(&self, name: &str, target: &str) -> Result<Arc<dyn Inode>, VfsError> {
        if target.is_empty() || target.len() >= self.fs.geometry.block_size {
            return Err(VfsError::NameTooLong);
        }
        let created = self.create_node(name, InodeKind::Symlink, 0o777, None)?;
        let ino = created.metadata()?.ino as u32;
        let mut state = self.fs.begin()?;
        if target.len() <= FAST_SYMLINK_MAX {
            let memory = state.inode_mut(ino)?;
            let flags = le32(&memory.raw, 0x20) & !FL_EXTENTS;
            put32(&mut memory.raw, 0x20, flags);
            let field = &mut memory.raw[INODE_BLOCK_OFFSET..INODE_BLOCK_OFFSET + INODE_BLOCK_LEN];
            field.fill(0);
            field[..target.len()].copy_from_slice(target.as_bytes());
            State::set_inode_size(&mut memory.raw, target.len() as u64);
            state.inode_store(ino)?;
        } else {
            state.write_file(ino, 0, target.as_bytes())?;
        }
        state.operation_end()?;
        Ok(created)
    }

    fn readlink(&self) -> Result<String, VfsError> {
        if self.kind != InodeKind::Symlink {
            return Err(VfsError::InvalidArgument);
        }
        let mut state = self.fs.state.lock();
        let raw = state.inode_raw(self.ino)?.to_vec();
        let size = State::inode_size(&raw) as usize;
        if size >= self.fs.geometry.block_size {
            return Err(VfsError::IoError);
        }
        let bytes = if le32(&raw, 0x20) & FL_EXTENTS == 0 && size <= FAST_SYMLINK_MAX {
            raw[INODE_BLOCK_OFFSET..INODE_BLOCK_OFFSET + size].to_vec()
        } else {
            let mut buffer = vec![0u8; size];
            state.read_file(self.ino, 0, &mut buffer)?;
            buffer
        };
        String::from_utf8(bytes).map_err(|_| VfsError::IoError)
    }

    fn unlink(&self, name: &str) -> Result<(), VfsError> {
        self.as_directory()?;
        let mut state = self.fs.begin()?;
        let (ino, _) = state
            .dir_lookup(self.ino, name)?
            .ok_or(VfsError::NotFound)?;
        let mode = le16(state.inode_raw(ino)?, 0);
        if mode & S_IFMT == S_IFDIR {
            return Err(VfsError::IsADirectory);
        }
        state.dir_remove(self.ino, name)?;
        let links = state.links(ino)?.saturating_sub(1);
        state.set_links(ino, links)?;
        let now = now_seconds();
        State::set_times(&mut state.inode_mut(ino)?.raw, false, false, true, now);
        state.inode_store(ino)?;
        if links == 0 {
            state.orphan_add(ino)?;
        }
        state.touch_dir(self.ino)?;
        state.operation_end()?;
        drop(state);
        if links == 0 {
            self.fs.reap_if_unreferenced(ino);
        }
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), VfsError> {
        self.as_directory()?;
        let mut state = self.fs.begin()?;
        let (ino, _) = state
            .dir_lookup(self.ino, name)?
            .ok_or(VfsError::NotFound)?;
        let mode = le16(state.inode_raw(ino)?, 0);
        if mode & S_IFMT != S_IFDIR {
            return Err(VfsError::NotADirectory);
        }
        if !state.dir_is_empty(ino)? {
            return Err(VfsError::NotEmpty);
        }
        state.dir_remove(self.ino, name)?;
        state.set_links(ino, 0)?;
        state.inode_store(ino)?;
        state.orphan_add(ino)?;
        state.dir_links_dec(self.ino)?;
        state.touch_dir(self.ino)?;
        state.operation_end()?;
        drop(state);
        self.fs.reap_if_unreferenced(ino);
        Ok(())
    }

    fn link(&self, name: &str, target: &Arc<dyn Inode>) -> Result<(), VfsError> {
        self.as_directory()?;
        Self::name_ok(name)?;
        let any: Arc<dyn core::any::Any + Send + Sync> = target.clone();
        let target = any
            .downcast::<Ext4Inode>()
            .map_err(|_| VfsError::CrossDevice)?;
        if !Arc::ptr_eq(&target.fs, &self.fs) {
            return Err(VfsError::CrossDevice);
        }
        if target.kind == InodeKind::Directory {
            return Err(VfsError::NotPermitted);
        }
        let mut state = self.fs.begin()?;
        if state.dir_lookup(self.ino, name)?.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        let links = state.links(target.ino)?;
        if links >= 65000 {
            return Err(VfsError::TooManyLinks);
        }
        state.dir_add(self.ino, name, target.ino, file_type_of(target.kind))?;
        state.set_links(target.ino, links + 1)?;
        let now = now_seconds();
        State::set_times(
            &mut state.inode_mut(target.ino)?.raw,
            false,
            false,
            true,
            now,
        );
        state.inode_store(target.ino)?;
        state.touch_dir(self.ino)?;
        state.operation_end()
    }

    fn rename(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn Inode>,
        new_name: &str,
        no_replace: bool,
    ) -> Result<(), VfsError> {
        self.as_directory()?;
        Self::name_ok(new_name)?;
        let any: Arc<dyn core::any::Any + Send + Sync> = new_parent.clone();
        let destination = any
            .downcast::<Ext4Inode>()
            .map_err(|_| VfsError::CrossDevice)?;
        if !Arc::ptr_eq(&destination.fs, &self.fs) {
            return Err(VfsError::CrossDevice);
        }
        destination.as_directory()?;
        let mut state = self.fs.begin()?;
        let (ino, ftype) = state
            .dir_lookup(self.ino, old_name)?
            .ok_or(VfsError::NotFound)?;
        let moving_dir = le16(state.inode_raw(ino)?, 0) & S_IFMT == S_IFDIR;
        if moving_dir {
            // A directory may not move underneath itself.
            let mut cursor = destination.ino;
            let mut guard = 0;
            while cursor != ROOT_INO && guard < 4096 {
                if cursor == ino {
                    return Err(VfsError::InvalidArgument);
                }
                let parent = state.dir_lookup(cursor, "..")?.ok_or(VfsError::IoError)?.0;
                if parent == cursor {
                    break;
                }
                cursor = parent;
                guard += 1;
            }
        }
        let mut replaced = None;
        if let Some((existing, _)) = state.dir_lookup(destination.ino, new_name)? {
            if existing == ino {
                return Ok(());
            }
            if no_replace {
                return Err(VfsError::AlreadyExists);
            }
            let existing_dir = le16(state.inode_raw(existing)?, 0) & S_IFMT == S_IFDIR;
            match (moving_dir, existing_dir) {
                (true, false) => return Err(VfsError::NotADirectory),
                (false, true) => return Err(VfsError::IsADirectory),
                (true, true) if !state.dir_is_empty(existing)? => {
                    return Err(VfsError::NotEmpty);
                }
                _ => {}
            }
            state.dir_remove(destination.ino, new_name)?;
            replaced = Some((existing, existing_dir));
        }
        state.dir_add(destination.ino, new_name, ino, ftype)?;
        state.dir_remove(self.ino, old_name)?;
        if moving_dir && destination.ino != self.ino {
            state.dir_set_parent(ino, destination.ino)?;
            state.dir_links_dec(self.ino)?;
            if replaced.is_none_or(|(_, is_dir)| !is_dir) {
                state.dir_links_inc(destination.ino)?;
            }
        } else if moving_dir && replaced.is_some_and(|(_, is_dir)| is_dir) {
            state.dir_links_dec(destination.ino)?;
        }
        let mut orphaned = None;
        if let Some((existing, is_dir)) = replaced {
            let remaining = if is_dir {
                0
            } else {
                state.links(existing)?.saturating_sub(1)
            };
            state.set_links(existing, remaining)?;
            state.inode_store(existing)?;
            if remaining == 0 {
                state.orphan_add(existing)?;
                orphaned = Some(existing);
            }
        }
        let now = now_seconds();
        State::set_times(&mut state.inode_mut(ino)?.raw, false, false, true, now);
        state.inode_store(ino)?;
        state.touch_dir(self.ino)?;
        if destination.ino != self.ino {
            state.touch_dir(destination.ino)?;
        }
        state.operation_end()?;
        drop(state);
        if let Some(existing) = orphaned {
            self.fs.reap_if_unreferenced(existing);
        }
        Ok(())
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, VfsError> {
        self.as_directory()?;
        let mut state = self.fs.state.lock();
        let mut entries = Vec::new();
        state.dir_scan(self.ino, |_, _, _, inode, _, ftype, name| {
            if let Ok(name) = core::str::from_utf8(name) {
                entries.push((inode, ftype, String::from(name)));
            }
            false
        })?;
        let mut result = Vec::with_capacity(entries.len());
        for (ino, ftype, name) in entries {
            let kind = match ftype {
                FT_REG => InodeKind::Regular,
                FT_DIR => InodeKind::Directory,
                FT_CHR => InodeKind::CharDevice,
                FT_BLK => InodeKind::BlockDevice,
                FT_FIFO => InodeKind::Fifo,
                FT_SOCK => InodeKind::Socket,
                FT_SYMLINK => InodeKind::Symlink,
                _ => kind_from_mode(le16(state.inode_raw(ino)?, 0))?,
            };
            result.push(DirEntry {
                ino: u64::from(ino),
                name,
                kind,
            });
        }
        Ok(result)
    }

    fn sync(&self) -> Result<(), VfsError> {
        let mut state = self.fs.begin()?;
        state.commit()
    }
}

impl Ext4Fs {
    fn device_id(&self) -> u64 {
        0xE400
    }

    /// Queue an unlinked inode for deletion when no handle remains open.
    fn reap_if_unreferenced(&self, ino: u32) {
        let live = self
            .handles
            .lock()
            .get(&ino)
            .is_some_and(|weak| weak.strong_count() > 0);
        if !live {
            self.reap.lock().push(ino);
        }
    }

    pub fn stats(&self) -> (u64, u64, u64) {
        let state = self.state.lock();
        (state.cache.hits, state.cache.misses, state.commits)
    }
}

impl FileSystem for Ext4Fs {
    fn fs_type(&self) -> &'static str {
        "ext4"
    }

    fn root_inode(&self) -> Arc<dyn Inode> {
        let strong = self
            .strong()
            .expect("mounted ext4 filesystem outlives its root");
        strong.handle(ROOT_INO, InodeKind::Directory)
    }

    fn statfs(&self) -> Result<StatFs, VfsError> {
        let state = self.state.lock();
        Ok(StatFs {
            magic: EXT4_SUPER_MAGIC,
            block_size: self.geometry.block_size as u64,
            total_blocks: self.geometry.blocks_count,
            free_blocks: state.free_blocks,
            total_files: u64::from(self.geometry.inodes_count),
            free_files: u64::from(state.free_inodes),
            max_name_len: MAX_NAME as u64,
        })
    }

    fn sync(&self) -> Result<(), VfsError> {
        let mut state = self.begin()?;
        state.sync_all()
    }
}
