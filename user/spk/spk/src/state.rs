//
//       user/spk/spk/src/state.rs
//       /var/lib/spk paths, state loading and the write lock
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rustix::fs::{flock, FlockOperation};
use serde::de::DeserializeOwned;
use serde::Serialize;
use spk_core::db::{FileDb, HoldsDb, InstalledDb, ReposDb, WorldDb, DB_SCHEMA};

use crate::error::{Result, SpkError};

pub const STATE_DIR: &str = "var/lib/spk";
pub const TRUST_DIR: &str = "etc/spk/trusted.d";

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
    pub state: PathBuf,
    pub trusted: PathBuf,
    pub staging: PathBuf,
    pub backup: PathBuf,
    pub lock: PathBuf,
    pub journal: PathBuf,
}

impl Paths {
    pub fn new(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root).map_err(|error| {
            SpkError::General(format!("cannot open root {}: {error}", root.display()))
        })?;
        if !root.is_dir() {
            return Err(SpkError::General(format!(
                "root {} is not a directory",
                root.display()
            )));
        }
        let state = root.join(STATE_DIR);
        Ok(Self {
            staging: state.join("staging"),
            backup: state.join("backup"),
            lock: state.join("lock"),
            journal: state.join("journal"),
            trusted: root.join(TRUST_DIR),
            state,
            root,
        })
    }

    pub fn ensure_state(&self) -> Result<()> {
        create_dir(&self.state)?;
        create_dir(&self.staging)?;
        create_dir(&self.backup)?;
        create_dir(&self.root.join("var/cache/spk"))?;
        Ok(())
    }

    pub fn ensure_trust(&self) -> Result<()> {
        create_dir(&self.trusted)?;
        Ok(())
    }

    pub fn db_path(&self, name: &str) -> PathBuf {
        self.state.join(name)
    }
}

fn create_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

use std::os::unix::fs::PermissionsExt;

pub struct State {
    pub installed: InstalledDb,
    pub files: FileDb,
    pub world: WorldDb,
    pub repos: ReposDb,
    pub holds: HoldsDb,
}

impl State {
    pub fn load(paths: &Paths) -> Result<Self> {
        Ok(Self {
            installed: load_db(paths, "installed.json")?,
            files: load_db(paths, "files.json")?,
            world: load_db(paths, "world.json")?,
            repos: load_db(paths, "repos.json")?,
            holds: load_db(paths, "holds.json")?,
        })
    }

    pub fn package_count(&self) -> usize {
        self.installed.packages.len()
    }
}

trait Schema {
    fn schema(&self) -> u32;
}

impl Schema for InstalledDb {
    fn schema(&self) -> u32 {
        self.schema
    }
}
impl Schema for FileDb {
    fn schema(&self) -> u32 {
        self.schema
    }
}
impl Schema for WorldDb {
    fn schema(&self) -> u32 {
        self.schema
    }
}
impl Schema for ReposDb {
    fn schema(&self) -> u32 {
        self.schema
    }
}
impl Schema for HoldsDb {
    fn schema(&self) -> u32 {
        self.schema
    }
}

fn load_db<T>(paths: &Paths, name: &str) -> Result<T>
where
    T: DeserializeOwned + Default + Schema + Serialize,
{
    let path = paths.db_path(name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() {
        return Err(SpkError::General(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let bytes = fs::read(&path)?;
    let db: T = serde_json::from_slice(&bytes)
        .map_err(|error| SpkError::General(format!("{}: {error}", path.display())))?;
    if db.schema() != DB_SCHEMA {
        return Err(SpkError::General(format!(
            "{}: unsupported schema {}",
            path.display(),
            db.schema()
        )));
    }
    Ok(db)
}

/// Write a state file next to its final name and fsync it. The rename happens
/// only after the whole transaction has reached its database phase.
pub fn write_temp_json<T: Serialize>(
    paths: &Paths,
    name: &str,
    value: &T,
    txid: &str,
) -> Result<PathBuf> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let path = paths.state.join(format!("{name}.tmp.{txid}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    Ok(path)
}

pub struct StateLock {
    _file: File,
}

/// Exclusive lock for mutating transactions. The file stays locked for the
/// lifetime of the guard.
pub fn lock_exclusive(paths: &Paths) -> Result<StateLock> {
    paths.ensure_state()?;
    if let Ok(metadata) = fs::symlink_metadata(&paths.lock) {
        if metadata.file_type().is_symlink() {
            return Err(SpkError::General(format!(
                "{} must not be a symlink",
                paths.lock.display()
            )));
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock)?;
    flock(&file, FlockOperation::LockExclusive).map_err(|error| {
        SpkError::General(format!("cannot lock {}: {error}", paths.lock.display()))
    })?;
    Ok(StateLock { _file: file })
}
