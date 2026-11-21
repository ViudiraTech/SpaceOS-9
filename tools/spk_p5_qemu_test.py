#!/usr/bin/env python3
#
#       tools/spk_p5_qemu_test.py
#       P5 offline-repository and ISO verification matrix
#
#       2026/11/23 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Boot the P5 image, verify the embedded repo and run offline transactions."""

import argparse
import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "build/p5"
ROOTFS = BUILD / "rootfs.img"
FAT = BUILD / "rootfs.fat"
INSTALL_ISO = BUILD / "p5-install.iso"
VERIFY_ISO = BUILD / "p5-verify.iso"
FIRMWARE = pathlib.Path("/usr/share/OVMF/OVMF_CODE_4M.fd")


def run(command, **kwargs):
    print("+ " + " ".join(str(item) for item in command), flush=True)
    return subprocess.run(command, check=True, **kwargs)


def make(*arguments: str) -> None:
    run(["make", "-C", str(ROOT), *arguments])


def build() -> None:
    make("p5-isos")


def qemu(iso: pathlib.Path, log: pathlib.Path, rootfs: pathlib.Path | None, marker: str) -> None:
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
        f"if=pflash,format=raw,readonly=on,file={FIRMWARE}",
    ]
    if rootfs is not None:
        command += [
            "-drive",
            f"file={rootfs},if=none,id=root,format=raw",
            "-device",
            "nvme,serial=SPROOT,drive=root",
        ]
    command += ["-cdrom", str(iso)]
    if os.access("/dev/kvm", os.R_OK | os.W_OK):
        command += ["-enable-kvm", "-cpu", "host"]
    print("+ " + " ".join(command), flush=True)
    with log.open("w") as output:
        process = subprocess.Popen(
            ["timeout", "600", *command],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
        )
        assert process.stdout is not None
        for line in process.stdout:
            output.write(line)
            output.flush()
            if line.strip() == marker:
                process.kill()
                process.wait()
                print(f"PASS: {marker}", flush=True)
                return
            if "SPK_P5_FAIL" in line:
                process.kill()
                process.wait()
                raise SystemExit(f"P5 driver failed: {line.strip()}")
        process.wait()
    raise SystemExit(f"{marker} not found; see {log}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--no-build", action="store_true")
    arguments = parser.parse_args()
    if not arguments.no_build:
        build()
    # Boot a disposable copy so the delivered ext4 image keeps its pristine,
    # reproducible on-disk state (the kernel mounts it read-write).
    qemu_rootfs = BUILD / "qemu-rootfs.img"
    shutil.copy2(ROOTFS, qemu_rootfs)
    try:
        qemu(INSTALL_ISO, BUILD / "p5-install.log", qemu_rootfs, "SPK_P5_OK")
    finally:
        qemu_rootfs.unlink(missing_ok=True)
    qemu(VERIFY_ISO, BUILD / "p5-verify.log", None, "SPK_P5_VERIFY_OK")
    print("P5 QEMU matrix PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
