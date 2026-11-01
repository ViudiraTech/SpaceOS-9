/*
 *
 *       user/desktop/src/main.rs
 *       SPUI 4.0 multi-call entry point
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! One binary hosts the shell and every SPUI application. The first argument
//! selects the mode (`shell`, `term`, `files`, `edit`, `settings`, `login`,
//! `notify`); the shell launches new modes as separate X clients.

mod app;
mod apps;
mod draw;
mod icons;
mod shell;
mod text;
mod theme;
mod ui;
mod x11;
mod xprobe;

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    let mode = arguments.get(1).map(String::as_str).unwrap_or("shell");
    let result = match mode {
        "shell" => shell::Shell::run(),
        "term" => apps::term::run(arguments.get(2).map(String::as_str)),
        "files" => apps::files::run(arguments.get(2).map(String::as_str)),
        "edit" => apps::edit::run(arguments.get(2).map(String::as_str)),
        "settings" => apps::settings::run(),
        "login" => apps::login::run(),
        "notify" => apps::notify::run(
            arguments.get(2).map(String::as_str).unwrap_or("通知"),
            arguments.get(3).map(String::as_str).unwrap_or(""),
        ),
        "xprobe" => xprobe::run(arguments.get(2..).unwrap_or(&[])),
        "version" => {
            println!("SPUI 4.0 (SpaceOS-9)");
            Ok(())
        }
        other => {
            eprintln!("spui: unknown mode {other:?}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("spui: {error}");
            ExitCode::FAILURE
        }
    }
}
