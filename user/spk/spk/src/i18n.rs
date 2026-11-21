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
        if let Some(value) = explicit {
            return if value.starts_with("zh") {
                Self::ZhCn
            } else {
                Self::En
            };
        }
        for variable in ["SPK_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(variable) {
                if value.starts_with("zh") {
                    return Self::ZhCn;
                }
                if !value.is_empty() {
                    return Self::En;
                }
            }
        }
        Self::En
    }

    pub fn is_zh(self) -> bool {
        self == Self::ZhCn
    }
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
        format!("OK: {size} in {count} 个软件包")
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

pub fn warning_script_unsupported(lang: Lang, package: &str) -> String {
    if lang.is_zh() {
        format!("警告：{package} 包含安装脚本；P3 之前不会执行")
    } else {
        format!("warning: {package} contains install scripts; not running them before P3")
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
