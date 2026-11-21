//
//       user/sudo/src/util.rs
//       Small file/time helpers with root-owned security checks
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn read_to_string(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    fs::File::open(path)?.read_to_string(&mut text)?;
    Ok(text)
}

/// Configuration files must be regular, root-owned, and not writable by
/// group/other. Symlinks are rejected outright.
pub fn check_secure_file(path: &Path, max_mode: u32) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if meta.uid() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not owned by root", path.display()),
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is writable by group or other", path.display()),
        ));
    }
    if meta.mode() & 0o7777 > max_mode {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} has permissions wider than {:04o}",
                path.display(),
                max_mode
            ),
        ));
    }
    Ok(())
}

pub fn ensure_dir(path: &Path, mode: u32) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

pub fn write_private(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
    }
    fs::rename(&temp, path)?;
    if let Some(parent) = path.parent() {
        let _ = fs::File::open(parent).and_then(|dir| dir.sync_all());
    }
    Ok(())
}

pub fn append_private(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
    Ok(())
}

pub fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "localhost".into())
}
