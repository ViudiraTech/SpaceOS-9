//
//       user/spk/spk/tests/p1.rs
//       P1 end-to-end tests: install, upgrade, downgrade, solver, recovery
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
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
    release: u64,
    depends: Vec<String>,
    provides: Vec<String>,
    conflicts: Vec<String>,
    replaces: Vec<String>,
    conffiles: Vec<String>,
    files: Vec<PkgFile>,
}

impl Pkg {
    fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            release: 1,
            depends: vec![],
            provides: vec![],
            conflicts: vec![],
            replaces: vec![],
            conffiles: vec![],
            files: vec![],
        }
    }

    fn depends(mut self, items: &[&str]) -> Self {
        self.depends = items.iter().map(|item| item.to_string()).collect();
        self
    }

    fn provides(mut self, items: &[&str]) -> Self {
        self.provides = items.iter().map(|item| item.to_string()).collect();
        self
    }

    fn replaces(mut self, items: &[&str]) -> Self {
        self.replaces = items.iter().map(|item| item.to_string()).collect();
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
release = {}
arch = "x86_64"
abi = "spaceos-1"
summary = "{} test package"
description = "P1 test package"
license = "MIT"
url = "https://example.invalid"
packager = "SpaceOS Test <test@spaceos>"
build_date = 0
depends = {}
provides = {}
conflicts = {}
replaces = {}
conffiles = {}
scripts = false
allow_setuid = false
"#,
            self.name,
            self.version,
            self.release,
            self.name,
            toml_array(&self.depends),
            toml_array(&self.provides),
            toml_array(&self.conflicts),
            toml_array(&self.replaces),
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
}

struct Root {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Root {
    fn new(repo: &TestRepo) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        repo.trust(&path);
        Self { _dir: dir, path }
    }

    fn arg(&self) -> &str {
        self.path.to_str().unwrap()
    }

    fn file(&self, logical: &str) -> PathBuf {
        self.path.join(logical.trim_start_matches('/'))
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_lang("en", args, &[])
    }

    fn run_zh(&self, args: &[&str]) -> Output {
        self.run_lang("zh_CN", args, &[])
    }

    fn run_lang(&self, lang: &str, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_spk"));
        command
            .arg("--root")
            .arg(self.arg())
            .arg("--noconfirm")
            .arg("--lang")
            .arg(lang)
            .args(args);
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn check(&self) -> Output {
        self.run(&["check"])
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {:?}\n{}",
        output.status.code(),
        text(output)
    );
}

fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "unexpected exit code\n{}",
        text(output)
    );
}

fn repo_arg(repo: &TestRepo) -> &str {
    repo.repo.to_str().unwrap()
}

#[test]
fn install_query_and_remove() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("hello", "0.1.0").file("/usr/bin/hello", b"hello-v1\n", 0o755));
    repo.publish();
    let root = Root::new(&repo);

    let out = root.run_zh(&["install", "--repo", repo_arg(&repo), "hello"]);
    assert_ok(&out);
    assert!(text(&out).contains("正在安装 hello"));
    assert_eq!(
        fs::read(root.file("/usr/bin/hello")).unwrap(),
        b"hello-v1\n"
    );

    let list = root.run(&["list"]);
    assert_ok(&list);
    assert!(text(&list).contains("hello 0.1.0-1"));

    let info = root.run_zh(&["info", "hello"]);
    assert_ok(&info);
    assert!(text(&info).contains("0.1.0-1"));
    assert!(text(&info).contains("显式安装"));

    let files = root.run(&["files", "hello"]);
    assert!(text(&files).contains("/usr/bin/hello"));

    let owns = root.run(&["owns", "/usr/bin/hello"]);
    assert!(text(&owns).contains("hello"));

    let check = root.check();
    assert_ok(&check);
    assert!(text(&check).contains("database is consistent"));

    let needed = root.run_zh(&[
        "install",
        "--needed",
        "--repo",
        repo_arg(&repo),
        "hello=0.1.0-1",
    ]);
    assert_ok(&needed);
    assert!(text(&needed).contains("今日无事可做"));

    // Corrupt the installed file; check must notice, reinstall must repair.
    fs::remove_file(root.file("/usr/bin/hello")).unwrap();
    assert_code(&root.check(), 1);
    let reinstall = root.run(&["reinstall", "--repo", repo_arg(&repo), "hello"]);
    assert_ok(&reinstall);
    assert!(root.check().status.success());

    let remove = root.run_zh(&["remove", "hello"]);
    assert_ok(&remove);
    assert!(!root.file("/usr/bin/hello").exists());
    let list = root.run(&["list"]);
    assert!(text(&list).trim().is_empty());
    assert!(root.check().status.success());
}

#[test]
fn upgrade_downgrade_and_partial_policy() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("hello", "0.1.0").file("/usr/bin/hello", b"hello-v1\n", 0o755));
    repo.publish();
    let root = Root::new(&repo);
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "hello"]));

    repo.add(
        &Pkg::new("hello", "0.2.0")
            .file("/usr/bin/hello", b"hello-v2\n", 0o755)
            .file("/usr/share/hello/version", b"0.2.0\n", 0o644),
    );
    repo.publish();

    let blocked = root.run(&["upgrade", "--repo", repo_arg(&repo), "hello"]);
    assert_code(&blocked, 1);
    assert!(text(&blocked).contains("partial upgrades are disabled"));

    let upgrade = root.run_zh(&["upgrade", "--repo", repo_arg(&repo)]);
    assert_ok(&upgrade);
    assert!(text(&upgrade).contains("正在升级 hello"));
    assert_eq!(
        fs::read(root.file("/usr/bin/hello")).unwrap(),
        b"hello-v2\n"
    );
    assert_eq!(
        fs::read(root.file("/usr/share/hello/version")).unwrap(),
        b"0.2.0\n"
    );
    assert!(root.check().status.success());

    let downgrade = root.run_zh(&["downgrade", "--repo", repo_arg(&repo), "hello=0.1.0-1"]);
    assert_ok(&downgrade);
    assert!(text(&downgrade).contains("正在降级 hello"));
    assert_eq!(
        fs::read(root.file("/usr/bin/hello")).unwrap(),
        b"hello-v1\n"
    );
    assert!(!root.file("/usr/share/hello/version").exists());
    assert!(root.check().status.success());

    let partial = root.run(&[
        "upgrade",
        "--allow-partial",
        "--repo",
        repo_arg(&repo),
        "hello",
    ]);
    assert_ok(&partial);
    assert_eq!(
        fs::read(root.file("/usr/bin/hello")).unwrap(),
        b"hello-v2\n"
    );

    let reinstall = root.run_zh(&["reinstall", "--repo", repo_arg(&repo), "hello"]);
    assert_ok(&reinstall);
    assert!(text(&reinstall).contains("正在重新安装 hello"));
    assert!(root.check().status.success());
}

#[test]
fn solver_pulls_dependencies_and_providers() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("app", "1.0.0").depends(&["lib>=1"]).file(
        "/usr/share/app",
        b"app\n",
        0o644,
    ));
    repo.add(&Pkg::new("lib", "1.0.0").file("/usr/share/lib", b"lib\n", 0o644));
    repo.add(
        &Pkg::new("provider-app", "1.0.0")
            .depends(&["virt>=2"])
            .file("/usr/share/provider-app", b"provider-app\n", 0o644),
    );
    repo.add(&Pkg::new("provider", "1.0.0").provides(&["virt=2"]).file(
        "/usr/share/provider",
        b"provider\n",
        0o644,
    ));
    repo.publish();
    let root = Root::new(&repo);

    assert_ok(&root.run_zh(&["install", "--repo", repo_arg(&repo), "app"]));
    assert!(root.file("/usr/share/app").exists());
    assert!(root.file("/usr/share/lib").exists());
    let info = root.run(&["info", "lib"]);
    assert!(text(&info).contains("Installed as a dependency"));

    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "provider-app"]));
    assert!(root.file("/usr/share/provider").exists());
    assert!(root.check().status.success());

    let remove = root.run_zh(&["remove", "-Rs", "app"]);
    assert_ok(&remove);
    assert!(!root.file("/usr/share/app").exists());
    assert!(!root.file("/usr/share/lib").exists());
    assert!(root.file("/usr/share/provider-app").exists());
    assert!(root.file("/usr/share/provider").exists());
    assert!(root.check().status.success());

    let remove = root.run_zh(&["remove", "-Rs", "provider-app"]);
    assert_ok(&remove);
    assert!(root.run(&["list"]).stdout.is_empty());
    assert!(root.check().status.success());
}

#[test]
fn dependency_conflict_reports_chain() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("dep-a", "1.0.0").depends(&["dep-b<2"]).file(
        "/usr/share/dep-a",
        b"a\n",
        0o644,
    ));
    repo.add(&Pkg::new("dep-b", "2.0.0").file("/usr/share/dep-b", b"b\n", 0o644));
    repo.publish();
    let root = Root::new(&repo);

    let out = root.run_zh(&["install", "--repo", repo_arg(&repo), "dep-a"]);
    assert_code(&out, 4);
    let out_text = text(&out);
    assert!(out_text.contains("dep-b<2"));
    assert!(out_text.contains("dep-a"));
    assert!(root.run(&["list"]).stdout.is_empty());
    assert!(!root.file("/usr/share/dep-a").exists());
    assert!(root.check().status.success());
}

#[test]
fn file_conflict_rolls_back() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("file-c", "1.0.0").file("/usr/share/shared", b"from-c\n", 0o644));
    repo.add(&Pkg::new("file-d", "1.0.0").file("/usr/share/shared", b"from-d\n", 0o644));
    repo.publish();
    let root = Root::new(&repo);

    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "file-c"]));
    let out = root.run_zh(&["install", "--repo", repo_arg(&repo), "file-d"]);
    assert_code(&out, 5);
    assert!(text(&out).contains("文件冲突"));
    assert_eq!(
        fs::read(root.file("/usr/share/shared")).unwrap(),
        b"from-c\n"
    );
    let list = text(&root.run(&["list"]));
    assert!(list.contains("file-c"));
    assert!(!list.contains("file-d"));
    assert!(root.check().status.success());
}

#[test]
fn sigkill_during_install_is_repaired() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("hello", "0.1.0").file("/usr/bin/hello", b"v1\n", 0o755));
    repo.publish();
    let root = Root::new(&repo);

    let out = root.run_lang(
        "en",
        &["install", "--repo", repo_arg(&repo), "hello"],
        &[("SPK_TEST_CRASH", "after-first-write")],
    );
    assert_eq!(out.status.signal(), Some(9), "{}", text(&out));
    assert!(root.path.join("var/lib/spk/journal").exists());
    assert!(root.file("/usr/bin/hello").exists());

    let repair = root.run_zh(&["repair"]);
    assert_ok(&repair);
    assert!(text(&repair).contains("事务已回滚"));
    assert!(!root.file("/usr/bin/hello").exists());
    assert!(!root.path.join("var/lib/spk/journal").exists());
    assert!(root.run(&["list"]).stdout.is_empty());
    assert!(root.check().status.success());

    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "hello"]));
    assert_eq!(fs::read(root.file("/usr/bin/hello")).unwrap(), b"v1\n");
    assert!(root.check().status.success());
}

#[test]
fn sigkill_during_upgrade_restores_old_version() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("hello", "0.1.0").file("/usr/bin/hello", b"v1\n", 0o755));
    repo.publish();
    let root = Root::new(&repo);
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "hello"]));

    repo.add(
        &Pkg::new("hello", "0.2.0")
            .file("/usr/bin/hello", b"v2\n", 0o755)
            .file("/usr/share/hello/version", b"0.2.0\n", 0o644),
    );
    repo.publish();

    let out = root.run_lang(
        "en",
        &["upgrade", "--repo", repo_arg(&repo)],
        &[("SPK_TEST_CRASH", "after-first-write")],
    );
    assert_eq!(out.status.signal(), Some(9), "{}", text(&out));
    assert_ok(&root.run(&["repair"]));
    assert_eq!(fs::read(root.file("/usr/bin/hello")).unwrap(), b"v1\n");
    assert!(!root.file("/usr/share/hello/version").exists());
    let list = text(&root.run(&["list"]));
    assert!(list.contains("hello 0.1.0-1"));
    assert!(root.check().status.success());

    assert_ok(&root.run(&["upgrade", "--repo", repo_arg(&repo)]));
    assert_eq!(fs::read(root.file("/usr/bin/hello")).unwrap(), b"v2\n");
    assert!(root.check().status.success());
}

#[test]
fn conffile_upgrade_and_removal_policy() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("cfg", "1.0.0").conffile("/etc/cfg.conf", b"old\n"));
    repo.publish();
    let root = Root::new(&repo);
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "cfg"]));
    fs::write(root.file("/etc/cfg.conf"), b"user\n").unwrap();

    repo.add(&Pkg::new("cfg", "2.0.0").conffile("/etc/cfg.conf", b"new\n"));
    repo.publish();
    let out = root.run(&["upgrade", "--repo", repo_arg(&repo)]);
    assert_ok(&out);
    assert!(text(&out).contains(".spksave"));
    assert_eq!(fs::read(root.file("/etc/cfg.conf")).unwrap(), b"user\n");
    assert_eq!(
        fs::read(root.file("/etc/cfg.conf.spksave")).unwrap(),
        b"new\n"
    );
    assert!(root.check().status.success());

    let remove = root.run(&["remove", "cfg"]);
    assert_ok(&remove);
    assert_eq!(
        fs::read(root.file("/etc/cfg.conf.spksave.1")).unwrap(),
        b"user\n"
    );
    assert!(!root.file("/etc/cfg.conf").exists());
    assert!(root.check().status.success());

    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "cfg"]));
    let purge = root.run(&["remove", "--purge", "cfg"]);
    assert_ok(&purge);
    assert!(!root.file("/etc/cfg.conf").exists());
    assert!(root.file("/etc/cfg.conf.spksave.1").exists());
    assert!(root.check().status.success());
}

#[test]
fn replaces_removes_old_package() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("old", "1.0.0").file("/usr/bin/old", b"old\n", 0o755));
    repo.publish();
    let root = Root::new(&repo);
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "old"]));

    repo.add(
        &Pkg::new("new", "2.0.0")
            .replaces(&["old"])
            .file("/usr/bin/new", b"new\n", 0o755),
    );
    repo.publish();
    let out = root.run(&["install", "--repo", repo_arg(&repo), "new"]);
    assert_ok(&out);
    assert!(!root.file("/usr/bin/old").exists());
    assert!(root.file("/usr/bin/new").exists());
    let list = text(&root.run(&["list"]));
    assert!(list.contains("new 2.0.0-1"));
    assert!(!list.contains("old"));
    assert!(root.check().status.success());
}

#[test]
fn installs_through_symlinked_directory() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("symtool", "1.0.0").file("/bin/tool", b"tool\n", 0o755));
    repo.publish();
    let root = Root::new(&repo);
    fs::create_dir_all(root.file("/usr/bin")).unwrap();
    symlink("usr/bin", root.file("/bin")).unwrap();

    let out = root.run(&["install", "--repo", repo_arg(&repo), "symtool"]);
    assert_ok(&out);
    assert_eq!(fs::read(root.file("/usr/bin/tool")).unwrap(), b"tool\n");
    assert_eq!(fs::read(root.file("/bin/tool")).unwrap(), b"tool\n");
    assert!(root.check().status.success());

    assert_ok(&root.run(&["remove", "symtool"]));
    assert!(!root.file("/usr/bin/tool").exists());
    assert_eq!(
        fs::read_link(root.file("/bin")).unwrap(),
        Path::new("usr/bin")
    );
    assert!(root.file("/usr/bin").exists());
    assert!(root.check().status.success());
}

#[test]
fn overwrite_unowned_owned_and_glob() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("over-a", "1.0.0").file("/usr/share/ow/shared", b"a\n", 0o644));
    repo.add(&Pkg::new("over-b", "1.0.0").file("/usr/share/ow/shared", b"b\n", 0o644));
    repo.add(&Pkg::new("over-dir", "1.0.0").file("/usr/share/ow/existing/inner", b"i\n", 0o644));
    repo.publish();
    let root = Root::new(&repo);

    // Unowned path: default is a file conflict.
    fs::create_dir_all(root.file("/usr/share/ow")).unwrap();
    fs::write(root.file("/usr/share/ow/shared"), b"unowned\n").unwrap();
    let conflict = root.run(&["install", "--repo", repo_arg(&repo), "over-a"]);
    assert_code(&conflict, 5);
    assert!(text(&conflict).contains("not owned") || text(&conflict).contains("exists"));
    assert_eq!(
        fs::read(root.file("/usr/share/ow/shared")).unwrap(),
        b"unowned\n"
    );

    // Exact overwrite works and check remains green.
    assert_ok(&root.run(&[
        "install",
        "--repo",
        repo_arg(&repo),
        "--overwrite",
        "/usr/share/ow/shared",
        "over-a",
    ]));
    assert_eq!(fs::read(root.file("/usr/share/ow/shared")).unwrap(), b"a\n");
    assert_ok(&root.check());

    // Owned path: default fails; glob overwrite transfers ownership.
    let conflict = root.run(&["install", "--repo", repo_arg(&repo), "over-b"]);
    assert_code(&conflict, 5);
    assert_ok(&root.run(&[
        "install",
        "--repo",
        repo_arg(&repo),
        "--overwrite",
        "/usr/share/ow/*",
        "over-b",
    ]));
    assert_eq!(fs::read(root.file("/usr/share/ow/shared")).unwrap(), b"b\n");
    let owns = text(&root.run(&["owns", "/usr/share/ow/shared"]));
    assert!(owns.contains("over-b"), "{owns}");
    let files_a = text(&root.run(&["files", "over-a"]));
    assert!(!files_a.contains("/usr/share/ow/shared"), "{files_a}");
    assert_ok(&root.check());

    // Existing directory is not a conflict.
    fs::create_dir_all(root.file("/usr/share/ow/existing")).unwrap();
    assert_ok(&root.run(&["install", "--repo", repo_arg(&repo), "over-dir"]));
    assert_ok(&root.check());
}

#[test]
fn overwrite_rejects_relative_and_parent_globs() {
    let repo = TestRepo::new();
    repo.add(&Pkg::new("over-bad", "1.0.0").file("/usr/share/owbad", b"x\n", 0o644));
    repo.publish();
    let root = Root::new(&repo);
    for pattern in ["usr/share/owbad", "/usr/../etc/passwd", "/usr/share/[bad"] {
        let out = root.run(&[
            "install",
            "--repo",
            repo_arg(&repo),
            "--overwrite",
            pattern,
            "over-bad",
        ]);
        assert_code(&out, 2);
    }
    assert_ok(&root.check());
}
