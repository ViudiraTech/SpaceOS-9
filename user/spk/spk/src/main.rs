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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let lang = Lang::detect(cli.lang.as_deref());
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
    let lang = Lang::detect(cli.lang.as_deref());
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
            targets,
        } => ops::install(
            &paths,
            &repos,
            &targets,
            needed,
            override_hold,
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
        Command::Upgrade {
            repos,
            allow_partial,
            override_hold,
            print,
            targets,
        } => ops::upgrade(
            &paths,
            &repos,
            &targets,
            allow_partial,
            override_hold,
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
        Command::Downgrade {
            repos,
            override_hold,
            print,
            targets,
        } => ops::downgrade(
            &paths,
            &repos,
            &targets,
            override_hold,
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
        Command::Reinstall {
            repos,
            override_hold,
            print,
            targets,
        } => ops::reinstall(
            &paths,
            &repos,
            &targets,
            override_hold,
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
