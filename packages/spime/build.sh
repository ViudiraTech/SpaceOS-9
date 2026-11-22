#!/bin/sh
#       packages/spime/build.sh
#       Stage the SPIME input method daemon, its dictionary and autostart entry
#
#       2026/11/22 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/spime}
BIN=$ROOT/target/x86_64-unknown-linux-musl/release/spimed
DICT=$ROOT/build/spime

if [ ! -x "$BIN" ]; then
    printf 'error: %s missing; run `make desktop-spime` first\n' "$BIN" >&2
    exit 1
fi
if [ ! -f "$DICT/spime.dict" ] || [ ! -f "$DICT/spime.lm" ]; then
    python3 -I "$ROOT/tools/build_spime_dict.py" --out-dir "$DICT"
fi

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin" "$STAGE/usr/share/spime" "$STAGE/etc/xdg/autostart"
install -m 0755 "$BIN" "$STAGE/usr/bin/spimed"
install -m 0644 "$DICT/spime.dict" "$DICT/spime.lm" "$STAGE/usr/share/spime/"
if [ -f "$DICT/NOTICE.txt" ]; then
    install -m 0644 "$DICT/NOTICE.txt" "$STAGE/usr/share/spime/NOTICE.txt"
fi
install -m 0644 "$ROOT/packages/spime/spimed.desktop" "$STAGE/etc/xdg/autostart/spimed.desktop"
printf 'staged spime in %s\n' "$STAGE"
