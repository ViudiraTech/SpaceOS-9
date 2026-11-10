#!/bin/sh
#
#       tools/build_modules.sh
#       Build the in-tree loadable modules for one target architecture
#
#       2026/11/6 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#
# Usage: tools/build_modules.sh <arch> <output-directory>
#
# Modules are freestanding ET_REL objects. They must not use floating point,
# SIMD, PIC/GOT indirection, stack protectors or unwind tables: the kernel
# linker implements only the relocation types these flags emit.

set -eu

ARCH=${1:?usage: build_modules.sh <x86_64|aarch64|riscv64> <out-dir> [module...]}
OUT=${2:?usage: build_modules.sh <arch> <out-dir> [module...]}
CC=${CC:-clang}
RUSTC=${RUSTC:-rustc}

# No third argument means "build every module" for direct script use. An
# explicit (possibly empty) list selects exactly the modules Kconfig enabled.
if [ "$#" -ge 3 ]; then
    MODULES=$3
else
    MODULES=__all__
fi

enabled() {
    if [ "$MODULES" = "__all__" ]; then
        return 0
    fi
    case " $MODULES " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
    esac
}

case "$ARCH" in
x86_64)
    C_TARGET="-target x86_64-unknown-none -mcmodel=kernel -mno-red-zone -mno-sse -mno-mmx -mno-80387 -mno-avx"
    RUST_TARGET=x86_64-unknown-none
    RUST_CODE_MODEL=kernel
    ;;
aarch64)
    C_TARGET="-target aarch64-unknown-none -mcmodel=large -mgeneral-regs-only"
    RUST_TARGET=aarch64-unknown-none-softfloat
    RUST_CODE_MODEL=large
    ;;
riscv64)
    C_TARGET="-target riscv64-unknown-none-elf -march=rv64imac -mabi=lp64 -mcmodel=medany -mno-relax"
    RUST_TARGET=riscv64imac-unknown-none-elf
    RUST_CODE_MODEL=medium
    ;;
*)
    echo "unsupported module architecture: $ARCH" >&2
    exit 1
    ;;
esac

VERMAGIC="3.0-spk $ARCH SMP"

COMMON_FLAGS="-ffreestanding -fno-builtin -fno-pic -fno-pie -fno-common \
 -fno-stack-protector -fno-asynchronous-unwind-tables -fno-unwind-tables \
 -O2 -Wall -Wextra -Werror -Imodules/include"

mkdir -p "$OUT"

# Drop artifacts for modules that are no longer selected, so a stale .ko does
# not end up in the root filesystem after a configuration change.
for artifact in "$OUT"/*.ko; do
    [ -e "$artifact" ] || continue
    name=$(basename "$artifact" .ko)
    enabled "$name" || rm -f "$artifact"
done

if [ -z "$MODULES" ]; then
    echo "module: no modules selected by Kconfig"
    exit 0
fi

for source in modules/*/*.c; do
    [ -f "$source" ] || continue
    name=$(basename "$source" .c)
    enabled "$name" || continue
    "$CC" $C_TARGET $COMMON_FLAGS -DSPK_VERMAGIC="\"$VERMAGIC\"" \
        -c "$source" -o "$OUT/$name.ko"
    printf 'module: %s -> %s\n' "$source" "$OUT/$name.ko"
done

for source in modules/*/src/lib.rs; do
    [ -f "$source" ] || continue
    directory=$(dirname "$(dirname "$source")")
    name=$(basename "$directory")
    enabled "$name" || continue
    SPK_VERMAGIC="$VERMAGIC" "$RUSTC" \
        --edition 2024 \
        --crate-type lib \
        --emit obj \
        --target "$RUST_TARGET" \
        -C opt-level=2 \
        -C panic=abort \
        -C relocation-model=static \
        -C code-model="$RUST_CODE_MODEL" \
        -C debuginfo=0 \
        -o "$OUT/$name.ko" \
        "$source"
    printf 'module: %s -> %s\n' "$source" "$OUT/$name.ko"
done
