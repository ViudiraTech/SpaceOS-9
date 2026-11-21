#!/bin/sh
#       packages/hello/build.sh
#       Stage hello 0.1 or 0.2 for spk-host pack
#
#       2026/11/20 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
VERSION=${1:-0.1.0}
STAGE=${2:-$ROOT/build/packages/hello-$VERSION}
CC=${CC:-clang}
LD=${LD:-ld.lld}
CFLAGS="-target x86_64-unknown-none -ffreestanding -fno-builtin -fno-pic -fno-pie \
 -fno-stack-protector -fno-asynchronous-unwind-tables -fno-unwind-tables \
 -mno-red-zone -O2 -Wall -Wextra -Werror -I$ROOT/user/splibc/include"

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin"

case "$VERSION" in
    0.1.0)
        SOURCE=${HELLO_BIN:-$ROOT/build/userland/bin/hello}
        if [ ! -x "$SOURCE" ]; then
            printf 'error: %s not found; run tools/build_userland.sh first\n' "$SOURCE" >&2
            exit 1
        fi
        install -m 0755 "$SOURCE" "$STAGE/usr/bin/hello"
        ;;
    0.2.0)
        OBJ=$ROOT/build/packages/hello-v2.o
        BIN=$ROOT/build/packages/hello-v2
        CRT=$ROOT/build/userland/obj/crt0.o
        LIBC=$ROOT/build/userland/libsplibc.a
        if [ ! -f "$CRT" ] || [ ! -f "$LIBC" ]; then
            printf 'error: splibc userland not built; run tools/build_userland.sh first\n' >&2
            exit 1
        fi
        mkdir -p "$ROOT/build/packages"
        $CC $CFLAGS -c "$ROOT/packages/hello/hello-v2.c" -o "$OBJ"
        $LD -m elf_x86_64 -T "$ROOT/user/userland.ld" --no-dynamic-linker -static \
            -o "$BIN" "$CRT" "$OBJ" "$LIBC"
        install -m 0755 "$BIN" "$STAGE/usr/bin/hello"
        install -d -m 0755 "$STAGE/usr/share/hello"
        printf '0.2.0\n' > "$STAGE/usr/share/hello/version"
        chmod 0644 "$STAGE/usr/share/hello/version"
        ;;
    *)
        printf 'error: unsupported hello version %s\n' "$VERSION" >&2
        exit 1
        ;;
esac

printf 'staged hello %s in %s\n' "$VERSION" "$STAGE"
