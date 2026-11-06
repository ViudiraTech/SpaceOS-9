/*
 *
 *       src/vfs/abi.rs
 *       Linux 64-bit ABI constants, errno codes and architecture-specific structures
 *
 *       2026/10/23 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux ABI constants, `struct stat`, `struct statfs`, `struct iovec`, and `struct linux_dirent64`.

use super::inode::{DirEntry, InodeKind, Stat, StatFs};

// Linux Errno Constants
pub const EPERM: i64 = 1;
pub const ENOENT: i64 = 2;
pub const ESRCH: i64 = 3;
pub const EINTR: i64 = 4;
pub const EIO: i64 = 5;
pub const ENXIO: i64 = 6;
pub const E2BIG: i64 = 7;
pub const ENOEXEC: i64 = 8;
pub const EBADF: i64 = 9;
pub const EAGAIN: i64 = 11;
pub const ENOMEM: i64 = 12;
pub const EACCES: i64 = 13;
pub const EFAULT: i64 = 14;
pub const EBUSY: i64 = 16;
pub const EEXIST: i64 = 17;
pub const EXDEV: i64 = 18;
pub const ENODEV: i64 = 19;
pub const ENOTDIR: i64 = 20;
pub const EISDIR: i64 = 21;
pub const EINVAL: i64 = 22;
pub const ECHILD: i64 = 10;
pub const ENFILE: i64 = 23;
pub const EMFILE: i64 = 24;
pub const ENOTTY: i64 = 25;
pub const EFBIG: i64 = 27;
pub const ENOSPC: i64 = 28;
pub const ESPIPE: i64 = 29;
pub const EROFS: i64 = 30;
pub const EPIPE: i64 = 32;
pub const ERANGE: i64 = 34;
pub const ENAMETOOLONG: i64 = 36;
pub const ENOSYS: i64 = 38;
pub const ENOTEMPTY: i64 = 39;
pub const ELOOP: i64 = 40;
pub const EPROTO: i64 = 71;
pub const ENOTSOCK: i64 = 88;
pub const EDESTADDRREQ: i64 = 89;
pub const EOPNOTSUPP: i64 = 95;
pub const ENODATA: i64 = 61;
pub const EMSGSIZE: i64 = 90;
pub const EPROTOTYPE: i64 = 91;
pub const ENOPROTOOPT: i64 = 92;
pub const EPROTONOSUPPORT: i64 = 93;
pub const ESOCKTNOSUPPORT: i64 = 94;
pub const EAFNOSUPPORT: i64 = 97;
pub const EADDRINUSE: i64 = 98;
pub const EADDRNOTAVAIL: i64 = 99;
pub const ECONNRESET: i64 = 104;
pub const ENOBUFS: i64 = 105;
pub const EISCONN: i64 = 106;
pub const ENOTCONN: i64 = 107;
pub const ETOOMANYREFS: i64 = 109;
pub const ECONNREFUSED: i64 = 111;
pub const ETIMEDOUT: i64 = 110;

// Open Flags (O_*)
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_ACCMODE: u32 = 3;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_NOCTTY: u32 = 0o400;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY_GENERIC: u32 = 0o200000;
pub const O_DIRECTORY_ARM64: u32 = 0o40000;
pub const O_CLOEXEC: u32 = 0o2000000;

// *at() Flags
pub const AT_FDCWD: i32 = -100;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
pub const AT_EACCESS: u32 = 0x200;
pub const AT_REMOVEDIR: u32 = 0x200;
pub const AT_EMPTY_PATH: u32 = 0x1000;

// Seek Whence
pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

// fcntl Commands
pub const F_DUPFD: u32 = 0;
pub const F_GETFD: u32 = 1;
pub const F_SETFD: u32 = 2;
pub const F_GETFL: u32 = 3;
pub const F_SETFL: u32 = 4;
pub const F_DUPFD_CLOEXEC: u32 = 1030;
pub const F_ADD_SEALS: u32 = 1033;
pub const F_GET_SEALS: u32 = 1034;
pub const FD_CLOEXEC: u32 = 1;

// File Mode S_IFMT Bits
pub const S_IFMT: u32 = 0o170000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFSOCK: u32 = 0o140000;

// Dirent d_type Constants
pub const DT_UNKNOWN: u8 = 0;
pub const DT_FIFO: u8 = 1;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_BLK: u8 = 6;
pub const DT_REG: u8 = 8;
pub const DT_LNK: u8 = 10;
pub const DT_SOCK: u8 = 12;

// Common ioctl Requests
pub const TCGETS: u64 = 0x5401;
pub const TCSETS: u64 = 0x5402;
pub const TCSETSW: u64 = 0x5403;
pub const TCSETSF: u64 = 0x5404;
pub const TIOCGPGRP: u64 = 0x540F;
pub const TIOCSPGRP: u64 = 0x5410;
pub const TIOCGWINSZ: u64 = 0x5413;
pub const TIOCGSID: u64 = 0x5429;
pub const BLKROGET: u64 = 0x125E;
pub const BLKGETSIZE: u64 = 0x1260;
pub const BLKSSZGET: u64 = 0x1268;
pub const BLKGETSIZE64: u64 = 0x8008_1272;

// Filesystem Magic Numbers
pub const RAMFS_MAGIC: u64 = 0x8584_58F6;
pub const DEVFS_MAGIC: u64 = 0x1373;
pub const PROC_SUPER_MAGIC: u64 = 0x9FA0;
pub const MSDOS_SUPER_MAGIC: u64 = 0x4D44;

/// Linux `struct iovec` for `readv` / `writev`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IoVec {
    pub iov_base: u64,
    pub iov_len: u64,
}

/// Linux `struct winsize` for `TIOCGWINSZ`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct WinSize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// glibc/Linux 64-bit `dev_t` encoding.
pub const fn makedev(major: u64, minor: u64) -> u64 {
    ((major & 0xFFF) << 8) | (minor & 0xFF) | ((major & !0xFFF) << 32) | ((minor & !0xFF) << 12)
}

pub const fn major(dev: u64) -> u64 {
    ((dev >> 8) & 0xFFF) | ((dev >> 32) & !0xFFF)
}

pub const fn minor(dev: u64) -> u64 {
    (dev & 0xFF) | ((dev >> 12) & !0xFF)
}

/// Linux `struct stat` on `x86_64` (144 bytes).
#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxStat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub __pad0: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    pub __unused: [i64; 3],
}

/// Linux `struct stat` on `aarch64` and `riscv64` (`asm-generic/stat.h`, 128 bytes).
#[cfg(not(target_arch = "x86_64"))]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxStat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub __pad1: u64,
    pub st_size: i64,
    pub st_blksize: i32,
    pub __pad2: i32,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    pub __unused4: u32,
    pub __unused5: u32,
}

impl From<Stat> for LinuxStat {
    fn from(s: Stat) -> Self {
        let type_bits = match s.kind {
            InodeKind::Regular => S_IFREG,
            InodeKind::Directory => S_IFDIR,
            InodeKind::CharDevice => S_IFCHR,
            InodeKind::BlockDevice => S_IFBLK,
            InodeKind::Symlink => S_IFLNK,
            InodeKind::Fifo => S_IFIFO,
            InodeKind::Socket => S_IFSOCK,
        };
        let mode = type_bits | (s.mode & 0o7777);
        #[cfg(target_arch = "x86_64")]
        {
            Self {
                st_dev: s.dev,
                st_ino: s.ino,
                st_nlink: u64::from(s.nlink.max(1)),
                st_mode: mode,
                st_uid: s.uid,
                st_gid: s.gid,
                __pad0: 0,
                st_rdev: s.rdev,
                st_size: s.size as i64,
                st_blksize: s.blksize as i64,
                st_blocks: s.blocks as i64,
                st_atime: s.atime,
                st_atime_nsec: s.atime_nsec,
                st_mtime: s.mtime,
                st_mtime_nsec: s.mtime_nsec,
                st_ctime: s.ctime,
                st_ctime_nsec: s.ctime_nsec,
                __unused: [0; 3],
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            Self {
                st_dev: s.dev,
                st_ino: s.ino,
                st_mode: mode,
                st_nlink: s.nlink.max(1),
                st_uid: s.uid,
                st_gid: s.gid,
                st_rdev: s.rdev,
                __pad1: 0,
                st_size: s.size as i64,
                st_blksize: s.blksize as i32,
                __pad2: 0,
                st_blocks: s.blocks as i64,
                st_atime: s.atime,
                st_atime_nsec: s.atime_nsec,
                st_mtime: s.mtime,
                st_mtime_nsec: s.mtime_nsec,
                st_ctime: s.ctime,
                st_ctime_nsec: s.ctime_nsec,
                __unused4: 0,
                __unused5: 0,
            }
        }
    }
}

/// Linux 64-bit `struct statfs` (120 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxStatFs {
    pub f_type: i64,
    pub f_bsize: i64,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_fsid: [i32; 2],
    pub f_namelen: i64,
    pub f_frsize: i64,
    pub f_flags: i64,
    pub f_spare: [i64; 4],
}

impl From<StatFs> for LinuxStatFs {
    fn from(s: StatFs) -> Self {
        Self {
            f_type: s.magic as i64,
            f_bsize: s.block_size as i64,
            f_blocks: s.total_blocks,
            f_bfree: s.free_blocks,
            f_bavail: s.free_blocks,
            f_files: s.total_files,
            f_ffree: s.free_files,
            f_fsid: [0, 0],
            f_namelen: s.max_name_len as i64,
            f_frsize: s.block_size as i64,
            f_flags: 0,
            f_spare: [0; 4],
        }
    }
}

/// Encode directory entries into Linux `struct linux_dirent64` records.
/// Returns `(bytes_written, entries_consumed)`.
pub fn encode_dirents64(
    entries: &[DirEntry],
    start_idx: usize,
    out_buf: &mut [u8],
) -> (usize, usize) {
    let mut written = 0usize;
    let mut consumed = 0usize;

    for (idx, entry) in entries.iter().enumerate().skip(start_idx) {
        let name_bytes = entry.name.as_bytes();
        // Fixed header is 19 bytes (u64 d_ino + i64 d_off + u16 d_reclen + u8 d_type),
        // followed by null-terminated d_name, padded to 8-byte alignment.
        let raw_len = 19 + name_bytes.len() + 1;
        let reclen = (raw_len + 7) & !7;
        if written + reclen > out_buf.len() {
            break;
        }
        let d_type = match entry.kind {
            InodeKind::Regular => DT_REG,
            InodeKind::Directory => DT_DIR,
            InodeKind::CharDevice => DT_CHR,
            InodeKind::BlockDevice => DT_BLK,
            InodeKind::Symlink => DT_LNK,
            InodeKind::Fifo => DT_FIFO,
            InodeKind::Socket => DT_SOCK,
        };
        let next_off = (idx + 1) as i64;
        let slot = &mut out_buf[written..written + reclen];
        slot.fill(0);
        slot[0..8].copy_from_slice(&entry.ino.to_le_bytes());
        slot[8..16].copy_from_slice(&next_off.to_le_bytes());
        slot[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        slot[18] = d_type;
        slot[19..19 + name_bytes.len()].copy_from_slice(name_bytes);

        written += reclen;
        consumed += 1;
    }

    (written, consumed)
}
