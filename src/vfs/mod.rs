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

pub fn current_root() -> String {
    current_fs().lock().root.clone()
}

/// Normalize a path against a task's chroot root. Paths already inside the
/// root are accepted as real paths so callers may pass either user-visible or
/// resolved absolute paths.
pub fn normalize_rooted(path: &str, root: &str) -> String {
    let root = normalize_path("/", root);
    if root == "/" {
        return normalize_path("/", path);
    }
    if path_is_under(path, &root) {
        return normalize_path("/", path);
    }
    if let Some(rest) = path.strip_prefix('/') {
        return normalize_path(&root, rest);
    }
    normalize_path(&root, path)
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

/// Linux mount namespace: every task group sees its own mount tree. The
/// initial namespace is shared by kernel threads and the boot process until a
/// task asks for CLONE_NEWNS.
pub struct MountNamespace {
    pub id: u64,
    pub mounts: Vec<MountEntry>,
}

/// A bind mount reuses the source inode as its mount root. The source mount's
/// filesystem supplies statfs/cache policy; no data is copied.
struct BindFs {
    source: Arc<dyn Inode>,
    source_fs: Arc<dyn FileSystem>,
}

impl FileSystem for BindFs {
    fn fs_type(&self) -> &'static str {
        "bind"
    }
    fn root_inode(&self) -> Arc<dyn Inode> {
        self.source.clone()
    }
    fn statfs(&self) -> Result<StatFs, VfsError> {
        self.source_fs.statfs()
    }
    fn sync(&self) -> Result<(), VfsError> {
        self.source_fs.sync()
    }
    fn cache_mode(&self) -> CacheMode {
        self.source_fs.cache_mode()
    }
}

static INIT_MOUNT_NS: SpinLock<Option<Arc<SpinLock<MountNamespace>>>> = SpinLock::new(None);
static NEXT_MOUNT_NS_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_MOUNT_ID: AtomicU64 = AtomicU64::new(1);

pub fn init_mount_namespace() -> Arc<SpinLock<MountNamespace>> {
    let mut guard = INIT_MOUNT_NS.lock();
    if let Some(ns) = guard.as_ref() {
        return ns.clone();
    }
    let ns = Arc::new(SpinLock::new(MountNamespace {
        id: NEXT_MOUNT_NS_ID.fetch_add(1, Ordering::Relaxed),
        mounts: Vec::new(),
    }));
    *guard = Some(ns.clone());
    ns
}

pub fn current_mount_namespace() -> Arc<SpinLock<MountNamespace>> {
    sched::current_mount_namespace().unwrap_or_else(init_mount_namespace)
}

pub fn copy_mount_namespace(
    source: &Arc<SpinLock<MountNamespace>>,
) -> Arc<SpinLock<MountNamespace>> {
    let mounts = source.lock().mounts.clone();
    Arc::new(SpinLock::new(MountNamespace {
        id: NEXT_MOUNT_NS_ID.fetch_add(1, Ordering::Relaxed),
        mounts,
    }))
}
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
    current_mount_namespace().lock().mounts.clone()
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
    let namespace = current_mount_namespace();
    let mut ns = namespace.lock();
    let mounts = &mut ns.mounts;
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
    mount_by_type_flags(source, target, fstype, 0, "")
}

pub fn mount_by_type_flags(
    source: &str,
    target: &str,
    fstype: &str,
    flags: u64,
    data: &str,
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
        "proc" | "procfs" => {
            let hidepid = data
                .split(',')
                .find_map(|option| option.strip_prefix("hidepid="))
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(0)
                .min(2);
            Arc::new(procfs::ProcFs::with_hidepid(hidepid))
        }
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

/// Create a bind mount of `source` at `target` in the current mount
/// namespace. `MS_REC` is accepted but P3 sandbox roots use explicit mounts;
/// child mounts of the source are not duplicated.
pub fn bind_mount(source: &str, target: &str, flags: u64) -> Result<(), VfsError> {
    let root = current_root();
    let source = normalize_rooted(source, &root);
    let target = normalize_rooted(target, &root);
    let (inode, _) = lookup_abs(&source, true)?;
    let meta = inode.metadata()?;
    let (source_mount, _) = find_mount(&source)?;
    let (target_inode, _) = lookup_abs(&target, true)?;
    let target_meta = target_inode.metadata()?;
    if meta.kind == InodeKind::Directory && target_meta.kind != InodeKind::Directory {
        return Err(VfsError::NotADirectory);
    }
    if meta.kind != InodeKind::Directory && target_meta.kind == InodeKind::Directory {
        return Err(VfsError::IsADirectory);
    }
    let fs: Arc<dyn FileSystem> = Arc::new(BindFs {
        source: inode,
        source_fs: source_mount.fs.clone(),
    });
    mount_fs_flags(&source, &target, fs, flags)
}

/// Remount flags on an existing bind mount (`MS_REMOUNT | MS_BIND`).
pub fn remount_bind(target: &str, flags: u64) -> Result<(), VfsError> {
    let root = current_root();
    let target = normalize_rooted(target, &root);
    let namespace = current_mount_namespace();
    let mut ns = namespace.lock();
    let mount = ns
        .mounts
        .iter_mut()
        .find(|mount| mount.mount_point == target)
        .ok_or(VfsError::InvalidArgument)?;
    mount.flags = flags & !abi::MS_BIND;
    Ok(())
}

/// Propagation flags are stored for ABI compatibility. P3 mount namespaces
/// are private trees, so there are no peer groups to propagate to.
pub fn set_mount_propagation(target: &str, flags: u64, recursive: bool) -> Result<(), VfsError> {
    if !recursive && !target.starts_with('/') {
        return Err(VfsError::InvalidArgument);
    }
    let _ = flags;
    Ok(())
}

pub fn path_read_only(path: &str) -> bool {
    let root = current_root();
    let path = normalize_rooted(path, &root);
    find_mount(&path)
        .map(|(mount, _)| mount.flags & abi::MS_RDONLY != 0)
        .unwrap_or(false)
}

/// Basic Linux pivot_root(2): move the process root to an existing mount.
/// The sandbox uses chroot after private bind mounts; pivot_root is kept for
/// ABI completeness and validates the Linux preconditions.
pub fn pivot_root(new_root: &str, put_old: &str) -> Result<(), VfsError> {
    let old_root = current_root();
    let new_root = normalize_rooted(new_root, &old_root);
    let put_old = normalize_rooted(put_old, &old_root);
    let (inode, _) = lookup_abs(&new_root, true)?;
    if inode.metadata()?.kind != InodeKind::Directory {
        return Err(VfsError::NotADirectory);
    }
    let (old_inode, _) = lookup_abs(&put_old, true)?;
    if old_inode.metadata()?.kind != InodeKind::Directory {
        return Err(VfsError::NotADirectory);
    }
    if !path_is_under(&put_old, &new_root) || put_old == new_root {
        return Err(VfsError::InvalidArgument);
    }
    let namespace = current_mount_namespace();
    let mut ns = namespace.lock();
    if !ns.mounts.iter().any(|mount| mount.mount_point == new_root) {
        return Err(VfsError::InvalidArgument);
    }
    let Some(old_mount_id) = ns
        .mounts
        .iter()
        .find(|mount| mount.mount_point == old_root)
        .map(|mount| mount.id)
    else {
        return Err(VfsError::InvalidArgument);
    };
    let old_prefix = if old_root == "/" {
        String::from("/")
    } else {
        format!("{old_root}/")
    };
    for mount in &mut ns.mounts {
        if mount.id == old_mount_id {
            mount.mount_point = put_old.clone();
        } else if path_is_under(&mount.mount_point, &new_root) {
            // Mounts under the new root keep their namespace-visible paths.
        } else if path_is_under(&mount.mount_point, &old_root) {
            let relative = mount
                .mount_point
                .strip_prefix(&old_prefix)
                .unwrap_or(mount.mount_point.trim_start_matches('/'));
            mount.mount_point = normalize_path(&put_old, relative);
        }
    }
    ns.mounts
        .sort_by(|a, b| b.mount_point.len().cmp(&a.mount_point.len()));
    drop(ns);
    let fs = current_fs();
    let mut fs = fs.lock();
    fs.root = new_root.clone();
    fs.cwd = new_root;
    Ok(())
}

/// Unmount a filesystem mounted at `target`./// Unmount a filesystem mounted at `target`.
pub fn umount(target: &str) -> Result<(), VfsError> {
    let mp = normalize_path("/", target);
    if mp == "/" {
        return Err(VfsError::Busy);
    }
    let namespace = current_mount_namespace();
    let mut ns = namespace.lock();
    let idx = ns
        .mounts
        .iter()
        .position(|m| m.mount_point == mp)
        .ok_or(VfsError::InvalidArgument)?;
    let removed = ns.mounts.remove(idx);
    drop(ns);
    dcache::invalidate_mount(removed.id);
    removed.fs.sync()
}

/// Find the best-matching mount entry for `abs_path` and return `(MountEntry, relative_subpath)`.
pub fn find_mount(abs_path: &str) -> Result<(MountEntry, String), VfsError> {
    let namespace = current_mount_namespace();
    let mounts = namespace.lock();
    for m in mounts.mounts.iter() {
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
    let root = current_root();
    if user_path.starts_with('/') {
        return Ok(normalize_rooted(user_path, &root));
    }
    if dirfd == abi::AT_FDCWD {
        let scoped = LookupOptions {
            root: root.clone(),
            anchor: Some(root.clone()),
            in_root: true,
            ..LookupOptions::default()
        };
        return normalize_scoped(&current_cwd(), user_path, &scoped);
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
    let scoped = LookupOptions {
        root: root.clone(),
        anchor: Some(root),
        in_root: true,
        ..LookupOptions::default()
    };
    normalize_scoped(&entry.file.abs_path, user_path, &scoped)
}

/// Path-resolution policy used by `openat(2)`, `openat2(2)` and the security
/// helpers. `anchor` is the directory used as `/` for `RESOLVE_IN_ROOT` and as
/// the containment root for `RESOLVE_BENEATH`.
#[derive(Clone, Debug)]
pub struct LookupOptions {
    pub root: String,
    pub follow_final: bool,
    pub no_symlinks: bool,
    pub no_xdev: bool,
    pub no_magiclinks: bool,
    pub anchor: Option<String>,
    pub in_root: bool,
    pub beneath: bool,
    pub xdev_start: Option<u64>,
    pub allow_final_symlink: bool,
}

impl Default for LookupOptions {
    fn default() -> Self {
        Self {
            root: String::from("/"),
            follow_final: true,
            no_symlinks: false,
            no_xdev: false,
            no_magiclinks: false,
            anchor: None,
            in_root: false,
            beneath: false,
            xdev_start: None,
            allow_final_symlink: false,
        }
    }
}

fn path_is_under(path: &str, root: &str) -> bool {
    if root == "/" {
        return path.starts_with('/');
    }
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Normalize `path` relative to the containing directory `parent`. When
/// `opts.in_root` is set, `opts.anchor` acts as `/` and `..` is clamped there;
/// when `opts.beneath` is set, escaping the anchor is EXDEV (Linux
/// RESOLVE_BENEATH). Normal relative paths may still climb to the real `/`.
pub fn normalize_scoped(
    parent: &str,
    path: &str,
    opts: &LookupOptions,
) -> Result<String, VfsError> {
    if path.is_empty() {
        return Err(VfsError::NotFound);
    }
    if path.starts_with('/') && opts.beneath {
        return Err(VfsError::CrossDevice);
    }
    if path.starts_with('/') && !opts.in_root {
        return Ok(normalize_rooted(path, &opts.root));
    }

    let scoped = opts.in_root || opts.beneath;
    let anchor = opts.anchor.as_deref().unwrap_or("/");
    let chroot = normalize_path("/", &opts.root);
    let clamp = if scoped {
        anchor.split('/').filter(|part| !part.is_empty()).count()
    } else if chroot != "/" {
        chroot.split('/').filter(|part| !part.is_empty()).count()
    } else {
        0
    };
    let base = if path.starts_with('/') && opts.in_root {
        anchor
    } else {
        parent
    };
    let mut stack: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    if stack.len() < clamp {
        return Err(VfsError::CrossDevice);
    }
    let mut pieces = path.split('/');
    if path.starts_with('/') {
        pieces.next();
    }
    for part in pieces {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if stack.len() > clamp {
                stack.pop();
            } else if opts.beneath {
                return Err(VfsError::CrossDevice);
            }
            continue;
        }
        stack.push(part);
    }
    let mut out = String::from("/");
    for (index, segment) in stack.iter().enumerate() {
        if index > 0 {
            out.push('/');
        }
        out.push_str(segment);
    }
    if opts.beneath && !path_is_under(&out, anchor) {
        return Err(VfsError::CrossDevice);
    }
    if !scoped && chroot != "/" && !path_is_under(&out, &chroot) {
        return Err(VfsError::CrossDevice);
    }
    Ok(out)
}

/// Resolve `path` (relative to `anchor`, or absolute for IN_ROOT) under an
/// explicit scope and look it up. Used by openat2(2).
pub fn lookup_at(
    anchor: &str,
    path: &str,
    opts: &LookupOptions,
) -> Result<(Arc<dyn Inode>, String), VfsError> {
    if path.is_empty() {
        return Err(VfsError::NotFound);
    }
    let mut scoped = opts.clone();
    if scoped.root.is_empty() {
        scoped.root = current_root();
    }
    let anchor = normalize_path("/", anchor);
    scoped.anchor = Some(anchor.clone());
    if opts.no_xdev && scoped.xdev_start.is_none() {
        scoped.xdev_start = Some(find_mount(&anchor)?.0.id);
    }
    let real = normalize_scoped(&anchor, path, &scoped)?;
    if let Some(root) = scoped.anchor.as_deref()
        && (scoped.in_root || scoped.beneath)
        && !path_is_under(&real, root)
    {
        return Err(VfsError::CrossDevice);
    }
    lookup_abs_opts(&real, &scoped)
}

/// Look up an inode by normalized absolute path, crossing mount points and resolving symlinks.
pub fn lookup_abs(
    abs_path: &str,
    follow_final_symlink: bool,
) -> Result<(Arc<dyn Inode>, String), VfsError> {
    lookup_abs_opts(
        abs_path,
        &LookupOptions {
            root: current_root(),
            follow_final: follow_final_symlink,
            ..LookupOptions::default()
        },
    )
}

/// Look up an absolute path with explicit resolution policy.
pub fn lookup_abs_opts(
    abs_path: &str,
    opts: &LookupOptions,
) -> Result<(Arc<dyn Inode>, String), VfsError> {
    let norm = normalize_rooted(abs_path, &opts.root);
    let xdev_start = if opts.no_xdev {
        Some(opts.xdev_start.unwrap_or(find_mount(&norm)?.0.id))
    } else {
        None
    };
    lookup_abs_depth(&norm, opts, 0, xdev_start)
}

fn lookup_abs_depth(
    abs_path: &str,
    opts: &LookupOptions,
    depth: usize,
    xdev_start: Option<u64>,
) -> Result<(Arc<dyn Inode>, String), VfsError> {
    if depth > MAX_SYMLINK_DEPTH {
        return Err(VfsError::TooManySymlinks);
    }
    let norm = normalize_rooted(abs_path, &opts.root);
    let (mount, rel) = find_mount(&norm)?;
    if let Some(start) = xdev_start
        && mount.id != start
    {
        return Err(VfsError::CrossDevice);
    }
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
        if meta.kind == InodeKind::Symlink {
            if opts.no_symlinks && !(is_last && !opts.follow_final && opts.allow_final_symlink) {
                return Err(VfsError::TooManySymlinks);
            }
            if opts.no_magiclinks && mount.fs_type == "proc" {
                return Err(VfsError::TooManySymlinks);
            }
            if is_last && !opts.follow_final {
                return Ok((next_inode, norm));
            }
            let target = next_inode.readlink()?;
            let parent_dir = split_parent_name(&cur_path)
                .map(|(p, _)| p)
                .unwrap_or_else(|| String::from("/"));
            let mut resolved = if target.starts_with('/') {
                if opts.beneath {
                    return Err(VfsError::CrossDevice);
                }
                if opts.in_root {
                    normalize_scoped(opts.anchor.as_deref().unwrap_or("/"), &target, opts)?
                } else {
                    normalize_rooted(&target, &opts.root)
                }
            } else {
                normalize_scoped(&parent_dir, &target, opts)?
            };
            if let Some(root) = opts.anchor.as_deref()
                && (opts.in_root || opts.beneath)
                && !path_is_under(&resolved, root)
            {
                return Err(VfsError::CrossDevice);
            }
            for &rem in &parts[idx + 1..] {
                resolved = normalize_scoped(&resolved, rem, opts)?;
            }
            return lookup_abs_depth(&resolved, opts, depth + 1, xdev_start);
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
    open_abs_opts(
        abs_path,
        flags,
        mode,
        &LookupOptions {
            root: current_root(),
            ..LookupOptions::default()
        },
    )
}

/// Open an already-resolved inode. Callers must have applied the path
/// resolution policy before reaching this point.
pub fn open_inode(
    inode: Arc<dyn Inode>,
    final_path: String,
    flags: u32,
) -> Result<Arc<OpenFile>, VfsError> {
    let path_op = (flags & abi::O_PATH) != 0;
    let create = (flags & abi::O_CREAT) != 0;
    let excl = (flags & abi::O_EXCL) != 0;
    let trunc = (flags & abi::O_TRUNC) != 0;
    let must_dir = (flags & (abi::O_DIRECTORY_GENERIC | abi::O_DIRECTORY_ARM64)) != 0;
    let nofollow = (flags & abi::O_NOFOLLOW) != 0;
    if path_op && create {
        return Err(VfsError::InvalidArgument);
    }
    let meta = inode.metadata()?;
    if meta.kind == InodeKind::Symlink {
        if path_op && nofollow {
            return Ok(Arc::new(OpenFile::new(inode, final_path, flags)));
        }
        if nofollow || excl {
            return Err(VfsError::TooManySymlinks);
        }
    }
    if meta.kind == InodeKind::Socket {
        return Err(VfsError::NoSuchDeviceOrAddress);
    }
    if must_dir && meta.kind != InodeKind::Directory {
        return Err(VfsError::NotADirectory);
    }
    let acc = flags & abi::O_ACCMODE;
    if !path_op {
        if meta.kind == InodeKind::Directory
            && (acc == abi::O_WRONLY || acc == abi::O_RDWR || trunc)
        {
            return Err(VfsError::IsADirectory);
        }
        if meta.kind != InodeKind::Directory && meta.kind != InodeKind::Symlink {
            if (acc == abi::O_RDONLY || acc == abi::O_RDWR) && (meta.mode & 0o444) == 0 {
                return Err(VfsError::PermissionDenied);
            }
            if (acc == abi::O_WRONLY || acc == abi::O_RDWR || trunc) && (meta.mode & 0o222) == 0 {
                return Err(VfsError::PermissionDenied);
            }
        }
        if trunc && meta.kind == InodeKind::Regular && (acc == abi::O_WRONLY || acc == abi::O_RDWR)
        {
            inode.truncate(0)?;
        }
    }
    let inode = if path_op {
        inode
    } else {
        inode.open(flags)?.unwrap_or(inode)
    };
    Ok(Arc::new(OpenFile::new(inode, final_path, flags)))
}

fn check_writable(path: &str) -> Result<(), VfsError> {
    if path_read_only(path) {
        Err(VfsError::ReadOnlyFs)
    } else {
        Ok(())
    }
}

pub fn open_abs_opts(
    abs_path: &str,
    flags: u32,
    mode: u32,
    opts: &LookupOptions,
) -> Result<Arc<OpenFile>, VfsError> {
    let mut opts_owned;
    let opts = if opts.root.is_empty() {
        opts_owned = opts.clone();
        opts_owned.root = current_root();
        &opts_owned
    } else {
        opts
    };
    let norm = normalize_rooted(abs_path, &opts.root);
    let create = (flags & abi::O_CREAT) != 0;
    let excl = (flags & abi::O_EXCL) != 0;
    let nofollow = (flags & abi::O_NOFOLLOW) != 0;
    let trunc = (flags & abi::O_TRUNC) != 0;
    let acc = flags & abi::O_ACCMODE;
    if flags & abi::O_PATH == 0 && (create || trunc || acc == abi::O_WRONLY || acc == abi::O_RDWR) {
        check_writable(&norm)?;
    }
    let follow = !excl && !nofollow;
    let mut lookup = opts.clone();
    lookup.follow_final = follow;

    let (inode, final_path, newly_created) = match lookup_abs_opts(&norm, &lookup) {
        Ok((ino, p)) => {
            if create && excl {
                return Err(VfsError::AlreadyExists);
            }
            (ino, p, false)
        }
        Err(VfsError::NotFound) if create => {
            let (parent_path, filename) =
                split_parent_name(&norm).ok_or(VfsError::InvalidArgument)?;
            let parent_lookup = LookupOptions {
                follow_final: true,
                ..lookup.clone()
            };
            let (parent_inode, _) = lookup_abs_opts(&parent_path, &parent_lookup)?;
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
    let _ = newly_created;
    open_inode(inode, final_path, flags)
}

/// Create a persistent filesystem entry for a pathname socket.
pub fn create_socket_abs(abs_path: &str) -> Result<(), VfsError> {
    let norm = normalize_path("/", abs_path);
    check_writable(&norm)?;
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
    check_writable(&norm)?;
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
    check_writable(&norm)?;
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
    check_writable(&norm)?;
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
    check_writable(&old)?;
    check_writable(&new)?;
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
    check_writable(&norm)?;
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
