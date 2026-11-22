#!/bin/sh
#       packages/font-hack/build.sh
#       Stage fonts from the pinned Alpine package
#
#       2026/11/22 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/font-hack}
WORK=$ROOT/build/packages/.src-font-hack

rm -rf "$STAGE" "$WORK"
python3 -I "$ROOT/tools/fetch_alpine_package.py" font-hack --dest "$WORK"
rm -rf "$STAGE"
install -d -m 0755 "$STAGE"
cp -a "$WORK/usr" "$STAGE/usr"
rm -rf "$WORK"
install -d -m 0755 "$STAGE/usr/share/licenses/font-hack"
install -m 0644 "$ROOT/packages/font-hack/LICENSE" "$STAGE/usr/share/licenses/font-hack/LICENSE"
if [ -f "$ROOT/packages/font-hack/COPYRIGHT" ]; then
    install -m 0644 "$ROOT/packages/font-hack/COPYRIGHT" "$STAGE/usr/share/licenses/font-hack/COPYRIGHT"
fi
find "$STAGE" -type d -exec chmod 0755 {} +
find "$STAGE" -type f -exec chmod 0644 {} +
printf 'staged %s in %s\n' font-hack "$STAGE"
