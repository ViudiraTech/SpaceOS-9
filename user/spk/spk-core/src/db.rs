//
//       user/spk/spk-core/src/db.rs
//       Frozen /var/lib/spk state schemas
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

//! State-file schemas from plan section 6. P0 freezes the JSON shapes; the
//! atomic write/journal implementation lands in P1 with the installer.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::manifest::{FileType, PackageFile};

pub const DB_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct InstalledDb {
    pub schema: u32,
    /// Package name -> installed record.
    pub packages: BTreeMap<String, InstalledPackage>,
}

impl Default for InstalledDb {
    fn default() -> Self {
        Self {
            schema: DB_SCHEMA,
            packages: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct InstalledPackage {
    pub name: String,
    pub epoch: u64,
    pub version: String,
    pub release: u64,
    pub arch: String,
    pub abi: String,
    pub files: Vec<PackageFile>,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    #[serde(default)]
    pub conffiles: Vec<String>,
    pub scripts: bool,
    pub allow_setuid: bool,
    pub install_date: u64,
    pub install_reason: InstallReason,
    /// Transaction that installed this exact record.
    pub txid: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InstallReason {
    Explicit,
    Dependency,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileDb {
    pub schema: u32,
    /// Absolute install path -> owning record.
    pub files: BTreeMap<String, FileRecord>,
}

impl Default for FileDb {
    fn default() -> Self {
        Self {
            schema: DB_SCHEMA,
            files: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    pub pkg: String,
    #[serde(rename = "type", default)]
    pub file_type: FileType,
    pub mode: String,
    pub uid: u32,
    pub gid: u32,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub conffile: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct WorldDb {
    pub schema: u32,
    /// Sorted set of explicitly installed package names.
    pub packages: BTreeSet<String>,
}

impl Default for WorldDb {
    fn default() -> Self {
        Self {
            schema: DB_SCHEMA,
            packages: BTreeSet::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ReposDb {
    pub schema: u32,
    pub repos: Vec<RepoConfig>,
}

impl Default for ReposDb {
    fn default() -> Self {
        Self {
            schema: DB_SCHEMA,
            repos: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub name: String,
    pub arch: String,
    pub urls: Vec<String>,
    /// Higher priority wins when the same package appears in several repos.
    pub priority: i32,
    pub key_id: String,
    pub last_sync: u64,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_schemas_roundtrip() {
        let mut installed = InstalledDb::default();
        installed.packages.insert(
            "hello".into(),
            InstalledPackage {
                name: "hello".into(),
                epoch: 0,
                version: "0.1.0".into(),
                release: 1,
                arch: "x86_64".into(),
                abi: "spaceos-1".into(),
                files: vec![PackageFile {
                    path: "/usr/bin/hello".into(),
                    file_type: FileType::File,
                    mode: "0755".into(),
                    uid: 0,
                    gid: 0,
                    sha256: "ab".repeat(32),
                    size: 3,
                    target: None,
                }],
                depends: vec![],
                provides: vec![],
                conflicts: vec![],
                replaces: vec![],
                conffiles: vec![],
                scripts: false,
                allow_setuid: false,
                install_date: 1760000000,
                install_reason: InstallReason::Explicit,
                txid: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            },
        );
        let text = serde_json::to_string_pretty(&installed).unwrap();
        let back: InstalledDb = serde_json::from_str(&text).unwrap();
        assert_eq!(back.packages["hello"].version, "0.1.0");

        let mut files = FileDb::default();
        files.files.insert(
            "/usr/bin/hello".into(),
            FileRecord {
                pkg: "hello".into(),
                file_type: FileType::File,
                mode: "0755".into(),
                uid: 0,
                gid: 0,
                sha256: "ab".repeat(32),
                size: 3,
                target: None,
                conffile: false,
            },
        );
        let text = serde_json::to_string_pretty(&files).unwrap();
        let back: FileDb = serde_json::from_str(&text).unwrap();
        assert_eq!(back.files["/usr/bin/hello"].pkg, "hello");

        let mut world = WorldDb::default();
        world.packages.insert("hello".into());
        let text = serde_json::to_string_pretty(&world).unwrap();
        assert!(text.contains("\"hello\""));

        let repos = ReposDb {
            schema: DB_SCHEMA,
            repos: vec![RepoConfig {
                name: "spaceos-main".into(),
                arch: "x86_64".into(),
                urls: vec!["https://goutoustdio.top/spaceos/repo/x86_64".into()],
                priority: 10,
                key_id: "ed25519:0123456789abcdef".into(),
                last_sync: 1760000000,
                enabled: true,
            }],
        };
        let text = serde_json::to_string_pretty(&repos).unwrap();
        let back: ReposDb = serde_json::from_str(&text).unwrap();
        assert_eq!(back.repos.len(), 1);

        let mut holds = HoldsDb::default();
        holds.packages.insert(
            "hello".into(),
            HeldPackage {
                epoch: 0,
                version: "0.1.0".into(),
                release: 1,
            },
        );
        let text = serde_json::to_string_pretty(&holds).unwrap();
        let back: HoldsDb = serde_json::from_str(&text).unwrap();
        assert_eq!(back.packages["hello"].version().to_string(), "0.1.0-1");
    }
}

/// Explicit version holds. A held package is never changed by the solver
/// unless the user passes `--override-hold` on an explicit target.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct HoldsDb {
    pub schema: u32,
    pub packages: BTreeMap<String, HeldPackage>,
}

impl Default for HoldsDb {
    fn default() -> Self {
        Self {
            schema: DB_SCHEMA,
            packages: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HeldPackage {
    pub epoch: u64,
    pub version: String,
    pub release: u64,
}

impl HeldPackage {
    pub fn version(&self) -> crate::version::Version {
        crate::version::Version::new(self.epoch, self.version.clone(), self.release)
    }
}
