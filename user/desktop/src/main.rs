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
mod compositor;
mod ddm;
mod desktop_entries;
mod draw;
mod fonts;
mod icons;
mod ime;
mod netmgr;
mod shell;
mod sound;
mod start_screen;
mod text;
mod theme;
mod ui;
mod vortex;
mod x11;
mod xprobe;

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    let prog = arguments.first().map(String::as_str).unwrap_or("");
    let default_mode = if prog.ends_with("spimed") {
        "imed"
    } else {
        "shell"
    };
    let mode = arguments.get(1).map(String::as_str).unwrap_or(default_mode);
    if matches!(mode, "auth" | "askpass") {
        let helper = "/usr/bin/spui-askpass";
        if !std::path::Path::new(helper).is_file() {
            eprintln!("spui: {helper} 未安装，请先安装 spui-askpass 软件包");
            return ExitCode::FAILURE;
        }
        return match std::process::Command::new(helper)
            .args(arguments.get(2..).unwrap_or(&[]))
            .status()
        {
            Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
            Err(error) => {
                eprintln!("spui: 无法启动 {helper}: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let result = match mode {
        "shell" => shell::Shell::run(),
        "imed" => ime::run(),
        "term" => apps::term::run(arguments.get(2).map(String::as_str)),
        "files" => apps::files::run(arguments.get(2).map(String::as_str)),
        "edit" => apps::edit::run(arguments.get(2).map(String::as_str)),
        "settings" => apps::settings::run(arguments.get(2).map(String::as_str)),
        "calc" | "calculator" => apps::calc::run(),
        "mines" | "minesweeper" => apps::mines::run(),
        "monitor" | "taskmgr" => apps::monitor::run(),
        "music" | "player" => apps::music::run(),
        "view" | "image" => apps::view::run(arguments.get(2).map(String::as_str)),
        "help" => apps::help::run(),
        "login" => apps::login::run(),
        "greeter" => apps::greeter::run(arguments.get(2..).unwrap_or(&[])),
        "oobe" => apps::oobe::run(arguments.get(2..).unwrap_or(&[])),
        "power-splash" => apps::power_splash::run(arguments.get(2..).unwrap_or(&[])),
        "ddm" => ddm::run(arguments.get(2..).unwrap_or(&[])).map_err(|error| {
            Box::new(std::io::Error::new(std::io::ErrorKind::Other, error))
                as Box<dyn std::error::Error>
        }),
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
