#!/bin/sh
#       tools/build_packages.sh
#       Reproducibly build the SpaceOS base, hello and sudo packages
#
#       2026/11/23 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BUILD=${BUILD:-$ROOT/build}
REPO=${REPO:-$ROOT/build/repo/x86_64}
ARCH=${ARCH:-x86_64}
HOST_BIN=${HOST_BIN:-$ROOT/target/release/spk-host}
MUSL_TARGET=x86_64-unknown-linux-musl
TOOLCHAIN=${RUSTUP_TOOLCHAIN:-nightly-2026-05-24}

if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH=$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || echo 0)
fi
export SOURCE_DATE_EPOCH
export RUSTUP_TOOLCHAIN="$TOOLCHAIN"
export CC_x86_64_unknown_linux_musl=${CC_x86_64_unknown_linux_musl:-clang}
export AR_x86_64_unknown_linux_musl=${AR_x86_64_unknown_linux_musl:-ar}

if [ ! -f "$ROOT/build/userland/libsplibc.a" ]; then
    "$ROOT/tools/build_userland.sh"
fi
if [ ! -x "$ROOT/build/userland/busybox" ]; then
    "$ROOT/tools/build_busybox.sh"
fi

( cd /tmp && cargo build \
    --manifest-path "$ROOT/user/spk/Cargo.toml" \
    -p spk --bins --release --target "$MUSL_TARGET" \
    --target-dir "$ROOT/target" ) 
( cd /tmp && cargo build \
    --manifest-path "$ROOT/user/spk/Cargo.toml" \
    -p spk --release --target-dir "$ROOT/target" )
( cd /tmp && cargo build \
    --manifest-path "$ROOT/user/spk/Cargo.toml" \
    -p spk-host --release --target-dir "$ROOT/target" )
( cd /tmp && cargo build \
    --manifest-path "$ROOT/user/sudo/Cargo.toml" \
    --bins --release --target "$MUSL_TARGET" \
    --target-dir "$ROOT/target" )

"$ROOT/packages/hello/build.sh" 0.1.0 "$BUILD/packages/hello-0.1.0"
"$ROOT/packages/hello/build.sh" 0.2.0 "$BUILD/packages/hello-0.2.0"
"$ROOT/packages/sudo/build.sh" "$BUILD/packages/sudo"
"$ROOT/packages/theme-winxp/build.sh" "$BUILD/packages/theme-winxp"
"$ROOT/packages/base/build.sh" "$BUILD/packages/base"

rm -rf "$BUILD/packages/meta" "$REPO"
install -d -m 0755 "$BUILD/packages/meta" "$REPO"

write_buildinfo() {
    name=$1
    version=$2
    directory=$3
    {
        printf 'format = 1\n'
        printf 'package = "%s"\n' "$name"
        printf 'version = "%s"\n' "$version"
        printf 'arch = "%s"\n' "$ARCH"
        printf 'abi = "spaceos-1"\n'
        printf 'source_date_epoch = %s\n' "$SOURCE_DATE_EPOCH"
        printf 'builder = "SpaceOS reproducible packages"\n'
    } > "$directory/.BUILDINFO"
    chmod 0644 "$directory/.BUILDINFO"
}

pack_one() {
    name=$1
    version=$2
    manifest=$3
    staging=$4
    meta=$BUILD/packages/meta/$name
    install -d -m 0755 "$meta"
    cp "$manifest" "$meta/manifest.toml"
    write_buildinfo "$name" "$version" "$meta"
    "$HOST_BIN" pack \
        --manifest "$meta/manifest.toml" \
        --root "$staging" \
        --output "$REPO" \
        --level 19
}

pack_one hello 0.1.0-1 "$ROOT/packages/hello/manifest.toml" "$BUILD/packages/hello-0.1.0"
pack_one hello 0.2.0-1 "$ROOT/packages/hello/manifest-0.2.0.toml" "$BUILD/packages/hello-0.2.0"
pack_one sudo 0.1.0-2 "$ROOT/packages/sudo/manifest.toml" "$BUILD/packages/sudo"
pack_one base 1.0.0-4 "$ROOT/packages/base/manifest.toml" "$BUILD/packages/base"
pack_one theme-winxp 1.0.0-1 "$ROOT/packages/theme-winxp/manifest.toml" "$BUILD/packages/theme-winxp"

printf 'reproducible packages in %s\n' "$REPO"
