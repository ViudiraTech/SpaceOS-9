#!/usr/bin/env python3
#
#       tools/ensure_musl_sysroot.py
#       Prepare Alpine musl C/C++ static sysroot in build/musl-sysroot
#
#       2026/11/14 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Download and unpack Alpine musl + libstdc++ headers/static archives for C++ FFI."""

import io
import os
import pathlib
import shutil
import sys
import tarfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
SYSROOT = ROOT / "build" / "musl-sysroot"
STAMP = SYSROOT / ".stamp"

SYSROOT_PACKAGES = (
    "musl",
    "musl-dev",
    "libgcc",
    "libstdc++",
    "libstdc++-dev",
    "fortify-headers",
    "linux-headers",
)


def extract_apk(apk_path: pathlib.Path, dest: pathlib.Path, safe_member, split_gzip_members) -> None:
    payload = apk_path.read_bytes()
    data = split_gzip_members(payload)[-1]
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive:
            if not safe_member(member):
                continue
            target = dest / member.name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                os.chmod(target, member.mode | 0o700)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.exists() or target.is_symlink():
                if target.is_dir() and not target.is_symlink():
                    continue
                target.unlink()
            if member.issym():
                os.symlink(member.linkname, target)
            elif member.islnk():
                os.link(dest / member.linkname, target)
            elif member.isfile():
                with archive.extractfile(member) as source, open(target, "wb") as out:
                    shutil.copyfileobj(source, out)
                os.chmod(target, (member.mode & 0o7777) | 0o600)


def main() -> int:
    expected = ",".join(SYSROOT_PACKAGES)
    if STAMP.exists() and STAMP.read_text().strip() == expected and (SYSROOT / "usr" / "lib" / "libstdc++.a").exists():
        print(f"musl sysroot ready: {SYSROOT}")
        return 0

    sys.path.insert(0, str(ROOT / "tools"))
    import build_desktop_rootfs as bdr

    packages, _ = bdr.load_indexes()
    if SYSROOT.exists():
        shutil.rmtree(SYSROOT)
    SYSROOT.mkdir(parents=True, exist_ok=True)

    for pkg in SYSROOT_PACKAGES:
        record = packages.get(pkg)
        if not record:
            raise SystemExit(f"missing Alpine package in index: {pkg}")
        apk = bdr.download(record)
        extract_apk(apk, SYSROOT, bdr.safe_member, bdr.split_gzip_members)

    STAMP.write_text(expected + "\n")
    print(f"musl sysroot unpacked: {SYSROOT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
