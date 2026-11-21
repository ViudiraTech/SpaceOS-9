//
//       user/spk/spk-host/tests/p0.rs
//       P0 integration tests: pack, index, sign, verify, tamper and safety
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use spk_core::keys::{write_keypair, DEFAULT_COMMENT};
use spk_core::Error;
use spk_host::{generate, pack, verify_index, verify_repo};

fn manifest(
    name: &str,
    version: &str,
    release: u64,
    arch: &str,
    scripts: bool,
    files: &str,
) -> String {
    format!(
        r#"name = "{name}"
epoch = 0
version = "{version}"
release = {release}
arch = "{arch}"
abi = "spaceos-1"
summary = "{name} package"
description = "P0 integration test package"
license = "LicenseRef-Proprietary"
url = "https://github.com/ViudiraTech/SpaceKernel"
packager = "SpaceOS Build <build@spaceos>"
build_date = 0
depends = []
provides = []
conflicts = []
replaces = []
conffiles = []
scripts = {scripts}
allow_setuid = false
{files}"#
    )
}

fn write_file(path: &Path, data: &[u8], mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn stage_hello(staging: &Path) {
    write_file(&staging.join("usr/bin/hello"), b"hello from spk\n", 0o755);
}

fn repo(tmp: &Path) -> std::path::PathBuf {
    let path = tmp.join("repo/x86_64");
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn pack_index_sign_verify_and_tamper() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    stage_hello(&staging);
    let manifest_path = tmp.path().join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest("hello", "0.1.0", 1, "x86_64", false, ""),
    )
    .unwrap();

    let repo_dir = repo(tmp.path());
    let output = repo_dir.join("hello-0.1.0-1-x86_64.spk");
    let packed = pack(&manifest_path, &staging, &output, 19).unwrap();
    assert_eq!(packed.filename, "hello-0.1.0-1-x86_64.spk");
    assert_eq!(packed.sha256.len(), 64);
    assert!(packed.size > 0);
    let hello = packed
        .manifest
        .files
        .iter()
        .find(|file| file.path == "/usr/bin/hello")
        .unwrap();
    assert_eq!(hello.mode, "0755");
    assert_eq!(hello.size, 15);

    let key_dir = tmp.path().join("keys");
    let (secret, public, key) =
        write_keypair(&key_dir, "spaceos-main", DEFAULT_COMMENT, false).unwrap();

    let generated = generate(
        &repo_dir,
        "spaceos-main",
        "x86_64",
        &secret,
        Some(1_760_000_000),
        true,
    )
    .unwrap();
    assert!(generated.signed);
    assert_eq!(generated.index.packages.len(), 1);
    assert_eq!(generated.index.key_id, key.key_id);
    assert!(repo_dir.join("index.json").is_file());
    assert!(repo_dir.join("index.json.sig").is_file());

    let report = verify_repo(&repo_dir, &public).unwrap();
    assert_eq!(report.packages.len(), 1);
    assert_eq!(report.packages[0], "hello 0.1.0-1");

    // Any byte change to index.json must fail the detached signature.
    let index_path = repo_dir.join("index.json");
    let original = fs::read(&index_path).unwrap();
    let mut tampered = original.clone();
    tampered.push(b' ');
    fs::write(&index_path, &tampered).unwrap();
    assert!(matches!(
        verify_index(&index_path, None, &public),
        Err(Error::Signature(_))
    ));
    fs::write(&index_path, &original).unwrap();
    verify_index(&index_path, None, &public).unwrap();

    // A modified .spk must fail the index's per-file sha256 check.
    let package_path = repo_dir.join("hello-0.1.0-1-x86_64.spk");
    let mut bytes = fs::read(&package_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&package_path, &bytes).unwrap();
    assert!(verify_repo(&repo_dir, &public).is_err());
}

#[test]
fn packing_is_reproducible() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    stage_hello(&staging);
    let manifest_path = tmp.path().join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest("hello", "0.1.0", 1, "x86_64", false, ""),
    )
    .unwrap();

    let first_dir = tmp.path().join("first");
    let second_dir = tmp.path().join("second");
    fs::create_dir_all(&first_dir).unwrap();
    fs::create_dir_all(&second_dir).unwrap();
    let first = pack(
        &manifest_path,
        &staging,
        &first_dir.join("hello-0.1.0-1-x86_64.spk"),
        19,
    )
    .unwrap();
    let second = pack(
        &manifest_path,
        &staging,
        &second_dir.join("hello-0.1.0-1-x86_64.spk"),
        19,
    )
    .unwrap();
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(first.size, second.size);
}

#[test]
fn pack_rejects_escaping_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    fs::create_dir_all(staging.join("usr/bin")).unwrap();
    symlink("../../../../etc/passwd", staging.join("usr/bin/evil")).unwrap();
    let manifest_path = tmp.path().join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest("evil", "1.0", 1, "x86_64", false, ""),
    )
    .unwrap();
    let output = tmp.path().join("evil-1.0-1-x86_64.spk");
    assert!(pack(&manifest_path, &staging, &output, 19).is_err());
}

#[test]
fn pack_accepts_safe_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    stage_hello(&staging);
    symlink("hello", staging.join("usr/bin/hi")).unwrap();
    symlink("/usr/bin/hello", staging.join("usr/bin/absolute")).unwrap();
    let manifest_path = tmp.path().join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest("hello", "0.1.0", 1, "x86_64", false, ""),
    )
    .unwrap();
    let output = tmp.path().join("hello-0.1.0-1-x86_64.spk");
    let packed = pack(&manifest_path, &staging, &output, 19).unwrap();
    let hi = packed
        .manifest
        .files
        .iter()
        .find(|file| file.path == "/usr/bin/hi")
        .unwrap();
    assert_eq!(hi.file_type, spk_core::manifest::FileType::Symlink);
    assert_eq!(hi.target.as_deref(), Some("hello"));
}

#[test]
fn declared_files_are_verified() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    stage_hello(&staging);
    let files = r#"
[[files]]
path = "/usr/bin/hello"
mode = "0755"
uid = 0
gid = 0
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
size = 16
"#;
    let manifest_path = tmp.path().join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest("hello", "0.1.0", 1, "x86_64", false, files),
    )
    .unwrap();
    let output = tmp.path().join("hello-0.1.0-1-x86_64.spk");
    assert!(pack(&manifest_path, &staging, &output, 19).is_err());
}

#[test]
fn scripts_flag_must_match_install_file() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    stage_hello(&staging);
    let manifest_path = tmp.path().join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest("hello", "0.1.0", 1, "x86_64", true, ""),
    )
    .unwrap();
    let output = tmp.path().join("hello-0.1.0-1-x86_64.spk");
    assert!(pack(&manifest_path, &staging, &output, 19).is_err());

    fs::write(tmp.path().join(".INSTALL"), "post_install() { :; }\n").unwrap();
    fs::write(
        &manifest_path,
        manifest("hello", "0.1.0", 1, "x86_64", true, ""),
    )
    .unwrap();
    let packed = pack(&manifest_path, &staging, &output, 19).unwrap();
    assert!(packed.manifest.scripts);
}

#[test]
fn hardlinked_staging_file_warns() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    stage_hello(&staging);
    fs::hard_link(
        staging.join("usr/bin/hello"),
        staging.join("usr/bin/hello2"),
    )
    .unwrap();
    let manifest_path = tmp.path().join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest("hello", "0.1.0", 1, "x86_64", false, ""),
    )
    .unwrap();
    let output = tmp.path().join("hello-0.1.0-1-x86_64.spk");
    let packed = pack(&manifest_path, &staging, &output, 19).unwrap();
    assert!(!packed.warnings.is_empty());
}

#[test]
fn index_rejects_foreign_arch_and_wrong_key() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    stage_hello(&staging);
    let manifest_path = tmp.path().join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest("hello", "0.1.0", 1, "x86_64", false, ""),
    )
    .unwrap();
    let repo_dir = repo(tmp.path());
    pack(
        &manifest_path,
        &staging,
        &repo_dir.join("hello-0.1.0-1-x86_64.spk"),
        19,
    )
    .unwrap();
    let key_dir = tmp.path().join("keys");
    let (secret, public, _) =
        write_keypair(&key_dir, "spaceos-main", DEFAULT_COMMENT, false).unwrap();

    assert!(generate(
        &repo_dir,
        "spaceos-main",
        "aarch64",
        &secret,
        Some(1_760_000_000),
        true,
    )
    .is_err());

    generate(
        &repo_dir,
        "spaceos-main",
        "x86_64",
        &secret,
        Some(1_760_000_000),
        true,
    )
    .unwrap();
    let other_dir = tmp.path().join("other-keys");
    let (_, other_public, _) = write_keypair(&other_dir, "other", DEFAULT_COMMENT, false).unwrap();
    assert!(matches!(
        verify_index(&repo_dir.join("index.json"), None, &other_public),
        Err(Error::Signature(_))
    ));
    verify_index(&repo_dir.join("index.json"), None, &public).unwrap();
}
