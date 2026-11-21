//
//       user/spk/spk/tests/p2.rs
//       P2: holds, multi-repo, transaction plans, conflict boundaries
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use spk_core::keys::{write_keypair, PublicKey, DEFAULT_COMMENT};
use spk_core::manifest::Manifest;
use spk_host::{generate, pack};

struct PkgFile {
    path: String,
    data: Vec<u8>,
    mode: u32,
}

struct Pkg {
    name: String,
    version: String,
    depends: Vec<String>,
    provides: Vec<String>,
    conflicts: Vec<String>,
    conffiles: Vec<String>,
    files: Vec<PkgFile>,
}

impl Pkg {
    fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            depends: vec![],
            provides: vec![],
            conflicts: vec![],
            conffiles: vec![],
            files: vec![],
        }
    }
    fn depends(mut self, items: &[&str]) -> Self {
        self.depends = items.iter().map(|item| item.to_string()).collect();
        self
    }
    fn file(mut self, path: &str, data: &[u8], mode: u32) -> Self {
        self.files.push(PkgFile {
            path: path.into(),
            data: data.to_vec(),
            mode,
        });
        self
    }
    fn conffile(mut self, path: &str, data: &[u8]) -> Self {
        self.conffiles.push(path.into());
        self.file(path, data, 0o644)
    }
    fn manifest(&self) -> String {
        format!(
            r#"name = "{}"
epoch = 0
version = "{}"
release = 1
arch = "x86_64"
abi = "spaceos-1"
summary = "{} test package"
description = "P2 test package"
license = "MIT"
url = "https://example.invalid"
packager = "SpaceOS Test <test@spaceos>"
build_date = 0
depends = {}
provides = {}
conflicts = {}
replaces = []
conffiles = {}
scripts = false
allow_setuid = false
"#,
            self.name,
            self.version,
            self.name,
            toml_array(&self.depends),
            toml_array(&self.provides),
            toml_array(&self.conflicts),
            toml_array(&self.conffiles),
        )
    }
}

fn toml_array(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|item| format!("\"{item}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

struct TestRepo {
    dir: tempfile::TempDir,
    repo: PathBuf,
    secret: PathBuf,
    public: PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let keys = dir.path().join("keys");
        let (secret, public, _) = write_keypair(&keys, "test", DEFAULT_COMMENT, false).unwrap();
        Self {
            dir,
            repo,
            secret,
            public,
        }
    }
    fn add(&self, package: &Pkg) {
        let build = self
            .dir
            .path()
            .join("build")
            .join(format!("{}-{}", package.name, package.version));
        let staging = build.join("staging");
        for file in &package.files {
            let path = staging.join(file.path.trim_start_matches('/'));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &file.data).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(file.mode)).unwrap();
        }
        let manifest_path = build.join("manifest.toml");
        fs::write(&manifest_path, package.manifest()).unwrap();
        let manifest = Manifest::load(&manifest_path).unwrap();
        pack(
            &manifest_path,
            &staging,
            &self.repo.join(manifest.package_filename()),
            19,
        )
        .unwrap();
    }
    fn publish(&self) {
        generate(
            &self.repo,
            "test",
            "x86_64",
            &self.secret,
            Some(1_760_000_000),
            true,
        )
        .unwrap();
    }
    fn trust(&self, root: &Path) {
        let key = PublicKey::from_file(&self.public).unwrap();
        let dir = root.join("etc/spk/trusted.d");
        fs::create_dir_all(&dir).unwrap();
        fs::copy(&self.public, dir.join(format!("{}.pub", key.key_id))).unwrap();
    }
    fn arg(&self) -> &str {
        self.repo.to_str().unwrap()
    }
}

struct Root {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Root {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        Self { _dir: dir, path }
    }
    fn with_repos(repos: &[&TestRepo]) -> Self {
        let root = Self::new();
        for repo in repos {
            repo.trust(&root.path);
        }
        root
    }
    fn file(&self, logical: &str) -> PathBuf {
        self.path.join(logical.trim_start_matches('/'))
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_spk"))
            .arg("--root")
            .arg(&self.path)
            .arg("--noconfirm")
            .arg("--lang")
            .arg("en")
            .args(args)
            .output()
            .unwrap()
    }
    fn check(&self) -> Output {
        self.run(&["check"])
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_ok(output: &Output) {
    assert!(output.status.success(), "command failed:\n{}", text(output));
}

fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "unexpected exit:\n{}",
        text(output)
    );
}

fn repo_arg(repo: &TestRepo) -> &str {
    repo.arg()
}

#[test]
fn hold_blocks_upgrade_and_override_clears() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("hello", "0.1.0").file("/usr/bin/hello", b"v1\n", 0o755));
    repo.publish();
    let root = Root::with_repos(&[&repo]);
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "hello"]));
    assert_ok(&root.run(&["hold", "hello"]));
    assert!(text(&root.run(&["hold"])).contains("0.1.0-1"));

    repo.add(&Pkg::new("hello", "0.2.0").file("/usr/bin/hello", b"v2\n", 0o755));
    repo.publish();
    let upgrade = root.run(&["upgrade", "--repo", repo_arg(&repo)]);
    assert_ok(&upgrade);
    assert!(text(&upgrade).contains("held back"));
    assert!(text(&root.run(&["list"])).contains("hello 0.1.0-1"));
    assert_ok(&root.check());

    let blocked = root.run(&["install", "--repo", repo_arg(&repo), "hello=0.2.0-1"]);
    assert_code(&blocked, 4);
    assert!(text(&blocked).contains("held"));

    let override_install = root.run(&[
        "install",
        "--override-hold",
        "--repo",
        repo_arg(&repo),
        "hello=0.2.0-1",
    ]);
    assert_ok(&override_install);
    assert!(text(&root.run(&["list"])).contains("hello 0.2.0-1"));
    assert!(text(&root.run(&["hold"])).trim().is_empty());
    assert_ok(&root.check());

    assert_ok(&root.run(&["hold", "hello"]));
    assert_ok(&root.run(&["remove", "hello"]));
    assert!(text(&root.run(&["hold"])).trim().is_empty());
    assert_ok(&root.check());
}

#[test]
fn held_dependency_requires_override() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("lib", "1.0.0").file("/usr/share/lib", b"1\n", 0o644));
    repo.publish();
    let root = Root::with_repos(&[&repo]);
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "lib"]));
    assert_ok(&root.run(&["hold", "lib"]));
    repo.add(&Pkg::new("lib", "2.0.0").file("/usr/share/lib", b"2\n", 0o644));
    repo.add(&Pkg::new("newapp", "1.0.0").depends(&["lib>=2"]).file(
        "/usr/share/newapp",
        b"app\n",
        0o644,
    ));
    repo.publish();

    let blocked = root.run(&["install", "--repo", repo_arg(&repo), "newapp"]);
    assert_code(&blocked, 4);
    assert!(text(&blocked).contains("held"));
    assert!(!root.file("/usr/share/newapp").exists());

    let upgrade = root.run(&["upgrade", "--override-hold", "--repo", repo_arg(&repo)]);
    assert_ok(&upgrade);
    assert!(text(&root.run(&["hold"])).trim().is_empty());
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "newapp"]));
    assert!(root.file("/usr/share/newapp").exists());
    assert_ok(&root.check());
}

#[test]
fn multi_repo_newest_priority_failover_and_conflicts() {
    let first = TestRepo::new();
    first.add(&Pkg::new("hello", "0.1.0").file("/usr/bin/hello", b"v1\n", 0o755));
    first.publish();
    let second = TestRepo::new();
    second.add(&Pkg::new("hello", "0.2.0").file("/usr/bin/hello", b"v2\n", 0o755));
    second.publish();
    let root = Root::with_repos(&[&first, &second]);

    // Newest across repositories wins.
    let installed = root.run(&[
        "install",
        "--repo",
        repo_arg(&first),
        "--repo",
        repo_arg(&second),
        "hello",
    ]);
    assert_ok(&installed);
    assert!(text(&root.run(&["list"])).contains("hello 0.2.0-1"));

    // Repository configuration and priority.
    assert_ok(&root.run(&["repo", "add", "first", repo_arg(&first), "--priority", "10"]));
    assert_ok(&root.run(&[
        "repo",
        "add",
        "second",
        repo_arg(&second),
        "--priority",
        "5",
    ]));
    assert!(text(&root.run(&["repo", "list"])).contains("first priority=10"));
    assert_ok(&root.run(&["repo", "priority", "second", "20"]));
    assert!(text(&root.run(&["repo", "list"])).contains("second priority=20"));
    assert_ok(&root.run(&["repo", "remove", "second"]));
    assert!(!text(&root.run(&["repo", "list"])).contains("second priority"));

    // Failover: the first mirror is missing, the second is valid.
    let missing = first.dir.path().join("missing-mirror");
    assert_ok(&root.run(&[
        "repo",
        "add",
        "mirror",
        missing.to_str().unwrap(),
        "--priority",
        "30",
    ]));
    assert_ok(&root.run(&[
        "repo",
        "add",
        "mirror",
        repo_arg(&first),
        "--priority",
        "30",
    ]));
    // No --repo: uses repos.json, where mirror has a missing first URL and a
    // valid fallback URL.
    let upgraded = root.run(&["install", "hello=0.1.0-1"]);
    assert_ok(&upgraded);
    assert!(text(&root.run(&["list"])).contains("hello 0.1.0-1"));

    // A repository whose index no longer matches its signature is fatal.
    let bad = TestRepo::new();
    bad.add(&Pkg::new("hello", "0.3.0").file("/usr/bin/hello", b"v3\n", 0o755));
    bad.publish();
    let index_path = bad.repo.join("index.json");
    let mut bytes = fs::read(&index_path).unwrap();
    bytes.push(b' ');
    fs::write(&index_path, bytes).unwrap();
    bad.trust(&root.path);
    let bad_install = root.run(&["install", "--repo", bad.arg(), "hello=0.3.0-1"]);
    assert_code(&bad_install, 3);

    // The same package version with different content is a multi-repo error.
    let other = TestRepo::new();
    other.add(&Pkg::new("hello", "0.1.0").file("/usr/bin/hello", b"different\n", 0o755));
    other.publish();
    other.trust(&root.path);
    let conflict = root.run(&[
        "install",
        "--print",
        "--repo",
        repo_arg(&first),
        "--repo",
        other.arg(),
        "hello",
    ]);
    assert_code(&conflict, 1);
    assert!(text(&conflict).contains("differs between repositories"));
}

#[test]
fn print_and_json_plans_are_deterministic() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("hello", "0.1.0").file("/usr/bin/hello", b"v1\n", 0o755));
    repo.publish();
    let root = Root::with_repos(&[&repo]);
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "hello=0.1.0-1"]));
    repo.add(&Pkg::new("hello", "0.2.0").file("/usr/bin/hello", b"v2\n", 0o755));
    repo.publish();

    let human = root.run(&["upgrade", "--print", "--repo", repo_arg(&repo)]);
    assert_ok(&human);
    assert!(text(&human).contains("upgrade hello"));
    assert!(!text(&human).contains("Proceed"));
    assert!(text(&root.run(&["list"])).contains("hello 0.1.0-1"));

    let first = root.run(&["--json", "upgrade", "--repo", repo_arg(&repo)]);
    let second = root.run(&["--json", "upgrade", "--repo", repo_arg(&repo)]);
    assert_ok(&first);
    assert_eq!(
        first.stdout, second.stdout,
        "JSON plans must be deterministic"
    );
    let plan: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(plan["schema"], 1);
    assert_eq!(plan["nothing_to_do"], false);
    assert_eq!(plan["packages"][0]["name"], "hello");
    assert_eq!(plan["packages"][0]["action"], "upgrade");
    assert_eq!(plan["packages"][0]["to"], "0.2.0-1");
    assert!(text(&root.run(&["list"])).contains("hello 0.1.0-1"));

    let failure = root.run(&["--json", "install", "--repo", repo_arg(&repo), "missing"]);
    assert_code(&failure, 1);
    let error: serde_json::Value = serde_json::from_slice(&failure.stdout).unwrap();
    assert_eq!(error["schema"], 1);
    assert!(error["error"].as_str().unwrap().contains("missing"));

    let remove = root.run(&["remove", "--print", "hello"]);
    assert_ok(&remove);
    assert!(text(&remove).contains("remove hello"));
    assert!(text(&root.run(&["list"])).contains("hello 0.1.0-1"));
}

#[test]
fn file_and_conffile_boundaries() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("file", "1.0.0").file("/usr/share/typecheck", b"file\n", 0o644));
    repo.add(&Pkg::new("dirc", "1.0.0").file("/usr/share/dircheck/inner", b"x\n", 0o644));
    repo.add(&Pkg::new("cfg", "1.0.0").conffile("/etc/cfg.conf", b"old\n"));
    repo.add(&Pkg::new("shared-a", "1.0.0").file("/usr/share/shared/a", b"a\n", 0o644));
    repo.add(&Pkg::new("shared-b", "1.0.0").file("/usr/share/shared/b", b"b\n", 0o644));
    repo.publish();
    let root = Root::with_repos(&[&repo]);

    fs::create_dir_all(root.file("/usr/share/typecheck")).unwrap();
    let conflict = root.run(&["install", "--repo", repo_arg(&repo), "file"]);
    assert_code(&conflict, 5);
    fs::remove_dir_all(root.file("/usr/share/typecheck")).unwrap();

    fs::create_dir_all(root.file("/usr/share")).unwrap();
    fs::write(root.file("/usr/share/dircheck"), b"not a dir\n").unwrap();
    let conflict = root.run(&["install", "--repo", repo_arg(&repo), "dirc"]);
    assert_code(&conflict, 5);
    fs::remove_file(root.file("/usr/share/dircheck")).unwrap();

    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "shared-a"]));
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "shared-b"]));
    assert_ok(&root.run(&["remove", "shared-a"]));
    assert!(root.file("/usr/share/shared/b").exists());
    assert_ok(&root.check());

    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "cfg"]));
    fs::write(root.file("/etc/cfg.conf"), b"user\n").unwrap();
    let reinstall = root.run(&["reinstall", "--repo", repo_arg(&repo), "cfg"]);
    assert_ok(&reinstall);
    assert_eq!(fs::read(root.file("/etc/cfg.conf")).unwrap(), b"user\n");
    assert_ok(&root.check());
}
