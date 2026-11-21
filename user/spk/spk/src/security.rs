//
//       user/spk/spk/src/security.rs
//       Signed repository security floor: min versions, revocations and keys
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use spk_core::index::{Index, IndexPackage, RevokedVersion};
use spk_core::version::Version;

use crate::error::{Result, SpkError};

pub const SECURITY_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityState {
    pub schema: u32,
    #[serde(default)]
    pub generated_at: u64,
    #[serde(default)]
    pub min_version: BTreeMap<String, String>,
    #[serde(default)]
    pub revoked_versions: Vec<RevokedVersion>,
    #[serde(default)]
    pub revoked_keys: BTreeSet<String>,
}

impl SecurityState {
    pub fn from_index(index: &Index) -> Self {
        Self {
            schema: SECURITY_SCHEMA,
            generated_at: index.generated_at,
            min_version: index.min_version.clone(),
            revoked_versions: index.revoked_versions.clone(),
            revoked_keys: index.revoked_keys.iter().cloned().collect(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                schema: SECURITY_SCHEMA,
                ..Self::default()
            }),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        spk_core::fsutil::atomic_write(path, &bytes, 0o644)?;
        Ok(())
    }

    pub fn check_floor(&self, index: &Index, repo: &str) -> Result<()> {
        if self.generated_at != 0 && index.generated_at < self.generated_at {
            return Err(SpkError::Security(format!(
                "repository {repo}: index generated_at {} is older than local floor {} (possible rollback)",
                index.generated_at, self.generated_at
            )));
        }
        Ok(())
    }

    /// Merge signed index security data into the persisted floor. Revocations
    /// and key revocations are monotonic; min versions never move backwards.
    pub fn merge(&self, index: &Index) -> Self {
        let mut merged = self.clone();
        merged.generated_at = merged.generated_at.max(index.generated_at);
        for (name, version) in &index.min_version {
            merged
                .min_version
                .entry(name.clone())
                .and_modify(|current| {
                    if version_is_newer(version, current) {
                        *current = version.clone();
                    }
                })
                .or_insert_with(|| version.clone());
        }
        for revoked in &index.revoked_versions {
            if !merged
                .revoked_versions
                .iter()
                .any(|item| item.name == revoked.name && item.version == revoked.version)
            {
                merged.revoked_versions.push(revoked.clone());
            }
        }
        merged
            .revoked_keys
            .extend(index.revoked_keys.iter().cloned());
        merged
    }

    pub fn reason(&self, package: &IndexPackage) -> Option<String> {
        let version = package.version();
        if let Some(minimum) = self.min_version.get(&package.name) {
            if version_is_older(&version.to_string(), minimum) {
                return Some(format!(
                    "{} {} is below the repository minimum {}",
                    package.name, version, minimum
                ));
            }
        }
        if self
            .revoked_versions
            .iter()
            .any(|item| item.name == package.name && item.version == version.to_string())
        {
            return Some(format!(
                "{} {} is revoked by the repository",
                package.name, version
            ));
        }
        None
    }

    pub fn rejects_target(&self, name: &str, version: &Version) -> Option<String> {
        if let Some(minimum) = self.min_version.get(name) {
            if version_is_older(&version.to_string(), minimum) {
                return Some(format!(
                    "{name} {version} is below the repository minimum {minimum}"
                ));
            }
        }
        if self
            .revoked_versions
            .iter()
            .any(|item| item.name == name && item.version == version.to_string())
        {
            return Some(format!("{name} {version} is revoked by the repository"));
        }
        None
    }

    pub fn key_revoked(&self, key_id: &str) -> bool {
        self.revoked_keys.contains(key_id)
    }
}

fn version_is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

fn version_is_older(candidate: &str, floor: &str) -> bool {
    match (Version::parse(candidate), Version::parse(floor)) {
        (Ok(candidate), Ok(floor)) => candidate < floor,
        _ => false,
    }
}
