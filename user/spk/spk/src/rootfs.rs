//
//       user/spk/spk/src/rootfs.rs
//       Symlink-aware path resolution and atomic file operations under --root
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{chown, lchown, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{renameat_with, RenameFlags, CWD};
use spk_core::fsutil::{sha256_bytes, sha256_file};
use spk_core::manifest::validate_absolute_path;

use crate::error::{Result, SpkError};

const MAX_SYMLINK_DEPTH: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnDiskKind {
    File,
    Dir,
    Symlink,
}

#[derive(Clone, Debug)]
pub struct OnDisk {
    pub kind: OnDiskKind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub sha256: String,
    pub target: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Fs {
    pub root: PathBuf,
}

impl Fs {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    fn privileged(&self) -> bool {
        (unsafe { libc::geteuid() }) == 0
    }

    /// Resolve every component of an absolute path, following symlinks but
    /// never allowing them to leave the root. The final component is not
    /// followed, so callers can replace a symlink itself.
    pub fn resolve_parent(&self, logical: &str) -> Result<(PathBuf, String)> {
        self.try_resolve_parent(logical)?.ok_or_else(|| {
            SpkError::General(format!("parent directory of '{logical}' does not exist"))
        })
    }

    fn try_resolve_parent(&self, logical: &str) -> Result<Option<(PathBuf, String)>> {
        validate_absolute_path(logical)?;
        let (parent, name) = logical
            .rsplit_once('/')
            .ok_or_else(|| SpkError::General(format!("invalid path '{logical}'")))?;
        let parent = if parent.is_empty() { "/" } else { parent };
        let Some(resolved) = self.try_resolve_dir(parent)? else {
            return Ok(None);
        };
        Ok(Some((resolved, name.to_string())))
    }

    pub fn resolve_dir(&self, logical: &str) -> Result<PathBuf> {
        self.try_resolve_dir(logical)?
            .ok_or_else(|| SpkError::General(format!("{logical} does not exist")))
    }

    fn try_resolve_dir(&self, logical: &str) -> Result<Option<PathBuf>> {
        validate_resolvable(logical)?;
        let mut resolved: Vec<String> = Vec::new();
        let mut pending: Vec<String> = logical
            .split('/')
            .filter(|component| !component.is_empty())
            .map(str::to_string)
            .collect();
        let mut depth = 0usize;
        let mut index = 0usize;
        while index < pending.len() {
            let component = pending[index].clone();
            if component == "." {
                index += 1;
                continue;
            }
            if component == ".." {
                resolved
                    .pop()
                    .ok_or_else(|| SpkError::General(format!("'{logical}' escapes the root")))?;
                index += 1;
                continue;
            }
            let candidate = self.root.join(resolved.join("/")).join(&component);
            let metadata = match fs::symlink_metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            if metadata.file_type().is_symlink() {
                depth += 1;
                if depth > MAX_SYMLINK_DEPTH {
                    return Err(SpkError::General(format!(
                        "too many symlinks resolving '{logical}'"
                    )));
                }
                let target = fs::read_link(&candidate)?;
                let target = target
                    .to_str()
                    .ok_or_else(|| SpkError::General("non-UTF-8 symlink target".into()))?;
                let mut replacement: Vec<String> = target
                    .split('/')
                    .filter(|component| !component.is_empty())
                    .map(str::to_string)
                    .collect();
                if target.starts_with('/') {
                    resolved.clear();
                }
                replacement.extend(pending[index + 1..].iter().cloned());
                pending = replacement;
                index = 0;
                continue;
            }
            if !metadata.is_dir() {
                return Err(SpkError::General(format!(
                    "{} is not a directory",
                    candidate.display()
                )));
            }
            resolved.push(component);
            index += 1;
        }
        Ok(Some(self.root.join(resolved.join("/"))))
    }

    pub fn on_disk(&self, logical: &str) -> Result<Option<OnDisk>> {
        let Some((parent, name)) = self.try_resolve_parent(logical)? else {
            return Ok(None);
        };
        let path = parent.join(&name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let file_type = metadata.file_type();
        let mode = metadata.mode() & 0o7777;
        if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            let target = target.to_string_lossy().into_owned();
            Ok(Some(OnDisk {
                kind: OnDiskKind::Symlink,
                mode,
                uid: metadata.uid(),
                gid: metadata.gid(),
                size: target.len() as u64,
                sha256: hash_bytes(target.as_bytes()),
                target: Some(target),
            }))
        } else if file_type.is_file() {
            Ok(Some(OnDisk {
                kind: OnDiskKind::File,
                mode,
                uid: metadata.uid(),
                gid: metadata.gid(),
                size: metadata.len(),
                sha256: sha256_file(&path)?,
                target: None,
            }))
        } else if file_type.is_dir() {
            Ok(Some(OnDisk {
                kind: OnDiskKind::Dir,
                mode,
                uid: metadata.uid(),
                gid: metadata.gid(),
                size: 0,
                sha256: String::new(),
                target: None,
            }))
        } else {
            Err(SpkError::General(format!(
                "{} is a device, FIFO or socket",
                path.display()
            )))
        }
    }

    pub fn exists(&self, logical: &str) -> Result<bool> {
        Ok(self.on_disk(logical)?.is_some())
    }

    /// True when the path resolves to a directory, following a final symlink.
    pub fn is_dir(&self, logical: &str) -> Result<bool> {
        match self.try_resolve_dir(logical)? {
            Some(path) => Ok(fs::metadata(path)?.is_dir()),
            None => Ok(false),
        }
    }

    /// Create `logical` as a directory, creating missing ancestors. Returns
    /// the absolute logical paths that were newly created, in order.
    pub fn ensure_dir(
        &self,
        logical: &str,
        mode: u32,
        uid: u32,
        gid: u32,
        created: &mut Vec<(String, u32)>,
    ) -> Result<()> {
        validate_absolute_path(logical)?;
        let mut resolved: Vec<String> = Vec::new();
        let mut pending: Vec<String> = logical
            .split('/')
            .filter(|component| !component.is_empty())
            .map(str::to_string)
            .collect();
        let mut depth = 0usize;
        let mut index = 0usize;
        while index < pending.len() {
            let component = pending[index].clone();
            if component == "." {
                index += 1;
                continue;
            }
            if component == ".." {
                resolved
                    .pop()
                    .ok_or_else(|| SpkError::General(format!("'{logical}' escapes the root")))?;
                index += 1;
                continue;
            }
            let candidate = self.root.join(resolved.join("/")).join(&component);
            match fs::symlink_metadata(&candidate) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    depth += 1;
                    if depth > MAX_SYMLINK_DEPTH {
                        return Err(SpkError::General(format!(
                            "too many symlinks resolving '{logical}'"
                        )));
                    }
                    let target = fs::read_link(&candidate)?;
                    let target = target
                        .to_str()
                        .ok_or_else(|| SpkError::General("non-UTF-8 symlink target".into()))?;
                    let mut replacement: Vec<String> = target
                        .split('/')
                        .filter(|component| !component.is_empty())
                        .map(str::to_string)
                        .collect();
                    if target.starts_with('/') {
                        resolved.clear();
                    }
                    replacement.extend(pending[index + 1..].iter().cloned());
                    pending = replacement;
                    index = 0;
                }
                Ok(metadata) if metadata.is_dir() => {
                    resolved.push(component);
                    index += 1;
                }
                Ok(_) => {
                    return Err(SpkError::General(format!(
                        "{} is not a directory",
                        candidate.display()
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let file_mode = if index + 1 == pending.len() {
                        mode
                    } else {
                        0o755
                    };
                    let (file_uid, file_gid) = if index + 1 == pending.len() {
                        (uid, gid)
                    } else {
                        (0, 0)
                    };
                    fs::create_dir(&candidate)?;
                    if self.privileged() {
                        chown(&candidate, Some(file_uid), Some(file_gid))?;
                    }
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))?;
                    if file_mode != 0o700 {
                        fs::set_permissions(&candidate, fs::Permissions::from_mode(file_mode))?;
                    }
                    File::open(&candidate)?.sync_all()?;
                    sync_parent(candidate.parent().unwrap_or(&self.root))?;
                    let mut logical_parts = resolved.clone();
                    logical_parts.push(component.clone());
                    created.push((format!("/{}", logical_parts.join("/")), file_mode));
                    resolved.push(component);
                    index += 1;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub fn write_from(
        &self,
        source: &Path,
        logical: &str,
        mode: u32,
        uid: u32,
        gid: u32,
        txid: &str,
    ) -> Result<()> {
        let (parent, name) = self.resolve_parent(logical)?;
        let destination = parent.join(&name);
        let existed = fs::symlink_metadata(&destination).is_ok();
        let temp = temp_name(&parent, txid, "f");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        let mut input = File::open(source)?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
        }
        output.sync_all()?;
        if self.privileged() {
            chown(&temp, Some(uid), Some(gid))?;
        }
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        place(&temp, &destination, existed)?;
        sync_parent(&parent)?;
        Ok(())
    }

    pub fn write_bytes(
        &self,
        data: &[u8],
        logical: &str,
        mode: u32,
        uid: u32,
        gid: u32,
        txid: &str,
    ) -> Result<()> {
        let (parent, name) = self.resolve_parent(logical)?;
        let destination = parent.join(&name);
        let existed = fs::symlink_metadata(&destination).is_ok();
        let temp = temp_name(&parent, txid, "b");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        output.write_all(data)?;
        output.sync_all()?;
        if self.privileged() {
            chown(&temp, Some(uid), Some(gid))?;
        }
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        place(&temp, &destination, existed)?;
        sync_parent(&parent)?;
        Ok(())
    }

    pub fn write_symlink(
        &self,
        target: &str,
        logical: &str,
        uid: u32,
        gid: u32,
        txid: &str,
    ) -> Result<()> {
        let (parent, name) = self.resolve_parent(logical)?;
        let destination = parent.join(&name);
        let existed = fs::symlink_metadata(&destination).is_ok();
        let temp = temp_name(&parent, txid, "l");
        std::os::unix::fs::symlink(target, &temp)?;
        if self.privileged() {
            lchown(&temp, Some(uid), Some(gid))?;
        }
        place(&temp, &destination, existed)?;
        sync_parent(&parent)?;
        Ok(())
    }

    pub fn set_metadata(
        &self,
        logical: &str,
        mode: u32,
        uid: u32,
        gid: u32,
        symlink: bool,
    ) -> Result<()> {
        let (parent, name) = self.resolve_parent(logical)?;
        let path = parent.join(&name);
        if self.privileged() {
            if symlink {
                lchown(&path, Some(uid), Some(gid))?;
            } else {
                chown(&path, Some(uid), Some(gid))?;
            }
        }
        if !symlink {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o7777))?;
        }
        Ok(())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (from_parent, from_name) = self.resolve_parent(from)?;
        let (to_parent, to_name) = self.resolve_parent(to)?;
        let from_path = from_parent.join(from_name);
        let to_path = to_parent.join(to_name);
        if fs::symlink_metadata(&to_path).is_ok() {
            return Err(SpkError::General(format!(
                "{} already exists",
                to_path.display()
            )));
        }
        renameat_with(CWD, &from_path, CWD, &to_path, RenameFlags::NOREPLACE).map_err(|error| {
            SpkError::General(format!(
                "cannot rename {} to {}: {error}",
                from_path.display(),
                to_path.display()
            ))
        })?;
        sync_parent(&from_parent)?;
        if from_parent != to_parent {
            sync_parent(&to_parent)?;
        }
        Ok(())
    }

    pub fn remove(&self, logical: &str) -> Result<()> {
        let (parent, name) = self.resolve_parent(logical)?;
        let path = parent.join(&name);
        match fs::remove_file(&path) {
            Ok(()) => {
                sync_parent(&parent)?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn remove_dir(&self, logical: &str) -> Result<()> {
        let path = self.resolve_dir(logical)?;
        match fs::remove_dir(&path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    sync_parent(parent)?;
                }
                Ok(())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Copy a file or symlink into the backup tree. Directories are not
    /// backed up. Returns the digest of the backed-up content.
    pub fn backup(
        &self,
        logical: &str,
        backup_path: &Path,
    ) -> Result<Option<(OnDiskKind, String)>> {
        let Some(entry) = self.on_disk(logical)? else {
            return Ok(None);
        };
        if entry.kind == OnDiskKind::Dir {
            return Ok(None);
        }
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent)?;
        }
        match entry.kind {
            OnDiskKind::File => {
                let (parent, name) = self.resolve_parent(logical)?;
                crate::util::copy_file(&parent.join(name), backup_path, entry.mode)?;
            }
            OnDiskKind::Symlink => {
                if backup_path.exists() || fs::symlink_metadata(backup_path).is_ok() {
                    let _ = fs::remove_file(backup_path);
                }
                std::os::unix::fs::symlink(
                    entry.target.as_deref().unwrap_or_default(),
                    backup_path,
                )?;
            }
            OnDiskKind::Dir => {}
        }
        sync_parent(backup_path.parent().unwrap_or(Path::new(".")))?;
        Ok(Some((entry.kind, entry.sha256)))
    }

    pub fn restore(&self, backup_path: &Path, logical: &str, mode: u32) -> Result<()> {
        let metadata = fs::symlink_metadata(backup_path)?;
        let uid = metadata.uid();
        let gid = metadata.gid();
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(backup_path)?;
            let target = target.to_string_lossy();
            self.write_symlink(&target, logical, uid, gid, "restore")?;
        } else {
            self.write_from(backup_path, logical, mode, uid, gid, "restore")?;
        }
        Ok(())
    }

    pub fn hard_sha(&self, logical: &str) -> Result<Option<String>> {
        Ok(self.on_disk(logical)?.map(|entry| entry.sha256))
    }
}

fn validate_resolvable(path: &str) -> Result<()> {
    if path == "/" {
        Ok(())
    } else {
        validate_absolute_path(path).map_err(SpkError::from)
    }
}

fn hash_bytes(data: &[u8]) -> String {
    sha256_bytes(data)
}

fn temp_name(parent: &Path, txid: &str, tag: &str) -> PathBuf {
    let mut counter = 0u64;
    loop {
        let path = parent.join(format!(".spk-{tag}-{txid}-{counter}"));
        if fs::symlink_metadata(&path).is_err() {
            return path;
        }
        counter += 1;
    }
}

fn place(temp: &Path, destination: &Path, destination_existed: bool) -> Result<()> {
    if destination_existed {
        fs::rename(temp, destination)?;
    } else {
        renameat_with(CWD, temp, CWD, destination, RenameFlags::NOREPLACE).map_err(|error| {
            SpkError::General(format!("cannot install {}: {error}", destination.display()))
        })?;
    }
    Ok(())
}

fn sync_parent(parent: &Path) -> Result<()> {
    File::open(parent)?.sync_all()?;
    Ok(())
}
