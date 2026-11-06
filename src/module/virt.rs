/*
 *
 *       src/module/virt.rs
 *       Module virtual-address allocator with a fixed free list
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Module text must live within +/-128 MiB of the kernel image so AArch64
//! CALL26 and RISC-V AUIPC+JALR reach every exported function without
//! veneers.  The allocator keeps a small merge-on-free interval list and
//! never allocates while holding its lock.

use crate::{mm::vmm, sync::SpinLock};

const WINDOW_OFFSET: u64 = 0x0200_0000; // 32 MiB above the kernel image
const WINDOW_SIZE: u64 = 0x0400_0000; // 64 MiB span
const PAGE: u64 = 4096;
const MAX_RANGES: usize = 64;

#[derive(Clone, Copy)]
struct Range {
    start: u64,
    end: u64,
}

struct State {
    initialized: bool,
    ranges: [Range; MAX_RANGES],
    count: usize,
}

static STATE: SpinLock<State> = SpinLock::new(State {
    initialized: false,
    ranges: [Range { start: 0, end: 0 }; MAX_RANGES],
    count: 0,
});

fn init(state: &mut State) -> Result<(), i32> {
    if state.initialized {
        return Ok(());
    }
    let kernel = crate::boot::kernel_virtual();
    if kernel == 0 {
        return Err(-(crate::vfs::abi::ENODEV as i32));
    }
    let start = (kernel + WINDOW_OFFSET) & !(PAGE - 1);
    let end = (kernel + WINDOW_OFFSET + WINDOW_SIZE) & !(PAGE - 1);
    if start >= end {
        return Err(-(crate::vfs::abi::ENOMEM as i32));
    }
    state.ranges[0] = Range { start, end };
    state.count = 1;
    state.initialized = true;
    Ok(())
}

fn insert(state: &mut State, index: usize, range: Range) -> bool {
    if state.count >= MAX_RANGES {
        return false;
    }
    for i in (index..state.count).rev() {
        state.ranges[i + 1] = state.ranges[i];
    }
    state.ranges[index] = range;
    state.count += 1;
    true
}

fn remove(state: &mut State, index: usize) {
    for i in index..state.count - 1 {
        state.ranges[i] = state.ranges[i + 1];
    }
    state.count -= 1;
}

/// Reserve a contiguous virtual range.  Pages already mapped in the kernel
/// page tables are skipped, so a clobbered window degrades to a smaller one
/// instead of corrupting an existing mapping.
pub fn allocate(pages: usize) -> Result<u64, i32> {
    if pages == 0 {
        return Err(-(crate::vfs::abi::EINVAL as i32));
    }
    let size = (pages as u64)
        .checked_mul(PAGE)
        .ok_or(-(crate::vfs::abi::ENOMEM as i32))?;
    let mut state = STATE.lock();
    init(&mut state)?;
    let mut index = 0;
    while index < state.count {
        let range = state.ranges[index];
        let mut candidate = range.start;
        while candidate
            .checked_add(size)
            .is_some_and(|end| end <= range.end)
        {
            let mut offset = 0u64;
            while offset < size && !vmm::is_mapped(candidate + offset) {
                offset += PAGE;
            }
            if offset == size {
                let tail_start = candidate + size;
                let inserted = if candidate == range.start && tail_start == range.end {
                    remove(&mut state, index);
                    true
                } else if candidate == range.start {
                    state.ranges[index] = Range {
                        start: tail_start,
                        end: range.end,
                    };
                    true
                } else if tail_start == range.end {
                    state.ranges[index] = Range {
                        start: range.start,
                        end: candidate,
                    };
                    true
                } else {
                    state.ranges[index] = Range {
                        start: range.start,
                        end: candidate,
                    };
                    insert(
                        &mut state,
                        index + 1,
                        Range {
                            start: tail_start,
                            end: range.end,
                        },
                    )
                };
                if inserted {
                    return Ok(candidate);
                }
                return Err(-(crate::vfs::abi::ENOMEM as i32));
            }
            candidate = (candidate + offset + PAGE) & !(PAGE - 1);
        }
        index += 1;
    }
    Err(-(crate::vfs::abi::ENOMEM as i32))
}

/// Return a range obtained from [`allocate`].  Adjacent ranges are merged.
pub fn deallocate(address: u64, pages: usize) {
    if pages == 0 || !address.is_multiple_of(PAGE) {
        return;
    }
    let Some(size) = (pages as u64).checked_mul(PAGE) else {
        return;
    };
    let Some(end) = address.checked_add(size) else {
        return;
    };
    let mut state = STATE.lock();
    if !state.initialized {
        return;
    }
    let mut index = 0;
    while index < state.count && state.ranges[index].start < address {
        index += 1;
    }
    if !insert(
        &mut state,
        index,
        Range {
            start: address,
            end,
        },
    ) {
        return;
    }
    let mut i = 0;
    while i + 1 < state.count {
        if state.ranges[i].end >= state.ranges[i + 1].start {
            state.ranges[i].end = state.ranges[i].end.max(state.ranges[i + 1].end);
            remove(&mut state, i + 1);
        } else {
            i += 1;
        }
    }
}
