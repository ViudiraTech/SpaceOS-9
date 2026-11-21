//
//       user/spk/spk/src/query.rs
//       list / info / files / owns
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use spk_core::db::InstallReason;
use spk_core::version::Version;

use crate::error::{Result, SpkError};
use crate::i18n::{self, Lang};
use crate::state::State;
use crate::util::{format_time, human_size};

pub fn list(state: &State) -> Result<()> {
    for package in state.installed.packages.values() {
        let version = Version::new(package.epoch, package.version.clone(), package.release);
        println!("{} {}", package.name, version);
    }
    Ok(())
}

pub fn info(state: &State, name: &str, lang: Lang) -> Result<()> {
    let package = state
        .installed
        .packages
        .get(name)
        .ok_or_else(|| SpkError::General(i18n::not_installed(lang, name)))?;
    let version = Version::new(package.epoch, package.version.clone(), package.release);
    let size: u64 = package.files.iter().map(|file| file.size).sum();
    let depends = join_or_none(&package.depends, lang);
    let provides = join_or_none(&package.provides, lang);
    let reason = match package.install_reason {
        InstallReason::Explicit => i18n::explicit_reason(lang),
        InstallReason::Dependency => i18n::dependency_reason(lang),
    };
    row(i18n::name_label(lang), &package.name);
    row(i18n::version_label(lang), &version.to_string());
    row(i18n::arch_label(lang), &package.arch);
    row(i18n::depends_label(lang), &depends);
    row(i18n::provides_label(lang), &provides);
    row(i18n::installed_size_label(lang), &human_size(size));
    row(
        i18n::install_date_label(lang),
        &format_time(package.install_date),
    );
    row(i18n::install_reason_label(lang), reason);
    Ok(())
}

fn row(label: &str, value: &str) {
    println!("{label:<16}: {value}");
}

fn join_or_none(items: &[String], lang: Lang) -> String {
    if items.is_empty() {
        i18n::none(lang).to_string()
    } else {
        items.join("  ")
    }
}

pub fn files(state: &State, name: &str, lang: Lang) -> Result<()> {
    let package = state
        .installed
        .packages
        .get(name)
        .ok_or_else(|| SpkError::General(i18n::not_installed(lang, name)))?;
    let mut paths: Vec<&String> = package.files.iter().map(|file| &file.path).collect();
    paths.sort();
    for path in paths {
        println!("{name} {path}");
    }
    Ok(())
}

pub fn owns(state: &State, path: &str, lang: Lang) -> Result<()> {
    if !path.starts_with('/') {
        return Err(SpkError::Usage(format!(
            "'{path}' must be an absolute path"
        )));
    }
    if let Some(record) = state.files.files.get(path) {
        println!("{}", record.pkg);
        return Ok(());
    }
    let mut owners: Vec<&str> = state
        .installed
        .packages
        .values()
        .filter(|package| package.files.iter().any(|file| file.path == path))
        .map(|package| package.name.as_str())
        .collect();
    owners.sort();
    if owners.is_empty() {
        return Err(SpkError::General(i18n::no_owner(lang, path)));
    }
    for owner in owners {
        println!("{owner}");
    }
    Ok(())
}
