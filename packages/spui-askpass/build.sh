#!/bin/sh
#       packages/spui-askpass/build.sh
#       Stage the standalone spui-askpass helper binary
#
#       2026/11/22 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/spui-askpass}
BIN=$ROOT/target/x86_64-unknown-linux-musl/release/spui-askpass

if [ ! -x "$BIN" ]; then
    printf 'error: %s missing; run `make desktop-askpass` first\n' "$BIN" >&2
    exit 1
fi

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin"
install -m 0755 "$BIN" "$STAGE/usr/bin/spui-askpass"
printf 'staged spui-askpass in %s\n' "$STAGE"
