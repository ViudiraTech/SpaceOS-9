#!/bin/sh
#
#       tools/build_busybox.sh
#       Build a dynamically linked BusyBox against a pinned musl toolchain
#
#       2026/10/25 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MUSL_VERSION=1.2.5
BUSYBOX_VERSION=1.36.1
MUSL_SHA256=a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4
BUSYBOX_SHA256=b8cc24c9574d809e7279c3be349795c5d5ceb6fdf19ca709f80cde50e47de314
DOWNLOADS="$ROOT/build/downloads"
TOOLCHAIN="$ROOT/build/toolchain"
MUSL="$TOOLCHAIN/musl"
MUSL_SRC="$TOOLCHAIN/musl-$MUSL_VERSION"
BUSYBOX_SRC="$TOOLCHAIN/busybox-$BUSYBOX_VERSION"
OUTPUT="$ROOT/build/userland"
JOBS=${JOBS:-$(nproc)}

mkdir -p "$DOWNLOADS" "$TOOLCHAIN" "$OUTPUT/musl"

fetch()
{
    url=$1
    checksum=$2
    destination=$3
    if [ ! -f "$destination" ]; then
        curl -fL --retry 3 -o "$destination" "$url"
    fi
    printf '%s  %s\n' "$checksum" "$destination" | sha256sum -c -
}

fetch "https://musl.libc.org/releases/musl-$MUSL_VERSION.tar.gz" \
    "$MUSL_SHA256" "$DOWNLOADS/musl-$MUSL_VERSION.tar.gz"
fetch "https://busybox.net/downloads/busybox-$BUSYBOX_VERSION.tar.bz2" \
    "$BUSYBOX_SHA256" "$DOWNLOADS/busybox-$BUSYBOX_VERSION.tar.bz2"

if [ ! -x "$MUSL/bin/musl-gcc" ] || [ "${FORCE:-0}" = 1 ]; then
    rm -rf "$MUSL_SRC" "$MUSL"
    tar -xzf "$DOWNLOADS/musl-$MUSL_VERSION.tar.gz" -C "$TOOLCHAIN"
    (
        cd "$MUSL_SRC"
        ./configure --prefix="$MUSL" --syslibdir=/lib
        make -j"$JOBS"
        make install-headers install-tools
        mkdir -p "$MUSL/lib"
        cp lib/libc.so lib/libc.a lib/libm.a lib/libpthread.a lib/libdl.a \
            lib/librt.a lib/libutil.a lib/libresolv.a lib/libcrypt.a \
            lib/libxnet.a "$MUSL/lib/"
        cp lib/crt1.o lib/crti.o lib/crtn.o lib/Scrt1.o lib/rcrt1.o "$MUSL/lib/"
        cp lib/musl-gcc.specs "$MUSL/lib/"
        ln -sf libc.so "$MUSL/lib/ld-musl-x86_64.so.1"
    )
fi

if [ ! -x "$OUTPUT/busybox" ] || [ "${FORCE:-0}" = 1 ]; then
    rm -rf "$BUSYBOX_SRC"
    tar -xjf "$DOWNLOADS/busybox-$BUSYBOX_VERSION.tar.bz2" -C "$TOOLCHAIN"
    (
        cd "$BUSYBOX_SRC"
        make defconfig
        # The host kernel headers lack the CBQ uAPI; the tc applet is not
        # needed by SpaceOS and the remaining applets stay enabled.
        sed -i 's/^CONFIG_TC=y/# CONFIG_TC is not set/' .config
        # Run applets inside the shell instead of requiring hundreds of
        # symlinks on the read-only FAT root.
        sed -i 's/^# CONFIG_FEATURE_SH_STANDALONE is not set/CONFIG_FEATURE_SH_STANDALONE=y/' .config
        sed -i 's/^# CONFIG_FEATURE_PREFER_APPLETS is not set/CONFIG_FEATURE_PREFER_APPLETS=y/' .config
        sed -i 's/^CONFIG_UNAME_OSNAME=.*/CONFIG_UNAME_OSNAME="SPK"/' .config
        make oldconfig </dev/null
        make -j"$JOBS" \
            CC="$MUSL/bin/musl-gcc -idirafter /usr/include -idirafter /usr/include/x86_64-linux-gnu" \
            HOSTCC="${HOSTCC:-cc}"
        cp busybox "$OUTPUT/busybox"
    )
fi

cp "$MUSL/lib/libc.so" "$OUTPUT/musl/ld-musl-x86_64.so.1"
cp "$MUSL/lib/libc.so" "$OUTPUT/musl/libc.so"
printf 'busybox ready: %s (%s bytes)\n' "$OUTPUT/busybox" \
    "$(wc -c < "$OUTPUT/busybox")"
