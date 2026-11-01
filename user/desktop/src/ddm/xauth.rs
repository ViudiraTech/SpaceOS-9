/*
 *
 *       user/desktop/src/ddm/xauth.rs
 *       Xauthority cookie generation and MIT-MAGIC-COOKIE-1 file handling
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! X11 authorization is the boundary that prevents one local user from
//! connecting to another user's display.  DDM keeps the server-side file
//! root-only and gives each client a private copy containing exactly one
//! wildcard `MIT-MAGIC-COOKIE-1` entry, which is how LightDM and SDDM hand a
//! display to a greeter or a session.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const COOKIE_NAME: &[u8] = b"MIT-MAGIC-COOKIE-1";
pub const COOKIE_LEN: usize = 16;
const FAMILY_WILD: u16 = 0xffff;
const MAX_AUTH_BYTES: usize = 64 * 1024;
const MAX_ENTRIES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthEntry {
    pub family: u16,
    pub address: Vec<u8>,
    pub number: Vec<u8>,
    pub name: Vec<u8>,
    pub data: Vec<u8>,
}

impl AuthEntry {
    pub fn wildcard_cookie(cookie: &[u8; COOKIE_LEN]) -> Self {
        Self {
            family: FAMILY_WILD,
            address: Vec::new(),
            number: Vec::new(),
            name: COOKIE_NAME.to_vec(),
            data: cookie.to_vec(),
        }
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.family.to_be_bytes());
        write_field(out, &self.address);
        write_field(out, &self.number);
        write_field(out, &self.name);
        write_field(out, &self.data);
    }
}

pub fn random_cookie() -> io::Result<[u8; COOKIE_LEN]> {
    let mut cookie = [0u8; COOKIE_LEN];
    let mut random = File::open("/dev/urandom")?;
    random.read_exact(&mut cookie)?;
    // Reject the all-zero cookie; a dead entropy source must not silently
    // produce a predictable display password.
    if cookie.iter().all(|byte| *byte == 0) {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "entropy source returned an all-zero cookie",
        ));
    }
    Ok(cookie)
}

pub fn write_file(
    path: &Path,
    entries: &[AuthEntry],
    mode: u32,
    owner: Option<(u32, u32)>,
) -> io::Result<()> {
    let mut data = Vec::new();
    for entry in entries.iter().take(MAX_ENTRIES) {
        entry.write_to(&mut data);
    }
    if data.len() > MAX_AUTH_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Xauthority data is too large",
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(path);
    let _ = fs::remove_file(&temporary);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    if let Err(error) = (|| -> io::Result<()> {
        file.write_all(&data)?;
        file.sync_all()?;
        set_mode(&temporary, mode)?;
        if let Some((uid, gid)) = owner {
            chown(&temporary, uid, gid)?;
        }
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, path)?;
    sync_directory(parent);
    Ok(())
}

pub fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn chown(path: &Path, uid: u32, gid: u32) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let result = unsafe { libc::chown(path.as_ptr(), uid, gid) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(name)
}

fn sync_directory(path: &Path) {
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

fn write_field(out: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().min(u16::MAX as usize);
    out.extend_from_slice(&(length as u16).to_be_bytes());
    out.extend_from_slice(&value[..length]);
}

#[cfg(test)]
fn read_u16(data: &[u8], offset: &mut usize) -> io::Result<u16> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "offset overflow"))?;
    let bytes = data
        .get(*offset..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "truncated Xauthority"))?;
    *offset = end;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

#[cfg(test)]
fn read_field(data: &[u8], offset: &mut usize) -> io::Result<Vec<u8>> {
    let length = read_u16(data, offset)? as usize;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "offset overflow"))?;
    let bytes = data
        .get(*offset..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "truncated Xauthority"))?;
    *offset = end;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_wildcard_entry() {
        let entry = AuthEntry::wildcard_cookie(&[0x5a; COOKIE_LEN]);
        let mut data = Vec::new();
        entry.write_to(&mut data);
        let mut offset = 0;
        let family = read_u16(&data, &mut offset).unwrap();
        let address = read_field(&data, &mut offset).unwrap();
        let number = read_field(&data, &mut offset).unwrap();
        let name = read_field(&data, &mut offset).unwrap();
        let value = read_field(&data, &mut offset).unwrap();
        assert_eq!(family, FAMILY_WILD);
        assert!(address.is_empty());
        assert!(number.is_empty());
        assert_eq!(name, COOKIE_NAME);
        assert_eq!(value, vec![0x5a; COOKIE_LEN]);
        assert_eq!(offset, data.len());
    }

    #[test]
    fn rejects_truncated_entry() {
        let entry = AuthEntry::wildcard_cookie(&[7; COOKIE_LEN]);
        let mut data = Vec::new();
        entry.write_to(&mut data);
        data.truncate(data.len() - 1);
        let mut offset = 0;
        assert!(read_u16(&data, &mut offset).is_ok());
        assert!(read_field(&data, &mut offset).is_ok());
        assert!(read_field(&data, &mut offset).is_ok());
        assert!(read_field(&data, &mut offset).is_ok());
        assert!(read_field(&data, &mut offset).is_err());
    }
}
