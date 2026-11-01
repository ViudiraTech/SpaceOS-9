/*
 *
 *       user/desktop/src/uxconf/storage.rs
 *       Layered per-channel configuration files and atomic persistence
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Manages per-channel configuration files with a two-tier lookup hierarchy:
//! 1. System defaults (`/etc/uxconf/channels/<channel>.conf` + compiled schema)
//! 2. Persistent user/runtime overrides (`/var/lib/uxconf/channels/<channel>.conf`)
//!
//! Writes serialize deterministically in sorted property order into a sibling
//! temporary file, `fsync` it, and atomically `rename(2)` over the target file.

use crate::schema::{self, BUILTIN_CHANNELS};
use crate::{validate_channel, validate_property, ChangeEvent, ConfError, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_SYSTEM_DIR: &str = "/etc/uxconf/channels";
pub const DEFAULT_USER_DIR: &str = "/var/lib/uxconf/channels";
pub const FALLBACK_USER_DIR: &str = "/tmp/uxconf/channels";

/// Parse a channel `.conf` text payload into a sorted `property -> Value` map.
pub fn parse_channel_conf(raw: &str, channel: &str) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((prop_raw, val_raw)) = trimmed.split_once('=') else {
            continue;
        };
        let prop = prop_raw.trim();
        if validate_property(prop, false).is_err() {
            continue;
        }
        let Ok(val) = Value::decode_typed(val_raw.trim()) else {
            continue;
        };
        if schema::validate_schema_type(channel, prop, &val).is_err() {
            continue;
        }
        map.insert(prop.to_string(), val);
    }
    map
}

/// Encode a `property -> Value` map into deterministic human-readable channel `.conf` text.
pub fn encode_channel_conf(channel: &str, entries: &BTreeMap<String, Value>) -> String {
    let mut out = format!("# uxconf channel: {channel}\n");
    for (prop, val) in entries {
        out.push_str(prop);
        out.push_str(" = ");
        out.push_str(&val.encode_typed());
        out.push('\n');
    }
    out
}

/// Atomically write `content` to `path` via write-temp + `sync_all` + `rename`.
pub fn atomic_write_file(path: &Path, content: &str) -> Result<(), ConfError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ConfError::Io(format!("{}: {e}", parent.display())))?;
    }
    let tmp_name = format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("channel.conf"),
        std::process::id()
    );
    let tmp_path = path.with_file_name(tmp_name);
    {
        let mut file = File::create(&tmp_path)
            .map_err(|e| ConfError::Io(format!("{}: {e}", tmp_path.display())))?;
        file.write_all(content.as_bytes())
            .map_err(|e| ConfError::Io(format!("{}: {e}", tmp_path.display())))?;
        let _ = file.sync_all();
    }
    fs::rename(&tmp_path, path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        ConfError::Io(format!("rename -> {}: {e}", path.display()))
    })
}

#[derive(Default)]
struct ChannelLayer {
    base: BTreeMap<String, Value>,
    user: BTreeMap<String, Value>,
}

impl ChannelLayer {
    fn effective(&self, property: &str) -> Option<&Value> {
        self.user.get(property).or_else(|| self.base.get(property))
    }

    fn merged(&self, prefix: &str) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        for (k, v) in &self.base {
            if matches_prefix(k, prefix) {
                out.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in &self.user {
            if matches_prefix(k, prefix) {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    fn is_empty(&self) -> bool {
        self.base.is_empty() && self.user.is_empty()
    }
}

fn matches_prefix(property: &str, prefix: &str) -> bool {
    if prefix == "/" || prefix.is_empty() {
        return true;
    }
    if property == prefix {
        return true;
    }
    property
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Choose a writable user configuration directory.
pub fn resolve_user_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir.to_path_buf();
    }
    if let Ok(env_dir) = std::env::var("UXCONF_USER_DIR") {
        if !env_dir.is_empty() {
            return PathBuf::from(env_dir);
        }
    }
    let primary = PathBuf::from(DEFAULT_USER_DIR);
    if fs::create_dir_all(&primary).is_ok() {
        let probe = primary.join(".write_probe");
        if fs::write(&probe, b"").is_ok() {
            let _ = fs::remove_file(&probe);
            return primary;
        }
    }
    PathBuf::from(FALLBACK_USER_DIR)
}

/// Resolve the system defaults directory.
pub fn resolve_system_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir.to_path_buf();
    }
    if let Ok(env_dir) = std::env::var("UXCONF_SYSTEM_DIR") {
        if !env_dir.is_empty() {
            return PathBuf::from(env_dir);
        }
    }
    PathBuf::from(DEFAULT_SYSTEM_DIR)
}

/// In-memory layered configuration database backed by `system_dir` and `user_dir`.
pub struct ConfigStore {
    system_dir: PathBuf,
    user_dir: PathBuf,
    channels: BTreeMap<String, ChannelLayer>,
}

impl ConfigStore {
    /// Load all built-in schemas, system defaults and user override files from disk.
    pub fn open(system_dir: PathBuf, user_dir: PathBuf) -> Self {
        let mut store = Self {
            system_dir,
            user_dir,
            channels: BTreeMap::new(),
        };
        store.reload();
        store
    }

    pub fn reload(&mut self) {
        self.channels.clear();
        let mut discovered = BTreeSet::new();
        for &ch in &BUILTIN_CHANNELS {
            discovered.insert(ch.to_string());
        }
        for dir in [&self.system_dir, &self.user_dir] {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("conf") {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            if let Ok(valid) = validate_channel(stem) {
                                discovered.insert(valid);
                            }
                        }
                    }
                }
            }
        }

        for ch in discovered {
            let mut layer = ChannelLayer::default();
            for (k, v) in schema::channel_defaults(&ch) {
                layer.base.insert(k.to_string(), v);
            }
            let sys_file = self.system_dir.join(format!("{ch}.conf"));
            if let Ok(raw) = fs::read_to_string(&sys_file) {
                for (k, v) in parse_channel_conf(&raw, &ch) {
                    layer.base.insert(k, v);
                }
            }
            let usr_file = self.user_dir.join(format!("{ch}.conf"));
            if let Ok(raw) = fs::read_to_string(&usr_file) {
                for (k, v) in parse_channel_conf(&raw, &ch) {
                    layer.user.insert(k, v);
                }
            }
            if !layer.is_empty() {
                self.channels.insert(ch, layer);
            }
        }
    }

    pub fn list_channels(&self) -> Vec<String> {
        self.channels
            .iter()
            .filter(|(_, layer)| !layer.is_empty())
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn get(&self, channel: &str, property: &str) -> Result<Value, ConfError> {
        let ch = validate_channel(channel)?;
        let prop = validate_property(property, false)?;
        self.channels
            .get(&ch)
            .and_then(|layer| layer.effective(prop))
            .cloned()
            .ok_or(ConfError::NotFound)
    }

    pub fn get_all(
        &self,
        channel: &str,
        prefix: &str,
    ) -> Result<BTreeMap<String, Value>, ConfError> {
        let ch = validate_channel(channel)?;
        let pfx = if prefix.is_empty() {
            "/"
        } else {
            validate_property(prefix, true)?
        };
        Ok(self
            .channels
            .get(&ch)
            .map(|layer| layer.merged(pfx))
            .unwrap_or_default())
    }

    /// Set `(channel, property)` to `value`, persisting user overrides atomically.
    /// Returns `Some(ChangeEvent)` only when the effective value actually changed.
    pub fn set(
        &mut self,
        channel: &str,
        property: &str,
        value: Value,
    ) -> Result<Option<ChangeEvent>, ConfError> {
        let ch = validate_channel(channel)?;
        let prop = validate_property(property, false)?.to_string();
        schema::validate_schema_type(&ch, &prop, &value)?;

        let prev = self
            .channels
            .get(&ch)
            .and_then(|layer| layer.effective(&prop))
            .cloned();

        let layer = self.channels.entry(ch.clone()).or_default();
        layer.user.insert(prop.clone(), value.clone());
        let encoded = encode_channel_conf(&ch, &layer.user);
        let usr_path = self.user_dir.join(format!("{ch}.conf"));
        atomic_write_file(&usr_path, &encoded)?;

        if prev.as_ref() != Some(&value) {
            Ok(Some(ChangeEvent {
                channel: ch,
                property: prop,
                value: Some(value),
            }))
        } else {
            Ok(None)
        }
    }

    /// Reset a single property or recursive subtree back to system/schema defaults.
    /// Returns every `ChangeEvent` whose effective value changed (or `None` if removed).
    pub fn reset(
        &mut self,
        channel: &str,
        property: &str,
        recursive: bool,
    ) -> Result<Vec<ChangeEvent>, ConfError> {
        let ch = validate_channel(channel)?;
        let prop = validate_property(property, recursive)?.to_string();

        let Some(layer) = self.channels.get_mut(&ch) else {
            return Ok(Vec::new());
        };

        let keys_to_remove: Vec<String> = layer
            .user
            .keys()
            .filter(|k| {
                if recursive {
                    matches_prefix(k, &prop)
                } else {
                    k.as_str() == prop
                }
            })
            .cloned()
            .collect();

        if keys_to_remove.is_empty() {
            return Ok(Vec::new());
        }

        let mut changes = Vec::new();
        for key in keys_to_remove {
            let old_val = layer.user.remove(&key);
            let new_effective = layer.base.get(&key).cloned();
            if old_val != new_effective {
                changes.push(ChangeEvent {
                    channel: ch.clone(),
                    property: key,
                    value: new_effective,
                });
            }
        }

        let usr_path = self.user_dir.join(format!("{ch}.conf"));
        if layer.user.is_empty() {
            let _ = fs::remove_file(&usr_path);
        } else {
            let encoded = encode_channel_conf(&ch, &layer.user);
            atomic_write_file(&usr_path, &encoded)?;
        }
        if layer.is_empty() {
            self.channels.remove(&ch);
        }

        Ok(changes)
    }
}
