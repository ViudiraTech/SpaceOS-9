//
//       user/spk/spk/src/hooks.rs
//       pacman-style post/pre transaction hooks
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use spk_core::manifest::{Manifest, HOOK_PREFIX};

use crate::error::{Result, SpkError};
use crate::i18n::{self, Lang};
use crate::sandbox;
use crate::state::Paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Operation {
    Install,
    Upgrade,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerType {
    Package,
    Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    PreTransaction,
    PostTransaction,
}

#[derive(Clone, Debug)]
pub struct Hook {
    pub path: PathBuf,
    pub operations: BTreeSet<Operation>,
    pub trigger_type: TriggerType,
    pub targets: Vec<String>,
    pub description: String,
    pub when: When,
    pub exec: String,
    pub package: Option<String>,
    pub stage: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Context {
    pub packages: BTreeSet<String>,
    pub paths: BTreeSet<String>,
    pub operations: BTreeSet<Operation>,
}

#[derive(Clone, Debug)]
pub struct HookPackage {
    pub name: String,
    pub manifest: Manifest,
    pub dir: PathBuf,
}

fn parse_operations(value: &str) -> Result<BTreeSet<Operation>> {
    let mut out = BTreeSet::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let operation = match item.to_ascii_lowercase().as_str() {
            "install" => Operation::Install,
            "upgrade" => Operation::Upgrade,
            "remove" => Operation::Remove,
            other => {
                return Err(SpkError::General(format!(
                    "hook has unknown Operation '{other}'"
                )))
            }
        };
        out.insert(operation);
    }
    Ok(out)
}

fn parse_hook(path: &Path) -> Result<Hook> {
    let text = fs::read_to_string(path)?;
    let mut section = String::new();
    let mut operations = BTreeSet::new();
    let mut trigger_type = None;
    let mut targets = Vec::new();
    let mut description = None;
    let mut when = None;
    let mut exec = None;
    for (line_number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(SpkError::General(format!(
                "{}:{}: expected key = value",
                path.display(),
                line_number + 1
            )));
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match (section.as_str(), key.as_str()) {
            ("trigger", "operation") => operations.extend(parse_operations(value)?),
            ("trigger", "type") => {
                trigger_type = Some(match value.to_ascii_lowercase().as_str() {
                    "package" => TriggerType::Package,
                    "path" => TriggerType::Path,
                    other => {
                        return Err(SpkError::General(format!(
                            "{}: unknown hook Type '{other}'",
                            path.display()
                        )))
                    }
                })
            }
            ("trigger", "target") => {
                targets.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|target| !target.is_empty())
                        .map(String::from),
                );
            }
            ("action", "description") => description = Some(value.to_string()),
            ("action", "when") => {
                when = Some(match value.to_ascii_lowercase().as_str() {
                    "pretransaction" => When::PreTransaction,
                    "posttransaction" => When::PostTransaction,
                    other => {
                        return Err(SpkError::General(format!(
                            "{}: unknown hook When '{other}'",
                            path.display()
                        )))
                    }
                })
            }
            ("action", "exec") => exec = Some(value.to_string()),
            ("action", "abortonfail") | ("action", "needs_targets") => {}
            _ => {}
        }
    }
    let trigger_type = trigger_type.ok_or_else(|| {
        SpkError::General(format!("{}: hook has no Trigger/Type", path.display()))
    })?;
    let exec = exec
        .ok_or_else(|| SpkError::General(format!("{}: hook has no Action/Exec", path.display())))?;
    let description = description.unwrap_or_else(|| exec.clone());
    let when = when.unwrap_or(When::PostTransaction);
    Ok(Hook {
        path: path.to_path_buf(),
        operations,
        trigger_type,
        targets,
        description,
        when,
        exec,
        package: None,
        stage: None,
    })
}

fn secure_system_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(SpkError::General(format!(
            "{} must be a root-owned regular file not writable by group/other",
            path.display()
        )));
    }
    Ok(())
}

fn hook_files(
    paths: &Paths,
    installed: &spk_core::db::InstalledDb,
    packages: &[HookPackage],
) -> Result<Vec<Hook>> {
    let mut seen = BTreeSet::new();
    let mut hooks = Vec::new();
    let system = paths.root.join("etc/spk/hooks.d");
    if system.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&system)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "hook")
            })
            .collect();
        entries.sort();
        for path in entries {
            secure_system_file(&path)?;
            let mut hook = parse_hook(&path)?;
            hook.package = None;
            hook.stage = None;
            if seen.insert(path.clone()) {
                hooks.push(hook);
            }
        }
    }
    for package in installed.packages.values() {
        for file in &package.files {
            if !file.path.starts_with(HOOK_PREFIX) || !file.path.ends_with(".hook") {
                continue;
            }
            let path = paths.root.join(file.path.trim_start_matches('/'));
            if !path.is_file() || !seen.insert(path.clone()) {
                continue;
            }
            let mut hook = parse_hook(&path)?;
            hook.package = Some(package.name.clone());
            hook.stage = None;
            hooks.push(hook);
        }
    }
    for package in packages {
        for file in &package.manifest.files {
            if !file.path.starts_with(HOOK_PREFIX) || !file.path.ends_with(".hook") {
                continue;
            }
            let path = package.dir.join(file.path.trim_start_matches('/'));
            if !path.is_file() || !seen.insert(path.clone()) {
                continue;
            }
            let mut hook = parse_hook(&path)?;
            hook.package = Some(package.name.clone());
            hook.stage = Some(package.dir.clone());
            hooks.push(hook);
        }
    }
    Ok(hooks)
}

pub fn matches_glob(pattern: &str, value: &str) -> bool {
    fn walk(pattern: &[u8], value: &[u8]) -> bool {
        if pattern.is_empty() {
            return value.is_empty();
        }
        match pattern[0] {
            b'*' => {
                for index in 0..=value.len() {
                    if index > 0 && value[index - 1] == b'/' {
                        break;
                    }
                    if walk(&pattern[1..], &value[index..]) {
                        return true;
                    }
                }
                false
            }
            b'?' => !value.is_empty() && value[0] != b'/' && walk(&pattern[1..], &value[1..]),
            byte => !value.is_empty() && value[0] == byte && walk(&pattern[1..], &value[1..]),
        }
    }
    walk(pattern.as_bytes(), value.as_bytes())
}

fn hook_matches(hook: &Hook, context: &Context) -> bool {
    if hook.operations.is_disjoint(&context.operations) {
        return false;
    }
    hook.targets.iter().any(|target| match hook.trigger_type {
        TriggerType::Package => context
            .packages
            .iter()
            .any(|package| matches_glob(target, package)),
        TriggerType::Path => {
            let target = if target.starts_with('/') {
                target.clone()
            } else {
                format!("/{target}")
            };
            context.paths.iter().any(|path| matches_glob(&target, path))
        }
    })
}

fn marker(paths: &Paths, txid: &str, hook: &Hook) -> PathBuf {
    paths.state.join("hooks").join(txid).join(
        hook.path
            .to_string_lossy()
            .trim_start_matches('/')
            .replace('/', "_"),
    )
}

fn run_hooks(
    paths: &Paths,
    txid: &str,
    hooks: &[Hook],
    context: &Context,
    when: When,
    lang: Lang,
    abort_on_failure: bool,
) -> Result<()> {
    let selected: Vec<&Hook> = hooks
        .iter()
        .filter(|hook| hook.when == when && hook_matches(hook, context))
        .collect();
    if selected.is_empty() {
        return Ok(());
    }
    println!(
        "{}",
        match when {
            When::PreTransaction => i18n::running_pre_hooks(lang),
            When::PostTransaction => i18n::running_post_hooks(lang),
        }
    );
    fs::create_dir_all(paths.state.join("hooks").join(txid))?;
    for (index, hook) in selected.iter().enumerate() {
        let marker = marker(paths, txid, hook);
        if marker.exists() {
            continue;
        }
        println!(
            "{}",
            i18n::hook_progress(lang, index + 1, selected.len(), &hook.description)
        );
        let result = match &hook.package {
            Some(package) => {
                let stage = hook.stage.clone().unwrap_or_else(|| {
                    paths
                        .sandbox
                        .join(txid)
                        .join(format!("{package}-hook-stage"))
                });
                if !stage.exists() {
                    let _ = fs::create_dir_all(&stage);
                }
                sandbox::run_package_hook(paths, txid, package, &hook.exec, &stage, lang)
            }
            None => sandbox::run_system_hook(&hook.exec, lang),
        };
        match result {
            Ok(()) => {
                fs::write(&marker, b"ok\n")?;
            }
            Err(error) => {
                if abort_on_failure {
                    return Err(error);
                }
                eprintln!(
                    "{}",
                    i18n::hook_failed(lang, &hook.description, &error.to_string())
                );
            }
        }
    }
    Ok(())
}

pub fn load(
    paths: &Paths,
    installed: &spk_core::db::InstalledDb,
    packages: &[HookPackage],
) -> Result<Vec<Hook>> {
    hook_files(paths, installed, packages)
}

pub fn run_pre(
    paths: &Paths,
    txid: &str,
    hooks: &[Hook],
    context: &Context,
    lang: Lang,
) -> Result<()> {
    run_hooks(
        paths,
        txid,
        hooks,
        context,
        When::PreTransaction,
        lang,
        true,
    )
}

pub fn run_post(
    paths: &Paths,
    txid: &str,
    hooks: &[Hook],
    context: &Context,
    lang: Lang,
) -> Result<()> {
    run_hooks(
        paths,
        txid,
        hooks,
        context,
        When::PostTransaction,
        lang,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_trigger_action_and_glob() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.hook");
        std::fs::write(
            &path,
            "[Trigger]\nOperation = Install\nOperation = Upgrade\nType = Path\nTarget = usr/lib/firmware/*\n\n[Action]\nDescription = 更新固件索引\nWhen = PostTransaction\nExec = /usr/bin/update-fw\n",
        )
        .unwrap();
        let hook = parse_hook(&path).unwrap();
        assert_eq!(hook.operations.len(), 2);
        assert_eq!(hook.trigger_type, TriggerType::Path);
        assert_eq!(hook.targets, vec!["usr/lib/firmware/*"]);
        assert_eq!(hook.when, When::PostTransaction);
        assert_eq!(hook.description, "更新固件索引");
        assert!(matches_glob(
            "/usr/lib/firmware/*",
            "/usr/lib/firmware/a.bin"
        ));
        assert!(!matches_glob(
            "/usr/lib/firmware/*",
            "/usr/lib/firmware/sub/a.bin"
        ));
        assert!(matches_glob("/bin/??", "/bin/sh"));
        assert!(!matches_glob("/bin/??", "/bin/bash"));
    }

    #[test]
    fn package_trigger_defaults_to_post() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg.hook");
        std::fs::write(
            &path,
            "[Trigger]\nOperation = Install\nType = Package\nTarget = foo\n[Action]\nExec = echo ok\n",
        )
        .unwrap();
        let hook = parse_hook(&path).unwrap();
        assert_eq!(hook.when, When::PostTransaction);
        assert_eq!(hook.description, "echo ok");
    }

    #[test]
    fn malformed_hook_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.hook");
        std::fs::write(&path, "[Trigger]\nOperation = Explode\n").unwrap();
        assert!(parse_hook(&path).is_err());
    }
}
