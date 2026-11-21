//
//       user/spk/spk/src/bin/spk_p5_test.rs
//       P5 init: verify the embedded repository and run offline transactions
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::io::{self, Write};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

const SPK: &str = "/usr/bin/spk";
const REPO: &str = "/var/lib/spk/repo/x86_64";

fn main() {
    let mode = fs::read_to_string("/proc/cmdline")
        .unwrap_or_default()
        .split_ascii_whitespace()
        .find_map(|word| word.strip_prefix("p5.mode="))
        .unwrap_or("install")
        .to_string();
    let result = if mode == "verify" {
        verify_only()
    } else {
        install_matrix()
    };
    match result {
        Ok(()) if mode == "verify" => println!("SPK_P5_VERIFY_OK"),
        Ok(()) => println!("SPK_P5_OK"),
        Err(error) => println!("SPK_P5_FAIL: {error}"),
    }
    io::stdout().flush().ok();
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn verify_only() -> Result<(), String> {
    let output = spk(&["verify", "--repo", REPO])?;
    expect_ok("verify", &output)?;
    Ok(())
}

fn install_matrix() -> Result<(), String> {
    let output = spk(&["verify", "--repo", REPO])?;
    expect_ok("verify", &output)?;
    expect_ok("check before install", &spk(&["check"])?)?;

    let output = spk(&["install", "--noconfirm", "hello=0.1.0-1"])?;
    expect_ok("install hello 0.1", &output)?;
    if !fs::metadata("/usr/bin/hello")
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return Err("hello is missing after install".into());
    }

    let output = spk(&["install", "--noconfirm", "hello=0.2.0-1"])?;
    expect_ok("upgrade hello 0.2", &output)?;
    let version = fs::read_to_string("/usr/share/hello/version")
        .map_err(|error| format!("hello version file: {error}"))?;
    if version.trim() != "0.2.0" {
        return Err(format!("unexpected hello version {version:?}"));
    }

    let output = spk(&["remove", "--noconfirm", "hello"])?;
    expect_ok("remove hello", &output)?;
    if fs::metadata("/usr/bin/hello").is_ok() {
        return Err("hello survived remove".into());
    }
    expect_ok("check after remove", &spk(&["check"])?)?;
    Ok(())
}

fn spk(arguments: &[&str]) -> Result<Output, String> {
    Command::new(SPK)
        .arg("--lang")
        .arg("en")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("run spk {arguments:?}: {error}"))
}

fn expect_ok(label: &str, output: &Output) -> Result<(), String> {
    if output.status.success() {
        println!("SPK_P5_STEP ok: {label}");
        Ok(())
    } else {
        Err(format!(
            "{label}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}
