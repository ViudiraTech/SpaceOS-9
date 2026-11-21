//
//       user/spk/spk/tests/p5.rs
//       P5 cache, audit and status API tests
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;

use spk_core::index::{Index, IndexPackage};
use spk_core::keys::KeyPair;
use spk_ops::state::Paths;
use spk_ops::transaction::OperationKind;

fn package(name: &str, sha: &str, size: u64) -> IndexPackage {
    IndexPackage {
        name: name.into(),
        epoch: 0,
        version: "1.0.0".into(),
        release: 1,
        arch: "x86_64".into(),
        abi: "spaceos-1".into(),
        filename: format!("{name}-1.0.0-1-x86_64.spk"),
        size,
        sha256: sha.into(),
        depends: vec![],
        provides: vec![],
        conflicts: vec![],
        replaces: vec![],
        summary: "P5 test".into(),
    }
}

#[test]
fn cache_clean_keeps_current_repository_packages() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path()).unwrap();
    paths.ensure_state().unwrap();
    let current = "11".repeat(32);
    let old = "22".repeat(32);
    let snapshot = paths.repo_cache.join("local/snapshots").join(&current);
    fs::create_dir_all(&snapshot).unwrap();
    fs::write(
        paths.repo_cache.join("local/current"),
        format!("{current}\n"),
    )
    .unwrap();
    let key = KeyPair::generate("P5").unwrap();
    let mut index = Index::new("local", "x86_64", 1, &key.key_id);
    index.packages.push(package("keep", &current, 4));
    fs::write(snapshot.join("index.json"), index.to_bytes().unwrap()).unwrap();
    fs::write(paths.package_cache.join(format!("{current}.spk")), b"keep").unwrap();
    fs::write(paths.package_cache.join(format!("{old}.spk")), b"old").unwrap();

    let keep = spk_ops::remote::cached_package_shas(&paths).unwrap();
    assert!(keep.contains(&current));
    assert!(!keep.contains(&old));
    spk_ops::remote::prune_package_cache_with(&paths, &keep, 0, 1).unwrap();
    assert!(paths.package_cache.join(format!("{current}.spk")).is_file());
    assert!(!paths.package_cache.join(format!("{old}.spk")).is_file());
    let info = spk_ops::remote::cache_info(&paths).unwrap();
    assert_eq!(info.package_count, 1);
    assert_eq!(info.snapshot_count, 1);
}

#[test]
fn audit_log_is_json_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path()).unwrap();
    paths.ensure_state().unwrap();
    spk_ops::audit::record(
        &paths,
        OperationKind::Install,
        &["hello 1.0.0-1".to_string()],
        "ok",
    )
    .unwrap();
    let text = fs::read_to_string(tmp.path().join("var/log/spk/audit.log")).unwrap();
    let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(line["schema"], 1);
    assert_eq!(line["operation"], "install");
    assert_eq!(line["result"], "ok");
    assert_eq!(line["packages"][0], "hello 1.0.0-1");
}

#[test]
fn large_repository_index_roundtrip() {
    let key = KeyPair::generate("P5 scale").unwrap();
    let mut index = Index::new("scale", "x86_64", 1, &key.key_id);
    for number in 0..2000 {
        index.packages.push(package(
            &format!("pkg-{number:04}"),
            &format!("{:064x}", number + 1),
            1,
        ));
    }
    let bytes = index.to_bytes().unwrap();
    let decoded = Index::from_slice(&bytes).unwrap();
    assert_eq!(decoded.packages.len(), 2000);
}

#[test]
fn audit_log_survives_concurrent_writers() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path()).unwrap();
    paths.ensure_state().unwrap();
    let mut threads = Vec::new();
    for number in 0..8 {
        let paths = paths.clone();
        threads.push(std::thread::spawn(move || {
            for index in 0..16 {
                spk_ops::audit::record(
                    &paths,
                    OperationKind::Upgrade,
                    &[format!("pkg {number}-{index}")],
                    "ok",
                )
                .unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let text = fs::read_to_string(tmp.path().join("var/log/spk/audit.log")).unwrap();
    assert_eq!(text.lines().count(), 128);
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}
