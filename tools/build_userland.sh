#!/bin/sh
#       tools/build_userland.sh
#       Build splibc and the static SpaceOS-9 userland
#       2026/10/25 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

CC=${CC:-clang}
LD=${LD:-ld.lld}
AR=${AR:-ar}
OUT=${OUT:-build/userland}
CFLAGS="-target x86_64-unknown-none -ffreestanding -fno-builtin -fno-pic -fno-pie \
 -fno-stack-protector -fno-asynchronous-unwind-tables -fno-unwind-tables \
 -mno-red-zone -O2 -Wall -Wextra -Werror -Iuser/splibc/include -Iuser/third_party/minimp3"

mkdir -p "$OUT/obj" "$OUT/bin"

for source in user/splibc/src/*.c; do
    name=${source##*/}
    object="$OUT/obj/${name%.c}.o"
    $CC $CFLAGS -c "$source" -o "$object"
done
$CC $CFLAGS -c user/splibc/syscall.S -o "$OUT/obj/syscall.o"
$CC $CFLAGS -c user/splibc/crt0.S -o "$OUT/obj/crt0.o"
$CC $CFLAGS -w -c user/third_party/minimp3/minimp3_impl.c -o "$OUT/obj/minimp3.o"

rm -f "$OUT/libsplibc.a"
$AR rcs "$OUT/libsplibc.a" "$OUT"/obj/*.o
# The archive must not provide _start; rebuild it without crt0.
rm -f "$OUT/libsplibc.a"
$AR rcs "$OUT/libsplibc.a" $(find "$OUT/obj" -name '*.o' ! -name 'crt0.o')

link() {
    program=$1
    source=$2
    object="$OUT/obj/${program}.o"
    $CC $CFLAGS -c "$source" -o "$object"
    $LD -m elf_x86_64 -T user/userland.ld --no-dynamic-linker -static \
        -o "$OUT/bin/$program" "$OUT/obj/crt0.o" "$object" "$OUT/libsplibc.a"
}

link hello user/bin/hello.c
link echo user/bin/echo.c
link cat user/bin/cat.c
[ -f user/bin/systemctl.c ] && link systemctl user/bin/systemctl.c
[ -f user/bin/systemg-analyze.c ] && link systemg-analyze user/bin/systemg-analyze.c
[ -f user/bin/journald.c ] && link journald user/bin/journald.c
[ -f user/bin/journalctl.c ] && link journalctl user/bin/journalctl.c
[ -f user/bin/dmesg.c ] && link dmesg user/bin/dmesg.c
[ -f user/systemg/systemg.c ] && link systemg user/systemg/systemg.c
[ -f user/sh/sh.c ] && link sh user/sh/sh.c
if [ -f user/bin/mp3play.c ]; then
    $CC $CFLAGS -c user/bin/mp3play.c -o "$OUT/obj/mp3play.o"
    $LD -m elf_x86_64 -T user/userland.ld --no-dynamic-linker -static \
        -o "$OUT/bin/mp3play" "$OUT/obj/crt0.o" "$OUT/obj/mp3play.o" \
        "$OUT/obj/minimp3.o" "$OUT/libsplibc.a"
fi

printf 'built splibc and userland in %s\n' "$OUT"
