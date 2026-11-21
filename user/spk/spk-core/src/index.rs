//
//       user/spk/spk-core/src/index.rs
//       Repository index.json schema and validation
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeSet;

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
    pub packages: Vec<IndexPackage>,
}

impl Index {
    pub fn new(repo: &str, arch: &str, generated_at: u64, key_id: &str) -> Self {
        Self {
            schema: INDEX_SCHEMA,
            repo: repo.to_string(),
            arch: arch.to_string(),
            generated_at,
            key_id: key_id.to_string(),
            packages: Vec::new(),
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
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
