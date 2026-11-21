#!/bin/sh
# Stage the Ambiance SPUI theme package.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/theme-ambiance}

rm -rf "$STAGE"
install -d -m 0755 \
    "$STAGE/usr/bin" \
    "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/doc/theme-ambiance" \
    "$STAGE/usr/share/spui/themes/ambiance" \
    "$STAGE/usr/share/spui/wallpapers"

install -m 0644 "$ROOT/packages/theme-ambiance/theme.toml" \
    "$STAGE/usr/share/spui/themes/ambiance/theme.toml"
for svg in \
    close.svg close_hover.svg close_active.svg \
    minimize.svg minimize_hover.svg minimize_active.svg \
    maximize.svg maximize_hover.svg maximize_active.svg \
    restore.svg restore_hover.svg restore_active.svg
do
    install -m 0644 "$ROOT/packages/theme-ambiance/$svg" \
        "$STAGE/usr/share/spui/themes/ambiance/$svg"
done
install -m 0644 "$ROOT/packages/theme-ambiance/ambiance-wave.jpg" \
    "$STAGE/usr/share/spui/wallpapers/ambiance-wave.jpg"
install -m 0644 "$ROOT/packages/theme-ambiance/README.md" \
    "$STAGE/usr/share/doc/theme-ambiance/README.md"
install -m 0755 "$ROOT/packages/theme-ambiance/spui-theme-ambiance" \
    "$STAGE/usr/bin/spui-theme-ambiance"
install -m 0644 "$ROOT/packages/theme-ambiance/spui-theme-ambiance.desktop" \
    "$STAGE/usr/share/applications/spui-theme-ambiance.desktop"
install -m 0644 "$ROOT/packages/theme-ambiance/.INSTALL" "$STAGE/.INSTALL"

printf 'staged theme-ambiance in %s\n' "$STAGE"
