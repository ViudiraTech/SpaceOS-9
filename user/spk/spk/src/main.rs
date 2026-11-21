//
//       user/spk/spk/src/main.rs
//       spk command line interface
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use spk_ops::error::SpkError;
use spk_ops::i18n::Lang;
use spk_ops::ops::{self, ReportOptions};
use spk_ops::state::Paths;

#[derive(Parser)]
#[command(name = "spk", version, about = "SpaceOS-9 native package manager")]
struct Cli {
    /// Install into this root (default: /)
    #[arg(long, global = true, default_value = "/")]
    root: PathBuf,
    /// Skip confirmation prompts
    #[arg(long, global = true)]
    noconfirm: bool,
    /// Output language: en or zh_CN
    #[arg(long, global = true)]
    lang: Option<String>,
    /// Machine-readable transaction plans and errors
    #[arg(long, global = true)]
    json: bool,
    /// Script failure policy: rollback (default) or keep (debug only)
    #[arg(long = "script-failure", global = true, default_value = "rollback")]
    script_failure: String,
    /// Run .INSTALL without the sandbox (debug only)
    #[arg(long = "no-sandbox-scripts", global = true)]
    no_sandbox_scripts: bool,
    /// Use cached indexes/packages without network access
    #[arg(long, global = true)]
    offline: bool,
    /// Accept an unsigned index (debug only)
    #[arg(long = "allow-unsigned", global = true)]
    allow_unsigned: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install or upgrade packages
    Install {
        #[arg(long)]
        needed: bool,
        #[arg(long)]
        override_hold: bool,
        #[arg(long)]
        print: bool,
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Overwrite matching paths (absolute glob; repeatable)
        #[arg(long, value_name = "GLOB")]
        overwrite: Vec<String>,
        targets: Vec<String>,
    },
    /// Upgrade all installed packages (or named ones with --allow-partial)
    Upgrade {
        #[arg(long = "repo")]
        repos: Vec<String>,
        #[arg(long)]
        allow_partial: bool,
        #[arg(long)]
        override_hold: bool,
        #[arg(long)]
        print: bool,
        /// Overwrite matching paths (absolute glob; repeatable)
        #[arg(long, value_name = "GLOB")]
        overwrite: Vec<String>,
        targets: Vec<String>,
    },
    /// Downgrade packages to name=<version>
    Downgrade {
        #[arg(long = "repo")]
        repos: Vec<String>,
        #[arg(long)]
        override_hold: bool,
        #[arg(long)]
        print: bool,
        /// Overwrite matching paths (absolute glob; repeatable)
        #[arg(long, value_name = "GLOB")]
        overwrite: Vec<String>,
        targets: Vec<String>,
    },
    /// Reinstall packages at their installed version
    Reinstall {
        #[arg(long = "repo")]
        repos: Vec<String>,
        #[arg(long)]
        override_hold: bool,
        #[arg(long)]
        print: bool,
        /// Overwrite matching paths (absolute glob; repeatable)
        #[arg(long, value_name = "GLOB")]
        overwrite: Vec<String>,
        targets: Vec<String>,
    },
    /// Hold packages at their installed versions
    Hold { targets: Vec<String> },
    /// Release held packages
    Unhold { targets: Vec<String> },
    /// Remove packages (-Rs removes orphaned dependencies)
    Remove {
        #[arg(short = 'R', long)]
        recursive: bool,
        #[arg(short = 's', hide = true)]
        recursive_short: bool,
        #[arg(long)]
        purge: bool,
        #[arg(long)]
        print: bool,
        targets: Vec<String>,
    },
    /// Show installed packages, holds, repositories and cache status
    Status,
    /// List installed packages
    List,
    /// Show package information
    Info { package: String },
    /// List files owned by a package
    Files { package: String },
    /// Show which package owns a path
    Owns { path: String },
    /// Check database and filesystem consistency
    Check,
    /// Recover an interrupted transaction
    Repair,
    /// Verify repository indexes and package files
    Verify {
        #[arg(long = "repo")]
        repos: Vec<String>,
    },
    /// Inspect or clean the package/repository cache
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Synchronize configured repository indexes
    #[command(alias = "update")]
    Sync {
        #[arg(long = "repo")]
        repos: Vec<String>,
    },
    /// Manage configured repositories
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Manage trusted repository keys
    Trust {
        #[command(subcommand)]
        command: TrustCommand,
    },
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Show cache sizes and entry counts
    Info,
    /// Remove unreferenced packages and old partial downloads
    Clean {
        #[arg(long, default_value_t = 30)]
        retention_days: u64,
        #[arg(long, default_value_t = 2 * 1024 * 1024 * 1024)]
        max_bytes: u64,
    },
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Add or update a repository URL
    Add {
        name: String,
        url: String,
        #[arg(long)]
        priority: Option<i32>,
    },
    /// Remove a repository
    Remove { name: String },
    /// List repositories
    List,
    /// Change a repository priority
    Priority { name: String, priority: i32 },
}

#[derive(Subcommand)]
enum TrustCommand {
    /// Trust an ed25519 public key
    Add { key: PathBuf },
    /// List trusted keys
    List,
}

const ZH_ROOT_HELP: &str = "\
SpaceOS-9 原生包管理器

用法: spk [选项] <命令>

命令:
  install       安装或升级软件包
  upgrade       升级所有软件包（命名升级需 --allow-partial）
  downgrade     降级到 name=<version>
  reinstall     重新安装当前版本
  hold          锁定软件包版本
  unhold        解除锁定
  remove        删除软件包（-Rs 删除孤儿依赖）
  status        显示已安装、锁定、仓库和缓存状态
  list          列出已安装软件包
  info          显示软件包信息
  files         列出软件包拥有的文件
  owns          显示路径属于哪个软件包
  check         检查数据库和文件系统一致性
  repair        恢复中断的事务
  verify        校验仓库索引和软件包文件
  cache         查看或清理软件包/仓库缓存
  sync          同步仓库索引（别名 update）
  repo          管理仓库
  trust         管理信任密钥

选项:
  --root <ROOT>                 安装根目录（默认 /）
  --noconfirm                   跳过确认
  --lang <LANG>                 输出语言：en 或 zh_CN
  --json                        输出 JSON（事务计划/错误）
  --script-failure <POLICY>     脚本失败策略：rollback 或 keep
  --no-sandbox-scripts          不使用沙箱运行 .INSTALL（仅调试）
  --offline                     只使用缓存，不访问网络
  --allow-unsigned              接受无签名索引（仅调试）
  -h, --help                    显示帮助
  -V, --version                 显示版本
";

fn zh_subcommand_help(topic: &str) -> Option<&'static str> {
    Some(match topic {
        "install" => {
            "\
安装或升级软件包

用法: spk [全局选项] install [选项] [目标...]

选项:
  --needed                 已安装的软件包不重复安装
  --override-hold          覆盖 hold 限制
  --print                  只打印事务计划
  --repo <URL|PATH>        指定仓库（可重复）
  --overwrite <GLOB>       覆盖匹配的已存在/已归属路径（可重复）
"
        }
        "upgrade" => {
            "\
升级所有已安装软件包

用法: spk [全局选项] upgrade [选项] [目标...]

选项:
  --allow-partial          允许命名升级
  --override-hold          覆盖 hold 限制
  --print                  只打印事务计划
  --repo <URL|PATH>        指定仓库（可重复）
  --overwrite <GLOB>       覆盖匹配的已存在/已归属路径（可重复）
"
        }
        "downgrade" => {
            "\
降级到 name=<version>

用法: spk [全局选项] downgrade [选项] 目标...

选项:
  --override-hold          覆盖 hold 限制
  --print                  只打印事务计划
  --repo <URL|PATH>        指定仓库（可重复）
  --overwrite <GLOB>       覆盖匹配的已存在/已归属路径（可重复）
"
        }
        "reinstall" => {
            "\
重新安装当前版本

用法: spk [全局选项] reinstall [选项] 目标...

选项:
  --override-hold          覆盖 hold 限制
  --print                  只打印事务计划
  --repo <URL|PATH>        指定仓库（可重复）
  --overwrite <GLOB>       覆盖匹配的已存在/已归属路径（可重复）
"
        }
        "remove" => {
            "\
删除软件包

用法: spk [全局选项] remove [选项] 目标...

选项:
  -R, --recursive          删除孤儿依赖
  --purge                  同时删除配置文件
  --print                  只打印事务计划
"
        }
        "hold" => "锁定软件包\n\n用法: spk [全局选项] hold <目标...>\n",
        "unhold" => "解除锁定\n\n用法: spk [全局选项] unhold <目标...>\n",
        "status" => "显示已安装、锁定、仓库和缓存状态\n\n用法: spk [全局选项] status\n",
        "list" => "列出已安装软件包\n\n用法: spk [全局选项] list\n",
        "info" => "显示软件包信息\n\n用法: spk [全局选项] info <软件包>\n",
        "files" => "列出软件包拥有的文件\n\n用法: spk [全局选项] files <软件包>\n",
        "owns" => "显示路径属于哪个软件包\n\n用法: spk [全局选项] owns <路径>\n",
        "check" => "检查数据库和文件系统一致性\n\n用法: spk [全局选项] check\n",
        "repair" => "恢复中断的事务\n\n用法: spk [全局选项] repair\n",
        "verify" => {
            "\
校验仓库索引和软件包文件

用法: spk [全局选项] verify [--repo <URL|PATH>]
"
        }
        "sync" | "update" => {
            "\
同步仓库索引

用法: spk [全局选项] sync [--repo <URL|PATH>]
"
        }
        "cache" => {
            "\
查看或清理软件包/仓库缓存

用法: spk [全局选项] cache <info|clean> [选项]

子命令:
  info                     显示缓存大小和条目数
  clean [--retention-days N] [--max-bytes N]
                           清理未引用的软件包和旧的部分下载
"
        }
        "cache info" => "显示缓存大小和条目数\n\n用法: spk [全局选项] cache info\n",
        "cache clean" => {
            "\
清理未引用的软件包和旧的部分下载

用法: spk [全局选项] cache clean [--retention-days N] [--max-bytes N]

选项:
  --retention-days <N>     保留天数（默认 30）
  --max-bytes <N>          缓存上限（默认 2 GiB）
"
        }
        "repo" => {
            "\
管理仓库

用法: spk [全局选项] repo <add|remove|list|priority> ...

子命令:
  add <名称> <URL> [--priority N]  添加或更新仓库
  remove <名称>                    删除仓库
  list                             列出仓库
  priority <名称> <N>              修改优先级
"
        }
        "repo add" => {
            "添加或更新仓库\n\n用法: spk [全局选项] repo add <名称> <URL> [--priority N]\n"
        }
        "repo remove" => "删除仓库\n\n用法: spk [全局选项] repo remove <名称>\n",
        "repo list" => "列出仓库\n\n用法: spk [全局选项] repo list\n",
        "repo priority" => "修改仓库优先级\n\n用法: spk [全局选项] repo priority <名称> <N>\n",
        "trust" => {
            "\
管理信任密钥

用法: spk [全局选项] trust <add|list> ...

子命令:
  add <公钥文件>           信任 ed25519 公钥
  list                     列出信任密钥
"
        }
        "trust add" => "信任 ed25519 公钥\n\n用法: spk [全局选项] trust add <公钥文件>\n",
        "trust list" => "列出信任密钥\n\n用法: spk [全局选项] trust list\n",
        _ => return None,
    })
}

fn detect_lang_from_args(arguments: &[String]) -> Lang {
    let mut index = 1usize;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--lang" {
            if let Some(value) = arguments.get(index + 1) {
                return Lang::detect(Some(value));
            }
        } else if let Some(value) = argument.strip_prefix("--lang=") {
            return Lang::detect(Some(value));
        }
        index += 1;
    }
    Lang::detect(None)
}

fn requested_help(arguments: &[String]) -> Option<String> {
    let mut help = false;
    let mut positional = Vec::new();
    let mut index = 1usize;
    while index < arguments.len() {
        let argument = &arguments[index];
        match argument.as_str() {
            "-h" | "--help" => help = true,
            "--root" | "--lang" | "--script-failure" => {
                index += 1;
            }
            value if value.starts_with('-') => {}
            "help" => help = true,
            value => positional.push(value.to_string()),
        }
        index += 1;
    }
    if !help {
        return None;
    }
    let topic = match positional.as_slice() {
        [] => return Some(ZH_ROOT_HELP.to_string()),
        [command] => command.clone(),
        [group, command, ..] if matches!(group.as_str(), "cache" | "repo" | "trust") => {
            format!("{group} {command}")
        }
        [command, ..] => command.clone(),
    };
    if topic == "help" || topic.is_empty() {
        return Some(ZH_ROOT_HELP.to_string());
    }
    zh_subcommand_help(&topic).map(|text| {
        format!("{text}\n全局选项请运行 `spk --help`；所有命令都接受 --root/--lang/--json。\n")
    })
}

fn has_command_argument(arguments: &[String]) -> bool {
    let mut index = 1usize;
    while index < arguments.len() {
        let argument = &arguments[index];
        match argument.as_str() {
            "--root" | "--lang" | "--script-failure" => index += 2,
            value if value.starts_with('-') => index += 1,
            _ => return true,
        }
    }
    false
}

fn localize_clap_error(lang: Lang, error: clap::Error) -> ! {
    if !lang.is_zh() {
        error.exit();
    }
    let text = error
        .to_string()
        .replace("error: ", "错误：")
        .replace("Usage:", "用法：")
        .replace(
            "For more information, try '--help'.",
            "更多信息请运行 'spk --help'。",
        )
        .replace("tip: a similar subcommand exists:", "提示：相近的子命令：")
        .replace("tip: a similar argument exists:", "提示：相近的参数：")
        .replace("unrecognized subcommand", "无法识别的子命令")
        .replace("unexpected argument", "意外参数")
        .replace("invalid value", "无效值")
        .replace("possible values", "可选值")
        .replace(
            "requires a subcommand but one was not provided",
            "需要子命令，但未提供",
        )
        .replace("[OPTIONS]", "[选项]")
        .replace("<COMMAND>", "<命令>")
        .replace("To pass", "要传入")
        .replace("as a value, use", "作为值，请使用");
    eprint!("{text}");
    std::process::exit(error.exit_code());
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    let lang_hint = detect_lang_from_args(&arguments);
    if lang_hint.is_zh() {
        if arguments
            .iter()
            .any(|value| value == "--version" || value == "-V")
        {
            println!("spk 0.1.0");
            return ExitCode::SUCCESS;
        }
        if let Some(help) = requested_help(&arguments) {
            print!("{help}");
            return ExitCode::SUCCESS;
        }
        if !has_command_argument(&arguments) {
            eprintln!("错误：需要子命令\n\n{ZH_ROOT_HELP}");
            return ExitCode::from(2);
        }
    }
    let cli = match Cli::try_parse_from(&arguments) {
        Ok(cli) => cli,
        Err(error) => localize_clap_error(lang_hint, error),
    };
    let json = cli.json;
    let lang = Lang::detect_for(&cli.root, cli.lang.as_deref());
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if json {
                let payload = serde_json::json!({
                    "schema": 1,
                    "error": error.to_string(),
                    "lines": error.lines(),
                });
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            } else {
                eprintln!("{}{error}", spk_ops::i18n::error_label(lang));
                for line in error.lines() {
                    eprintln!("{line}");
                }
            }
            ExitCode::from(error.exit_code())
        }
    }
}

fn run(cli: Cli) -> Result<(), SpkError> {
    let lang = Lang::detect_for(&cli.root, cli.lang.as_deref());
    let script_failure_keep = match cli.script_failure.as_str() {
        "rollback" => false,
        "keep" => true,
        other => {
            return Err(SpkError::Usage(format!(
                "invalid --script-failure '{other}'; use rollback or keep"
            )))
        }
    };
    let paths = Paths::new(&cli.root)?;
    match cli.command {
        Command::Install {
            needed,
            override_hold,
            print,
            repos,
            overwrite,
            targets,
        } => ops::install(
            &paths,
            &repos,
            &targets,
            needed,
            override_hold,
            cli.noconfirm,
            &overwrite,
            ReportOptions {
                json: cli.json,
                print,
                script_failure_keep,
                no_sandbox_scripts: cli.no_sandbox_scripts,
                offline: cli.offline,
                allow_unsigned: cli.allow_unsigned,
            },
            lang,
        ),
        Command::Upgrade {
            repos,
            allow_partial,
            override_hold,
            print,
            overwrite,
            targets,
        } => ops::upgrade(
            &paths,
            &repos,
            &targets,
            allow_partial,
            override_hold,
            cli.noconfirm,
            &overwrite,
            ReportOptions {
                json: cli.json,
                print,
                script_failure_keep,
                no_sandbox_scripts: cli.no_sandbox_scripts,
                offline: cli.offline,
                allow_unsigned: cli.allow_unsigned,
            },
            lang,
        ),
        Command::Downgrade {
            repos,
            override_hold,
            print,
            overwrite,
            targets,
        } => ops::downgrade(
            &paths,
            &repos,
            &targets,
            override_hold,
            cli.noconfirm,
            &overwrite,
            ReportOptions {
                json: cli.json,
                print,
                script_failure_keep,
                no_sandbox_scripts: cli.no_sandbox_scripts,
                offline: cli.offline,
                allow_unsigned: cli.allow_unsigned,
            },
            lang,
        ),
        Command::Reinstall {
            repos,
            override_hold,
            print,
            overwrite,
            targets,
        } => ops::reinstall(
            &paths,
            &repos,
            &targets,
            override_hold,
            cli.noconfirm,
            &overwrite,
            ReportOptions {
                json: cli.json,
                print,
                script_failure_keep,
                no_sandbox_scripts: cli.no_sandbox_scripts,
                offline: cli.offline,
                allow_unsigned: cli.allow_unsigned,
            },
            lang,
        ),
        Command::Cache { command } => match command {
            CacheCommand::Info => ops::cache_info(&paths, cli.json, lang),
            CacheCommand::Clean {
                retention_days,
                max_bytes,
            } => ops::cache_clean(&paths, retention_days, max_bytes, cli.json, lang),
        },
        Command::Verify { repos } => ops::verify(
            &paths,
            &repos,
            ReportOptions {
                json: cli.json,
                print: false,
                script_failure_keep,
                no_sandbox_scripts: cli.no_sandbox_scripts,
                offline: cli.offline,
                allow_unsigned: cli.allow_unsigned,
            },
            lang,
        ),
        Command::Sync { repos } => ops::sync(
            &paths,
            &repos,
            ReportOptions {
                json: cli.json,
                print: false,
                script_failure_keep,
                no_sandbox_scripts: cli.no_sandbox_scripts,
                offline: cli.offline,
                allow_unsigned: cli.allow_unsigned,
            },
            lang,
        ),
        Command::Hold { targets } => ops::hold(&paths, &targets, lang),
        Command::Unhold { targets } => ops::unhold(&paths, &targets, lang),
        Command::Remove {
            recursive,
            recursive_short,
            purge,
            print,
            targets,
        } => ops::remove(
            &paths,
            &targets,
            recursive || recursive_short,
            purge,
            cli.noconfirm,
            ReportOptions {
                json: cli.json,
                print,
                script_failure_keep,
                no_sandbox_scripts: cli.no_sandbox_scripts,
                offline: cli.offline,
                allow_unsigned: cli.allow_unsigned,
            },
            lang,
        ),
        Command::Status => ops::status(&paths, cli.json, lang),
        Command::List => ops::list(&paths),
        Command::Info { package } => ops::info(&paths, &package, lang),
        Command::Files { package } => ops::files(&paths, &package, lang),
        Command::Owns { path } => ops::owns(&paths, &path, lang),
        Command::Check => ops::check_database(&paths, lang),
        Command::Repair => ops::repair_database(&paths, lang),
        Command::Repo { command } => match command {
            RepoCommand::Add {
                name,
                url,
                priority,
            } => spk_ops::repo::add(&paths, &name, &url, priority),
            RepoCommand::Remove { name } => spk_ops::repo::remove(&paths, &name),
            RepoCommand::List => spk_ops::repo::list(&paths),
            RepoCommand::Priority { name, priority } => {
                spk_ops::repo::set_priority(&paths, &name, priority)
            }
        },
        Command::Trust { command } => match command {
            TrustCommand::Add { key } => ops::trust_add(&paths, &key, lang),
            TrustCommand::List => ops::trust_list(&paths),
        },
    }
}
