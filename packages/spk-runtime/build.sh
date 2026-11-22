#!/bin/sh
# Stage the spk runtime binaries for desktop images.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/spk-runtime}
BIN=${BIN:-$ROOT/target/x86_64-unknown-linux-musl/release}

for name in spk spk-sandbox; do
    if [ ! -x "$BIN/$name" ]; then
        printf 'error: %s missing; build the musl spk binaries first\n' "$BIN/$name" >&2
        exit 1
    fi
done

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin" "$STAGE/etc/spk/hooks.d"
install -m 0755 "$BIN/spk" "$STAGE/usr/bin/spk"
install -m 0755 "$BIN/spk-sandbox" "$STAGE/usr/bin/spk-sandbox"
install -m 0755 "$ROOT/packages/base-core/spui-theme-kick" "$STAGE/usr/bin/spui-theme-kick"
install -m 0644 "$ROOT/packages/base-core/90-spui-theme-reload.hook" "$STAGE/etc/spk/hooks.d/90-spui-theme-reload.hook"
install -m 0755 "$ROOT/packages/base-core/spui-apps-kick" "$STAGE/usr/bin/spui-apps-kick"
install -m 0644 "$ROOT/packages/base-core/90-spui-apps-reload.hook" "$STAGE/etc/spk/hooks.d/90-spui-apps-reload.hook"

printf 'staged spk-runtime in %s\n' "$STAGE"
