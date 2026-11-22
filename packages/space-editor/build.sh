#!/bin/sh
#       packages/space-editor/build.sh
#       Stage the SpaceEditor binary and desktop entry
#
#       2026/11/22 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/space-editor}
BIN=$ROOT/target/x86_64-unknown-linux-musl/release/space-editor

if [ ! -x "$BIN" ]; then
    printf 'error: %s missing; run `make desktop-editor` first\n' "$BIN" >&2
    exit 1
fi

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin" "$STAGE/usr/share/applications"
install -m 0755 "$BIN" "$STAGE/usr/bin/space-editor"
install -m 0644 "$ROOT/packages/space-editor/space-editor.desktop" "$STAGE/usr/share/applications/space-editor.desktop"
printf 'staged space-editor in %s\n' "$STAGE"
