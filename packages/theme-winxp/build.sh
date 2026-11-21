#!/bin/sh
# Stage the Windows XP Luna-inspired SPUI theme package.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/theme-winxp}

rm -rf "$STAGE"
install -d -m 0755 \
    "$STAGE/usr/bin" \
    "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/doc/theme-winxp" \
    "$STAGE/usr/share/spui/themes/winxp" \
    "$STAGE/usr/share/spui/wallpapers"

install -m 0644 "$ROOT/packages/theme-winxp/theme.toml" \
    "$STAGE/usr/share/spui/themes/winxp/theme.toml"
install -m 0644 "$ROOT/packages/theme-winxp/bliss.svg" \
    "$STAGE/usr/share/spui/themes/winxp/bliss.svg"
install -m 0644 "$ROOT/packages/theme-winxp/winxp-bliss.jpg" \
    "$STAGE/usr/share/spui/wallpapers/winxp-bliss.jpg"
install -m 0644 "$ROOT/packages/theme-winxp/README.md" \
    "$STAGE/usr/share/doc/theme-winxp/README.md"
install -m 0755 "$ROOT/packages/theme-winxp/spui-theme-winxp" \
    "$STAGE/usr/bin/spui-theme-winxp"
install -m 0644 "$ROOT/packages/theme-winxp/spui-theme-winxp.desktop" \
    "$STAGE/usr/share/applications/spui-theme-winxp.desktop"

printf 'staged theme-winxp in %s\n' "$STAGE"
