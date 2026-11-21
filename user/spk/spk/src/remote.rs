//
//       user/spk/spk/src/remote.rs
//       Remote repository index snapshots, package cache and Range downloads
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spk_core::fsutil::{sha256_bytes, sha256_file};
use spk_core::index::{Index, IndexPackage};
use spk_core::keys::{read_signature, SignatureFile};

use crate::error::{Result, SpkError};
use crate::http::{self, HttpClient};
use crate::i18n::{self, Lang};
use crate::repo;
use crate::security::SecurityState;
use crate::state::Paths;

const MAX_INDEX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 256 * 1024;
const RETRIES: usize = 3;
const RETENTION_DAYS: u64 = 30;
const CACHE_LIMIT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct RemoteOptions {
    pub offline: bool,
    pub allow_unsigned: bool,
}

#[derive(Clone, Debug)]
pub struct RemoteSync {
    pub dir: PathBuf,
    pub key_id: String,
    pub index: Index,
    pub security: SecurityState,
    pub mirrors: Vec<String>,
    pub stale: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotMeta {
    #[serde(default)]
    schema: u32,
    url: String,
    #[serde(default)]
    etag: String,
    #[serde(default)]
    last_modified: String,
    sha256: String,
    size: u64,
    #[serde(default)]
    fetched_at: u64,
}

#[derive(Clone, Debug)]
struct CachedSnapshot {
    dir: PathBuf,
    index: Index,
    meta: SnapshotMeta,
    key_id: String,
}

pub fn repo_id(name_hint: &str, urls: &[String]) -> String {
    let hint: String = name_hint
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if !hint.is_empty() {
        return hint;
    }
    let first = urls.first().map(String::as_str).unwrap_or("unknown");
    let digest = Sha256::digest(first.as_bytes());
    format!("url-{}", hex::encode(&digest[..8]))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn retryable_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

fn get(
    client: &HttpClient,
    url: &str,
    headers: &[(&str, String)],
    max_body: u64,
) -> Result<http::HttpResponse> {
    let mut last = None;
    for attempt in 0..RETRIES {
        match client.get(url, headers, max_body) {
            Ok(response) => {
                if retryable_status(response.status) && attempt + 1 < RETRIES {
                    last = Some(SpkError::Network(format!(
                        "HTTP {} from {}",
                        response.status, url
                    )));
                    std::thread::sleep(Duration::from_millis(200u64 << attempt));
                    continue;
                }
                return Ok(response);
            }
            Err(error) => {
                last = Some(error);
                if attempt + 1 < RETRIES {
                    std::thread::sleep(Duration::from_millis(200u64 << attempt));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| SpkError::Network(format!("request failed: {url}"))))
}

fn load_current(
    paths: &Paths,
    root: &Path,
    options: RemoteOptions,
) -> Result<Option<CachedSnapshot>> {
    let current = root.join("current");
    let sha = match fs::read_to_string(&current) {
        Ok(text) => text.trim().to_string(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SpkError::Security(format!(
            "{}: malformed current snapshot pointer",
            current.display()
        )));
    }
    let dir = root.join("snapshots").join(&sha);
    let index_bytes = fs::read(dir.join("index.json"))?;
    let signature_path = dir.join("index.json.sig");
    let signature = if signature_path.is_file() {
        Some(read_signature(&signature_path)?)
    } else {
        None
    };
    let key_id = repo::verify_index_bytes(
        paths,
        &index_bytes,
        signature.as_ref(),
        options.allow_unsigned,
    )?;
    let index = Index::from_slice(&index_bytes)?;
    let meta_bytes = fs::read(dir.join("meta.json"))?;
    let meta: SnapshotMeta = serde_json::from_slice(&meta_bytes)?;
    Ok(Some(CachedSnapshot {
        dir,
        index,
        meta,
        key_id,
    }))
}

fn mirror_index_url(base: &str) -> String {
    http::join_url(base, "index.json")
}

fn mirror_signature_url(base: &str) -> String {
    http::join_url(base, "index.json.sig")
}

fn write_snapshot(
    paths: &Paths,
    root: &Path,
    bytes: &[u8],
    signature: Option<&SignatureFile>,
    meta: &SnapshotMeta,
    security: &SecurityState,
) -> Result<PathBuf> {
    let sha = sha256_bytes(bytes);
    let snapshots = root.join("snapshots");
    let dir = snapshots.join(&sha);
    fs::create_dir_all(&dir)?;
    spk_core::fsutil::atomic_write(&dir.join("index.json"), bytes, 0o644)?;
    match signature {
        Some(signature) => {
            let mut json = serde_json::to_vec_pretty(signature)?;
            json.push(b'\n');
            spk_core::fsutil::atomic_write(&dir.join("index.json.sig"), &json, 0o644)?;
        }
        None => {
            let _ = fs::remove_file(dir.join("index.json.sig"));
        }
    }
    let mut meta_bytes = serde_json::to_vec_pretty(meta)?;
    meta_bytes.push(b'\n');
    spk_core::fsutil::atomic_write(&dir.join("meta.json"), &meta_bytes, 0o644)?;
    security.save(&root.join("security.json"))?;
    let mut pointer = sha.clone().into_bytes();
    pointer.push(b'\n');
    spk_core::fsutil::atomic_write(&root.join("current"), &pointer, 0o644)?;
    prune_snapshots(root, &sha)?;
    let _ = paths;
    Ok(dir)
}

fn prune_snapshots(root: &Path, current: &str) -> Result<()> {
    let snapshots = root.join("snapshots");
    let entries = match fs::read_dir(&snapshots) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    let mut keep = BTreeSet::new();
    keep.insert(current.to_string());
    let mut other: Vec<_> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            (name != current).then_some((name, entry.metadata().ok()?.modified().ok()?))
        })
        .collect();
    other.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    for (name, _) in other.into_iter().take(1) {
        keep.insert(name);
    }
    if let Ok(entries) = fs::read_dir(&snapshots) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !keep.contains(&name) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(())
}

fn sync_mirror(
    paths: &Paths,
    root: &Path,
    name: &str,
    base: &str,
    old: Option<&CachedSnapshot>,
    security: &SecurityState,
    options: RemoteOptions,
) -> Result<CachedSnapshot> {
    let client = HttpClient::new()?;
    let mut headers = vec![("Cache-Control", "no-cache".to_string())];
    if let Some(old) = old {
        if !old.meta.etag.is_empty() {
            headers.push(("If-None-Match", old.meta.etag.clone()));
        }
    }
    let response = get(&client, &mirror_index_url(base), &headers, MAX_INDEX_BYTES)?;
    match response.status {
        304 => {
            let old = old.ok_or_else(|| {
                SpkError::Network(format!("{} returned 304 without a cached index", base))
            })?;
            Ok(old.clone())
        }
        200..=299 => {
            let signature = match get(
                &client,
                &mirror_signature_url(base),
                &[("Cache-Control", "no-cache".to_string())],
                MAX_SIGNATURE_BYTES,
            ) {
                Ok(response) if (200..=299).contains(&response.status) => Some(
                    serde_json::from_slice::<SignatureFile>(&response.body).map_err(|error| {
                        SpkError::Signature(format!("{}: malformed signature JSON: {error}", base))
                    })?,
                ),
                Ok(response) if response.status == 404 && options.allow_unsigned => None,
                Ok(response) => {
                    return Err(SpkError::Signature(format!(
                        "{}: HTTP {} fetching index.json.sig",
                        base, response.status
                    )))
                }
                Err(error) => return Err(error),
            };
            let mut key_id = repo::verify_index_bytes(
                paths,
                &response.body,
                signature.as_ref(),
                options.allow_unsigned,
            )?;
            let index = Index::from_slice(&response.body).map_err(|error| {
                SpkError::Security(format!("repository {name}: invalid signed index: {error}"))
            })?;
            if key_id.is_empty() {
                key_id = index.key_id.clone();
            }
            if index.arch != repo::target_arch() {
                return Err(SpkError::Security(format!(
                    "repository {name} is for {}, expected {}",
                    index.arch,
                    repo::target_arch()
                )));
            }
            if security.key_revoked(&key_id) {
                return Err(SpkError::Security(format!(
                    "repository {name} is signed by revoked key {key_id}"
                )));
            }
            security.check_floor(&index, name)?;
            let merged = security.merge(&index);
            let meta = SnapshotMeta {
                schema: 1,
                url: base.to_string(),
                etag: response.headers.get("etag").cloned().unwrap_or_default(),
                last_modified: response
                    .headers
                    .get("last-modified")
                    .cloned()
                    .unwrap_or_default(),
                sha256: sha256_bytes(&response.body),
                size: response.body.len() as u64,
                fetched_at: now(),
            };
            let dir = write_snapshot(
                paths,
                root,
                &response.body,
                signature.as_ref(),
                &meta,
                &merged,
            )?;
            Ok(CachedSnapshot {
                dir,
                index,
                meta,
                key_id,
            })
        }
        status => Err(SpkError::Network(format!(
            "HTTP {status} fetching {}",
            mirror_index_url(base)
        ))),
    }
}

pub fn sync_repo(
    paths: &Paths,
    name_hint: &str,
    urls: &[String],
    options: RemoteOptions,
    lang: Lang,
) -> Result<RemoteSync> {
    let mirrors: Vec<String> = urls
        .iter()
        .map(|url| url.trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
        .collect();
    if mirrors.is_empty() {
        return Err(SpkError::Usage("remote repository has no URL".into()));
    }
    let id = repo_id(name_hint, &mirrors);
    let root = paths.repo_cache.join(&id);
    fs::create_dir_all(root.join("snapshots"))?;
    fs::create_dir_all(&paths.package_cache)?;
    let old = load_current(paths, &root, options)?;
    let security = SecurityState::load(&root.join("security.json"))?;
    if options.offline {
        return match old {
            Some(old) => Ok(RemoteSync {
                dir: old.dir,
                key_id: old.key_id,
                index: old.index,
                security,
                mirrors,
                stale: true,
            }),
            None => Err(SpkError::Network(format!(
                "repository {name_hint} is not cached and --offline was given"
            ))),
        };
    }
    let mut last_error = None;
    for base in &mirrors {
        match sync_mirror(
            paths,
            &root,
            name_hint,
            base,
            old.as_ref(),
            &security,
            options,
        ) {
            Ok(cached) => {
                let merged = security.merge(&cached.index);
                return Ok(RemoteSync {
                    dir: cached.dir,
                    key_id: cached.key_id,
                    index: cached.index,
                    security: merged,
                    mirrors,
                    stale: false,
                });
            }
            Err(error @ SpkError::Signature(_)) | Err(error @ SpkError::Security(_)) => {
                return Err(error)
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    i18n::repo_mirror_unavailable(lang, name_hint, base, &error.to_string())
                );
                last_error = Some(error);
            }
        }
    }
    if let Some(old) = old {
        eprintln!(
            "{}",
            i18n::repo_using_cache(
                lang,
                name_hint,
                &last_error
                    .map(|error| error.to_string())
                    .unwrap_or_default()
            )
        );
        return Ok(RemoteSync {
            dir: old.dir,
            key_id: old.key_id,
            index: old.index,
            security,
            mirrors,
            stale: true,
        });
    }
    Err(last_error.unwrap_or_else(|| {
        SpkError::Network(format!("repository {name_hint} has no usable mirror"))
    }))
}

fn verify_cached_package(path: &Path, package: &IndexPackage) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let metadata = fs::metadata(path)?;
    if metadata.len() != package.size || sha256_file(path)? != package.sha256 {
        return Ok(false);
    }
    Ok(true)
}

fn download_one(
    client: &HttpClient,
    mirror: &str,
    package: &IndexPackage,
    part: &Path,
) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..RETRIES {
        let resume = fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
        if resume > package.size {
            let _ = fs::remove_file(part);
            continue;
        }
        match client.download_file(
            &http::join_url(mirror, &package.filename),
            &[],
            part,
            resume,
            package.size,
        ) {
            Ok(response) => {
                let state = response.status;
                let part_size = fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
                if part_size != package.size {
                    let _ = fs::remove_file(part);
                    last_error = Some(SpkError::Network(format!(
                        "{mirror}: size {} does not match index {} (HTTP {state})",
                        part_size, package.size
                    )));
                } else if sha256_file(part)? == package.sha256 {
                    return Ok(());
                } else {
                    let _ = fs::remove_file(part);
                    last_error = Some(SpkError::Signature(format!(
                        "{mirror}: {} sha256 does not match the index",
                        package.filename
                    )));
                }
            }
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < RETRIES {
                    std::thread::sleep(Duration::from_millis(200u64 << attempt));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        SpkError::Network(format!(
            "{mirror}: download failed for {}",
            package.filename
        ))
    }))
}

pub fn ensure_package(
    cache: &Path,
    mirrors: &[String],
    package: &IndexPackage,
    offline: bool,
) -> Result<PathBuf> {
    fs::create_dir_all(cache)?;
    let destination = cache.join(format!("{}.spk", package.sha256));
    if verify_cached_package(&destination, package)? {
        return Ok(destination);
    }
    let _ = fs::remove_file(&destination);
    if offline {
        return Err(SpkError::Network(format!(
            "{} is not cached and --offline was given",
            package.filename
        )));
    }
    let part = cache.join(format!("{}.spk.part", package.sha256));
    let client = HttpClient::new()?;
    let mut last_error = None;
    for mirror in mirrors {
        match download_one(&client, mirror, package, &part) {
            Ok(()) => {
                if verify_cached_package(&part, package)? {
                    fs::rename(&part, &destination)?;
                    sync_dir(cache);
                    return Ok(destination);
                }
            }
            Err(error) => last_error = Some(error),
        }
    }
    let _ = fs::remove_file(&part);
    Err(last_error
        .unwrap_or_else(|| SpkError::Network(format!("no mirror provided {}", package.filename))))
}

fn sync_dir(path: &Path) {
    if let Ok(directory) = fs::File::open(path) {
        let _ = directory.sync_all();
    }
}

/// Remove old `.part` files and enforce a configurable size/age policy.
pub fn prune_package_cache_with(
    paths: &Paths,
    keep: &BTreeSet<String>,
    retention_days: u64,
    limit_bytes: u64,
) -> Result<()> {
    let entries = match fs::read_dir(&paths.package_cache) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    let now = SystemTime::now();
    let mut packages = Vec::new();
    let mut parts = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".spk.part") {
            parts.push((path, metadata.modified().ok()));
        } else if name.ends_with(".spk") {
            total = total.saturating_add(metadata.len());
            let sha = name.trim_end_matches(".spk").to_string();
            if !keep.contains(&sha) {
                packages.push((path, metadata.len(), metadata.modified().ok()));
            }
        }
    }
    for (path, modified) in parts {
        let age = modified
            .and_then(|time| now.duration_since(time).ok())
            .unwrap_or_default();
        if age > Duration::from_secs(24 * 60 * 60) {
            let _ = fs::remove_file(path);
        }
    }
    packages.sort_by_key(|(_, _, modified)| *modified);
    for (path, length, modified) in packages {
        let age = modified
            .and_then(|time| now.duration_since(time).ok())
            .unwrap_or_default();
        if ((retention_days > 0 && age > Duration::from_secs(retention_days * 24 * 60 * 60))
            || (limit_bytes > 0 && total > limit_bytes))
            && fs::remove_file(&path).is_ok()
        {
            total = total.saturating_sub(length);
        }
    }
    Ok(())
}

pub fn prune_package_cache(paths: &Paths, keep: &BTreeSet<String>) -> Result<()> {
    prune_package_cache_with(paths, keep, RETENTION_DAYS, CACHE_LIMIT_BYTES)
}

/// Package hashes referenced by the currently cached repository snapshots.
pub fn cached_package_shas(paths: &Paths) -> Result<BTreeSet<String>> {
    let mut shas = BTreeSet::new();
    let entries = match fs::read_dir(&paths.repo_cache) {
        Ok(entries) => entries,
        Err(_) => return Ok(shas),
    };
    for entry in entries.flatten() {
        let root = entry.path();
        let pointer = match fs::read_to_string(root.join("current")) {
            Ok(pointer) => pointer.trim().to_string(),
            Err(_) => continue,
        };
        if pointer.len() != 64 {
            continue;
        }
        let index_path = root.join("snapshots").join(&pointer).join("index.json");
        if let Ok(bytes) = fs::read(&index_path) {
            if let Ok(index) = Index::from_slice(&bytes) {
                for package in index.packages {
                    shas.insert(package.sha256);
                }
            }
        }
    }
    Ok(shas)
}

#[derive(Clone, Debug, Default)]
pub struct CacheInfo {
    pub repository_bytes: u64,
    pub package_bytes: u64,
    pub part_bytes: u64,
    pub package_count: usize,
    pub part_count: usize,
    pub snapshot_count: usize,
}

pub fn cache_info(paths: &Paths) -> Result<CacheInfo> {
    let mut info = CacheInfo::default();
    if let Ok(entries) = fs::read_dir(&paths.package_cache) {
        for entry in entries.flatten() {
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".spk.part") {
                info.part_bytes = info.part_bytes.saturating_add(metadata.len());
                info.part_count += 1;
            } else if name.ends_with(".spk") {
                info.package_bytes = info.package_bytes.saturating_add(metadata.len());
                info.package_count += 1;
            }
        }
    }
    if let Ok(entries) = fs::read_dir(&paths.repo_cache) {
        for entry in entries.flatten() {
            let root = entry.path();
            if let Ok(entries) = fs::read_dir(root.join("snapshots")) {
                for snapshot in entries.flatten() {
                    info.snapshot_count += 1;
                    let mut stack = vec![snapshot.path()];
                    while let Some(path) = stack.pop() {
                        if path.is_dir() {
                            if let Ok(entries) = fs::read_dir(&path) {
                                stack.extend(entries.flatten().map(|entry| entry.path()));
                            }
                        } else if let Ok(metadata) = path.metadata() {
                            info.repository_bytes =
                                info.repository_bytes.saturating_add(metadata.len());
                        }
                    }
                }
            }
        }
    }
    Ok(info)
}

pub fn package_cache_keep(repos: &[crate::repo::LoadedRepo]) -> BTreeSet<String> {
    repos
        .iter()
        .flat_map(|repo| repo.index.packages.iter())
        .map(|package| package.sha256.clone())
        .collect()
}
