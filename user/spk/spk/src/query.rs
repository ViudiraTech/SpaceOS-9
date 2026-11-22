//
//       user/spk/spk/src/query.rs
//       list / info / files / owns
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeMap;

use serde::Serialize;
use spk_core::db::InstallReason;
use spk_core::manifest::FileType;
use spk_core::version::Version;

use crate::error::{Result, SpkError};
use crate::i18n::{self, Lang};
use crate::repo::{LoadedRepo, RepoLoadStatus};
use crate::state::State;
use crate::util::{format_time, human_size};

#[derive(Clone, Debug, Serialize)]
pub struct CatalogEntry {
    pub name: String,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
    pub upgrade_available: bool,
    pub held: bool,
    pub install_reason: Option<&'static str>,
    pub install_date: Option<u64>,
    pub installed_size: Option<u64>,
    pub file_count: Option<usize>,
    pub repo: Option<String>,
    pub arch: String,
    pub abi: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub filename: Option<String>,
    pub depends: Vec<String>,
    pub provides: Vec<String>,
    pub conflicts: Vec<String>,
    pub replaces: Vec<String>,
    pub summary: String,
}

pub fn build_catalog(state: &State, repos: &[LoadedRepo]) -> Vec<CatalogEntry> {
    let mut map: BTreeMap<String, CatalogEntry> = BTreeMap::new();
    for package in state.installed.packages.values() {
        let ver = Version::new(package.epoch, package.version.clone(), package.release);
        let size: u64 = package.files.iter().map(|file| file.size).sum();
        map.insert(
            package.name.clone(),
            CatalogEntry {
                name: package.name.clone(),
                installed: true,
                installed_version: Some(ver.to_string()),
                available_version: None,
                upgrade_available: false,
                held: state.holds.packages.contains_key(&package.name),
                install_reason: Some(match package.install_reason {
                    InstallReason::Explicit => "explicit",
                    InstallReason::Dependency => "dependency",
                }),
                install_date: Some(package.install_date),
                installed_size: Some(size),
                file_count: Some(package.files.len()),
                repo: None,
                arch: package.arch.clone(),
                abi: package.abi.clone(),
                size: 0,
                sha256: None,
                filename: None,
                depends: package.depends.clone(),
                provides: package.provides.clone(),
                conflicts: package.conflicts.clone(),
                replaces: package.replaces.clone(),
                summary: String::new(),
            },
        );
    }

    for repo in repos {
        for pkg in &repo.index.packages {
            if !repo.allows(pkg) {
                continue;
            }
            let avail_ver = pkg.version();
            if let Some(entry) = map.get_mut(&pkg.name) {
                if entry.repo.is_none() {
                    let installed_ver = state.installed.packages.get(&pkg.name).map(|inst| {
                        Version::new(inst.epoch, inst.version.clone(), inst.release)
                    });
                    entry.available_version = Some(avail_ver.to_string());
                    entry.upgrade_available =
                        installed_ver.as_ref().is_some_and(|inst| avail_ver > *inst);
                    entry.repo = Some(repo.name.clone());
                    entry.size = pkg.size;
                    entry.sha256 = Some(pkg.sha256.clone());
                    entry.filename = Some(pkg.filename.clone());
                    entry.depends = pkg.depends.clone();
                    entry.provides = pkg.provides.clone();
                    entry.conflicts = pkg.conflicts.clone();
                    entry.replaces = pkg.replaces.clone();
                    entry.summary = pkg.summary.clone();
                }
            } else {
                map.insert(
                    pkg.name.clone(),
                    CatalogEntry {
                        name: pkg.name.clone(),
                        installed: false,
                        installed_version: None,
                        available_version: Some(avail_ver.to_string()),
                        upgrade_available: false,
                        held: false,
                        install_reason: None,
                        install_date: None,
                        installed_size: None,
                        file_count: None,
                        repo: Some(repo.name.clone()),
                        arch: pkg.arch.clone(),
                        abi: pkg.abi.clone(),
                        size: pkg.size,
                        sha256: Some(pkg.sha256.clone()),
                        filename: Some(pkg.filename.clone()),
                        depends: pkg.depends.clone(),
                        provides: pkg.provides.clone(),
                        conflicts: pkg.conflicts.clone(),
                        replaces: pkg.replaces.clone(),
                        summary: pkg.summary.clone(),
                    },
                );
            }
        }
    }

    map.into_values().collect()
}

pub fn catalog(
    state: &State,
    repos: &[LoadedRepo],
    statuses: &[RepoLoadStatus],
    json: bool,
    lang: Lang,
) -> Result<()> {
    let entries = build_catalog(state, repos);
    if json {
        let updates: Vec<&str> = entries
            .iter()
            .filter(|e| e.upgrade_available)
            .map(|e| e.name.as_str())
            .collect();
        let store = repos.iter().find_map(|repo| repo.store.as_ref());
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "repositories": statuses,
                "packages": entries,
                "updates": updates,
                "store": store,
            }))?
        );
        return Ok(());
    }
    print_catalog_entries(&entries, lang);
    Ok(())
}

pub fn search(
    state: &State,
    repos: &[LoadedRepo],
    terms: &[String],
    json: bool,
    lang: Lang,
) -> Result<()> {
    let lowered: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
    let entries: Vec<CatalogEntry> = build_catalog(state, repos)
        .into_iter()
        .filter(|entry| {
            lowered.iter().all(|term| {
                entry.name.to_ascii_lowercase().contains(term)
                    || entry.summary.to_ascii_lowercase().contains(term)
                    || entry
                        .provides
                        .iter()
                        .any(|p| p.to_ascii_lowercase().contains(term))
            })
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "query": terms,
                "packages": entries,
            }))?
        );
        return Ok(());
    }
    print_catalog_entries(&entries, lang);
    Ok(())
}

fn print_catalog_entries(entries: &[CatalogEntry], lang: Lang) {
    if entries.is_empty() {
        println!(
            "{}",
            if lang.is_zh() {
                "没有匹配的软件包。"
            } else {
                "no matching packages."
            }
        );
        return;
    }
    for entry in entries {
        let ver = entry
            .available_version
            .as_deref()
            .or(entry.installed_version.as_deref())
            .unwrap_or("-");
        let tag = if entry.upgrade_available {
            if lang.is_zh() {
                format!(
                    " [已安装 {} -> 可升级 {}]",
                    entry.installed_version.as_deref().unwrap_or("?"),
                    ver
                )
            } else {
                format!(
                    " [installed {} -> upgrade {}]",
                    entry.installed_version.as_deref().unwrap_or("?"),
                    ver
                )
            }
        } else if entry.installed {
            if lang.is_zh() {
                " [已安装]".to_string()
            } else {
                " [installed]".to_string()
            }
        } else {
            String::new()
        };
        let hold_tag = if entry.held {
            if lang.is_zh() {
                " [已锁定]"
            } else {
                " [held]"
            }
        } else {
            ""
        };
        if entry.summary.is_empty() {
            println!("{} {ver}{tag}{hold_tag}", entry.name);
        } else {
            println!("{} {ver}{tag}{hold_tag} - {}", entry.name, entry.summary);
        }
    }
}

pub fn list(state: &State) -> Result<()> {
    list_with_json(state, false)
}

pub fn list_with_json(state: &State, json: bool) -> Result<()> {
    if json {
        let packages: Vec<serde_json::Value> = state
            .installed
            .packages
            .values()
            .map(|package| {
                let version = Version::new(package.epoch, package.version.clone(), package.release);
                let size: u64 = package.files.iter().map(|file| file.size).sum();
                serde_json::json!({
                    "name": package.name,
                    "version": version.to_string(),
                    "arch": package.arch,
                    "abi": package.abi,
                    "installed_size": size,
                    "file_count": package.files.len(),
                    "install_date": package.install_date,
                    "install_reason": match package.install_reason {
                        InstallReason::Explicit => "explicit",
                        InstallReason::Dependency => "dependency",
                    },
                    "held": state.holds.packages.contains_key(&package.name),
                    "depends": package.depends,
                    "provides": package.provides,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "packages": packages,
            }))?
        );
        return Ok(());
    }
    for package in state.installed.packages.values() {
        let version = Version::new(package.epoch, package.version.clone(), package.release);
        println!("{} {}", package.name, version);
    }
    Ok(())
}

pub fn info(state: &State, name: &str, lang: Lang) -> Result<()> {
    info_with_repos(state, &[], name, false, lang)
}

pub fn info_with_repos(
    state: &State,
    repos: &[LoadedRepo],
    name: &str,
    json: bool,
    lang: Lang,
) -> Result<()> {
    let entries = build_catalog(state, repos);
    let Some(entry) = entries.into_iter().find(|item| item.name == name) else {
        return Err(SpkError::General(i18n::not_installed(lang, name)));
    };
    if json {
        let store = repos.iter().find_map(|repo| {
            repo.store
                .as_ref()
                .and_then(|doc| doc.packages.get(name))
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "package": entry,
                "store": store,
            }))?
        );
        return Ok(());
    }
    row(i18n::name_label(lang), &entry.name);
    if let Some(ref inst) = entry.installed_version {
        row(i18n::version_label(lang), inst);
    }
    if let Some(ref avail) = entry.available_version {
        if entry.installed_version.as_deref() != Some(avail.as_str()) {
            row(
                if lang.is_zh() { "可用版本" } else { "Available" },
                avail,
            );
        }
    }
    row(i18n::arch_label(lang), &entry.arch);
    if let Some(ref repo) = entry.repo {
        row(if lang.is_zh() { "仓库" } else { "Repository" }, repo);
    }
    if !entry.summary.is_empty() {
        row(
            if lang.is_zh() { "简介" } else { "Summary" },
            &entry.summary,
        );
    }
    row(i18n::depends_label(lang), &join_or_none(&entry.depends, lang));
    row(i18n::provides_label(lang), &join_or_none(&entry.provides, lang));
    if entry.size > 0 {
        row(
            if lang.is_zh() {
                "下载大小"
            } else {
                "Download Size"
            },
            &human_size(entry.size),
        );
    }
    if let Some(inst_sz) = entry.installed_size {
        row(i18n::installed_size_label(lang), &human_size(inst_sz));
    }
    if let Some(date) = entry.install_date {
        row(i18n::install_date_label(lang), &format_time(date));
    }
    if let Some(reason_str) = entry.install_reason {
        let reason = if reason_str == "explicit" {
            i18n::explicit_reason(lang)
        } else {
            i18n::dependency_reason(lang)
        };
        row(i18n::install_reason_label(lang), reason);
    }
    if let Some(ref sha) = entry.sha256 {
        row("SHA-256", sha);
    }
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
    files_with_json(state, name, false, lang)
}

pub fn files_with_json(state: &State, name: &str, json: bool, lang: Lang) -> Result<()> {
    let package = state
        .installed
        .packages
        .get(name)
        .ok_or_else(|| SpkError::General(i18n::not_installed(lang, name)))?;
    let mut entries = package.files.clone();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    if json {
        let rows: Vec<serde_json::Value> = entries
            .iter()
            .map(|f| {
                serde_json::json!({
                    "path": f.path,
                    "type": match f.file_type {
                        FileType::File => "file",
                        FileType::Dir => "dir",
                        FileType::Symlink => "symlink",
                    },
                    "mode": f.mode,
                    "size": f.size,
                    "target": f.target,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "package": name,
                "files": rows,
            }))?
        );
        return Ok(());
    }
    for file in &entries {
        println!("{name} {}", file.path);
    }
    Ok(())
}

pub fn owns(state: &State, path: &str, lang: Lang) -> Result<()> {
    owns_with_json(state, path, false, lang)
}

pub fn owns_with_json(state: &State, path: &str, json: bool, lang: Lang) -> Result<()> {
    if !path.starts_with('/') {
        return Err(SpkError::Usage(format!(
            "'{path}' must be an absolute path"
        )));
    }
    let owners: Vec<&str> = if let Some(record) = state.files.files.get(path) {
        vec![record.pkg.as_str()]
    } else {
        let mut matches: Vec<&str> = state
            .installed
            .packages
            .values()
            .filter(|package| package.files.iter().any(|file| file.path == path))
            .map(|package| package.name.as_str())
            .collect();
        matches.sort();
        matches
    };
    if owners.is_empty() {
        return Err(SpkError::General(i18n::no_owner(lang, path)));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "path": path,
                "owners": owners,
            }))?
        );
        return Ok(());
    }
    for owner in owners {
        println!("{owner}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityState;
    use spk_core::db::{
        FileDb, HeldPackage, HoldsDb, InstalledPackage, InstalledDb, ReposDb, WorldDb,
    };
    use spk_core::index::{Index, IndexPackage};
    use std::path::PathBuf;

    fn sample_repo(pkgs: Vec<IndexPackage>) -> LoadedRepo {
        let mut index = Index::new("spaceos-main", "x86_64", 100, "ed25519:0123456789abcdef");
        index.packages = pkgs;
        let security = SecurityState::from_index(&index);
        LoadedRepo {
            name: "spaceos-main".into(),
            key_id: index.key_id.clone(),
            dir: PathBuf::from("/tmp/repo"),
            index,
            priority: 10,
            order: 0,
            security,
            mirrors: vec![],
            package_cache: None,
            offline: true,
            allow_unsigned: true,
            manifest: None,
            history: BTreeMap::new(),
            store: None,
            stale: false,
        }
    }

    fn idx_pkg(name: &str, ver: &str, release: u64, provides: &[&str], summary: &str) -> IndexPackage {
        IndexPackage {
            name: name.into(),
            epoch: 0,
            version: ver.into(),
            release,
            arch: "x86_64".into(),
            abi: "spaceos-1".into(),
            filename: format!("{name}-{ver}-{release}-x86_64.spk"),
            size: 4096,
            sha256: "a".repeat(64),
            depends: vec![],
            provides: provides.iter().map(|s| (*s).to_string()).collect(),
            conflicts: vec![],
            replaces: vec![],
            summary: summary.into(),
        }
    }

    #[test]
    fn build_catalog_merges_installed_updates_holds_and_remote() {
        let mut installed = InstalledDb::default();
        installed.packages.insert(
            "theme-winxp".into(),
            InstalledPackage {
                name: "theme-winxp".into(),
                epoch: 0,
                version: "1.0.0".into(),
                release: 4,
                arch: "x86_64".into(),
                abi: "spaceos-1".into(),
                files: vec![],
                depends: vec![],
                provides: vec!["spui-theme".into()],
                conflicts: vec![],
                replaces: vec![],
                conffiles: vec![],
                scripts: true,
                triggers: vec![],
                install_script: None,
                allow_setuid: false,
                install_date: 12345,
                install_reason: InstallReason::Explicit,
                txid: "tx1".into(),
            },
        );
        let mut holds = HoldsDb::default();
        holds.packages.insert(
            "theme-winxp".into(),
            HeldPackage {
                epoch: 0,
                version: "1.0.0".into(),
                release: 4,
            },
        );
        let state = State {
            installed,
            files: FileDb::default(),
            world: WorldDb::default(),
            repos: ReposDb::default(),
            holds,
        };
        let repo = sample_repo(vec![
            idx_pkg("theme-winxp", "1.0.0", 5, &["spui-theme"], "XP Luna theme"),
            idx_pkg("theme-aqua", "1.0.0", 2, &["spui-theme"], "Aqua Classic theme"),
        ]);

        let cat = build_catalog(&state, &[repo]);
        assert_eq!(cat.len(), 2);

        let xp = cat.iter().find(|p| p.name == "theme-winxp").unwrap();
        assert!(xp.installed);
        assert_eq!(xp.installed_version.as_deref(), Some("1.0.0-4"));
        assert_eq!(xp.available_version.as_deref(), Some("1.0.0-5"));
        assert!(xp.upgrade_available);
        assert!(xp.held);
        assert_eq!(xp.repo.as_deref(), Some("spaceos-main"));

        let aqua = cat.iter().find(|p| p.name == "theme-aqua").unwrap();
        assert!(!aqua.installed);
        assert!(aqua.installed_version.is_none());
        assert_eq!(aqua.available_version.as_deref(), Some("1.0.0-2"));
        assert!(!aqua.upgrade_available);
        assert!(!aqua.held);
    }

    #[test]
    fn json_progress_helper_respects_env_and_offline_non_root_catalog() {
        let dir = std::env::temp_dir().join(format!("spk_catalog_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("var/lib/spk")).unwrap();
        let paths = crate::state::Paths::new(&dir).unwrap();
        let mut state = State::load(&paths).unwrap();
        state.repos.repos.push(spk_core::db::RepoConfig {
            name: "remote-main".into(),
            arch: crate::repo::target_arch().into(),
            urls: vec!["https://example.invalid/repo/x86_64".into()],
            priority: 10,
            key_id: String::new(),
            last_sync: 0,
            enabled: true,
        });
        let bytes = serde_json::to_vec_pretty(&state.repos).unwrap();
        std::fs::write(paths.db_path("repos.json"), bytes).unwrap();

        let (repos, statuses) = crate::repo::load_repos_best_effort(
            &paths,
            &[],
            Lang::ZhCn,
            crate::remote::RemoteOptions {
                offline: true,
                allow_unsigned: false,
                refresh: false,
            },
        )
        .unwrap();
        assert!(repos.is_empty());
        assert_eq!(statuses.len(), 1);
        assert!(!statuses[0].synced);
        assert!(!paths.package_cache.exists());

        // Non-root dry-run remove preview should not require root or lock.
        let opts = crate::ops::ReportOptions {
            json: true,
            print: true,
            offline: true,
            ..Default::default()
        };
        assert!(crate::ops::upgrade(&paths, &[], &[], false, false, true, &[], opts, Lang::ZhCn).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
