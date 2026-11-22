//
//       user/spk/spk-core/src/manifest.rs
//       manifest.toml schema, path policy and dependency syntax
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::version::{validate_pkgver, Version};
use crate::{Error, Result};

pub const ARCHES: &[&str] = &["x86_64", "aarch64", "riscv64"];
/// SpaceOS syscalls copy paths through a 512-byte buffer including the NUL
/// terminator, so the longest usable absolute path is 511 bytes. Linux allows
/// PATH_MAX 4096; the tighter SpaceOS limit is enforced here so packages that
/// cannot be installed are rejected at build time.
pub const MAX_PATH_LEN: usize = 511;
pub const MAX_COMPONENT_LEN: usize = 255;
/// Package payloads may never replace the package database, the lock file or
/// the trust store.
pub const RESERVED_PREFIXES: &[&str] = &["/var/lib/spk", "/etc/spk/trusted.d"];
pub const RESERVED_PATHS: &[&str] = &["/etc/spk/setuid.allow"];
pub const META_MANIFEST: &str = "manifest.toml";
pub const META_INSTALL: &str = ".INSTALL";
pub const META_CHANGELOG: &str = ".CHANGELOG";
pub const META_BUILDINFO: &str = ".BUILDINFO";
pub const HOOK_PREFIX: &str = "/usr/lib/spk/hooks/";
pub const TRIGGER_PREFIX: &str = "/usr/lib/spk/triggers/";
pub const RESERVED_ROOT_FILES: &[&str] =
    &[META_MANIFEST, META_INSTALL, META_CHANGELOG, META_BUILDINFO];

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileType {
    #[default]
    File,
    Dir,
    Symlink,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageFile {
    /// Absolute install path, e.g. `/usr/bin/foo`.
    pub path: String,
    #[serde(rename = "type", default, skip_serializing_if = "is_regular_file")]
    pub file_type: FileType,
    /// Four-digit octal string, e.g. `0755`.
    pub mode: String,
    #[serde(default)]
    pub uid: u32,
    #[serde(default)]
    pub gid: u32,
    /// SHA-256 of file contents, or of the target string for symlinks.
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

fn is_regular_file(kind: &FileType) -> bool {
    *kind == FileType::File
}

impl PackageFile {
    pub fn mode_bits(&self) -> Result<u32> {
        parse_mode(&self.mode)
    }
}

/// Optional presentation metadata for the graphical store. Every field is
/// optional so packages published before the store existed keep validating,
/// and so an older `spk` can still install a package that carries it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageStoreMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Long description shown on the store's detail page. Defaults to the
    /// manifest's own `description` when left empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// One or two `#rrggbb` colours used for the store card gradient.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accent: Vec<String>,
    /// Repository-relative banner or preview image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
    /// Higher sorts first in the store's featured grid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub featured_rank: Option<i32>,
}

fn is_hex_color(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('#') else {
        return false;
    };
    rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_hexdigit())
}

impl PackageStoreMeta {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.subtitle.is_none()
            && self.description.is_none()
            && self.category.is_none()
            && self.highlights.is_empty()
            && self.icon.is_none()
            && self.accent.is_empty()
            && self.banner.is_none()
            && self.featured_rank.is_none()
    }

    pub fn validate(&self, package: &str) -> Result<()> {
        for (name, value, limit) in [
            ("display_name", &self.display_name, 64usize),
            ("subtitle", &self.subtitle, 120),
            ("icon", &self.icon, 32),
        ] {
            if let Some(value) = value {
                if value.trim().is_empty() || value.chars().count() > limit {
                    return Err(Error::Manifest(format!(
                        "{package}: store {name} must be 1..={limit} characters"
                    )));
                }
                if value.chars().any(|ch| ch.is_control()) {
                    return Err(Error::Manifest(format!(
                        "{package}: store {name} contains control characters"
                    )));
                }
            }
        }
        if let Some(category) = &self.category {
            if !matches!(
                category.as_str(),
                "themes" | "system" | "utilities" | "development" | "multimedia"
            ) {
                return Err(Error::Manifest(format!(
                    "{package}: unknown store category '{category}'"
                )));
            }
        }
        if let Some(description) = &self.description {
            if description.trim().is_empty() || description.chars().count() > 2000 {
                return Err(Error::Manifest(format!(
                    "{package}: store description must be 1..=2000 characters"
                )));
            }
        }
        if self.highlights.len() > 6
            || self
                .highlights
                .iter()
                .any(|line| line.trim().is_empty() || line.chars().count() > 120)
        {
            return Err(Error::Manifest(format!(
                "{package}: store highlights must be at most 6 lines of 1..=120 characters"
            )));
        }
        if self.accent.len() > 2 || self.accent.iter().any(|color| !is_hex_color(color)) {
            return Err(Error::Manifest(format!(
                "{package}: store accent must be one or two #rrggbb colours"
            )));
        }
        if let Some(banner) = &self.banner {
            if banner.starts_with('/') || banner.contains("..") || banner.contains('\\') {
                return Err(Error::Manifest(format!(
                    "{package}: store banner must be a repository-relative path"
                )));
            }
        }
        Ok(())
    }
}

/// `deny_unknown_fields` is deliberately absent: the target-side `spk` parses
/// manifest.toml out of every package, and an older client must be able to
/// install a package that grew a new optional section.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub epoch: u64,
    pub version: String,
    pub release: u64,
    pub arch: String,
    pub abi: String,
    pub summary: String,
    #[serde(default)]
    pub description: String,
    pub license: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub packager: String,
    #[serde(default)]
    pub build_date: u64,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    /// Absolute conffile paths; these must also appear in `files`.
    #[serde(default)]
    pub conffiles: Vec<String>,
    #[serde(default)]
    pub scripts: bool,
    /// Path globs that activate this package's file trigger.
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub allow_setuid: bool,
    /// Graphical store presentation, published via the repository's store.json.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<PackageStoreMeta>,
    #[serde(default)]
    pub files: Vec<PackageFile>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let manifest: Manifest = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_toml(&self) -> Result<String> {
        let mut text = toml::to_string_pretty(self)?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Ok(text)
    }

    pub fn version(&self) -> Version {
        Version::new(self.epoch, self.version.clone(), self.release)
    }

    pub fn package_filename(&self) -> String {
        package_filename(
            &self.name,
            self.epoch,
            &self.version,
            self.release,
            &self.arch,
        )
    }

    pub fn files_by_path(&self) -> BTreeMap<&str, &PackageFile> {
        self.files.iter().map(|f| (f.path.as_str(), f)).collect()
    }

    pub fn validate(&self) -> Result<()> {
        validate_package_name(&self.name)?;
        validate_pkgver(&self.version).map_err(|e| Error::Manifest(e.to_string()))?;
        if self.release == 0 {
            return Err(Error::Manifest("release must be >= 1".into()));
        }
        validate_arch(&self.arch)?;
        validate_abi(&self.abi)?;
        if self.summary.trim().is_empty() {
            return Err(Error::Manifest("summary must not be empty".into()));
        }
        if self.license.trim().is_empty() {
            return Err(Error::Manifest("license must not be empty".into()));
        }
        if let Some(store) = &self.store {
            store.validate(&self.name)?;
        }
        for list in [
            (&self.depends, "depends"),
            (&self.provides, "provides"),
            (&self.conflicts, "conflicts"),
            (&self.replaces, "replaces"),
        ] {
            for item in list.0 {
                Dependency::parse(item).map_err(|e| {
                    Error::Manifest(format!("invalid {} entry '{item}': {e}", list.1))
                })?;
            }
        }

        let mut paths = BTreeSet::new();
        let mut non_dirs = BTreeSet::new();
        for file in &self.files {
            validate_absolute_path(&file.path)?;
            if !paths.insert(file.path.clone()) {
                return Err(Error::Manifest(format!(
                    "duplicate file entry '{}'",
                    file.path
                )));
            }
            let mode = file.mode_bits()?;
            if file.uid != 0 || file.gid != 0 {
                return Err(Error::Manifest(format!(
                    "{}: packages must be root-owned (uid/gid must be 0)",
                    file.path
                )));
            }
            if !self.allow_setuid && mode & 0o6000 != 0 {
                return Err(Error::Manifest(format!(
                    "{}: setuid/setgid bit requires allow_setuid = true",
                    file.path
                )));
            }
            match file.file_type {
                FileType::File => {
                    if !is_sha256_hex(&file.sha256) {
                        return Err(Error::Manifest(format!(
                            "{}: missing or malformed sha256",
                            file.path
                        )));
                    }
                    if file.target.is_some() {
                        return Err(Error::Manifest(format!(
                            "{}: regular files cannot have a link target",
                            file.path
                        )));
                    }
                    non_dirs.insert(file.path.clone());
                }
                FileType::Dir => {
                    if !file.sha256.is_empty() || file.size != 0 || file.target.is_some() {
                        return Err(Error::Manifest(format!(
                            "{}: directory entries carry no digest, size or target",
                            file.path
                        )));
                    }
                }
                FileType::Symlink => {
                    let target = file.target.as_deref().ok_or_else(|| {
                        Error::Manifest(format!("{}: symlink without target", file.path))
                    })?;
                    validate_symlink_target(&file.path, target)?;
                    if file.sha256 != crate::fsutil::sha256_bytes(target.as_bytes()) {
                        return Err(Error::Manifest(format!(
                            "{}: symlink sha256 does not match its target",
                            file.path
                        )));
                    }
                    non_dirs.insert(file.path.clone());
                }
            }
        }
        for path in &non_dirs {
            for ancestor in ancestors(path) {
                if non_dirs.contains(ancestor) {
                    return Err(Error::Manifest(format!(
                        "'{ancestor}' is a file or symlink but is also used as a directory"
                    )));
                }
            }
        }

        let files = self.files_by_path();
        if !self.triggers.is_empty() && !self.scripts {
            return Err(Error::Manifest("triggers require scripts = true".into()));
        }
        for trigger in &self.triggers {
            validate_trigger_pattern(trigger)?;
        }
        if !self.files.is_empty() {
            if !self.triggers.is_empty() {
                let trigger = trigger_path(&self.name);
                match files.get(trigger.as_str()) {
                    Some(file) if file.file_type == FileType::File => {}
                    _ => {
                        return Err(Error::Manifest(format!(
                            "trigger package requires regular file {trigger}"
                        )))
                    }
                }
            }
            for file in &self.files {
                if file.path.starts_with(HOOK_PREFIX) && !self.scripts {
                    return Err(Error::Manifest(format!(
                        "{}: package hooks require scripts = true",
                        file.path
                    )));
                }
            }
        }

        let mut conffiles = BTreeSet::new();
        for path in &self.conffiles {
            validate_absolute_path(path)?;
            if !conffiles.insert(path.as_str()) {
                return Err(Error::Manifest(format!("duplicate conffile '{path}'")));
            }
            // Source manifests may omit the generated files table; `spk build`
            // revalidates after filling it.
            if !self.files.is_empty() {
                match files.get(path.as_str()) {
                    Some(file) if file.file_type == FileType::File => {}
                    _ => {
                        return Err(Error::Manifest(format!(
                            "conffile '{path}' is not a regular file in this package"
                        )))
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn trigger_path(name: &str) -> String {
    format!("{TRIGGER_PREFIX}{name}.trigger")
}

fn validate_trigger_pattern(pattern: &str) -> Result<()> {
    if !pattern.starts_with('/') || pattern.ends_with('/') || pattern.contains('\0') {
        return Err(Error::Manifest(format!(
            "invalid trigger pattern '{pattern}'"
        )));
    }
    if pattern.len() > MAX_PATH_LEN {
        return Err(Error::Manifest(format!(
            "trigger pattern exceeds {MAX_PATH_LEN} bytes"
        )));
    }
    for component in pattern.split('/') {
        if component == ".." || component == "." {
            return Err(Error::Manifest(format!(
                "trigger pattern contains traversal: '{pattern}'"
            )));
        }
    }
    if pattern.chars().any(|ch| ch.is_control()) {
        return Err(Error::Manifest(
            "trigger pattern contains control characters".into(),
        ));
    }
    Ok(())
}

pub fn package_filename(name: &str, epoch: u64, version: &str, release: u64, arch: &str) -> String {
    let version = if epoch == 0 {
        version.to_string()
    } else {
        format!("{epoch}:{version}")
    };
    format!("{name}-{version}-{release}-{arch}.spk")
}

fn ancestors(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut end = path.len();
    while let Some(pos) = path[..end].rfind('/') {
        if pos == 0 {
            break;
        }
        out.push(&path[..pos]);
        end = pos;
    }
    out
}

pub fn validate_package_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        return Err(Error::Manifest(format!("invalid package name '{name}'")));
    }
    let mut bytes = name.bytes();
    let first = bytes.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(Error::Manifest(format!(
            "package name must start with an alphanumeric character: '{name}'"
        )));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'_' | b'+' | b'.' | b'-'))
    {
        return Err(Error::Manifest(format!(
            "package name contains invalid characters: '{name}'"
        )));
    }
    Ok(())
}

pub fn validate_arch(arch: &str) -> Result<()> {
    if ARCHES.contains(&arch) {
        Ok(())
    } else {
        Err(Error::Manifest(format!(
            "unsupported architecture '{arch}'"
        )))
    }
}

pub fn validate_abi(abi: &str) -> Result<()> {
    if abi.is_empty()
        || abi.len() > 64
        || !abi
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(Error::Manifest(format!("invalid ABI '{abi}'")));
    }
    Ok(())
}

/// Validate a path as it appears in the manifest: absolute, normalized, no
/// traversal, bounded length, no control characters or backslashes.
pub fn validate_absolute_path(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        return Err(Error::UnsafePath(format!("'{path}' is not absolute")));
    }
    if path == "/" {
        return Err(Error::UnsafePath(
            "the package root cannot be a file".into(),
        ));
    }
    if path.ends_with('/') {
        return Err(Error::UnsafePath(format!("'{path}' has a trailing slash")));
    }
    validate_components(&path[1..], path)?;
    if RESERVED_ROOT_FILES.contains(&&path[1..]) {
        return Err(Error::UnsafePath(format!(
            "'{path}' collides with package metadata"
        )));
    }
    if RESERVED_PATHS.contains(&path) {
        return Err(Error::UnsafePath(format!(
            "'{path}' is reserved for the package manager"
        )));
    }
    if RESERVED_PREFIXES.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    }) {
        return Err(Error::UnsafePath(format!(
            "'{path}' is inside a reserved package-manager path"
        )));
    }
    Ok(())
}

/// Validate a path as stored inside the payload tar: relative, no leading
/// slash, no `.`/`..` components.
pub fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(Error::UnsafePath("empty archive path".into()));
    }
    if path.starts_with('/') {
        return Err(Error::UnsafePath(format!("absolute archive path '{path}'")));
    }
    if path.ends_with('/') {
        return Err(Error::UnsafePath(format!("'{path}' has a trailing slash")));
    }
    if path.len() + 1 > MAX_PATH_LEN {
        return Err(Error::UnsafePath(format!(
            "archive path would exceed the {MAX_PATH_LEN}-byte absolute path limit"
        )));
    }
    validate_components(path, path)
}

fn validate_components(path: &str, original: &str) -> Result<()> {
    if original.len() > MAX_PATH_LEN {
        return Err(Error::UnsafePath(format!(
            "path exceeds {MAX_PATH_LEN} bytes: '{original}'"
        )));
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err(Error::UnsafePath(format!(
                "empty path component in '{original}'"
            )));
        }
        if component == "." || component == ".." {
            return Err(Error::UnsafePath(format!(
                "path traversal component in '{original}'"
            )));
        }
        if component.len() > MAX_COMPONENT_LEN {
            return Err(Error::UnsafePath(format!(
                "path component exceeds {MAX_COMPONENT_LEN} bytes in '{original}'"
            )));
        }
        if component.contains('\\') {
            return Err(Error::UnsafePath(format!("backslash in path '{original}'")));
        }
        if component.chars().any(|c| c.is_control()) {
            return Err(Error::UnsafePath(format!(
                "control character in path '{original}'"
            )));
        }
    }
    Ok(())
}

/// A package symlink may point at an absolute in-image path or at a relative
/// path that stays inside the installation root. Relative targets that climb
/// above `/` are rejected.
pub fn validate_symlink_target(link_path: &str, target: &str) -> Result<()> {
    validate_absolute_path(link_path)?;
    if target.is_empty() {
        return Err(Error::UnsafePath(format!(
            "{link_path}: empty symlink target"
        )));
    }
    if target.len() > MAX_PATH_LEN {
        return Err(Error::UnsafePath(format!(
            "{link_path}: symlink target too long"
        )));
    }
    if target.contains('\\') || target.chars().any(|c| c.is_control()) {
        return Err(Error::UnsafePath(format!(
            "{link_path}: invalid character in symlink target"
        )));
    }
    if target.starts_with('/') {
        if target == "/" {
            return Ok(());
        }
        if target.ends_with('/') {
            return Err(Error::UnsafePath(format!(
                "{link_path}: non-normalized symlink target '{target}'"
            )));
        }
        for component in target.split('/').skip(1) {
            if component.is_empty() || component == "." || component == ".." {
                return Err(Error::UnsafePath(format!(
                    "{link_path}: non-normalized absolute symlink target '{target}'"
                )));
            }
        }
        return Ok(());
    }
    let parent = link_path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let mut depth = if parent.is_empty() {
        0isize
    } else {
        parent.split('/').filter(|c| !c.is_empty()).count() as isize
    };
    for component in target.split('/') {
        match component {
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return Err(Error::UnsafePath(format!(
                        "{link_path}: symlink target '{target}' escapes the install root"
                    )));
                }
            }
            "." | "" => {
                return Err(Error::UnsafePath(format!(
                    "{link_path}: non-normalized symlink target '{target}'"
                )))
            }
            _ => depth += 1,
        }
    }
    Ok(())
}

pub fn parse_mode(text: &str) -> Result<u32> {
    let digits = text.strip_prefix("0o").unwrap_or(text);
    if digits.len() < 3 || digits.len() > 4 {
        return Err(Error::Manifest(format!("invalid mode '{text}'")));
    }
    if !digits.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err(Error::Manifest(format!("invalid mode '{text}'")));
    }
    let mode = u32::from_str_radix(digits, 8)
        .map_err(|_| Error::Manifest(format!("invalid mode '{text}'")))?;
    Ok(mode & 0o7777)
}

pub fn format_mode(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

pub fn strip_special_bits(mode: u32, allow_setuid: bool) -> u32 {
    if allow_setuid {
        mode
    } else {
        mode & !0o6000
    }
}

pub fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Eq,
    Greater,
    GreaterEq,
    Less,
    LessEq,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub relation: Option<Relation>,
    pub version: Option<Version>,
}

impl Dependency {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() || input.chars().any(|c| c.is_whitespace()) {
            return Err(Error::Manifest(format!("malformed dependency '{input}'")));
        }
        let op_at = input.find(['<', '>', '=']);
        let Some(pos) = op_at else {
            validate_package_name(input)?;
            return Ok(Self {
                name: input.to_string(),
                relation: None,
                version: None,
            });
        };
        let name = &input[..pos];
        validate_package_name(name)?;
        let rest = &input[pos..];
        let (relation, version_text) = if let Some(v) = rest.strip_prefix(">=") {
            (Relation::GreaterEq, v)
        } else if let Some(v) = rest.strip_prefix("<=") {
            (Relation::LessEq, v)
        } else if let Some(v) = rest.strip_prefix('=') {
            (Relation::Eq, v)
        } else if let Some(v) = rest.strip_prefix('>') {
            (Relation::Greater, v)
        } else if let Some(v) = rest.strip_prefix('<') {
            (Relation::Less, v)
        } else {
            return Err(Error::Manifest(format!(
                "malformed dependency operator in '{input}'"
            )));
        };
        if version_text.is_empty() {
            return Err(Error::Manifest(format!(
                "missing version in dependency '{input}'"
            )));
        }
        let version = Version::parse(version_text)
            .map_err(|e| Error::Manifest(format!("dependency '{input}': {e}")))?;
        Ok(Self {
            name: name.to_string(),
            relation: Some(relation),
            version: Some(version),
        })
    }

    pub fn matches(&self, version: &Version) -> bool {
        let (Some(relation), Some(required)) = (self.relation, self.version.as_ref()) else {
            return true;
        };
        let ordering = version.cmp(required);
        match relation {
            Relation::Eq => ordering == std::cmp::Ordering::Equal,
            Relation::Greater => ordering == std::cmp::Ordering::Greater,
            Relation::GreaterEq => ordering != std::cmp::Ordering::Less,
            Relation::Less => ordering == std::cmp::Ordering::Less,
            Relation::LessEq => ordering != std::cmp::Ordering::Greater,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
name = "foo"
epoch = 0
version = "1.2.3"
release = 1
arch = "x86_64"
abi = "spaceos-1"
summary = "example"
description = "example package"
license = "MIT"
url = "https://example.invalid"
packager = "SpaceOS Build <build@spaceos>"
build_date = 1760000000

depends = ["bar>=2.0", "baz"]
provides = ["foo-abi=1"]
conflicts = ["foo-legacy"]
replaces = ["foo-old"]

conffiles = []
scripts = false
allow_setuid = false

[[files]]
path = "/usr/bin/foo"
mode = "0755"
uid = 0
gid = 0
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
size = 12345
"#;

    #[test]
    fn parses_plan_example() {
        let manifest = Manifest::from_toml(EXAMPLE).unwrap();
        assert_eq!(manifest.name, "foo");
        assert_eq!(manifest.version().to_string(), "1.2.3-1");
        assert_eq!(manifest.package_filename(), "foo-1.2.3-1-x86_64.spk");
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].mode_bits().unwrap(), 0o755);
    }

    #[test]
    fn tolerates_unknown_fields_but_validates_store() {
        // An older client must be able to install a package that grew a new
        // optional section, so unknown keys are ignored rather than rejected.
        let text = EXAMPLE.replace("summary = \"example\"", "summary = \"example\"\nbogus = 1");
        assert!(Manifest::from_toml(&text).is_ok());

        // The store section itself is validated.
        let bad = format!(
            "{EXAMPLE}\n[store]\ndisplay_name = \"x\"\ncategory = \"nonsense\"\n"
        );
        assert!(Manifest::from_toml(&bad).is_err());
        let good = format!(
            "{EXAMPLE}\n[store]\ndisplay_name = \"示例\"\ncategory = \"themes\"\naccent = [\"#3b82f6\"]\n"
        );
        let parsed = Manifest::from_toml(&good).unwrap();
        assert_eq!(
            parsed.store.as_ref().and_then(|s| s.display_name.as_deref()),
            Some("示例")
        );
    }

    #[test]
    fn path_policy() {
        for good in ["/usr/bin/foo", "/etc/foo.conf", "/a/b-c_d.e"] {
            assert!(validate_absolute_path(good).is_ok(), "{good}");
        }
        for bad in [
            "usr/bin/foo",
            "/",
            "/usr/",
            "/usr//bin",
            "/usr/../etc",
            "/usr/./bin",
            "/usr\\bin",
            "/manifest.toml",
            "/.INSTALL",
        ] {
            assert!(validate_absolute_path(bad).is_err(), "{bad}");
        }
        for good in ["usr/bin/foo", "a", "a-b_c.d"] {
            assert!(validate_relative_path(good).is_ok(), "{good}");
        }
        for bad in ["", "/usr", "../etc", "usr/../etc", "usr//bin", "usr/"] {
            assert!(validate_relative_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn epoch_enters_the_filename() {
        let manifest = Manifest::from_toml(&EXAMPLE.replace("epoch = 0", "epoch = 2")).unwrap();
        assert_eq!(manifest.package_filename(), "foo-2:1.2.3-1-x86_64.spk");
    }

    #[test]
    fn reserved_paths_and_spaceos_length_limit() {
        for bad in [
            "/var/lib/spk/installed.json",
            "/etc/spk/trusted.d/ed25519:x.pub",
        ] {
            assert!(validate_absolute_path(bad).is_err(), "{bad}");
        }
        assert!(validate_absolute_path("/var/lib/spkfoo/x").is_ok());
        let long = format!("/{}/{}", "a".repeat(254), "b".repeat(256));
        assert!(validate_absolute_path(&long).is_err());
        let at_limit = format!("/{}/{}", "a".repeat(254), "b".repeat(255));
        assert_eq!(at_limit.len(), MAX_PATH_LEN);
        assert!(validate_absolute_path(&at_limit).is_ok());
    }

    #[test]
    fn symlink_policy() {
        assert!(validate_symlink_target("/usr/lib/libfoo.so", "libfoo.so.1").is_ok());
        assert!(validate_symlink_target("/usr/bin/foo", "/bin/foo").is_ok());
        assert!(validate_symlink_target("/usr/bin/foo", "../lib/foo").is_ok());
        assert!(validate_symlink_target("/usr/bin/foo", "../../../etc/passwd").is_err());
        assert!(validate_symlink_target("/usr/bin/foo", "/usr/../etc/passwd").is_err());
        assert!(validate_symlink_target("/usr/bin/foo", "./foo").is_err());
        assert!(validate_symlink_target("/usr/bin/foo", "").is_err());
    }

    #[test]
    fn modes() {
        assert_eq!(parse_mode("0755").unwrap(), 0o755);
        assert_eq!(parse_mode("755").unwrap(), 0o755);
        assert_eq!(parse_mode("4755").unwrap(), 0o4755);
        assert_eq!(format_mode(0o755), "0755");
        assert_eq!(strip_special_bits(0o4755, false), 0o755);
        assert_eq!(strip_special_bits(0o4755, true), 0o4755);
        for bad in ["", "08", "999", "abc", "77777"] {
            assert!(parse_mode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn dependency_syntax() {
        assert_eq!(
            Dependency::parse("bar>=2.0").unwrap().relation,
            Some(Relation::GreaterEq)
        );
        assert_eq!(Dependency::parse("baz").unwrap().relation, None);
        assert_eq!(
            Dependency::parse("foo-abi=1").unwrap().relation,
            Some(Relation::Eq)
        );
        for bad in ["", "foo>=", ">=1.0", "foo=>1.0", "foo bar", "foo>>1"] {
            assert!(Dependency::parse(bad).is_err(), "{bad}");
        }
        let dep = Dependency::parse("bar>=2.0-1").unwrap();
        assert!(dep.matches(&Version::parse("2.0-1").unwrap()));
        assert!(dep.matches(&Version::parse("2.0-2").unwrap()));
        assert!(!dep.matches(&Version::parse("1.9-9").unwrap()));
    }
}
