//
//       user/store/src/main.rs
//       Space 商店 entry point
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::process::ExitCode;

use space_store::store::{parse_args, run_store};

const HELP: &str = "\
Space 商店 — SpaceOS-9 图形软件中心

用法: space-store [选项]

  --updates          直接打开「软件更新」
  --maintenance      直接打开「仓库与维护」
  --installed        直接打开「已安装」
  --browse           直接打开「分类浏览」
  --package <名称>   打开指定软件包的详情页
  --category <类别>  按类别浏览（themes / system / utilities）
  --search <关键词>  以搜索模式启动
  -h, --help         显示本帮助
  -V, --version      显示版本
";

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)) {
        Ok(options) => match run_store(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Space 商店无法启动：{error}");
                ExitCode::FAILURE
            }
        },
        Err(message) if message == "help" => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Err(message) if message == "version" => {
            println!("space-store {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}
