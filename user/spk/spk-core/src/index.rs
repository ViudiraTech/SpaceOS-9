//
//       user/spk/spk-core/src/index.rs
//       Repository index.json schema and validation
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::keys::is_valid_key_id;
use crate::manifest::{
    is_sha256_hex, package_filename, validate_abi, validate_arch, validate_package_name,
    Dependency, Manifest,
};
use crate::version::{validate_pkgver, Version};
use crate::{Error, Result};

pub const INDEX_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct IndexPackage {
    pub name: String,
    #[serde(default)]
    pub epoch: u64,
    pub version: String,
    pub release: u64,
    pub arch: String,
    pub abi: String,
    pub filename: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    pub summary: String,
}

impl IndexPackage {
    pub fn from_manifest(manifest: &Manifest, filename: String, size: u64, sha256: String) -> Self {
        Self {
            name: manifest.name.clone(),
            epoch: manifest.epoch,
            version: manifest.version.clone(),
            release: manifest.release,
            arch: manifest.arch.clone(),
            abi: manifest.abi.clone(),
            filename,
            size,
            sha256,
            depends: manifest.depends.clone(),
            provides: manifest.provides.clone(),
            conflicts: manifest.conflicts.clone(),
            replaces: manifest.replaces.clone(),
            summary: manifest.summary.clone(),
        }
    }

    pub fn version(&self) -> Version {
        Version::new(self.epoch, self.version.clone(), self.release)
    }

    pub fn identity(&self) -> (String, u64, String, u64, String) {
        (
            self.name.clone(),
            self.epoch,
            self.version.clone(),
            self.release,
            self.arch.clone(),
        )
    }

    pub fn validate(&self, index_arch: &str) -> Result<()> {
        validate_package_name(&self.name)?;
        validate_pkgver(&self.version).map_err(|e| Error::Index(e.to_string()))?;
        if self.release == 0 {
            return Err(Error::Index(format!("{}: release must be >= 1", self.name)));
        }
        validate_arch(&self.arch)?;
        if self.arch != index_arch {
            return Err(Error::Index(format!(
                "{}: package architecture {} does not match index architecture {}",
                self.name, self.arch, index_arch
            )));
        }
        validate_abi(&self.abi)?;
        if self.filename.contains('/')
            || self.filename.contains('\\')
            || self.filename.starts_with('.')
        {
            return Err(Error::Index(format!(
                "{}: unsafe package filename '{}'",
                self.name, self.filename
            )));
        }
        let expected = package_filename(
            &self.name,
            self.epoch,
            &self.version,
            self.release,
            &self.arch,
        );
        if self.filename != expected {
            return Err(Error::Index(format!(
                "{}: filename '{}' does not match '{}'",
                self.name, self.filename, expected
            )));
        }
        if self.size == 0 {
            return Err(Error::Index(format!("{}: package size is zero", self.name)));
        }
        if !is_sha256_hex(&self.sha256) {
            return Err(Error::Index(format!(
                "{}: malformed package sha256",
                self.name
            )));
        }
        if self.summary.trim().is_empty() {
            return Err(Error::Index(format!("{}: empty summary", self.name)));
        }
        for list in [
            (&self.depends, "depends"),
            (&self.provides, "provides"),
            (&self.conflicts, "conflicts"),
            (&self.replaces, "replaces"),
        ] {
            for item in list.0 {
                Dependency::parse(item).map_err(|e| {
                    Error::Index(format!(
                        "{}: invalid {} entry '{}': {}",
                        self.name, list.1, item, e
                    ))
                })?;
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub schema: u32,
    pub repo: String,
    pub arch: String,
    pub generated_at: u64,
    pub key_id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub min_version: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked_versions: Vec<RevokedVersion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked_keys: Vec<String>,
    pub packages: Vec<IndexPackage>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevokedVersion {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub reason: String,
}

/// Repository root manifest (schema 2). It carries the hash chain: the signed
/// manifest pins the catalog, and the catalog pins the per-package history
/// files, so only this one file needs a detached signature.
pub const REPO_SCHEMA: u32 = 2;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    pub sha256: String,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RepoManifest {
    pub schema: u32,
    pub repo: String,
    pub arch: String,
    pub generated_at: u64,
    pub key_id: String,
    pub catalog: FileDigest,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub history: BTreeMap<String, FileDigest>,
    /// Optional `store.json`: per-package presentation plus editorial picks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<FileDigest>,
}

impl RepoManifest {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let manifest: RepoManifest = serde_json::from_slice(bytes)?;
        if manifest.schema != REPO_SCHEMA {
            return Err(Error::Index(format!(
                "unsupported repository manifest schema {}",
                manifest.schema
            )));
        }
        validate_repo_name(&manifest.repo)?;
        validate_arch(&manifest.arch)?;
        if !is_valid_key_id(&manifest.key_id) {
            return Err(Error::Index(format!(
                "malformed repository manifest key id '{}'",
                manifest.key_id
            )));
        }
        if !is_sha256_hex(&manifest.catalog.sha256) {
            return Err(Error::Index("malformed catalog sha256".into()));
        }
        for (name, digest) in &manifest.history {
            validate_package_name(name)?;
            if !is_sha256_hex(&digest.sha256) {
                return Err(Error::Index(format!(
                    "{name}: malformed history sha256"
                )));
            }
        }
        if let Some(store) = &manifest.store {
            if !is_sha256_hex(&store.sha256) {
                return Err(Error::Index("malformed store sha256".into()));
            }
        }
        Ok(manifest)
    }
}

/// Relative path of a package's version history, laid out like the cargo
/// sparse registry: short names get their own top-level bucket, longer names
/// are split by their first four characters.
pub fn history_path(name: &str) -> String {
    if name.len() == 1 {
        format!("pkg/1/{name}")
    } else if name.len() == 2 {
        format!("pkg/2/{name}")
    } else if name.len() == 3 {
        format!("pkg/3/{}/{name}", &name[..1])
    } else {
        // Package names are ASCII by construction, so byte slicing is safe.
        format!("pkg/{}/{}/{name}", &name[..2], &name[2..4])
    }
}

/// Serialise every known version of one package as newline-delimited JSON,
/// oldest first.
pub fn history_to_bytes(packages: &[IndexPackage]) -> Result<Vec<u8>> {
    let mut sorted: Vec<&IndexPackage> = packages.iter().collect();
    sorted.sort_by(|a, b| a.version().cmp(&b.version()));
    let mut out = Vec::with_capacity(sorted.len() * 320);
    for package in sorted {
        out.extend_from_slice(&serde_json::to_vec(package)?);
        out.push(b'\n');
    }
    Ok(out)
}

pub fn history_from_slice(bytes: &[u8], arch: &str) -> Result<Vec<IndexPackage>> {
    let mut packages = Vec::new();
    for (line_no, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        let package: IndexPackage = serde_json::from_slice(line).map_err(|error| {
            Error::Index(format!("history line {}: {error}", line_no + 1))
        })?;
        package.validate(arch)?;
        packages.push(package);
    }
    Ok(packages)
}

impl Index {
    pub fn new(repo: &str, arch: &str, generated_at: u64, key_id: &str) -> Self {
        Self {
            schema: INDEX_SCHEMA,
            repo: repo.to_string(),
            arch: arch.to_string(),
            generated_at,
            key_id: key_id.to_string(),
            min_version: BTreeMap::new(),
            revoked_versions: Vec::new(),
            revoked_keys: Vec::new(),
            packages: Vec::new(),
        }
    }

    /// Compact catalog serialisation: one package per line so that adding or
    /// updating a package shows up as a single changed line in version
    /// control instead of a multi-hundred-line block.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = String::with_capacity(self.packages.len() * 320 + 256);
        out.push('{');
        out.push_str(&format!("\"schema\":{},", self.schema));
        out.push_str("\"repo\":");
        out.push_str(&serde_json::to_string(&self.repo)?);
        out.push_str(",\"arch\":");
        out.push_str(&serde_json::to_string(&self.arch)?);
        out.push_str(&format!(",\"generated_at\":{},", self.generated_at));
        out.push_str("\"key_id\":");
        out.push_str(&serde_json::to_string(&self.key_id)?);
        if !self.min_version.is_empty() {
            out.push_str(",\"min_version\":");
            out.push_str(&serde_json::to_string(&self.min_version)?);
        }
        if !self.revoked_versions.is_empty() {
            out.push_str(",\"revoked_versions\":");
            out.push_str(&serde_json::to_string(&self.revoked_versions)?);
        }
        if !self.revoked_keys.is_empty() {
            out.push_str(",\"revoked_keys\":");
            out.push_str(&serde_json::to_string(&self.revoked_keys)?);
        }
        out.push_str(",\"packages\":[");
        for (index, package) in self.packages.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&serde_json::to_string(package)?);
        }
        if !self.packages.is_empty() {
            out.push('\n');
        }
        out.push_str("]}\n");
        Ok(out.into_bytes())
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let index: Index = serde_json::from_slice(bytes)?;
        index.validate()?;
        Ok(index)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != INDEX_SCHEMA {
            return Err(Error::Index(format!(
                "unsupported index schema {}",
                self.schema
            )));
        }
        validate_repo_name(&self.repo)?;
        validate_arch(&self.arch)?;
        if !is_valid_key_id(&self.key_id) {
            return Err(Error::Index(format!(
                "malformed index key id '{}'",
                self.key_id
            )));
        }
        for (name, version) in &self.min_version {
            validate_package_name(name)?;
            Version::parse(version).map_err(|error| {
                Error::Index(format!("min_version for {name} is invalid: {error}"))
            })?;
        }
        let mut revoked = BTreeSet::new();
        for item in &self.revoked_versions {
            validate_package_name(&item.name)?;
            Version::parse(&item.version).map_err(|error| {
                Error::Index(format!(
                    "revoked version {} {} is invalid: {error}",
                    item.name, item.version
                ))
            })?;
            if !revoked.insert((item.name.clone(), item.version.clone())) {
                return Err(Error::Index(format!(
                    "duplicate revoked version {} {}",
                    item.name, item.version
                )));
            }
            if item.reason.chars().any(|ch| ch.is_control()) {
                return Err(Error::Index(format!(
                    "revoked version {} has a malformed reason",
                    item.name
                )));
            }
        }
        let mut revoked_keys = BTreeSet::new();
        for key in &self.revoked_keys {
            if !is_valid_key_id(key) {
                return Err(Error::Index(format!("malformed revoked key id '{key}'")));
            }
            if !revoked_keys.insert(key) {
                return Err(Error::Index(format!("duplicate revoked key id '{key}'")));
            }
        }
        let mut seen = BTreeSet::new();
        for package in &self.packages {
            package.validate(&self.arch)?;
            if !seen.insert(package.identity()) {
                return Err(Error::Index(format!(
                    "duplicate package {} {}-{} in index",
                    package.name,
                    package.version(),
                    package.release
                )));
            }
        }
        Ok(())
    }
}

pub fn validate_repo_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        || !name
            .bytes()
            .next()
            .map(|b| b.is_ascii_alphanumeric())
            .unwrap_or(false)
    {
        return Err(Error::Index(format!("invalid repository name '{name}'")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::from_toml(
            r#"
name = "foo"
version = "1.2.3"
release = 1
arch = "x86_64"
abi = "spaceos-1"
summary = "example"
license = "MIT"
"#,
        )
        .unwrap()
    }

    #[test]
    fn roundtrip() {
        let manifest = manifest();
        let package = IndexPackage::from_manifest(
            &manifest,
            manifest.package_filename(),
            1234,
            "ab".repeat(32),
        );
        let mut index = Index::new(
            "spaceos-main",
            "x86_64",
            1760000000,
            "ed25519:0123456789abcdef",
        );
        index.packages.push(package);
        let bytes = index.to_bytes().unwrap();
        let parsed = Index::from_slice(&bytes).unwrap();
        assert_eq!(parsed.packages.len(), 1);
        assert_eq!(parsed.packages[0].version().to_string(), "1.2.3-1");
    }

    #[test]
    fn rejects_bad_packages() {
        let manifest = manifest();
        let good =
            IndexPackage::from_manifest(&manifest, manifest.package_filename(), 1, "ab".repeat(32));

        let mut wrong_name = good.clone();
        wrong_name.filename = "bar-1.2.3-1-x86_64.spk".into();
        assert!(wrong_name.validate("x86_64").is_err());

        let mut wrong_arch = good.clone();
        wrong_arch.arch = "aarch64".into();
        assert!(wrong_arch.validate("x86_64").is_err());

        let mut bad_hash = good;
        bad_hash.sha256 = "zz".repeat(32);
        assert!(bad_hash.validate("x86_64").is_err());
    }

    #[test]
    fn rejects_duplicates_and_bad_key_ids() {
        let manifest = manifest();
        let package =
            IndexPackage::from_manifest(&manifest, manifest.package_filename(), 1, "ab".repeat(32));
        let mut index = Index::new("spaceos-main", "x86_64", 0, "ed25519:0123456789abcdef");
        index.packages.push(package.clone());
        index.packages.push(package);
        assert!(index.validate().is_err());

        let mut bad_key = Index::new("spaceos-main", "x86_64", 0, "ed25519:NOTHEX");
        bad_key.packages.clear();
        assert!(bad_key.validate().is_err());
    }
}
