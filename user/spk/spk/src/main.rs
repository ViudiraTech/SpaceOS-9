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
use spk_ops::ops;
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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install or upgrade packages
    Install {
        #[arg(long)]
        needed: bool,
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
        targets: Vec<String>,
    },
    /// Downgrade packages to name=<version>
    Downgrade {
        #[arg(long = "repo")]
        repos: Vec<String>,
        targets: Vec<String>,
    },
    /// Reinstall packages at their installed version
    Reinstall {
        #[arg(long = "repo")]
        repos: Vec<String>,
        targets: Vec<String>,
    },
    /// Remove packages (-Rs removes orphaned dependencies)
    Remove {
        #[arg(short = 'R', long)]
        recursive: bool,
        #[arg(short = 's', hide = true)]
        recursive_short: bool,
        #[arg(long)]
        purge: bool,
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
    /// Manage trusted repository keys
    Trust {
        #[command(subcommand)]
        command: TrustCommand,
    },
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
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            for line in error.lines() {
                eprintln!("{line}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}

fn run(cli: Cli) -> Result<(), SpkError> {
    let lang = Lang::detect(cli.lang.as_deref());
    let paths = Paths::new(&cli.root)?;
    match cli.command {
        Command::Install {
            needed,
            repos,
            targets,
        } => ops::install(&paths, &repos, &targets, needed, cli.noconfirm, lang),
        Command::Upgrade {
            repos,
            allow_partial,
            targets,
        } => ops::upgrade(&paths, &repos, &targets, allow_partial, cli.noconfirm, lang),
        Command::Downgrade { repos, targets } => {
            ops::downgrade(&paths, &repos, &targets, cli.noconfirm, lang)
        }
        Command::Reinstall { repos, targets } => {
            ops::reinstall(&paths, &repos, &targets, cli.noconfirm, lang)
        }
        Command::Remove {
            recursive,
            recursive_short,
            purge,
            targets,
        } => ops::remove(
            &paths,
            &targets,
            recursive || recursive_short,
            purge,
            cli.noconfirm,
            lang,
        ),
        Command::List => ops::list(&paths),
        Command::Info { package } => ops::info(&paths, &package, lang),
        Command::Files { package } => ops::files(&paths, &package, lang),
        Command::Owns { path } => ops::owns(&paths, &path, lang),
        Command::Check => ops::check_database(&paths, lang),
        Command::Repair => ops::repair_database(&paths, lang),
        Command::Trust { command } => match command {
            TrustCommand::Add { key } => ops::trust_add(&paths, &key, lang),
            TrustCommand::List => ops::trust_list(&paths),
        },
    }
}
