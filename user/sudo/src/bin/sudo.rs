//
//       user/sudo/src/bin/sudo.rs
//       sudo command line
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::process::ExitCode;

use clap::Parser;
use sudo_core::app::{self, Options};
use sudo_core::config::{Doas, Sudoers};
use sudo_core::Paths;

#[derive(Parser)]
#[command(name = "sudo", version, about = "Execute a command as another user")]
struct Cli {
    #[arg(short = 'u', long = "user")]
    user: Option<String>,
    #[arg(short = 'g', long = "group")]
    group: Option<String>,
    #[arg(short = 'i', long)]
    login: bool,
    #[arg(short = 's', long)]
    shell: bool,
    #[arg(short = 'E', long)]
    preserve_env: bool,
    /// Invalidate the user's timestamp
    #[arg(short = 'k', long)]
    reset: bool,
    /// Remove all of the user's timestamps
    #[arg(short = 'K', long = "remove-timestamp")]
    reset_all: bool,
    /// Update the timestamp without running a command
    #[arg(short = 'v', long)]
    validate: bool,
    /// List allowed commands
    #[arg(short = 'l', long)]
    list: bool,
    /// Do not prompt for a password
    #[arg(short = 'n', long)]
    non_interactive: bool,
    /// Read the password from standard input
    #[arg(short = 'S', long)]
    stdin_password: bool,
    /// Use an external GUI askpass helper for password input
    #[arg(short = 'A', long = "askpass")]
    askpass: bool,
    /// Edit files instead of running a command
    #[arg(short = 'e', long)]
    edit: bool,
    /// Validate a sudoers file without running a command
    #[arg(long = "check-config", value_name = "PATH")]
    check_config: Option<std::path::PathBuf>,
    /// Validate a doas.conf file without running a command
    #[arg(long = "check-doas", value_name = "PATH")]
    check_doas: Option<std::path::PathBuf>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

fn options(cli: &Cli, doas: bool) -> Options {
    Options {
        target_user: cli.user.clone(),
        target_group: cli.group.clone(),
        login: cli.login,
        shell: cli.shell,
        preserve_env: cli.preserve_env,
        non_interactive: cli.non_interactive,
        stdin_password: cli.stdin_password,
        askpass: cli.askpass,
        doas,
        command: cli.command.clone(),
    }
}

fn check_config(path: &std::path::Path, doas: bool) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("sudo: cannot read {}: {error}", path.display());
            return ExitCode::from(1);
        }
    };
    let result = if doas {
        Doas::parse(&text).map(|_| ())
    } else {
        Sudoers::parse(&text).map(|_| ())
    };
    match result {
        Ok(()) => {
            println!("{}: OK", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("sudo: {}: {error}", path.display());
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(path) = &cli.check_config {
        return check_config(path, false);
    }
    if let Some(path) = &cli.check_doas {
        return check_config(path, true);
    }
    let paths = Paths::system();
    let uid = unsafe { libc::getuid() };

    if cli.reset_all {
        if let Err(error) = app::clear_all_tickets(&paths) {
            eprintln!("sudo: {error}");
            return ExitCode::from(1);
        }
        if cli.command.is_empty() {
            return ExitCode::SUCCESS;
        }
    }
    if cli.reset {
        if let Err(error) = app::clear_ticket(&paths, uid) {
            eprintln!("sudo: {error}");
            return ExitCode::from(1);
        }
        if cli.command.is_empty() {
            return ExitCode::SUCCESS;
        }
    }
    if cli.validate {
        return match app::validate(&paths, &options(&cli, false)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sudo: {error}");
                ExitCode::from(1)
            }
        };
    }
    if cli.list {
        return match app::list(&paths, &options(&cli, false)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("sudo: {error}");
                ExitCode::from(1)
            }
        };
    }
    let result = if cli.edit {
        app::edit(&paths, options(&cli, false))
    } else {
        app::execute(&paths, options(&cli, false))
    };
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("sudo: {error}");
            ExitCode::from(1)
        }
    }
}
