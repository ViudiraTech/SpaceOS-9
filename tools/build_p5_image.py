#!/usr/bin/env python3
#
#       tools/build_p5_image.py
#       Build the P5 ext4 rootfs, FAT32 ISO rootfs and local-repo layout
#
#       2026/11/23 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Create a reproducible P5 root filesystem and signed local repository."""

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent


def run(*command, **kwargs):
    if len(command) == 1 and isinstance(command[0], (list, tuple)):
        command = tuple(command[0])
    print("+ " + " ".join(str(item) for item in command), flush=True)
    return subprocess.run(command, check=True, **kwargs)


def write_file(path: pathlib.Path, data: str, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(data)
    path.chmod(mode)


def source_date_epoch() -> int:
    value = os.environ.get("SOURCE_DATE_EPOCH")
    if value:
        return int(value)
    try:
        return int(
            subprocess.check_output(
                ["git", "-C", str(ROOT), "log", "-1", "--format=%ct"],
                text=True,
            ).strip()
        )
    except (OSError, ValueError, subprocess.CalledProcessError):
        return 0


def install_base(stage: pathlib.Path, repo: pathlib.Path, spk: pathlib.Path) -> None:
    trust = stage / "etc/spk/trusted.json"
    if not trust.is_file():
        raise SystemExit(f"trust bundle missing: {trust}")
    write_file(
        stage / "etc/spk/setuid.allow",
        "/usr/bin/sudo\n/usr/bin/doas\n",
        0o644,
    )
    run(
        str(spk),
        "--root",
        str(stage),
        "--lang",
        "en",
        "--noconfirm",
        "install",
        "--repo",
        str(repo),
        "base",
        "base-core",
    )


def rootfs_skeleton(stage: pathlib.Path, epoch: int, repo: pathlib.Path, key_id: str) -> None:
    for directory in (
        "bin",
        "sbin",
        "lib",
        "usr/bin",
        "usr/share",
        "etc",
        "etc/spk",
        "etc/ssl/certs",
        "etc/systemg",
        "var/lib/spk",
        "var/lib/sudo",
        "var/cache/spk",
        "var/log",
        "tmp",
        "proc",
        "sys",
        "dev",
        "run",
        "root",
        "home",
        "mnt",
        "opt",
    ):
        (stage / directory).mkdir(parents=True, exist_ok=True)

    write_file(
        stage / "etc/passwd",
        "root:x:0:0:root:/root:/bin/sh\n"
        "test:x:1000:1000:Test User:/home/test:/bin/sh\n",
    )
    write_file(stage / "etc/group", "root:x:0:\ntest:x:1000:\n")
    write_file(stage / "etc/hostname", "spaceos9\n")
    write_file(
        stage / "etc/os-release",
        'NAME="SpaceOS"\nVERSION="9"\nID=spaceos9\nVERSION_ID="9"\n'
        'PRETTY_NAME="SpaceOS 9"\nHOME_URL="https://github.com/ViudiraTech/SpaceOS-9"\n',
    )
    write_file(
        stage / "etc/fstab",
        "rootfs  /      ext4  defaults 0 0\n"
        "proc    /proc  proc  defaults 0 0\n"
        "sysfs   /sys   sysfs defaults 0 0\n"
        "devtmpfs /dev  devtmpfs defaults 0 0\n"
        "tmpfs   /tmp   tmpfs defaults 0 0\n",
    )
    write_file(
        stage / "etc/systemg/console.service",
        "[Unit]\nDescription=Console shell\n\n[Service]\nType=simple\n"
        # Force interactive mode: SpaceKernel's /dev/console is a console
        # inode without a full TTY ioctl surface, so BusyBox otherwise treats
        # the inherited fd as a non-interactive pipe and never prints a prompt.
        "ExecStart=/bin/sh -i\nRestart=always\nRestartSec=1\n",
    )
    write_file(
        stage / "etc/systemg/spk-verify.service",
        "[Unit]\nDescription=Verify embedded package repository\n\n"
        "[Service]\nType=oneshot\n"
        "ExecStart=/usr/bin/spk verify --repo /var/lib/spk/repo/x86_64\n",
    )
    # The key id contains ':', which FAT32 cannot store. Use the JSON bundle
    # fallback for both images; the trust directory remains optional.
    key = pathlib.Path(os.environ["SPK_TRUST_KEY"])
    public = json.loads(key.read_text())
    if "secret_key" in public:
        raise SystemExit(f"refusing to embed a private key: {key}")
    write_file(
        stage / "etc/spk/trusted.json",
        json.dumps({"schema": 1, "keys": [public]}, indent=2) + "\n",
    )
    write_file(
        stage / "var/lib/spk/repos.json",
        json.dumps(
            {
                "schema": 1,
                "repos": [
                    {
                        "name": "local",
                        "arch": "x86_64",
                        "urls": ["/var/lib/spk/repo/x86_64"],
                        "priority": 10,
                        "key_id": key_id,
                        "last_sync": epoch,
                        "enabled": True,
                    }
                ],
            },
            indent=2,
        )
        + "\n",
    )
    target = stage / "var/lib/spk/repo/x86_64"
    target.mkdir(parents=True, exist_ok=True)
    for item in repo.iterdir():
        if item.is_file():
            shutil.copy2(item, target / item.name)


def set_times(root: pathlib.Path, epoch: int) -> None:
    for path in sorted(root.rglob("*"), reverse=True):
        os.utime(path, (epoch, epoch), follow_symlinks=False)
    os.utime(root, (epoch, epoch))


def build_ext4(stage: pathlib.Path, image: pathlib.Path, epoch: int, uuid_seed: str) -> None:
    image.parent.mkdir(parents=True, exist_ok=True)
    if image.exists():
        image.unlink()
    with image.open("wb") as file:
        file.truncate(128 * 1024 * 1024)
    digest = hashlib.sha256(uuid_seed.encode()).hexdigest()
    uuid = str(__import__("uuid").UUID(digest[:32]))
    command = (
        f"chown -R 0:0 {shlex_quote(str(stage))}; "
        f"/usr/sbin/mke2fs -q -F -t ext4 -b 4096 "
        f"-O has_journal,extent,huge_file,flex_bg,metadata_csum,metadata_csum_seed,"
        f"64bit,dir_nlink,extra_isize,^orphan_file,^resize_inode,^dir_index "
        f"-E lazy_itable_init=0,lazy_journal_init=0,hash_seed={uuid} -J size=16 -L SPACEROOT "
        f"-U {uuid} -d {shlex_quote(str(stage))} {shlex_quote(str(image))}"
    )
    run(["fakeroot", "sh", "-c", command])
    run(["/usr/sbin/e2fsck", "-fn", str(image)])


def build_fat(stage: pathlib.Path, image: pathlib.Path, seed: str) -> None:
    image.parent.mkdir(parents=True, exist_ok=True)
    if image.exists():
        image.unlink()
    with image.open("wb") as file:
        file.truncate(128 * 1024 * 1024)
    run(["mkfs.vfat", "-F", "32", "-n", "SPACEROOT", "-i", f"0x{seed[:8]}", str(image)])

    def copy_tree(directory: pathlib.Path, prefix: str) -> None:
        for entry in sorted(directory.iterdir(), key=lambda item: item.name):
            if prefix == "" and entry.name in ("proc", "sys", "dev", "trusted.d"):
                continue
            relative = entry.name if prefix == "" else f"{prefix}/{entry.name}"
            if entry.is_dir():
                run(["mmd", "-i", str(image), f"::{relative}"])
                copy_tree(entry, relative)
            elif entry.is_file():
                run(["mcopy", "-i", str(image), str(entry), f"::{relative}"])

    copy_tree(stage, "")

def shlex_quote(text: str) -> str:
    return "'" + text.replace("'", "'\\''") + "'"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=pathlib.Path, default=ROOT / "build/repo/x86_64")
    parser.add_argument("--stage", type=pathlib.Path, default=ROOT / "build/p5/root")
    parser.add_argument("--ext4", type=pathlib.Path, default=ROOT / "build/p5/rootfs.img")
    parser.add_argument("--fat", type=pathlib.Path, default=ROOT / "build/p5/rootfs.fat")
    parser.add_argument("--spk", type=pathlib.Path, default=ROOT / "target/release/spk")
    parser.add_argument("--trust-key", type=pathlib.Path, default=ROOT / "build/keys/repo.pub")
    parser.add_argument(
        "--driver",
        type=pathlib.Path,
        default=ROOT / "target/x86_64-unknown-linux-musl/release/spk-p5-test",
    )
    arguments = parser.parse_args()
    os.environ["SPK_TRUST_KEY"] = str(arguments.trust_key)
    epoch = source_date_epoch()
    os.environ["SOURCE_DATE_EPOCH"] = str(epoch)
    key = json.loads(arguments.trust_key.read_text())
    key_id = key["key_id"]
    if arguments.stage.exists():
        shutil.rmtree(arguments.stage)
    arguments.stage.mkdir(parents=True)
    rootfs_skeleton(arguments.stage, epoch, arguments.repo, key_id)
    install_base(arguments.stage, arguments.repo, arguments.spk)
    # Build-time metadata embeds wall-clock time and a random transaction id;
    # normalize it so the generated image is reproducible.
    installed = arguments.stage / "var/lib/spk/installed.json"
    if installed.is_file():
        text = installed.read_text()
        import re
        text = re.sub(r'"install_date": \d+', f'"install_date": {epoch}', text)
        text = re.sub(r'"txid": "[0-9a-f]+"', '"txid": "00000000000000000000"', text)
        installed.write_text(text)
    # Build-time audit entries embed wall-clock time; images are generated
    # state, so do not ship the generator's transaction log.
    (arguments.stage / "var/log/spk/audit.log").unlink(missing_ok=True)
    if arguments.driver.is_file():
        driver = arguments.stage / "sbin/spk-p5-test"
        shutil.copy2(arguments.driver, driver)
        driver.chmod(0o755)
    else:
        raise SystemExit(f"P5 test driver missing: {arguments.driver}")
    set_times(arguments.stage, epoch)
    build_ext4(arguments.stage, arguments.ext4, epoch, f"rootfs:{epoch}:{key_id}")
    build_fat(arguments.stage, arguments.fat, key_id.replace(":", ""))
    print(f"ext4 rootfs: {arguments.ext4}")
    print(f"FAT32 rootfs: {arguments.fat}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
