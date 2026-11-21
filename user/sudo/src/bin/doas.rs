//
//       user/sudo/src/bin/doas.rs
//       doas command line
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::process::ExitCode;

use clap::Parser;
use sudo_core::app::{self, Options};
use sudo_core::Paths;

#[derive(Parser)]
#[command(name = "doas", version, about = "Execute a command as another user")]
struct Cli {
    #[arg(short = 'u', long = "user")]
    user: Option<String>,
    /// Do not prompt for a password
    #[arg(short = 'n', long)]
    non_interactive: bool,
    /// Run a shell
    #[arg(short = 's', long)]
    shell: bool,
    #[arg(short = 'S', long)]
    stdin_password: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let paths = Paths::system();
    let options = Options {
        target_user: cli.user,
        target_group: None,
        login: false,
        shell: cli.shell,
        preserve_env: false,
        non_interactive: cli.non_interactive,
        stdin_password: cli.stdin_password,
        doas: true,
        command: cli.command,
    };
    match app::execute(&paths, options) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("doas: {error}");
            ExitCode::from(1)
        }
    }
}
