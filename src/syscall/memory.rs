/*
 *       src/syscall/memory.rs
 *       Linux memory syscalls with per-process page ownership
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

use crate::{
    user::{
        address_space::{UserAddressSpace, align_up},
        uaccess,
    },
    vfs::{VfsError, abi},
};
const MAP_SHARED: u32 = 1;
const MAP_PRIVATE: u32 = 2;
const MAP_SHARED_VALIDATE: u32 = 3;
const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_GROWSDOWN: u32 = 0x100;
const MAP_DENYWRITE: u32 = 0x800;
const MAP_EXECUTABLE: u32 = 0x1000;
const MAP_LOCKED: u32 = 0x2000;
const MAP_NORESERVE: u32 = 0x4000;
const MAP_POPULATE: u32 = 0x8000;
const MAP_NONBLOCK: u32 = 0x10000;
const MAP_STACK: u32 = 0x20000;
const MAP_HUGETLB: u32 = 0x40000;
const MAP_FIXED_NOREPLACE: u32 = 0x100000;
const KNOWN_FLAGS: u32 = MAP_SHARED
    | MAP_PRIVATE
    | MAP_FIXED
    | MAP_ANONYMOUS
    | MAP_GROWSDOWN
    | MAP_DENYWRITE
    | MAP_EXECUTABLE
    | MAP_LOCKED
    | MAP_NORESERVE
    | MAP_POPULATE
    | MAP_NONBLOCK
    | MAP_STACK
    | MAP_FIXED_NOREPLACE;

pub fn mmap(hint: u64, length: u64, prot: u32, flags: u32, fd: i32, offset: u64) -> i64 {
    let result = (|| -> Result<u64, VfsError> {
        let length = align_up(length)
            .filter(|&n| n != 0)
            .ok_or(VfsError::InvalidArgument)?;
        let kind = flags & 3;
        if prot & !7 != 0 || offset & 4095 != 0 || kind == 0 {
            return Err(VfsError::InvalidArgument);
        }
        if flags & MAP_HUGETLB != 0 {
            return Err(VfsError::NoMemory);
        }
        if kind == MAP_SHARED_VALIDATE && flags & !(KNOWN_FLAGS | MAP_SHARED_VALIDATE) != 0 {
            return Err(VfsError::MessageNotSupported);
        }
        let shared = kind != MAP_PRIVATE;
        let file = if flags & MAP_ANONYMOUS == 0 {
            let f = super::get_open_file(fd)?.file;
            if !f.can_read() {
                return Err(VfsError::PermissionDenied);
            }
            if shared && prot & 2 != 0 && !f.can_write() {
                return Err(VfsError::PermissionDenied);
            }
            offset
                .checked_add(length)
                .ok_or(VfsError::InvalidArgument)?;
            // A filesystem may sleep here, so ask before taking the space lock.
            if !shared {
                // Device nodes without file contents have nothing to copy.
                let meta = f.inode.metadata()?;
                if !matches!(
                    meta.kind,
                    crate::vfs::InodeKind::Regular
                        | crate::vfs::InodeKind::CharDevice
                        | crate::vfs::InodeKind::BlockDevice
                ) {
                    return Err(VfsError::NoDevice);
                }
            }
            Some(f)
        } else {
            None
        };
        // Shared anonymous memory is an unnamed memfd that fork keeps shared.
        let anonymous_shared = if file.is_none() && shared {
            let (memfd, _) =
                crate::ipc::memfd::MemFd::create(alloc::string::String::from("dev/zero"), 0)?;
            crate::vfs::Inode::truncate(memfd.as_ref(), length)?;
            Some(memfd)
        } else {
            None
        };
        let space = uaccess::current_space()?;
        let mut space = space.lock();
        let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
        let address = if fixed {
            if hint & 4095 != 0 || !UserAddressSpace::valid_range(hint, length as usize) {
                return Err(VfsError::InvalidArgument);
            }
            if flags & MAP_FIXED_NOREPLACE != 0 && space.range_in_use(hint, hint + length) {
                return Err(VfsError::AlreadyExists);
            }
            space.unmap(hint, length)?;
            hint
        } else {
            let hint = align_up(hint).unwrap_or(0);
            space
                .find_free(hint, length)
                .map_err(|_| VfsError::NoMemory)?
        };
        if let Some(memfd) = anonymous_shared {
            crate::vfs::Inode::mmap(memfd.as_ref(), &mut space, address, length, 0, prot)?;
        } else if let Some(file) = file {
            if shared {
                file.inode.mmap(&mut space, address, length, offset, prot)?;
                if !file.can_write() {
                    space.restrict_permissions(address, length, 5)?;
                }
            } else {
                space.map_lazy(
                    address,
                    length,
                    prot,
                    7,
                    Some(file.inode.clone()),
                    offset,
                    false,
                )?;
            }
        } else {
            space.map_lazy(address, length, prot, 7, None, 0, false)?;
        }
        Ok(address)
    })();
    match result {
        Ok(address) => {
            if flags & MAP_POPULATE != 0 {
                let _ = uaccess::prefault(address, length as usize, false);
            }
            address as i64
        }
        Err(e) => e.to_errno(),
    }
}

pub fn munmap(address: u64, length: u64) -> i64 {
    if length == 0 {
        return -abi::EINVAL;
    }
    match uaccess::current_space().and_then(|s| s.lock().unmap(address, length)) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

pub fn mprotect(address: u64, length: u64, prot: u32) -> i64 {
    if length == 0 {
        return 0;
    }
    match uaccess::current_space().and_then(|s| s.lock().protect(address, length, prot)) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

const MREMAP_MAYMOVE: u32 = 1;
const MREMAP_FIXED: u32 = 2;
const MREMAP_DONTUNMAP: u32 = 4;

pub fn mremap(old: u64, old_size: u64, new_size: u64, flags: u32, new_address: u64) -> i64 {
    let result = (|| -> Result<u64, VfsError> {
        if old & 4095 != 0 || flags & !(MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP) != 0 {
            return Err(VfsError::InvalidArgument);
        }
        if flags & (MREMAP_FIXED | MREMAP_DONTUNMAP) != 0 && flags & MREMAP_MAYMOVE == 0 {
            return Err(VfsError::InvalidArgument);
        }
        // userfaultfd-style source preservation has no consumer in this kernel.
        if flags & MREMAP_DONTUNMAP != 0 {
            return Err(VfsError::InvalidArgument);
        }
        let new_size = align_up(new_size)
            .filter(|&n| n != 0)
            .ok_or(VfsError::InvalidArgument)?;
        let old_size = align_up(old_size).ok_or(VfsError::InvalidArgument)?;
        let fixed = (flags & MREMAP_FIXED != 0).then_some(new_address);
        let space = uaccess::current_space()?;
        let mut space = space.lock();
        if old_size == 0 {
            if flags & MREMAP_MAYMOVE == 0 {
                return Err(VfsError::InvalidArgument);
            }
            return space.duplicate_shared(old, new_size, fixed);
        }
        space
            .remap(old, old_size, new_size, flags & MREMAP_MAYMOVE != 0, fixed)
            .map_err(|error| match error {
                VfsError::Fault => VfsError::Fault,
                other => other,
            })
    })();
    match result {
        Ok(address) => address as i64,
        Err(e) => e.to_errno(),
    }
}

const MADV_DONTNEED: u32 = 4;
const MADV_FREE: u32 = 8;

/// Advisory calls succeed as no-ops; `MADV_DONTNEED` has real effect because
/// allocators depend on it to zero and release pages.
pub fn madvise(address: u64, length: u64, advice: u32) -> i64 {
    if address & 4095 != 0 {
        return -abi::EINVAL;
    }
    let Some(length) = align_up(length) else {
        return -abi::EINVAL;
    };
    if length == 0 {
        return 0;
    }
    let space = match uaccess::current_space() {
        Ok(space) => space,
        Err(e) => return e.to_errno(),
    };
    let mut space = space.lock();
    if !space.range_mapped(address, length) {
        return -abi::ENOMEM;
    }
    match advice {
        MADV_DONTNEED | MADV_FREE => match space.discard_private(address, length) {
            Ok(()) => 0,
            Err(e) => e.to_errno(),
        },
        // NORMAL, RANDOM, SEQUENTIAL, WILLNEED, REMOVE-free hints, HUGEPAGE,
        // NOHUGEPAGE, DONTFORK-style hints do not change visible contents.
        0..=3 | 9..=21 => 0,
        _ => -abi::EINVAL,
    }
}

/// Writes through shared mappings are already visible; flush is a no-op for
/// memory-backed files and syncs on-disk ones through fsync.
pub fn msync(address: u64, length: u64, flags: u32) -> i64 {
    if address & 4095 != 0 || flags & !7 != 0 || (flags & 1 != 0 && flags & 4 != 0) {
        return -abi::EINVAL;
    }
    let Some(length) = align_up(length) else {
        return -abi::EINVAL;
    };
    match uaccess::current_space() {
        Ok(space) => {
            if length == 0 || space.lock().range_mapped(address, length) {
                0
            } else {
                -abi::ENOMEM
            }
        }
        Err(e) => e.to_errno(),
    }
}

/// `mincore`: this kernel has no swap, so a page is resident or reserved.
pub fn mincore(address: u64, length: u64, vector: u64) -> i64 {
    if address & 4095 != 0 {
        return -abi::EINVAL;
    }
    let Some(length) = align_up(length) else {
        return -abi::EINVAL;
    };
    let pages = (length / 4096) as usize;
    let mut output = alloc::vec![0u8; pages];
    {
        let space = match uaccess::current_space() {
            Ok(space) => space,
            Err(e) => return e.to_errno(),
        };
        let space = space.lock();
        if !space.range_mapped(address, length) {
            return -abi::ENOMEM;
        }
        for (index, slot) in output.iter_mut().enumerate() {
            *slot = u8::from(space.page_present(address + index as u64 * 4096));
        }
    }
    match uaccess::write(vector, &output) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

/// Memory locking is trivially satisfied without swap; ranges are still checked.
pub fn mlock(address: u64, length: u64) -> i64 {
    let Some(end) = address.checked_add(length) else {
        return -abi::EINVAL;
    };
    let start = address & !4095;
    let Some(span) = align_up(end).map(|end| end - start) else {
        return -abi::EINVAL;
    };
    match uaccess::current_space() {
        Ok(space) => {
            if span == 0 || space.lock().range_mapped(start, span) {
                0
            } else {
                -abi::ENOMEM
            }
        }
        Err(e) => e.to_errno(),
    }
}
