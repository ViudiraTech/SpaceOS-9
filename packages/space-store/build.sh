#!/bin/sh
#       packages/space-store/build.sh
#       Stage the graphical store binary
#
#       2026/11/22 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/space-store}
BIN=$ROOT/target/x86_64-unknown-linux-musl/release/space-store

if [ ! -x "$BIN" ]; then
    printf 'error: %s missing; run `make desktop-store` first\n' "$BIN" >&2
    exit 1
fi

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin"
install -m 0755 "$BIN" "$STAGE/usr/bin/space-store"
printf 'staged space-store in %s\n' "$STAGE"
