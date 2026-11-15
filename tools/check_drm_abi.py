#!/usr/bin/env python3
#
#       tools/check_drm_abi.py
#       Compare the kernel's DRM ioctl tables with the host Linux UAPI
#
#       2026/11/15 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#
# Usage: check_drm_abi.py <drm_abi_probe output>
#
# Every ioctl the probe prints (number, size, direction from the Linux
# headers) must appear with the same size and direction in the core table
# (src/drm/ioctl.rs) or, for driver ioctls, in the virtio-gpu driver table
# (src/drm/virtio/mod.rs). Directions use the Linux encoding: 1 = _IOC_WRITE
# (user to kernel), 2 = _IOC_READ, 3 = both.
#
# As in Linux drm_ioctl(), the kernel may know a longer (newer) struct than
# the headers: shorter user structs are zero-extended and a longer one must
# have a zero tail. So the kernel size must be >= the header size.
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIRECTIONS = {"rw": 3, "READ": 1, "WRITE": 2, "0": 0, "READ | WRITE": 3}


def core_table():
    text = (ROOT / "src/drm/ioctl.rs").read_text()
    numbers = {name: int(value, 16) for name, value in
               re.findall(r"pub const (\w+): u8 = (0x[0-9a-fA-F]+);", text)}
    body = text[text.index("pub fn descriptor"):text.index("pub struct Ioctl")]
    table = {}
    for names, size, direction in re.findall(r"^\s*([A-Z0-9_| ]+?) => \((\d+), ([\w| ]+)\)", body, re.M):
        for name in names.split("|"):
            table[numbers[name.strip()]] = (int(size), DIRECTIONS[direction.strip()], name.strip())
    return table


def virtio_table():
    text = (ROOT / "src/drm/virtio/mod.rs").read_text()
    body = text[text.index("fn ioctl_descriptor"):]
    body = body[:body.index("\n}\n")]
    return {int(nr, 16): (int(size), DIRECTIONS[d], "VIRTGPU")
            for nr, size, d in re.findall(r"(0x[0-9a-f]+) => \((\d+), (\w+)\)", body)}


def main():
    core, driver = core_table(), virtio_table()
    failures = 0
    for line in Path(sys.argv[1]).read_text().splitlines():
        match = re.match(r"0x([0-9a-f]+) => Some\(\((\d+), (\d+)\)\), // (\w+)", line)
        if not match:
            continue
        nr, size, direction, name = int(match[1], 16), int(match[2]), int(match[3]), match[4]
        table = driver if name.startswith("DRM_IOCTL_VIRTGPU_") else core
        got = table.get(nr)
        if got is None:
            print(f"missing: {name} (nr {nr:#04x})")
            failures += 1
        elif got[1] != direction or got[0] < size:
            print(f"mismatch: {name} nr {nr:#04x}: linux ({size}, {direction}), kernel {got[:2]}")
            failures += 1
        elif got[0] > size:
            print(f"note: {name}: kernel struct {got[0]} bytes, headers {size} (shorter user structs are zero-extended)")
    if failures:
        print(f"DRM ABI check: {failures} problem(s)")
        return 1
    print(f"DRM ABI check: {len(core)} core + {len(driver)} virtio-gpu ioctls match Linux UAPI")
    return 0


if __name__ == "__main__":
    sys.exit(main())
