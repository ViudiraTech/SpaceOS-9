//
//       user/spk/spk-host/src/indexgen.rs
//       Scan a repository directory, emit index.json and sign it
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use spk_core::archive::inspect_spk;
use spk_core::fsutil::{atomic_write, sha256_bytes, sha256_file};
use spk_core::index::{
    history_path, history_to_bytes, validate_repo_name, FileDigest, Index, IndexPackage,
    RepoManifest, RevokedVersion, REPO_SCHEMA,
};
use spk_core::keys::{read_signature, signature_path, write_signature, PublicKey, SignatureEntry};
use spk_core::manifest::validate_arch;
use spk_core::{Error, Result};

pub struct IndexResult {
    pub index: Index,
    pub index_path: PathBuf,
    pub manifest_path: PathBuf,
    pub history_files: usize,
    pub signed: bool,
    pub warnings: Vec<String>,
}

/// Per-package inspection cache. Rebuilding an index used to decompress every
/// archive in the repository; with the cache only new or replaced packages are
/// inspected, so adding one package costs one package.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct IndexCache {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    entries: BTreeMap<String, CacheEntry>,
}

const CACHE_SCHEMA: u32 = 1;
const CACHE_FILE: &str = ".spk-index-cache.json";

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CacheEntry {
    size: u64,
    mtime: u64,
    package: IndexPackage,
}

impl IndexCache {
    fn load(repo_dir: &Path) -> Self {
        let Ok(bytes) = fs::read(repo_dir.join(CACHE_FILE)) else {
            return Self::default();
        };
        match serde_json::from_slice::<IndexCache>(&bytes) {
            Ok(cache) if cache.schema == CACHE_SCHEMA => cache,
            _ => Self::default(),
        }
    }
}

pub struct VerifyReport {
    pub index: Index,
    pub packages: Vec<String>,
}

/// Optional signed security metadata for an index. An empty object keeps the
/// historical byte-for-byte index format used by the production repository.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    #[serde(default)]
    pub min_version: BTreeMap<String, String>,
    #[serde(default)]
    pub revoked_versions: Vec<RevokedVersion>,
    #[serde(default)]
    pub revoked_keys: Vec<String>,
}

pub fn generate(
    repo_dir: &Path,
    repo: &str,
    arch: &str,
    key_path: &Path,
    generated_at: Option<u64>,
    sign: bool,
) -> Result<IndexResult> {
    generate_with_keys(
        repo_dir,
        repo,
        arch,
        &[key_path.to_path_buf()],
        generated_at,
        sign,
    )
}

pub fn generate_with_keys(
    repo_dir: &Path,
    repo: &str,
    arch: &str,
    key_paths: &[PathBuf],
    generated_at: Option<u64>,
    sign: bool,
) -> Result<IndexResult> {
    generate_with_keys_and_security(repo_dir, repo, arch, key_paths, generated_at, sign, None)
}

pub fn generate_with_keys_and_security(
    repo_dir: &Path,
    repo: &str,
    arch: &str,
    key_paths: &[PathBuf],
    generated_at: Option<u64>,
    sign: bool,
    security: Option<&SecurityConfig>,
) -> Result<IndexResult> {
    validate_repo_name(repo)?;
    validate_arch(arch)?;
    if !repo_dir.is_dir() {
        return Err(Error::Index(format!(
            "repository directory {} does not exist",
            repo_dir.display()
        )));
    }

    if key_paths.is_empty() {
        return Err(Error::Index("at least one signing key is required".into()));
    }
    let (public, signing) = if sign {
        let keys: Vec<_> = key_paths
            .iter()
            .map(|path| spk_core::keys::load_secret_key(path))
            .collect::<Result<Vec<_>>>()?;
        (keys[0].public(), Some(keys))
    } else {
        (PublicKey::from_file(&key_paths[0])?, None)
    };

    let mut paths = Vec::new();
    for entry in fs::read_dir(repo_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            Error::Index(format!("non-UTF-8 file name in {}", repo_dir.display()))
        })?;
        if !name.ends_with(".spk") {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(Error::Index(format!("'{name}' is not a regular file")));
        }
        paths.push((name.to_string(), path));
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0));

    let generated_at = generated_at.unwrap_or(default_generated_at()?);
    let mut index = Index::new(repo, arch, generated_at, &public.key_id);
    if let Some(security) = security {
        index.min_version = security.min_version.clone();
        index.revoked_versions = security.revoked_versions.clone();
        index.revoked_keys = security.revoked_keys.clone();
    }

    let cache = IndexCache::load(repo_dir);
    let mut next_cache = IndexCache {
        schema: CACHE_SCHEMA,
        entries: BTreeMap::new(),
    };
    let mut identities = BTreeSet::new();
    let mut all = Vec::new();
    for (name, path) in paths {
        let metadata = fs::metadata(&path)?;
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|age| age.as_secs())
            .unwrap_or(0);
        let cached = cache.entries.get(&name).filter(|entry| {
            entry.size == metadata.len()
                && entry.mtime == mtime
                && entry.package.filename == name
                && entry.package.arch == arch
        });
        let package = match cached {
            Some(entry) => entry.package.clone(),
            None => {
                let inspected = inspect_spk(&path)?;
                if inspected.manifest.arch != arch {
                    return Err(Error::Index(format!(
                        "{name}: package architecture {} does not match index architecture {arch}",
                        inspected.manifest.arch
                    )));
                }
                let expected = inspected.manifest.package_filename();
                if name != expected {
                    return Err(Error::Index(format!(
                        "{name}: expected package filename '{expected}'"
                    )));
                }
                IndexPackage::from_manifest(
                    &inspected.manifest,
                    name.clone(),
                    inspected.package_size,
                    inspected.package_sha256,
                )
            }
        };
        next_cache.entries.insert(
            name,
            CacheEntry {
                size: metadata.len(),
                mtime,
                package: package.clone(),
            },
        );
        if !identities.insert(package.identity()) {
            return Err(Error::Index(format!(
                "duplicate package {} {}-{}",
                package.name,
                package.version(),
                package.release
            )));
        }
        all.push(package);
    }

    // The published catalog keeps the newest release of every package; older
    // releases live in the per-package history files.
    let mut latest: BTreeMap<String, IndexPackage> = BTreeMap::new();
    for package in &all {
        match latest.get(&package.name) {
            Some(current) if current.version() >= package.version() => {}
            _ => {
                latest.insert(package.name.clone(), package.clone());
            }
        }
    }
    index.packages = latest.into_values().collect();
    index.validate()?;

    let bytes = index.to_bytes()?;
    let index_path = repo_dir.join("index.json");
    atomic_write(&index_path, &bytes, 0o644)?;

    let mut by_name: BTreeMap<String, Vec<IndexPackage>> = BTreeMap::new();
    for package in &all {
        by_name
            .entry(package.name.clone())
            .or_default()
            .push(package.clone());
    }
    let mut history = BTreeMap::new();
    let mut history_files = 0usize;
    for (name, versions) in &by_name {
        let history_bytes = history_to_bytes(versions)?;
        let path = repo_dir.join(history_path(name));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&path, &history_bytes, 0o644)?;
        history.insert(
            name.clone(),
            FileDigest {
                sha256: sha256_bytes(&history_bytes),
                size: history_bytes.len() as u64,
            },
        );
        history_files += 1;
    }

    let cache_bytes = serde_json::to_vec(&next_cache)?;
    atomic_write(&repo_dir.join(CACHE_FILE), &cache_bytes, 0o644)?;

    let manifest = RepoManifest {
        schema: REPO_SCHEMA,
        repo: repo.to_string(),
        arch: arch.to_string(),
        generated_at,
        key_id: public.key_id.clone(),
        catalog: FileDigest {
            sha256: sha256_bytes(&bytes),
            size: bytes.len() as u64,
        },
        history,
    };
    let manifest_bytes = manifest.to_bytes()?;
    let manifest_path = repo_dir.join("repo.json");
    atomic_write(&manifest_path, &manifest_bytes, 0o644)?;

    let mut warnings = Vec::new();
    match signing {
        Some(keys) => {
            for (path, payload) in [(&index_path, &bytes), (&manifest_path, &manifest_bytes)] {
                let mut signature = keys[0].sign(payload);
                if keys.len() > 1 {
                    signature.signatures = keys
                        .iter()
                        .map(|key| {
                            let signed = key.sign(payload);
                            SignatureEntry {
                                key_id: signed.key_id,
                                signature: signed.signature,
                            }
                        })
                        .collect();
                }
                write_signature(&signature_path(path), &signature)?;
            }
        }
        None => {
            for path in [&index_path, &manifest_path] {
                let stale = signature_path(path);
                if stale.exists() {
                    warnings.push(format!(
                        "{} was not regenerated and no longer matches the index",
                        stale.display()
                    ));
                }
            }
        }
    }

    Ok(IndexResult {
        index,
        index_path,
        manifest_path,
        history_files,
        signed: sign,
        warnings,
    })
}

fn default_generated_at() -> Result<u64> {
    match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(text) => text
            .parse::<u64>()
            .map_err(|_| Error::Index(format!("invalid SOURCE_DATE_EPOCH '{text}'"))),
        Err(std::env::VarError::NotPresent) => Ok(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()),
        Err(e) => Err(Error::Index(format!("SOURCE_DATE_EPOCH: {e}"))),
    }
}

pub fn verify_index(index_path: &Path, sig_path: Option<&Path>, key_path: &Path) -> Result<Index> {
    let public = PublicKey::from_file(key_path)?;
    let bytes = fs::read(index_path)?;
    let sig_path = sig_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| signature_path(index_path));
    let signature = read_signature(&sig_path)?;
    public.verify(&bytes, &signature)?;
    let index = Index::from_slice(&bytes)?;
    if index.key_id != public.key_id {
        return Err(Error::Signature(format!(
            "index is signed by {}, key is {}",
            index.key_id, public.key_id
        )));
    }
    Ok(index)
}

pub fn verify_repo(repo_dir: &Path, key_path: &Path) -> Result<VerifyReport> {
    let index_path = repo_dir.join("index.json");
    let index = verify_index(&index_path, None, key_path)?;
    verify_manifest(repo_dir, &index, key_path)?;
    let mut verified = Vec::new();
    for package in &index.packages {
        let path = repo_dir.join(&package.filename);
        let metadata =
            fs::metadata(&path).map_err(|e| Error::Index(format!("{}: {e}", package.filename)))?;
        if metadata.len() != package.size {
            return Err(Error::Index(format!(
                "{}: size is {}, index says {}",
                package.filename,
                metadata.len(),
                package.size
            )));
        }
        if sha256_file(&path)? != package.sha256 {
            return Err(Error::Index(format!(
                "{}: sha256 does not match the index",
                package.filename
            )));
        }
        let inspected = inspect_spk(&path)?;
        if !index_matches_manifest(package, &inspected.manifest) {
            return Err(Error::Index(format!(
                "{}: manifest does not match the index entry",
                package.filename
            )));
        }
        verified.push(format!("{} {}", package.name, package.version()));
    }
    Ok(VerifyReport {
        index,
        packages: verified,
    })
}

/// Check the schema 2 hash chain: the signed repo.json pins the catalog, and
/// the catalog pins every per-package history file. Repositories generated
/// before schema 2 have no repo.json and are accepted unchanged.
fn verify_manifest(repo_dir: &Path, index: &Index, key_path: &Path) -> Result<()> {
    let manifest_path = repo_dir.join("repo.json");
    if !manifest_path.exists() {
        return Ok(());
    }
    let public = PublicKey::from_file(key_path)?;
    let bytes = fs::read(&manifest_path)?;
    let signature = read_signature(&signature_path(&manifest_path))?;
    public.verify(&bytes, &signature)?;
    let manifest = RepoManifest::from_slice(&bytes)?;
    if manifest.key_id != public.key_id {
        return Err(Error::Signature(format!(
            "repository manifest is signed by {}, key is {}",
            manifest.key_id, public.key_id
        )));
    }
    if manifest.repo != index.repo || manifest.arch != index.arch {
        return Err(Error::Index(
            "repo.json and index.json describe different repositories".into(),
        ));
    }
    let catalog = fs::read(repo_dir.join("index.json"))?;
    if catalog.len() as u64 != manifest.catalog.size
        || sha256_bytes(&catalog) != manifest.catalog.sha256
    {
        return Err(Error::Index(
            "repo.json catalog digest does not match index.json".into(),
        ));
    }
    for (name, digest) in &manifest.history {
        let path = repo_dir.join(history_path(name));
        let bytes = fs::read(&path)
            .map_err(|e| Error::Index(format!("{name}: history file missing: {e}")))?;
        if bytes.len() as u64 != digest.size || sha256_bytes(&bytes) != digest.sha256 {
            return Err(Error::Index(format!(
                "{name}: history file does not match repo.json"
            )));
        }
        let versions = spk_core::index::history_from_slice(&bytes, &index.arch)?;
        if versions.iter().any(|package| package.name != *name) {
            return Err(Error::Index(format!(
                "{name}: history file describes another package"
            )));
        }
        for package in index.packages.iter().filter(|package| package.name == *name) {
            if !versions.iter().any(|entry| entry.identity() == package.identity()) {
                return Err(Error::Index(format!(
                    "{name}: catalog entry missing from its history file"
                )));
            }
        }
    }
    for package in &index.packages {
        if !manifest.history.contains_key(&package.name) {
            return Err(Error::Index(format!(
                "{}: no history file recorded in repo.json",
                package.name
            )));
        }
    }
    Ok(())
}

fn index_matches_manifest(package: &IndexPackage, manifest: &spk_core::manifest::Manifest) -> bool {
    package.name == manifest.name
        && package.epoch == manifest.epoch
        && package.version == manifest.version
        && package.release == manifest.release
        && package.arch == manifest.arch
        && package.abi == manifest.abi
        && package.depends == manifest.depends
        && package.provides == manifest.provides
        && package.conflicts == manifest.conflicts
        && package.replaces == manifest.replaces
        && package.summary == manifest.summary
}
