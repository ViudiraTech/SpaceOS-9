//
//       user/editor/src/main.rs
//       Entry point and CLI flag parser for SpaceEditor
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use space_editor::app::{run_editor, EditorOptions, RibbonTab};
use std::path::PathBuf;
use std::process::ExitCode;

fn parse_args() -> EditorOptions {
    let mut opts = EditorOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => opts.demo = true,
            "--tab" => {
                if let Some(val) = args.next() {
                    opts.tab = RibbonTab::parse(&val);
                }
            }
            "--find" => {
                if let Some(val) = args.next() {
                    opts.find = Some(val);
                }
            }
            "--help" | "-h" => {
                println!("SpaceEditor — SpaceOS-9 WYSIWYG Document Editor");
                println!("Usage: space-editor [FILE] [--demo] [--tab home|insert|layout|view] [--find QUERY]");
                std::process::exit(0);
            }
            other if !other.starts_with('-') => {
                if opts.file.is_none() {
                    opts.file = Some(PathBuf::from(other));
                }
            }
            _ => {}
        }
    }
    opts
}

fn main() -> ExitCode {
    let opts = parse_args();
    match run_editor(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("space-editor: {err}");
            ExitCode::FAILURE
        }
    }
}
