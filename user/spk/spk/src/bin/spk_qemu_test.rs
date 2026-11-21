//
//       user/spk/spk/src/bin/spk_qemu_test.rs
//       PID 1 test driver for the QEMU install/upgrade/recovery matrix
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

const PHASE_FILE: &str = "/spk-test-phase";
const SPK: &str = "/usr/bin/spk";

fn main() {
    match std::panic::catch_unwind(run) {
        Ok(Ok(())) => {}
        Ok(Err(message)) => fail(&message),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|text| text.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            fail(&format!("panic: {message}"));
        }
    }
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn run() -> Result<(), String> {
    let phase = fs::read_to_string(PHASE_FILE)
        .unwrap_or_else(|_| "0".into())
        .trim()
        .to_string();
    match phase.as_str() {
        "0" => phase_install(),
        "1" => phase_upgrade_and_repair(),
        "2" => phase_conflicts_and_cleanup(),
        _ => {
            println!("SPK_QEMU_OK");
            io::stdout().flush().ok();
            Ok(())
        }
    }
}

fn phase_install() -> Result<(), String> {
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "hello=0.1.0-1",
    ])?;
    expect_ok("install hello 0.1", &output)?;
    expect_contains("install hello 0.1", &output, "正在安装 hello")?;
    print_output("install hello 0.1", &output);

    let output = spk(&["list"])?;
    expect_ok("list", &output)?;
    expect_contains("list", &output, "hello 0.1.0-1")?;

    let output = spk_zh(&["info", "hello"])?;
    expect_ok("info hello", &output)?;
    expect_contains("info hello", &output, "0.1.0-1")?;

    let output = spk(&["files", "hello"])?;
    expect_ok("files hello", &output)?;
    expect_contains("files hello", &output, "/usr/bin/hello")?;

    let output = spk(&["owns", "/usr/bin/hello"])?;
    expect_ok("owns", &output)?;
    expect_contains("owns", &output, "hello")?;

    expect_ok("check after install", &spk(&["check"])?)?;
    let hello = Command::new("/usr/bin/hello")
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run hello: {error}"))?;
    expect_ok("run hello", &hello)?;
    expect_contains("run hello", &hello, "hello from splibc")?;

    let output = spk_zh(&[
        "install",
        "--needed",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "hello=0.1.0-1",
    ])?;
    expect_ok("install --needed", &output)?;
    expect_contains("install --needed", &output, "今日无事可做")?;

    set_phase("1")?;
    finish_phase("SPK_QEMU_REBOOT 1");
    Ok(())
}

fn phase_upgrade_and_repair() -> Result<(), String> {
    expect_ok("check after reboot", &spk(&["check"])?)?;
    let list = spk(&["list"])?;
    expect_contains("list after reboot", &list, "hello 0.1.0-1")?;

    let output = spk_zh(&["upgrade", "--repo", "/repo/update", "--noconfirm"])?;
    expect_ok("upgrade", &output)?;
    expect_contains("upgrade", &output, "正在升级 hello")?;
    print_output("upgrade to 0.2", &output);
    let hello = Command::new("/usr/bin/hello")
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run hello v2: {error}"))?;
    expect_contains("run hello v2", &hello, "hello v2 from splibc")?;
    expect_ok("version file", &spk(&["check"])?)?;
    let version =
        fs::read_to_string("/usr/share/hello/version").map_err(|error| error.to_string())?;
    if version.trim() != "0.2.0" {
        return Err(format!("unexpected version file '{version}'"));
    }

    let output = spk_zh(&[
        "downgrade",
        "--repo",
        "/repo/update",
        "--noconfirm",
        "hello=0.1.0-1",
    ])?;
    expect_ok("downgrade", &output)?;
    expect_contains("downgrade", &output, "正在降级 hello")?;
    print_output("downgrade to 0.1", &output);
    expect_ok("check after downgrade", &spk(&["check"])?)?;
    if fs::metadata("/usr/share/hello/version").is_ok() {
        return Err("downgrade left /usr/share/hello/version behind".into());
    }

    let crash = spk_env(
        &["upgrade", "--repo", "/repo/update", "--noconfirm"],
        &[("SPK_TEST_CRASH", "after-first-write")],
    )?;
    if crash.status.signal() != Some(9) {
        return Err(format!(
            "upgrade crash injection did not SIGKILL: {:?}\n{}",
            crash.status,
            text(&crash)
        ));
    }
    if fs::metadata("/var/lib/spk/journal").is_err() {
        return Err("crash left no journal".into());
    }
    let repair = spk_zh(&["repair", "--noconfirm"])?;
    expect_ok("repair", &repair)?;
    expect_contains("repair", &repair, "事务已回滚")?;
    print_output("repair", &repair);
    expect_ok("check after repair", &spk(&["check"])?)?;
    let list = spk(&["list"])?;
    expect_contains("list after repair", &list, "hello 0.1.0-1")?;
    let hello = Command::new("/usr/bin/hello")
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run hello after repair: {error}"))?;
    expect_contains("run hello after repair", &hello, "hello from splibc")?;
    if fs::metadata("/var/lib/spk/journal").is_ok() {
        return Err("repair left the journal behind".into());
    }

    let output = spk_zh(&["upgrade", "--repo", "/repo/update", "--noconfirm"])?;
    expect_ok("upgrade after repair", &output)?;
    expect_contains("upgrade after repair", &output, "正在升级 hello")?;
    let hello = Command::new("/usr/bin/hello")
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run hello v2 again: {error}"))?;
    expect_contains("run hello v2 again", &hello, "hello v2 from splibc")?;
    expect_ok("check after retry", &spk(&["check"])?)?;

    set_phase("2")?;
    finish_phase("SPK_QEMU_REBOOT 2");
    Ok(())
}

fn phase_conflicts_and_cleanup() -> Result<(), String> {
    expect_ok("check after reboot", &spk(&["check"])?)?;
    let list = spk(&["list"])?;
    expect_contains("list after reboot", &list, "hello 0.2.0-1")?;

    let output = spk_zh(&["remove", "--noconfirm", "hello"])?;
    expect_ok("remove hello", &output)?;
    expect_contains("remove hello", &output, "正在删除 hello")?;
    print_output("remove hello", &output);
    if fs::metadata("/usr/bin/hello").is_ok() {
        return Err("remove left /usr/bin/hello behind".into());
    }
    if !text(&spk(&["list"])?).trim().is_empty() {
        return Err("installed DB is not empty after remove".into());
    }
    expect_ok("check after remove", &spk(&["check"])?)?;

    let output = spk_zh(&["install", "--repo", "/repo/base", "--noconfirm", "dep-a"])?;
    if output.status.code() != Some(4) {
        return Err(format!(
            "dependency conflict did not exit 4: {:?}\n{}",
            output.status,
            text(&output)
        ));
    }
    expect_contains("dep conflict", &output, "dep-b<2")?;
    expect_contains("dep conflict", &output, "dep-a")?;
    print_output("dependency conflict", &output);
    if !text(&spk(&["list"])?).trim().is_empty() {
        return Err("failed dependency transaction changed the DB".into());
    }

    expect_ok(
        "install file-c",
        &spk_zh(&["install", "--repo", "/repo/base", "--noconfirm", "file-c"])?,
    )?;
    let output = spk_zh(&["install", "--repo", "/repo/base", "--noconfirm", "file-d"])?;
    if output.status.code() != Some(5) {
        return Err(format!(
            "file conflict did not exit 5: {:?}\n{}",
            output.status,
            text(&output)
        ));
    }
    expect_contains("file conflict", &output, "文件冲突")?;
    print_output("file conflict", &output);
    let shared =
        fs::read_to_string("/usr/share/spk-tests/shared").map_err(|error| error.to_string())?;
    if shared.trim() != "from-c" {
        return Err(format!("file-c content changed to '{shared}'"));
    }
    expect_ok("check after file conflict", &spk(&["check"])?)?;
    expect_ok("remove file-c", &spk(&["remove", "--noconfirm", "file-c"])?)?;

    expect_ok(
        "install app",
        &spk_zh(&["install", "--repo", "/repo/base", "--noconfirm", "app"])?,
    )?;
    let list = text(&spk(&["list"])?);
    if !list.contains("app") || !list.contains("lib") {
        return Err(format!("app did not pull lib:\n{list}"));
    }
    let output = spk_zh(&["remove", "-Rs", "--noconfirm", "app"])?;
    expect_ok("remove -Rs", &output)?;
    expect_contains("remove -Rs", &output, "正在删除 app")?;
    print_output("remove -Rs", &output);
    if !text(&spk(&["list"])?).trim().is_empty() {
        return Err("-Rs left packages installed".into());
    }
    expect_ok("check after -Rs", &spk(&["check"])?)?;

    set_phase("3")?;
    // Earlier phase transitions deliberately leave the journal for the next
    // boot. The final state is checkpointed so the host-side offline checks
    // can read it without mounting or replaying the image.
    rustix::fs::sync();
    finish_phase("SPK_QEMU_OK");
    Ok(())
}

fn spk(args: &[&str]) -> Result<Output, String> {
    spk_env(args, &[])
}

fn spk_zh(args: &[&str]) -> Result<Output, String> {
    let mut full = vec!["--lang", "zh_CN"];
    full.extend_from_slice(args);
    spk(&full)
}

fn spk_env(args: &[&str], envs: &[(&str, &str)]) -> Result<Output, String> {
    let mut command = Command::new(SPK);
    command.args(args).stdin(Stdio::inherit());
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .map_err(|error| format!("cannot run spk {}: {error}", args.join(" ")))
}

fn expect_ok(label: &str, output: &Output) -> Result<(), String> {
    if output.status.success() {
        println!("SPK_QEMU_STEP ok: {label}");
        io::stdout().flush().ok();
        Ok(())
    } else {
        Err(format!(
            "{label} failed with {:?}\n{}",
            output.status,
            text(output)
        ))
    }
}

fn expect_contains(label: &str, output: &Output, needle: &str) -> Result<(), String> {
    let body = text(output);
    if body.contains(needle) {
        Ok(())
    } else {
        Err(format!(
            "{label}: output does not contain '{needle}'\n{body}"
        ))
    }
}

fn print_output(label: &str, output: &Output) {
    println!("--- {label} ---");
    print!("{}", String::from_utf8_lossy(&output.stdout));
    print!("{}", String::from_utf8_lossy(&output.stderr));
    println!("--- end {label} ---");
    io::stdout().flush().ok();
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Leave the ext4 journal as-is and let the next boot replay it. This is the
/// regression for the directory-block revoke fix; the spk crash test itself is
/// the explicit SIGKILL in phase 2.
fn finish_phase(marker: &str) {
    println!("{marker}");
    io::stdout().flush().ok();
}

fn set_phase(phase: &str) -> Result<(), String> {
    let mut file = fs::File::create(PHASE_FILE).map_err(|error| error.to_string())?;
    file.write_all(format!("{phase}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    fs::File::open("/")
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn fail(message: &str) {
    println!("SPK_QEMU_FAIL {message}");
    io::stdout().flush().ok();
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
