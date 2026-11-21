//
//       user/spk/spk/src/repo.rs
//       Local repositories, index signatures and the trust store
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use spk_core::fsutil::atomic_write;
use spk_core::index::Index;
use spk_core::keys::{read_signature, PublicKey};

use crate::error::{Result, SpkError};
use crate::state::Paths;

#[derive(Clone, Debug)]
pub struct LoadedRepo {
    pub name: String,
    pub key_id: String,
    pub dir: PathBuf,
    pub index: Index,
    pub priority: i32,
    pub order: usize,
}

impl LoadedRepo {
    pub fn package_path(&self, filename: &str) -> PathBuf {
        self.dir.join(filename)
    }
}

pub fn target_arch() -> &'static str {
    std::env::consts::ARCH
}

pub fn load_repos(
    paths: &Paths,
    urls: &[String],
    lang: crate::i18n::Lang,
) -> Result<Vec<LoadedRepo>> {
    let mut repos = Vec::new();
    if urls.is_empty() {
        let state = crate::state::State::load(paths)?;
        let mut configs: Vec<_> = state
            .repos
            .repos
            .iter()
            .filter(|repo| repo.enabled)
            .enumerate()
            .collect();
        configs.sort_by(|a, b| b.1.priority.cmp(&a.1.priority).then_with(|| a.0.cmp(&b.0)));
        if configs.is_empty() {
            return Err(SpkError::General(
                "no repositories configured; pass --repo <path> or add a repository".into(),
            ));
        }
        for (order, (_, config)) in configs.into_iter().enumerate() {
            let mut loaded = None;
            for url in &config.urls {
                match load_repo(paths, url, config.priority, order) {
                    Ok(mut repo) => {
                        repo.name = config.name.clone();
                        loaded = Some(repo);
                        break;
                    }
                    Err(SpkError::Signature(error)) => {
                        return Err(SpkError::Signature(format!(
                            "repository '{}': {error}",
                            config.name
                        )))
                    }
                    Err(error) => {
                        eprintln!(
                            "{}",
                            crate::i18n::repo_mirror_unavailable(
                                lang,
                                &config.name,
                                url,
                                &error.to_string()
                            )
                        );
                    }
                }
            }
            match loaded {
                Some(repo) => repos.push(repo),
                None => eprintln!("{}", crate::i18n::repo_no_mirror(lang, &config.name)),
            }
        }
    } else {
        for (order, url) in urls.iter().enumerate() {
            match load_repo(paths, url, 0, order) {
                Ok(repo) => repos.push(repo),
                Err(SpkError::Signature(error)) => return Err(SpkError::Signature(error)),
                Err(error) => {
                    eprintln!(
                        "{}",
                        crate::i18n::repo_unavailable(lang, url, &error.to_string())
                    );
                }
            }
        }
        if repos.is_empty() {
            return Err(SpkError::General(
                "no usable repositories; check --repo paths and signatures".into(),
            ));
        }
    }
    validate_consistency(&repos)?;
    Ok(repos)
}

type PackageIdentity = (String, u64, String, u64, String);
type SeenPackage<'a> = (&'a str, &'a spk_core::index::IndexPackage);

fn validate_consistency(repos: &[LoadedRepo]) -> Result<()> {
    let mut seen: BTreeMap<PackageIdentity, SeenPackage> = BTreeMap::new();
    for repo in repos {
        for package in &repo.index.packages {
            let key = package.identity();
            if let Some((other_repo, other)) = seen.get(&key) {
                if package.sha256 != other.sha256
                    || package.size != other.size
                    || package.filename != other.filename
                    || package.abi != other.abi
                    || package.depends != other.depends
                    || package.provides != other.provides
                    || package.conflicts != other.conflicts
                    || package.replaces != other.replaces
                    || package.summary != other.summary
                {
                    return Err(SpkError::General(format!(
                        "package {} {} differs between repositories '{}' and '{}'",
                        package.name,
                        package.version(),
                        other_repo,
                        repo.name
                    )));
                }
            } else {
                seen.insert(key, (&repo.name, package));
            }
        }
    }
    Ok(())
}

pub fn local_path(url: &str) -> Result<PathBuf> {
    if let Some(path) = url.strip_prefix("file://") {
        if path.is_empty() {
            return Err(SpkError::General(format!("invalid repository URL '{url}'")));
        }
        return Ok(PathBuf::from(path));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Err(SpkError::General(format!(
            "remote repository '{url}' arrives in P4; use a local path or file:// for now"
        )));
    }
    Ok(PathBuf::from(url))
}

pub fn load_repo(paths: &Paths, url: &str, priority: i32, order: usize) -> Result<LoadedRepo> {
    let dir = local_path(url)?;
    let index_path = dir.join("index.json");
    let sig_path = dir.join("index.json.sig");
    let bytes = match fs::read(&index_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SpkError::General(format!(
                "WARNING: Ignoring {}: No such file or directory",
                index_path.display()
            )))
        }
        Err(error) => return Err(error.into()),
    };
    let signature = read_signature(&sig_path)?;
    let keys = trust_keys(paths)?;
    let key = keys
        .iter()
        .find(|key| key.key_id == signature.key_id)
        .ok_or_else(|| {
            SpkError::Signature(format!(
                "signature from {} is unknown trust",
                signature.key_id
            ))
        })?;
    key.verify(&bytes, &signature)?;
    let index = Index::from_slice(&bytes)?;
    if index.key_id != signature.key_id {
        return Err(SpkError::Signature(format!(
            "index key id {} does not match signature {}",
            index.key_id, signature.key_id
        )));
    }
    if index.arch != target_arch() {
        return Err(SpkError::General(format!(
            "repository '{}' is for architecture {}, expected {}",
            index.repo,
            index.arch,
            target_arch()
        )));
    }
    Ok(LoadedRepo {
        name: index.repo.clone(),
        key_id: index.key_id.clone(),
        dir,
        index,
        priority,
        order,
    })
}

pub fn trust_keys(paths: &Paths) -> Result<Vec<PublicKey>> {
    let entries = match fs::read_dir(&paths.trusted) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths_found: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".pub"))
        {
            paths_found.push(path);
        }
    }
    paths_found.sort();
    let mut keys = Vec::new();
    for path in paths_found {
        let key = PublicKey::from_file(&path)?;
        let expected = format!("{}.pub", key.key_id);
        let actual = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if actual != expected {
            return Err(SpkError::General(format!(
                "trusted key {} must be named {expected}",
                path.display()
            )));
        }
        keys.push(key);
    }
    Ok(keys)
}

pub fn trust_add(paths: &Paths, key_path: &Path) -> Result<PublicKey> {
    let key = PublicKey::from_file(key_path)?;
    paths.ensure_trust()?;
    let destination = paths.trusted.join(format!("{}.pub", key.key_id));
    let text = key.to_json()?;
    atomic_write(&destination, text.as_bytes(), 0o644)?;
    Ok(key)
}

pub fn trust_list(paths: &Paths) -> Result<Vec<PublicKey>> {
    trust_keys(paths)
}

pub fn add(paths: &Paths, name: &str, url: &str, priority: Option<i32>) -> Result<()> {
    spk_core::index::validate_repo_name(name)?;
    if url.trim().is_empty() {
        return Err(SpkError::Usage("repository URL must not be empty".into()));
    }
    let _lock = crate::state::lock_exclusive(paths)?;
    let mut state = crate::state::State::load(paths)?;
    if let Some(repo) = state.repos.repos.iter_mut().find(|repo| repo.name == name) {
        if !repo.urls.iter().any(|existing| existing == url) {
            repo.urls.push(url.to_string());
        }
        if let Some(priority) = priority {
            repo.priority = priority;
        }
        repo.enabled = true;
    } else {
        state.repos.repos.push(spk_core::db::RepoConfig {
            name: name.to_string(),
            arch: target_arch().to_string(),
            urls: vec![url.to_string()],
            priority: priority.unwrap_or(0),
            key_id: String::new(),
            last_sync: 0,
            enabled: true,
        });
    }
    write_repos(paths, &state.repos)
}

pub fn remove(paths: &Paths, name: &str) -> Result<()> {
    let _lock = crate::state::lock_exclusive(paths)?;
    let mut state = crate::state::State::load(paths)?;
    let before = state.repos.repos.len();
    state.repos.repos.retain(|repo| repo.name != name);
    if state.repos.repos.len() == before {
        return Err(SpkError::General(format!(
            "repository '{name}' is not configured"
        )));
    }
    write_repos(paths, &state.repos)
}

pub fn set_priority(paths: &Paths, name: &str, priority: i32) -> Result<()> {
    let _lock = crate::state::lock_exclusive(paths)?;
    let mut state = crate::state::State::load(paths)?;
    let repo = state
        .repos
        .repos
        .iter_mut()
        .find(|repo| repo.name == name)
        .ok_or_else(|| SpkError::General(format!("repository '{name}' is not configured")))?;
    repo.priority = priority;
    write_repos(paths, &state.repos)
}

pub fn list(paths: &Paths) -> Result<()> {
    let state = crate::state::State::load(paths)?;
    for repo in &state.repos.repos {
        println!(
            "{} priority={} arch={} enabled={}",
            repo.name, repo.priority, repo.arch, repo.enabled
        );
        for url in &repo.urls {
            println!("  {url}");
        }
    }
    Ok(())
}

fn write_repos(paths: &Paths, repos: &spk_core::db::ReposDb) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(repos)?;
    bytes.push(b'\n');
    spk_core::fsutil::atomic_write(&paths.db_path("repos.json"), &bytes, 0o644)?;
    Ok(())
}
