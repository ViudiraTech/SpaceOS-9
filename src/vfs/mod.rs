/*
 *
 *       src/vfs/mod.rs
 *       Linux ABI Virtual File System (VFS) core, mount table and path resolution
 *
 *       2026/10/23 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Industrial-grade VFS subsystem providing mount management, symlink-aware path
//! resolution, per-task file descriptor tables, RamFS (`/`), DevFS (`/dev`),
//! ProcFS (`/proc`), and FAT32 (`/mnt`).

pub mod abi;
pub mod dcache;
pub mod devfs;
pub mod devtmpfs;
pub mod ext4;
pub mod fat32;
pub mod fd;
pub mod fifo;
pub mod inode;
pub mod path;
pub mod procfs;
#[cfg(feature = "boot-self-test")]
pub mod pseudo_test;
pub mod ramfs;
pub mod rootfs;
pub mod sysfs;
pub mod tree;

pub use fd::{
    FdEntry, FileDescriptorTable, OpenFile, create_pipe_pair, create_pipe_pair_with_flags,
};
pub use fifo::FifoInode;
pub use inode::{CacheMode, DirEntry, FileSystem, Inode, InodeKind, Stat, StatFs, VfsError};
pub use path::{normalize_path, split_parent_name};

use crate::{block, kinfo, kwarn, sched, sync::SpinLock};
use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

const MAX_SYMLINK_DEPTH: usize = 40;

/// `struct fs_struct`: per-process working directory, root and umask.  Threads
/// created without CLONE_FS get a copy, with it share this object.
pub struct FsContext {
    pub cwd: String,
    pub root: String,
    pub umask: u32,
}

impl FsContext {
    pub fn new() -> Self {
        Self {
            cwd: String::from("/"),
            root: String::from("/"),
            umask: 0o022,
        }
    }
}

impl Default for FsContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FsContext {
    fn clone(&self) -> Self {
        Self {
            cwd: self.cwd.clone(),
            root: self.root.clone(),
            umask: self.umask,
        }
    }
}

static KERNEL_FS: SpinLock<Option<Arc<SpinLock<FsContext>>>> = SpinLock::new(None);

/// Kernel fallback context for early boot and kernel threads.
pub fn kernel_fs() -> Arc<SpinLock<FsContext>> {
    let mut guard = KERNEL_FS.lock();
    if let Some(fs) = guard.as_ref() {
        return fs.clone();
    }
    let fs = Arc::new(SpinLock::new(FsContext::new()));
    *guard = Some(fs.clone());
    fs
}

pub fn current_fs() -> Arc<SpinLock<FsContext>> {
    sched::current_fs().unwrap_or_else(kernel_fs)
}

pub fn current_cwd() -> String {
    current_fs().lock().cwd.clone()
}

pub fn set_current_cwd(path: String) {
    current_fs().lock().cwd = path;
}

pub fn current_umask() -> u32 {
    current_fs().lock().umask
}

#[derive(Clone)]
pub struct MountEntry {
    pub id: u64,
    pub device: String,
    pub mount_point: String,
    pub fs_type: &'static str,
    pub fs: Arc<dyn FileSystem>,
    /// Linux `MS_*` flags supplied to mount(2).
    pub flags: u64,
}

static MOUNTS: SpinLock<Vec<MountEntry>> = SpinLock::new(Vec::new());
static NEXT_MOUNT_ID: AtomicU64 = AtomicU64::new(1);
static KERNEL_FD_TABLE: SpinLock<Option<Arc<SpinLock<FileDescriptorTable>>>> = SpinLock::new(None);

/// Return the current task's file descriptor table, or the kernel fallback table.
pub fn current_fd_table() -> Arc<SpinLock<FileDescriptorTable>> {
    if let Some(tbl) = sched::current_fd_table() {
        return tbl;
    }
    let mut guard = KERNEL_FD_TABLE.lock();
    if let Some(tbl) = guard.as_ref() {
        return tbl.clone();
    }
    let tbl = Arc::new(SpinLock::new(FileDescriptorTable::new_with_stdio()));
    *guard = Some(tbl.clone());
    tbl
}

/// Return a snapshot of all active filesystem mounts.
pub fn list_mounts() -> Vec<MountEntry> {
    MOUNTS.lock().clone()
}

/// Mount a filesystem instance at `mount_point` with no special flags.
pub fn mount_fs(device: &str, mount_point: &str, fs: Arc<dyn FileSystem>) -> Result<(), VfsError> {
    mount_fs_flags(device, mount_point, fs, 0)
}

/// Mount a filesystem instance at `mount_point` with Linux `MS_*` flags.
pub fn mount_fs_flags(
    device: &str,
    mount_point: &str,
    fs: Arc<dyn FileSystem>,
    flags: u64,
) -> Result<(), VfsError> {
    let mp = normalize_path("/", mount_point);
    let mut mounts = MOUNTS.lock();
    if let Some(existing) = mounts.iter_mut().find(|m| m.mount_point == mp) {
        dcache::invalidate_mount(existing.id);
        existing.device = String::from(device);
        existing.fs_type = fs.fs_type();
        existing.fs = fs;
        existing.flags = flags;
        return Ok(());
    }
    mounts.push(MountEntry {
        id: NEXT_MOUNT_ID.fetch_add(1, Ordering::Relaxed),
        device: String::from(device),
        mount_point: mp,
        fs_type: fs.fs_type(),
        fs,
        flags,
    });
    // Sort by descending mount_point length so longest prefix always matches first
    mounts.sort_by(|a, b| b.mount_point.len().cmp(&a.mount_point.len()));
    Ok(())
}

/// Mount by filesystem type name (`ramfs`, `devfs`, `proc`, `vfat` / `fat32`).
pub fn mount_by_type(source: &str, target: &str, fstype: &str) -> Result<(), VfsError> {
    mount_by_type_flags(source, target, fstype, 0)
}

pub fn mount_by_type_flags(
    source: &str,
    target: &str,
    fstype: &str,
    flags: u64,
) -> Result<(), VfsError> {
    let mp = normalize_path("/", target);
    if mp != "/" {
        let _ = mkdir_abs(&mp, 0o755);
    }
    let fs: Arc<dyn FileSystem> = match fstype {
        "ramfs" | "tmpfs" => Arc::new(ramfs::RamFs::new()),
        "devfs" | "devtmpfs" => Arc::new(devtmpfs::DevTmpFs),
        "sysfs" => Arc::new(sysfs::SysFs),
        "devpts" => Arc::new(crate::tty::pty::DevPts),
        "proc" | "procfs" => Arc::new(procfs::ProcFs::new()),
        "ext4" | "ext3" | "ext2" => {
            let dev_name = source.strip_prefix("/dev/").unwrap_or(source);
            let (_, dev) = block::find_device(dev_name).ok_or(VfsError::NotFound)?;
            ext4::Ext4Fs::mount(dev)?
        }
        "vfat" | "fat32" | "msdos" => {
            let dev_name = source.strip_prefix("/dev/").unwrap_or(source);
            let (dev_id, dev) = block::find_device(dev_name).ok_or(VfsError::NotFound)?;
            fat32::Fat32Fs::mount(dev_id, dev)?
        }
        _ => return Err(VfsError::NotSupported),
    };
    mount_fs_flags(source, &mp, fs, flags)
}

/// Unmount a filesystem mounted at `target`.
pub fn umount(target: &str) -> Result<(), VfsError> {
    let mp = normalize_path("/", target);
    if mp == "/" {
        return Err(VfsError::Busy);
    }
    let mut mounts = MOUNTS.lock();
    let idx = mounts
        .iter()
        .position(|m| m.mount_point == mp)
        .ok_or(VfsError::InvalidArgument)?;
    let removed = mounts.remove(idx);
    drop(mounts);
    dcache::invalidate_mount(removed.id);
    removed.fs.sync()
}

/// Find the best-matching mount entry for `abs_path` and return `(MountEntry, relative_subpath)`.
pub fn find_mount(abs_path: &str) -> Result<(MountEntry, String), VfsError> {
    let mounts = MOUNTS.lock();
    for m in mounts.iter() {
        if m.mount_point == "/" {
            let rel = abs_path.strip_prefix('/').unwrap_or(abs_path);
            return Ok((m.clone(), String::from(rel)));
        }
        if abs_path == m.mount_point {
            return Ok((m.clone(), String::new()));
        }
        if let Some(rest) = abs_path.strip_prefix(m.mount_point.as_str())
            && let Some(rel) = rest.strip_prefix('/')
        {
            return Ok((m.clone(), String::from(rel)));
        }
    }
    Err(VfsError::NotFound)
}

/// Resolve `dirfd` and user `path` into a normalized absolute path.
pub fn resolve_at_path(
    table: &FileDescriptorTable,
    dirfd: i32,
    user_path: &str,
) -> Result<String, VfsError> {
    if user_path.is_empty() {
        return Err(VfsError::NotFound);
    }
    if user_path.starts_with('/') {
        return Ok(normalize_path("/", user_path));
    }
    if dirfd == abi::AT_FDCWD {
        return Ok(normalize_path(&current_cwd(), user_path));
    }
    if dirfd < 0 {
        return Err(VfsError::BadFileDescriptor);
    }
    let entry = table
        .get(dirfd as usize)
        .ok_or(VfsError::BadFileDescriptor)?;
    let meta = entry.file.inode.metadata()?;
    if meta.kind != InodeKind::Directory {
        return Err(VfsError::NotADirectory);
    }
    Ok(normalize_path(&entry.file.abs_path, user_path))
}

/// Look up an inode by normalized absolute path, crossing mount points and resolving symlinks.
pub fn lookup_abs(
    abs_path: &str,
    follow_final_symlink: bool,
) -> Result<(Arc<dyn Inode>, String), VfsError> {
    lookup_abs_depth(abs_path, follow_final_symlink, 0)
}

fn lookup_abs_depth(
    abs_path: &str,
    follow_final_symlink: bool,
    depth: usize,
) -> Result<(Arc<dyn Inode>, String), VfsError> {
    if depth > MAX_SYMLINK_DEPTH {
        return Err(VfsError::TooManySymlinks);
    }
    let norm = normalize_path("/", abs_path);
    let (mount, rel) = find_mount(&norm)?;
    if rel.is_empty() {
        return Ok((mount.fs.root_inode(), norm));
    }

    let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let mount_id = mount.id;
    let cacheable = mount.fs.cache_mode() == CacheMode::Stable;
    let mut cur_inode = mount.fs.root_inode();
    let mut cur_ino = cur_inode.metadata()?.ino;
    let mut cur_path = if mount.mount_point == "/" {
        String::new()
    } else {
        mount.mount_point.clone()
    };

    for (idx, &seg) in parts.iter().enumerate() {
        let is_last = idx + 1 == parts.len();
        cur_path.push('/');
        cur_path.push_str(seg);

        let (next_inode, meta) = if cacheable {
            let key = dcache::DentryKey::new(mount_id, cur_ino, seg);
            match dcache::lookup(&key) {
                dcache::Lookup::Positive(inode) => {
                    let meta = inode.metadata()?;
                    (inode, meta)
                }
                dcache::Lookup::Negative => return Err(VfsError::NotFound),
                dcache::Lookup::Miss => {
                    let fresh = match cur_inode.lookup(seg) {
                        Ok(inode) => inode,
                        Err(VfsError::NotFound) => {
                            dcache::insert_negative(key, &cur_path);
                            return Err(VfsError::NotFound);
                        }
                        Err(error) => return Err(error),
                    };
                    let fresh_meta = fresh.metadata()?;
                    let inode =
                        if let Some(cached) = dcache::icache_lookup(mount_id, fresh_meta.ino) {
                            cached
                        } else {
                            dcache::icache_insert(mount_id, fresh_meta.ino, &fresh);
                            fresh
                        };
                    dcache::insert_positive(key, inode.clone(), &cur_path);
                    (inode, fresh_meta)
                }
            }
        } else {
            // Procfs/sysfs/devtmpfs generate entries from live kernel state;
            // never serve a stale namespace object from the cache.
            let fresh = cur_inode.lookup(seg)?;
            let meta = fresh.metadata()?;
            (fresh, meta)
        };
        if meta.kind == InodeKind::Symlink && (!is_last || follow_final_symlink) {
            let target = next_inode.readlink()?;
            let parent_dir = split_parent_name(&cur_path)
                .map(|(p, _)| p)
                .unwrap_or_else(|| String::from("/"));
            let mut resolved = normalize_path(&parent_dir, &target);
            if !is_last {
                for &rem in &parts[idx + 1..] {
                    if !resolved.ends_with('/') {
                        resolved.push('/');
                    }
                    resolved.push_str(rem);
                }
            }
            return lookup_abs_depth(&resolved, follow_final_symlink, depth + 1);
        }
        cur_inode = next_inode;
        cur_ino = meta.ino;
    }

    Ok((cur_inode, norm))
}

/// Read directory entries and merge any child mount points directly under `abs_path`.
pub fn readdir_with_mounts(
    abs_path: &str,
    inode: &Arc<dyn Inode>,
) -> Result<Vec<DirEntry>, VfsError> {
    let mut entries = inode.readdir()?;
    let norm = normalize_path("/", abs_path);
    let prefix = if norm == "/" {
        String::from("/")
    } else {
        format!("{}/", norm)
    };

    for m in list_mounts() {
        if m.mount_point == "/" || m.mount_point == norm {
            continue;
        }
        if let Some(rest) = m.mount_point.strip_prefix(prefix.as_str())
            && !rest.is_empty()
            && !rest.contains('/')
            && !entries.iter().any(|e| e.name == rest)
        {
            entries.push(DirEntry {
                ino: 9999,
                name: String::from(rest),
                kind: InodeKind::Directory,
            });
        }
    }

    Ok(entries)
}

/// Publish a newly created inode into the dentry and inode caches.
fn publish_created(mount: u64, parent_ino: u64, name: &str, inode: &Arc<dyn Inode>, path: &str) {
    let Ok(meta) = inode.metadata() else {
        return;
    };
    // A directory entry can reuse an inode number after deletion; never let
    // the previous object win the identity lookup for the new file.
    dcache::icache_remove(mount, meta.ino);
    dcache::icache_insert(mount, meta.ino, inode);
    dcache::insert_positive(
        dcache::DentryKey::new(mount, parent_ino, name),
        inode.clone(),
        path,
    );
}

/// Open a file by absolute path and `O_*` flags.
pub fn open_abs(abs_path: &str, flags: u32, mode: u32) -> Result<Arc<OpenFile>, VfsError> {
    let norm = normalize_path("/", abs_path);
    let create = (flags & abi::O_CREAT) != 0;
    let excl = (flags & abi::O_EXCL) != 0;
    let trunc = (flags & abi::O_TRUNC) != 0;
    let must_dir = (flags & (abi::O_DIRECTORY_GENERIC | abi::O_DIRECTORY_ARM64)) != 0;

    let (inode, final_path, newly_created) = match lookup_abs(&norm, !excl) {
        Ok((ino, p)) => {
            if create && excl {
                return Err(VfsError::AlreadyExists);
            }
            (ino, p, false)
        }
        Err(VfsError::NotFound) if create => {
            let (parent_path, filename) =
                split_parent_name(&norm).ok_or(VfsError::InvalidArgument)?;
            let (parent_inode, _) = lookup_abs(&parent_path, true)?;
            let parent_meta = parent_inode.metadata()?;
            let mount_id = find_mount(&parent_path)?.0.id;
            let created = parent_inode.create(
                &filename,
                InodeKind::Regular,
                mode & 0o7777 & !current_umask(),
            )?;
            publish_created(mount_id, parent_meta.ino, &filename, &created, &norm);
            crate::ipc::inotify::notify(&norm, crate::ipc::inotify::IN_CREATE);
            (created, norm, true)
        }
        Err(e) => return Err(e),
    };

    let meta = inode.metadata()?;
    if meta.kind == InodeKind::Socket {
        return Err(VfsError::NoSuchDeviceOrAddress);
    }
    if must_dir && meta.kind != InodeKind::Directory {
        return Err(VfsError::NotADirectory);
    }
    let acc = flags & abi::O_ACCMODE;
    if meta.kind == InodeKind::Directory && (acc == abi::O_WRONLY || acc == abi::O_RDWR || trunc) {
        return Err(VfsError::IsADirectory);
    }
    if !newly_created && meta.kind != InodeKind::Directory && meta.kind != InodeKind::Symlink {
        if (acc == abi::O_RDONLY || acc == abi::O_RDWR) && (meta.mode & 0o444) == 0 {
            return Err(VfsError::PermissionDenied);
        }
        if (acc == abi::O_WRONLY || acc == abi::O_RDWR || trunc) && (meta.mode & 0o222) == 0 {
            return Err(VfsError::PermissionDenied);
        }
    }
    if trunc && meta.kind == InodeKind::Regular && (acc == abi::O_WRONLY || acc == abi::O_RDWR) {
        inode.truncate(0)?;
    }

    let inode = inode.open(flags)?.unwrap_or(inode);
    Ok(Arc::new(OpenFile::new(inode, final_path, flags)))
}

/// Create a persistent filesystem entry for a pathname socket.
pub fn create_socket_abs(abs_path: &str) -> Result<(), VfsError> {
    let norm = normalize_path("/", abs_path);
    let (parent_path, name) = split_parent_name(&norm).ok_or(VfsError::InvalidArgument)?;
    let (parent, _) = lookup_abs(&parent_path, true)?;
    let parent_meta = parent.metadata()?;
    let mount_id = find_mount(&parent_path)?.0.id;
    let created = parent.create(&name, InodeKind::Socket, 0o777 & !current_umask())?;
    publish_created(mount_id, parent_meta.ino, &name, &created, &norm);
    crate::ipc::inotify::notify(&norm, crate::ipc::inotify::IN_CREATE);
    Ok(())
}

/// Create a directory at normalized absolute path `abs_path`.
pub fn mkdir_abs(abs_path: &str, mode: u32) -> Result<Arc<dyn Inode>, VfsError> {
    let norm = normalize_path("/", abs_path);
    if lookup_abs(&norm, false).is_ok() {
        return Err(VfsError::AlreadyExists);
    }
    let (parent_path, name) = split_parent_name(&norm).ok_or(VfsError::AlreadyExists)?;
    let (parent_inode, _) = lookup_abs(&parent_path, true)?;
    let parent_meta = parent_inode.metadata()?;
    let mount_id = find_mount(&parent_path)?.0.id;
    let created = parent_inode.mkdir(&name, mode & 0o7777 & !current_umask())?;
    publish_created(mount_id, parent_meta.ino, &name, &created, &norm);
    crate::ipc::inotify::notify(
        &norm,
        crate::ipc::inotify::IN_CREATE | crate::ipc::inotify::IN_ISDIR,
    );
    Ok(created)
}

/// Remove a file or directory at normalized absolute path `abs_path`.
pub fn unlink_abs(abs_path: &str, remove_dir: bool) -> Result<(), VfsError> {
    let norm = normalize_path("/", abs_path);
    if norm == "/" {
        return Err(VfsError::Busy);
    }
    let (parent_path, name) = split_parent_name(&norm).ok_or(VfsError::InvalidArgument)?;
    let (parent_inode, _) = lookup_abs(&parent_path, true)?;
    let parent_meta = parent_inode.metadata()?;
    let mount_id = find_mount(&parent_path)?.0.id;
    let target_ino = lookup_abs(&norm, false)
        .ok()
        .and_then(|(inode, _)| inode.metadata().ok())
        .map(|meta| meta.ino);
    if remove_dir {
        parent_inode.rmdir(&name)?;
    } else {
        parent_inode.unlink(&name)?;
        crate::ipc::unix::unbind_path(&norm);
    }
    dcache::invalidate(&dcache::DentryKey::new(mount_id, parent_meta.ino, &name));
    if remove_dir {
        dcache::invalidate_path_prefix(mount_id, &norm);
    }
    if let Some(ino) = target_ino {
        dcache::icache_remove(mount_id, ino);
    }
    crate::ipc::inotify::notify(&norm, crate::ipc::inotify::IN_DELETE);
    Ok(())
}

/// Create the hard link `new_abs` to the existing file `old_abs`.
pub fn link_abs(old_abs: &str, new_abs: &str, follow: bool) -> Result<(), VfsError> {
    let (target, _) = lookup_abs(old_abs, follow)?;
    let norm = normalize_path("/", new_abs);
    if lookup_abs(&norm, false).is_ok() {
        return Err(VfsError::AlreadyExists);
    }
    let (parent_path, name) = split_parent_name(&norm).ok_or(VfsError::InvalidArgument)?;
    let (parent_inode, _) = lookup_abs(&parent_path, true)?;
    if find_mount(&norm)?.0.id != find_mount(old_abs)?.0.id {
        return Err(VfsError::CrossDevice);
    }
    let parent_meta = parent_inode.metadata()?;
    let mount_id = find_mount(&parent_path)?.0.id;
    parent_inode.link(&name, &target)?;
    dcache::invalidate(&dcache::DentryKey::new(mount_id, parent_meta.ino, &name));
    crate::ipc::inotify::notify(&norm, crate::ipc::inotify::IN_CREATE);
    Ok(())
}

/// Atomically rename a path, replacing an existing compatible target.
pub fn rename_abs(old_abs: &str, new_abs: &str, no_replace: bool) -> Result<(), VfsError> {
    let old = normalize_path("/", old_abs);
    let new = normalize_path("/", new_abs);
    if old == "/" || new == "/" {
        return Err(VfsError::Busy);
    }
    if old == new {
        return lookup_abs(&old, false).map(|_| ());
    }
    if new.starts_with(&format!("{}/", old)) {
        return Err(VfsError::InvalidArgument);
    }
    let (old_parent, old_name) = split_parent_name(&old).ok_or(VfsError::InvalidArgument)?;
    let (new_parent, new_name) = split_parent_name(&new).ok_or(VfsError::InvalidArgument)?;
    let mount = find_mount(&old)?.0;
    if mount.id != find_mount(&new)?.0.id || list_mounts().iter().any(|m| m.mount_point == old) {
        return Err(VfsError::CrossDevice);
    }
    let (old_dir, _) = lookup_abs(&old_parent, true)?;
    let (new_dir, _) = lookup_abs(&new_parent, true)?;
    let old_dir_ino = old_dir.metadata()?.ino;
    let new_dir_ino = new_dir.metadata()?.ino;
    let moved = lookup_abs(&old, false)?.0;
    let moved_ino = moved.metadata()?.ino;
    let replaced_ino = lookup_abs(&new, false)
        .ok()
        .and_then(|(inode, _)| inode.metadata().ok())
        .map(|meta| meta.ino);
    old_dir.rename(&old_name, &new_dir, &new_name, no_replace)?;
    dcache::invalidate(&dcache::DentryKey::new(mount.id, old_dir_ino, &old_name));
    dcache::invalidate(&dcache::DentryKey::new(mount.id, new_dir_ino, &new_name));
    dcache::invalidate_path_prefix(mount.id, &old);
    dcache::invalidate_path_prefix(mount.id, &new);
    if let Some(ino) = replaced_ino
        && ino != moved_ino
    {
        dcache::icache_remove(mount.id, ino);
    }
    crate::ipc::inotify::notify(&old, crate::ipc::inotify::IN_MOVED_FROM);
    crate::ipc::inotify::notify(&new, crate::ipc::inotify::IN_MOVED_TO);
    Ok(())
}

/// Create a symbolic link at `link_abs` pointing to `target`.
pub fn symlink_abs(target: &str, link_abs: &str) -> Result<Arc<dyn Inode>, VfsError> {
    let norm = normalize_path("/", link_abs);
    if lookup_abs(&norm, false).is_ok() {
        return Err(VfsError::AlreadyExists);
    }
    let (parent_path, name) = split_parent_name(&norm).ok_or(VfsError::InvalidArgument)?;
    let (parent_inode, _) = lookup_abs(&parent_path, true)?;
    let parent_meta = parent_inode.metadata()?;
    let mount_id = find_mount(&parent_path)?.0.id;
    let created = parent_inode.symlink(&name, target)?;
    publish_created(mount_id, parent_meta.ino, &name, &created, &norm);
    crate::ipc::inotify::notify(&norm, crate::ipc::inotify::IN_CREATE);
    Ok(created)
}

/// Read symbolic link target at `abs_path`.
pub fn readlink_abs(abs_path: &str) -> Result<String, VfsError> {
    let (inode, _) = lookup_abs(abs_path, false)?;
    inode.readlink()
}

/// Sync all mounted filesystems to their backing block devices.
pub fn sync_all() {
    for m in list_mounts() {
        let _ = m.fs.sync();
    }
}

/// Convenience helper to write a string/byte slice to an absolute file path.
pub fn write_file(abs_path: &str, data: &[u8]) -> Result<(), VfsError> {
    let f = open_abs(abs_path, abi::O_CREAT | abi::O_WRONLY | abi::O_TRUNC, 0o644)?;
    f.write(data)?;
    f.inode.sync()
}

/// Convenience helper to read an entire file at `abs_path` into a `Vec<u8>`.
pub fn read_file(abs_path: &str) -> Result<Vec<u8>, VfsError> {
    let f = open_abs(abs_path, abi::O_RDONLY, 0)?;
    // Size the buffer up front and read straight into it: growing a Vec in
    // 64 KiB steps reallocates and copies the whole image repeatedly.
    let hint = f
        .inode
        .metadata()
        .map(|meta| meta.size as usize)
        .unwrap_or(0)
        .min(256 * 1024 * 1024);
    let mut out = alloc::vec![0u8; hint];
    let mut filled = 0;
    loop {
        if filled == out.len() {
            // Unknown or understated size (procfs, growing files): keep going.
            out.resize((out.len() * 2).max(64 * 1024), 0);
        }
        // Keep each read at 64 KiB, the size the block/ext4 path has always
        // been driven with; a single multi-megabyte read failed with EIO.
        let end = (filled + 64 * 1024).min(out.len());
        let n = f.read(&mut out[filled..end])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    out.truncate(filled);
    Ok(out)
}

/// Initialize the VFS subsystem, mount `/`, `/dev`, `/proc`, and `/mnt` (FAT32).
fn init_module_root() {
    let disk = rootfs::module_disk().expect("rootfs module disappeared");
    block::register_device(disk);
    let (dev_id, device) = block::find_device("rootfs").expect("rootfs device not registered");
    let filesystem = fat32::Fat32Fs::mount(dev_id, device).expect("FAT32 rootfs mount failed");
    mount_fs("rootfs", "/", filesystem).expect("failed to mount FAT32 rootfs");

    crate::device::init();
    for directory in ["/dev", "/proc", "/sys", "/tmp", "/run"] {
        let _ = mkdir_abs(directory, 0o755);
    }
    let _ = mount_fs("devtmpfs", "/dev", Arc::new(devtmpfs::DevTmpFs));
    let _ = mount_fs("sysfs", "/sys", Arc::new(sysfs::SysFs));
    let _ = mount_fs("proc", "/proc", Arc::new(procfs::ProcFs::new()));
    let _ = mount_fs("tmpfs", "/tmp", Arc::new(ramfs::RamFs::new()));
    let _ = mount_fs("tmpfs", "/run", Arc::new(ramfs::RamFs::new()));
    kinfo!(
        "VFS: FAT32 rootfs module mounted on / ({} filesystems mounted)",
        list_mounts().len()
    );
}

/// Mount the ext4 volume named by `root=` on the kernel command line.
fn init_disk_root() -> bool {
    let Some(source) = crate::boot::cmdline()
        .split_ascii_whitespace()
        .find_map(|word| word.strip_prefix("root="))
    else {
        return false;
    };
    let name = source.strip_prefix("/dev/").unwrap_or(source);
    let Some((_, device)) = block::find_device(name) else {
        kwarn!("VFS: root device {} not found", source);
        return false;
    };
    let filesystem = match ext4::Ext4Fs::mount(device) {
        Ok(filesystem) => filesystem,
        Err(error) => {
            kwarn!("VFS: ext4 mount of {} failed: {:?}", source, error);
            return false;
        }
    };
    ROOT_EXT4.lock().replace(filesystem.clone());
    if mount_fs(source, "/", filesystem).is_err() {
        return false;
    }
    crate::device::init();
    for directory in ["/dev", "/proc", "/sys", "/tmp", "/run"] {
        let _ = mkdir_abs(directory, 0o755);
    }
    let _ = mount_fs("devtmpfs", "/dev", Arc::new(devtmpfs::DevTmpFs));
    let _ = mount_fs("sysfs", "/sys", Arc::new(sysfs::SysFs));
    let _ = mount_fs("proc", "/proc", Arc::new(procfs::ProcFs::new()));
    let _ = mount_fs("tmpfs", "/tmp", Arc::new(ramfs::RamFs::new()));
    let _ = mount_fs("tmpfs", "/run", Arc::new(ramfs::RamFs::new()));
    let _ = mkdir_abs("/dev/pts", 0o755);
    let _ = mount_fs("devpts", "/dev/pts", Arc::new(crate::tty::pty::DevPts));
    let _ = mkdir_abs("/dev/shm", 0o1777);
    let _ = mount_fs("tmpfs", "/dev/shm", Arc::new(ramfs::RamFs::new()));
    kinfo!("VFS: ext4 root {} mounted on /", source);
    true
}

static ROOT_EXT4: SpinLock<Option<Arc<ext4::Ext4Fs>>> = SpinLock::new(None);

/// Start background services that need the scheduler, such as journal commit.
pub fn start_services() {
    if let Some(filesystem) = ROOT_EXT4.lock().clone() {
        filesystem.start_flusher();
    }
}

pub fn init() {
    if init_disk_root() {
        return;
    }
    // When a FAT32 root module is attached, it becomes `/`; the kernel's
    // RamFS remains only for /tmp and /run.
    if rootfs::module_disk().is_some() {
        init_module_root();
        return;
    }

    // 1. Mount RamFS on `/`
    let root_fs = Arc::new(ramfs::RamFs::new());
    mount_fs("ramfs", "/", root_fs).expect("VFS: failed to mount rootfs");

    for dir in [
        "/dev", "/proc", "/sys", "/tmp", "/mnt", "/etc", "/var", "/home",
    ] {
        let _ = mkdir_abs(dir, 0o755);
    }

    // Populate standard /etc configuration files
    let _ = write_file(
        "/etc/hostname",
        format!("{}\n", crate::spk::hostname()).as_bytes(),
    );
    let _ = write_file(
        "/etc/os-release",
        format!(
            "NAME=\"{}\"\nVERSION=\"{}\"\nID={}\nVERSION_ID=\"{}\"\nPRETTY_NAME=\"{}\"\nHOME_URL=\"{}\"\n",
            crate::spk::DISTRO_NAME,
            crate::spk::DISTRO_VERSION,
            crate::spk::DISTRO_ID,
            crate::spk::DISTRO_VERSION,
            crate::spk::DISTRO_PRETTY_NAME,
            crate::spk::DISTRO_HOME_URL
        )
        .as_bytes(),
    );
    let _ = write_file(
        "/etc/motd",
        b"Welcome to SpaceKernel (Linux ABI VFS + IDE/AHCI/NVMe Storage Subsystem)\n",
    );
    let _ = write_file(
        "/etc/fstab",
        b"ramfs   /      ramfs  defaults 0 0\ndevfs   /dev   devfs  defaults 0 0\nproc    /proc  proc   defaults 0 0\n",
    );

    // 2. Mount DevFS on `/dev` and ProcFS on `/proc`
    crate::device::init();
    let _ = mount_fs("devtmpfs", "/dev", Arc::new(devtmpfs::DevTmpFs));
    let _ = mount_fs("sysfs", "/sys", Arc::new(sysfs::SysFs));
    let _ = mount_fs("proc", "/proc", Arc::new(procfs::ProcFs::new()));

    // 3. Mount the preferred FAT32 partition on `/mnt`
    // Prefer hardware NVMe/AHCI/IDE partitions first if available, falling back to `ram0p1`.
    let candidates = ["nvme0n1p1", "sda1", "hda1", "ram0p1"];
    let mut mounted_fat = false;
    for cand in candidates {
        if let Some((dev_id, dev)) = block::find_device(cand)
            && let Ok(fat_fs) = fat32::Fat32Fs::mount(dev_id, dev)
        {
            let dev_path = format!("/dev/{}", cand);
            if mount_fs(&dev_path, "/mnt", fat_fs).is_ok() {
                kinfo!("VFS: mounted FAT32 ({}) on /mnt", dev_path);
                mounted_fat = true;
                break;
            }
        }
    }

    if mounted_fat {
        let _ = write_file(
            "/mnt/README.TXT",
            b"SpaceKernel FAT32 Volume\r\nBacked by unified block cache and storage driver.\r\n",
        );
        let _ = write_file(
            "/mnt/KERNEL.INI",
            b"[spacekernel]\r\nvfs=linux-abi\r\nstorage=ide,ahci,nvme,ramdisk\r\n",
        );
    }

    kinfo!(
        "VFS: initialized ({} filesystems mounted)",
        list_mounts().len()
    );
}

#[cfg(feature = "boot-self-test")]
pub fn run_self_test() {
    // 1. Test RamFS create, write, read, symlink, readlink, unlink
    write_file("/tmp/vfs_test.txt", b"hello-vfs-ramfs").expect("VFS ramfs write failed");
    let data = read_file("/tmp/vfs_test.txt").expect("VFS ramfs read failed");
    assert_eq!(&data, b"hello-vfs-ramfs");

    symlink_abs("/tmp/vfs_test.txt", "/tmp/vfs_link.txt").expect("VFS symlink failed");
    assert_eq!(
        readlink_abs("/tmp/vfs_link.txt").expect("VFS readlink failed"),
        "/tmp/vfs_test.txt"
    );
    let link_data = read_file("/tmp/vfs_link.txt").expect("VFS symlink read failed");
    assert_eq!(&link_data, b"hello-vfs-ramfs");
    unlink_abs("/tmp/vfs_link.txt", false).expect("VFS unlink symlink failed");
    unlink_abs("/tmp/vfs_test.txt", false).expect("VFS unlink file failed");

    // 2. Test DevFS (/dev/zero, /dev/null, /dev/urandom, /dev/ram0)
    let f_zero = open_abs("/dev/zero", abi::O_RDONLY, 0).expect("open /dev/zero failed");
    let mut zbuf = [0xAAu8; 32];
    assert_eq!(f_zero.read(&mut zbuf).unwrap(), 32);
    assert!(zbuf.iter().all(|&b| b == 0));

    let f_null = open_abs("/dev/null", abi::O_RDWR, 0).expect("open /dev/null failed");
    assert_eq!(f_null.write(b"discarded").unwrap(), 9);
    assert_eq!(f_null.read(&mut zbuf).unwrap(), 0);

    let f_blk = open_abs("/dev/ram0", abi::O_RDONLY, 0).expect("open /dev/ram0 failed");
    let mut mbr_sig = [0u8; 2];
    assert_eq!(f_blk.pread(510, &mut mbr_sig).unwrap(), 2);
    assert_eq!(mbr_sig, [0x55, 0xAA]);

    // 3. Test ProcFS (/proc/version, /proc/mounts, /proc/partitions)
    let ver = read_file("/proc/version").expect("read /proc/version failed");
    assert!(ver.starts_with(b"SpaceKernel version "));
    let release = crate::spk::release();
    assert!(
        ver.windows(release.len())
            .any(|window| window == release.as_bytes()),
        "/proc/version lacks UTS release"
    );
    let mounts = read_file("/proc/mounts").expect("read /proc/mounts failed");
    assert!(!mounts.is_empty());

    // 4. Test FAT32 (/mnt) create, LFN support, write, read, mkdir, unlink, rmdir
    if let Err(error) = mkdir_abs("/mnt/testdir", 0o755) {
        panic!("FAT32 mkdir failed: {:?}", error);
    }
    write_file("/mnt/testdir/long_file_name_test.txt", b"FAT32-LFN-OK")
        .expect("FAT32 LFN write failed");
    let fat_read =
        read_file("/mnt/testdir/long_file_name_test.txt").expect("FAT32 LFN read failed");
    assert_eq!(&fat_read, b"FAT32-LFN-OK");
    unlink_abs("/mnt/testdir/long_file_name_test.txt", false).expect("FAT32 unlink failed");
    unlink_abs("/mnt/testdir", true).expect("FAT32 rmdir failed");
    sync_all();

    crate::verbose::log(format_args!(
        "VFS (RamFS/DevFS/ProcFS/FAT32) self-test passed"
    ));
}
