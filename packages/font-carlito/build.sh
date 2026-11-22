#!/bin/sh
#       packages/font-carlito/build.sh
#       Stage fonts from the pinned Alpine package
#
#       2026/11/22 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/font-carlito}
WORK=$ROOT/build/packages/.src-font-carlito

rm -rf "$STAGE" "$WORK"
python3 -I "$ROOT/tools/fetch_alpine_package.py" font-carlito --dest "$WORK"
rm -rf "$STAGE"
install -d -m 0755 "$STAGE"
cp -a "$WORK/usr" "$STAGE/usr"
rm -rf "$WORK"
install -d -m 0755 "$STAGE/usr/share/licenses/font-carlito"
install -m 0644 "$ROOT/packages/font-carlito/LICENSE" "$STAGE/usr/share/licenses/font-carlito/LICENSE"
if [ -f "$ROOT/packages/font-carlito/COPYRIGHT" ]; then
    install -m 0644 "$ROOT/packages/font-carlito/COPYRIGHT" "$STAGE/usr/share/licenses/font-carlito/COPYRIGHT"
fi
find "$STAGE" -type d -exec chmod 0755 {} +
find "$STAGE" -type f -exec chmod 0644 {} +
printf 'staged %s in %s\n' font-carlito "$STAGE"
