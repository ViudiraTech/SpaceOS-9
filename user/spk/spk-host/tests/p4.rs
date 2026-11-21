//
//       user/spk/spk-host/tests/p4.rs
//       Dual-signature index format (rotation support, not enabled in production)
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use spk_core::keys::{read_signature, verify_any, write_keypair, PublicKey, DEFAULT_COMMENT};
use spk_host::{generate_with_keys, generate_with_keys_and_security, pack, SecurityConfig};

#[test]
fn dual_signature_index_verifies_with_either_key() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let staging = tmp.path().join("stage");
    fs::create_dir_all(repo.join("x86_64")).unwrap();
    fs::create_dir_all(staging.join("usr/bin")).unwrap();
    fs::write(staging.join("usr/bin/hello"), b"hello\n").unwrap();
    let manifest = tmp.path().join("manifest.toml");
    fs::write(
        &manifest,
        "name = \"hello\"\nepoch = 0\nversion = \"1.0.0\"\nrelease = 1\narch = \"x86_64\"\nabi = \"spaceos-1\"\nsummary = \"hello\"\nlicense = \"MIT\"\nscripts = false\nallow_setuid = false\n",
    )
    .unwrap();
    pack(
        &manifest,
        &staging,
        &repo.join("x86_64/hello-1.0.0-1-x86_64.spk"),
        19,
    )
    .unwrap();
    let keys = tmp.path().join("keys");
    let (secret_a, public_a, key_a) =
        write_keypair(&keys, "rotation-a", DEFAULT_COMMENT, false).unwrap();
    let (secret_b, public_b, key_b) =
        write_keypair(&keys, "rotation-b", DEFAULT_COMMENT, false).unwrap();
    let result = generate_with_keys(
        &repo.join("x86_64"),
        "rotation",
        "x86_64",
        &[secret_a, secret_b],
        Some(100),
        true,
    )
    .unwrap();
    assert_eq!(result.index.key_id, key_a.key_id);
    let index = fs::read(repo.join("x86_64/index.json")).unwrap();
    let signature = read_signature(&repo.join("x86_64/index.json.sig")).unwrap();
    assert_eq!(signature.signatures.len(), 2);
    let trusted = vec![
        PublicKey::from_file(&public_a).unwrap(),
        PublicKey::from_file(&public_b).unwrap(),
    ];
    let revoked = BTreeSet::new();
    assert_eq!(
        verify_any(&index, &signature, &trusted, &revoked).unwrap(),
        key_a.key_id
    );
    // Remove the primary key from trust: the secondary signature still works.
    let only_b = vec![PublicKey::from_file(&public_b).unwrap()];
    assert_eq!(
        verify_any(&index, &signature, &only_b, &revoked).unwrap(),
        key_b.key_id
    );
    let mut revoked = BTreeSet::new();
    revoked.insert(key_a.key_id.clone());
    assert_eq!(
        verify_any(&index, &signature, &trusted, &revoked).unwrap(),
        key_b.key_id
    );
}

#[test]
fn security_metadata_is_signed_only_when_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let staging = tmp.path().join("stage");
    fs::create_dir_all(repo.join("x86_64")).unwrap();
    fs::create_dir_all(staging.join("usr/bin")).unwrap();
    fs::write(staging.join("usr/bin/hello"), b"hello\n").unwrap();
    let manifest = tmp.path().join("manifest.toml");
    fs::write(
        &manifest,
        "name = \"hello\"\nepoch = 0\nversion = \"1.0.0\"\nrelease = 1\narch = \"x86_64\"\nabi = \"spaceos-1\"\nsummary = \"hello\"\nlicense = \"MIT\"\nscripts = false\nallow_setuid = false\n",
    )
    .unwrap();
    pack(
        &manifest,
        &staging,
        &repo.join("x86_64/hello-1.0.0-1-x86_64.spk"),
        19,
    )
    .unwrap();
    let keys = tmp.path().join("keys");
    let (secret, _public, _key) = write_keypair(&keys, "security", DEFAULT_COMMENT, false).unwrap();
    let security = SecurityConfig {
        min_version: BTreeMap::from([("hello".to_string(), "1.0.0-1".to_string())]),
        revoked_versions: vec![spk_core::index::RevokedVersion {
            name: "hello".to_string(),
            version: "0.9.0-1".to_string(),
            reason: "test".to_string(),
        }],
        revoked_keys: vec!["ed25519:0000000000000000".to_string()],
    };
    let result = generate_with_keys_and_security(
        &repo.join("x86_64"),
        "security",
        "x86_64",
        &[secret],
        Some(1),
        true,
        Some(&security),
    )
    .unwrap();
    assert_eq!(result.index.min_version["hello"], "1.0.0-1");
    let raw = fs::read_to_string(repo.join("x86_64/index.json")).unwrap();
    assert!(raw.contains("min_version"));
    assert!(raw.contains("revoked_versions"));
    assert!(raw.contains("revoked_keys"));
}
