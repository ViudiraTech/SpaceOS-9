#!/usr/bin/env python3
#
#       tools/qemu_test.py
#       Boot the generated ISO and require the kernel's self-test marker
#
#       2026/9/30 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Boot the generated ISO and require the kernel's self-test marker."""

import selectors
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time


def make_usb_stick(path):
    """16 MiB disk: MBR with one FAT32 (LBA) partition at 1 MiB holding HELLO.TXT."""
    size, start = 16 << 20, 2048
    with open(path, "wb") as disk:
        disk.truncate(size)
        sectors = size // 512 - start
        entry = bytes([0x00, 0, 0, 0, 0x0c, 0, 0, 0]) + start.to_bytes(4, "little") \
            + sectors.to_bytes(4, "little")
        disk.seek(446)
        disk.write(entry)
        disk.seek(510)
        disk.write(b"\x55\xaa")
    subprocess.run(["mkfs.vfat", "-F", "32", "--offset", str(start), "-n", "SPKUSB", path,
                    str(sectors // 2)], check=True, stdout=subprocess.DEVNULL)
    hello = path + ".txt"
    with open(hello, "w") as file:
        file.write("SPACEOS USB OK\n")
    subprocess.run(["mcopy", "-i", f"{path}@@{start * 512}", hello, "::/HELLO.TXT"], check=True)


def write_result(path, result):
    """Persist a machine-readable result; the Makefile matrix consumes it."""
    if not path:
        return
    parent = os.path.dirname(os.path.abspath(path))
    os.makedirs(parent, exist_ok=True)
    with open(path, "w", encoding="utf-8") as stream:
        json.dump(result, stream, indent=2, sort_keys=True)
        stream.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("arch", choices=["x86_64", "aarch64", "riscv64"])
    for argument in ["iso", "firmware", "cpus", "memory_mib"]:
        parser.add_argument(argument)
    parser.add_argument("--accel", choices=["tcg", "kvm"])
    parser.add_argument("--stream", action="store_true")
    parser.add_argument("--gpu", choices=["2d", "virgl", "venus"])
    parser.add_argument("--simplefb", action="store_true",
                        help="no virtio-gpu: test simpledrm on the firmware framebuffer "
                             "(ramfb on aarch64/riscv64)")
    parser.add_argument("--cpu")
    parser.add_argument("--machine")
    parser.add_argument("--nic", help="QEMU -nic argument, e.g. user,model=e1000")
    parser.add_argument("--sound", choices=["sb16", "hda", "both"])
    parser.add_argument("--usb", action="store_true",
                        help="attach qemu-xhci with a keyboard, a mouse and a FAT32 USB stick")
    parser.add_argument("--timeout", type=float,
                        help="seconds (default: 300 x86_64, 600 aarch64, 900 riscv64 under TCG)")
    parser.add_argument("--online-cpus", type=int)
    parser.add_argument("--require", action="append", default=[])
    parser.add_argument("--result", help="Write a machine-readable JSON result")
    parser.add_argument("--verbose", action="store_true", help="Print full log on success")
    parser.add_argument("--gdb", type=int, metavar="PORT",
                        help="Expose the QEMU gdbstub on this TCP port (build with "
                             "CONFIG_KASLR=n so kernel symbols match)")
    args = parser.parse_args()
    arch, iso, firmware, cpus, memory_mib = args.arch, args.iso, args.firmware, args.cpus, args.memory_mib
    binary = {
        "x86_64": "qemu-system-x86_64",
        "aarch64": "qemu-system-aarch64",
        "riscv64": "qemu-system-riscv64",
    }[arch]
    machine = args.machine or ("q35" if arch == "x86_64" else "virt")
    if args.timeout is None:
        args.timeout = {"x86_64": 300, "aarch64": 600, "riscv64": 900}[arch]
    command = [binary, "-machine", machine, "-m", memory_mib, "-smp", cpus,
               "-display", "none", "-serial", "stdio", "-monitor", "none", "-no-reboot",
               "-drive", f"if=pflash,format=raw,readonly=on,file={firmware}"]
    if arch == "riscv64":
        # The RISC-V virt machine has no controller for -cdrom's default
        # interface in some QEMU releases (8.x), so EDK2 finds no boot device.
        # The ISO is an isohybrid GPT image, which EDK2 boots as a disk.
        command += ["-drive", f"file={iso},if=none,id=bootiso,format=raw,readonly=on",
                    "-device", "virtio-blk-pci,drive=bootiso"]
    else:
        command += ["-cdrom", iso]
    if args.gpu == "2d":
        command += ["-device", "virtio-gpu-pci"]
    elif args.gpu in ("virgl", "venus"):
        command[command.index("-display") + 1] = "egl-headless,rendernode=/dev/dri/renderD128"
        device = "virtio-gpu-gl-pci"
        if args.gpu == "venus":
            device += ",hostmem=256M,blob=true,venus=true"
            command += ["-object", f"memory-backend-memfd,id=mem,size={memory_mib}M,share=on", "-numa", "node,memdev=mem"]
        command += ["-device", device]
    if args.simplefb:
        if args.gpu:
            parser.error("--simplefb needs the firmware framebuffer; drop --gpu")
        if arch != "x86_64":
            command += ["-device", "ramfb"]
        args.require.append("DRM_SIMPLEFB_OK")
    cpu = args.cpu or ("max" if arch == "aarch64" else None)
    if cpu:
        command += ["-cpu", cpu]
    if args.accel:
        command += ["-accel", args.accel]
    if args.gdb:
        command += ["-gdb", f"tcp:127.0.0.1:{args.gdb}"]
    if args.nic:
        command += ["-nic", args.nic]
    if arch == "x86_64" and args.sound:
        command += ["-audiodev", "none,id=snd0"]
    if arch == "x86_64" and args.sound in ("hda", "both"):
        command += ["-device", "intel-hda", "-device", "hda-duplex,audiodev=snd0"]
    if arch == "x86_64" and args.sound in ("sb16", "both"):
        command += ["-device", "sb16,audiodev=snd0"]

    temp_dir = tempfile.TemporaryDirectory(prefix="spacekernel_disks_")
    if args.usb:
        stick = os.path.join(temp_dir.name, "usbstick.img")
        make_usb_stick(stick)
        command += ["-device", "qemu-xhci,id=xhci",
                    "-device", "usb-kbd,bus=xhci.0",
                    "-device", "usb-mouse,bus=xhci.0",
                    "-drive", f"file={stick},if=none,id=usbstick,format=raw",
                    "-device", "usb-storage,bus=xhci.0,drive=usbstick"]
        for marker in ("USB_HID_OK keyboard", "USB_HID_OK mouse", "USB_MSC_OK",
                       "USB_MSC_FS_OK"):
            args.require.append(marker)
    nvme_img = os.path.join(temp_dir.name, "nvme.img")
    sata_img = os.path.join(temp_dir.name, "sata.img")
    for p in (nvme_img, sata_img):
        with open(p, "wb") as f:
            f.truncate(4 * 1024 * 1024)

    command += [
        "-drive", f"file={nvme_img},if=none,id=nvme0,format=raw",
        "-device", "nvme,serial=SKNVME01,drive=nvme0",
        "-device", "ich9-ahci,id=ahci0",
        "-drive", f"file={sata_img},if=none,id=sata0,format=raw",
        "-device", "ide-hd,drive=sata0,bus=ahci0.0",
    ]
    if arch == "x86_64":
        ide_img = os.path.join(temp_dir.name, "ide.img")
        with open(ide_img, "wb") as f:
            f.truncate(4 * 1024 * 1024)
        command += [
            "-device", "piix3-ide,id=ide0",
            "-drive", f"file={ide_img},if=none,id=pata0,format=raw",
            "-device", "ide-hd,drive=pata0,bus=ide0.0",
        ]

    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    output = bytearray()
    deadline = time.monotonic() + args.timeout
    if args.online_cpus is not None:
        args.require.append(f"SMP: {args.online_cpus}/{args.online_cpus} CPUs online")
    result = {
        "arch": arch,
        "gpu": args.gpu or "none",
        "status": "fail",
        "required": list(args.require),
        "missing": [],
        "selftest_ok": False,
        "boot_ok": False,
        "summary": "",
        "exit_code": None,
    }

    def summarize(decoded):
        for line in decoded.splitlines():
            if "SELFTEST_OK" in line:
                return line.strip()
        for line in decoded.splitlines():
            if "BOOT_OK" in line:
                return line.strip()
        return ""

    try:
        while time.monotonic() < deadline:
            for key, _ in selector.select(timeout=0.5):
                chunk = key.fileobj.read1(4096)
                if chunk:
                    output.extend(chunk)
                    if args.stream:
                        print(chunk.decode(errors="replace"), end="", flush=True)
                    if b"KERNEL EMERGENCY" in output:
                        deadline = min(deadline, time.monotonic() + 2)
                    if b"BOOT_OK" in output and all(marker.encode() in output for marker in args.require):
                        decoded = output.decode(errors="replace")
                        result.update(
                            status="pass",
                            selftest_ok="SELFTEST_OK" in decoded,
                            boot_ok="BOOT_OK" in decoded,
                            summary=summarize(decoded),
                            exit_code=0,
                        )
                        write_result(args.result, result)
                        if not args.stream:
                            if args.verbose:
                                print(decoded)
                            else:
                                print(f"PASS: {arch} ({result['summary']})")
                        return 0
            if process.poll() is not None:
                break
        decoded = output.decode(errors="replace")
        print(decoded, file=sys.stderr)
        missing = [m for m in args.require if m.encode() not in output]
        result.update(
            missing=missing,
            selftest_ok="SELFTEST_OK" in decoded,
            boot_ok="BOOT_OK" in decoded,
            summary=summarize(decoded),
            exit_code=process.poll(),
        )
        write_result(args.result, result)
        if missing:
            print(f"QEMU boot test failed on {arch}: missing required markers {missing}", file=sys.stderr)
        else:
            print(f"QEMU boot test failed on {arch}: timeout or exit before BOOT_OK", file=sys.stderr)
        return 1
    finally:
        selector.close()
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


if __name__ == "__main__":
    sys.exit(main())
