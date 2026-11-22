//
//       user/spk/spk/tests/solver_fuzz.rs
//       Random dependency-graph invariants and solver scaling
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::time::Instant;

use proptest::prelude::*;
use spk_core::db::{FileDb, HoldsDb, InstalledDb, ReposDb, WorldDb};
use spk_core::index::{Index, IndexPackage};
use spk_core::manifest::Dependency;
use spk_core::version::Version;
use spk_ops::i18n::Lang;
use spk_ops::repo::LoadedRepo;
use spk_ops::security::SecurityState;
use spk_ops::solver::{parse_spec, solve, Operation, Solution, TargetKind};
use spk_ops::state::State;

fn package(
    name: &str,
    version: &str,
    depends: Vec<String>,
    provides: Vec<String>,
    conflicts: Vec<String>,
) -> IndexPackage {
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
        depends,
        provides,
        conflicts,
        replaces: vec![],
        summary: name.into(),
    }
}

fn repo(packages: Vec<IndexPackage>) -> LoadedRepo {
    let mut index = Index::new("fuzz", "x86_64", 0, "ed25519:0123456789abcdef");
    index.packages = packages;
    let security = SecurityState::from_index(&index);
    LoadedRepo {
        name: "fuzz".into(),
        key_id: index.key_id.clone(),
        dir: "/repo".into(),
        index,
        priority: 0,
        order: 0,
        security,
        mirrors: Vec::new(),
        package_cache: None,
        offline: false,
        allow_unsigned: true,
        manifest: None,
        history: std::collections::BTreeMap::new(),
    }
}

fn empty_state() -> State {
    State {
        installed: InstalledDb::default(),
        files: FileDb::default(),
        world: WorldDb::default(),
        repos: ReposDb::default(),
        holds: HoldsDb::default(),
    }
}

fn matches(package: &IndexPackage, spec: &Dependency) -> bool {
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
        match (&spec.version, provision.version) {
            (None, _) => true,
            (Some(_), Some(version)) => spec.matches(&version),
            (Some(_), None) => false,
        }
    })
}

fn signature(solution: &Solution) -> Vec<String> {
    let mut actions: Vec<String> = solution
        .actions
        .iter()
        .map(|action| format!("{}:{}:{:?}", action.to.name, action.to.version, action.kind))
        .collect();
    actions.sort();
    let mut removals: Vec<String> = solution
        .removals
        .iter()
        .map(|package| format!("-{}:{}-{}", package.name, package.version, package.release))
        .collect();
    removals.sort();
    actions.extend(removals);
    actions
}

fn installed_action_names(solution: &Solution) -> Vec<String> {
    let mut names: Vec<String> = solution
        .actions
        .iter()
        .map(|action| action.to.name.clone())
        .collect();
    names.sort();
    names
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn random_graph_solver_is_consistent_and_deterministic(
        versions in prop::collection::vec(1u64..=2, 6),
        deps in prop::collection::vec(
            prop::collection::vec((0usize..6, prop::option::of("[<>=]{1,2}(1|2)")), 0..3),
            6,
        ),
        conflict_seeds in prop::collection::vec(prop::collection::vec(0usize..6, 0..2), 6),
    ) {
        let mut packages = Vec::new();
        for index in 0..6 {
            let version = if versions[index] == 1 { "1.0" } else { "2.0" };
            let name = format!("p{index}");
            let depends = deps[index]
                .iter()
                .filter(|(target, _)| *target != index)
                .map(|(target, op)| format!("p{target}{}", op.clone().unwrap_or_default()))
                .collect();
            let conflicts = conflict_seeds[index]
                .iter()
                .filter(|target| **target != index)
                .map(|target| format!("p{target}"))
                .collect();
            packages.push(package(&name, version, depends, vec![], conflicts));
        }
        let repos = vec![repo(packages)];
        let state = empty_state();
        let operation = || Operation::Install {
            specs: vec![parse_spec("p0").unwrap()],
            needed: false,
        };
        let first = solve(&state, &repos, operation(), Lang::En, false);
        if let Ok(solution) = &first {
            let second = solve(&state, &repos, operation(), Lang::En, false).unwrap();
            prop_assert_eq!(signature(solution), signature(&second));
            prop_assert!(!solution.actions.is_empty());
            prop_assert!(installed_action_names(solution).iter().any(|name| name == "p0"));
            for candidate in solution.assignment.values() {
                let package = candidate.repo_pkg.as_ref().expect("repo candidate");
                for dependency in &package.depends {
                    let dependency = Dependency::parse(dependency).unwrap();
                    prop_assert!(
                        solution.assignment.values().any(|other| {
                            let other = other.repo_pkg.as_ref().expect("repo candidate");
                            matches(other, &dependency)
                        }),
                        "unmet dependency {:?} for {}", dependency, package.name
                    );
                }
                for conflict in &package.conflicts {
                    let conflict = Dependency::parse(conflict).unwrap();
                    prop_assert!(
                        !solution.assignment.values().any(|other| {
                            let other = other.repo_pkg.as_ref().expect("repo candidate");
                            matches(other, &conflict) && other.name != package.name
                        }),
                        "conflict {:?} in {}", conflict, package.name
                    );
                }
            }
        }
    }
}

#[test]
fn solves_large_chain_quickly() {
    let count = 120usize;
    let mut packages = Vec::new();
    for index in 0..count {
        let depends = if index == 0 {
            vec![]
        } else {
            vec![format!("p{}>=1", index - 1)]
        };
        packages.push(package(
            &format!("p{index}"),
            "1.0",
            depends,
            vec![],
            vec![],
        ));
    }
    let repos = vec![repo(packages)];
    let state = empty_state();
    let started = Instant::now();
    let solution = solve(
        &state,
        &repos,
        Operation::Install {
            specs: vec![parse_spec(&format!("p{}", count - 1)).unwrap()],
            needed: false,
        },
        Lang::En,
        false,
    )
    .unwrap();
    assert_eq!(solution.actions.len(), count);
    assert_eq!(solution.actions.last().unwrap().kind, TargetKind::Install);
    assert!(
        started.elapsed().as_secs() < 10,
        "solver took {:?}",
        started.elapsed()
    );
}
