//
//       user/spk/spk/src/journal.rs
//       Append-only transaction journal and recovery reader
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use spk_core::fsutil::sync_dir;

use crate::error::{Result, SpkError};

pub const JOURNAL_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Begin {
        txid: String,
        op: String,
        packages: Vec<String>,
        time: u64,
    },
    MkDir {
        path: String,
        mode: u32,
    },
    /// Intent: the old entry is about to be copied to `backup`.
    Backup {
        path: String,
        backup: String,
        old_sha256: Option<String>,
        kind: String,
    },
    /// Intent: `path` is about to receive new content with this digest.
    Write {
        path: String,
        new_sha256: Option<String>,
        kind: String,
    },
    /// Intent: `path` is about to be unlinked (its Backup event precedes it).
    Remove {
        path: String,
    },
    /// Intent: a conffile is about to be renamed (removal policy or .spksave).
    Rename {
        from: String,
        to: String,
    },
    /// Intent: an empty directory is about to be removed.
    RmDir {
        path: String,
        mode: u32,
    },
    DbBegin {
        files: Vec<DbBackup>,
    },
    DbDone,
    End,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DbBackup {
    pub logical: String,
    pub backup: String,
    pub existed: bool,
}

#[derive(Serialize, Deserialize)]
struct Line {
    schema: u32,
    #[serde(flatten)]
    event: Event,
}

pub struct Journal {
    file: File,
    path: PathBuf,
    state_dir: PathBuf,
    pub txid: String,
}

impl Journal {
    pub fn begin(
        journal_path: &Path,
        txid: &str,
        operation: &str,
        packages: &[String],
    ) -> Result<Self> {
        let state_dir = journal_path
            .parent()
            .ok_or_else(|| SpkError::General("journal path has no parent".into()))?
            .to_path_buf();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(journal_path)
            .map_err(|error| {
                SpkError::General(format!(
                    "cannot create journal {}: {error}",
                    journal_path.display()
                ))
            })?;
        sync_dir(&state_dir)?;
        let mut journal = Self {
            file,
            path: journal_path.to_path_buf(),
            state_dir,
            txid: txid.to_string(),
        };
        journal.append(&Event::Begin {
            txid: txid.to_string(),
            op: operation.to_string(),
            packages: packages.to_vec(),
            time: crate::util::now(),
        })?;
        Ok(journal)
    }

    pub fn append(&mut self, event: &Event) -> Result<()> {
        let mut bytes = serde_json::to_vec(&Line {
            schema: JOURNAL_SCHEMA,
            event: event.clone(),
        })?;
        bytes.push(b'\n');
        self.file.write_all(&bytes)?;
        self.file.sync_all()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append the End marker, remove the journal and fsync the directory.
    pub fn finish(mut self) -> Result<()> {
        self.append(&Event::End)?;
        fs::remove_file(&self.path)?;
        sync_dir(&self.state_dir)?;
        Ok(())
    }
}

pub fn read(path: &Path) -> Result<(Vec<Event>, bool)> {
    let bytes = fs::read(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| SpkError::General("journal is not valid UTF-8".into()))?;
    let mut events: Vec<Event> = Vec::new();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<Line>(line) {
            Ok(parsed) => {
                if parsed.schema != JOURNAL_SCHEMA {
                    return Err(SpkError::General(format!(
                        "journal uses unsupported schema {}",
                        parsed.schema
                    )));
                }
                events.push(parsed.event);
            }
            // A torn final line was written before fsync returned, so the
            // action it described never started. Ignore it.
            Err(_) if index + 1 == lines.len() => break,
            Err(error) => {
                return Err(SpkError::General(format!(
                    "journal line {}: {error}",
                    index + 1
                )))
            }
        }
    }
    if !matches!(events.first(), Some(Event::Begin { .. })) {
        return Err(SpkError::General(
            "journal does not start with a begin event".into(),
        ));
    }
    let ended = matches!(events.last(), Some(Event::End));
    Ok((events, ended))
}

pub fn begin_txid(events: &[Event]) -> Option<&str> {
    match events.first() {
        Some(Event::Begin { txid, .. }) => Some(txid),
        _ => None,
    }
}

pub fn pending_txid(paths: &crate::state::Paths) -> Result<Option<String>> {
    if !paths.journal.exists() {
        return Ok(None);
    }
    let (events, _) = read(&paths.journal)?;
    Ok(begin_txid(&events).map(str::to_string))
}

pub fn remove(paths: &crate::state::Paths) -> Result<()> {
    if paths.journal.exists() {
        fs::remove_file(&paths.journal)?;
        sync_dir(&paths.state)?;
    }
    Ok(())
}
