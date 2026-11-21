#!/bin/sh
# Stage the Aqua Classic SPUI theme package.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/theme-aqua}

rm -rf "$STAGE"
install -d -m 0755 \
    "$STAGE/usr/bin" \
    "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/doc/theme-aqua" \
    "$STAGE/usr/share/spui/themes/aqua" \
    "$STAGE/usr/share/spui/wallpapers"

install -m 0644 "$ROOT/packages/theme-aqua/theme.toml" \
    "$STAGE/usr/share/spui/themes/aqua/theme.toml"
for svg in \
    close.svg close_hover.svg close_active.svg \
    minimize.svg minimize_hover.svg minimize_active.svg \
    maximize.svg maximize_hover.svg maximize_active.svg \
    restore.svg restore_hover.svg restore_active.svg
do
    install -m 0644 "$ROOT/packages/theme-aqua/$svg" \
        "$STAGE/usr/share/spui/themes/aqua/$svg"
done
install -m 0644 "$ROOT/packages/theme-aqua/aqua-wave.jpg" \
    "$STAGE/usr/share/spui/wallpapers/aqua-wave.jpg"
install -m 0644 "$ROOT/packages/theme-aqua/README.md" \
    "$STAGE/usr/share/doc/theme-aqua/README.md"
install -m 0755 "$ROOT/packages/theme-aqua/spui-theme-aqua" \
    "$STAGE/usr/bin/spui-theme-aqua"
install -m 0644 "$ROOT/packages/theme-aqua/spui-theme-aqua.desktop" \
    "$STAGE/usr/share/applications/spui-theme-aqua.desktop"
install -m 0644 "$ROOT/packages/theme-aqua/.INSTALL" "$STAGE/.INSTALL"

printf 'staged theme-aqua in %s\n' "$STAGE"
