//
//       user/spk/spk/src/repo.rs
//       Local repositories, index signatures and the trust store
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

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
}

impl LoadedRepo {
    pub fn package_path(&self, filename: &str) -> PathBuf {
        self.dir.join(filename)
    }
}

pub fn target_arch() -> &'static str {
    std::env::consts::ARCH
}

pub fn load_repos(paths: &Paths, urls: &[String]) -> Result<Vec<LoadedRepo>> {
    let mut chosen = urls.to_vec();
    if chosen.is_empty() {
        let state = crate::state::State::load(paths)?;
        for repo in state.repos.repos.iter().filter(|repo| repo.enabled) {
            chosen.extend(repo.urls.iter().cloned());
        }
    }
    if chosen.is_empty() {
        return Err(SpkError::General(
            "no repositories configured; pass --repo <path> or add a repository".into(),
        ));
    }
    let mut repos = Vec::new();
    for url in chosen {
        repos.push(load_repo(paths, &url)?);
    }
    Ok(repos)
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

pub fn load_repo(paths: &Paths, url: &str) -> Result<LoadedRepo> {
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
