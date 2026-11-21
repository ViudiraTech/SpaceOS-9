//
//       user/spk/spk/src/triggers.rs
//       apk-style file triggers
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;

use spk_core::manifest::trigger_path;

use crate::error::{Result, SpkError};
use crate::hooks::matches_glob;
use crate::i18n::{self, Lang};
use crate::sandbox;
use crate::solver::{Solution, TargetKind};
use crate::state::Paths;
use crate::transaction::Plan;

fn marker(paths: &Paths, txid: &str, package: &str) -> std::path::PathBuf {
    paths.state.join("triggers").join(txid).join(package)
}

pub(crate) fn run(
    paths: &Paths,
    plan: &Plan,
    solution: &Solution,
    txid: &str,
    lang: Lang,
) -> Result<()> {
    let changed: Vec<String> = plan.changed_paths().into_iter().collect();
    if changed.is_empty() {
        return Ok(());
    }
    let mut actions = std::collections::BTreeMap::new();
    for action in &solution.actions {
        let verb = match action.kind {
            TargetKind::Install => "install",
            _ => "upgrade",
        };
        actions.insert(action.to.name.clone(), verb);
    }
    for removal in &solution.removals {
        actions.insert(removal.name.clone(), "remove");
    }
    let marker_dir = paths.state.join("triggers").join(txid);
    fs::create_dir_all(&marker_dir)?;
    for package in plan.installed().packages.values() {
        if package.triggers.is_empty() {
            continue;
        }
        let matched = package.triggers.iter().any(|pattern| {
            let pattern = if pattern.starts_with('/') {
                pattern.clone()
            } else {
                format!("/{pattern}")
            };
            changed.iter().any(|path| matches_glob(&pattern, path))
        });
        if !matched {
            continue;
        }
        let marker = marker(paths, txid, &package.name);
        if marker.exists() {
            continue;
        }
        let script = paths
            .root
            .join(trigger_path(&package.name).trim_start_matches('/'));
        if !script.is_file() {
            return Err(SpkError::General(format!(
                "{}: trigger script is missing",
                script.display()
            )));
        }
        let version = format!("{}-{}", package.version, package.release);
        let action = actions.get(&package.name).copied().unwrap_or("change");
        match sandbox::run_trigger(
            paths,
            txid,
            &package.name,
            &version,
            &script.to_string_lossy(),
            action,
            &changed,
            lang,
        ) {
            Ok(()) => {
                fs::write(&marker, b"ok\n")?;
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    i18n::trigger_failed(lang, &package.name, &error.to_string())
                );
            }
        }
    }
    Ok(())
}
