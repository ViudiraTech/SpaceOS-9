//
//       user/spk/spk/src/solver.rs
//       Backtracking dependency solver for install, upgrade and downgrade
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};

use spk_core::db::InstalledPackage;
use spk_core::index::IndexPackage;
use spk_core::manifest::{validate_package_name, Dependency};
use spk_core::version::Version;

use crate::error::{Result, SpkError};
use crate::i18n::{self, Lang};
use crate::repo::LoadedRepo;
use crate::state::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Installed,
    Repo(usize),
}

#[derive(Clone, Debug)]
pub struct Provision {
    pub name: String,
    pub version: Option<Version>,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    pub version: Version,
    pub arch: String,
    pub abi: String,
    pub depends: Vec<Dependency>,
    pub provides: Vec<Provision>,
    pub conflicts: Vec<Dependency>,
    pub replaces: Vec<Dependency>,
    pub origin: Origin,
    pub repo_pkg: Option<IndexPackage>,
    pub installed_pkg: Option<InstalledPackage>,
}

impl Candidate {
    fn from_index(package: IndexPackage, repo: usize) -> Result<Self> {
        let depends = parse_dependencies(&package.depends)?;
        let provides = parse_provisions(&package.provides)?;
        let conflicts = parse_dependencies(&package.conflicts)?;
        let replaces = parse_dependencies(&package.replaces)?;
        Ok(Self {
            name: package.name.clone(),
            version: package.version(),
            arch: package.arch.clone(),
            abi: package.abi.clone(),
            depends,
            provides,
            conflicts,
            replaces,
            origin: Origin::Repo(repo),
            repo_pkg: Some(package),
            installed_pkg: None,
        })
    }

    fn from_installed(package: &InstalledPackage) -> Result<Self> {
        let depends = parse_dependencies(&package.depends)?;
        let provides = parse_provisions(&package.provides)?;
        let conflicts = parse_dependencies(&package.conflicts)?;
        let replaces = parse_dependencies(&package.replaces)?;
        Ok(Self {
            name: package.name.clone(),
            version: Version::new(package.epoch, package.version.clone(), package.release),
            arch: package.arch.clone(),
            abi: package.abi.clone(),
            depends,
            provides,
            conflicts,
            replaces,
            origin: Origin::Installed,
            repo_pkg: None,
            installed_pkg: Some(package.clone()),
        })
    }

    pub fn display(&self) -> String {
        format!("{}", self.version)
    }

    fn matches_spec(&self, spec: &Dependency) -> bool {
        if spec.name == self.name && spec.matches(&self.version) {
            return true;
        }
        self.provides.iter().any(|provision| {
            provision.name == spec.name
                && match (&spec.version, &provision.version) {
                    (None, _) => true,
                    (Some(_), Some(version)) => spec.matches(version),
                    (Some(_), None) => false,
                }
        })
    }

    fn satisfies(&self, dependency: &Dependency) -> bool {
        self.matches_spec(dependency)
    }

    fn conflicts_with(&self, other: &Self) -> bool {
        self.conflicts.iter().any(|spec| other.matches_spec(spec))
            || other.conflicts.iter().any(|spec| self.matches_spec(spec))
    }

    fn replaces(&self, other: &Self) -> bool {
        self.replaces.iter().any(|spec| other.matches_spec(spec))
    }
}

fn parse_dependencies(items: &[String]) -> Result<Vec<Dependency>> {
    items
        .iter()
        .map(|item| Dependency::parse(item).map_err(SpkError::from))
        .collect()
}

fn parse_provisions(items: &[String]) -> Result<Vec<Provision>> {
    items
        .iter()
        .map(|item| {
            let dependency = Dependency::parse(item).map_err(SpkError::from)?;
            match dependency.relation {
                None | Some(spk_core::manifest::Relation::Eq) => Ok(Provision {
                    name: dependency.name,
                    version: dependency.version,
                }),
                Some(_) => Err(SpkError::General(format!(
                    "provides entry '{item}' must be unversioned or use '='"
                ))),
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct PkgSpec {
    pub name: String,
    pub version: Option<Version>,
}

pub fn parse_spec(text: &str) -> Result<PkgSpec> {
    let (name, version) = match text.split_once('=') {
        Some((name, version)) => {
            let version =
                Version::parse(version).map_err(|error| SpkError::Usage(error.to_string()))?;
            (name, Some(version))
        }
        None => (text, None),
    };
    validate_package_name(name).map_err(SpkError::from)?;
    Ok(PkgSpec {
        name: name.to_string(),
        version,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Install,
    Upgrade,
    Downgrade,
    Reinstall,
}

#[derive(Clone, Debug)]
enum Request {
    Install { specs: Vec<PkgSpec>, needed: bool },
    UpgradeAll,
    UpgradeSome(Vec<String>),
    Downgrade(Vec<PkgSpec>),
    Reinstall(Vec<String>),
}

pub enum Operation {
    Install { specs: Vec<PkgSpec>, needed: bool },
    UpgradeAll,
    UpgradeSome(Vec<String>),
    Downgrade(Vec<PkgSpec>),
    Reinstall(Vec<String>),
}

impl Operation {
    fn into_request(self) -> Request {
        match self {
            Self::Install { specs, needed } => Request::Install { specs, needed },
            Self::UpgradeAll => Request::UpgradeAll,
            Self::UpgradeSome(names) => Request::UpgradeSome(names),
            Self::Downgrade(specs) => Request::Downgrade(specs),
            Self::Reinstall(names) => Request::Reinstall(names),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Action {
    pub from: Option<InstalledPackage>,
    pub to: Candidate,
    pub kind: TargetKind,
}

#[derive(Clone, Debug)]
pub struct HeldBack {
    pub name: String,
    pub installed: Version,
    pub available: Version,
}

#[derive(Clone, Debug)]
pub struct Solution {
    pub assignment: BTreeMap<String, Candidate>,
    pub actions: Vec<Action>,
    pub removals: Vec<InstalledPackage>,
    pub explicit: BTreeSet<String>,
    pub held_back: Vec<HeldBack>,
    pub cleared_holds: BTreeSet<String>,
}

impl Solution {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.removals.is_empty()
    }
}

#[derive(Clone, Debug)]
struct Pinned {
    version: Version,
    kind: TargetKind,
    repo: bool,
}

struct Solver<'a> {
    by_name: BTreeMap<String, Vec<Candidate>>,
    assignment: BTreeMap<String, Candidate>,
    pinned: BTreeMap<String, Pinned>,
    installed: BTreeMap<String, InstalledPackage>,
    steps: u64,
    path: BTreeSet<String>,
    lang: Lang,
    holds: BTreeMap<String, Version>,
    override_hold: bool,
    override_names: BTreeSet<String>,
    held_back: Vec<HeldBack>,
    _repos: &'a [LoadedRepo],
}

pub fn solve(
    state: &State,
    repos: &[LoadedRepo],
    operation: Operation,
    lang: Lang,
    override_hold: bool,
) -> Result<Solution> {
    let request = operation.into_request();
    let mut solver = Solver::new(state, repos, lang, override_hold)?;
    solver.plan(&request)?;
    solver.finish()
}

impl<'a> Solver<'a> {
    fn new(
        state: &'a State,
        repos: &'a [LoadedRepo],
        lang: Lang,
        override_hold: bool,
    ) -> Result<Self> {
        let mut by_name: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
        let mut installed = BTreeMap::new();
        for package in state.installed.packages.values() {
            let candidate = Candidate::from_installed(package)?;
            by_name
                .entry(candidate.name.clone())
                .or_default()
                .push(candidate);
            installed.insert(package.name.clone(), package.clone());
        }
        for (repo_index, repo) in repos.iter().enumerate() {
            for package in &repo.index.packages {
                let candidate = Candidate::from_index(package.clone(), repo_index)?;
                let name = candidate.name.clone();
                let entry = by_name.entry(name).or_default();
                // A version present in an earlier repository wins; the
                // installed candidate is always kept separately.
                let duplicate = entry.iter().any(|existing| {
                    matches!(existing.origin, Origin::Repo(_))
                        && existing.version == candidate.version
                });
                if !duplicate {
                    entry.push(candidate);
                }
            }
        }
        for candidates in by_name.values_mut() {
            candidates.sort_by(|a, b| {
                b.version
                    .cmp(&a.version)
                    .then_with(|| origin_rank(&a.origin).cmp(&origin_rank(&b.origin)))
            });
        }
        let mut holds = BTreeMap::new();
        for (name, held) in &state.holds.packages {
            let held = held.version();
            let Some(package) = installed.get(name) else {
                return Err(SpkError::General(format!(
                    "held package '{name}' is not installed"
                )));
            };
            let current = Version::new(package.epoch, package.version.clone(), package.release);
            if current != held {
                return Err(SpkError::General(format!(
                    "held package '{name}' is at {held}, installed version is {current}; run spk check or repair"
                )));
            }
            holds.insert(name.clone(), held);
        }
        Ok(Self {
            by_name,
            assignment: BTreeMap::new(),
            pinned: BTreeMap::new(),
            installed,
            steps: 0,
            path: BTreeSet::new(),
            lang,
            holds,
            override_hold,
            override_names: BTreeSet::new(),
            held_back: Vec::new(),
            _repos: repos,
        })
    }

    fn plan(&mut self, request: &Request) -> Result<()> {
        self.override_names = if self.override_hold {
            match request {
                Request::Install { specs, .. } => {
                    specs.iter().map(|spec| spec.name.clone()).collect()
                }
                Request::UpgradeAll => self.holds.keys().cloned().collect(),
                Request::UpgradeSome(names) => names.iter().cloned().collect(),
                Request::Downgrade(specs) => specs.iter().map(|spec| spec.name.clone()).collect(),
                Request::Reinstall(names) => names.iter().cloned().collect(),
            }
        } else {
            BTreeSet::new()
        };
        match request {
            Request::Install { specs, needed } => self.plan_install(specs, *needed)?,
            Request::UpgradeAll => self.plan_upgrade(None)?,
            Request::UpgradeSome(names) => self.plan_upgrade(Some(names))?,
            Request::Downgrade(specs) => self.plan_downgrade(specs)?,
            Request::Reinstall(names) => self.plan_reinstall(names)?,
        }
        Ok(())
    }

    fn plan_install(&mut self, specs: &[PkgSpec], needed: bool) -> Result<()> {
        for spec in specs {
            let installed = self.installed.get(&spec.name).cloned();
            let repo_candidate = if let Some(version) = &spec.version {
                self.repo_candidate(&spec.name, version)
            } else {
                self.newest_repo_candidate(&spec.name)
            };
            let (candidate, kind) = match (&installed, repo_candidate) {
                (Some(installed), Some(candidate)) => {
                    let installed_version = Version::new(
                        installed.epoch,
                        installed.version.clone(),
                        installed.release,
                    );
                    if installed_version == candidate.version {
                        if needed {
                            continue;
                        }
                        (candidate, TargetKind::Reinstall)
                    } else if installed_version < candidate.version {
                        (candidate, TargetKind::Upgrade)
                    } else if spec.version.is_some() {
                        (candidate, TargetKind::Downgrade)
                    } else {
                        // A locally newer version is not silently downgraded.
                        continue;
                    }
                }
                (Some(installed), None) => {
                    let installed_version = Version::new(
                        installed.epoch,
                        installed.version.clone(),
                        installed.release,
                    );
                    if spec.version.as_ref() == Some(&installed_version) && needed {
                        continue;
                    }
                    return Err(SpkError::General(i18n::unknown_target(
                        self.lang, &spec.name,
                    )));
                }
                (None, Some(candidate)) => (candidate, TargetKind::Install),
                (None, None) => {
                    return Err(SpkError::General(i18n::unknown_target(
                        self.lang, &spec.name,
                    )));
                }
            };
            if let Some(held) = self.holds.get(&spec.name) {
                if !self.override_names.contains(&spec.name) && candidate.version != *held {
                    return Err(self.failure(Failure::Held {
                        name: spec.name.clone(),
                        held: held.to_string(),
                        attempted: candidate.version.to_string(),
                    }));
                }
            }
            self.pin(&spec.name, &candidate, kind)?;
        }
        self.seed_assignment()
    }

    fn plan_upgrade(&mut self, names: Option<&[String]>) -> Result<()> {
        let targets: Vec<String> = match names {
            Some(names) => {
                for name in names {
                    if !self.installed.contains_key(name) {
                        return Err(SpkError::General(i18n::not_installed(self.lang, name)));
                    }
                }
                names.to_vec()
            }
            None => self.installed.keys().cloned().collect(),
        };
        for name in targets {
            let installed = self.installed.get(&name).expect("checked");
            let installed_version = Version::new(
                installed.epoch,
                installed.version.clone(),
                installed.release,
            );
            if let Some(held) = self.holds.get(&name) {
                if !self.override_names.contains(&name) {
                    if let Some(candidate) = self.newest_repo_candidate(&name) {
                        if candidate.version > *held {
                            self.held_back.push(HeldBack {
                                name: name.clone(),
                                installed: installed_version,
                                available: candidate.version,
                            });
                        }
                    }
                    continue;
                }
            }
            if let Some(candidate) = self.newest_repo_candidate(&name) {
                if candidate.version > installed_version {
                    self.pin(&name, &candidate, TargetKind::Upgrade)?;
                }
            }
        }
        self.seed_assignment()
    }

    fn plan_downgrade(&mut self, specs: &[PkgSpec]) -> Result<()> {
        for spec in specs {
            let version = spec.version.as_ref().ok_or_else(|| {
                SpkError::Usage(format!("downgrade {} requires =<version>", spec.name))
            })?;
            let installed = self
                .installed
                .get(&spec.name)
                .ok_or_else(|| SpkError::General(i18n::not_installed(self.lang, &spec.name)))?;
            let installed_version = Version::new(
                installed.epoch,
                installed.version.clone(),
                installed.release,
            );
            let candidate = self.repo_candidate(&spec.name, version).ok_or_else(|| {
                SpkError::General(i18n::unknown_target(
                    self.lang,
                    &format!("{}={}", spec.name, version),
                ))
            })?;
            if candidate.version >= installed_version {
                return Err(SpkError::General(format!(
                    "{} {} is not older than the installed {}",
                    spec.name, candidate.version, installed_version
                )));
            }
            if let Some(held) = self.holds.get(&spec.name) {
                if !self.override_names.contains(&spec.name) && candidate.version != *held {
                    return Err(self.failure(Failure::Held {
                        name: spec.name.clone(),
                        held: held.to_string(),
                        attempted: candidate.version.to_string(),
                    }));
                }
            }
            self.pin(&spec.name, &candidate, TargetKind::Downgrade)?;
        }
        self.seed_assignment()
    }

    fn plan_reinstall(&mut self, names: &[String]) -> Result<()> {
        for name in names {
            let installed = self
                .installed
                .get(name)
                .ok_or_else(|| SpkError::General(i18n::not_installed(self.lang, name)))?;
            let version = Version::new(
                installed.epoch,
                installed.version.clone(),
                installed.release,
            );
            let candidate = self
                .repo_candidate(name, &version)
                .ok_or_else(|| SpkError::General(i18n::unknown_target(self.lang, name)))?;
            self.pin(name, &candidate, TargetKind::Reinstall)?;
        }
        self.seed_assignment()
    }

    fn pin(&mut self, name: &str, candidate: &Candidate, kind: TargetKind) -> Result<()> {
        if let Some(existing) = self.pinned.get(name) {
            if existing.version != candidate.version || existing.kind != kind {
                return Err(SpkError::Usage(format!("conflicting targets for {name}")));
            }
            return Ok(());
        }
        self.pinned.insert(
            name.to_string(),
            Pinned {
                version: candidate.version.clone(),
                kind,
                repo: matches!(candidate.origin, Origin::Repo(_)),
            },
        );
        Ok(())
    }

    fn seed_assignment(&mut self) -> Result<()> {
        for (name, package) in self.installed.clone() {
            let candidate = Candidate::from_installed(&package)?;
            self.assignment.insert(name, candidate);
        }
        let pinned: Vec<(String, Pinned)> = self
            .pinned
            .iter()
            .map(|(name, pinned)| (name.clone(), pinned.clone()))
            .collect();
        for (name, pinned) in pinned {
            if !pinned.repo {
                continue;
            }
            let candidate = self
                .candidate_for_version(&name, &pinned.version)
                .ok_or_else(|| SpkError::General(i18n::unknown_target(self.lang, &name)))?;
            self.try_assign(candidate)?;
        }
        Ok(())
    }

    fn repo_candidate(&self, name: &str, version: &Version) -> Option<Candidate> {
        self.by_name.get(name).and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| {
                    matches!(candidate.origin, Origin::Repo(_)) && candidate.version == *version
                })
                .cloned()
        })
    }

    fn newest_repo_candidate(&self, name: &str) -> Option<Candidate> {
        self.by_name.get(name).and_then(|candidates| {
            candidates
                .iter()
                .filter(|candidate| matches!(candidate.origin, Origin::Repo(_)))
                .max_by(|a, b| a.version.cmp(&b.version))
                .cloned()
        })
    }

    fn candidate_for_version(&self, name: &str, version: &Version) -> Option<Candidate> {
        let candidates = self.by_name.get(name)?;
        candidates
            .iter()
            .find(|candidate| {
                candidate.version == *version && matches!(candidate.origin, Origin::Repo(_))
            })
            .or_else(|| {
                candidates
                    .iter()
                    .find(|candidate| candidate.version == *version)
            })
            .cloned()
    }

    fn try_assign(&mut self, candidate: Candidate) -> Result<()> {
        if let Some(held) = self.holds.get(&candidate.name) {
            if !self.override_names.contains(&candidate.name) && candidate.version != *held {
                return Err(self.failure(Failure::Held {
                    name: candidate.name.clone(),
                    held: held.to_string(),
                    attempted: candidate.version.to_string(),
                }));
            }
        }
        if let Some(pinned) = self.pinned.get(&candidate.name) {
            if pinned.version != candidate.version {
                return Err(self.failure(Failure::Pinned(candidate.name.clone())));
            }
            if pinned.repo && !matches!(candidate.origin, Origin::Repo(_)) {
                return Err(self.failure(Failure::Pinned(candidate.name.clone())));
            }
        }
        let mut removed = Vec::new();
        for (other_name, other) in self.assignment.clone() {
            if other_name == candidate.name {
                continue;
            }
            if other.replaces(&candidate) {
                return Err(self.failure(Failure::Conflict {
                    a: candidate.name.clone(),
                    av: candidate.version.to_string(),
                    b: other.name.clone(),
                    bv: other.version.to_string(),
                }));
            }
            if candidate.replaces(&other) || candidate.conflicts_with(&other) {
                if let Some(pinned) = self.pinned.get(&other_name) {
                    if pinned.version == other.version {
                        return Err(self.failure(Failure::Conflict {
                            a: candidate.name.clone(),
                            av: candidate.version.to_string(),
                            b: other.name.clone(),
                            bv: other.version.to_string(),
                        }));
                    }
                }
                if candidate.replaces(&other) {
                    self.assignment.remove(&other_name);
                    removed.push(other_name);
                    continue;
                }
                return Err(self.failure(Failure::Conflict {
                    a: candidate.name.clone(),
                    av: candidate.version.to_string(),
                    b: other.name.clone(),
                    bv: other.version.to_string(),
                }));
            }
        }
        let _ = removed;
        self.assignment.insert(candidate.name.clone(), candidate);
        Ok(())
    }

    fn search(&mut self) -> Result<()> {
        self.steps += 1;
        if self.steps > 200_000 {
            return Err(self.failure(Failure::Budget));
        }
        let key = self.state_key();
        if !self.path.insert(key.clone()) {
            return Err(self.failure(Failure::Budget));
        }
        let result = self.search_inner();
        self.path.remove(&key);
        result
    }

    fn search_inner(&mut self) -> Result<()> {
        if let Some((dependent, dependency)) = self
            .first_unmet()
            .map(|(candidate, dependency)| (candidate.name.clone(), dependency))
        {
            let options = self.options_for(&dependency);
            if options.is_empty() {
                let failure = if let Some(candidate) = self.newest_any(&dependency.name) {
                    Failure::VersionConflict {
                        candidate: candidate.name,
                        version: candidate.version.to_string(),
                        dependent,
                        dependency: display_dependency(&dependency),
                    }
                } else {
                    Failure::Missing {
                        dependent,
                        dependency: display_dependency(&dependency),
                    }
                };
                return Err(self.failure(failure));
            }
            let mut last_error: Option<SpkError> = None;
            for option in options {
                let saved = self.assignment.clone();
                let saved_steps = self.steps;
                let result = match self.try_assign(option) {
                    Ok(()) => self.search(),
                    Err(error) => Err(error),
                };
                match result {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        last_error = Some(error);
                        self.assignment = saved;
                        self.steps = saved_steps;
                    }
                }
            }
            return Err(last_error.unwrap_or_else(|| self.failure(Failure::Budget)));
        }
        if let Some((a, b)) = self.first_conflict() {
            return Err(self.failure(Failure::Conflict {
                a: a.name.clone(),
                av: a.version.to_string(),
                b: b.name.clone(),
                bv: b.version.to_string(),
            }));
        }
        for (name, pinned) in &self.pinned {
            match self.assignment.get(name) {
                Some(candidate) if candidate.version == pinned.version => {}
                _ => {
                    return Err(self.failure(Failure::Pinned(name.clone())));
                }
            }
        }
        Ok(())
    }

    fn first_unmet(&self) -> Option<(&Candidate, Dependency)> {
        for candidate in self.assignment.values() {
            for dependency in &candidate.depends {
                if !self
                    .assignment
                    .values()
                    .any(|other| other.satisfies(dependency))
                {
                    return Some((candidate, dependency.clone()));
                }
            }
        }
        None
    }

    fn first_conflict(&self) -> Option<(&Candidate, &Candidate)> {
        let candidates: Vec<&Candidate> = self.assignment.values().collect();
        for (index, a) in candidates.iter().enumerate() {
            for b in candidates.iter().skip(index + 1) {
                if a.conflicts_with(b) {
                    return Some((a, b));
                }
            }
        }
        None
    }

    fn options_for(&self, dependency: &Dependency) -> Vec<Candidate> {
        let mut options: Vec<Candidate> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut push = |candidate: &Candidate| {
            let key = (candidate.name.clone(), candidate.version.clone());
            if seen.insert(key) {
                options.push(candidate.clone());
            }
        };
        if let Some(candidates) = self.by_name.get(&dependency.name) {
            // Prefer the installed version, then newer repository versions.
            for candidate in candidates.iter().filter(|c| c.satisfies(dependency)) {
                push(candidate);
            }
        }
        let mut provider_names = BTreeSet::new();
        for (name, candidates) in &self.by_name {
            if name == &dependency.name {
                continue;
            }
            if candidates
                .iter()
                .any(|candidate| candidate.satisfies(dependency))
            {
                provider_names.insert(name.clone());
            }
        }
        for name in provider_names {
            if let Some(candidates) = self.by_name.get(&name) {
                for candidate in candidates.iter().filter(|c| c.satisfies(dependency)) {
                    push(candidate);
                }
            }
        }
        options.sort_by(|a, b| {
            preference(self, a)
                .cmp(&preference(self, b))
                .then_with(|| b.version.cmp(&a.version))
                .then_with(|| a.name.cmp(&b.name))
        });
        options
    }

    fn newest_any(&self, name: &str) -> Option<Candidate> {
        let candidates = self.by_name.get(name)?;
        candidates
            .iter()
            .filter(|candidate| matches!(candidate.origin, Origin::Repo(_)))
            .max_by(|a, b| a.version.cmp(&b.version))
            .or_else(|| candidates.first())
            .cloned()
    }

    fn state_key(&self) -> String {
        let mut parts = Vec::new();
        for (name, candidate) in &self.assignment {
            parts.push(format!("{name}={}", candidate.version));
        }
        parts.join("|")
    }

    fn failure(&self, failure: Failure) -> SpkError {
        match failure {
            Failure::Conflict { a, av, b, bv } => SpkError::Dependency {
                message: i18n::solver_header_conflicts(self.lang).to_string(),
                lines: vec![i18n::solver_conflict(self.lang, &a, &av, &b, &bv)],
            },
            Failure::Missing {
                dependent,
                dependency,
            } => SpkError::Dependency {
                message: i18n::solver_header_dependencies(self.lang).to_string(),
                lines: vec![i18n::solver_missing(self.lang, &dependency, &dependent)],
            },
            Failure::VersionConflict {
                candidate,
                version,
                dependent,
                dependency,
            } => SpkError::Dependency {
                message: i18n::solver_header_dependencies(self.lang).to_string(),
                lines: vec![i18n::solver_version_conflict(
                    self.lang,
                    &candidate,
                    &version,
                    &dependency,
                    &dependent,
                )],
            },
            Failure::Pinned(name) => SpkError::Dependency {
                message: i18n::solver_header_dependencies(self.lang).to_string(),
                lines: vec![i18n::target_requires(self.lang, &name, &name)],
            },
            Failure::Held {
                name,
                held,
                attempted,
            } => SpkError::Dependency {
                message: i18n::solver_header_dependencies(self.lang).to_string(),
                lines: vec![i18n::solver_held(self.lang, &name, &held, &attempted)],
            },
            Failure::Budget => SpkError::Dependency {
                message: i18n::solver_header_dependencies(self.lang).to_string(),
                lines: vec![i18n::solver_budget(self.lang).to_string()],
            },
        }
    }

    fn finish(mut self) -> Result<Solution> {
        // Everything currently assigned must be consistent.
        self.search()?;
        self.validate_final()?;
        let mut actions = Vec::new();
        let mut removals = Vec::new();
        for (name, package) in &self.installed {
            if !self.assignment.contains_key(name) {
                removals.push(package.clone());
            }
        }
        for (name, candidate) in &self.assignment {
            match self.installed.get(name) {
                None => {
                    if matches!(candidate.origin, Origin::Repo(_)) {
                        actions.push(Action {
                            from: None,
                            to: candidate.clone(),
                            kind: TargetKind::Install,
                        });
                    }
                }
                Some(installed) => {
                    let installed_version = Version::new(
                        installed.epoch,
                        installed.version.clone(),
                        installed.release,
                    );
                    let kind = match self.pinned.get(name) {
                        Some(pinned) if pinned.kind == TargetKind::Reinstall => {
                            Some(TargetKind::Reinstall)
                        }
                        Some(pinned) if pinned.version == candidate.version => Some(pinned.kind),
                        _ => {
                            if candidate.version > installed_version {
                                Some(TargetKind::Upgrade)
                            } else if candidate.version < installed_version {
                                Some(TargetKind::Downgrade)
                            } else {
                                None
                            }
                        }
                    };
                    if let Some(kind) = kind {
                        if matches!(candidate.origin, Origin::Repo(_)) {
                            actions.push(Action {
                                from: Some(installed.clone()),
                                to: candidate.clone(),
                                kind,
                            });
                        }
                    }
                }
            }
        }
        order_actions(&mut actions, &self.assignment);
        let explicit = self.pinned.keys().cloned().collect();
        let cleared_holds = self
            .override_names
            .intersection(&self.holds.keys().cloned().collect())
            .cloned()
            .collect();
        Ok(Solution {
            assignment: self.assignment,
            actions,
            removals,
            explicit,
            held_back: self.held_back,
            cleared_holds,
        })
    }

    fn validate_final(&self) -> Result<()> {
        for (name, pinned) in &self.pinned {
            match self.assignment.get(name) {
                Some(candidate) => {
                    if candidate.version != pinned.version {
                        return Err(self.failure(Failure::Pinned(name.clone())));
                    }
                    if pinned.repo && !matches!(candidate.origin, Origin::Repo(_)) {
                        return Err(self.failure(Failure::Pinned(name.clone())));
                    }
                }
                None => return Err(self.failure(Failure::Pinned(name.clone()))),
            }
        }
        if let Some((a, b)) = self.first_conflict() {
            return Err(self.failure(Failure::Conflict {
                a: a.name.clone(),
                av: a.version.to_string(),
                b: b.name.clone(),
                bv: b.version.to_string(),
            }));
        }
        if let Some((candidate, dependency)) = self.first_unmet() {
            return Err(self.failure(Failure::Missing {
                dependent: candidate.name.clone(),
                dependency: display_dependency(&dependency),
            }));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum Failure {
    Missing {
        dependent: String,
        dependency: String,
    },
    VersionConflict {
        candidate: String,
        version: String,
        dependent: String,
        dependency: String,
    },
    Conflict {
        a: String,
        av: String,
        b: String,
        bv: String,
    },
    Pinned(String),
    Held {
        name: String,
        held: String,
        attempted: String,
    },
    Budget,
}

fn preference(solver: &Solver<'_>, candidate: &Candidate) -> (u8, u8) {
    let installed = matches!(candidate.origin, Origin::Installed);
    let pinned = solver.pinned.contains_key(&candidate.name);
    (
        if pinned {
            0
        } else if installed {
            1
        } else {
            2
        },
        if installed { 0 } else { 1 },
    )
}

fn origin_rank(origin: &Origin) -> u8 {
    match origin {
        Origin::Installed => 0,
        Origin::Repo(index) => *index as u8 + 1,
    }
}

fn display_dependency(dependency: &Dependency) -> String {
    let name = &dependency.name;
    match (&dependency.relation, &dependency.version) {
        (Some(relation), Some(version)) => {
            let operator = match relation {
                spk_core::manifest::Relation::Eq => "=",
                spk_core::manifest::Relation::Greater => ">",
                spk_core::manifest::Relation::GreaterEq => ">=",
                spk_core::manifest::Relation::Less => "<",
                spk_core::manifest::Relation::LessEq => "<=",
            };
            format!("{name}{operator}{version}")
        }
        _ => name.clone(),
    }
}

fn order_actions(actions: &mut Vec<Action>, assignment: &BTreeMap<String, Candidate>) {
    let mut remaining: BTreeSet<String> = actions.iter().map(|a| a.to.name.clone()).collect();
    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|name| {
                let candidate = assignment.get(*name).expect("action assigned");
                candidate.depends.iter().all(|dependency| {
                    if dependency.name == **name {
                        return true;
                    }
                    assignment.values().any(|other| {
                        other.satisfies(dependency) && !remaining.contains(&other.name)
                    })
                })
            })
            .cloned()
            .collect();
        let batch = if ready.is_empty() {
            // Dependency cycle: install the remaining set together.
            remaining.iter().cloned().collect::<Vec<_>>()
        } else {
            ready
        };
        for name in batch {
            if remaining.remove(&name) {
                if let Some(action) = actions.iter().find(|a| a.to.name == name) {
                    ordered.push(action.clone());
                }
            }
        }
    }
    *actions = ordered;
}

#[cfg(test)]
mod tests {
    use super::*;
    use spk_core::db::{FileDb, InstalledDb, ReposDb, WorldDb};
    use spk_core::index::{Index, IndexPackage};
    use spk_core::manifest::Manifest;

    fn package(name: &str, version: &str, depends: &[&str], provides: &[&str]) -> IndexPackage {
        IndexPackage {
            name: name.into(),
            epoch: 0,
            version: version.into(),
            release: 1,
            arch: "x86_64".into(),
            abi: "spaceos-1".into(),
            filename: format!("{name}-{version}-1-x86_64.spk"),
            size: 1,
            sha256: "ab".repeat(32),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            provides: provides.iter().map(|s| s.to_string()).collect(),
            conflicts: vec![],
            replaces: vec![],
            summary: name.into(),
        }
    }

    fn repo(packages: Vec<IndexPackage>) -> LoadedRepo {
        let mut index = Index::new("test", "x86_64", 0, "ed25519:0123456789abcdef");
        index.packages = packages;
        LoadedRepo {
            name: "test".into(),
            key_id: index.key_id.clone(),
            dir: std::path::PathBuf::from("/repo"),
            index,
            priority: 0,
            order: 0,
        }
    }

    fn state() -> State {
        State {
            installed: InstalledDb::default(),
            files: FileDb::default(),
            world: WorldDb::default(),
            repos: ReposDb::default(),
            holds: Default::default(),
        }
    }

    fn manifest(name: &str, version: &str, depends: &[&str]) -> Manifest {
        Manifest::from_toml(&format!(
            r#"name = "{name}"
version = "{version}"
release = 1
arch = "x86_64"
abi = "spaceos-1"
summary = "{name}"
license = "MIT"
depends = {depends:?}
"#
        ))
        .unwrap()
    }

    #[test]
    fn install_pulls_dependencies() {
        let mut state = state();
        state.installed = InstalledDb::default();
        let repos = vec![repo(vec![
            package("app", "1.0", &["lib>=1"], &[]),
            package("lib", "1.0", &[], &[]),
        ])];
        let solution = solve(
            &state,
            &repos,
            Operation::Install {
                specs: vec![parse_spec("app").unwrap()],
                needed: false,
            },
            Lang::En,
            false,
        )
        .unwrap();
        assert_eq!(solution.actions.len(), 2);
        assert!(solution.assignment.contains_key("app"));
        assert!(solution.assignment.contains_key("lib"));
    }

    #[test]
    fn missing_dependency_reports_chain() {
        let state = state();
        let repos = vec![repo(vec![package("app", "1.0", &["lib<2"], &[])])];
        let error = solve(
            &state,
            &repos,
            Operation::Install {
                specs: vec![parse_spec("app").unwrap()],
                needed: false,
            },
            Lang::En,
            false,
        )
        .unwrap_err();
        assert_eq!(error.exit_code(), 4);
        assert!(format!("{:?}", error).contains("lib<2"));
    }

    #[test]
    fn version_conflict_reports_chain() {
        let state = state();
        let repos = vec![repo(vec![
            package("app", "1.0", &["lib<2"], &[]),
            package("lib", "2.0", &[], &[]),
        ])];
        let error = solve(
            &state,
            &repos,
            Operation::Install {
                specs: vec![parse_spec("app").unwrap()],
                needed: false,
            },
            Lang::En,
            false,
        )
        .unwrap_err();
        let text = format!("{:?}", error);
        assert!(text.contains("lib") && text.contains("2.0"));
    }

    #[test]
    fn versioned_provides_satisfy_dependency() {
        let state = state();
        let repos = vec![repo(vec![
            package("app", "1.0", &["virt>=2"], &[]),
            package("provider", "1.0", &[], &["virt=2"]),
        ])];
        let solution = solve(
            &state,
            &repos,
            Operation::Install {
                specs: vec![parse_spec("app").unwrap()],
                needed: false,
            },
            Lang::En,
            false,
        )
        .unwrap();
        assert!(solution.assignment.contains_key("provider"));
    }

    #[test]
    fn replaces_removes_old_package() {
        let mut state = state();
        let installed = package("old", "1.0", &[], &[]);
        let manifest = manifest("old", "1.0", &[]);
        state.installed.packages.insert(
            "old".into(),
            spk_core::db::InstalledPackage {
                name: "old".into(),
                epoch: 0,
                version: "1.0".into(),
                release: 1,
                arch: "x86_64".into(),
                abi: "spaceos-1".into(),
                files: manifest.files.clone(),
                depends: vec![],
                provides: vec![],
                conflicts: vec![],
                replaces: vec![],
                conffiles: vec![],
                scripts: false,
                triggers: vec![],
                install_script: None,
                allow_setuid: false,
                install_date: 0,
                install_reason: spk_core::db::InstallReason::Explicit,
                txid: "x".into(),
            },
        );
        let _ = installed;
        let mut new = package("new", "2.0", &[], &[]);
        new.replaces = vec!["old".into()];
        let repos = vec![repo(vec![new])];
        let solution = solve(
            &state,
            &repos,
            Operation::Install {
                specs: vec![parse_spec("new").unwrap()],
                needed: false,
            },
            Lang::En,
            false,
        )
        .unwrap();
        assert_eq!(solution.removals.len(), 1);
        assert_eq!(solution.removals[0].name, "old");
        assert!(solution.assignment.contains_key("new"));
    }

    #[test]
    fn dependency_cycle_is_accepted() {
        let state = state();
        let repos = vec![repo(vec![
            package("a", "1.0", &["b"], &[]),
            package("b", "1.0", &["a"], &[]),
        ])];
        let solution = solve(
            &state,
            &repos,
            Operation::Install {
                specs: vec![parse_spec("a").unwrap()],
                needed: false,
            },
            Lang::En,
            false,
        )
        .unwrap();
        assert_eq!(solution.actions.len(), 2);
    }

    #[test]
    fn upgrade_chooses_newest_and_downgrade_is_exact() {
        let mut state = state();
        let installed = spk_core::db::InstalledPackage {
            name: "hello".into(),
            epoch: 0,
            version: "0.1.0".into(),
            release: 1,
            arch: "x86_64".into(),
            abi: "spaceos-1".into(),
            files: vec![],
            depends: vec![],
            provides: vec![],
            conflicts: vec![],
            replaces: vec![],
            conffiles: vec![],
            scripts: false,
            triggers: vec![],
            install_script: None,
            allow_setuid: false,
            install_date: 0,
            install_reason: spk_core::db::InstallReason::Explicit,
            txid: "x".into(),
        };
        state.installed.packages.insert("hello".into(), installed);
        let repos = vec![repo(vec![
            package("hello", "0.1.0", &[], &[]),
            package("hello", "0.2.0", &[], &[]),
        ])];
        let solution = solve(&state, &repos, Operation::UpgradeAll, Lang::En, false).unwrap();
        assert_eq!(solution.actions.len(), 1);
        assert_eq!(solution.actions[0].kind, TargetKind::Upgrade);
        assert_eq!(solution.actions[0].to.version.to_string(), "0.2.0-1");

        state.installed.packages.get_mut("hello").unwrap().version = "0.2.0".into();
        let solution = solve(
            &state,
            &repos,
            Operation::Downgrade(vec![parse_spec("hello=0.1.0-1").unwrap()]),
            Lang::En,
            false,
        )
        .unwrap();
        assert_eq!(solution.actions.len(), 1);
        assert_eq!(solution.actions[0].kind, TargetKind::Downgrade);
        assert_eq!(solution.actions[0].to.version.to_string(), "0.1.0-1");
    }
}
