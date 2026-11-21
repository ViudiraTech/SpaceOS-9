//
//       user/spk/spk-core/src/fsutil.rs
//       Durable file helpers: sha256, atomic writes, directory fsync
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn create_temp(parent: &Path, name: &str, mode: u32) -> io::Result<(File, PathBuf)> {
    for _ in 0..100 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".{}.tmp.{}.{}", name, std::process::id(), sequence));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(mode);
        match options.open(&temp) {
            Ok(file) => return Ok((file, temp)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique temporary file",
    ))
}

/// Write `data` to `path` through a same-directory temporary file, fsync the
/// file, rename it into place and fsync the parent directory. A reader either
/// sees the old file or the complete new one, including after a crash.
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy()
        .into_owned();
    fs::create_dir_all(parent)?;
    let (mut file, temp) = create_temp(parent, &name, mode)?;

    let result = (|| -> io::Result<()> {
        file.write_all(data)?;
        file.sync_all()?;
        // Open mode is subject to umask; enforce the requested mode explicitly.
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// fsync a directory after a rename performed elsewhere.
pub fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
