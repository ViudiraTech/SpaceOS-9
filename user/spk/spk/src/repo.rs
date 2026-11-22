//
//       user/spk/spk/src/repo.rs
//       Local repositories, index signatures and the trust store
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use spk_core::fsutil::atomic_write;
use spk_core::index::{history_path, FileDigest, Index, IndexPackage, RepoManifest};
use spk_core::keys::{read_signature, verify_any, PublicKey, PublicKeyFile, SignatureFile};

use crate::error::{Result, SpkError};
use crate::security::SecurityState;
use crate::state::Paths;

#[derive(Clone, Debug)]
pub struct LoadedRepo {
    pub name: String,
    pub key_id: String,
    /// Local repository directory, or the cached snapshot directory for a
    /// remote repository.
    pub dir: PathBuf,
    pub index: Index,
    pub priority: i32,
    pub order: usize,
    pub security: SecurityState,
    /// Base URLs for remote mirrors; empty for a local repository.
    pub mirrors: Vec<String>,
    /// Content-addressed package cache, present for remote repositories.
    pub package_cache: Option<PathBuf>,
    pub offline: bool,
    pub allow_unsigned: bool,
    /// Schema 2 repository manifest, loaded on demand.
    pub manifest: Option<RepoManifest>,
    /// Versions fetched from the per-package history files.
    pub history: BTreeMap<String, Vec<IndexPackage>>,
    /// Presentation and editorial metadata published by the repository.
    pub store: Option<spk_core::store::StoreDoc>,
    pub stale: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct RepoLoadStatus {
    pub name: String,
    pub arch: String,
    pub urls: Vec<String>,
    pub priority: i32,
    pub enabled: bool,
    pub last_sync: u64,
    pub key_id: String,
    pub synced: bool,
    pub stale: bool,
    pub package_count: usize,
    pub generated_at: u64,
    pub error: Option<String>,
}

impl LoadedRepo {
    pub fn package_path(&self, package: &spk_core::index::IndexPackage) -> PathBuf {
        match &self.package_cache {
            Some(cache) => cache.join(format!("{}.spk", package.sha256)),
            None => self.dir.join(&package.filename),
        }
    }

    pub fn allows(&self, package: &spk_core::index::IndexPackage) -> bool {
        self.security.reason(package).is_none()
    }

    pub fn rejection(&self, package: &spk_core::index::IndexPackage) -> Option<String> {
        self.security.reason(package)
    }

    pub fn ensure_package(&self, package: &spk_core::index::IndexPackage) -> Result<PathBuf> {
        self.ensure_package_with_progress(package, &mut |_| {}, crate::i18n::Lang::En)
    }

    pub fn ensure_package_with_progress(
        &self,
        package: &spk_core::index::IndexPackage,
        progress: &mut dyn FnMut(u64),
        lang: crate::i18n::Lang,
    ) -> Result<PathBuf> {
        match &self.package_cache {
            Some(cache) => crate::remote::ensure_package_with_progress(
                cache,
                &self.mirrors,
                package,
                self.offline,
                progress,
                lang,
            ),
            None => {
                let path = self.dir.join(&package.filename);
                if !path.is_file() {
                    return Err(SpkError::General(format!(
                        "repository file {} is missing",
                        path.display()
                    )));
                }
                progress(package.size);
                Ok(path)
            }
        }
    }

    /// Load the full version history of the named packages. Only explicitly
    /// pinned versions (and older installed versions) need this: the catalog
    /// already carries the newest release of every package.
    pub fn ensure_history(&mut self, paths: &Paths, names: &BTreeSet<String>) -> Result<()> {
        if names.is_empty() {
            return Ok(());
        }
        let manifest = match self.manifest.clone() {
            Some(manifest) => manifest,
            None => {
                let loaded = crate::remote::load_manifest(paths, self)?;
                self.manifest = loaded.clone();
                match loaded {
                    Some(manifest) => manifest,
                    // Schema 1 repository: every version is already in the index.
                    None => return Ok(()),
                }
            }
        };
        let mut changed = false;
        for name in names {
            if self.history.contains_key(name) {
                continue;
            }
            let Some(digest) = manifest.history.get(name).cloned() else {
                continue;
            };
            let versions = self.read_history(name, &digest)?;
            for package in versions.clone() {
                if !self
                    .index
                    .packages
                    .iter()
                    .any(|known| known.identity() == package.identity())
                {
                    self.index.packages.push(package);
                    changed = true;
                }
            }
            self.history.insert(name.clone(), versions);
        }
        if changed {
            self.index.packages.sort_by(|a, b| {
                a.name
                    .cmp(&b.name)
                    .then_with(|| a.version().cmp(&b.version()))
            });
        }
        Ok(())
    }

    fn read_history(&self, name: &str, digest: &FileDigest) -> Result<Vec<IndexPackage>> {
        let relative = history_path(name);
        let cached = self.dir.join(&relative);
        let bytes = match fs::read(&cached) {
            Ok(bytes)
                if bytes.len() as u64 == digest.size
                    && spk_core::fsutil::sha256_bytes(&bytes) == digest.sha256 =>
            {
                bytes
            }
            _ => {
                let bytes = crate::remote::fetch_history(self, &relative)?;
                if bytes.len() as u64 != digest.size
                    || spk_core::fsutil::sha256_bytes(&bytes) != digest.sha256
                {
                    return Err(SpkError::Security(format!(
                        "{name}: history file does not match the signed repository manifest"
                    )));
                }
                if let Some(parent) = cached.parent() {
                    fs::create_dir_all(parent)?;
                }
                atomic_write(&cached, &bytes, 0o644)?;
                bytes
            }
        };
        Ok(spk_core::index::history_from_slice(
            &bytes,
            &self.index.arch,
        )?)
    }
}

/// Verify a detached signature over an index using any trusted, non-revoked
/// key. Missing signatures are accepted only with `--allow-unsigned`.
pub fn verify_index_bytes(
    paths: &Paths,
    bytes: &[u8],
    signature: Option<&SignatureFile>,
    allow_unsigned: bool,
) -> Result<String> {
    let signature = match signature {
        Some(signature) => signature,
        None if allow_unsigned => {
            eprintln!("warning: --allow-unsigned: index has no signature");
            return Ok(String::new());
        }
        None => {
            return Err(SpkError::Signature(
                "repository index has no signature".into(),
            ))
        }
    };
    let keys = trust_keys(paths)?;
    if keys.is_empty() {
        return Err(SpkError::Signature(
            "no trusted repository keys are configured".into(),
        ));
    }
    let revoked = revoked_keys(paths)?;
    let key_id = verify_any(bytes, signature, &keys, &revoked)?;
    if signature.schema != spk_core::keys::SIGNATURE_SCHEMA {
        return Err(SpkError::Signature("unsupported signature schema".into()));
    }
    Ok(key_id)
}

pub fn revoked_keys(paths: &Paths) -> Result<BTreeSet<String>> {
    let entries = match fs::read_dir(&paths.revoked) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.into()),
    };
    let mut revoked = BTreeSet::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let key_id = name.strip_suffix(".revoked").unwrap_or(name);
        if spk_core::keys::is_valid_key_id(key_id) {
            revoked.insert(key_id.to_string());
        }
    }
    Ok(revoked)
}

pub fn target_arch() -> &'static str {
    std::env::consts::ARCH
}

pub fn load_repos(
    paths: &Paths,
    urls: &[String],
    lang: crate::i18n::Lang,
    remote_options: crate::remote::RemoteOptions,
) -> Result<Vec<LoadedRepo>> {
    let (repos, _) = load_repos_inner(paths, urls, lang, remote_options, true)?;
    Ok(repos)
}

pub fn load_repos_best_effort(
    paths: &Paths,
    urls: &[String],
    lang: crate::i18n::Lang,
    remote_options: crate::remote::RemoteOptions,
) -> Result<(Vec<LoadedRepo>, Vec<RepoLoadStatus>)> {
    load_repos_inner(paths, urls, lang, remote_options, false)
}

fn load_repos_inner(
    paths: &Paths,
    urls: &[String],
    lang: crate::i18n::Lang,
    remote_options: crate::remote::RemoteOptions,
    strict: bool,
) -> Result<(Vec<LoadedRepo>, Vec<RepoLoadStatus>)> {
    let mut repos = Vec::new();
    let mut statuses = Vec::new();
    if urls.is_empty() {
        let state = crate::state::State::load(paths)?;
        let mut all_configs: Vec<_> = state.repos.repos.iter().enumerate().collect();
        all_configs.sort_by(|a, b| b.1.priority.cmp(&a.1.priority).then_with(|| a.0.cmp(&b.0)));
        if strict && all_configs.iter().all(|(_, repo)| !repo.enabled) {
            return Err(SpkError::General(
                "no repositories configured; pass --repo <path> or add a repository".into(),
            ));
        }
        for (order, (_, config)) in all_configs.into_iter().enumerate() {
            if !config.enabled {
                statuses.push(RepoLoadStatus {
                    name: config.name.clone(),
                    arch: config.arch.clone(),
                    urls: config.urls.clone(),
                    priority: config.priority,
                    enabled: false,
                    last_sync: config.last_sync,
                    key_id: config.key_id.clone(),
                    synced: false,
                    stale: false,
                    package_count: 0,
                    generated_at: 0,
                    error: None,
                });
                continue;
            }
            let remote_urls: Vec<String> = config
                .urls
                .iter()
                .filter(|url| is_remote_url(url))
                .cloned()
                .collect();
            if !remote_urls.is_empty() {
                match load_remote_repo(
                    paths,
                    &config.name,
                    &remote_urls,
                    config.priority,
                    order,
                    remote_options,
                    lang,
                ) {
                    Ok(repo) => {
                        statuses.push(RepoLoadStatus {
                            name: config.name.clone(),
                            arch: repo.index.arch.clone(),
                            urls: config.urls.clone(),
                            priority: config.priority,
                            enabled: true,
                            last_sync: config.last_sync,
                            key_id: repo.key_id.clone(),
                            synced: true,
                            stale: repo.stale,
                            package_count: repo.index.packages.len(),
                            generated_at: repo.index.generated_at,
                            error: None,
                        });
                        repos.push(repo);
                    }
                    Err(error @ SpkError::Signature(_)) if strict => return Err(error),
                    Err(SpkError::Security(error)) if strict => return Err(SpkError::Security(error)),
                    Err(error) => {
                        let msg = error.to_string();
                        if strict {
                            eprintln!(
                                "{}",
                                crate::i18n::repo_mirror_unavailable(
                                    lang,
                                    &config.name,
                                    &remote_urls.join(", "),
                                    &msg
                                )
                            );
                        }
                        statuses.push(RepoLoadStatus {
                            name: config.name.clone(),
                            arch: config.arch.clone(),
                            urls: config.urls.clone(),
                            priority: config.priority,
                            enabled: true,
                            last_sync: config.last_sync,
                            key_id: config.key_id.clone(),
                            synced: false,
                            stale: true,
                            package_count: 0,
                            generated_at: 0,
                            error: Some(msg),
                        });
                    }
                }
                continue;
            }
            let mut loaded = None;
            let mut last_err = None;
            for url in &config.urls {
                match load_repo(
                    paths,
                    url,
                    config.priority,
                    order,
                    remote_options.allow_unsigned,
                ) {
                    Ok(mut repo) => {
                        repo.name = config.name.clone();
                        loaded = Some(repo);
                        break;
                    }
                    Err(SpkError::Signature(error)) if strict => {
                        return Err(SpkError::Signature(format!(
                            "repository '{}': {error}",
                            config.name
                        )))
                    }
                    Err(error) => {
                        let msg = error.to_string();
                        if strict {
                            eprintln!(
                                "{}",
                                crate::i18n::repo_mirror_unavailable(
                                    lang,
                                    &config.name,
                                    url,
                                    &msg
                                )
                            );
                        }
                        last_err = Some(msg);
                    }
                }
            }
            match loaded {
                Some(repo) => {
                    statuses.push(RepoLoadStatus {
                        name: config.name.clone(),
                        arch: repo.index.arch.clone(),
                        urls: config.urls.clone(),
                        priority: config.priority,
                        enabled: true,
                        last_sync: config.last_sync,
                        key_id: repo.key_id.clone(),
                        synced: true,
                        stale: false,
                        package_count: repo.index.packages.len(),
                        generated_at: repo.index.generated_at,
                        error: None,
                    });
                    repos.push(repo);
                }
                None => {
                    if strict {
                        eprintln!("{}", crate::i18n::repo_no_mirror(lang, &config.name));
                    }
                    statuses.push(RepoLoadStatus {
                        name: config.name.clone(),
                        arch: config.arch.clone(),
                        urls: config.urls.clone(),
                        priority: config.priority,
                        enabled: true,
                        last_sync: config.last_sync,
                        key_id: config.key_id.clone(),
                        synced: false,
                        stale: true,
                        package_count: 0,
                        generated_at: 0,
                        error: last_err,
                    });
                }
            }
        }
    } else {
        for (order, url) in urls.iter().enumerate() {
            let result = if is_remote_url(url) {
                load_remote_repo(
                    paths,
                    "",
                    std::slice::from_ref(url),
                    0,
                    order,
                    remote_options,
                    lang,
                )
            } else {
                load_repo(paths, url, 0, order, remote_options.allow_unsigned)
            };
            match result {
                Ok(repo) => {
                    statuses.push(RepoLoadStatus {
                        name: repo.name.clone(),
                        arch: repo.index.arch.clone(),
                        urls: vec![url.clone()],
                        priority: 0,
                        enabled: true,
                        last_sync: 0,
                        key_id: repo.key_id.clone(),
                        synced: true,
                        stale: repo.stale,
                        package_count: repo.index.packages.len(),
                        generated_at: repo.index.generated_at,
                        error: None,
                    });
                    repos.push(repo);
                }
                Err(SpkError::Signature(error)) if strict => return Err(SpkError::Signature(error)),
                Err(SpkError::Security(error)) if strict => return Err(SpkError::Security(error)),
                Err(error) => {
                    let msg = error.to_string();
                    if strict {
                        eprintln!(
                            "{}",
                            crate::i18n::repo_unavailable(lang, url, &msg)
                        );
                    }
                    statuses.push(RepoLoadStatus {
                        name: url.clone(),
                        arch: target_arch().to_string(),
                        urls: vec![url.clone()],
                        priority: 0,
                        enabled: true,
                        last_sync: 0,
                        key_id: String::new(),
                        synced: false,
                        stale: true,
                        package_count: 0,
                        generated_at: 0,
                        error: Some(msg),
                    });
                }
            }
        }
        if strict && repos.is_empty() {
            return Err(SpkError::General(
                "no usable repositories; check --repo paths and signatures".into(),
            ));
        }
    }
    if strict {
        validate_consistency(&repos)?;
    }
    Ok((repos, statuses))
}

pub fn is_remote_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn load_remote_repo(
    paths: &Paths,
    name_hint: &str,
    urls: &[String],
    priority: i32,
    order: usize,
    options: crate::remote::RemoteOptions,
    lang: crate::i18n::Lang,
) -> Result<LoadedRepo> {
    let synced = crate::remote::sync_repo(paths, name_hint, urls, options, lang)?;
    Ok(LoadedRepo {
        name: if name_hint.is_empty() {
            synced.index.repo.clone()
        } else {
            name_hint.to_string()
        },
        key_id: synced.key_id,
        dir: synced.dir,
        index: synced.index,
        priority,
        order,
        security: synced.security,
        mirrors: synced.mirrors,
        package_cache: Some(paths.package_cache.clone()),
        offline: options.offline,
        allow_unsigned: options.allow_unsigned,
        manifest: None,
        history: BTreeMap::new(),
        store: synced.store,
        stale: synced.stale,
    })
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
    Ok(PathBuf::from(url))
}

pub fn load_repo(
    paths: &Paths,
    url: &str,
    priority: i32,
    order: usize,
    allow_unsigned: bool,
) -> Result<LoadedRepo> {
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
    let signature = if sig_path.is_file() {
        Some(read_signature(&sig_path)?)
    } else {
        None
    };
    let key_id = verify_index_bytes(paths, &bytes, signature.as_ref(), allow_unsigned)?;
    let index = Index::from_slice(&bytes)?;
    if let Some(signature) = &signature {
        if index.key_id != signature.key_id {
            return Err(SpkError::Signature(format!(
                "index key id {} does not match primary signature {}",
                index.key_id, signature.key_id
            )));
        }
    }
    if index.arch != target_arch() {
        return Err(SpkError::General(format!(
            "repository '{}' is for architecture {}, expected {}",
            index.repo,
            index.arch,
            target_arch()
        )));
    }
    let security = SecurityState::from_index(&index);
    let store = fs::read(dir.join("store.json"))
        .ok()
        .and_then(|bytes| spk_core::store::StoreDoc::from_slice(&bytes).ok());
    Ok(LoadedRepo {
        name: index.repo.clone(),
        key_id,
        dir,
        index,
        priority,
        order,
        security,
        mirrors: Vec::new(),
        package_cache: None,
        offline: false,
        allow_unsigned,
        manifest: None,
        history: BTreeMap::new(),
        store,
        stale: false,
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustBundle {
    schema: u32,
    keys: Vec<PublicKeyFile>,
}

pub fn trust_keys(paths: &Paths) -> Result<Vec<PublicKey>> {
    let entries = match fs::read_dir(&paths.trusted) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return trust_bundle(paths),
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
    if keys.is_empty() {
        return trust_bundle(paths);
    }
    Ok(keys)
}

fn trust_bundle(paths: &Paths) -> Result<Vec<PublicKey>> {
    let bytes = match fs::read(&paths.trusted_bundle) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let bundle: TrustBundle = serde_json::from_slice(&bytes)?;
    if bundle.schema != 1 {
        return Err(SpkError::General(format!(
            "{}: unsupported trust bundle schema {}",
            paths.trusted_bundle.display(),
            bundle.schema
        )));
    }
    let mut keys = Vec::with_capacity(bundle.keys.len());
    for file in bundle.keys {
        keys.push(PublicKey::from_public_file(&file)?);
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
    if !url.starts_with("http://")
        && !url.starts_with("https://")
        && !url.starts_with("file://")
        && !url.starts_with('/')
    {
        return Err(SpkError::Usage(format!(
            "repository URL must be http(s), file:// or an absolute path: {url}"
        )));
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
    list_with_json(paths, false)
}

pub fn list_with_json(paths: &Paths, json: bool) -> Result<()> {
    let state = crate::state::State::load(paths)?;
    if json {
        let rows: Vec<serde_json::Value> = state
            .repos
            .repos
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "arch": r.arch,
                    "urls": r.urls,
                    "priority": r.priority,
                    "key_id": r.key_id,
                    "last_sync": r.last_sync,
                    "enabled": r.enabled,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "repositories": rows,
            }))?
        );
        return Ok(());
    }
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

pub fn add_with_json(
    paths: &Paths,
    name: &str,
    url: &str,
    priority: Option<i32>,
    json: bool,
) -> Result<()> {
    add(paths, name, url, priority)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "ok": true,
                "action": "add",
                "name": name,
                "url": url,
            }))?
        );
    }
    Ok(())
}

pub fn remove_with_json(paths: &Paths, name: &str, json: bool) -> Result<()> {
    remove(paths, name)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "ok": true,
                "action": "remove",
                "name": name,
            }))?
        );
    }
    Ok(())
}

pub fn set_priority_with_json(paths: &Paths, name: &str, priority: i32, json: bool) -> Result<()> {
    set_priority(paths, name, priority)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": 1,
                "ok": true,
                "action": "priority",
                "name": name,
                "priority": priority,
            }))?
        );
    }
    Ok(())
}

fn write_repos(paths: &Paths, repos: &spk_core::db::ReposDb) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(repos)?;
    bytes.push(b'\n');
    spk_core::fsutil::atomic_write(&paths.db_path("repos.json"), &bytes, 0o644)?;
    Ok(())
}
