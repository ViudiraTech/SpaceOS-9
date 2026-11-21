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
use spk_core::fsutil::{atomic_write, sha256_file};
use spk_core::index::{validate_repo_name, Index, IndexPackage, RevokedVersion};
use spk_core::keys::{read_signature, signature_path, write_signature, PublicKey, SignatureEntry};
use spk_core::manifest::validate_arch;
use spk_core::{Error, Result};

pub struct IndexResult {
    pub index: Index,
    pub index_path: PathBuf,
    pub signed: bool,
    pub warnings: Vec<String>,
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
    let mut identities = BTreeSet::new();
    for (name, path) in paths {
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
        let package = IndexPackage::from_manifest(
            &inspected.manifest,
            name,
            inspected.package_size,
            inspected.package_sha256,
        );
        if !identities.insert(package.identity()) {
            return Err(Error::Index(format!(
                "duplicate package {} {}-{}",
                package.name,
                package.version(),
                package.release
            )));
        }
        index.packages.push(package);
    }
    index.packages.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.version().cmp(&b.version()))
    });
    index.validate()?;

    let bytes = index.to_bytes()?;
    let index_path = repo_dir.join("index.json");
    atomic_write(&index_path, &bytes, 0o644)?;

    let mut warnings = Vec::new();
    if let Some(keys) = signing {
        let mut signature = keys[0].sign(&bytes);
        if keys.len() > 1 {
            signature.signatures = keys
                .iter()
                .map(|key| {
                    let signed = key.sign(&bytes);
                    SignatureEntry {
                        key_id: signed.key_id,
                        signature: signed.signature,
                    }
                })
                .collect();
        }
        write_signature(&signature_path(&index_path), &signature)?;
    } else {
        let stale = signature_path(&index_path);
        if stale.exists() {
            warnings.push(format!(
                "{} was not regenerated and no longer matches index.json",
                stale.display()
            ));
        }
    }

    Ok(IndexResult {
        index,
        index_path,
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
