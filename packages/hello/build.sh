#!/bin/sh
#       packages/hello/build.sh
#       Stage the splibc hello binary for spk-host pack
#
#       2026/11/20 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
SOURCE=${HELLO_BIN:-$ROOT/build/userland/bin/hello}
STAGE=${1:-$ROOT/build/packages/hello}

if [ ! -x "$SOURCE" ]; then
    printf 'error: %s not found; run tools/build_userland.sh first\n' "$SOURCE" >&2
    exit 1
fi

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin"
install -m 0755 "$SOURCE" "$STAGE/usr/bin/hello"
printf 'staged hello in %s\n' "$STAGE"
