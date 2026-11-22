//
//       user/spk/spk/src/transaction.rs
//       File-level transaction planning, journaled apply, database commit
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use spk_core::archive::extract_spk;
use spk_core::db::{
    FileDb, FileRecord, HoldsDb, InstallReason, InstalledDb, InstalledPackage, WorldDb,
};
use spk_core::fsutil::sha256_file;
use spk_core::index::IndexPackage;
use spk_core::manifest::{validate_absolute_path, FileType, Manifest, PackageFile};

use crate::crash;
use crate::error::{Result, SpkError};
use crate::hooks;
use crate::i18n::{self, Lang};
use crate::journal::{DbBackup, Event, Journal};
use crate::repo::{target_arch, LoadedRepo};
use crate::rootfs::{Fs, OnDiskKind};
use crate::sandbox;
use crate::solver::{Solution, TargetKind};
use crate::state::{write_temp_json, Paths, State};
use crate::triggers;
use crate::ui;
use crate::util::{copy_file, human_size, now, txid as new_txid};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Install,
    Upgrade,
    Remove,
}

pub struct Options {
    pub lang: Lang,
    pub noconfirm: bool,
    pub purge: bool,
    pub operation: OperationKind,
    pub script_failure_keep: bool,
    pub no_sandbox_scripts: bool,
    pub overwrite: crate::overwrite::Overwrite,
    pub interactive: bool,
    pub approved_overwrites: RefCell<BTreeSet<String>>,
}

struct Staged {
    manifest: Manifest,
    dir: PathBuf,
    conffiles: BTreeSet<String>,
    install_script: Option<String>,
}

#[derive(Clone, Debug)]
struct Owner {
    package: String,
    file_type: FileType,
    mode: String,
    uid: u32,
    gid: u32,
    sha256: String,
    size: u64,
    target: Option<String>,
    conffile: bool,
}

impl Owner {
    fn from_record(package: &str, record: &FileRecord) -> Self {
        Self {
            package: package.to_string(),
            file_type: record.file_type,
            mode: record.mode.clone(),
            uid: record.uid,
            gid: record.gid,
            sha256: record.sha256.clone(),
            size: record.size,
            target: record.target.clone(),
            conffile: record.conffile,
        }
    }

    fn from_package_file(package: &str, file: &PackageFile, conffile: bool) -> Self {
        Self {
            package: package.to_string(),
            file_type: file.file_type,
            mode: file.mode.clone(),
            uid: file.uid,
            gid: file.gid,
            sha256: file.sha256.clone(),
            size: file.size,
            target: file.target.clone(),
            conffile,
        }
    }

    fn to_record(&self) -> FileRecord {
        FileRecord {
            pkg: self.package.clone(),
            file_type: self.file_type,
            mode: self.mode.clone(),
            uid: self.uid,
            gid: self.gid,
            sha256: self.sha256.clone(),
            size: self.size,
            target: self.target.clone(),
            conffile: self.conffile,
        }
    }
}

#[derive(Clone, Debug)]
enum WriteSource {
    File(PathBuf),
    Symlink(String),
}

#[derive(Clone, Debug)]
struct WriteOp {
    logical: String,
    source: WriteSource,
    mode: u32,
    uid: u32,
    gid: u32,
    sha256: String,
    kind: &'static str,
}

#[derive(Clone, Debug)]
struct SaveOp {
    logical: String,
    source: PathBuf,
    mode: u32,
    uid: u32,
    gid: u32,
    sha256: String,
}

#[derive(Clone, Debug)]
struct RemoveOp {
    logical: String,
    /// Rename instead of deleting (conffile removal policy).
    rename_to: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Plan {
    dirs: Vec<(String, u32, u32, u32)>,
    writes: Vec<WriteOp>,
    saves: Vec<SaveOp>,
    removes: Vec<RemoveOp>,
    dir_removes: Vec<(String, u32)>,
    installed: InstalledDb,
    files: FileDb,
    world: WorldDb,
    holds: HoldsDb,
    warnings: Vec<String>,
    download_size: u64,
    installed_size: u64,
    removed_size: u64,
    total_installed_size: u64,
    final_count: usize,
}

impl Plan {
    pub(crate) fn changed_paths(&self) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        paths.extend(self.writes.iter().map(|write| write.logical.clone()));
        paths.extend(self.saves.iter().map(|save| save.logical.clone()));
        paths.extend(self.removes.iter().map(|remove| remove.logical.clone()));
        paths
    }

    pub(crate) fn installed(&self) -> &InstalledDb {
        &self.installed
    }

    fn net_size(&self) -> i64 {
        self.installed_size as i64 - self.removed_size as i64
    }
}

pub fn execute(
    paths: &Paths,
    state: &State,
    solution: Solution,
    repos: &[LoadedRepo],
    options: &Options,
) -> Result<()> {
    let txid = new_txid();
    let count = solution.actions.len().max(1);
    let staged = if solution.actions.is_empty() {
        BTreeMap::new()
    } else {
        println!("{}", i18n::retrieving(options.lang));
        let staged = match stage_packages(paths, &solution, repos, &txid, options.lang) {
            Ok(staged) => staged,
            Err(error) => {
                cleanup_dir(&paths.staging.join(&txid));
                return Err(error);
            }
        };
        println!("({count}/{count}) {}", i18n::loading_packages(options.lang));
        staged
    };

    if let Err(error) = check_sudo_configs(paths, &staged, &solution) {
        cleanup_dir(&paths.staging.join(&txid));
        return Err(error);
    }
    let fs = Fs::new(&paths.root)?;
    let plan = match build_plan(paths, state, &solution, &staged, options, &txid) {
        Ok(plan) => plan,
        Err(error) => {
            cleanup_dir(&paths.staging.join(&txid));
            return Err(error);
        }
    };

    print_changes(&solution, &options.lang, options.operation);
    print_summary(&plan, &options.lang, options.operation);
    if !confirm(&options.lang, options.noconfirm, options.operation)? {
        cleanup_dir(&paths.staging.join(&txid));
        return Err(SpkError::General("transaction cancelled".into()));
    }
    if !solution.actions.is_empty() {
        println!(
            "({count}/{count}) {}",
            i18n::checking_file_conflicts(options.lang)
        );
    }
    println!("({count}/{count}) {}", i18n::checking_disk(options.lang));
    check_disk(&fs, &plan)?;

    let hook_packages: Vec<hooks::HookPackage> = solution
        .actions
        .iter()
        .filter_map(|action| staged.get(&action.to.name))
        .map(|staged| hooks::HookPackage {
            name: staged.manifest.name.clone(),
            manifest: staged.manifest.clone(),
            dir: staged.dir.clone(),
        })
        .collect();
    let hooks = hooks::load(paths, &state.installed, &hook_packages)?;
    let mut hook_operations = BTreeSet::new();
    let mut hook_packages_names = BTreeSet::new();
    for action in &solution.actions {
        hook_packages_names.insert(action.to.name.clone());
        hook_operations.insert(match action.kind {
            TargetKind::Install => hooks::Operation::Install,
            _ => hooks::Operation::Upgrade,
        });
    }
    for removal in &solution.removals {
        hook_packages_names.insert(removal.name.clone());
        hook_operations.insert(hooks::Operation::Remove);
    }
    let hook_context = hooks::Context {
        packages: hook_packages_names,
        paths: plan.changed_paths(),
        operations: hook_operations,
    };
    if let Err(error) = hooks::run_pre(paths, &txid, &hooks, &hook_context, options.lang) {
        cleanup_dir(&paths.staging.join(&txid));
        cleanup_dir(&paths.backup.join(&txid));
        return Err(error);
    }

    for warning in &plan.warnings {
        eprintln!("{warning}");
    }

    if let Err(error) = run_scripts(paths, state, &solution, &staged, options, &txid) {
        cleanup_dir(&paths.staging.join(&txid));
        cleanup_dir(&paths.backup.join(&txid));
        let _ = fs::remove_dir_all(paths.sandbox.join(&txid));
        let _ = fs::remove_dir_all(paths.scripts.join(&txid));
        return Err(error);
    }
    // Scriptlets may rewrite declared files; rebuild the plan so file DB and
    // journal hashes describe the contents that will actually be installed.
    let plan = match build_plan(paths, state, &solution, &staged, options, &txid) {
        Ok(plan) => plan,
        Err(error) => {
            cleanup_dir(&paths.staging.join(&txid));
            cleanup_dir(&paths.backup.join(&txid));
            return Err(error);
        }
    };
    check_disk(&fs, &plan)?;

    println!("{}", i18n::processing(options.lang));

    let result = apply(paths, &fs, &plan, &solution, &txid, options.lang);
    if let Err(error) = result {
        match crate::repair::rollback_pending(paths, options.lang, true) {
            Ok(()) => {
                cleanup_dir(&paths.staging.join(&txid));
                cleanup_dir(&paths.backup.join(&txid));
                return Err(SpkError::RolledBack(format!(
                    "transaction failed: {error}; previous state restored"
                )));
            }
            Err(repair_error) => {
                return Err(SpkError::General(format!(
                    "transaction failed: {error}; automatic rollback failed: {repair_error}; run 'spk repair'"
                )));
            }
        }
    }

    if let Err(error) = hooks::run_post(paths, &txid, &hooks, &hook_context, options.lang) {
        eprintln!("{error}");
    }
    if let Err(error) = triggers::run(paths, &plan, &solution, &txid, options.lang) {
        eprintln!("{error}");
    }
    cleanup_dir(&paths.staging.join(&txid));
    cleanup_dir(&paths.backup.join(&txid));
    let _ = fs::remove_dir_all(paths.state.join("hooks").join(&txid));
    let _ = fs::remove_dir_all(paths.state.join("triggers").join(&txid));
    let _ = fs::remove_dir_all(paths.sandbox.join(&txid));
    let _ = fs::remove_dir_all(paths.scripts.join(&txid));
    let keep = crate::remote::package_cache_keep(repos);
    let _ = crate::remote::prune_package_cache(paths, &keep);
    let changed: Vec<String> = solution
        .actions
        .iter()
        .map(|action| format!("{} {}", action.to.name, action.to.version))
        .collect();
    let _ = crate::audit::record(paths, options.operation, &changed, "ok");
    println!(
        "{}",
        i18n::ok_summary(
            options.lang,
            &human_size(plan.total_installed_size),
            plan.final_count
        )
    );
    Ok(())
}

fn print_changes(solution: &Solution, lang: &Lang, operation: OperationKind) {
    let mut names = Vec::new();
    for action in &solution.actions {
        names.push(format!("{} {}", action.to.name, action.to.version));
    }
    for removal in &solution.removals {
        let version = spk_core::version::Version::new(
            removal.epoch,
            removal.version.clone(),
            removal.release,
        );
        if operation == OperationKind::Remove {
            names.push(format!("{} {}", removal.name, version));
        } else {
            names.push(format!("{} {} [removal]", removal.name, version));
        }
    }
    if names.is_empty() {
        return;
    }
    println!();
    println!(
        "{} {}",
        i18n::packages(*lang, names.len()),
        names.join("  ")
    );
}

fn print_summary(plan: &Plan, lang: &Lang, operation: OperationKind) {
    if plan.download_size > 0 {
        println!();
        println!(
            "{:<20} {}",
            i18n::total_download(*lang),
            human_size(plan.download_size)
        );
    }
    if plan.installed_size > 0 {
        println!(
            "{:<20} {}",
            i18n::total_installed(*lang),
            human_size(plan.installed_size)
        );
    }
    if plan.removed_size > 0 && (operation == OperationKind::Remove || plan.installed_size == 0) {
        println!(
            "{:<20} {}",
            i18n::total_removed(*lang),
            human_size(plan.removed_size)
        );
    }
    if operation != OperationKind::Remove && plan.net_size() != 0 {
        let size = if plan.net_size() >= 0 {
            human_size(plan.net_size() as u64)
        } else {
            format!("-{}", human_size((-plan.net_size()) as u64))
        };
        println!("{:<20} {}", i18n::net_upgrade(*lang), size);
    }
    println!();
}

fn confirm(lang: &Lang, noconfirm: bool, operation: OperationKind) -> Result<bool> {
    if noconfirm {
        return Ok(true);
    }
    let prompt = match operation {
        OperationKind::Remove => i18n::proceed_remove(*lang),
        _ => i18n::proceed_install(*lang),
    };
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}

fn check_sudo_configs(
    paths: &Paths,
    staged: &BTreeMap<String, Staged>,
    solution: &Solution,
) -> Result<()> {
    let sudo = paths.root.join("usr/bin/sudo");
    for action in &solution.actions {
        let Some(package) = staged.get(&action.to.name) else {
            continue;
        };
        for file in &package.manifest.files {
            let doas = file.path == "/etc/doas.conf";
            if file.path != "/etc/sudoers" && !doas {
                continue;
            }
            if file.file_type != FileType::File {
                return Err(SpkError::General(format!(
                    "{}: sudo configuration must be a regular file",
                    file.path
                )));
            }
            if !sudo.is_file() {
                return Err(SpkError::General(format!(
                    "cannot validate {}: /usr/bin/sudo is not installed (fail closed)",
                    file.path
                )));
            }
            let staged_path = package.dir.join(file.path.trim_start_matches('/'));
            let mut command = std::process::Command::new(&sudo);
            command.arg(if doas {
                "--check-doas"
            } else {
                "--check-config"
            });
            command.arg(&staged_path);
            let output = command.output().map_err(|error| {
                SpkError::General(format!("cannot run sudo configuration check: {error}"))
            })?;
            if !output.status.success() {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return Err(SpkError::General(format!(
                    "{}: sudo configuration rejected: {}",
                    file.path,
                    text.trim()
                )));
            }
        }
    }
    Ok(())
}

fn version_string(epoch: u64, version: &str, release: u64) -> String {
    if epoch == 0 {
        format!("{version}-{release}")
    } else {
        format!("{epoch}:{version}-{release}")
    }
}

fn script_function(kind: TargetKind, has_from: bool, pre: bool) -> &'static str {
    match (kind, has_from, pre) {
        (TargetKind::Install, _, true) => "pre_install",
        (TargetKind::Install, _, false) => "post_install",
        (_, true, true) => "pre_upgrade",
        (_, true, false) => "post_upgrade",
        (_, false, true) => "pre_install",
        (_, false, false) => "post_install",
    }
}

fn run_scripts(
    paths: &Paths,
    state: &State,
    solution: &Solution,
    staged: &BTreeMap<String, Staged>,
    options: &Options,
    txid: &str,
) -> Result<()> {
    for action in &solution.actions {
        let Some(staged_package) = staged.get(&action.to.name) else {
            continue;
        };
        let Some(script) = staged_package.install_script.as_deref() else {
            continue;
        };
        let new = version_string(
            staged_package.manifest.epoch,
            &staged_package.manifest.version,
            staged_package.manifest.release,
        );
        let old = action
            .from
            .as_ref()
            .map(|old| version_string(old.epoch, &old.version, old.release));
        let before = sandbox::snapshot_stage(&staged_package.dir)?;
        let mut failed = None;
        for pre in [true, false] {
            let function = script_function(action.kind, old.is_some(), pre);
            let args = if matches!(action.kind, TargetKind::Install) || old.is_none() {
                vec![new.clone()]
            } else {
                vec![new.clone(), old.clone().unwrap_or_default()]
            };
            if let Err(error) = sandbox::run_install_script(
                paths,
                txid,
                &action.to.name,
                &staged_package.dir,
                script,
                function,
                &args,
                options.lang,
                !options.no_sandbox_scripts,
            ) {
                failed = Some((function.to_string(), error));
                break;
            }
        }
        let after = sandbox::snapshot_stage(&staged_package.dir)?;
        if let Err(error) =
            sandbox::verify_stage(&staged_package.manifest, &before, &after, options.lang)
        {
            if !options.script_failure_keep {
                return Err(error);
            }
            eprintln!(
                "{}",
                i18n::script_failure_kept(options.lang, "staging", &error.to_string())
            );
        }
        if let Some((function, error)) = failed {
            if options.script_failure_keep {
                eprintln!(
                    "{}",
                    i18n::script_failure_kept(options.lang, &function, &error.to_string())
                );
            } else {
                return Err(error);
            }
        }
    }

    for removal in &solution.removals {
        let Some(installed) = state.installed.packages.get(&removal.name) else {
            continue;
        };
        let Some(script) = installed.install_script.as_deref() else {
            continue;
        };
        let old = version_string(removal.epoch, &removal.version, removal.release);
        let empty = paths
            .staging
            .join(txid)
            .join(format!(".remove-{}", removal.name));
        if empty.exists() {
            let _ = fs::remove_dir_all(&empty);
        }
        fs::create_dir_all(&empty)?;
        for function in ["pre_remove", "post_remove"] {
            if let Err(error) = sandbox::run_install_script(
                paths,
                txid,
                &removal.name,
                &empty,
                script,
                function,
                std::slice::from_ref(&old),
                options.lang,
                !options.no_sandbox_scripts,
            ) {
                if options.script_failure_keep {
                    eprintln!(
                        "{}",
                        i18n::script_failure_kept(options.lang, function, &error.to_string())
                    );
                } else {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

fn stage_packages(
    paths: &Paths,
    solution: &Solution,
    repos: &[LoadedRepo],
    txid: &str,
    lang: Lang,
) -> Result<BTreeMap<String, Staged>> {
    let mut staged = BTreeMap::new();
    let total = solution.actions.len().max(1);
    for (position, action) in solution.actions.iter().enumerate() {
        let (repo_index, index) = match (&action.to.origin, &action.to.repo_pkg) {
            (crate::solver::Origin::Repo(index), Some(package)) => (*index, package),
            _ => {
                return Err(SpkError::General(format!(
                    "{} has no repository package",
                    action.to.name
                )))
            }
        };
        let repo = repos.get(repo_index).ok_or_else(|| {
            SpkError::General(format!("repository index {repo_index} disappeared"))
        })?;
        let label = format!("({}/{total}) {}", position + 1, index.filename);
        let mut bar = ui::Bar::new(label, Some(index.size));
        let package_path = repo.ensure_package_with_progress(
            index,
            &mut |written| bar.advance(written),
            lang,
        )?;
        bar.finish();
        let metadata = fs::metadata(&package_path)
            .map_err(|error| SpkError::General(format!("{}: {error}", package_path.display())))?;
        if metadata.len() != index.size {
            return Err(SpkError::General(i18n::package_size_mismatch(
                lang,
                &index.filename,
                metadata.len(),
                index.size,
            )));
        }
        if sha256_file(&package_path)? != index.sha256 {
            return Err(SpkError::Signature(format!(
                "{}: sha256 does not match the repository index",
                index.filename
            )));
        }
        let stage_dir = paths.staging.join(txid).join(&index.name);
        let scanned = extract_spk(&package_path, &stage_dir)?;
        if !index_matches(index, &scanned.manifest) {
            return Err(SpkError::General(format!(
                "{}: manifest does not match the repository index",
                index.filename
            )));
        }
        if scanned.manifest.arch != target_arch() {
            return Err(SpkError::General(format!(
                "{}: package architecture {} does not match {}",
                index.filename,
                scanned.manifest.arch,
                target_arch()
            )));
        }
        let install_script = match scanned.install_script {
            Some(bytes) => Some(String::from_utf8(bytes).map_err(|_| {
                SpkError::General(format!("{}: .INSTALL is not valid UTF-8", index.filename))
            })?),
            None => None,
        };
        let _ = lang;
        check_setuid_policy(paths, &scanned.manifest, lang)?;
        let conffiles = scanned.manifest.conffiles.iter().cloned().collect();
        staged.insert(
            index.name.clone(),
            Staged {
                manifest: scanned.manifest,
                dir: stage_dir,
                conffiles,
                install_script,
            },
        );
    }
    Ok(staged)
}

fn check_setuid_policy(paths: &Paths, manifest: &Manifest, lang: Lang) -> Result<()> {
    let setuid: Vec<&PackageFile> = manifest
        .files
        .iter()
        .filter(|file| {
            file.mode_bits()
                .map(|mode| mode & 0o6000 != 0)
                .unwrap_or(false)
        })
        .collect();
    if setuid.is_empty() {
        return Ok(());
    }
    if !manifest.allow_setuid {
        return Err(SpkError::General(format!(
            "{}: setuid bits require allow_setuid",
            manifest.name
        )));
    }
    let whitelist = paths.root.join("etc/spk/setuid.allow");
    let metadata = fs::symlink_metadata(&whitelist).map_err(|error| {
        SpkError::General(format!(
            "{}: cannot read setuid whitelist: {error}",
            whitelist.display()
        ))
    })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o777 != 0o644
    {
        return Err(SpkError::General(format!(
            "{} must be root:root mode 0644",
            whitelist.display()
        )));
    }
    let text = fs::read_to_string(&whitelist)?;
    let allowed: BTreeSet<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    for file in setuid {
        if !allowed.contains(file.path.as_str()) {
            return Err(SpkError::General(format!(
                "setuid path {} is not listed in {}",
                file.path,
                whitelist.display()
            )));
        }
    }
    eprintln!("{}", i18n::setuid_preserved(lang, &manifest.name));
    Ok(())
}

fn index_matches(index: &IndexPackage, manifest: &Manifest) -> bool {
    index.name == manifest.name
        && index.epoch == manifest.epoch
        && index.version == manifest.version
        && index.release == manifest.release
        && index.arch == manifest.arch
        && index.abi == manifest.abi
        && index.depends == manifest.depends
        && index.provides == manifest.provides
        && index.conflicts == manifest.conflicts
        && index.replaces == manifest.replaces
}

fn staged_digest(staged: &Staged, file: &PackageFile) -> Result<(String, u64)> {
    match file.file_type {
        FileType::Dir => Ok((String::new(), 0)),
        FileType::Symlink => {
            let target = file.target.as_deref().ok_or_else(|| {
                SpkError::General(format!("{}: symlink without target", file.path))
            })?;
            Ok((
                spk_core::fsutil::sha256_bytes(target.as_bytes()),
                target.len() as u64,
            ))
        }
        FileType::File => {
            let path = staged.dir.join(file.path.trim_start_matches('/'));
            let meta = fs::metadata(&path)
                .map_err(|error| SpkError::General(format!("{}: {error}", path.display())))?;
            Ok((sha256_file(&path)?, meta.len()))
        }
    }
}

fn approve_overwrite(
    options: &Options,
    path: &str,
    owner: Option<&str>,
    replaces_owner: bool,
) -> Result<bool> {
    if replaces_owner || options.overwrite.matches(path) {
        return Ok(true);
    }
    if options.approved_overwrites.borrow().contains(path) {
        return Ok(true);
    }
    if !options.interactive || options.noconfirm || !io::stdin().is_terminal() {
        return Ok(false);
    }
    let question = match owner {
        Some(owner) => i18n::overwrite_owned_prompt(options.lang, path, owner),
        None => i18n::overwrite_unowned_prompt(options.lang, path),
    };
    eprint!("{question}");
    io::stderr().flush().ok();
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return Ok(false);
    }
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        options
            .approved_overwrites
            .borrow_mut()
            .insert(path.to_string());
        Ok(true)
    } else {
        Ok(false)
    }
}

fn build_plan(
    paths: &Paths,
    state: &State,
    solution: &Solution,
    staged: &BTreeMap<String, Staged>,
    options: &Options,
    txid: &str,
) -> Result<Plan> {
    let fs = Fs::new(&paths.root)?;
    let changed: BTreeSet<String> = solution.actions.iter().map(|a| a.to.name.clone()).collect();
    let removed: BTreeSet<String> = solution.removals.iter().map(|p| p.name.clone()).collect();
    let affected: BTreeSet<String> = changed.union(&removed).cloned().collect();

    let mut owners: BTreeMap<String, Owner> = BTreeMap::new();
    for (path, record) in &state.files.files {
        if !affected.contains(&record.pkg) {
            owners.insert(path.clone(), Owner::from_record(&record.pkg, record));
        }
    }

    let mut dirs: BTreeMap<String, (u32, u32, u32)> = BTreeMap::new();
    let mut kept_dirs: BTreeSet<String> = BTreeSet::new();
    for (name, package) in &state.installed.packages {
        if affected.contains(name) {
            continue;
        }
        for file in &package.files {
            if file.file_type == FileType::Dir {
                kept_dirs.insert(file.path.clone());
            }
        }
    }
    let mut takeovers: BTreeMap<String, String> = BTreeMap::new();
    let mut warnings = Vec::new();
    for action in &solution.actions {
        let staged = staged
            .get(&action.to.name)
            .ok_or_else(|| SpkError::General(format!("{} was not staged", action.to.name)))?;
        for file in &staged.manifest.files {
            if file.file_type == FileType::Dir {
                dirs.insert(file.path.clone(), (file.mode_bits()?, file.uid, file.gid));
                continue;
            }
            let (sha256, size) = staged_digest(staged, file)?;
            let mut owner = Owner::from_package_file(
                &action.to.name,
                file,
                staged.conffiles.contains(&file.path),
            );
            owner.sha256 = sha256;
            owner.size = size;
            let existing_package = owners
                .get(&file.path)
                .map(|existing| existing.package.clone());
            if let Some(existing_package) = existing_package {
                if existing_package != owner.package {
                    let replaces_owner = staged.manifest.replaces.iter().any(|entry| {
                        entry.split(['<', '>', '=']).next() == Some(existing_package.as_str())
                    });
                    if approve_overwrite(
                        options,
                        &file.path,
                        Some(&existing_package),
                        replaces_owner,
                    )? {
                        takeovers.insert(file.path.clone(), existing_package.clone());
                        owners.remove(&file.path);
                        warnings.push(i18n::overwrite_takeover_warning(
                            options.lang,
                            &file.path,
                            &owner.package,
                            &existing_package,
                        ));
                    } else {
                        return Err(SpkError::FileConflict {
                            message: i18n::file_conflict_header(options.lang).to_string(),
                            lines: vec![i18n::overwrite_owned_detail(
                                options.lang,
                                &file.path,
                                &owner.package,
                                &existing_package,
                            )],
                        });
                    }
                }
            }
            owners.insert(file.path.clone(), owner);
        }
    }

    for path in owners.keys() {
        let mut ancestor = path.as_str();
        while let Some((parent, _)) = ancestor.rsplit_once('/') {
            if parent.is_empty() {
                break;
            }
            dirs.entry(parent.to_string()).or_insert((0o755, 0, 0));
            ancestor = parent;
        }
    }
    for path in dirs.keys() {
        let on_disk = fs.on_disk(path).map_err(|_| SpkError::FileConflict {
            message: i18n::file_conflict_header(options.lang).to_string(),
            lines: vec![i18n::file_conflict_unowned(options.lang, path, "package")],
        })?;
        if let Some(on_disk) = on_disk {
            if on_disk.kind != OnDiskKind::Dir && !fs.is_dir(path).unwrap_or(false) {
                return Err(SpkError::FileConflict {
                    message: i18n::file_conflict_header(options.lang).to_string(),
                    lines: vec![i18n::file_conflict_unowned(options.lang, path, "package")],
                });
            }
        }
    }

    let mut writes = Vec::new();
    let mut saves = Vec::new();
    let mut installed_size = 0u64;
    for action in &solution.actions {
        let staged = staged.get(&action.to.name).expect("staged");
        for file in &staged.manifest.files {
            if file.file_type == FileType::Dir {
                continue;
            }
            let (actual_sha256, actual_size) = staged_digest(staged, file)?;
            installed_size += actual_size;
            let old_record = state.files.files.get(&file.path);
            let on_disk = fs.on_disk(&file.path).map_err(|_| SpkError::FileConflict {
                message: i18n::file_conflict_header(options.lang).to_string(),
                lines: vec![i18n::file_conflict_unowned(
                    options.lang,
                    &file.path,
                    &action.to.name,
                )],
            })?;
            if old_record.is_none() {
                if on_disk.is_some() {
                    if approve_overwrite(options, &file.path, None, false)? {
                        warnings.push(i18n::overwrite_unowned_warning(options.lang, &file.path));
                    } else {
                        return Err(SpkError::FileConflict {
                            message: i18n::file_conflict_header(options.lang).to_string(),
                            lines: vec![i18n::overwrite_unowned_detail(
                                options.lang,
                                &file.path,
                                &action.to.name,
                            )],
                        });
                    }
                }
            } else if let Some(record) = old_record {
                if record.pkg != action.to.name
                    && !affected.contains(&record.pkg)
                    && !takeovers.contains_key(&file.path)
                {
                    return Err(SpkError::FileConflict {
                        message: i18n::file_conflict_header(options.lang).to_string(),
                        lines: vec![i18n::overwrite_owned_detail(
                            options.lang,
                            &file.path,
                            &action.to.name,
                            &record.pkg,
                        )],
                    });
                }
            }
            let is_conffile = staged.conffiles.contains(&file.path);
            if is_conffile {
                if let (Some(record), Some(entry)) = (old_record, on_disk.as_ref()) {
                    if record.conffile && entry.kind == OnDiskKind::File {
                        let user_modified = entry.sha256 != record.sha256;
                        let upstream_changed = actual_sha256 != record.sha256;
                        if user_modified && upstream_changed {
                            let saved = unique_save_path(&fs, paths, state, &owners, &file.path)?;
                            saves.push(SaveOp {
                                logical: saved.clone(),
                                source: staged.dir.join(file.path.trim_start_matches('/')),
                                mode: file.mode_bits()?,
                                uid: file.uid,
                                gid: file.gid,
                                sha256: actual_sha256.clone(),
                            });
                            warnings.push(i18n::warning_conffile(options.lang, &file.path, &saved));
                            continue;
                        }
                        if user_modified {
                            // Upstream did not change: keep the user file.
                            continue;
                        }
                    }
                }
            }
            let source = staged.dir.join(file.path.trim_start_matches('/'));
            let write = match file.file_type {
                FileType::File => WriteOp {
                    logical: file.path.clone(),
                    source: WriteSource::File(source),
                    mode: file.mode_bits()?,
                    uid: file.uid,
                    gid: file.gid,
                    sha256: actual_sha256.clone(),
                    kind: "file",
                },
                FileType::Symlink => WriteOp {
                    logical: file.path.clone(),
                    source: WriteSource::Symlink(file.target.clone().ok_or_else(|| {
                        SpkError::General(format!("{}: symlink without target", file.path))
                    })?),
                    mode: file.mode_bits()?,
                    uid: file.uid,
                    gid: file.gid,
                    sha256: actual_sha256.clone(),
                    kind: "symlink",
                },
                FileType::Dir => unreachable!(),
            };
            writes.push(write);
        }
    }

    let mut removes = Vec::new();
    let mut dir_removes: BTreeMap<String, u32> = BTreeMap::new();
    let mut removed_size = 0u64;
    for name in &affected {
        let Some(old) = state.installed.packages.get(name) else {
            continue;
        };
        for file in &old.files {
            removed_size += file.size;
            if file.file_type == FileType::Dir {
                if !dirs.contains_key(&file.path)
                    && !kept_dirs.contains(&file.path)
                    && matches!(fs.on_disk(&file.path)?, Some(entry) if entry.kind == OnDiskKind::Dir)
                {
                    dir_removes.insert(file.path.clone(), file.mode_bits()?);
                }
                continue;
            }
            if owners.contains_key(&file.path) {
                continue;
            }
            let Some(record) = state.files.files.get(&file.path) else {
                continue;
            };
            if fs.on_disk(&file.path)?.is_none() {
                continue;
            }
            let rename_to = if record.conffile && !options.purge {
                Some(unique_save_path(&fs, paths, state, &owners, &file.path)?)
            } else {
                None
            };
            removes.push(RemoveOp {
                logical: file.path.clone(),
                rename_to,
            });
        }
    }
    removes.sort_by(|a, b| a.logical.cmp(&b.logical));
    let mut dir_removes: Vec<(String, u32)> = dir_removes.into_iter().collect();
    dir_removes.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

    let mut installed = InstalledDb::default();
    for (name, package) in &state.installed.packages {
        if !affected.contains(name) {
            installed.packages.insert(name.clone(), package.clone());
        }
    }
    for (path, old_owner) in &takeovers {
        if let Some(package) = installed.packages.get_mut(old_owner) {
            package.files.retain(|file| file.path != *path);
            package.conffiles.retain(|conffile| conffile != path);
        }
    }
    let mut world = state.world.clone();
    for name in &removed {
        installed.packages.remove(name);
        world.packages.remove(name);
    }
    for action in &solution.actions {
        let staged = staged.get(&action.to.name).expect("staged");
        let reason = match &action.from {
            Some(old) => old.install_reason,
            None if solution.explicit.contains(&action.to.name) => InstallReason::Explicit,
            None => InstallReason::Dependency,
        };
        let mut installed_files = staged.manifest.files.clone();
        for file in &mut installed_files {
            if file.file_type != FileType::Dir {
                let (sha256, size) = staged_digest(staged, file)?;
                file.sha256 = sha256;
                file.size = size;
            }
        }
        let package = InstalledPackage {
            name: staged.manifest.name.clone(),
            epoch: staged.manifest.epoch,
            version: staged.manifest.version.clone(),
            release: staged.manifest.release,
            arch: staged.manifest.arch.clone(),
            abi: staged.manifest.abi.clone(),
            files: installed_files,
            depends: staged.manifest.depends.clone(),
            provides: staged.manifest.provides.clone(),
            conflicts: staged.manifest.conflicts.clone(),
            replaces: staged.manifest.replaces.clone(),
            conffiles: staged.manifest.conffiles.clone(),
            scripts: staged.manifest.scripts,
            triggers: staged.manifest.triggers.clone(),
            install_script: staged.install_script.clone(),
            allow_setuid: staged.manifest.allow_setuid,
            install_date: now(),
            install_reason: reason,
            txid: txid.to_string(),
        };
        if reason == InstallReason::Explicit {
            world.packages.insert(package.name.clone());
        }
        installed.packages.insert(package.name.clone(), package);
    }
    if !world
        .packages
        .is_subset(&installed.packages.keys().cloned().collect())
    {
        return Err(SpkError::General(
            "world database references an uninstalled package".into(),
        ));
    }

    let mut files = FileDb::default();
    for (path, owner) in &owners {
        files.files.insert(path.clone(), owner.to_record());
    }
    let total_installed_size: u64 = files.files.values().map(|record| record.size).sum();
    let mut holds = state.holds.clone();
    for name in &removed {
        holds.packages.remove(name);
    }
    for name in &solution.cleared_holds {
        holds.packages.remove(name);
    }
    let final_count = installed.packages.len();
    let download_size: u64 = solution
        .actions
        .iter()
        .filter_map(|action| action.to.repo_pkg.as_ref().map(|package| package.size))
        .sum();

    dirs.retain(|path, _| !fs.exists(path).unwrap_or(false));
    let mut dirs: Vec<(String, u32, u32, u32)> = dirs
        .into_iter()
        .map(|(path, (mode, uid, gid))| (path, mode, uid, gid))
        .collect();
    dirs.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));

    Ok(Plan {
        dirs,
        writes,
        saves,
        removes,
        dir_removes,
        installed,
        files,
        world,
        holds,
        warnings,
        download_size,
        installed_size,
        removed_size,
        total_installed_size,
        final_count,
    })
}

fn unique_save_path(
    fs: &Fs,
    paths: &Paths,
    state: &State,
    owners: &BTreeMap<String, Owner>,
    logical: &str,
) -> Result<String> {
    for index in 0..1000 {
        let candidate = if index == 0 {
            format!("{logical}.spksave")
        } else {
            format!("{logical}.spksave.{index}")
        };
        validate_absolute_path(&candidate)?;
        if state.files.files.contains_key(&candidate) || owners.contains_key(&candidate) {
            continue;
        }
        let resolved = paths.root.join(candidate.trim_start_matches('/'));
        if fs.on_disk(&candidate)?.is_none() && fs::symlink_metadata(&resolved).is_err() {
            return Ok(candidate);
        }
    }
    Err(SpkError::General(format!(
        "cannot find a free .spksave name for {logical}"
    )))
}

fn check_disk(fs: &Fs, plan: &Plan) -> Result<()> {
    let stats = rustix::fs::statfs(&fs.root)
        .map_err(|error| SpkError::General(format!("cannot read free space: {error}")))?;
    let available = stats.f_bavail.saturating_mul(stats.f_bsize.max(1) as u64);
    let needed: u64 = plan
        .writes
        .iter()
        .map(|write| match &write.source {
            WriteSource::File(path) => fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            WriteSource::Symlink(target) => target.len() as u64,
        })
        .sum();
    let needed = needed.saturating_add(64 * 1024 * 1024 / 64);
    if needed > available {
        return Err(SpkError::General(format!(
            "not enough free disk space: need {}, available {}",
            human_size(needed),
            human_size(available)
        )));
    }
    Ok(())
}

fn apply(
    paths: &Paths,
    fs: &Fs,
    plan: &Plan,
    solution: &Solution,
    txid: &str,
    lang: Lang,
) -> Result<()> {
    let mut journal = Journal::begin(
        &paths.journal,
        txid,
        match plan.writes.is_empty() {
            true if plan.removes.is_empty() => "noop",
            true => "remove",
            false => "install",
        },
        &solution
            .actions
            .iter()
            .map(|action| action.to.name.clone())
            .chain(solution.removals.iter().map(|package| package.name.clone()))
            .collect::<Vec<_>>(),
    )?;
    let backup_root = paths.backup.join(txid).join("fs");

    let mut created = Vec::new();
    for (path, mode, uid, gid) in &plan.dirs {
        let mut new_dirs = Vec::new();
        fs.ensure_dir(path, *mode, *uid, *gid, &mut new_dirs)?;
        for (created_path, created_mode) in new_dirs {
            journal.append(&Event::MkDir {
                path: created_path.clone(),
                mode: created_mode,
            })?;
            created.push(created_path);
        }
    }

    let mut first_write = true;
    for (index, action) in solution.actions.iter().enumerate() {
        let label = match action.kind {
            TargetKind::Install => i18n::installing(lang),
            TargetKind::Upgrade => i18n::upgrading(lang),
            TargetKind::Downgrade => i18n::downgrading(lang),
            TargetKind::Reinstall => i18n::reinstalling(lang),
        };
        let from = action
            .from
            .as_ref()
            .map(|old| format!("{}-{}", old.version, old.release));
        match (action.kind, from) {
            (TargetKind::Reinstall, Some(_)) | (_, None) => println!(
                "({}/{}) {} {} ({})",
                index + 1,
                solution.actions.len().max(1),
                label,
                action.to.name,
                action.to.version
            ),
            (_, Some(old)) => println!(
                "({}/{}) {} {} ({} -> {})",
                index + 1,
                solution.actions.len().max(1),
                label,
                action.to.name,
                old,
                action.to.version
            ),
        }
    }
    let total_changes = solution.actions.len() + solution.removals.len();
    for (offset, removal) in solution.removals.iter().enumerate() {
        println!(
            "({}/{}) {} {} ({}-{})",
            solution.actions.len() + offset + 1,
            total_changes.max(1),
            i18n::removing(lang),
            removal.name,
            removal.version,
            removal.release
        );
    }

    let mut bar = ui::Bar::counter(i18n::writing_files(lang), plan.writes.len() as u64);
    for write in &plan.writes {
        let old = fs.on_disk(&write.logical)?;
        if let Some(old) = &old {
            let backup = backup_root.join(write.logical.trim_start_matches('/'));
            let backup_text = backup.to_string_lossy().into_owned();
            journal.append(&Event::Backup {
                path: write.logical.clone(),
                backup: backup_text,
                old_sha256: Some(old.sha256.clone()),
                kind: kind_name(old.kind).to_string(),
            })?;
            fs.backup(&write.logical, &backup)?;
        }
        journal.append(&Event::Write {
            path: write.logical.clone(),
            new_sha256: Some(write.sha256.clone()),
            kind: write.kind.to_string(),
        })?;
        match &write.source {
            WriteSource::File(source) => fs.write_from(
                source,
                &write.logical,
                write.mode,
                write.uid,
                write.gid,
                txid,
            )?,
            WriteSource::Symlink(target) => {
                fs.write_symlink(target, &write.logical, write.uid, write.gid, txid)?
            }
        }
        if first_write {
            first_write = false;
            crash::point("after-first-write");
        }
        bar.advance(1);
    }
    bar.finish();

    for save in &plan.saves {
        journal.append(&Event::Write {
            path: save.logical.clone(),
            new_sha256: Some(save.sha256.clone()),
            kind: "file".into(),
        })?;
        fs.write_from(
            &save.source,
            &save.logical,
            save.mode,
            save.uid,
            save.gid,
            txid,
        )?;
    }

    verify_written(fs, plan, lang)?;

    for remove in &plan.removes {
        if let Some(rename_to) = &remove.rename_to {
            journal.append(&Event::Rename {
                from: remove.logical.clone(),
                to: rename_to.clone(),
            })?;
            fs.rename(&remove.logical, rename_to)?;
            continue;
        }
        let old = fs.on_disk(&remove.logical)?;
        if let Some(old) = &old {
            let backup = backup_root.join(remove.logical.trim_start_matches('/'));
            journal.append(&Event::Backup {
                path: remove.logical.clone(),
                backup: backup.to_string_lossy().into_owned(),
                old_sha256: Some(old.sha256.clone()),
                kind: kind_name(old.kind).to_string(),
            })?;
            fs.backup(&remove.logical, &backup)?;
        }
        journal.append(&Event::Remove {
            path: remove.logical.clone(),
        })?;
        fs.remove(&remove.logical)?;
    }

    for (path, mode) in &plan.dir_removes {
        journal.append(&Event::RmDir {
            path: path.clone(),
            mode: *mode,
        })?;
        fs.remove_dir(path)?;
    }

    commit_db(
        paths,
        &plan.installed,
        &plan.files,
        &plan.world,
        &plan.holds,
        txid,
        &mut journal,
    )?;
    journal.finish()?;
    Ok(())
}

fn commit_db(
    paths: &Paths,
    installed: &InstalledDb,
    files: &FileDb,
    world: &WorldDb,
    holds: &HoldsDb,
    txid: &str,
    journal: &mut Journal,
) -> Result<()> {
    let temps = vec![
        (
            "installed.json",
            write_temp_json(paths, "installed.json", installed, txid)?,
        ),
        (
            "files.json",
            write_temp_json(paths, "files.json", files, txid)?,
        ),
        (
            "world.json",
            write_temp_json(paths, "world.json", world, txid)?,
        ),
        (
            "holds.json",
            write_temp_json(paths, "holds.json", holds, txid)?,
        ),
    ];
    let db_backup = paths.backup.join(txid).join("db");
    fs::create_dir_all(&db_backup)?;
    let mut backups = Vec::new();
    for (name, _) in &temps {
        let source = paths.db_path(name);
        let existed = fs::symlink_metadata(&source).is_ok();
        let target = db_backup.join(name);
        if existed {
            copy_file(&source, &target, 0o644)?;
        }
        backups.push(DbBackup {
            logical: format!("/var/lib/spk/{name}"),
            backup: target.to_string_lossy().into_owned(),
            existed,
        });
    }
    journal.append(&Event::DbBegin { files: backups })?;
    crash::point("after-db-begin");
    for (name, temp) in temps {
        fs::rename(temp, paths.db_path(name))?;
    }
    fs::File::open(&paths.state)?.sync_all()?;
    journal.append(&Event::DbDone)?;
    crash::point("after-db-done");
    Ok(())
}

fn verify_written(fs: &Fs, plan: &Plan, lang: Lang) -> Result<()> {
    let privileged = unsafe { libc::geteuid() } == 0;
    for write in &plan.writes {
        let entry = fs.on_disk(&write.logical)?.ok_or_else(|| {
            SpkError::General(format!("{} is missing after install", write.logical))
        })?;
        if write.mode & 0o6000 != 0 && entry.uid != 0 {
            return Err(SpkError::Security(i18n::setuid_not_root(
                lang,
                &write.logical,
                entry.uid,
            )));
        }
        if privileged && (entry.uid != write.uid || entry.gid != write.gid) {
            return Err(SpkError::Security(i18n::owner_mismatch(
                lang,
                &write.logical,
                write.uid,
                write.gid,
                entry.uid,
                entry.gid,
            )));
        }
        if write.kind == "file" && entry.mode & 0o7777 != write.mode & 0o7777 {
            return Err(SpkError::Security(i18n::mode_mismatch(
                lang,
                &write.logical,
                write.mode & 0o7777,
                entry.mode & 0o7777,
            )));
        }
    }
    for save in &plan.saves {
        let entry = fs.on_disk(&save.logical)?.ok_or_else(|| {
            SpkError::General(format!("{} is missing after install", save.logical))
        })?;
        if privileged && (entry.uid != save.uid || entry.gid != save.gid) {
            return Err(SpkError::Security(i18n::owner_mismatch(
                lang,
                &save.logical,
                save.uid,
                save.gid,
                entry.uid,
                entry.gid,
            )));
        }
        if entry.mode & 0o7777 != save.mode & 0o7777 {
            return Err(SpkError::Security(i18n::mode_mismatch(
                lang,
                &save.logical,
                save.mode & 0o7777,
                entry.mode & 0o7777,
            )));
        }
    }
    Ok(())
}

fn kind_name(kind: OnDiskKind) -> &'static str {
    match kind {
        OnDiskKind::File => "file",
        OnDiskKind::Dir => "dir",
        OnDiskKind::Symlink => "symlink",
    }
}

fn cleanup_dir(path: &Path) {
    let _ = fs::remove_dir_all(path);
}
