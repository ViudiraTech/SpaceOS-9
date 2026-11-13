#!/usr/bin/env python3
#
#       tools/build_rootfs.py
#       Build the FAT32 root filesystem image for SpaceOS-9
#
#       2026/10/25 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Assemble build/rootfs/fat32.img from the static and dynamic userland."""

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "build" / "rootfs"
IMAGE = BUILD / "fat32.img"
USERLAND = ROOT / "build" / "userland" / "bin"
BUSYBOX = ROOT / "build" / "userland" / "busybox"
MUSL = ROOT / "build" / "userland" / "musl"

SERVICES = {
    "console.service": """[Unit]
Description=Console shell

[Service]
Type=simple
ExecStart=/bin/busybox sh -c "echo BUSYBOX_READY; exec /bin/busybox sh"
Restart=always
RestartSec=1
""",
}

SELFTEST_SERVICES = {
    "hello.service": """[Unit]
Description=Hello userland service

[Service]
Type=oneshot
ExecStart=/bin/hello
""",
    "selftest.service": """[Unit]
Description=Shell pipeline and redirection self-test
After=hello.service

[Service]
Type=oneshot
ExecStart=/bin/sh -c "echo SH_OK; echo pipeline-ok | cat; echo REDIR_OK > /tmp/sh_test; cat /tmp/sh_test"
""",
    "busybox-check.service": """[Unit]
Description=BusyBox dynamic-link userland check
After=selftest.service

[Service]
Type=oneshot
ExecStart=/bin/busybox sh -c "echo BUSYBOX_OK; uname -a; id; nproc; ls /bin; echo BUSYBOX_CMD_OK"
""",
}


def run(*command: str) -> None:
    subprocess.run(command, check=True)


def main() -> int:
    subprocess.run([str(ROOT / "tools" / "build_userland.sh")], check=True)
    subprocess.run([str(ROOT / "tools" / "build_busybox.sh")], check=True)
    BUILD.mkdir(parents=True, exist_ok=True)
    if IMAGE.exists():
        IMAGE.unlink()
    with open(IMAGE, "wb") as image:
        image.truncate(64 * 1024 * 1024)
    run("mkfs.vfat", "-F", "32", "-n", "SPACEROOT", str(IMAGE))
    for directory in ("/sbin", "/bin", "/lib", "/lib/modules", "/etc", "/etc/systemg",
                      "/tmp", "/dev", "/proc", "/sys", "/run", "/root", "/usr",
                      "/usr/share", "/usr/share/music"):
        run("mmd", "-i", str(IMAGE), f"::{directory}")

    for name in ("hello", "cat", "echo", "sh", "fastfetch", "systemctl", "systemg-analyze",
                 "mp3play"):
        source = USERLAND / name
        if source.exists():
            run("mcopy", "-i", str(IMAGE), str(source), f"::/bin/{name}")
    systemg = USERLAND / "systemg"
    if systemg.exists():
        run("mcopy", "-i", str(IMAGE), str(systemg), "::/sbin/systemg")

    staging = BUILD / "services"
    staging.mkdir(exist_ok=True)
    module_dir = ROOT / "build" / "modules" / "x86_64"
    module_names = []
    if module_dir.is_dir():
        for ko in sorted(module_dir.glob("*.ko")):
            run("mcopy", "-i", str(IMAGE), str(ko), f"::/lib/modules/{ko.name}")
            module_names.append(ko.stem)
    modules_list = staging / "modules"
    modules_list.write_text("".join(f"{name}\n" for name in module_names))
    run("mcopy", "-i", str(IMAGE), str(modules_list), "::/etc/modules")

    music = ROOT / "user" / "assets" / "music" / "spaceos-theme.mp3"
    if music.exists():
        run("mcopy", "-i", str(IMAGE), str(music), "::/usr/share/music/spaceos-theme.mp3")
    run("mcopy", "-i", str(IMAGE), str(BUSYBOX), "::/bin/busybox")
    run("mcopy", "-i", str(IMAGE), str(MUSL / "ld-musl-x86_64.so.1"),
        "::/lib/ld-musl-x86_64.so.1")
    run("mcopy", "-i", str(IMAGE), str(MUSL / "libc.so"), "::/lib/libc.so")

    services = dict(SERVICES)
    if os.environ.get("ROOTFS_SELFTEST") == "1":
        services.update(SELFTEST_SERVICES)
    for name, content in services.items():
        if name.startswith("console") and not (USERLAND / "sh").exists():
            continue
        path = staging / name
        path.write_text(content)
        run("mcopy", "-i", str(IMAGE), str(path), f"::/etc/systemg/{name}")

    motd = staging / "motd"
    motd.write_text("SpaceOS-9 FAT32 root filesystem\n")
    run("mcopy", "-i", str(IMAGE), str(motd), "::/etc/motd")
    hostname = staging / "hostname"
    hostname.write_text("spaceos9\n")
    run("mcopy", "-i", str(IMAGE), str(hostname), "::/etc/hostname")

    extra_files = {
        "passwd": "root:x:0:0:root:/root:/bin/sh\n",
        "group": "root:x:0:\n",
        "profile": "export PATH=/bin:/sbin\nexport HOME=/root\nexport PS1='\\u@\\h:\\w\\$ '\n",
        "fstab": "rootfs  /      vfat  defaults 0 0\nproc    /proc  proc  defaults 0 0\nsysfs   /sys   sysfs defaults 0 0\ndevtmpfs /dev  devtmpfs defaults 0 0\ntmpfs   /tmp   tmpfs defaults 0 0\n",
        "os-release": "NAME=\"SpaceOS\"\nVERSION=\"9\"\nID=spaceos9\nVERSION_ID=\"9\"\nPRETTY_NAME=\"SpaceOS 9\"\nHOME_URL=\"https://github.com/ViudiraTech/SpaceOS-9\"\n",
        "lsb-release": "DISTRIB_ID=SpaceOS\nDISTRIB_RELEASE=9\nDISTRIB_DESCRIPTION=\"SpaceOS 9\"\n",
    }
    for name, content in extra_files.items():
        path = staging / name
        path.write_text(content)
        run("mcopy", "-i", str(IMAGE), str(path), f"::/etc/{name}")

    print(f"rootfs image ready: {IMAGE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
