/*
 *
 *       src/mm/pmm.rs
 *       Bitmap physical frame allocator and ownership checks
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::sync::atomic::{AtomicU64, Ordering};

use crate::sync::SpinLock;
use limine::memmap::{Entry, MEMMAP_USABLE};

pub const PAGE_SIZE: u64 = 4096;
static HHDM: AtomicU64 = AtomicU64::new(0);
static PMM: SpinLock<State> = SpinLock::new(State::empty());

struct State {
    used_addr: usize,
    usable_addr: usize,
    refs_addr: usize,
    frames: usize,
    free: usize,
    hint: usize,
}

impl State {
    const fn empty() -> Self {
        Self {
            used_addr: 0,
            usable_addr: 0,
            refs_addr: 0,
            frames: 0,
            free: 0,
            hint: 0,
        }
    }

    fn bit(&self, base: usize, frame: usize) -> bool {
        // SAFETY: the bitmap storage is permanently reserved, and the lock
        // serializes every access to it.
        unsafe { (*(base as *const u8).add(frame / 8) & (1 << (frame % 8))) != 0 }
    }

    fn set_bit(&self, base: usize, frame: usize, value: bool) {
        // SAFETY: as above; frame is always bounded by self.frames.
        unsafe {
            let byte = (base as *mut u8).add(frame / 8);
            let mask = 1 << (frame % 8);
            let old = byte.read();
            byte.write(if value { old | mask } else { old & !mask });
        }
    }

    fn free_frame(&self, frame: usize) -> bool {
        self.bit(self.usable_addr, frame) && !self.bit(self.used_addr, frame)
    }

    fn refs(&self) -> *mut u16 {
        self.refs_addr as *mut u16
    }

    fn ref_count(&self, frame: usize) -> u16 {
        // SAFETY: refs_addr covers all `frames` entries and the lock serializes.
        unsafe { self.refs().add(frame).read() }
    }

    fn set_ref_count(&self, frame: usize, value: u16) {
        // SAFETY: as above; frame is always bounded by self.frames.
        unsafe { self.refs().add(frame).write(value) }
    }
}

pub struct Stats {
    pub frames: usize,
    pub free: usize,
}

pub fn init(entries: &[&Entry], hhdm: u64) {
    let mut state = PMM.lock();
    assert_eq!(state.frames, 0, "PMM initialized twice");
    let max_end = entries
        .iter()
        .filter(|e| e.type_ == MEMMAP_USABLE)
        .filter_map(|e| e.base.checked_add(e.length))
        .max()
        .expect("no usable RAM");
    let frames = max_end.div_ceil(PAGE_SIZE) as usize;
    let bitmap_bytes = frames.div_ceil(8);
    // Two bitmaps (used/usable) followed by a page-aligned u16 refcount array.
    let refs_offset = (bitmap_bytes * 2).div_ceil(PAGE_SIZE as usize) * PAGE_SIZE as usize;
    let metadata_bytes = refs_offset + frames * core::mem::size_of::<u16>();
    let metadata_pages = (metadata_bytes as u64).div_ceil(PAGE_SIZE);
    let bitmap_phys = entries
        .iter()
        .filter(|e| e.type_ == MEMMAP_USABLE)
        .find_map(|e| {
            let first = e.base.max(PAGE_SIZE).div_ceil(PAGE_SIZE) * PAGE_SIZE;
            let end = e.base.checked_add(e.length)?;
            (first.checked_add(metadata_pages * PAGE_SIZE)? <= end).then_some(first)
        })
        .expect("no room for PMM metadata");
    let bitmap_virt = hhdm.checked_add(bitmap_phys).expect("HHDM overflow") as usize;
    // SAFETY: the chosen pages are usable and hence present in Limine's HHDM;
    // they are reserved before the allocator is published to other CPUs.
    unsafe {
        core::ptr::write_bytes(bitmap_virt as *mut u8, 0xff, bitmap_bytes);
        core::ptr::write_bytes((bitmap_virt + bitmap_bytes) as *mut u8, 0, bitmap_bytes);
        core::ptr::write_bytes(
            (bitmap_virt + refs_offset) as *mut u8,
            0,
            frames * core::mem::size_of::<u16>(),
        );
    }
    state.used_addr = bitmap_virt;
    state.usable_addr = bitmap_virt + bitmap_bytes;
    state.refs_addr = bitmap_virt + refs_offset;
    state.frames = frames;
    for entry in entries.iter().filter(|e| e.type_ == MEMMAP_USABLE) {
        let start = entry.base.max(PAGE_SIZE).div_ceil(PAGE_SIZE) as usize;
        let end = (entry
            .base
            .checked_add(entry.length)
            .expect("usable RAM span overflow")
            / PAGE_SIZE) as usize;
        for frame in start..end.min(frames) {
            if state.bit(state.usable_addr, frame) {
                continue;
            }
            state.set_bit(state.usable_addr, frame, true);
            state.set_bit(state.used_addr, frame, false);
            state.free += 1;
        }
    }
    for frame in
        (bitmap_phys / PAGE_SIZE) as usize..((bitmap_phys / PAGE_SIZE) + metadata_pages) as usize
    {
        state.set_bit(state.used_addr, frame, true);
        state.set_bit(state.usable_addr, frame, false); // Permanent metadata, never allocatable/freeable.
        state.free -= 1;
    }
    state.hint = (bitmap_phys / PAGE_SIZE + metadata_pages) as usize;
    HHDM.store(hhdm, Ordering::Release);
}

pub fn hhdm() -> u64 {
    HHDM.load(Ordering::Acquire)
}

pub fn phys_to_virt(physical: u64) -> usize {
    hhdm().checked_add(physical).expect("HHDM overflow") as usize
}

pub fn stats() -> Stats {
    let state = PMM.lock();
    Stats {
        frames: state.frames,
        free: state.free,
    }
}

fn span(physical: u64, pages: usize) -> Result<(usize, usize), &'static str> {
    if !physical.is_multiple_of(PAGE_SIZE) || pages == 0 {
        return Err("invalid frame span");
    }
    let first = (physical / PAGE_SIZE) as usize;
    let end = first.checked_add(pages).ok_or("frame span overflow")?;
    Ok((first, end))
}

fn check_allocated(state: &State, first: usize, end: usize) -> Result<(), &'static str> {
    if first == 0 || end > state.frames {
        return Err("frame span out of bounds");
    }
    for frame in first..end {
        if !state.bit(state.usable_addr, frame) || !state.bit(state.used_addr, frame) {
            return Err("frame not allocated");
        }
        if state.ref_count(frame) == 0 {
            return Err("frame refcount zero");
        }
    }
    Ok(())
}

/// Allocation is exclusive: the frame starts with one reference.
pub fn alloc(pages: usize) -> Option<u64> {
    if pages == 0 {
        return None;
    }
    let mut state = PMM.lock();
    if state.frames == 0 || pages > state.free {
        return None;
    }
    for pass in 0..2 {
        let (start, end) = if pass == 0 {
            (state.hint, state.frames)
        } else {
            // Include runs straddling the hint; it is not an allocation boundary.
            (1, state.frames)
        };
        let mut run = 0;
        for frame in start..end {
            if state.free_frame(frame) {
                run += 1;
            } else {
                run = 0;
            }
            if run == pages {
                let first = frame + 1 - pages;
                for index in first..=frame {
                    state.set_bit(state.used_addr, index, true);
                    debug_assert_eq!(state.ref_count(index), 0);
                    state.set_ref_count(index, 1);
                }
                state.free -= pages;
                state.hint = (frame + 1).min(state.frames);
                return Some(first as u64 * PAGE_SIZE);
            }
        }
    }
    None
}

/// Allocate `pages` physically contiguous frames whose end stays below
/// `max_end` (exclusive).  Used by ISA DMA, which is limited to 24-bit
/// addresses.
pub fn alloc_below(pages: usize, max_end: u64) -> Option<u64> {
    if pages == 0 {
        return None;
    }
    let max_frame = (max_end / PAGE_SIZE as u64) as usize;
    let mut state = PMM.lock();
    if pages >= max_frame || pages > state.free {
        return None;
    }
    let end = max_frame.min(state.frames);
    let mut run = 0;
    for frame in 1..end {
        if state.free_frame(frame) {
            run += 1;
            if run == pages {
                let first = frame + 1 - pages;
                for index in first..=frame {
                    state.set_bit(state.used_addr, index, true);
                    debug_assert_eq!(state.ref_count(index), 0);
                    state.set_ref_count(index, 1);
                }
                state.free -= pages;
                state.hint = (frame + 1).min(state.frames);
                return Some(first as u64 * PAGE_SIZE as u64);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Free an exclusively owned frame span. Shared frames must go through
/// `release`; returning an error here prevents a double free from silently
/// dropping another mapping's page.
pub fn free(physical: u64, pages: usize) -> Result<(), &'static str> {
    let (first, end) = span(physical, pages)?;
    let mut state = PMM.lock();
    check_allocated(&state, first, end)?;
    for frame in first..end {
        if state.ref_count(frame) != 1 {
            return Err("frame is shared");
        }
    }
    for frame in first..end {
        state.set_bit(state.used_addr, frame, false);
        state.set_ref_count(frame, 0);
    }
    state.free += end - first;
    state.hint = state.hint.min(first);
    Ok(())
}

/// Add one reference to every frame in a span. Used by CoW fork before the
/// parent/child page tables are published.
pub fn retain(physical: u64, pages: usize) -> Result<(), &'static str> {
    let (first, end) = span(physical, pages)?;
    let state = PMM.lock();
    check_allocated(&state, first, end)?;
    if (first..end).any(|frame| state.ref_count(frame) == u16::MAX) {
        return Err("frame refcount overflow");
    }
    for frame in first..end {
        state.set_ref_count(frame, state.ref_count(frame) + 1);
    }
    Ok(())
}

/// Drop one reference from every frame in a span. Frames reaching zero return
/// to the free pool; shared frames stay allocated.
pub fn release(physical: u64, pages: usize) -> Result<(), &'static str> {
    let (first, end) = span(physical, pages)?;
    let mut state = PMM.lock();
    check_allocated(&state, first, end)?;
    let mut freed = 0;
    for frame in first..end {
        let next = state.ref_count(frame) - 1;
        state.set_ref_count(frame, next);
        if next == 0 {
            state.set_bit(state.used_addr, frame, false);
            freed += 1;
        }
    }
    state.free += freed;
    state.hint = state.hint.min(first);
    Ok(())
}

/// Current reference count for a 4 KiB frame.
pub fn refcount(physical: u64) -> Result<u16, &'static str> {
    let (first, _) = span(physical, 1)?;
    let state = PMM.lock();
    if first == 0 || first >= state.frames {
        return Err("frame out of bounds");
    }
    if !state.bit(state.usable_addr, first) || !state.bit(state.used_addr, first) {
        return Err("frame not allocated");
    }
    Ok(state.ref_count(first))
}
