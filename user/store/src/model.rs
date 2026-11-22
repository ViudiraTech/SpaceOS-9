use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CatalogJson {
    #[serde(default)]
    pub repositories: Vec<RepoStatusEntry>,
    #[serde(default)]
    pub packages: Vec<StorePackage>,
    #[serde(default)]
    pub updates: Vec<String>,
    /// Merchandising document published by the repository (`store.json`).
    #[serde(default)]
    pub store: Option<StoreDoc>,
}

/// Mirrors `spk_core::store::StoreDoc`; the store talks to `spk` over JSON, so
/// it deliberately keeps its own copy of the wire format.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct StoreDoc {
    #[serde(default)]
    pub packages: BTreeMap<String, StoreMeta>,
    #[serde(default)]
    pub hero: Vec<StoreHeroJson>,
    #[serde(default)]
    pub sections: Vec<StoreSectionJson>,
    #[serde(default)]
    pub categories: Vec<StoreCategoryJson>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StoreMeta {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub highlights: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub accent: Vec<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub screenshots: Vec<String>,
    #[serde(default)]
    pub featured_rank: Option<i32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StoreHeroJson {
    #[serde(default)]
    pub overline: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub cta: String,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub go_updates: bool,
    #[serde(default)]
    pub accent: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StoreSectionJson {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StoreCategoryJson {
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub subtitle: String,
}

impl CatalogJson {
    pub fn meta_for(&self, package: &str) -> Option<&StoreMeta> {
        self.store.as_ref()?.packages.get(package)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RepoStatusEntry {
    pub name: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub last_sync: u64,
    #[serde(default)]
    pub key_id: String,
    #[serde(default)]
    pub synced: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub package_count: usize,
    #[serde(default)]
    pub generated_at: u64,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StorePackage {
    pub name: String,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub available_version: Option<String>,
    #[serde(default)]
    pub upgrade_available: bool,
    #[serde(default)]
    pub held: bool,
    #[serde(default)]
    pub install_reason: Option<String>,
    #[serde(default)]
    pub install_date: Option<u64>,
    #[serde(default)]
    pub installed_size: Option<u64>,
    #[serde(default)]
    pub file_count: Option<usize>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub abi: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    #[serde(default)]
    pub summary: String,
}

impl StorePackage {
    pub fn display_version(&self) -> &str {
        self.available_version
            .as_deref()
            .or(self.installed_version.as_deref())
            .unwrap_or("-")
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PackageFilesJson {
    #[allow(dead_code)]
    #[serde(default)]
    pub package: String,
    #[serde(default)]
    pub files: Vec<PackageFileEntry>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PackageFileEntry {
    pub path: String,
    #[serde(rename = "type", default)]
    pub file_type: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct InfoPackageJson {
    pub package: StorePackage,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CacheInfoJson {
    #[serde(default)]
    pub repository_bytes: u64,
    #[serde(default)]
    pub package_bytes: u64,
    #[serde(default)]
    pub part_bytes: u64,
    #[serde(default)]
    pub package_count: usize,
    #[serde(default)]
    pub part_count: usize,
    #[serde(default)]
    pub snapshot_count: usize,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TrustListJson {
    #[serde(default)]
    pub keys: Vec<TrustKeyEntry>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TrustKeyEntry {
    pub key_id: String,
    #[serde(default)]
    pub comment: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TxPlanJson {
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub nothing_to_do: bool,
    #[serde(default)]
    pub total_download_bytes: u64,
    #[serde(default)]
    pub packages: Vec<TxPlanAction>,
    #[serde(default)]
    pub removals: Vec<TxPlanRemoval>,
    #[serde(default)]
    pub held_back: Vec<TxHeldBack>,
    #[serde(default)]
    pub final_package_count: usize,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TxPlanAction {
    pub name: String,
    #[serde(default)]
    pub from: Option<String>,
    pub to: String,
    pub action: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TxPlanRemoval {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TxHeldBack {
    pub name: String,
    pub installed: String,
    pub available: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SpkErrorJson {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CheckOkJson {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub packages: usize,
    #[serde(default)]
    pub files: usize,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct VerifyJson {
    #[serde(default)]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub packages: Vec<VerifyPkgJson>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct VerifyPkgJson {
    #[serde(default)]
    pub repository: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub ok: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum ProgressStreamEvent {
    Phase {
        phase: String,
        message: String,
    },
    Progress {
        kind: String,
        label: String,
        done: u64,
        #[serde(default)]
        total: Option<u64>,
        percent: usize,
        #[serde(default)]
        rate_bps: u64,
    },
}

#[derive(Clone, Debug, Default)]
pub struct FullSnapshot {
    pub catalog: CatalogJson,
    pub cache: CacheInfoJson,
    pub trust_keys: Vec<TrustKeyEntry>,
    pub offline: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavSection {
    Discover,
    Browse,
    Updates,
    Installed,
    Maintenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreCategory {
    All,
    Themes,
    System,
    Utilities,
}

impl StoreCategory {
    pub const ALL: [StoreCategory; 4] = [
        Self::All,
        Self::Themes,
        Self::System,
        Self::Utilities,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "全部软件",
            Self::Themes => "桌面主题",
            Self::System => "系统核心",
            Self::Utilities => "实用工具",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Themes => "主题",
            Self::System => "系统",
            Self::Utilities => "工具",
        }
    }

    pub fn from_slug(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "themes" | "theme" => Self::Themes,
            "system" | "core" => Self::System,
            "utilities" | "tools" | "apps" => Self::Utilities,
            _ => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstalledFilter {
    All,
    Explicit,
    Dependency,
    Held,
}

impl InstalledFilter {
    pub const ALL: [InstalledFilter; 4] = [
        Self::All,
        Self::Explicit,
        Self::Dependency,
        Self::Held,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "全部已装",
            Self::Explicit => "手动安装",
            Self::Dependency => "自动依赖",
            Self::Held => "已锁定",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    Featured,
    Name,
    SizeDesc,
    InstalledFirst,
}

impl SortMode {
    pub const ALL: [SortMode; 4] = [
        Self::Featured,
        Self::Name,
        Self::SizeDesc,
        Self::InstalledFirst,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Featured => "推荐排序",
            Self::Name => "按名称",
            Self::SizeDesc => "按体积",
            Self::InstalledFirst => "已装优先",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Files,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueTab {
    Jobs,
    Console,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxJobKind {
    Sync,
    Install(Vec<String>),
    UpgradeAll,
    UpgradeOne(String),
    Reinstall(String),
    Remove {
        name: String,
        recursive: bool,
        purge: bool,
    },
    Hold(String),
    Unhold(String),
    CacheClean,
    RepoAdd {
        name: String,
        url: String,
        priority: Option<i32>,
    },
    RepoRemove(String),
    RepoPriority {
        name: String,
        priority: i32,
    },
    TrustAdd(String),
    Repair,
}

impl TxJobKind {
    pub fn primary_package(&self) -> Option<&str> {
        match self {
            Self::Install(list) if list.len() == 1 => list.first().map(String::as_str),
            Self::UpgradeOne(n)
            | Self::Reinstall(n)
            | Self::Hold(n)
            | Self::Unhold(n) => Some(n.as_str()),
            Self::Remove { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    pub fn needs_preview(&self) -> bool {
        matches!(
            self,
            Self::Install(_)
                | Self::UpgradeAll
                | Self::UpgradeOne(_)
                | Self::Reinstall(_)
                | Self::Remove { .. }
        )
    }

    pub fn command_summary(&self) -> String {
        match self {
            Self::Sync => "spk sync".into(),
            Self::Install(targets) => format!("spk install {}", targets.join(" ")),
            Self::UpgradeAll => "spk upgrade".into(),
            Self::UpgradeOne(name) => format!("spk upgrade --allow-partial {name}"),
            Self::Reinstall(name) => format!("spk reinstall {name}"),
            Self::Remove {
                name,
                recursive,
                purge,
            } => {
                let mut cmd = String::from("spk remove");
                if *recursive {
                    cmd.push_str(" -Rs");
                }
                if *purge {
                    cmd.push_str(" --purge");
                }
                cmd.push(' ');
                cmd.push_str(name);
                cmd
            }
            Self::Hold(name) => format!("spk hold {name}"),
            Self::Unhold(name) => format!("spk unhold {name}"),
            Self::CacheClean => "spk cache clean".into(),
            Self::RepoAdd { name, url, priority } => match priority {
                Some(p) => format!("spk repo add {name} {url} --priority {p}"),
                None => format!("spk repo add {name} {url}"),
            },
            Self::RepoRemove(name) => format!("spk repo remove {name}"),
            Self::RepoPriority { name, priority } => {
                format!("spk repo priority {name} {priority}")
            }
            Self::TrustAdd(path) => format!("spk trust add {path}"),
            Self::Repair => "spk repair".into(),
        }
    }

    pub fn to_spk_args(&self, for_preview: bool) -> Vec<String> {
        let mut args = Vec::new();
        if for_preview {
            args.push("--offline".into());
            args.push("--json".into());
        } else {
            args.push("--noconfirm".into());
            args.push("--progress-json".into());
            if matches!(
                self,
                Self::CacheClean
                    | Self::RepoAdd { .. }
                    | Self::RepoRemove(_)
                    | Self::RepoPriority { .. }
                    | Self::TrustAdd(_)
                    | Self::Hold(_)
                    | Self::Unhold(_)
            ) {
                args.push("--json".into());
            }
        }
        match self {
            Self::Sync => {
                args.push("sync".into());
            }
            Self::Install(targets) => {
                args.push("install".into());
                if for_preview {
                    args.push("--print".into());
                }
                args.extend(targets.iter().cloned());
            }
            Self::UpgradeAll => {
                args.push("upgrade".into());
                if for_preview {
                    args.push("--print".into());
                }
            }
            Self::UpgradeOne(name) => {
                args.push("upgrade".into());
                args.push("--allow-partial".into());
                if for_preview {
                    args.push("--print".into());
                }
                args.push(name.clone());
            }
            Self::Reinstall(name) => {
                args.push("reinstall".into());
                if for_preview {
                    args.push("--print".into());
                }
                args.push(name.clone());
            }
            Self::Remove {
                name,
                recursive,
                purge,
            } => {
                args.push("remove".into());
                if *recursive {
                    args.push("-Rs".into());
                }
                if *purge {
                    args.push("--purge".into());
                }
                if for_preview {
                    args.push("--print".into());
                }
                args.push(name.clone());
            }
            Self::Hold(name) => {
                args.push("hold".into());
                args.push(name.clone());
            }
            Self::Unhold(name) => {
                args.push("unhold".into());
                args.push(name.clone());
            }
            Self::CacheClean => {
                args.push("cache".into());
                args.push("clean".into());
            }
            Self::RepoAdd {
                name,
                url,
                priority,
            } => {
                args.push("repo".into());
                args.push("add".into());
                args.push(name.clone());
                args.push(url.clone());
                if let Some(p) = priority {
                    args.push("--priority".into());
                    args.push(p.to_string());
                }
            }
            Self::RepoRemove(name) => {
                args.push("repo".into());
                args.push("remove".into());
                args.push(name.clone());
            }
            Self::RepoPriority { name, priority } => {
                args.push("repo".into());
                args.push("priority".into());
                args.push(name.clone());
                args.push(priority.to_string());
            }
            Self::TrustAdd(path) => {
                args.push("trust".into());
                args.push("add".into());
                args.push(path.clone());
            }
            Self::Repair => {
                args.push("repair".into());
            }
        }
        args
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxJobStatus {
    Queued,
    WaitingAuth,
    Running,
    Succeeded,
    Failed(String),
    Cancelled,
}

impl TxJobStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::WaitingAuth | Self::Running)
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Queued => "排队中",
            Self::WaitingAuth => "等待授权",
            Self::Running => "执行中",
            Self::Succeeded => "已完成",
            Self::Failed(_) => "失败",
            Self::Cancelled => "已取消",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Phase,
    Warn,
    Error,
    Success,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub text: String,
    pub level: LogLevel,
}

#[derive(Clone, Debug)]
pub struct TxJob {
    pub id: u64,
    pub kind: TxJobKind,
    pub title: String,
    pub status: TxJobStatus,
    pub phase: String,
    pub phase_msg: String,
    pub current_label: String,
    pub percent: usize,
    pub done_bytes: u64,
    pub total_bytes: Option<u64>,
    pub rate_bps: u64,
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut val = bytes as f64;
    let mut idx = 0usize;
    while val >= 1024.0 && idx + 1 < UNITS.len() {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes} B")
    } else {
        format!("{val:.1} {}", UNITS[idx])
    }
}

#[allow(deprecated)]
pub fn format_timestamp(ts: u64) -> String {
    if ts == 0 {
        return "未同步".into();
    }
    let secs = ts as i64;
    let secs = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes to a valid stack `tm`.
    let ptr = unsafe { libc::localtime_r(&secs, &mut tm) };
    if ptr.is_null() {
        return format!("{ts}");
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintResult {
    pub ok: bool,
    pub packages: usize,
}

impl Serialize for MaintResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("MaintResult", 2)?;
        s.serialize_field("ok", &self.ok)?;
        s.serialize_field("packages", &self.packages)?;
        s.end()
    }
}
