/*
 *
 *       src/vfs/fd.rs
 *       Open file descriptions, per-process file descriptor tables and pipes
 *
 *       2026/10/23 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Open file descriptions (`OpenFile`), per-task `FileDescriptorTable`, console FDs, and pipes.

use super::{
    abi,
    inode::{DirEntry, Inode, InodeKind, Stat, VfsError},
};
use crate::{
    sync::{Mutex, SpinLock},
    tty::{self, ReadError, TtyDevice},
};
use alloc::{collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

pub const MAX_FDS: usize = 4096;

/// Console character device inode backing `/dev/console` and default FDs 0, 1, 2.
pub struct ConsoleInode {
    nonblock: AtomicBool,
    ino: u64,
    vt: usize,
    rdev: u64,
}

impl ConsoleInode {
    pub fn new() -> Self {
        Self::with_ino(15)
    }

    pub fn with_ino(ino: u64) -> Self {
        Self {
            nonblock: AtomicBool::new(false),
            ino,
            vt: 0,
            rdev: (5 << 8) | 1,
        }
    }

    /// `/dev/ttyN` for `vt` in 1..=8, or `/dev/tty0` for the foreground VT.
    pub fn for_vt(ino: u64, vt: usize) -> Self {
        Self {
            nonblock: AtomicBool::new(false),
            ino,
            vt,
            rdev: (4 << 8) | vt as u64,
        }
    }
}

impl Default for ConsoleInode {
    fn default() -> Self {
        Self::new()
    }
}

/// `/dev/tty`: the opener's controlling terminal, a pty or the console.
pub struct ControllingTty;

impl Inode for ControllingTty {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(0x6FFF_FFF0, InodeKind::CharDevice, 0o666, 0);
        stat.rdev = 5 << 8;
        Ok(stat)
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        if let Some(slave) = crate::tty::pty::open_controlling() {
            return Ok(Some(slave));
        }
        let console = Arc::new(ConsoleInode::new());
        console.open(flags)?;
        Ok(Some(console))
    }
}

impl Inode for ConsoleInode {
    fn console_vt(&self) -> Option<usize> {
        Some(self.vt)
    }

    fn open(&self, _flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        // Linux assigns the controlling terminal when a session leader opens
        // it for reading and no session owns it yet.
        if let Some(task) = crate::sched::current() {
            let stats = task.stats();
            if stats.user
                && stats.sid != 0
                && stats.tgid == stats.sid
                && crate::tty::controlling_session() == 0
                && crate::tty::assign_session(stats.sid)
            {
                crate::tty::set_foreground_pgrp(stats.sid, stats.pgid);
            }
        }
        Ok(None)
    }

    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut s = Stat::simple(self.ino, InodeKind::CharDevice, 0o666, 0);
        s.rdev = self.rdev;
        Ok(s)
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(task) = crate::sched::current() {
            let stats = task.stats();
            let session = tty::controlling_session();
            if stats.user
                && session != 0
                && stats.sid == session
                && stats.pgid != tty::foreground_pgrp()
            {
                // POSIX: a blocked or ignored SIGTTIN turns the read into
                // EIO; an orphaned background group also gets EIO.
                if crate::signal::signal_ignored(&task, crate::signal::SIGTTIN)
                    || crate::signal::pgrp_orphaned(stats.pgid, stats.sid)
                {
                    return Err(VfsError::IoError);
                }
                let _ = crate::signal::send_to_pgrp(stats.pgid, crate::signal::SIGTTIN);
                return Err(VfsError::Interrupted);
            }
        }
        loop {
            tty::poll_serial();
            match tty::read(TtyDevice::Serial0, buf) {
                Ok(n) if n > 0 => return Ok(n),
                _ => match tty::read(TtyDevice::Foreground, buf) {
                    Ok(n) if n > 0 => return Ok(n),
                    Ok(_) | Err(ReadError::WouldBlock) => {
                        if self.nonblock.load(Ordering::Acquire) {
                            return Err(VfsError::WouldBlock);
                        }
                        if crate::signal::pending_for_current() {
                            return Err(VfsError::Interrupted);
                        }
                        crate::sched::sleep_micros(1000);
                    }
                    Err(_) => return Err(VfsError::IoError),
                },
            }
        }
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> Result<usize, VfsError> {
        tty::write_kernel(buf);
        Ok(buf.len())
    }

    fn ioctl(&self, cmd: u64, _arg: u64) -> Result<u64, VfsError> {
        match cmd {
            abi::TCGETS | abi::TIOCGWINSZ => Ok(0),
            _ => Err(VfsError::InvalidArgument),
        }
    }

    fn poll(&self) -> u32 {
        tty::poll_serial();
        let mut events = 0x04; // POLLOUT
        if tty::readable(TtyDevice::Serial0) || tty::readable(TtyDevice::Foreground) {
            events |= 0x01; // POLLIN
        }
        events
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }
}

pub(crate) struct PipeBuffer {
    pub(crate) queue: VecDeque<u8>,
    pub(crate) readers: u32,
    pub(crate) writers: u32,
}

pub(crate) const PIPE_CAPACITY: usize = 64 * 1024;
const PIPE_BUF: usize = 4096;

pub(crate) fn pipe_read(
    shared: &SpinLock<PipeBuffer>,
    nonblock: bool,
    buf: &mut [u8],
) -> Result<usize, VfsError> {
    if buf.is_empty() {
        return Ok(0);
    }
    loop {
        let mut pipe = shared.lock();
        if !pipe.queue.is_empty() {
            let count = buf.len().min(pipe.queue.len());
            for slot in &mut buf[..count] {
                *slot = pipe.queue.pop_front().unwrap();
            }
            drop(pipe);
            crate::ipc::wake_object(shared as *const SpinLock<PipeBuffer>);
            return Ok(count);
        }
        if pipe.writers == 0 {
            return Ok(0);
        }
        drop(pipe);
        if nonblock {
            return Err(VfsError::WouldBlock);
        }
        if crate::signal::pending_for_current() {
            return Err(VfsError::Interrupted);
        }
        crate::sched::sleep_micros(1000);
    }
}

pub(crate) fn pipe_write(
    shared: &SpinLock<PipeBuffer>,
    nonblock: bool,
    buf: &[u8],
) -> Result<usize, VfsError> {
    if buf.is_empty() {
        return Ok(0);
    }
    loop {
        let mut pipe = shared.lock();
        if pipe.readers == 0 {
            drop(pipe);
            crate::signal::raise_sigpipe();
            return Err(VfsError::BrokenPipe);
        }
        let available = PIPE_CAPACITY.saturating_sub(pipe.queue.len());
        let count = if buf.len() <= PIPE_BUF {
            if available >= buf.len() { buf.len() } else { 0 }
        } else {
            available.min(buf.len())
        };
        if count != 0 {
            for &byte in &buf[..count] {
                pipe.queue.push_back(byte);
            }
            drop(pipe);
            crate::ipc::wake_object(shared as *const SpinLock<PipeBuffer>);
            return Ok(count);
        }
        drop(pipe);
        if nonblock {
            return Err(VfsError::WouldBlock);
        }
        if crate::signal::pending_for_current() {
            return Err(VfsError::Interrupted);
        }
        crate::sched::sleep_micros(1000);
    }
}

pub struct PipeEndInode {
    shared: Arc<SpinLock<PipeBuffer>>,
    is_writer: bool,
    nonblock: AtomicBool,
    ino: u64,
}

impl Inode for PipeEndInode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let len = self.shared.lock().queue.len() as u64;
        Ok(Stat::simple(self.ino, InodeKind::Fifo, 0o600, len))
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        if self.is_writer {
            return Err(VfsError::BadFileDescriptor);
        }
        pipe_read(&self.shared, self.nonblock.load(Ordering::Acquire), buf)
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> Result<usize, VfsError> {
        if !self.is_writer {
            return Err(VfsError::BadFileDescriptor);
        }
        pipe_write(&self.shared, self.nonblock.load(Ordering::Acquire), buf)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(Arc::as_ptr(&self.shared));
    }

    fn poll(&self) -> u32 {
        let pipe = self.shared.lock();
        if self.is_writer {
            let mut events = 0;
            if pipe.readers == 0 {
                events |= 0x08; // POLLERR
            } else if pipe.queue.len() < PIPE_CAPACITY {
                events |= 0x04; // POLLOUT
            }
            events
        } else {
            let mut events = 0;
            if !pipe.queue.is_empty() {
                events |= 0x01; // POLLIN
            }
            if pipe.writers == 0 {
                events |= 0x10; // POLLHUP
            }
            events
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }
}

impl Drop for PipeEndInode {
    fn drop(&mut self) {
        let mut pipe = self.shared.lock();
        if self.is_writer {
            pipe.writers = pipe.writers.saturating_sub(1);
        } else {
            pipe.readers = pipe.readers.saturating_sub(1);
        }
        crate::ipc::poll_kick();
    }
}

pub fn create_pipe_pair() -> (Arc<OpenFile>, Arc<OpenFile>) {
    create_pipe_pair_with_flags(0)
}

pub fn create_pipe_pair_with_flags(extra_flags: u32) -> (Arc<OpenFile>, Arc<OpenFile>) {
    let status_flags = extra_flags & abi::O_NONBLOCK;
    let shared = Arc::new(SpinLock::new(PipeBuffer {
        queue: VecDeque::with_capacity(4096),
        readers: 1,
        writers: 1,
    }));
    let r_ino = super::tree::ino();
    let w_ino = super::tree::ino();
    let nonblock = status_flags & abi::O_NONBLOCK != 0;
    let read_inode: Arc<dyn Inode> = Arc::new(PipeEndInode {
        shared: shared.clone(),
        is_writer: false,
        nonblock: AtomicBool::new(nonblock),
        ino: r_ino,
    });
    let write_inode: Arc<dyn Inode> = Arc::new(PipeEndInode {
        shared,
        is_writer: true,
        nonblock: AtomicBool::new(nonblock),
        ino: w_ino,
    });
    let reader = Arc::new(OpenFile::new(
        read_inode,
        alloc::format!("pipe:[{}]", r_ino),
        abi::O_RDONLY | status_flags,
    ));
    let writer = Arc::new(OpenFile::new(
        write_inode,
        alloc::format!("pipe:[{}]", w_ino),
        abi::O_WRONLY | status_flags,
    ));
    (reader, writer)
}

/// Kernel open file description (`struct file` in Linux).
pub struct OpenFile {
    pub inode: Arc<dyn Inode>,
    pub abs_path: String,
    flags: SpinLock<u32>,
    /// Held across file I/O, which may sleep, to serialize the file position.
    offset: Mutex<u64>,
    dir_cursor: SpinLock<usize>,
    /// `flock` key held by this description, released when it is destroyed.
    flock_key: SpinLock<Option<(u64, u64)>>,
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        if let Some(key) = *self.flock_key.lock() {
            crate::syscall::flock_release(key, self as *const Self as u64);
        }
        if self.can_write() {
            crate::ipc::inotify::notify(&self.abs_path, crate::ipc::inotify::IN_CLOSE_WRITE);
        }
    }
}

impl OpenFile {
    pub fn track_flock(&self, key: (u64, u64)) {
        *self.flock_key.lock() = Some(key);
    }

    pub fn new(inode: Arc<dyn Inode>, abs_path: String, flags: u32) -> Self {
        Self {
            inode,
            abs_path,
            flags: SpinLock::new(flags),
            offset: Mutex::new(0),
            dir_cursor: SpinLock::new(0),
            flock_key: SpinLock::new(None),
        }
    }

    pub fn flags(&self) -> u32 {
        *self.flags.lock()
    }

    pub fn set_flags(&self, new_flags: u32) -> Result<(), VfsError> {
        let mut f = self.flags.lock();
        let updated = (*f & abi::O_ACCMODE) | (new_flags & !abi::O_ACCMODE);
        *f = updated;
        self.inode.set_nonblock(updated & abi::O_NONBLOCK != 0)?;
        Ok(())
    }

    pub fn offset(&self) -> u64 {
        *self.offset.lock()
    }

    pub fn can_read(&self) -> bool {
        let mode = self.flags() & abi::O_ACCMODE;
        mode == abi::O_RDONLY || mode == abi::O_RDWR
    }

    pub fn can_write(&self) -> bool {
        let mode = self.flags() & abi::O_ACCMODE;
        mode == abi::O_WRONLY || mode == abi::O_RDWR
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, VfsError> {
        if !self.can_read() {
            return Err(VfsError::BadFileDescriptor);
        }
        let meta = self.inode.metadata()?;
        if meta.kind == InodeKind::Directory {
            return Err(VfsError::IsADirectory);
        }
        if matches!(meta.kind, InodeKind::Regular | InodeKind::BlockDevice) {
            let mut off = self.offset.lock();
            let n = self.inode.read_at(*off, buf)?;
            *off = off.saturating_add(n as u64);
            return Ok(n);
        }
        // Sockets, pipes and character devices may block; their inode offset is
        // unused, so never hold the open-file offset lock across the call.
        let n = self.inode.read_at(0, buf)?;
        Ok(n)
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize, VfsError> {
        if !self.can_write() {
            return Err(VfsError::BadFileDescriptor);
        }
        let meta = self.inode.metadata()?;
        if meta.kind == InodeKind::Directory {
            return Err(VfsError::IsADirectory);
        }
        if matches!(meta.kind, InodeKind::Regular | InodeKind::BlockDevice) {
            let mut off = self.offset.lock();
            if (self.flags() & abi::O_APPEND) != 0 {
                *off = meta.size;
            }
            let n = self.inode.write_at(*off, buf)?;
            *off = off.saturating_add(n as u64);
            return Ok(n);
        }
        // Blocking socket/pipe writes must not hold up other users of the position.
        self.inode.write_at(0, buf)
    }

    pub fn pread(&self, pos: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        if !self.can_read() {
            return Err(VfsError::BadFileDescriptor);
        }
        let meta = self.inode.metadata()?;
        if meta.kind == InodeKind::Directory {
            return Err(VfsError::IsADirectory);
        }
        if matches!(
            meta.kind,
            InodeKind::Fifo | InodeKind::Socket | InodeKind::CharDevice
        ) {
            return Err(VfsError::IllegalSeek);
        }
        self.inode.read_at(pos, buf)
    }

    pub fn pwrite(&self, pos: u64, buf: &[u8]) -> Result<usize, VfsError> {
        if !self.can_write() {
            return Err(VfsError::BadFileDescriptor);
        }
        let meta = self.inode.metadata()?;
        if meta.kind == InodeKind::Directory {
            return Err(VfsError::IsADirectory);
        }
        if matches!(
            meta.kind,
            InodeKind::Fifo | InodeKind::Socket | InodeKind::CharDevice
        ) {
            return Err(VfsError::IllegalSeek);
        }
        self.inode.write_at(pos, buf)
    }

    pub fn seek(&self, offset: i64, whence: u32) -> Result<u64, VfsError> {
        let meta = self.inode.metadata()?;
        if matches!(
            meta.kind,
            InodeKind::Fifo | InodeKind::Socket | InodeKind::CharDevice
        ) {
            return Err(VfsError::IllegalSeek);
        }
        let mut cur = self.offset.lock();
        let base: i64 = match whence {
            abi::SEEK_SET => 0,
            abi::SEEK_CUR => *cur as i64,
            abi::SEEK_END => meta.size as i64,
            _ => return Err(VfsError::InvalidArgument),
        };
        let target = base.checked_add(offset).ok_or(VfsError::InvalidArgument)?;
        if target < 0 {
            return Err(VfsError::InvalidArgument);
        }
        *cur = target as u64;
        if meta.kind == InodeKind::Directory && target == 0 {
            *self.dir_cursor.lock() = 0;
        }
        Ok(*cur)
    }

    pub fn getdents64(&self, out_buf: &mut [u8]) -> Result<usize, VfsError> {
        let entries: Vec<DirEntry> = super::readdir_with_mounts(&self.abs_path, &self.inode)?;
        let mut cursor = self.dir_cursor.lock();
        if *cursor >= entries.len() {
            return Ok(0);
        }
        let (written, consumed) = abi::encode_dirents64(&entries, *cursor, out_buf);
        if written == 0 && *cursor < entries.len() {
            return Err(VfsError::InvalidArgument);
        }
        *cursor += consumed;
        Ok(written)
    }
}

#[derive(Clone)]
pub struct FdEntry {
    pub file: Arc<OpenFile>,
    pub cloexec: bool,
}

/// Per-process file descriptor table (`struct files_struct`).  The working
/// directory lives in the task's separate `FsContext` (`struct fs_struct`).
#[derive(Clone)]
pub struct FileDescriptorTable {
    entries: Vec<Option<FdEntry>>,
}

impl Default for FileDescriptorTable {
    fn default() -> Self {
        Self::new_with_stdio()
    }
}

impl FileDescriptorTable {
    pub fn new_with_stdio() -> Self {
        let console: Arc<dyn Inode> = Arc::new(ConsoleInode::new());
        let stdin = Arc::new(OpenFile::new(
            console.clone(),
            String::from("/dev/console"),
            abi::O_RDONLY,
        ));
        let stdout = Arc::new(OpenFile::new(
            console.clone(),
            String::from("/dev/console"),
            abi::O_WRONLY,
        ));
        let stderr = Arc::new(OpenFile::new(
            console,
            String::from("/dev/console"),
            abi::O_WRONLY,
        ));

        let mut entries = Vec::with_capacity(16);
        entries.push(Some(FdEntry {
            file: stdin,
            cloexec: false,
        }));
        entries.push(Some(FdEntry {
            file: stdout,
            cloexec: false,
        }));
        entries.push(Some(FdEntry {
            file: stderr,
            cloexec: false,
        }));

        Self { entries }
    }

    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn snapshot(&self) -> Vec<(usize, FdEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.clone().map(|e| (i, e)))
            .collect()
    }
    pub fn close_on_exec(&mut self) {
        for slot in &mut self.entries {
            if slot.as_ref().is_some_and(|e| e.cloexec) {
                *slot = None;
            }
        }
    }
    pub fn close_all(&mut self) {
        self.entries.clear();
    }

    pub fn get(&self, fd: usize) -> Option<FdEntry> {
        self.entries.get(fd).and_then(|slot| slot.clone())
    }

    pub fn alloc_fd(&mut self, min_fd: usize, entry: FdEntry) -> Result<usize, VfsError> {
        if min_fd >= MAX_FDS {
            return Err(VfsError::InvalidArgument);
        }
        for (idx, slot) in self.entries.iter_mut().enumerate().skip(min_fd) {
            if slot.is_none() {
                *slot = Some(entry);
                return Ok(idx);
            }
        }
        while self.entries.len() < min_fd {
            self.entries.push(None);
        }
        if self.entries.len() >= MAX_FDS {
            return Err(VfsError::TooManyOpenFiles);
        }
        let fd = self.entries.len();
        self.entries.push(Some(entry));
        Ok(fd)
    }

    pub fn insert_at(&mut self, fd: usize, entry: FdEntry) -> Result<usize, VfsError> {
        if fd >= MAX_FDS {
            return Err(VfsError::BadFileDescriptor);
        }
        while self.entries.len() <= fd {
            self.entries.push(None);
        }
        self.entries[fd] = Some(entry);
        Ok(fd)
    }

    pub fn set_cloexec(&mut self, fd: usize, cloexec: bool) -> Result<(), VfsError> {
        let slot = self
            .entries
            .get_mut(fd)
            .and_then(|s| s.as_mut())
            .ok_or(VfsError::BadFileDescriptor)?;
        slot.cloexec = cloexec;
        Ok(())
    }

    pub fn close(&mut self, fd: usize) -> Result<(), VfsError> {
        let slot = self
            .entries
            .get_mut(fd)
            .ok_or(VfsError::BadFileDescriptor)?;
        if slot.take().is_none() {
            return Err(VfsError::BadFileDescriptor);
        }
        Ok(())
    }
}
