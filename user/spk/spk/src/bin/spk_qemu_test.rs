//
//       user/spk/spk/src/bin/spk_qemu_test.rs
//       PID 1 test driver for the QEMU install/upgrade/recovery matrix
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{chown, MetadataExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
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
    // Kernel setuid/setgid exec: the probe is root-owned mode 6755, and the
    // child starts as uid/gid 1000.
    let probe = Command::new("/usr/bin/setuid-probe")
        .uid(1000)
        .gid(1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run setuid-probe: {error}"))?;
    expect_ok("setuid probe", &probe)?;
    expect_contains("setuid probe", &probe, "UID=1000")?;
    expect_contains("setuid probe", &probe, "EUID=0")?;
    expect_contains("setuid probe", &probe, "EGID=0")?;
    print_output("setuid probe", &probe);

    let plain = Command::new("/usr/bin/nosetuid-probe")
        .uid(1000)
        .gid(1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run nosetuid-probe: {error}"))?;
    expect_ok("nosuid probe", &plain)?;
    expect_contains("nosuid probe", &plain, "EUID=1000")?;
    expect_contains("nosuid probe", &plain, "EGID=1000")?;

    let nonroot = spk_as(
        1000,
        &[
            "install",
            "--repo",
            "/repo/base",
            "--noconfirm",
            "hello=0.1.0-1",
        ],
    )?;
    if nonroot.status.success() {
        return Err("install as uid 1000 unexpectedly succeeded".into());
    }
    expect_contains("non-root install", &nonroot, "root")?;
    print_output("non-root install", &nonroot);

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

    let hold = spk_zh(&["hold", "hello"])?;
    expect_ok("hold hello", &hold)?;
    expect_contains("hold hello", &hold, "0.1.0-1")?;
    let hold_list = spk(&["hold"])?;
    expect_contains("hold list", &hold_list, "hello 0.1.0-1")?;
    expect_ok("check after hold", &spk(&["check"])?)?;

    // sudo is installed by spk as a setuid package; the whitelist lives in
    // /etc/spk/setuid.allow on the rootfs.
    let output = spk_zh(&["install", "--repo", "/repo/base", "--noconfirm", "sudo"])?;
    expect_ok("install sudo", &output)?;
    expect_contains("install sudo", &output, "正在安装 sudo")?;
    let metadata =
        fs::metadata("/usr/bin/sudo").map_err(|error| format!("sudo is missing: {error}"))?;
    if metadata.uid() != 0 || metadata.gid() != 0 {
        return Err(format!(
            "/usr/bin/sudo owner is {}:{}, not 0:0",
            metadata.uid(),
            metadata.gid()
        ));
    }
    let mode = metadata.permissions().mode();
    if mode & 0o4000 == 0 {
        return Err(format!("/usr/bin/sudo is not setuid (mode {mode:04o})"));
    }

    // Break ownership, then prove check catches it and repair restores it.
    chown("/usr/bin/sudo", Some(1000), Some(1000))
        .map_err(|error| format!("cannot chown test file: {error}"))?;
    let broken = spk(&["check"])?;
    if broken.status.success() {
        return Err("check missed the wrong sudo owner".into());
    }
    expect_contains("check owner", &broken, "owner")?;
    print_output("check owner", &broken);
    expect_ok("repair owner", &spk(&["repair"])?)?;
    expect_ok("check after repair", &spk(&["check"])?)?;
    let metadata = fs::metadata("/usr/bin/sudo")
        .map_err(|error| format!("sudo is missing after repair: {error}"))?;
    if metadata.uid() != 0 || metadata.gid() != 0 || metadata.permissions().mode() & 0o4000 == 0 {
        return Err("repair did not restore root:root setuid sudo".into());
    }

    expect_ok("check after sudo", &spk(&["check"])?)?;

    run_sudo_tests()?;
    run_p3_script_tests()?;
    run_p4_http_tests()?;

    // Exercise setuid package removal before the reboot matrix continues.
    let output = spk_zh(&["remove", "--noconfirm", "sudo"])?;
    expect_ok("remove sudo", &output)?;
    expect_contains("remove sudo", &output, "正在删除 sudo")?;
    if fs::metadata("/usr/bin/sudo").is_ok() {
        return Err("remove sudo left /usr/bin/sudo behind".into());
    }
    expect_ok("check after remove sudo", &spk(&["check"])?)?;

    set_phase("1")?;
    finish_phase("SPK_QEMU_REBOOT 1");
    Ok(())
}

fn phase_upgrade_and_repair() -> Result<(), String> {
    expect_ok("check after reboot", &spk(&["check"])?)?;
    let list = spk(&["list"])?;
    expect_contains("list after reboot", &list, "hello 0.1.0-1")?;

    let held = spk_zh(&[
        "upgrade",
        "--repo",
        "/repo/base",
        "--repo",
        "/repo/update",
        "--noconfirm",
    ])?;
    expect_ok("held upgrade", &held)?;
    expect_contains("held upgrade", &held, "被锁定")?;
    expect_contains("held upgrade", &held, "0.1.0-1")?;
    print_output("held upgrade", &held);
    let list = spk(&["list"])?;
    expect_contains("list after held upgrade", &list, "hello 0.1.0-1")?;
    expect_ok("check after held upgrade", &spk(&["check"])?)?;

    let unhold = spk_zh(&["unhold", "hello"])?;
    expect_ok("unhold hello", &unhold)?;
    if !text(&spk(&["hold"])?).trim().is_empty() {
        return Err("unhold left a hold behind".into());
    }

    let output = spk_zh(&[
        "upgrade",
        "--repo",
        "/repo/base",
        "--repo",
        "/repo/update",
        "--noconfirm",
    ])?;
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

    // Explicit --overwrite transfers ownership from file-c to file-d.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "--overwrite",
        "/usr/share/spk-tests/shared",
        "file-d",
    ])?;
    expect_ok("overwrite file-d", &output)?;
    let content =
        fs::read_to_string("/usr/share/spk-tests/shared").map_err(|error| error.to_string())?;
    if content.trim() != "from-d" {
        return Err(format!("overwrite left content '{content}'"));
    }
    let owns = text(&spk(&["owns", "/usr/share/spk-tests/shared"])?);
    if !owns.contains("file-d") {
        return Err(format!("overwrite did not transfer ownership: {owns}"));
    }
    let files_c = text(&spk(&["files", "file-c"])?);
    if files_c.contains("/usr/share/spk-tests/shared") {
        return Err("file-c still lists the overwritten path".into());
    }
    expect_ok("check after overwrite", &spk(&["check"])?)?;
    expect_ok("remove file-d", &spk(&["remove", "--noconfirm", "file-d"])?)?;
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

fn run_p4_http_tests() -> Result<(), String> {
    setup_network()?;
    let base = fs::read_to_string("/etc/spk-http-base")
        .map_err(|error| format!("read /etc/spk-http-base: {error}"))?
        .trim()
        .to_string();
    if !base.starts_with("http://") {
        return Err(format!("invalid http test base '{base}'"));
    }
    let output = spk_zh(&["repo", "add", "p4-http", &base, "--priority", "20"])?;
    expect_ok("repo add http", &output)?;

    let output = spk_zh(&["sync"])?;
    expect_ok("sync http", &output)?;
    expect_contains("sync http", &output, "已同步")?;

    let output = spk_zh(&["install", "--noconfirm", "http-pkg=1.0.0-1"])?;
    expect_ok("install http-pkg", &output)?;
    expect_contains("install http-pkg", &output, "正在安装 http-pkg")?;
    let content = fs::read_to_string("/usr/share/spk-tests/http-pkg")
        .map_err(|error| format!("http-pkg file missing: {error}"))?;
    if content.trim() != "http-v1" {
        return Err(format!("unexpected http-pkg content '{content}'"));
    }

    let output = spk_zh(&["install", "--noconfirm", "http-pkg=1.1.0-1"])?;
    expect_ok("upgrade http-pkg", &output)?;
    expect_contains("upgrade http-pkg", &output, "正在升级 http-pkg")?;
    let content = fs::read_to_string("/usr/share/spk-tests/http-pkg")
        .map_err(|error| format!("http-pkg file missing after upgrade: {error}"))?;
    if content.trim() != "http-v2" {
        return Err(format!("unexpected upgraded http-pkg content '{content}'"));
    }

    let output = spk_zh(&["remove", "--noconfirm", "http-pkg"])?;
    expect_ok("remove http-pkg", &output)?;
    if fs::metadata("/usr/share/spk-tests/http-pkg").is_ok() {
        return Err("remove http-pkg left its file behind".into());
    }

    let output = spk_zh(&["--offline", "install", "--noconfirm", "http-pkg=1.1.0-1"])?;
    expect_ok("offline http-pkg", &output)?;
    expect_contains("offline http-pkg", &output, "正在安装 http-pkg")?;
    let content = fs::read_to_string("/usr/share/spk-tests/http-pkg")
        .map_err(|error| format!("offline http-pkg file missing: {error}"))?;
    if content.trim() != "http-v2" {
        return Err(format!("unexpected offline http-pkg content '{content}'"));
    }
    let output = spk_zh(&["remove", "--noconfirm", "http-pkg"])?;
    expect_ok("remove offline http-pkg", &output)?;
    expect_ok("check after http", &spk(&["check"])?)?;
    Ok(())
}

fn setup_network() -> Result<(), String> {
    if !std::path::Path::new("/sys/class/net/eth0").exists() {
        let file = fs::File::open("/lib/modules/e1000e.ko")
            .map_err(|error| format!("open e1000e.ko: {error}"))?;
        use std::os::unix::io::AsRawFd;
        let result = unsafe { libc::syscall(libc::SYS_finit_module, file.as_raw_fd(), 0, 0) };
        if result < 0 {
            return Err(format!(
                "finit_module(e1000e) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        for _ in 0..50 {
            if std::path::Path::new("/sys/class/net/eth0").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if !std::path::Path::new("/sys/class/net/eth0").exists() {
            return Err("e1000e did not create eth0".into());
        }
    }
    // libc's if_nametoindex uses SIOCGIFINDEX; SpaceKernel exposes the index
    // through sysfs, so read it there instead of relying on the legacy ioctl.
    let ifindex = fs::read_to_string("/sys/class/net/eth0/ifindex")
        .map_err(|error| format!("read eth0 ifindex: {error}"))?
        .trim()
        .parse::<u32>()
        .map_err(|error| format!("parse eth0 ifindex: {error}"))?;
    if ifindex == 0 {
        return Err("eth0 has no ifindex".into());
    }
    // BusyBox `ip link set` uses SIOCSIFFLAGS; the kernel exposes the same
    // operation through rtnetlink RTM_NEWLINK. Configure through netlink so
    // the test does not depend on the legacy socket ioctl path.
    netlink_request(16, link_payload(ifindex))?;
    let address = [10, 0, 2, 15];
    netlink_request(20, address_payload(ifindex, &address))?;
    netlink_request(24, route_payload(ifindex, [10, 0, 2, 2]))?;
    let base = fs::read_to_string("/etc/spk-http-base")
        .map_err(|error| error.to_string())?
        .trim()
        .to_string();
    let response = Command::new("/bin/busybox")
        .args([
            "wget",
            "-q",
            "-O",
            "/dev/null",
            &format!("{base}/index.json"),
        ])
        .output()
        .map_err(|error| format!("busybox wget: {error}"))?;
    if !response.status.success() {
        return Err(format!(
            "guest cannot reach the host HTTP server: {}{}",
            String::from_utf8_lossy(&response.stdout),
            String::from_utf8_lossy(&response.stderr)
        ));
    }
    Ok(())
}

fn netlink_request(kind: u16, payload: Vec<u8>) -> Result<(), String> {
    const NLM_F_REQUEST: u16 = 1;
    const NLM_F_ACK: u16 = 4;
    const NLM_F_EXCL: u16 = 0x200;
    const NLM_F_CREATE: u16 = 0x400;
    let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_ROUTE) };
    if fd < 0 {
        return Err(format!(
            "netlink socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    let result = (|| {
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as u16;
        if unsafe {
            libc::bind(
                fd,
                &address as *const libc::sockaddr_nl as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        } < 0
        {
            return Err(format!("bind netlink: {}", std::io::Error::last_os_error()));
        }
        let length = 16 + payload.len();
        let mut message = Vec::with_capacity(length);
        message.extend_from_slice(&(length as u32).to_le_bytes());
        message.extend_from_slice(&kind.to_le_bytes());
        message.extend_from_slice(
            &(NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL).to_le_bytes(),
        );
        message.extend_from_slice(&1u32.to_le_bytes());
        message.extend_from_slice(&0u32.to_le_bytes());
        message.extend_from_slice(&payload);
        let sent = unsafe { libc::send(fd, message.as_ptr() as *const _, message.len(), 0) };
        if sent < 0 {
            return Err(format!("send netlink: {}", std::io::Error::last_os_error()));
        }
        let mut reply = [0u8; 4096];
        let read = unsafe { libc::recv(fd, reply.as_mut_ptr() as *mut _, reply.len(), 0) };
        if read < 0 {
            return Err(format!("recv netlink: {}", std::io::Error::last_os_error()));
        }
        let read = read as usize;
        if read < 20 {
            return Err("short netlink reply".into());
        }
        let reply_kind = u16::from_le_bytes([reply[4], reply[5]]);
        if reply_kind != 2 {
            return Err(format!("unexpected netlink reply type {reply_kind}"));
        }
        let error = i32::from_le_bytes([reply[16], reply[17], reply[18], reply[19]]);
        if error != 0 {
            return Err(format!("netlink error {error}"));
        }
        Ok(())
    })();
    unsafe {
        libc::close(fd);
    }
    result
}

fn put_attr(out: &mut Vec<u8>, kind: u16, data: &[u8]) {
    let length = 4 + data.len();
    out.extend_from_slice(&(length as u16).to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn link_payload(ifindex: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(16);
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(&0u16.to_le_bytes());
    payload.extend_from_slice(&(ifindex as i32).to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload
}

fn address_payload(ifindex: u32, address: &[u8; 4]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(24);
    payload.push(2);
    payload.push(24);
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(&ifindex.to_le_bytes());
    put_attr(&mut payload, 2, address);
    put_attr(&mut payload, 1, address);
    payload
}

fn route_payload(ifindex: u32, gateway: [u8; 4]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(20);
    payload.push(2);
    payload.push(0);
    payload.push(0);
    payload.push(0);
    payload.push(254);
    payload.push(3);
    payload.push(0);
    payload.push(1);
    payload.extend_from_slice(&0u32.to_le_bytes());
    put_attr(&mut payload, 5, &gateway);
    payload.extend_from_slice(&8u16.to_le_bytes());
    payload.extend_from_slice(&4u16.to_le_bytes());
    payload.extend_from_slice(&ifindex.to_le_bytes());
    payload
}

fn expect_code(label: &str, output: &Output, code: i32) -> Result<(), String> {
    if output.status.code() == Some(code) {
        Ok(())
    } else {
        Err(format!(
            "{label} exited with {:?}, expected {code}\n{}",
            output.status,
            text(output)
        ))
    }
}

fn sudo_command(args: &[&str], uid: u32) -> Command {
    let mut command = Command::new("/usr/bin/sudo");
    command
        .args(args)
        .uid(uid)
        .gid(uid)
        .env("LANG", "en")
        .env("TERM", "dumb");
    command
}

fn sudo_password(args: &[&str], uid: u32, password: &[u8]) -> Result<Output, String> {
    let mut child = sudo_command(args, uid)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run sudo {}: {error}", args.join(" ")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(password)
            .map_err(|error| format!("cannot write sudo password: {error}"))?;
    }
    child
        .wait_with_output()
        .map_err(|error| format!("sudo wait failed: {error}"))
}

fn run_p3_script_tests() -> Result<(), String> {
    // Declared file is modified by pre_install and post_install; the script
    // arguments use version-release.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "script-ok",
    ])?;
    expect_ok("script-ok install", &output)?;
    expect_contains("script-ok install", &output, "pre_install 脚本")?;
    let content =
        fs::read_to_string("/usr/share/spk-tests/script-ok").map_err(|error| error.to_string())?;
    if !content.contains("pre_install 1.0.0-1") || !content.contains("post_install 1.0.0-1") {
        return Err(format!(
            "script-ok install scriptlet output is wrong:\n{content}"
        ));
    }

    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/update",
        "--noconfirm",
        "script-ok=0.2.0-1",
    ])?;
    expect_ok("script-ok upgrade", &output)?;
    expect_contains("script-ok upgrade", &output, "pre_upgrade 脚本")?;
    let content =
        fs::read_to_string("/usr/share/spk-tests/script-ok").map_err(|error| error.to_string())?;
    if !content.contains("pre_upgrade 0.2.0-1 1.0.0-1")
        || !content.contains("post_upgrade 0.2.0-1 1.0.0-1")
    {
        return Err(format!(
            "script-ok upgrade scriptlet output is wrong:\n{content}"
        ));
    }
    let output = spk_zh(&["remove", "--noconfirm", "script-ok"])?;
    expect_ok("script-ok remove", &output)?;
    expect_contains("script-ok remove", &output, "pre_remove")?;

    // pre_install failure aborts before any files are written.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "script-fail",
    ])?;
    expect_code("script-fail rollback", &output, 6)?;
    if fs::metadata("/usr/share/spk-tests/script-fail").is_ok() {
        return Err("script-fail left its file behind".into());
    }

    // post_install failure rolls back the committed files as well.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "script-post-fail",
    ])?;
    expect_code("script-post-fail rollback", &output, 6)?;
    if fs::metadata("/usr/share/spk-tests/script-post-fail").is_ok() {
        return Err("script-post-fail left its file behind".into());
    }

    // Undeclared staging paths are rejected after the scriptlet.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "script-extra",
    ])?;
    expect_code("script-extra rollback", &output, 6)?;
    if fs::metadata("/usr/share/spk-tests/undeclared-extra").is_ok() {
        return Err("script-extra committed an undeclared file".into());
    }

    // /source is read-only; the script observes the failure and succeeds.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "script-source",
    ])?;
    expect_ok("script-source install", &output)?;
    if fs::metadata("/etc/spk-script-write").is_ok() {
        return Err("script wrote through /source".into());
    }
    let content = fs::read_to_string("/usr/share/spk-tests/source-blocked")
        .map_err(|error| error.to_string())?;
    if !content.contains("blocked") {
        return Err("script-source did not reach its declared write".into());
    }

    // The sandbox probe checks network, mount, chroot, clone namespace,
    // clone3, ptrace, mknod and /source writes from inside the scriptlet.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "sandbox-ok",
    ])?;
    expect_ok("sandbox probe", &output)?;
    let probe = fs::read_to_string("/usr/share/spk-tests/sandbox-probe.txt")
        .map_err(|error| error.to_string())?;
    if !probe.contains("SANDBOX_OK") {
        return Err(format!("sandbox probe was not fully denied:\n{probe}"));
    }

    // One trigger per package per transaction, even with two matching paths.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "trigger-pkg",
    ])?;
    expect_ok("trigger-pkg install", &output)?;
    let body = text(&output);
    if body.matches("正在执行 trigger-pkg-1.0.0-1.trigger").count() != 1
        || !body.contains("TRIGGER_RAN trigger-pkg install")
    {
        return Err(format!("trigger-pkg did not run exactly once:\n{body}"));
    }
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "trigger-other",
    ])?;
    expect_ok("trigger-other install", &output)?;
    expect_contains(
        "trigger-other install",
        &output,
        "正在执行 trigger-other-1.0.0-1.trigger",
    )?;

    // System pre/post hooks and the package hook. PreTransaction runs before
    // the filesystem phase; PostTransaction after it.
    let output = spk_zh(&["install", "--repo", "/repo/base", "--noconfirm", "hook-pkg"])?;
    expect_ok("hooks install", &output)?;
    let body = text(&output);
    for needle in [
        "正在运行事务前钩子",
        "系统测试钩子（前）",
        "SYSTEM_PRE_RAN",
        "正在运行事务后钩子",
        "系统测试钩子（后）",
        "SYSTEM_POST_RAN",
        "包内测试钩子",
        "PACKAGE_HOOK_RAN",
    ] {
        if !body.contains(needle) {
            return Err(format!("hook output is missing {needle}:\n{body}"));
        }
    }
    let pre = body.find("SYSTEM_PRE_RAN").unwrap();
    let processing = body.find("正在处理软件包的变化").unwrap_or(usize::MAX);
    let post = body.find("SYSTEM_POST_RAN").unwrap();
    if !(pre < processing && processing < post) {
        return Err(format!("hook order is wrong:\n{body}"));
    }

    // A bad sudoers file is rejected before any file is committed.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "bad-sudoers",
    ])?;
    expect_code("bad sudoers", &output, 1)?;
    let sudoers = fs::read_to_string("/etc/sudoers").map_err(|error| error.to_string())?;
    if sudoers.contains("not a valid sudoers") {
        return Err("bad sudoers was committed".into());
    }

    // scripts=true from an untrusted repository must fail signature checking.
    let output = spk(&["install", "--repo", "/repo/bad", "--noconfirm", "script-ok"])?;
    expect_code("bad signature scripts", &output, 3)?;

    // sudo package upgrade and downgrade preserve the setuid bit.
    let output = spk_zh(&[
        "install",
        "--repo",
        "/repo/update",
        "--noconfirm",
        "sudo=0.2.0-1",
    ])?;
    expect_ok("sudo upgrade", &output)?;
    expect_contains("sudo upgrade", &output, "正在升级 sudo")?;
    if fs::metadata("/usr/bin/sudo")
        .map_err(|error| error.to_string())?
        .permissions()
        .mode()
        & 0o4000
        == 0
    {
        return Err("sudo upgrade lost the setuid bit".into());
    }
    let output = spk_zh(&[
        "downgrade",
        "--repo",
        "/repo/base",
        "--noconfirm",
        "sudo=0.1.0-2",
    ])?;
    expect_ok("sudo downgrade", &output)?;
    expect_contains("sudo downgrade", &output, "正在降级 sudo")?;
    if fs::metadata("/usr/bin/sudo")
        .map_err(|error| error.to_string())?
        .permissions()
        .mode()
        & 0o4000
        == 0
    {
        return Err("sudo downgrade lost the setuid bit".into());
    }
    expect_ok("check after p3 scripts", &spk(&["check"])?)?;

    // Clean the script fixtures before the reboot matrix.
    for package in [
        "script-source",
        "sandbox-ok",
        "trigger-pkg",
        "trigger-other",
        "hook-pkg",
    ] {
        let output = spk_zh(&["remove", "--noconfirm", package])?;
        expect_ok("remove p3 fixture", &output)?;
    }
    expect_ok("check after p3 cleanup", &spk(&["check"])?)?;
    Ok(())
}

fn run_sudo_tests() -> Result<(), String> {
    // First authorization: password rule, first-auth notice, SHA-512 auth.
    let output = sudo_password(
        &["-S", "-u", "root", "/usr/bin/sudo-probe-pw"],
        1000,
        b"testpass\n",
    )?;
    expect_ok("sudo password auth", &output)?;
    expect_contains("sudo password auth", &output, "UID=0 EUID=0")?;
    expect_contains(
        "sudo password auth",
        &output,
        "First successful authorization",
    )?;
    print_output("sudo password auth", &output);

    // Ticket is global in the test sudoers; no TTY is required.
    let output = sudo_command(&["-n", "-u", "root", "/usr/bin/sudo-probe-pw"], 1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run sudo ticket: {error}"))?;
    expect_ok("sudo ticket", &output)?;
    expect_contains("sudo ticket", &output, "UID=0 EUID=0")?;

    // BusyBox dispatches /bin/su from argv[0]; sudo must not rewrite it to
    // bbsuid when canonicalizing the symlink.
    let output = sudo_command(&["-n", "su", "-c", "id"], 1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run sudo su: {error}"))?;
    expect_ok("sudo su", &output)?;
    expect_contains("sudo su", &output, "uid=0")?;

    // Target a non-root user: exercises setgroups/setgid/setuid order.
    let output = sudo_command(&["-n", "-u", "empty", "/usr/bin/sudo-probe"], 1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run sudo -u empty: {error}"))?;
    expect_ok("sudo -u empty", &output)?;
    expect_contains("sudo -u empty", &output, "UID=1001")?;
    expect_contains("sudo -u empty", &output, "EUID=1001")?;
    expect_contains("sudo -u empty", &output, "GID=1001")?;
    expect_contains("sudo -u empty", &output, "EGID=1001")?;

    // NOPASSWD rule.
    let output = sudo_command(&["-n", "-u", "root", "/usr/bin/sudo-probe"], 1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run sudo nopass: {error}"))?;
    expect_ok("sudo nopass", &output)?;
    expect_contains("sudo nopass", &output, "UID=0 EUID=0")?;

    // sudoedit keeps a valid ticket and runs the editor as the invoking user.
    let output = sudo_command(&["-n", "-e", "/etc/test-edit"], 1000)
        .env("SUDO_EDITOR", "/usr/bin/sudo-edit-probe")
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run sudoedit: {error}"))?;
    expect_ok("sudoedit", &output)?;
    let edited = fs::read_to_string("/etc/test-edit").map_err(|error| error.to_string())?;
    if !edited.contains("edited") {
        return Err(format!(
            "sudoedit did not update /etc/test-edit: {edited:?}"
        ));
    }

    // sudo -l and a denied command.
    let output = sudo_command(&["-l"], 1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run sudo -l: {error}"))?;
    expect_ok("sudo -l", &output)?;
    expect_contains("sudo -l", &output, "NOPASSWD")?;
    let output = sudo_command(&["-n", "-u", "root", "/usr/bin/nosetuid-probe"], 1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run denied sudo: {error}"))?;
    expect_code("denied sudo", &output, 1)?;

    // doas uses doas.conf and has no ticket.
    let output = Command::new("/usr/bin/doas")
        .args(["-u", "root", "/usr/bin/sudo-probe"])
        .uid(1000)
        .gid(1000)
        .env("LANG", "en")
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run doas: {error}"))?;
    expect_ok("doas", &output)?;
    expect_contains("doas", &output, "UID=0 EUID=0")?;

    // OOBE-style empty password: the empty shadow field accepts an empty
    // password and the first-auth notice carries the security warning.
    let output = sudo_password(&["-S", "-u", "root", "/usr/bin/sudo-probe-pw"], 1001, b"\n")?;
    expect_ok("empty password sudo", &output)?;
    expect_contains("empty password sudo", &output, "UID=0 EUID=0")?;
    expect_contains("empty password sudo", &output, "empty password")?;

    // Clearing the ticket forces a password; non-interactive must fail.
    let output = sudo_command(&["-k"], 1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run sudo -k: {error}"))?;
    expect_ok("sudo -k", &output)?;
    let output = sudo_command(&["-n", "-u", "root", "/usr/bin/sudo-probe-pw"], 1000)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run sudo after -k: {error}"))?;
    expect_code("sudo after -k", &output, 1)?;

    let audit = fs::read_to_string("/var/log/sudo.log").map_err(|error| error.to_string())?;
    if !audit.contains("result=executed") || !audit.contains("sudoedit-changed") {
        return Err(format!("sudo audit log is incomplete:\n{audit}"));
    }
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

fn spk_as(uid: u32, args: &[&str]) -> Result<Output, String> {
    Command::new(SPK)
        .args(args)
        .uid(uid)
        .gid(uid)
        .stdin(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run spk {} as uid {uid}: {error}", args.join(" ")))
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
    println!("{message}");
    println!("SPK_QEMU_FAIL");
    io::stdout().flush().ok();
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
