#!/bin/sh
# Stage the SpaceOS base meta package.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/base}

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/share/doc/base"
install -m 0644 "$ROOT/packages/base/README.md" "$STAGE/usr/share/doc/base/README.md"

printf 'staged base meta in %s\n' "$STAGE"
