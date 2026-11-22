/*
 *
 *       user/spimed/src/main.rs
 *       Standalone entry point for `spimed`, the SpaceOS-9 SPIME Chinese IME daemon.
 *
 *       2026/11/22 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

mod daemon;
mod popup;
mod xim;

use std::process::ExitCode;

fn main() -> ExitCode {
    match daemon::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("spimed: {error}");
            ExitCode::FAILURE
        }
    }
}
