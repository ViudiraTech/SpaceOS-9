//
//       user/spk/spk/src/check.rs
//       Database and filesystem consistency checker
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeSet;

use spk_core::db::InstallReason;
use spk_core::manifest::{validate_absolute_path, FileType, MAX_PATH_LEN};

use crate::error::{Result, SpkError};
use crate::i18n::{self, Lang};
use crate::rootfs::{Fs, OnDiskKind};
use crate::state::{Paths, State};

pub fn check(paths: &Paths, state: &State, lang: Lang) -> Result<()> {
    let errors = collect_errors(paths, state)?;
    if errors.is_empty() {
        println!("{}", i18n::database_ok(lang));
        Ok(())
    } else {
        Err(SpkError::Check {
            message: i18n::database_failed(lang).to_string(),
            lines: errors,
        })
    }
}

pub fn collect_errors(paths: &Paths, state: &State) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    let fs = Fs::new(&paths.root)?;

    for (name, package) in &state.installed.packages {
        if name != &package.name {
            errors.push(format!(
                "installed.json key '{name}' does not match package name '{}'",
                package.name
            ));
        }
        let mut seen = BTreeSet::new();
        for file in &package.files {
            if let Err(error) = validate_absolute_path(&file.path) {
                errors.push(format!("{}: {error}", file.path));
                continue;
            }
            if !seen.insert(file.path.clone()) {
                errors.push(format!("{}: duplicate file entry in {name}", file.path));
            }
            let on_disk = fs.on_disk(&file.path)?;
            match file.file_type {
                FileType::Dir => {
                    if state.files.files.contains_key(&file.path) {
                        errors.push(format!(
                            "{}: directories must not appear in files.json",
                            file.path
                        ));
                    }
                    if !fs.is_dir(&file.path)? {
                        errors.push(format!("{}: directory is missing", file.path));
                    }
                }
                FileType::File | FileType::Symlink => {
                    let record = state.files.files.get(&file.path);
                    match record {
                        Some(record) if record.pkg == *name => {
                            let expected_type = file.file_type;
                            if record.file_type != expected_type
                                || record.mode != file.mode
                                || record.uid != file.uid
                                || record.gid != file.gid
                                || record.sha256 != file.sha256
                                || record.size != file.size
                                || record.target != file.target
                            {
                                errors.push(format!(
                                    "{}: files.json metadata does not match {name}",
                                    file.path
                                ));
                            }
                        }
                        Some(record) => errors.push(format!(
                            "{}: owned by {} in files.json, listed by {name}",
                            file.path, record.pkg
                        )),
                        None => errors.push(format!(
                            "{}: missing from files.json (owner {name})",
                            file.path
                        )),
                    }
                    let Some(entry) = on_disk else {
                        errors.push(format!("{}: file is missing", file.path));
                        continue;
                    };
                    let expected = match file.file_type {
                        FileType::File => OnDiskKind::File,
                        FileType::Symlink => OnDiskKind::Symlink,
                        FileType::Dir => unreachable!(),
                    };
                    if entry.kind != expected {
                        errors.push(format!("{}: recorded type does not match disk", file.path));
                        continue;
                    }
                    let conffile = package.conffiles.contains(&file.path);
                    if !conffile {
                        if entry.sha256 != file.sha256 {
                            errors.push(format!("{}: sha256 does not match", file.path));
                        }
                        if entry.size != file.size {
                            errors.push(format!("{}: size does not match", file.path));
                        }
                    }
                    if file.file_type == FileType::Symlink && entry.target != file.target {
                        errors.push(format!("{}: symlink target does not match", file.path));
                    }
                }
            }
        }
        for path in &package.conffiles {
            if !package
                .files
                .iter()
                .any(|file| file.path == *path && file.file_type == FileType::File)
            {
                errors.push(format!("{path}: conffile is not a regular file of {name}"));
            }
        }
        if package.files.is_empty() && package.allow_setuid {
            errors.push(format!("{name}: allow_setuid package has no files"));
        }
    }

    for (path, record) in &state.files.files {
        if let Err(error) = validate_absolute_path(path) {
            errors.push(format!("{path}: {error}"));
            continue;
        }
        if path.len() > MAX_PATH_LEN {
            errors.push(format!("{path}: path is too long"));
        }
        let Some(package) = state.installed.packages.get(&record.pkg) else {
            errors.push(format!("{path}: owner {} is not installed", record.pkg));
            continue;
        };
        let listed = package
            .files
            .iter()
            .any(|file| file.path == *path && file.file_type != FileType::Dir);
        if !listed {
            errors.push(format!(
                "{path}: files.json owner {} does not list this path",
                record.pkg
            ));
        }
    }

    for name in &state.world.packages {
        match state.installed.packages.get(name) {
            None => errors.push(format!("world: {name} is not installed")),
            Some(package) if package.install_reason != InstallReason::Explicit => {
                errors.push(format!(
                    "world: {name} is installed as a dependency, not explicitly"
                ));
            }
            Some(_) => {}
        }
    }
    Ok(errors)
}
