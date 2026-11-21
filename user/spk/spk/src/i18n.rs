//
//       user/spk/spk/src/i18n.rs
//       English / pacman zh_CN message catalog
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    ZhCn,
}

impl Lang {
    pub fn detect(explicit: Option<&str>) -> Self {
        Self::detect_for(std::path::Path::new("/"), explicit)
    }

    /// Resolve the output language from, in order: `--lang`, the environment,
    /// then the target root's `/etc/locale.conf`.  The config fallback matters
    /// under privilege tools such as doas, which scrub the environment.
    pub fn detect_for(root: &std::path::Path, explicit: Option<&str>) -> Self {
        if let Some(value) = explicit {
            return Self::from_value(value);
        }
        for variable in ["SPK_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(variable) {
                if !value.is_empty() {
                    return Self::from_value(&value);
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(root.join("etc/locale.conf")) {
            for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
                if let Some(value) = locale_value(&text, key) {
                    return Self::from_value(&value);
                }
            }
        }
        Self::En
    }

    fn from_value(value: &str) -> Self {
        if value.trim().starts_with("zh") {
            Self::ZhCn
        } else {
            Self::En
        }
    }

    pub fn is_zh(self) -> bool {
        self == Self::ZhCn
    }
}

fn locale_value(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub fn tr<'a>(lang: Lang, en: &'a str, zh: &'a str) -> &'a str {
    if lang.is_zh() {
        zh
    } else {
        en
    }
}

pub fn resolving(lang: Lang) -> &'static str {
    tr(lang, "resolving dependencies...", "正在解析依赖关系...")
}

pub fn looking_for_conflicts(lang: Lang) -> &'static str {
    tr(
        lang,
        "looking for conflicting packages...",
        "正在查找软件包冲突...",
    )
}

pub fn packages(lang: Lang, count: usize) -> String {
    if lang.is_zh() {
        format!("软件包 ({count})")
    } else {
        format!("Packages ({count})")
    }
}

pub fn total_download(lang: Lang) -> &'static str {
    tr(lang, "Total Download Size:", "全部下载大小：")
}

pub fn total_installed(lang: Lang) -> &'static str {
    tr(lang, "Total Installed Size:", "全部安装大小：")
}

pub fn net_upgrade(lang: Lang) -> &'static str {
    tr(lang, "Net Upgrade Size:", "净更新大小：")
}

pub fn total_removed(lang: Lang) -> &'static str {
    tr(lang, "Total Removed Size:", "全部删除大小：")
}

pub fn proceed_install(lang: Lang) -> &'static str {
    tr(
        lang,
        ":: Proceed with installation? [Y/n] ",
        ":: 进行安装吗？[Y/n] ",
    )
}

pub fn proceed_remove(lang: Lang) -> &'static str {
    tr(
        lang,
        ":: Do you want to remove these packages? [Y/n] ",
        ":: 要删除这些软件包吗？[Y/n] ",
    )
}

pub fn checking_keys(lang: Lang) -> &'static str {
    tr(lang, "checking keys in keyring", "正在检查密钥环中的密钥")
}

pub fn checking_integrity(lang: Lang) -> &'static str {
    tr(lang, "checking package integrity", "正在检查软件包完整性")
}

pub fn loading_packages(lang: Lang) -> &'static str {
    tr(lang, "loading package files", "正在加载软件包文件")
}

pub fn checking_file_conflicts(lang: Lang) -> &'static str {
    tr(lang, "checking for file conflicts", "正在检查文件冲突")
}

pub fn checking_disk(lang: Lang) -> &'static str {
    tr(
        lang,
        "checking available disk space",
        "正在检查可用磁盘空间",
    )
}

pub fn processing(lang: Lang) -> &'static str {
    tr(
        lang,
        ":: Processing package changes...",
        ":: 正在处理软件包的变化...",
    )
}

pub fn starting_full_upgrade(lang: Lang) -> &'static str {
    tr(
        lang,
        ":: Starting full system upgrade...",
        ":: 正在进行全面系统更新...",
    )
}

pub fn nothing_to_do(lang: Lang) -> &'static str {
    if lang.is_zh() {
        " 今日无事可做"
    } else {
        " there is nothing to do"
    }
}

pub fn installing(lang: Lang) -> &'static str {
    tr(lang, "Installing", "正在安装")
}

pub fn upgrading(lang: Lang) -> &'static str {
    tr(lang, "Upgrading", "正在升级")
}

pub fn downgrading(lang: Lang) -> &'static str {
    tr(lang, "Downgrading", "正在降级")
}

pub fn reinstalling(lang: Lang) -> &'static str {
    tr(lang, "Reinstalling", "正在重新安装")
}

pub fn removing(lang: Lang) -> &'static str {
    tr(lang, "Removing", "正在删除")
}

pub fn checking_dependencies(lang: Lang) -> &'static str {
    tr(lang, "checking dependencies...", "正在检查依赖关系...")
}

pub fn ok_summary(lang: Lang, size: &str, count: usize) -> String {
    if lang.is_zh() {
        format!("OK: {size}，共 {count} 个软件包")
    } else {
        format!(
            "OK: {size} in {count} package{}",
            if count == 1 { "" } else { "s" }
        )
    }
}

pub fn name_label(lang: Lang) -> &'static str {
    tr(lang, "Name", "名称")
}

pub fn version_label(lang: Lang) -> &'static str {
    tr(lang, "Version", "版本")
}

pub fn arch_label(lang: Lang) -> &'static str {
    tr(lang, "Architecture", "架构")
}

pub fn depends_label(lang: Lang) -> &'static str {
    tr(lang, "Depends On", "依赖")
}

pub fn provides_label(lang: Lang) -> &'static str {
    tr(lang, "Provides", "提供")
}

pub fn installed_size_label(lang: Lang) -> &'static str {
    tr(lang, "Installed Size", "安装大小")
}

pub fn install_date_label(lang: Lang) -> &'static str {
    tr(lang, "Install Date", "安装日期")
}

pub fn install_reason_label(lang: Lang) -> &'static str {
    tr(lang, "Install Reason", "安装原因")
}

pub fn explicit_reason(lang: Lang) -> &'static str {
    tr(lang, "Explicitly installed", "显式安装")
}

pub fn dependency_reason(lang: Lang) -> &'static str {
    tr(lang, "Installed as a dependency", "作为依赖安装")
}

pub fn none(lang: Lang) -> &'static str {
    tr(lang, "None", "无")
}

pub fn database_ok(lang: Lang) -> &'static str {
    tr(lang, "database is consistent", "数据库一致")
}

pub fn database_failed(lang: Lang) -> &'static str {
    tr(lang, "database check failed", "数据库检查失败")
}

pub fn no_owner(lang: Lang, path: &str) -> String {
    if lang.is_zh() {
        format!("没有软件包拥有 {path}")
    } else {
        format!("no package owns {path}")
    }
}

pub fn not_installed(lang: Lang, package: &str) -> String {
    if lang.is_zh() {
        format!("软件包 '{package}' 尚未安装")
    } else {
        format!("package '{package}' is not installed")
    }
}

pub fn already_installed(lang: Lang, package: &str) -> String {
    if lang.is_zh() {
        format!("软件包 '{package}' 已经安装")
    } else {
        format!("package '{package}' is already installed")
    }
}

pub fn unknown_target(lang: Lang, package: &str) -> String {
    if lang.is_zh() {
        format!("找不到目标：{package}")
    } else {
        format!("target not found: {package}")
    }
}

pub fn partial_disabled(lang: Lang) -> &'static str {
    tr(
        lang,
        "partial upgrades are disabled; run 'spk upgrade' or pass --allow-partial",
        "默认禁止部分升级；请运行 'spk upgrade' 或传入 --allow-partial",
    )
}

pub fn repair_start(lang: Lang, txid: &str) -> String {
    if lang.is_zh() {
        format!(":: 正在恢复事务 {txid}")
    } else {
        format!(":: Recovering transaction {txid}")
    }
}

pub fn repair_rolled_back(lang: Lang) -> &'static str {
    tr(lang, ":: Transaction rolled back", ":: 事务已回滚")
}

pub fn nothing_to_repair(lang: Lang) -> &'static str {
    tr(lang, "nothing to repair", "无需修复")
}

pub fn repair_metadata(lang: Lang, count: usize) -> String {
    if lang.is_zh() {
        format!(":: 已修正 {count} 个文件的属主/权限")
    } else {
        format!(":: fixed ownership/mode on {count} files")
    }
}

pub fn restoring(lang: Lang) -> &'static str {
    tr(lang, "restoring", "正在恢复")
}

pub fn removing_dir(lang: Lang) -> &'static str {
    tr(lang, "removing directory", "正在删除目录")
}

pub fn unfinished_transaction(lang: Lang, txid: &str) -> String {
    if lang.is_zh() {
        format!("存在未完成的事务 {txid}；请先运行 'spk repair'")
    } else {
        format!("unfinished transaction {txid}; run 'spk repair' first")
    }
}

pub fn solver_header_dependencies(lang: Lang) -> &'static str {
    tr(
        lang,
        "failed to prepare transaction (could not satisfy dependencies)",
        "无法准备事务（无法满足依赖关系）",
    )
}

pub fn solver_header_conflicts(lang: Lang) -> &'static str {
    tr(
        lang,
        "failed to prepare transaction (conflicting dependencies)",
        "无法准备事务（依赖关系冲突）",
    )
}

pub fn solver_missing(lang: Lang, dep: &str, dependent: &str) -> String {
    if lang.is_zh() {
        format!(":: 无法满足由 {dependent} 要求的依赖 '{dep}'")
    } else {
        format!(":: unable to satisfy dependency '{dep}' required by {dependent}")
    }
}

pub fn solver_version_conflict(
    lang: Lang,
    candidate: &str,
    version: &str,
    dep: &str,
    dependent: &str,
) -> String {
    if lang.is_zh() {
        format!(":: 安装 {candidate} ({version}) 会破坏由 {dependent} 要求的依赖 '{dep}'")
    } else {
        format!(
            ":: installing {candidate} ({version}) breaks dependency '{dep}' required by {dependent}"
        )
    }
}

pub fn solver_conflict(lang: Lang, a: &str, av: &str, b: &str, bv: &str) -> String {
    if lang.is_zh() {
        format!(":: 安装 {a} ({av}) 与 {b} ({bv}) 冲突")
    } else {
        format!(":: installing {a} ({av}) conflicts with {b} ({bv})")
    }
}

pub fn solver_budget(lang: Lang) -> &'static str {
    tr(
        lang,
        "dependency solver exhausted its search budget",
        "依赖求解器搜索预算耗尽",
    )
}

pub fn target_requires(lang: Lang, target: &str, dependency: &str) -> String {
    if lang.is_zh() {
        format!(":: {target} 需要 {dependency}")
    } else {
        format!(":: {target} requires {dependency}")
    }
}

pub fn file_conflict_header(lang: Lang) -> &'static str {
    tr(
        lang,
        "failed to prepare transaction (conflicting files)",
        "无法准备事务（文件冲突）",
    )
}

pub fn file_conflict(lang: Lang, path: &str, package: &str, owner: &str) -> String {
    if lang.is_zh() {
        format!(":: {path} 已存在于 {owner} 中（尝试安装 {package}）")
    } else {
        format!(":: {path} exists in {owner} (trying to install {package})")
    }
}

pub fn file_conflict_unowned(lang: Lang, path: &str, package: &str) -> String {
    if lang.is_zh() {
        format!(":: {path} 已存在于文件系统中（尝试安装 {package}）")
    } else {
        format!(":: {path} exists in the filesystem (trying to install {package})")
    }
}

pub fn warning_conffile(lang: Lang, path: &str, saved: &str) -> String {
    if lang.is_zh() {
        format!("警告：{path} 已安装为 {saved}")
    } else {
        format!("warning: {path} installed as {saved}")
    }
}

pub fn running_script(lang: Lang, function: &str, package: &str) -> String {
    if lang.is_zh() {
        format!(":: 正在为 {package} 运行 {function} 脚本...")
    } else {
        format!(":: Running {function} script for {package}...")
    }
}

pub fn script_failed(lang: Lang, function: &str, code: i32) -> String {
    if lang.is_zh() {
        format!("{function} 脚本失败（退出码 {code}）")
    } else {
        format!("{function} script failed (exit code {code})")
    }
}

pub fn script_timeout(lang: Lang, function: &str) -> String {
    if lang.is_zh() {
        format!("{function} 脚本在 60 秒后超时")
    } else {
        format!("{function} script timed out after 60 seconds")
    }
}

pub fn script_failure_kept(lang: Lang, function: &str, error: &str) -> String {
    if lang.is_zh() {
        format!("警告：{function} 脚本失败但已按 --script-failure=keep 继续：{error}")
    } else {
        format!("warning: {function} script failed but --script-failure=keep continues: {error}")
    }
}

pub fn running_pre_hooks(lang: Lang) -> &'static str {
    tr(
        lang,
        ":: Running pre-transaction hooks...",
        ":: 正在运行事务前钩子...",
    )
}

pub fn running_post_hooks(lang: Lang) -> &'static str {
    tr(
        lang,
        ":: Running post-transaction hooks...",
        ":: 正在运行事务后钩子...",
    )
}

pub fn hook_progress(_lang: Lang, index: usize, total: usize, description: &str) -> String {
    format!("({index}/{total}) {description}")
}

pub fn trigger_executing(lang: Lang, package: &str, version: &str) -> String {
    if lang.is_zh() {
        format!("正在执行 {package}-{version}.trigger")
    } else {
        format!("Executing {package}-{version}.trigger")
    }
}

pub fn trigger_failed(lang: Lang, package: &str, error: &str) -> String {
    if lang.is_zh() {
        format!("警告：{package} trigger 执行失败：{error}")
    } else {
        format!("warning: {package} trigger failed: {error}")
    }
}

pub fn hook_failed(lang: Lang, description: &str, error: &str) -> String {
    if lang.is_zh() {
        format!("警告：钩子“{description}”失败：{error}")
    } else {
        format!("warning: hook '{description}' failed: {error}")
    }
}

pub fn staging_undeclared(lang: Lang, path: &str) -> String {
    if lang.is_zh() {
        format!("脚本新增了未声明的 staging 路径 {path}")
    } else {
        format!("scriptlet added undeclared staging path {path}")
    }
}

pub fn removing_breaks(lang: Lang, removed: &str, dep: &str, dependent: &str) -> String {
    if lang.is_zh() {
        format!(":: 删除 {removed} 会破坏由 {dependent} 要求的依赖 '{dep}'")
    } else {
        format!(":: removing {removed} breaks dependency '{dep}' required by {dependent}")
    }
}

pub fn transaction_cancelled(lang: Lang) -> &'static str {
    tr(lang, "transaction cancelled", "事务已取消")
}

pub fn trust_added(lang: Lang, key_id: &str) -> String {
    if lang.is_zh() {
        format!(":: 已信任密钥 {key_id}")
    } else {
        format!(":: Trusted key {key_id}")
    }
}

pub fn solver_held(lang: Lang, name: &str, held: &str, attempted: &str) -> String {
    if lang.is_zh() {
        format!(":: 软件包 {name} 被锁定在 {held}；拒绝使用 {attempted}")
    } else {
        format!(":: package {name} is held at {held}; refusing {attempted}")
    }
}

pub fn held_back(lang: Lang, name: &str, installed: &str, available: &str) -> String {
    if lang.is_zh() {
        format!("警告：{name} 被锁定在 {installed}（可升级到 {available}）")
    } else {
        format!("warning: {name} is held back ({installed} -> {available})")
    }
}

pub fn error_label(lang: Lang) -> &'static str {
    tr(lang, "error: ", "错误：")
}

pub fn repo_mirror_unavailable(lang: Lang, name: &str, url: &str, error: &str) -> String {
    if lang.is_zh() {
        format!("警告：仓库“{name}”镜像 {url} 不可用：{error}")
    } else {
        format!("warning: repository '{name}' mirror {url} unavailable: {error}")
    }
}

pub fn repo_unavailable(lang: Lang, url: &str, error: &str) -> String {
    if lang.is_zh() {
        format!("警告：仓库 {url} 不可用：{error}")
    } else {
        format!("warning: repository {url} unavailable: {error}")
    }
}

pub fn repo_no_mirror(lang: Lang, name: &str) -> String {
    if lang.is_zh() {
        format!("警告：仓库“{name}”没有可用镜像")
    } else {
        format!("warning: repository '{name}' has no usable mirror")
    }
}

pub fn not_held(lang: Lang, name: &str) -> String {
    if lang.is_zh() {
        format!("警告：{name} 未被锁定")
    } else {
        format!("warning: {name} is not held")
    }
}

pub fn setuid_preserved(lang: Lang, package: &str) -> String {
    if lang.is_zh() {
        format!("警告：保留来自签名软件包 {package} 的 setuid/setgid 位")
    } else {
        format!("warning: preserving setuid/setgid bits from signed package {package}")
    }
}

pub fn root_required(lang: Lang) -> &'static str {
    if lang.is_zh() {
        "spk 必须以 root 运行（setuid/系统文件写入需要 euid 0）"
    } else {
        "spk must be run as root (setuid and system writes require euid 0)"
    }
}

pub fn setuid_not_root(lang: Lang, path: &str, uid: u32) -> String {
    if lang.is_zh() {
        format!("{path} 声明了 setuid/setgid，但安装后属主是 uid {uid}，不是 root 0")
    } else {
        format!("{path} declares setuid/setgid but is owned by uid {uid}, not root 0")
    }
}

pub fn owner_mismatch(
    lang: Lang,
    path: &str,
    want_uid: u32,
    want_gid: u32,
    got_uid: u32,
    got_gid: u32,
) -> String {
    if lang.is_zh() {
        format!("{path} 属主应为 {want_uid}:{want_gid}，实际为 {got_uid}:{got_gid}")
    } else {
        format!("{path} should be owned by {want_uid}:{want_gid}, got {got_uid}:{got_gid}")
    }
}

pub fn mode_mismatch(lang: Lang, path: &str, want: u32, got: u32) -> String {
    if lang.is_zh() {
        format!("{path} 权限应为 {want:04o}，实际为 {got:04o}")
    } else {
        format!("{path} should be mode {want:04o}, got {got:04o}")
    }
}

pub fn staging_removed(lang: Lang, path: &str) -> String {
    if lang.is_zh() {
        format!("脚本删除了已声明的 staging 路径 {path}")
    } else {
        format!("scriptlet removed declared staging path {path}")
    }
}

pub fn staging_type_changed(lang: Lang, path: &str) -> String {
    if lang.is_zh() {
        format!("脚本改变了已声明 staging 路径 {path} 的类型")
    } else {
        format!("scriptlet changed the type of declared staging path {path}")
    }
}

pub fn sandbox_disabled(lang: Lang, package: &str) -> String {
    if lang.is_zh() {
        format!("警告：--no-sandbox-scripts 已启用，{package} 的安装脚本不受沙箱保护")
    } else {
        format!("warning: --no-sandbox-scripts is active; {package} install script is unsandboxed")
    }
}

pub fn staging_symlink_changed(lang: Lang, path: &str) -> String {
    if lang.is_zh() {
        format!("脚本改变了已声明符号链接 {path} 的目标")
    } else {
        format!("scriptlet changed the target of declared symlink {path}")
    }
}

pub fn repo_using_cache(lang: Lang, name: &str, error: &str) -> String {
    if lang.is_zh() {
        format!("警告：仓库“{name}”同步失败，使用本地缓存：{error}")
    } else {
        format!("warning: repository '{name}' sync failed; using cached index: {error}")
    }
}

pub fn repo_synced(lang: Lang, name: &str, packages: usize) -> String {
    if lang.is_zh() {
        format!(":: 仓库 {name} 已同步（{packages} 个软件包）")
    } else {
        format!(":: repository {name} synchronized ({packages} packages)")
    }
}

pub fn verifying_repo(lang: Lang, name: &str) -> String {
    if lang.is_zh() {
        format!("正在校验仓库 {name}")
    } else {
        format!("Verifying repository {name}")
    }
}

pub fn no_sync_repositories(lang: Lang) -> &'static str {
    tr(
        lang,
        "no usable repositories to synchronize",
        "没有可用于同步的仓库",
    )
}

pub fn no_verify_repositories(lang: Lang) -> &'static str {
    tr(
        lang,
        "no usable repositories to verify",
        "没有可用于校验的仓库",
    )
}

pub fn offline_not_cached(lang: Lang, name: &str) -> String {
    if lang.is_zh() {
        format!("仓库 {name} 未缓存，且指定了 --offline")
    } else {
        format!("repository {name} is not cached and --offline was given")
    }
}

pub fn no_usable_mirror(lang: Lang, name: &str) -> String {
    if lang.is_zh() {
        format!("仓库 {name} 没有可用镜像")
    } else {
        format!("repository {name} has no usable mirror")
    }
}

pub fn verify_size_mismatch(lang: Lang, name: &str, indexed: u64, actual: u64) -> String {
    if lang.is_zh() {
        format!("{name}: 大小不匹配：索引 {indexed}，文件 {actual}")
    } else {
        format!("{name}: size mismatch: index {indexed}, file {actual}")
    }
}

pub fn verify_hash_mismatch(lang: Lang, name: &str) -> String {
    if lang.is_zh() {
        format!("{name}: sha256 不匹配")
    } else {
        format!("{name}: sha256 mismatch")
    }
}

pub fn verify_ok(lang: Lang) -> String {
    if lang.is_zh() {
        "仓库校验通过".to_string()
    } else {
        "repository verification passed".to_string()
    }
}

pub fn overwrite_unowned_prompt(lang: Lang, path: &str) -> String {
    if lang.is_zh() {
        format!("{path} 已存在，但不属于任何已安装软件包。是否覆盖？[y/N] ")
    } else {
        format!("{path} exists but is not owned by any installed package. Overwrite? [y/N] ")
    }
}

pub fn overwrite_owned_prompt(lang: Lang, path: &str, owner: &str) -> String {
    if lang.is_zh() {
        format!("{path} 由 {owner} 拥有。是否覆盖并转移归属？[y/N] ")
    } else {
        format!("{path} is owned by {owner}. Overwrite and transfer ownership? [y/N] ")
    }
}

pub fn overwrite_unowned_detail(lang: Lang, path: &str, package: &str) -> String {
    if lang.is_zh() {
        format!(
            "{path} 已存在，但不属于任何已安装软件包；使用 spk install --overwrite '{path}' {package} 覆盖，或先删除该文件；spk owns {path} 可查看归属。"
        )
    } else {
        format!(
            "{path} exists but is not owned by any installed package; use spk install --overwrite '{path}' {package}, or remove the file first; spk owns {path} shows ownership."
        )
    }
}

pub fn overwrite_owned_detail(lang: Lang, path: &str, package: &str, owner: &str) -> String {
    if lang.is_zh() {
        format!(
            "{path} 已由 {owner} 拥有；使用 spk install --overwrite '{path}' {package}，或让包声明 replaces={owner}。"
        )
    } else {
        format!(
            "{path} is owned by {owner}; use spk install --overwrite '{path}' {package}, or declare replaces={owner}."
        )
    }
}

pub fn overwrite_unowned_warning(lang: Lang, path: &str) -> String {
    if lang.is_zh() {
        format!("警告：覆盖了不属于任何软件包的 {path}")
    } else {
        format!("warning: overwrote unowned path {path}")
    }
}

pub fn overwrite_takeover_warning(lang: Lang, path: &str, package: &str, owner: &str) -> String {
    if lang.is_zh() {
        format!("警告：{package} 接管了 {path}（原属 {owner}）")
    } else {
        format!("warning: {package} took over {path} from {owner}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_conf_values_are_parsed() {
        let text = "# comment\nLANG=zh_CN.UTF-8\nLC_MESSAGES=en_US.UTF-8\n";
        assert_eq!(locale_value(text, "LANG").as_deref(), Some("zh_CN.UTF-8"));
        assert_eq!(
            locale_value(text, "LC_MESSAGES").as_deref(),
            Some("en_US.UTF-8")
        );
        assert_eq!(locale_value(text, "LC_ALL"), None);
    }

    #[test]
    fn language_values_use_prefix_fallback() {
        assert_eq!(Lang::from_value("zh_CN.UTF-8"), Lang::ZhCn);
        assert_eq!(Lang::from_value("zh_TW"), Lang::ZhCn);
        assert_eq!(Lang::from_value("en_US.UTF-8"), Lang::En);
        assert_eq!(Lang::from_value("C.UTF-8"), Lang::En);
    }
}
