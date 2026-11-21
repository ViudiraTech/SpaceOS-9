//
//       user/spk/spk-core/src/archive.rs
//       .spk tar+zstd scanning, path safety and manifest verification
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use sha2::{Digest, Sha256};
use tar::EntryType;

use crate::fsutil::{sha256_bytes, sha256_file};
use crate::manifest::{
    validate_relative_path, validate_symlink_target, FileType, Manifest, META_BUILDINFO,
    META_CHANGELOG, META_INSTALL, META_MANIFEST, RESERVED_ROOT_FILES,
};
use crate::{Error, Result};

pub const MAX_META_SIZE: u64 = 4 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 1_000_000;
pub const MAX_ZSTD_WINDOW: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

impl EntryKind {
    pub fn from_file_type(kind: FileType) -> Self {
        match kind {
            FileType::File => Self::File,
            FileType::Dir => Self::Dir,
            FileType::Symlink => Self::Symlink,
        }
    }

    pub fn to_file_type(self) -> FileType {
        match self {
            Self::File => FileType::File,
            Self::Dir => FileType::Dir,
            Self::Symlink => FileType::Symlink,
        }
    }
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::File => "file",
            Self::Dir => "directory",
            Self::Symlink => "symlink",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryInfo {
    pub path: String,
    pub kind: EntryKind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// File bytes, symlink target bytes, or 0 for directories.
    pub size: u64,
    /// SHA-256 of file contents or of the symlink target string.
    pub sha256: String,
    pub target: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RawScan {
    /// Payload metadata files, keyed by their root-relative archive name.
    pub meta: BTreeMap<String, Vec<u8>>,
    pub entries: Vec<EntryInfo>,
}

#[derive(Clone, Debug)]
pub struct ScannedPackage {
    pub manifest: Manifest,
    pub entries: Vec<EntryInfo>,
    pub install_script: Option<Vec<u8>>,
    pub changelog: Option<Vec<u8>>,
    pub buildinfo: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct InspectedPackage {
    pub package_sha256: String,
    pub package_size: u64,
    pub manifest: Manifest,
    pub entries: Vec<EntryInfo>,
    pub install_script: Option<Vec<u8>>,
    pub changelog: Option<Vec<u8>>,
    pub buildinfo: Option<Vec<u8>>,
}

pub fn is_meta_path(path: &str) -> bool {
    !path.contains('/') && RESERVED_ROOT_FILES.contains(&path)
}

/// Read every tar entry of an uncompressed payload and enforce the package
/// path/type policy. This is the single place where tar input becomes trusted.
pub fn scan_tar<R: Read>(reader: R) -> Result<RawScan> {
    scan_tar_with(reader, None)
}

fn scan_tar_with<R: Read>(reader: R, extract_root: Option<&Path>) -> Result<RawScan> {
    let mut archive = tar::Archive::new(reader);
    let mut scan = RawScan::default();
    let mut seen = BTreeSet::new();
    let mut non_dirs = BTreeSet::new();
    let mut directory_prefixes = BTreeSet::new();
    let mut count = 0usize;

    for entry in archive
        .entries()
        .map_err(|e| Error::Package(format!("cannot read tar: {e}")))?
    {
        count += 1;
        if count > MAX_ENTRIES {
            return Err(Error::Package(format!(
                "archive has more than {MAX_ENTRIES} entries"
            )));
        }
        let mut entry = entry?;
        let header = entry.header();
        let entry_type = header.entry_type();
        let kind = match entry_type {
            EntryType::Regular => EntryKind::File,
            EntryType::Directory => EntryKind::Dir,
            EntryType::Symlink => EntryKind::Symlink,
            EntryType::Link => {
                return Err(Error::Package(
                    "hard links are not supported yet (planned for P2)".into(),
                ))
            }
            EntryType::Char | EntryType::Block | EntryType::Fifo => {
                return Err(Error::UnsafePath(format!(
                    "device or FIFO entry is not allowed ({entry_type:?})"
                )))
            }
            EntryType::GNUSparse => {
                return Err(Error::UnsafePath(
                    "sparse tar entries are not supported".into(),
                ))
            }
            other => {
                return Err(Error::UnsafePath(format!(
                    "unsupported tar entry type {other:?}"
                )))
            }
        };

        let raw_path = entry
            .path()
            .map_err(|e| Error::UnsafePath(format!("invalid tar path: {e}")))?;
        let raw_path = raw_path
            .to_str()
            .ok_or_else(|| Error::UnsafePath("non-UTF-8 tar path".into()))?;
        let mut path = raw_path.to_string();
        if kind == EntryKind::Dir {
            while path.ends_with('/') {
                path.pop();
            }
        }
        validate_relative_path(&path)?;

        if !seen.insert(path.clone()) {
            return Err(Error::Package(format!("duplicate archive entry '{path}'")));
        }
        if let Some(ancestor) = non_dir_ancestor(&path, &non_dirs) {
            return Err(Error::UnsafePath(format!(
                "'{path}' is nested below non-directory entry '{ancestor}'"
            )));
        }
        if kind == EntryKind::Dir {
            if non_dirs.contains(&path) {
                return Err(Error::UnsafePath(format!(
                    "'{path}' would replace a file or symlink with a directory"
                )));
            }
        } else {
            if directory_prefixes.contains(&path) {
                return Err(Error::UnsafePath(format!(
                    "'{path}' would replace an already-populated directory"
                )));
            }
            non_dirs.insert(path.clone());
        }
        for ancestor in ancestors(&path) {
            directory_prefixes.insert(ancestor);
        }
        if kind == EntryKind::Dir {
            directory_prefixes.insert(path.clone());
        }

        let uid = header.uid()?;
        let gid = header.gid()?;
        if uid != 0 || gid != 0 {
            return Err(Error::Package(format!(
                "'{path}' is owned by {uid}:{gid}; packages must be root-owned"
            )));
        }
        let mode = header.mode()? & 0o7777;
        let declared_size = entry.size();

        if is_meta_path(&path) {
            if kind != EntryKind::File {
                return Err(Error::Package(format!(
                    "'{path}' must be a regular metadata file"
                )));
            }
            if declared_size > MAX_META_SIZE {
                return Err(Error::Package(format!(
                    "'{path}' exceeds {MAX_META_SIZE} bytes"
                )));
            }
            let mut data = Vec::new();
            (&mut entry)
                .take(MAX_META_SIZE + 1)
                .read_to_end(&mut data)?;
            if data.len() as u64 != declared_size {
                return Err(Error::Package(format!(
                    "'{path}': expected {declared_size} bytes, read {}",
                    data.len()
                )));
            }
            scan.meta.insert(path, data);
            continue;
        }

        match kind {
            EntryKind::File => {
                let mut hasher = Sha256::new();
                let mut buffer = [0u8; 64 * 1024];
                let mut total = 0u64;
                let mut staged = match extract_root {
                    Some(root) => Some(create_staged_file(root, &path)?),
                    None => None,
                };
                loop {
                    let read = entry.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                    if let Some(file) = staged.as_mut() {
                        file.write_all(&buffer[..read])?;
                    }
                    total += read as u64;
                }
                if total != declared_size {
                    return Err(Error::Package(format!(
                        "'{path}': expected {declared_size} bytes, read {total}"
                    )));
                }
                if let Some(file) = staged {
                    drop(file);
                    set_staged_mode(extract_root.unwrap(), &path, mode)?;
                }
                scan.entries.push(EntryInfo {
                    path,
                    kind,
                    mode,
                    uid: uid as u32,
                    gid: gid as u32,
                    size: total,
                    sha256: hex::encode(hasher.finalize()),
                    target: None,
                });
            }
            EntryKind::Dir => {
                if declared_size != 0 {
                    return Err(Error::Package(format!(
                        "'{path}': directory entry carries data"
                    )));
                }
                if let Some(root) = extract_root {
                    extract_dir(root, &path, mode)?;
                }
                scan.entries.push(EntryInfo {
                    path,
                    kind,
                    mode,
                    uid: uid as u32,
                    gid: gid as u32,
                    size: 0,
                    sha256: String::new(),
                    target: None,
                });
            }
            EntryKind::Symlink => {
                if declared_size != 0 {
                    return Err(Error::Package(format!(
                        "'{path}': symlink entry carries data"
                    )));
                }
                let target = entry
                    .link_name()?
                    .ok_or_else(|| Error::UnsafePath(format!("'{path}': symlink without target")))?
                    .to_str()
                    .ok_or_else(|| {
                        Error::UnsafePath(format!("'{path}': non-UTF-8 symlink target"))
                    })?
                    .to_string();
                validate_symlink_target(&format!("/{path}"), &target)?;
                if let Some(root) = extract_root {
                    extract_symlink(root, &path, &target)?;
                }
                scan.entries.push(EntryInfo {
                    path,
                    kind,
                    mode,
                    uid: uid as u32,
                    gid: gid as u32,
                    size: target.len() as u64,
                    sha256: sha256_bytes(target.as_bytes()),
                    target: Some(target),
                });
            }
        }
    }

    Ok(scan)
}

fn create_staged_file(root: &Path, path: &str) -> Result<File> {
    let full = root.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&full)?)
}

fn set_staged_mode(root: &Path, path: &str, mode: u32) -> Result<()> {
    fs::set_permissions(root.join(path), fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn extract_dir(root: &Path, path: &str, mode: u32) -> Result<()> {
    extract_dir_impl(root, path, mode).map_err(Error::from)
}

fn extract_dir_impl(root: &Path, path: &str, mode: u32) -> std::io::Result<()> {
    let full = root.join(path);
    fs::create_dir_all(&full)?;
    fs::set_permissions(&full, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn extract_symlink(root: &Path, path: &str, target: &str) -> Result<()> {
    extract_symlink_impl(root, path, target).map_err(Error::from)
}

fn extract_symlink_impl(root: &Path, path: &str, target: &str) -> std::io::Result<()> {
    let full = root.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(target, &full)?;
    Ok(())
}

fn ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut end = path.len();
    while let Some(pos) = path[..end].rfind('/') {
        out.push(path[..pos].to_string());
        end = pos;
    }
    out
}

fn non_dir_ancestor(path: &str, non_dirs: &BTreeSet<String>) -> Option<String> {
    let mut end = 0usize;
    while let Some(pos) = path[end..].find('/') {
        let prefix_end = end + pos;
        let prefix = &path[..prefix_end];
        if non_dirs.contains(prefix) {
            return Some(prefix.to_string());
        }
        end = prefix_end + 1;
    }
    None
}

/// Cross-check a scanned archive against its declared manifest. Every payload
/// entry must be listed exactly once, with identical kind, mode, owner, size,
/// digest and link target.
pub fn verify_scan(scan: RawScan) -> Result<ScannedPackage> {
    let manifest_bytes = scan
        .meta
        .get(META_MANIFEST)
        .ok_or_else(|| Error::Package("archive has no manifest.toml".into()))?;
    let manifest_text = std::str::from_utf8(manifest_bytes)
        .map_err(|_| Error::Package("manifest.toml is not valid UTF-8".into()))?;
    let manifest = Manifest::from_toml(manifest_text)?;

    if scan.entries.len() != manifest.files.len() {
        return Err(Error::Package(format!(
            "archive contains {} payload entries, manifest lists {}",
            scan.entries.len(),
            manifest.files.len()
        )));
    }
    let entries: BTreeMap<&str, &EntryInfo> = scan
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    if entries.len() != scan.entries.len() {
        return Err(Error::Package("duplicate payload paths".into()));
    }
    for file in &manifest.files {
        let relative = file.path.trim_start_matches('/');
        let entry = entries.get(relative).ok_or_else(|| {
            Error::Package(format!(
                "{} is listed in the manifest but missing from the archive",
                file.path
            ))
        })?;
        let expected_kind = EntryKind::from_file_type(file.file_type);
        if entry.kind != expected_kind {
            return Err(Error::Package(format!(
                "{}: manifest says {}, archive says {}",
                file.path, expected_kind, entry.kind
            )));
        }
        if entry.mode != file.mode_bits()? {
            return Err(Error::Package(format!(
                "{}: mode mismatch (manifest {}, archive {:04o})",
                file.path, file.mode, entry.mode
            )));
        }
        if entry.uid != file.uid || entry.gid != file.gid {
            return Err(Error::Package(format!(
                "{}: owner mismatch (manifest {}:{}, archive {}:{})",
                file.path, file.uid, file.gid, entry.uid, entry.gid
            )));
        }
        if entry.size != file.size {
            return Err(Error::Package(format!(
                "{}: size mismatch (manifest {}, archive {})",
                file.path, file.size, entry.size
            )));
        }
        if entry.sha256 != file.sha256 {
            return Err(Error::Package(format!("{}: sha256 mismatch", file.path)));
        }
        if entry.target.as_deref() != file.target.as_deref() {
            return Err(Error::Package(format!(
                "{}: symlink target mismatch",
                file.path
            )));
        }
    }

    let has_script = scan.meta.contains_key(META_INSTALL);
    if manifest.scripts != has_script {
        return Err(Error::Package(format!(
            "manifest declares scripts = {}, archive {} .INSTALL",
            manifest.scripts,
            if has_script { "contains" } else { "lacks" }
        )));
    }

    Ok(ScannedPackage {
        manifest,
        entries: scan.entries,
        install_script: scan.meta.get(META_INSTALL).cloned(),
        changelog: scan.meta.get(META_CHANGELOG).cloned(),
        buildinfo: scan.meta.get(META_BUILDINFO).cloned(),
    })
}

pub fn inspect_spk(path: &Path) -> Result<InspectedPackage> {
    let package_size = fs::metadata(path)?.len();
    let package_sha256 = sha256_file(path)?;
    let reader = BufReader::new(File::open(path)?);
    let decoder =
        ruzstd::decoding::StreamingDecoder::new_with_max_window_size(reader, MAX_ZSTD_WINDOW)
            .map_err(|e| Error::Package(format!("zstd: {e}")))?;
    let scanned = verify_scan(scan_tar(decoder)?)?;
    Ok(InspectedPackage {
        package_sha256,
        package_size,
        manifest: scanned.manifest,
        entries: scanned.entries,
        install_script: scanned.install_script,
        changelog: scanned.changelog,
        buildinfo: scanned.buildinfo,
    })
}

/// Verify and unpack a `.spk` payload into a fresh staging directory. The
/// directory is removed again if any check fails, so a partial tree never
/// reaches the commit phase.
pub fn extract_spk(path: &Path, dest: &Path) -> Result<ScannedPackage> {
    if dest.exists() {
        return Err(Error::Package(format!(
            "staging directory {} already exists",
            dest.display()
        )));
    }
    fs::create_dir_all(dest)?;
    let result = (|| -> Result<ScannedPackage> {
        let reader = BufReader::new(File::open(path)?);
        let decoder =
            ruzstd::decoding::StreamingDecoder::new_with_max_window_size(reader, MAX_ZSTD_WINDOW)
                .map_err(|e| Error::Package(format!("zstd: {e}")))?;
        verify_scan(scan_tar_with(decoder, Some(dest))?)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MAX_COMPONENT_LEN;
    use std::io::{self, Cursor};
    use tar::{EntryType, Header};

    struct Item<'a> {
        path: &'a str,
        kind: EntryType,
        data: &'a [u8],
        target: Option<&'a str>,
    }

    fn item(path: &'static str, kind: EntryType, data: &'static [u8]) -> Item<'static> {
        Item {
            path,
            kind,
            data,
            target: None,
        }
    }

    fn tar_bytes(items: &[Item<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for item in items {
            let mut header = Header::new_gnu();
            header.set_entry_type(item.kind);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            match item.kind {
                EntryType::Symlink => {
                    header.set_size(0);
                    builder
                        .append_link(&mut header, item.path, item.target.unwrap())
                        .unwrap();
                }
                EntryType::Directory | EntryType::Char | EntryType::Block | EntryType::Fifo => {
                    header.set_size(0);
                    builder
                        .append_data(&mut header, item.path, io::empty())
                        .unwrap();
                }
                _ => {
                    header.set_size(item.data.len() as u64);
                    builder
                        .append_data(&mut header, item.path, item.data)
                        .unwrap();
                }
            }
        }
        builder.finish().unwrap();
        builder.into_inner().unwrap()
    }

    fn scan(items: &[Item<'_>]) -> Result<RawScan> {
        scan_tar(Cursor::new(tar_bytes(items)))
    }

    /// Build a tar header with an arbitrary name, bypassing the tar crate's
    /// own path validation, so hostile inputs can be exercised.
    fn raw_tar(name: &str, kind: EntryType, data: &[u8]) -> Vec<u8> {
        let mut header = Header::new_gnu();
        header.set_entry_type(kind);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(data.len() as u64);
        header.set_path("placeholder").unwrap();
        {
            let bytes = header.as_mut_bytes();
            bytes[..100].fill(0);
            bytes[..name.len()].copy_from_slice(name.as_bytes());
        }
        header.set_cksum();
        let mut out = Vec::new();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        while out.len() % 512 != 0 {
            out.push(0);
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    #[test]
    fn accepts_plain_payload() {
        let raw = scan(&[
            item("usr", EntryType::Directory, b""),
            item("usr/bin", EntryType::Directory, b""),
            item("usr/bin/hello", EntryType::Regular, b"hello\n"),
            Item {
                path: "usr/bin/hi",
                kind: EntryType::Symlink,
                data: b"",
                target: Some("hello"),
            },
            Item {
                path: "usr/bin/sh",
                kind: EntryType::Symlink,
                data: b"",
                target: Some("/bin/sh"),
            },
        ])
        .unwrap();
        assert_eq!(raw.entries.len(), 5);
        let hello = raw
            .entries
            .iter()
            .find(|e| e.path == "usr/bin/hello")
            .unwrap();
        assert_eq!(hello.sha256, sha256_bytes(b"hello\n"));
        assert_eq!(hello.size, 6);
        let relative = raw.entries.iter().find(|e| e.path == "usr/bin/hi").unwrap();
        assert_eq!(relative.target.as_deref(), Some("hello"));
        assert_eq!(relative.kind, EntryKind::Symlink);
    }

    #[test]
    fn accepts_long_but_valid_paths() {
        let component = "a".repeat(200);
        let path = format!("usr/share/{component}/file");
        let raw = scan(&[Item {
            path: &path,
            kind: EntryType::Regular,
            data: b"x",
            target: None,
        }])
        .unwrap();
        assert_eq!(raw.entries.len(), 1);
        assert_eq!(raw.entries[0].path, path);
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        for path in [
            "../etc/passwd",
            "/etc/passwd",
            "usr/../../etc/passwd",
            "./hello",
        ] {
            let bytes = raw_tar(path, EntryType::Regular, b"x");
            let result = scan_tar(Cursor::new(bytes));
            assert!(result.is_err(), "should reject {path}");
        }
    }

    #[test]
    fn rejects_device_fifo_and_sparse() {
        for kind in [EntryType::Char, EntryType::Block, EntryType::Fifo] {
            let result = scan(&[item("dev/evil", kind, b"")]);
            assert!(result.is_err(), "should reject {kind:?}");
        }
    }

    #[test]
    fn rejects_symlink_escape_and_writes_through_symlink() {
        let escape = scan(&[Item {
            path: "usr/bin/evil",
            kind: EntryType::Symlink,
            data: b"",
            target: Some("../../../../etc/passwd"),
        }]);
        assert!(escape.is_err());

        let through = scan(&[
            Item {
                path: "usr/lib/evil",
                kind: EntryType::Symlink,
                data: b"",
                target: Some("/tmp"),
            },
            item("usr/lib/evil/passwd", EntryType::Regular, b"x"),
        ]);
        assert!(through.is_err());
    }

    #[test]
    fn rejects_file_over_directory_and_symlink_over_populated_dir() {
        let file_over_dir = scan(&[
            item("usr/lib/x", EntryType::Regular, b"x"),
            item("usr/lib", EntryType::Regular, b"x"),
        ]);
        assert!(file_over_dir.is_err());

        let symlink_over_dir = scan(&[
            item("usr/lib/x", EntryType::Regular, b"x"),
            Item {
                path: "usr/lib",
                kind: EntryType::Symlink,
                data: b"",
                target: Some(".."),
            },
        ]);
        assert!(symlink_over_dir.is_err());
    }

    #[test]
    fn rejects_duplicates_and_overlong_components() {
        let duplicate = scan(&[
            item("usr/bin/hello", EntryType::Regular, b"a"),
            item("usr/bin/hello", EntryType::Regular, b"b"),
        ]);
        assert!(duplicate.is_err());

        let long = "a".repeat(MAX_COMPONENT_LEN + 1);
        let overlong = scan(&[Item {
            path: &long,
            kind: EntryType::Regular,
            data: b"x",
            target: None,
        }]);
        assert!(overlong.is_err());
    }

    #[test]
    fn rejects_non_root_owned_entries() {
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(1000);
        header.set_gid(1000);
        header.set_mtime(0);
        header.set_size(1);
        tar.append_data(&mut header, "usr/bin/hello", &b"x"[..])
            .unwrap();
        tar.finish().unwrap();
        let bytes = tar.into_inner().unwrap();
        assert!(scan_tar(Cursor::new(bytes)).is_err());
    }
}
