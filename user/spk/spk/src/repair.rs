//
//       user/spk/spk/src/repair.rs
//       Journal rollback and crash recovery
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use spk_core::fsutil::sha256_file;
use spk_core::manifest::validate_absolute_path;

use crate::error::{Result, SpkError};
use crate::i18n::{self, Lang};
use crate::journal::{self, DbBackup, Event};
use crate::rootfs::{Fs, OnDiskKind};
use crate::state::{lock_exclusive, Paths, State};

#[derive(Clone, Debug)]
struct BackupInfo {
    host: PathBuf,
    old_sha256: Option<String>,
    kind: String,
}

pub fn repair(paths: &Paths, lang: Lang) -> Result<()> {
    let _lock = lock_exclusive(paths)?;
    if !paths.journal.exists() {
        println!("{}", i18n::nothing_to_repair(lang));
        return Ok(());
    }
    let (events, ended) = journal::read(&paths.journal)?;
    let txid = journal::begin_txid(&events)
        .unwrap_or("unknown")
        .to_string();
    println!("{}", i18n::repair_start(lang, &txid));
    let db_done = events.iter().any(|event| matches!(event, Event::DbDone));
    if !ended && !db_done {
        rollback_events(paths, lang, &events, false)?;
        println!("{}", i18n::repair_rolled_back(lang));
    }
    cleanup(paths, &txid)?;
    journal::remove(paths)?;
    let state = State::load(paths)?;
    crate::check::check(paths, &state, lang)?;
    Ok(())
}

/// Roll back the current journal, if any. Used both by `repair` and by a
/// failed transaction. The caller must already hold the write lock.
pub fn rollback_pending(paths: &Paths, lang: Lang, quiet: bool) -> Result<()> {
    if !paths.journal.exists() {
        return Ok(());
    }
    let (events, ended) = journal::read(&paths.journal)?;
    let txid = journal::begin_txid(&events)
        .unwrap_or("unknown")
        .to_string();
    let db_done = events.iter().any(|event| matches!(event, Event::DbDone));
    if !ended && !db_done {
        rollback_events(paths, lang, &events, quiet)?;
    }
    cleanup(paths, &txid)?;
    journal::remove(paths)?;
    Ok(())
}

fn rollback_events(paths: &Paths, lang: Lang, events: &[Event], quiet: bool) -> Result<()> {
    let fs = Fs::new(&paths.root)?;
    let backups = collect_backups(events);
    let actionable: Vec<&Event> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::Write { .. }
                    | Event::Remove { .. }
                    | Event::Rename { .. }
                    | Event::MkDir { .. }
                    | Event::RmDir { .. }
                    | Event::Backup { .. }
            )
        })
        .collect();
    let total = actionable.len();
    for (index, event) in actionable.iter().rev().enumerate() {
        let position = index + 1;
        match event {
            Event::Write {
                path,
                new_sha256,
                kind,
            } => {
                validate_absolute_path(path)?;
                if !quiet {
                    println!("({position}/{total}) {} {path}", i18n::removing(lang));
                }
                rollback_write(&fs, path, new_sha256.as_deref(), kind, &backups)?;
            }
            Event::Remove { path } => {
                validate_absolute_path(path)?;
                if !quiet {
                    println!("({position}/{total}) {} {path}", i18n::restoring(lang));
                }
                if fs.on_disk(path)?.is_none() {
                    restore_backup(&fs, path, &backups)?;
                }
            }
            Event::Rename { from, to } => {
                validate_absolute_path(from)?;
                validate_absolute_path(to)?;
                if !quiet {
                    println!(
                        "({position}/{total}) {} {to} -> {from}",
                        i18n::restoring(lang)
                    );
                }
                if fs.on_disk(to)?.is_some() {
                    fs.rename(to, from)?;
                }
            }
            Event::MkDir { path, .. } => {
                validate_absolute_path(path)?;
                fs.remove_dir(path)?;
            }
            Event::RmDir { path, mode } => {
                validate_absolute_path(path)?;
                if fs.on_disk(path)?.is_none() {
                    let mut created = Vec::new();
                    fs.ensure_dir(path, *mode, &mut created)?;
                }
            }
            Event::Backup { path, .. } => {
                validate_absolute_path(path)?;
                if fs.on_disk(path)?.is_none() {
                    if !quiet {
                        println!("({position}/{total}) {} {path}", i18n::restoring(lang));
                    }
                    restore_backup(&fs, path, &backups)?;
                }
            }
            _ => {}
        }
    }
    restore_database(paths, events)?;
    Ok(())
}

fn collect_backups(events: &[Event]) -> BTreeMap<String, BackupInfo> {
    let mut backups = BTreeMap::new();
    for event in events {
        if let Event::Backup {
            path,
            backup,
            old_sha256,
            kind,
        } = event
        {
            backups.insert(
                path.clone(),
                BackupInfo {
                    host: PathBuf::from(backup),
                    old_sha256: old_sha256.clone(),
                    kind: kind.clone(),
                },
            );
        }
    }
    backups
}

fn rollback_write(
    fs: &Fs,
    path: &str,
    new_sha256: Option<&str>,
    kind: &str,
    backups: &BTreeMap<String, BackupInfo>,
) -> Result<()> {
    let current = fs.on_disk(path)?;
    let matches_new = match (&current, new_sha256) {
        (Some(entry), Some(expected)) => entry.sha256 == expected,
        (Some(entry), None) => kind_matches(entry.kind, kind),
        (None, _) => false,
    };
    if matches_new {
        if backups.contains_key(path) {
            restore_backup(fs, path, backups)?;
        } else {
            fs.remove(path)?;
        }
    } else if current.is_none() && backups.contains_key(path) {
        restore_backup(fs, path, backups)?;
    }
    Ok(())
}

fn restore_backup(fs: &Fs, path: &str, backups: &BTreeMap<String, BackupInfo>) -> Result<()> {
    let Some(info) = backups.get(path) else {
        return Ok(());
    };
    verify_backup(&info.host, &info.kind, info.old_sha256.as_deref())?;
    let metadata = fs::symlink_metadata(&info.host)?;
    let mode = metadata.mode() & 0o7777;
    fs.restore(&info.host, path, mode)
}

fn verify_backup(host: &Path, kind: &str, expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let metadata = fs::symlink_metadata(host)?;
    let actual = if metadata.file_type().is_symlink() {
        let target = fs::read_link(host)?;
        spk_core::fsutil::sha256_bytes(target.to_string_lossy().as_bytes())
    } else {
        sha256_file(host)?
    };
    if actual != expected {
        return Err(SpkError::General(format!(
            "backup {} no longer matches the journal; refusing to restore",
            host.display()
        )));
    }
    let _ = kind;
    Ok(())
}

fn kind_matches(kind: OnDiskKind, text: &str) -> bool {
    matches!(
        (kind, text),
        (OnDiskKind::File, "file") | (OnDiskKind::Symlink, "symlink") | (OnDiskKind::Dir, "dir")
    )
}

fn restore_database(paths: &Paths, events: &[Event]) -> Result<()> {
    let backups: Option<&Vec<DbBackup>> = events.iter().find_map(|event| match event {
        Event::DbBegin { files } => Some(files),
        _ => None,
    });
    let Some(backups) = backups else {
        return Ok(());
    };
    for backup in backups {
        let name = backup
            .logical
            .strip_prefix("/var/lib/spk/")
            .ok_or_else(|| SpkError::General(format!("invalid DB path '{}'", backup.logical)))?;
        if !matches!(name, "installed.json" | "files.json" | "world.json") {
            return Err(SpkError::General(format!(
                "invalid DB backup name '{name}'"
            )));
        }
        let target = paths.db_path(name);
        if backup.existed {
            let host = PathBuf::from(&backup.backup);
            if !host.starts_with(&paths.backup) {
                return Err(SpkError::General(format!(
                    "DB backup {} is outside the backup directory",
                    host.display()
                )));
            }
            let temp = paths.state.join(format!(".{name}.repair"));
            crate::util::copy_file(&host, &temp, 0o644)?;
            fs::rename(&temp, &target)?;
        } else {
            match fs::remove_file(&target) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    fs::File::open(&paths.state)?.sync_all()?;
    Ok(())
}

fn cleanup(paths: &Paths, txid: &str) -> Result<()> {
    let _ = fs::remove_dir_all(paths.staging.join(txid));
    let _ = fs::remove_dir_all(paths.backup.join(txid));
    if let Ok(entries) = fs::read_dir(&paths.state) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().contains(".tmp.")
                || name.to_string_lossy().ends_with(".repair")
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}
