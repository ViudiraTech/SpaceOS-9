#!/bin/sh
#       tools/build_user_tests.sh
#       Build freestanding x86_64 user test ELFs
#       2026/10/25 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

CC=${CC:-clang}
LD=${LD:-ld.lld}
OUT=build/user
CFLAGS="-target x86_64-unknown-none -ffreestanding -fno-builtin -fno-pic -fno-pie \
 -fno-stack-protector -fno-asynchronous-unwind-tables -fno-unwind-tables \
 -mno-red-zone -O2 -Wall -Wextra -Werror"

mkdir -p "$OUT"

$CC $CFLAGS -c user/test/crt0_x86_64.S -o "$OUT/crt0_x86_64.o"
$CC $CFLAGS -c user/test/clone_thread_x86_64.S -o "$OUT/clone_thread_x86_64.o"
$CC $CFLAGS -c user/test/thread_entry_x86_64.c -o "$OUT/thread_entry_x86_64.o"
$CC $CFLAGS -c user/test/process_signal_x86_64.c -o "$OUT/process_signal_x86_64.o"
$CC $CFLAGS -c user/test/exec_target_x86_64.c -o "$OUT/exec_target_x86_64.o"
$CC $CFLAGS -c user/test/p3_syscalls_x86_64.c -o "$OUT/p3_syscalls_x86_64.o"

$LD -m elf_x86_64 -T user/test/user_x86_64.ld --no-dynamic-linker \
    -o user/test/process_signal_x86_64.elf \
    "$OUT/crt0_x86_64.o" "$OUT/clone_thread_x86_64.o" \
    "$OUT/thread_entry_x86_64.o" "$OUT/process_signal_x86_64.o"

$LD -m elf_x86_64 -T user/test/user_x86_64.ld --no-dynamic-linker \
    -o user/test/exec_target_x86_64.elf \
    "$OUT/crt0_x86_64.o" "$OUT/exec_target_x86_64.o"

$LD -m elf_x86_64 -T user/test/user_x86_64.ld --no-dynamic-linker \
    -o user/test/p3_syscalls_x86_64.elf \
    "$OUT/crt0_x86_64.o" "$OUT/p3_syscalls_x86_64.o"

printf 'built %s\n' user/test/process_signal_x86_64.elf user/test/exec_target_x86_64.elf user/test/p3_syscalls_x86_64.elf
