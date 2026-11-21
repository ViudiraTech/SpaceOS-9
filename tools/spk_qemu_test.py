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
    run(
        ["cargo", *command],
        cwd="/tmp",
        env=environment,
    )


def build_artifacts() -> None:
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
    run([str(ROOT / "packages" / "hello" / "build.sh"), "0.1.0"], cwd=ROOT)
    run([str(ROOT / "packages" / "hello" / "build.sh"), "0.2.0"], cwd=ROOT)
    run([str(ROOT / "packages" / "spk-tests" / "build.sh")], cwd=ROOT)


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
    for name in ("dep-a", "dep-b", "file-c", "file-d", "app", "lib"):
        pack(
            ROOT / "packages" / "spk-tests" / name / "manifest.toml",
            ROOT / "build" / "spk-tests" / name,
            base,
        )
    for package in base.glob("*.spk"):
        shutil.copy2(package, update / package.name)
    pack(
        ROOT / "packages" / "hello" / "manifest-0.2.0.toml",
        ROOT / "build" / "packages" / "hello-0.2.0",
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
    return key_id


def build_rootfs(key_id: str) -> None:
    stage = BUILD / "root"
    for directory in (
        "sbin",
        "usr/bin",
        "etc/spk/trusted.d",
        "repo/base",
        "repo/update",
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
        (TARGET_BIN / "spk-qemu-test", stage / "sbin/spk-qemu-test"),
    ):
        if not source.exists():
            raise SystemExit(f"missing {source}; run with --build")
        shutil.copy2(source, destination)
        destination.chmod(0o755)
    for directory in ("base", "update"):
        for package in (BUILD / f"repo-{directory}").iterdir():
            shutil.copy2(package, stage / "repo" / directory / package.name)
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


def boot(marker: str, timeout: float, log_path: pathlib.Path, accel: str) -> bool:
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
        "none",
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
    if not detected:
        tail = "\n".join(body.splitlines()[-40:])
        log(f"boot did not reach {marker!r}; tail:\n{tail}")
    return detected


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
            build_rootfs(key_id)
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
            if not boot(marker, timeout, path, accel):
                if accel == "kvm":
                    log("KVM boot failed; retrying this phase under TCG")
                    accel = "tcg"
                    if not boot(marker, max(timeout, 600.0), path, accel):
                        return report(arguments.result, False, key_id, logs)
                else:
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
