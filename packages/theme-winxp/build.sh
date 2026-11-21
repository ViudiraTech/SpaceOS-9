#!/bin/sh
# Stage the XP Luna SPUI theme package.
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
for svg in \
    close.svg close_hover.svg close_active.svg \
    minimize.svg minimize_hover.svg minimize_active.svg \
    maximize.svg maximize_hover.svg maximize_active.svg \
    restore.svg restore_hover.svg restore_active.svg
do
    install -m 0644 "$ROOT/packages/theme-winxp/$svg" \
        "$STAGE/usr/share/spui/themes/winxp/$svg"
done
install -m 0644 "$ROOT/packages/theme-winxp/winxp-bliss.jpg" \
    "$STAGE/usr/share/spui/wallpapers/winxp-bliss.jpg"
install -m 0644 "$ROOT/packages/theme-winxp/README.md" \
    "$STAGE/usr/share/doc/theme-winxp/README.md"
install -m 0755 "$ROOT/packages/theme-winxp/spui-theme-winxp" \
    "$STAGE/usr/bin/spui-theme-winxp"
install -m 0644 "$ROOT/packages/theme-winxp/spui-theme-winxp.desktop" \
    "$STAGE/usr/share/applications/spui-theme-winxp.desktop"
install -m 0644 "$ROOT/packages/theme-winxp/.INSTALL" "$STAGE/.INSTALL"

printf 'staged theme-winxp in %s\n' "$STAGE"
