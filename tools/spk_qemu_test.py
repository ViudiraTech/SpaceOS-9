#!/usr/bin/env python3
#
#       tools/spk_qemu_test.py
#       QEMU matrix for spk P1: install, reboot, upgrade, downgrade,
#       SIGKILL + repair, dependency/file conflicts and -Rs cleanup.
#
#       2026/11/20 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Build a minimal ext4 rootfs and run the spk P1 matrix in QEMU.

The guest is a static musl `spk-qemu-test` binary running as PID 1. It writes
a phase marker to /spk-test-phase, prints a reboot marker and waits; the host
terminates QEMU and boots the same disk again for the next phase.
"""

import argparse
import codecs
import json
import os
import pathlib
import selectors
import shutil
import signal
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "build" / "spk-qemu"
TARGET_BIN = ROOT / "target" / "x86_64-unknown-linux-musl" / "release"
HOST_BIN = ROOT / "target" / "release" / "spk-host"
IMAGE = BUILD / "rootfs.img"
ISO = BUILD / "SpaceKernel-spk.iso"
EXT4_FEATURES = (
    "has_journal,extent,huge_file,flex_bg,metadata_csum,metadata_csum_seed,64bit,"
    "dir_nlink,extra_isize,^orphan_file,^resize_inode,^dir_index"
)
CMDLINE = (
    "console=ttyS0 root=/dev/nvme0n1 rootfstype=ext4 rw "
    "init=/sbin/spk-qemu-test"
)
REQUIRES = ("SPK_QEMU_REBOOT 1", "SPK_QEMU_REBOOT 2", "SPK_QEMU_OK")


def log(message: str) -> None:
    print(f"[spk-qemu] {message}", flush=True)


def run(command, **kwargs):
    log("+ " + " ".join(str(part) for part in command))
    return subprocess.run(command, check=True, **kwargs)


def cargo(command: list[str]) -> None:
    environment = os.environ.copy()
    environment["RUSTUP_TOOLCHAIN"] = "nightly-2026-05-24"
    # ring/rustls needs a C toolchain for the musl target; clang can assemble
    # the pre-generated objects without a musl-gcc wrapper.
    environment.setdefault("CC_x86_64_unknown_linux_musl", "clang")
    environment.setdefault("AR_x86_64_unknown_linux_musl", "ar")
    run(
        ["cargo", *command],
        cwd="/tmp",
        env=environment,
    )


def build_artifacts() -> None:
    run(["make", "-C", str(ROOT), "modules", "ARCH=x86_64", "PROFILE=release"], cwd=ROOT)
    if not (ROOT / "build" / "userland" / "libsplibc.a").exists():
        run([str(ROOT / "tools" / "build_userland.sh")], cwd=ROOT)
    cargo(
        [
            "build",
            "--manifest-path",
            str(ROOT / "user" / "spk" / "Cargo.toml"),
            "-p",
            "spk",
            "--bins",
            "--release",
            "--target",
            "x86_64-unknown-linux-musl",
            "--target-dir",
            str(ROOT / "target"),
        ]
    )
    cargo(
        [
            "build",
            "--manifest-path",
            str(ROOT / "user" / "spk" / "Cargo.toml"),
            "-p",
            "spk-host",
            "--release",
            "--target-dir",
            str(ROOT / "target"),
        ]
    )
    cargo(
        [
            "build",
            "--manifest-path",
            str(ROOT / "user" / "sudo" / "Cargo.toml"),
            "--bins",
            "--release",
            "--target",
            "x86_64-unknown-linux-musl",
            "--target-dir",
            str(ROOT / "target"),
        ]
    )
    run([str(ROOT / "packages" / "hello" / "build.sh"), "0.1.0"], cwd=ROOT)
    run([str(ROOT / "packages" / "hello" / "build.sh"), "0.2.0"], cwd=ROOT)
    run([str(ROOT / "packages" / "spk-tests" / "build.sh")], cwd=ROOT)
    run([str(ROOT / "packages" / "sudo" / "build.sh")], cwd=ROOT)


def pack(manifest: pathlib.Path, staging: pathlib.Path, output_dir: pathlib.Path) -> None:
    run(
        [
            str(HOST_BIN),
            "pack",
            "--manifest",
            str(manifest),
            "--root",
            str(staging),
            "--output",
            str(output_dir),
        ],
        cwd=ROOT,
    )


def prepare_repositories() -> str:
    keys = BUILD / "keys"
    run(
        [
            str(HOST_BIN),
            "keygen",
            "--name",
            "spk-test",
            "--out",
            str(keys),
            "--comment",
            "SpaceOS QEMU Test <test@spaceos>",
        ]
    )
    key_id = subprocess.check_output(
        [str(HOST_BIN), "keyid", "--key", str(keys / "spk-test.pub")],
        text=True,
    ).strip()

    base = BUILD / "repo-base"
    update = BUILD / "repo-update"
    base.mkdir(parents=True)
    update.mkdir(parents=True)
    pack(
        ROOT / "packages" / "hello" / "manifest.toml",
        ROOT / "build" / "packages" / "hello-0.1.0",
        base,
    )
    for name in (
        "dep-a",
        "dep-b",
        "file-c",
        "file-d",
        "app",
        "lib",
        "script-ok",
        "script-fail",
        "script-post-fail",
        "script-extra",
        "script-source",
        "sandbox-ok",
        "trigger-pkg",
        "trigger-other",
        "hook-pkg",
        "bad-sudoers",
    ):
        pack(
            ROOT / "packages" / "spk-tests" / name / "manifest.toml",
            ROOT / "build" / "spk-tests" / name,
            base,
        )
    pack(
        ROOT / "packages" / "sudo" / "manifest.toml",
        ROOT / "build" / "packages" / "sudo",
        base,
    )
    for package in base.glob("*.spk"):
        shutil.copy2(package, update / package.name)
    pack(
        ROOT / "packages" / "hello" / "manifest-0.2.0.toml",
        ROOT / "build" / "packages" / "hello-0.2.0",
        update,
    )
    pack(
        ROOT / "packages" / "spk-tests" / "script-ok-0.2" / "manifest.toml",
        ROOT / "build" / "spk-tests" / "script-ok-0.2",
        update,
    )
    pack(
        ROOT / "packages" / "sudo" / "manifest-0.2.0.toml",
        ROOT / "build" / "packages" / "sudo",
        update,
    )
    for directory in (base, update):
        run(
            [
                str(HOST_BIN),
                "index",
                "--repo-dir",
                str(directory),
                "--repo",
                "qemu-test",
                "--arch",
                "x86_64",
                "--key",
                str(keys / "spk-test.key"),
            ]
        )
        run(
            [
                str(HOST_BIN),
                "verify",
                "--repo-dir",
                str(directory),
                "--key",
                str(keys / "spk-test.pub"),
            ]
        )
    bad_keys = BUILD / "keys-bad"
    run(
        [
            str(HOST_BIN),
            "keygen",
            "--name",
            "spk-bad-test",
            "--out",
            str(bad_keys),
            "--comment",
            "SpaceOS QEMU bad signature <bad@spaceos>",
        ]
    )
    bad = BUILD / "repo-bad"
    bad.mkdir(parents=True)
    shutil.copy2(
        base / "script-ok-1.0.0-1-x86_64.spk",
        bad / "script-ok-1.0.0-1-x86_64.spk",
    )
    for directory, key, name in ((bad, bad_keys, "spk-bad-test"),):
        run(
            [
                str(HOST_BIN),
                "index",
                "--repo-dir",
                str(directory),
                "--repo",
                name,
                "--arch",
                "x86_64",
                "--key",
                str(key / "spk-bad-test.key"),
            ]
        )
        run(
            [
                str(HOST_BIN),
                "verify",
                "--repo-dir",
                str(directory),
                "--key",
                str(key / "spk-bad-test.pub"),
            ]
        )
    http_repo = BUILD / "repo-http"
    http_repo.mkdir(parents=True)
    for package in base.glob("*.spk"):
        shutil.copy2(package, http_repo / package.name)
    for name in ("http-pkg", "http-pkg-1.1"):
        pack(
            ROOT / "packages" / "spk-tests" / name / "manifest.toml",
            ROOT / "build" / "spk-tests" / name,
            http_repo,
        )
    run(
        [
            str(HOST_BIN),
            "index",
            "--repo-dir",
            str(http_repo),
            "--repo",
            "qemu-test",
            "--arch",
            "x86_64",
            "--key",
            str(keys / "spk-test.key"),
        ]
    )
    run(
        [
            str(HOST_BIN),
            "verify",
            "--repo-dir",
            str(http_repo),
            "--key",
            str(keys / "spk-test.pub"),
        ]
    )
    return key_id


def start_http_server(directory: pathlib.Path):
    import http.server
    import threading

    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=str(directory), **kwargs)

        def log_message(self, format, *args):
            log("http " + (format % args))

    server = http.server.ThreadingHTTPServer(("0.0.0.0", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, server.server_address[1]


def build_rootfs(key_id: str, http_base: str) -> None:
    stage = BUILD / "root"
    for directory in (
        "sbin",
        "bin",
        "lib",
        "usr/bin",
        "usr/share/spk-tests",
        "lib/modules",
        "etc/ssl/certs",
        "etc/spk/trusted.d",
        "etc/spk/hooks.d",
        "var/log",
        "var/lib/sudo",
        "var/tmp",
        "home/test",
        "home/empty",
        "repo/base",
        "repo/update",
        "repo/bad",
        "var/lib",
        "var/cache",
        "tmp",
        "proc",
        "sys",
        "dev",
        "run",
    ):
        (stage / directory).mkdir(parents=True, exist_ok=True)
    for source, destination in (
        (TARGET_BIN / "spk", stage / "usr/bin/spk"),
        (TARGET_BIN / "spk-sandbox", stage / "usr/bin/spk-sandbox"),
        (TARGET_BIN / "spk-qemu-test", stage / "sbin/spk-qemu-test"),
    ):
        if not source.exists():
            raise SystemExit(f"missing {source}; run with --build")
        shutil.copy2(source, destination)
        destination.chmod(0o755)
    busybox = next(
        (
            candidate
            for candidate in (
                ROOT / "build" / "userland" / "busybox",
                ROOT / "build" / "rootfs" / "stage" / "bin" / "busybox",
            )
            if candidate.is_file()
        ),
        None,
    )
    if busybox is None:
        raise SystemExit("missing busybox; run tools/build_busybox.sh")
    shutil.copy2(busybox, stage / "bin" / "busybox")
    (stage / "bin" / "busybox").chmod(0o755)
    shutil.copy2(busybox, stage / "bin" / "sh")
    (stage / "bin" / "sh").chmod(0o755)
    # Setuid BusyBox applet, mirroring the desktop rootfs.  sudo must preserve
    # argv[0]="su" instead of canonicalizing the symlink to "bbsuid".
    shutil.copy2(busybox, stage / "bin" / "bbsuid")
    (stage / "bin" / "bbsuid").chmod(0o4755)
    su = stage / "bin" / "su"
    if su.exists() or su.is_symlink():
        su.unlink()
    su.symlink_to("bbsuid")
    loader = next(
        (
            candidate
            for candidate in (
                ROOT / "build" / "userland" / "musl" / "ld-musl-x86_64.so.1",
                ROOT / "build" / "rootfs" / "stage" / "lib" / "ld-musl-x86_64.so.1",
            )
            if candidate.is_file()
        ),
        None,
    )
    libc = next(
        (
            candidate
            for candidate in (
                ROOT / "build" / "userland" / "musl" / "libc.so",
                ROOT / "build" / "rootfs" / "stage" / "lib" / "libc.musl-x86_64.so.1",
            )
            if candidate.is_file()
        ),
        None,
    )
    if loader is None or libc is None:
        raise SystemExit("missing musl loader/libc; run tools/build_busybox.sh")
    shutil.copy2(loader, stage / "lib" / "ld-musl-x86_64.so.1")
    (stage / "lib" / "ld-musl-x86_64.so.1").chmod(0o755)
    shutil.copy2(libc, stage / "lib" / "libc.musl-x86_64.so.1")
    (stage / "lib" / "libc.musl-x86_64.so.1").chmod(0o755)
    sandbox_probe = ROOT / "build" / "spk-tests" / "sandbox-probe"
    if not sandbox_probe.is_file():
        raise SystemExit(f"missing {sandbox_probe}; run packages/spk-tests/build.sh")
    shutil.copy2(sandbox_probe, stage / "usr/bin/sandbox-probe")
    (stage / "usr/bin/sandbox-probe").chmod(0o755)
    module = ROOT / "build" / "modules" / "x86_64" / "e1000e.ko"
    if not module.is_file():
        raise SystemExit(f"missing {module}; run make modules ARCH=x86_64 PROFILE=release")
    shutil.copy2(module, stage / "lib/modules/e1000e.ko")
    (stage / "lib/modules/e1000e.ko").chmod(0o644)
    (stage / "etc/spk-http-base").write_text(http_base + "\n")
    (stage / "etc/spk-http-base").chmod(0o644)
    ca_bundle = pathlib.Path("/etc/ssl/certs/ca-certificates.crt")
    if ca_bundle.is_file():
        shutil.copy2(ca_bundle, stage / "etc/ssl/certs/ca-certificates.crt")
        (stage / "etc/ssl/certs/ca-certificates.crt").chmod(0o644)
    for directory in ("base", "update", "bad"):
        for package in (BUILD / f"repo-{directory}").iterdir():
            shutil.copy2(package, stage / "repo" / directory / package.name)
    probe = ROOT / "build" / "spk-tests" / "setuid-probe"
    if not probe.exists():
        raise SystemExit(f"missing {probe}; run packages/spk-tests/build.sh")
    setuid = stage / "usr/bin/setuid-probe"
    shutil.copy2(probe, setuid)
    setuid.chmod(0o6755)
    plain = stage / "usr/bin/nosetuid-probe"
    shutil.copy2(probe, plain)
    plain.chmod(0o0755)
    for name in ("sudo-probe", "sudo-probe-pw"):
        target = stage / "usr/bin" / name
        shutil.copy2(probe, target)
        target.chmod(0o0755)
    edit_probe = ROOT / "build" / "spk-tests" / "sudo-edit-probe"
    if not edit_probe.exists():
        raise SystemExit(f"missing {edit_probe}; run packages/spk-tests/build.sh")
    shutil.copy2(edit_probe, stage / "usr/bin/sudo-edit-probe")
    (stage / "usr/bin/sudo-edit-probe").chmod(0o0755)

    hash_value = subprocess.check_output(
        ["openssl", "passwd", "-6", "-salt", "spktest", "testpass"],
        text=True,
    ).strip()
    if not hash_value.startswith("$6$"):
        raise SystemExit("openssl did not produce a SHA-512 crypt hash")
    (stage / "etc/passwd").write_text(
        "root:x:0:0:root:/root:/bin/sh\n"
        "test:x:1000:1000:Test User:/home/test:/bin/sh\n"
        "empty:x:1001:1001:Empty User:/home/empty:/bin/sh\n"
    )
    (stage / "etc/group").write_text(
        "root:x:0:\ntest:x:1000:\nempty:x:1001:\nwheel:x:10:test\n"
    )
    (stage / "etc/shadow").write_text(
        f"root:!::0:::::\ntest:{hash_value}:20000:0:99999:7:::\nempty::20000:0:99999:7:::\n"
    )
    (stage / "etc/sudoers").write_text(
        "Defaults lecture=never,timestamp_timeout=15,timestamp_type=global,"
        "secure_path=\"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\"\n"
        "test ALL=(root) NOPASSWD: /usr/bin/sudo-probe\n"
        "test ALL=(empty) NOPASSWD: /usr/bin/sudo-probe\n"
        "test ALL=(root) /usr/bin/sudo-probe-pw\n"
        "test ALL=(root) sudoedit\n"
        "empty ALL=(root) /usr/bin/sudo-probe-pw\n"
    )
    (stage / "etc/doas.conf").write_text(
        "permit nopass test as root cmd /usr/bin/sudo-probe\n"
    )
    (stage / "etc/spk/setuid.allow").write_text("/usr/bin/sudo\n/usr/bin/doas\n")
    (stage / "etc/spk/hooks.d/10-pre.hook").write_text(
        "[Trigger]\nOperation = Install\nType = Package\nTarget = hook-pkg\n\n"
        "[Action]\nDescription = 系统测试钩子（前）\nWhen = PreTransaction\n"
        "Exec = printf 'SYSTEM_PRE_RAN\\n'\n"
    )
    (stage / "etc/spk/hooks.d/20-post.hook").write_text(
        "[Trigger]\nOperation = Install\nType = Package\nTarget = hook-pkg\n\n"
        "[Action]\nDescription = 系统测试钩子（后）\nWhen = PostTransaction\n"
        "Exec = printf 'SYSTEM_POST_RAN\\n'\n"
    )
    (stage / "etc/spk/hooks.d/10-pre.hook").chmod(0o644)
    (stage / "etc/spk/hooks.d/20-post.hook").chmod(0o644)
    (stage / "etc/test-edit").write_text("original\n")
    for name in ("etc/passwd", "etc/group"):
        (stage / name).chmod(0o644)
    (stage / "etc/shadow").chmod(0o600)
    for name in ("etc/sudoers", "etc/doas.conf"):
        (stage / name).chmod(0o440)
    (stage / "etc/spk/setuid.allow").chmod(0o644)
    (stage / "etc/test-edit").chmod(0o644)
    public = BUILD / "keys" / "spk-test.pub"
    shutil.copy2(public, stage / "etc" / "spk" / "trusted.d" / f"{key_id}.pub")
    (stage / "etc" / "spk" / "trusted.d" / f"{key_id}.pub").chmod(0o644)

    if IMAGE.exists():
        IMAGE.unlink()
    with open(IMAGE, "wb") as image:
        image.truncate(128 * 1024 * 1024)
    mke2fs = shutil.which("mke2fs") or "/usr/sbin/mke2fs"
    prefix = ["fakeroot"] if shutil.which("fakeroot") else []
    run(
        prefix
        + [
            mke2fs,
            "-q",
            "-F",
            "-t",
            "ext4",
            "-b",
            "4096",
            "-O",
            EXT4_FEATURES,
            "-E",
            "lazy_itable_init=0,lazy_journal_init=0",
            "-J",
            "size=16",
            "-L",
            "SPKTEST",
            "-d",
            str(stage),
            str(IMAGE),
        ]
    )
    e2fsck = shutil.which("e2fsck") or "/usr/sbin/e2fsck"
    run([e2fsck, "-fn", str(IMAGE)], stdout=subprocess.DEVNULL)


def build_iso() -> None:
    command = [
        "make",
        "-C",
        str(ROOT),
        "iso",
        "ARCH=x86_64",
        "PROFILE=release",
        "WITH_ROOTFS=0",
        f"CMDLINE={CMDLINE}",
        f"ISO={ISO}",
        f"ISO_ROOT={BUILD / 'iso'}",
    ]
    log("+ " + " ".join(command))
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    (BUILD / "iso-build.log").write_text(result.stdout, encoding="utf-8")
    if result.returncode != 0:
        print(result.stdout[-4000:])
        raise SystemExit("ISO build failed; see build/spk-qemu/iso-build.log")
    if not ISO.exists():
        raise SystemExit("ISO build did not produce the expected file")


def boot(marker: str, timeout: float, log_path: pathlib.Path, accel: str) -> tuple[bool, bool]:
    command = [
        "qemu-system-x86_64",
        "-machine",
        "q35",
        "-m",
        "1024",
        "-smp",
        "2",
        "-display",
        "none",
        "-serial",
        "stdio",
        "-monitor",
        "none",
        "-no-reboot",
        "-nic",
        "user,model=e1000e",
        "-drive",
        "if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd",
        "-drive",
        f"file={IMAGE},if=none,id=root,format=raw",
        "-device",
        "nvme,serial=SPROOT,drive=root",
        "-cdrom",
        str(ISO),
    ]
    if accel == "kvm":
        command += ["-enable-kvm", "-cpu", "host"]
    else:
        command += ["-accel", "tcg", "-cpu", "max"]
    log("+ " + " ".join(command))
    process = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        bufsize=0,
    )
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    chunks: list[str] = []
    buffer = ""
    deadline = time.time() + timeout
    detected = False
    failed = False
    try:
        while time.time() < deadline:
            if process.poll() is not None and not selector.get_map():
                break
            for key, _ in selector.select(0.5):
                data = os.read(key.fd, 65536)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                text = decoder.decode(data)
                chunks.append(text)
                buffer += text
                if "SPK_QEMU_FAIL" in buffer:
                    failed = True
                    detected = True
                    marker = "SPK_QEMU_FAIL"
                    break
                if marker in buffer:
                    detected = True
                    break
            if detected:
                break
    finally:
        if process.poll() is None:
            process.kill()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        try:
            selector.close()
        except Exception:
            pass
        if process.stdout is not None:
            try:
                tail = process.stdout.read()
                if tail:
                    chunks.append(decoder.decode(tail))
                chunks.append(decoder.decode(b"", final=True))
            except Exception:
                pass
    body = "".join(chunks)
    log_path.write_text(body, encoding="utf-8")
    interesting = (
        "SPK_QEMU_",
        "正在安装",
        "正在升级",
        "正在降级",
        "正在重新安装",
        "正在删除",
        "事务已回滚",
        "数据库一致",
        "OK:",
        "error:",
    )
    for line in body.splitlines():
        if any(token in line for token in interesting):
            print(f"  | {line}")
    if not detected or failed:
        tail = "\n".join(body.splitlines()[-60:])
        log(f"boot did not reach {marker!r}; tail:\n{tail}")
    return detected and not failed, failed


def offline_checks() -> bool:
    def debugfs_cat(path: str, include_stderr: bool = False) -> str:
        result = subprocess.run(
            ["debugfs", "-R", f"cat {path}", str(IMAGE)],
            text=True,
            capture_output=True,
        )
        return result.stdout + (result.stderr if include_stderr else "")

    phase_output = debugfs_cat("/spk-test-phase")
    phase = next(
        (
            line.strip()
            for line in phase_output.splitlines()
            if line.strip().isdigit()
        ),
        "",
    )
    if phase != "3":
        log(f"unexpected phase marker: {phase_output!r}")
        return False
    installed = debugfs_cat("/var/lib/spk/installed.json")
    try:
        start = installed.index("{")
        database = json.loads(installed[start:])
    except Exception as error:
        log(f"cannot parse installed.json from image: {error}\n{installed}")
        return False
    if database.get("packages"):
        log(f"installed.json is not empty: {database}")
        return False
    stat = debugfs_cat("/usr/bin/hello", include_stderr=True)
    if "File not found" not in stat and "not found" not in stat.lower():
        log(f"/usr/bin/hello still exists in the final image:\n{stat}")
        return False
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true", help="reuse existing artifacts")
    parser.add_argument("--timeout", type=float, default=240.0)
    parser.add_argument("--accel", choices=["auto", "kvm", "tcg"], default="auto")
    parser.add_argument("--result", help="write a JSON result")
    arguments = parser.parse_args()

    if not arguments.no_build:
        shutil.rmtree(BUILD, ignore_errors=True)
    BUILD.mkdir(parents=True, exist_ok=True)

    try:
        if not arguments.no_build:
            build_artifacts()
            key_id = prepare_repositories()
            http_server, http_port = start_http_server(BUILD / "repo-http")
            http_base = f"http://10.0.2.2:{http_port}"
            build_rootfs(key_id, http_base)
            build_iso()
        else:
            key_id = subprocess.check_output(
                [str(HOST_BIN), "keyid", "--key", str(BUILD / "keys" / "spk-test.pub")],
                text=True,
            ).strip()

        accel = arguments.accel
        if accel == "auto":
            accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) else "tcg"
        timeout = arguments.timeout if accel == "kvm" else max(arguments.timeout, 600.0)
        logs = []
        for index, marker in enumerate(REQUIRES, start=1):
            path = BUILD / f"boot{index}.log"
            log(f"boot {index}: waiting for {marker!r} ({accel})")
            started = time.time()
            ok, guest_failed = boot(marker, timeout, path, accel)
            if not ok and not guest_failed and accel == "kvm":
                log("KVM boot failed; retrying this phase under TCG")
                accel = "tcg"
                path = BUILD / f"boot{index}-tcg.log"
                ok, guest_failed = boot(marker, max(timeout, 600.0), path, accel)
            if not ok:
                return report(arguments.result, False, key_id, logs)
            logs.append({"phase": index, "marker": marker, "seconds": round(time.time() - started, 1)})
        ok = offline_checks()
        return report(arguments.result, ok, key_id, logs)
    except Exception as error:
        log(f"failure: {error}")
        return 1


def report(path: str | None, ok: bool, key_id: str, logs: list[dict]) -> int:
    result = {
        "ok": ok,
        "key_id": key_id,
        "image": str(IMAGE),
        "iso": str(ISO),
        "boots": logs,
    }
    if path:
        pathlib.Path(path).parent.mkdir(parents=True, exist_ok=True)
        pathlib.Path(path).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    log("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
