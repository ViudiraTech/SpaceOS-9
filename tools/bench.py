#!/usr/bin/env python3
#
#       tools/bench.py
#       Build spbench into a FAT32 rootfs, boot it as PID 1 and collect results
#
#       2026/11/15 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Run the SpaceOS-9 Linux-ABI benchmark under QEMU.

The kernel is built with the release profile and without boot self-tests so
the numbers reflect the shipping configuration.  Results are printed as a
table and optionally written to JSON; ``--compare`` prints the delta against
an earlier JSON run so optimizations can be judged from the same harness.
"""

import argparse
import json
import os
import pathlib
import re
import selectors
import shutil
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "build" / "bench"
IMAGE = OUT / "rootfs.img"
BENCH_RE = re.compile(r"^BENCH (\S+)\s+([-0-9.]+) (\S+)")
FIRMWARE = {
    "x86_64": "/usr/share/OVMF/OVMF_CODE_4M.fd",
}
# Lower is better for every unit except throughput units.
HIGHER_IS_BETTER = {"MiB/s", "Mops/s", "Mloops/s"}


def run(*command, **kwargs):
    subprocess.run(command, check=True, cwd=ROOT, **kwargs)


def build_binary() -> pathlib.Path:
    OUT.mkdir(parents=True, exist_ok=True)
    binary = OUT / "spbench"
    cc = shutil.which("musl-gcc")
    if cc is None:
        raise SystemExit("musl-gcc not found (install musl-tools)")
    run(cc, "-static", "-O2", "-Wall", "-Wextra", "-Werror", "-o", str(binary),
        "user/bench/spbench.c", "-lpthread")
    return binary


def build_image(binary: pathlib.Path, conf: str = "") -> None:
    if IMAGE.exists():
        IMAGE.unlink()
    with open(IMAGE, "wb") as image:
        image.truncate(32 * 1024 * 1024)
    run("mkfs.vfat", "-F", "32", "-n", "SPBENCH", str(IMAGE), stdout=subprocess.DEVNULL)
    for directory in ("/bin", "/etc", "/tmp", "/dev", "/proc", "/sys", "/run"):
        run("mmd", "-i", str(IMAGE), f"::{directory}")
    run("mcopy", "-i", str(IMAGE), str(binary), "::/bin/spbench")
    hostname = OUT / "hostname"
    hostname.write_text("spbench\n")
    run("mcopy", "-i", str(IMAGE), str(hostname), "::/etc/hostname")
    if conf:
        settings = OUT / "spbench.conf"
        settings.write_text(conf)
        run("mcopy", "-i", str(IMAGE), str(settings), "::/etc/spbench.conf")


def build_iso(arch: str, profile: str) -> pathlib.Path:
    env = dict(os.environ)
    env["CMDLINE"] = "console=ttyS0 init=/bin/spbench"
    run("make", "iso", f"ARCH={arch}", f"PROFILE={profile}", "WITH_ROOTFS=1",
        "ROOTFS_BUILD=0", f"ROOTFS_IMG={IMAGE}", "CONFIG_BOOT_SELF_TEST=",
        env=env, stdout=subprocess.DEVNULL)
    return ROOT / "build" / arch / profile / f"SpaceKernel-{arch}.iso"


class StalledBoot(Exception):
    """QEMU printed nothing at all: the firmware stalled before the kernel."""


# No output for this long: before the kernel starts it means QEMU/OVMF hung
# (seen occasionally under TCG, sometimes after a few escape sequences), so
# the boot is retried; once the kernel runs it means the benchmark hung.
SILENCE_SECONDS = 90


def boot_once(arch: str, iso: pathlib.Path, cpus: int, memory: int, timeout: float,
              accel: str, log: pathlib.Path) -> dict:
    command = [
        "qemu-system-x86_64", "-machine", "q35", "-m", str(memory), "-smp", str(cpus),
        "-accel", accel, "-display", "none", "-serial", "stdio", "-monitor", "none",
        "-no-reboot", "-drive", f"if=pflash,format=raw,readonly=on,file={FIRMWARE[arch]}",
        "-cdrom", str(iso),
    ]
    if accel == "kvm":
        command[1:1] = ["-cpu", "host"]
    results = {}
    done = False
    started = time.monotonic()
    deadline = started + timeout
    pending = b""
    last_activity = started
    kernel_started = False
    with open(log, "w") as transcript:
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   stdin=subprocess.DEVNULL)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        try:
            # Read with a deadline: a silent QEMU must not block forever.
            while not done:
                now = time.monotonic()
                if now > deadline:
                    break
                if now - last_activity > SILENCE_SECONDS:
                    if not kernel_started:
                        raise StalledBoot()
                    break
                if not selector.select(timeout=0.5):
                    continue
                chunk = process.stdout.read1(4096)
                if not chunk:
                    break
                last_activity = time.monotonic()
                kernel_started = kernel_started or b"SpaceKernel" in chunk or b"BENCH" in chunk
                pending += chunk
                *lines, pending = pending.split(b"\n")
                for raw in lines:
                    line = raw.decode("utf-8", "replace")
                    transcript.write(line + "\n")
                    line = line.strip()
                    match = BENCH_RE.match(line)
                    if match:
                        results[match.group(1)] = (float(match.group(2)), match.group(3))
                        print(line, flush=True)
                    elif line.startswith("BENCH_ERROR") or "panic" in line.lower():
                        print(line, flush=True)
                    if line.startswith("BENCH_DONE"):
                        done = True
                        break
        finally:
            process.kill()
            process.wait()
    if not done:
        raise SystemExit(f"benchmark did not finish; see {log}")
    return results


def boot(arch: str, iso: pathlib.Path, cpus: int, memory: int, timeout: float,
         accel: str, log: pathlib.Path) -> dict:
    for attempt in range(3):
        try:
            return boot_once(arch, iso, cpus, memory, timeout, accel, log)
        except StalledBoot:
            print(f"-- QEMU silent for {SILENCE_SECONDS}s before the kernel started "
                  f"(firmware stall, attempt {attempt + 1}); retrying", flush=True)
    raise SystemExit(f"QEMU stalled before the kernel on every attempt; see {log}")


def compare(current: dict, baseline_path: pathlib.Path) -> None:
    baseline = json.loads(baseline_path.read_text())["results"]
    print(f"\n{'benchmark':30} {'baseline':>14} {'current':>14} {'change':>9}")
    for name, (value, unit) in current.items():
        if name not in baseline:
            continue
        old = baseline[name][0]
        if old == 0:
            continue
        delta = (value - old) / old * 100.0
        better = delta > 0 if unit in HIGHER_IS_BETTER else delta < 0
        mark = "+" if better else "-"
        print(f"{name:30} {old:14.3f} {value:14.3f} {delta:+8.1f}% {mark} {unit}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", default="x86_64", choices=sorted(FIRMWARE))
    parser.add_argument("--profile", default="release")
    parser.add_argument("--cpus", type=int, default=4)
    parser.add_argument("--memory", type=int, default=1024)
    parser.add_argument("--timeout", type=float, default=900)
    parser.add_argument("--accel", default="kvm" if os.path.exists("/dev/kvm") else "tcg")
    parser.add_argument("--json", type=pathlib.Path, help="write results here")
    parser.add_argument("--compare", type=pathlib.Path, help="baseline JSON to diff against")
    parser.add_argument("--no-build", action="store_true", help="reuse the last ISO")
    parser.add_argument("--filter", default="",
                        help="space-separated test names to run (e.g. 'fault hackbench')")
    parser.add_argument("--window", type=float, default=0,
                        help="seconds per test (default: spbench's 1.0)")
    parser.add_argument("--repeat", type=int, default=1,
                        help="boot N times and report the per-benchmark median "
                             "(TCG timing varies by tens of percent between boots)")
    args = parser.parse_args()

    iso = ROOT / "build" / args.arch / args.profile / f"SpaceKernel-{args.arch}.iso"
    if not args.no_build:
        conf = ""
        if args.filter:
            conf += f"filter {args.filter}\n"
        if args.window:
            conf += f"window {args.window}\n"
        build_image(build_binary(), conf)
        iso = build_iso(args.arch, args.profile)
    runs = []
    for index in range(max(1, args.repeat)):
        log = OUT / f"boot-{args.arch}-{index}.log"
        if args.repeat > 1:
            print(f"-- run {index + 1}/{args.repeat}", flush=True)
        runs.append(boot(args.arch, iso, args.cpus, args.memory, args.timeout, args.accel, log))
    results = {}
    for name, (_, unit) in runs[0].items():
        values = sorted(run[name][0] for run in runs if name in run)
        results[name] = (values[len(values) // 2], unit)
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps({
            "arch": args.arch, "cpus": args.cpus, "accel": args.accel,
            "commit": subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT,
                                     capture_output=True, text=True).stdout.strip(),
            "repeat": len(runs),
            "results": results,
        }, indent=2))
    if args.compare:
        compare(results, args.compare)
    return 0


if __name__ == "__main__":
    sys.exit(main())
