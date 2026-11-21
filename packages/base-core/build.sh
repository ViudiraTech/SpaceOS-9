#!/bin/sh
#       packages/base/build.sh
#       Stage the base runtime package for spk-host pack
#
#       2026/11/23 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/base-core}
SPK_BIN=${SPK_BIN:-$ROOT/target/x86_64-unknown-linux-musl/release/spk}
SANDBOX_BIN=${SANDBOX_BIN:-$ROOT/target/x86_64-unknown-linux-musl/release/spk-sandbox}
BUSYBOX=${BUSYBOX:-$ROOT/build/userland/busybox}
MUSL=${MUSL:-$ROOT/build/userland/musl}
CA_BUNDLE=${CA_BUNDLE:-/etc/ssl/certs/ca-certificates.crt}
SYSTEMG=${SYSTEMG:-$ROOT/build/userland/bin/systemg}

for path in "$SPK_BIN" "$SANDBOX_BIN" "$BUSYBOX" "$MUSL/ld-musl-x86_64.so.1" "$MUSL/libc.so"; do
    if [ ! -e "$path" ]; then
        printf 'error: base dependency missing: %s\n' "$path" >&2
        exit 1
    fi
done
if [ ! -f "$CA_BUNDLE" ]; then
    printf 'error: CA bundle missing: %s\n' "$CA_BUNDLE" >&2
    exit 1
fi

rm -rf "$STAGE"
install -d -m 0755 \
    "$STAGE/bin" \
    "$STAGE/lib" \
    "$STAGE/sbin" \
    "$STAGE/usr/bin" \
    "$STAGE/etc/ssl/certs" \
    "$STAGE/etc/spk/hooks.d"

install -m 0755 "$SPK_BIN" "$STAGE/usr/bin/spk"
install -m 0755 "$SANDBOX_BIN" "$STAGE/usr/bin/spk-sandbox"
install -m 0755 "$BUSYBOX" "$STAGE/bin/busybox"
ln -s busybox "$STAGE/bin/sh"
install -m 0755 "$MUSL/ld-musl-x86_64.so.1" "$STAGE/lib/ld-musl-x86_64.so.1"
ln -s ld-musl-x86_64.so.1 "$STAGE/lib/libc.musl-x86_64.so.1"
install -m 0644 "$CA_BUNDLE" "$STAGE/etc/ssl/certs/ca-certificates.crt"
install -d -m 0755 "$STAGE/usr/share/doc/base-core"
install -m 0644 "$ROOT/packages/base-core/THIRD_PARTY_NOTICES.md" "$STAGE/usr/share/doc/base-core/THIRD_PARTY_NOTICES.md"
install -m 0755 "$ROOT/packages/base-core/spui-theme-kick" "$STAGE/usr/bin/spui-theme-kick"
install -m 0644 "$ROOT/packages/base-core/90-spui-theme-reload.hook" "$STAGE/etc/spk/hooks.d/90-spui-theme-reload.hook"
if [ -f "$SYSTEMG" ]; then
    install -m 0755 "$SYSTEMG" "$STAGE/sbin/systemg"
fi

printf 'staged base in %s\n' "$STAGE"
