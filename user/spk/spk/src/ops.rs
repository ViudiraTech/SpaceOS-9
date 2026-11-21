//
//       user/spk/spk/src/ops.rs
//       High-level install / upgrade / remove / query orchestration
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use spk_core::db::{HeldPackage, InstallReason, InstalledPackage};
use spk_core::fsutil::atomic_write;
use spk_core::manifest::{Dependency, Relation};
use spk_core::version::Version;

use crate::check;
use crate::error::{Result, SpkError};
use crate::i18n::{self, Lang};
use crate::journal;
use crate::query;
use crate::repair;
use crate::repo;
use crate::solver::{self, Operation, PkgSpec, Solution};
use crate::state::{lock_exclusive, Paths, State, StateLock};
use crate::transaction::{self, OperationKind, Options};

#[derive(Clone, Copy, Debug, Default)]
pub struct ReportOptions {
    pub json: bool,
    pub print: bool,
}

impl ReportOptions {
    fn active(self) -> bool {
        self.json || self.print
    }
}

fn prepare(paths: &Paths, lang: Lang) -> Result<(StateLock, State)> {
    let lock = lock_exclusive(paths)?;
    if let Some(txid) = journal::pending_txid(paths)? {
        return Err(SpkError::General(i18n::unfinished_transaction(lang, &txid)));
    }
    let state = State::load(paths)?;
    Ok((lock, state))
}

fn parse_targets(targets: &[String]) -> Result<Vec<PkgSpec>> {
    targets
        .iter()
        .map(|target| solver::parse_spec(target))
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn install(
    paths: &Paths,
    urls: &[String],
    targets: &[String],
    needed: bool,
    override_hold: bool,
    noconfirm: bool,
    report: ReportOptions,
    lang: Lang,
) -> Result<()> {
    if targets.is_empty() {
        return Err(SpkError::Usage(
            "install requires at least one package".into(),
        ));
    }
    let (_lock, state) = prepare(paths, lang)?;
    let specs = parse_targets(targets)?;
    let repos = repo::load_repos(paths, urls)?;
    if !report.json {
        println!("{}", i18n::resolving(lang));
        println!("{}", i18n::looking_for_conflicts(lang));
    }
    let solution = solver::solve(
        &state,
        &repos,
        Operation::Install { specs, needed },
        lang,
        override_hold,
    )?;
    if !report.json {
        print_held_back(&solution, lang);
    }
    if report.active() {
        report_solution("install", &solution, &repos, &state, report, lang)?;
        return Ok(());
    }
    if solution.is_empty() {
        println!("{}", i18n::nothing_to_do(lang));
        return Ok(());
    }
    transaction::execute(
        paths,
        &state,
        solution,
        &repos,
        &Options {
            lang,
            noconfirm,
            purge: false,
            operation: OperationKind::Install,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub fn upgrade(
    paths: &Paths,
    urls: &[String],
    targets: &[String],
    allow_partial: bool,
    override_hold: bool,
    noconfirm: bool,
    report: ReportOptions,
    lang: Lang,
) -> Result<()> {
    let (_lock, state) = prepare(paths, lang)?;
    if !targets.is_empty() && !allow_partial {
        return Err(SpkError::General(i18n::partial_disabled(lang).to_string()));
    }
    let repos = repo::load_repos(paths, urls)?;
    if !report.json {
        if targets.is_empty() {
            println!("{}", i18n::starting_full_upgrade(lang));
        } else {
            println!("{}", i18n::resolving(lang));
            println!("{}", i18n::looking_for_conflicts(lang));
        }
    }
    let operation = if targets.is_empty() {
        Operation::UpgradeAll
    } else {
        Operation::UpgradeSome(targets.to_vec())
    };
    let solution = solver::solve(&state, &repos, operation, lang, override_hold)?;
    if !report.json {
        print_held_back(&solution, lang);
    }
    if report.active() {
        report_solution("upgrade", &solution, &repos, &state, report, lang)?;
        return Ok(());
    }
    if solution.is_empty() {
        println!("{}", i18n::nothing_to_do(lang));
        return Ok(());
    }
    transaction::execute(
        paths,
        &state,
        solution,
        &repos,
        &Options {
            lang,
            noconfirm,
            purge: false,
            operation: OperationKind::Upgrade,
        },
    )
}

pub fn downgrade(
    paths: &Paths,
    urls: &[String],
    targets: &[String],
    override_hold: bool,
    noconfirm: bool,
    report: ReportOptions,
    lang: Lang,
) -> Result<()> {
    if targets.is_empty() {
        return Err(SpkError::Usage(
            "downgrade requires package=<version> targets".into(),
        ));
    }
    let (_lock, state) = prepare(paths, lang)?;
    let specs = parse_targets(targets)?;
    let repos = repo::load_repos(paths, urls)?;
    if !report.json {
        println!("{}", i18n::resolving(lang));
        println!("{}", i18n::looking_for_conflicts(lang));
    }
    let solution = solver::solve(
        &state,
        &repos,
        Operation::Downgrade(specs),
        lang,
        override_hold,
    )?;
    if !report.json {
        print_held_back(&solution, lang);
    }
    if report.active() {
        report_solution("downgrade", &solution, &repos, &state, report, lang)?;
        return Ok(());
    }
    if solution.is_empty() {
        println!("{}", i18n::nothing_to_do(lang));
        return Ok(());
    }
    transaction::execute(
        paths,
        &state,
        solution,
        &repos,
        &Options {
            lang,
            noconfirm,
            purge: false,
            operation: OperationKind::Install,
        },
    )
}

pub fn reinstall(
    paths: &Paths,
    urls: &[String],
    targets: &[String],
    override_hold: bool,
    noconfirm: bool,
    report: ReportOptions,
    lang: Lang,
) -> Result<()> {
    if targets.is_empty() {
        return Err(SpkError::Usage(
            "reinstall requires at least one package".into(),
        ));
    }
    let (_lock, state) = prepare(paths, lang)?;
    let repos = repo::load_repos(paths, urls)?;
    if !report.json {
        println!("{}", i18n::resolving(lang));
        println!("{}", i18n::looking_for_conflicts(lang));
    }
    let solution = solver::solve(
        &state,
        &repos,
        Operation::Reinstall(targets.to_vec()),
        lang,
        override_hold,
    )?;
    if !report.json {
        print_held_back(&solution, lang);
    }
    if report.active() {
        report_solution("reinstall", &solution, &repos, &state, report, lang)?;
        return Ok(());
    }
    if solution.is_empty() {
        println!("{}", i18n::nothing_to_do(lang));
        return Ok(());
    }
    transaction::execute(
        paths,
        &state,
        solution,
        &repos,
        &Options {
            lang,
            noconfirm,
            purge: false,
            operation: OperationKind::Install,
        },
    )
}

pub fn remove(
    paths: &Paths,
    targets: &[String],
    recursive: bool,
    purge: bool,
    noconfirm: bool,
    report: ReportOptions,
    lang: Lang,
) -> Result<()> {
    if targets.is_empty() {
        return Err(SpkError::Usage(
            "remove requires at least one package".into(),
        ));
    }
    let (_lock, state) = prepare(paths, lang)?;
    let mut removal_names = BTreeSet::new();
    for name in targets {
        if !state.installed.packages.contains_key(name) {
            return Err(SpkError::General(i18n::not_installed(lang, name)));
        }
        removal_names.insert(name.clone());
    }
    let mut remaining: BTreeSet<String> = state
        .installed
        .packages
        .keys()
        .filter(|name| !removal_names.contains(*name))
        .cloned()
        .collect();
    if recursive {
        let reachable = reachable(&state, &remaining);
        let orphans: Vec<String> = remaining
            .iter()
            .filter(|name| {
                let package = &state.installed.packages[*name];
                package.install_reason == InstallReason::Dependency && !reachable.contains(*name)
            })
            .cloned()
            .collect();
        for orphan in orphans {
            removal_names.insert(orphan);
        }
        remaining.retain(|name| !removal_names.contains(name));
    }
    if !report.json {
        println!("{}", i18n::checking_dependencies(lang));
    }
    let removals: Vec<InstalledPackage> = removal_names
        .iter()
        .filter_map(|name| state.installed.packages.get(name).cloned())
        .collect();
    check_remove_dependencies(&state, &remaining, &removals, lang)?;
    let solution = Solution {
        assignment: BTreeMap::new(),
        actions: Vec::new(),
        removals,
        explicit: BTreeSet::new(),
        held_back: Vec::new(),
        cleared_holds: BTreeSet::new(),
    };
    if report.active() {
        report_solution("remove", &solution, &[], &state, report, lang)?;
        return Ok(());
    }
    transaction::execute(
        paths,
        &state,
        solution,
        &[],
        &Options {
            lang,
            noconfirm,
            purge,
            operation: OperationKind::Remove,
        },
    )
}

fn reachable(state: &State, remaining: &BTreeSet<String>) -> BTreeSet<String> {
    let mut reachable = BTreeSet::new();
    let mut queue: VecDeque<String> = remaining
        .iter()
        .filter(|name| {
            state.world.packages.contains(*name)
                || state.installed.packages[*name].install_reason == InstallReason::Explicit
        })
        .cloned()
        .collect();
    while let Some(name) = queue.pop_front() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        let package = &state.installed.packages[&name];
        for dependency in parse_dependencies(&package.depends) {
            for other in remaining {
                if other == &name || reachable.contains(other) {
                    continue;
                }
                if package_matches(&state.installed.packages[other], &dependency) {
                    queue.push_back(other.clone());
                }
            }
        }
    }
    reachable
}

fn check_remove_dependencies(
    state: &State,
    remaining: &BTreeSet<String>,
    removals: &[InstalledPackage],
    lang: Lang,
) -> Result<()> {
    for name in remaining {
        let package = &state.installed.packages[name];
        for dependency in parse_dependencies(&package.depends) {
            let satisfied = remaining
                .iter()
                .any(|other| package_matches(&state.installed.packages[other], &dependency));
            if !satisfied {
                let removed = removals
                    .iter()
                    .find(|removal| package_matches(removal, &dependency))
                    .map(|removal| removal.name.clone())
                    .unwrap_or_else(|| "?".into());
                return Err(SpkError::Dependency {
                    message: i18n::solver_header_dependencies(lang).to_string(),
                    lines: vec![i18n::removing_breaks(
                        lang,
                        &removed,
                        &display_dependency(&dependency),
                        name,
                    )],
                });
            }
        }
    }
    Ok(())
}

fn parse_dependencies(items: &[String]) -> Vec<Dependency> {
    items
        .iter()
        .filter_map(|item| Dependency::parse(item).ok())
        .collect()
}

fn package_matches(package: &InstalledPackage, spec: &Dependency) -> bool {
    let version = Version::new(package.epoch, package.version.clone(), package.release);
    if spec.name == package.name && spec.matches(&version) {
        return true;
    }
    package.provides.iter().any(|item| {
        let Ok(provision) = Dependency::parse(item) else {
            return false;
        };
        if provision.name != spec.name {
            return false;
        }
        let provided = provision.version;
        match (&spec.version, provided) {
            (None, _) => true,
            (Some(_), Some(version)) => spec.matches(&version),
            (Some(_), None) => false,
        }
    })
}

fn display_dependency(dependency: &Dependency) -> String {
    let name = &dependency.name;
    match (&dependency.relation, &dependency.version) {
        (Some(relation), Some(version)) => {
            let operator = match relation {
                Relation::Eq => "=",
                Relation::Greater => ">",
                Relation::GreaterEq => ">=",
                Relation::Less => "<",
                Relation::LessEq => "<=",
            };
            format!("{name}{operator}{version}")
        }
        _ => name.clone(),
    }
}

fn report_solution(
    operation: &str,
    solution: &Solution,
    repos: &[crate::repo::LoadedRepo],
    state: &State,
    report: ReportOptions,
    lang: Lang,
) -> Result<()> {
    let mut packages = Vec::new();
    let mut actions: Vec<_> = solution.actions.iter().collect();
    actions.sort_by(|a, b| a.to.name.cmp(&b.to.name));
    for action in actions {
        let (repo, filename, sha256, size) = match (&action.to.origin, &action.to.repo_pkg) {
            (solver::Origin::Repo(index), Some(package)) => (
                repos
                    .get(*index)
                    .map(|repo| repo.name.clone())
                    .unwrap_or_default(),
                package.filename.clone(),
                package.sha256.clone(),
                package.size,
            ),
            _ => (String::new(), String::new(), String::new(), 0),
        };
        packages.push(serde_json::json!({
            "name": action.to.name,
            "from": action.from.as_ref().map(|old| format!("{}-{}", old.version, old.release)),
            "to": action.to.version.to_string(),
            "action": match action.kind {
                solver::TargetKind::Install => "install",
                solver::TargetKind::Upgrade => "upgrade",
                solver::TargetKind::Downgrade => "downgrade",
                solver::TargetKind::Reinstall => "reinstall",
            },
            "repo": repo,
            "filename": filename,
            "sha256": sha256,
            "size": size,
        }));
    }
    let mut removals: Vec<serde_json::Value> = solution
        .removals
        .iter()
        .map(|package| {
            serde_json::json!({
                "name": package.name,
                "version": format!("{}-{}", package.version, package.release),
            })
        })
        .collect();
    removals.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let held_back: Vec<serde_json::Value> = solution
        .held_back
        .iter()
        .map(|held| {
            serde_json::json!({
                "name": held.name,
                "installed": held.installed.to_string(),
                "available": held.available.to_string(),
            })
        })
        .collect();
    let payload = serde_json::json!({
        "schema": 1,
        "operation": operation,
        "nothing_to_do": solution.is_empty(),
        "packages": packages,
        "removals": removals,
        "held_back": held_back,
        "final_package_count": if solution.actions.is_empty() && !solution.removals.is_empty() {
            state.installed.packages.len().saturating_sub(solution.removals.len())
        } else if solution.is_empty() {
            state.installed.packages.len()
        } else {
            solution.assignment.len()
        },
    });
    if report.json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    println!();
    for action in &solution.actions {
        let from = action
            .from
            .as_ref()
            .map(|old| format!("{}-{} -> ", old.version, old.release))
            .unwrap_or_default();
        let verb = match action.kind {
            solver::TargetKind::Install => "install",
            solver::TargetKind::Upgrade => "upgrade",
            solver::TargetKind::Downgrade => "downgrade",
            solver::TargetKind::Reinstall => "reinstall",
        };
        println!("{verb} {} {from}{}", action.to.name, action.to.version);
    }
    for removal in &solution.removals {
        println!(
            "remove {} {}-{}",
            removal.name, removal.version, removal.release
        );
    }
    if solution.is_empty() {
        println!("{}", i18n::nothing_to_do(lang));
    }
    Ok(())
}

fn print_held_back(solution: &Solution, lang: Lang) {
    for held in &solution.held_back {
        eprintln!(
            "{}",
            i18n::held_back(
                lang,
                &held.name,
                &held.installed.to_string(),
                &held.available.to_string(),
            )
        );
    }
}

fn write_holds(paths: &Paths, holds: &spk_core::db::HoldsDb) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(holds)?;
    bytes.push(b'\n');
    atomic_write(&paths.db_path("holds.json"), &bytes, 0o644)?;
    Ok(())
}

pub fn hold(paths: &Paths, targets: &[String], lang: Lang) -> Result<()> {
    if targets.is_empty() {
        return hold_list(paths);
    }
    let (_lock, state) = prepare(paths, lang)?;
    let mut holds = state.holds.clone();
    for target in targets {
        let spec = solver::parse_spec(target)?;
        let Some(package) = state.installed.packages.get(&spec.name) else {
            return Err(SpkError::General(i18n::not_installed(lang, &spec.name)));
        };
        let version = Version::new(package.epoch, package.version.clone(), package.release);
        if let Some(wanted) = &spec.version {
            if wanted != &version {
                return Err(SpkError::General(format!(
                    "{} is installed at {version}, not {wanted}",
                    spec.name
                )));
            }
        }
        holds.packages.insert(
            spec.name.clone(),
            HeldPackage {
                epoch: package.epoch,
                version: package.version.clone(),
                release: package.release,
            },
        );
        println!("{} {}", spec.name, version);
    }
    write_holds(paths, &holds)
}

pub fn unhold(paths: &Paths, targets: &[String], lang: Lang) -> Result<()> {
    if targets.is_empty() {
        return Err(SpkError::Usage(
            "unhold requires at least one package".into(),
        ));
    }
    let (_lock, state) = prepare(paths, lang)?;
    let mut holds = state.holds.clone();
    for name in targets {
        if holds.packages.remove(name).is_none() {
            eprintln!("warning: {} is not held", name);
        } else {
            println!("{}", name);
        }
    }
    write_holds(paths, &holds)
}

pub fn hold_list(paths: &Paths) -> Result<()> {
    let state = State::load(paths)?;
    for (name, held) in &state.holds.packages {
        println!("{} {}", name, held.version());
    }
    Ok(())
}

pub fn list(paths: &Paths) -> Result<()> {
    let state = State::load(paths)?;
    query::list(&state)
}

pub fn info(paths: &Paths, name: &str, lang: Lang) -> Result<()> {
    let state = State::load(paths)?;
    query::info(&state, name, lang)
}

pub fn files(paths: &Paths, name: &str, lang: Lang) -> Result<()> {
    let state = State::load(paths)?;
    query::files(&state, name, lang)
}

pub fn owns(paths: &Paths, path: &str, lang: Lang) -> Result<()> {
    let state = State::load(paths)?;
    query::owns(&state, path, lang)
}

pub fn check_database(paths: &Paths, lang: Lang) -> Result<()> {
    let state = State::load(paths)?;
    check::check(paths, &state, lang)
}

pub fn repair_database(paths: &Paths, lang: Lang) -> Result<()> {
    repair::repair(paths, lang)
}

pub fn trust_add(paths: &Paths, key: &std::path::Path, lang: Lang) -> Result<()> {
    let _lock = lock_exclusive(paths)?;
    let key = repo::trust_add(paths, key)?;
    println!("{}", i18n::trust_added(lang, &key.key_id));
    Ok(())
}

pub fn trust_list(paths: &Paths) -> Result<()> {
    for key in repo::trust_list(paths)? {
        if key.comment.is_empty() {
            println!("{}", key.key_id);
        } else {
            println!("{} {}", key.key_id, key.comment);
        }
    }
    Ok(())
}
