#!/bin/sh
#       packages/spk-tests/build.sh
#       Stage the small P1 solver/conflict fixtures
#
#       2026/11/20 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
OUT=${1:-$ROOT/build/spk-tests}
CC=${CC:-clang}
LD=${LD:-ld.lld}
CFLAGS="-target x86_64-unknown-none -ffreestanding -fno-builtin -fno-pic -fno-pie \
 -fno-stack-protector -fno-asynchronous-unwind-tables -fno-unwind-tables \
 -mno-red-zone -O2 -Wall -Wextra -Werror -I$ROOT/user/splibc/include"

stage() {
    name=$1
    file=$2
    data=$3
    stage_dir=$OUT/$name
    rm -rf "$stage_dir"
    install -d -m 0755 "$stage_dir/usr/share/spk-tests"
    printf '%s\n' "$data" > "$stage_dir/usr/share/spk-tests/$file"
    chmod 0644 "$stage_dir/usr/share/spk-tests/$file"
    printf 'staged %s in %s\n' "$name" "$stage_dir"
}

stage dep-a dep-a "dep-a"
stage dep-b dep-b "dep-b"
stage file-c shared "from-c"
stage file-d shared "from-d"
stage app app "app"
stage lib lib "lib"

# Kernel setuid-exec probe (run by QEMU as uid/gid 1000).
if [ -f "$ROOT/build/userland/obj/crt0.o" ] && [ -f "$ROOT/build/userland/libsplibc.a" ]; then
    $CC $CFLAGS -c "$ROOT/packages/spk-tests/setuid-probe.c" -o "$OUT/setuid-probe.o"
    $LD -m elf_x86_64 -T "$ROOT/user/userland.ld" --no-dynamic-linker -static \
        -o "$OUT/setuid-probe" "$ROOT/build/userland/obj/crt0.o" \
        "$OUT/setuid-probe.o" "$ROOT/build/userland/libsplibc.a"
    printf 'built setuid-probe in %s\n' "$OUT"
fi

# sudoedit probe used by the QEMU sudo test.
if [ -f "$ROOT/build/userland/obj/crt0.o" ] && [ -f "$ROOT/build/userland/libsplibc.a" ]; then
    $CC $CFLAGS -c "$ROOT/packages/spk-tests/sudo-edit-probe.c" -o "$OUT/sudo-edit-probe.o"
    $LD -m elf_x86_64 -T "$ROOT/user/userland.ld" --no-dynamic-linker -static \
        -o "$OUT/sudo-edit-probe" "$ROOT/build/userland/obj/crt0.o" \
        "$OUT/sudo-edit-probe.o" "$ROOT/build/userland/libsplibc.a"
    printf 'built sudo-edit-probe in %s\n' "$OUT"
fi
