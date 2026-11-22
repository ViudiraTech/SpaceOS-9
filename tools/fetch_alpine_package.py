#!/usr/bin/env python3
"""Extract an Alpine package into a staging directory.

Font packages use this to pull their files from the same pinned Alpine
release the desktop image already installs Noto and DejaVu from, so the bytes
are reproducible and checksum-verified instead of hand-copied.

Usage: tools/fetch_alpine_package.py font-hack --dest <dir> [--prefix usr/share/fonts]
"""

from __future__ import annotations

import argparse
import io
import pathlib
import sys
import tarfile

TOOLS = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))
import build_desktop_rootfs as builder  # noqa: E402  (guarded by __main__)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("name", help="Alpine package name, e.g. font-hack")
    parser.add_argument("--dest", required=True, help="directory to extract into")
    parser.add_argument(
        "--prefix",
        default="usr/share/fonts",
        help="only extract members under this prefix",
    )
    args = parser.parse_args()

    packages, _ = builder.load_indexes()
    record = packages.get(args.name)
    if record is None:
        print(f"{args.name}: not found in the Alpine index", file=sys.stderr)
        return 1
    apk = builder.download(record)
    data = builder.split_gzip_members(apk.read_bytes())[-1]

    dest = pathlib.Path(args.dest)
    dest.mkdir(parents=True, exist_ok=True)
    extracted = 0
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive.getmembers():
            if not member.name.startswith(args.prefix):
                continue
            if not (member.isfile() or member.isdir() or member.issym()):
                continue
            archive.extract(member, dest, filter="data")
            extracted += 1
    print(f"{args.name} {record.get('V', '')}: {extracted} members -> {dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
