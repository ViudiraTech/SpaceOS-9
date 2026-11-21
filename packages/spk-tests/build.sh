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

stage http-pkg http-pkg "http-v1"
stage http-pkg-1.1 http-pkg "http-v2"
stage dep-a dep-a "dep-a"
stage dep-b dep-b "dep-b"
stage file-c shared "from-c"
stage file-d shared "from-d"
stage app app "app"
stage lib lib "lib"


# P3b scriptlet/hook/trigger fixtures.
stage_script_from_stdin() {
    name=$1
    file=$2
    content=$3
    stage_dir=$OUT/$name
    rm -rf "$stage_dir"
    install -d -m 0755 "$stage_dir/usr/share/spk-tests"
    printf '%s\n' "$content" > "$stage_dir/usr/share/spk-tests/$file"
    chmod 0644 "$stage_dir/usr/share/spk-tests/$file"
    cat > "$stage_dir/.INSTALL"
    chmod 0444 "$stage_dir/.INSTALL"
}

# script-ok: declared file is modified by both pre and post install.
stage_script_from_stdin script-ok script-ok "base" <<'SCRIPT'
pre_install() {
    printf 'pre_install %s\n' "$1" >> "$SPK_ROOT/usr/share/spk-tests/script-ok"
}
post_install() {
    printf 'post_install %s\n' "$1" >> "$SPK_ROOT/usr/share/spk-tests/script-ok"
}
pre_upgrade() {
    printf 'pre_upgrade %s %s\n' "$1" "$2" >> "$SPK_ROOT/usr/share/spk-tests/script-ok"
}
post_upgrade() {
    printf 'post_upgrade %s %s\n' "$1" "$2" >> "$SPK_ROOT/usr/share/spk-tests/script-ok"
}
pre_remove() {
    printf 'pre_remove %s\n' "$1"
}
post_remove() {
    printf 'post_remove %s\n' "$1"
}
SCRIPT

# script-ok 0.2: upgrade arguments and a declared args file.
stage_script_from_stdin script-ok-0.2 script-ok "base-v2" <<'SCRIPT'
post_install() {
    printf 'post_install %s\n' "$1" >> "$SPK_ROOT/usr/share/spk-tests/script-ok"
}
pre_upgrade() {
    printf 'pre_upgrade %s %s\n' "$1" "$2" >> "$SPK_ROOT/usr/share/spk-tests/script-ok"
}
post_upgrade() {
    printf 'post_upgrade %s %s\n' "$1" "$2" >> "$SPK_ROOT/usr/share/spk-tests/script-ok"
}
pre_remove() {
    printf 'pre_remove %s\n' "$1"
}
post_remove() {
    printf 'post_remove %s\n' "$1"
}
SCRIPT

stage_script_from_stdin script-fail script-fail "base" <<'SCRIPT'
pre_install() {
    echo "script-fail pre_install" >&2
    exit 1
}
SCRIPT

stage_script_from_stdin script-post-fail script-post-fail "base" <<'SCRIPT'
post_install() {
    echo "script-post-fail post_install" >&2
    exit 1
}
SCRIPT

stage_script_from_stdin script-extra script-extra "base" <<'SCRIPT'
pre_install() {
    printf 'extra\n' > "$SPK_ROOT/usr/share/spk-tests/undeclared-extra"
}
SCRIPT

stage_script_from_stdin script-source source-blocked "blocked" <<'SCRIPT'
pre_install() {
    if printf 'escape\n' > /source/etc/spk-script-write 2>/dev/null; then
        echo "source write unexpectedly succeeded" >&2
        exit 1
    fi
    printf 'blocked\n' >> "$SPK_ROOT/usr/share/spk-tests/source-blocked"
}
SCRIPT

stage_script_from_stdin sandbox-ok sandbox-probe.txt "probe" <<'SCRIPT'
pre_install() {
    /usr/bin/sandbox-probe > "$SPK_ROOT/usr/share/spk-tests/sandbox-probe.txt"
}
SCRIPT

# trigger packages: both files match one trigger pattern, so the trigger runs once.
trigger_stage() {
    name=$1
    stage_dir=$OUT/$name
    rm -rf "$stage_dir"
    install -d -m 0755 "$stage_dir/usr/share/spk-tests/trigger" \
        "$stage_dir/usr/lib/spk/triggers"
    printf 'trigger-target\n' > "$stage_dir/usr/share/spk-tests/trigger/target-$name"
    printf 'trigger-target-2\n' > "$stage_dir/usr/share/spk-tests/trigger/target2-$name"
    chmod 0644 "$stage_dir/usr/share/spk-tests/trigger/target-$name" \
        "$stage_dir/usr/share/spk-tests/trigger/target2-$name"
    printf '#!/bin/sh\necho "TRIGGER_RAN %s $1"\n' "$name" > "$stage_dir/usr/lib/spk/triggers/$name.trigger"
    chmod 0755 "$stage_dir/usr/lib/spk/triggers/$name.trigger"
}
trigger_stage trigger-pkg
trigger_stage trigger-other

stage_dir=$OUT/hook-pkg
rm -rf "$stage_dir"
install -d -m 0755 "$stage_dir/usr/lib/spk/hooks/hook-pkg"
cat > "$stage_dir/usr/lib/spk/hooks/hook-pkg/10-test.hook" <<'HOOK'
[Trigger]
Operation = Install
Type = Package
Target = hook-pkg

[Action]
Description = 包内测试钩子
When = PostTransaction
Exec = echo PACKAGE_HOOK_RAN
HOOK
chmod 0644 "$stage_dir/usr/lib/spk/hooks/hook-pkg/10-test.hook"

stage_dir=$OUT/bad-sudoers
rm -rf "$stage_dir"
install -d -m 0755 "$stage_dir/etc"
printf 'this is not a valid sudoers rule\n' > "$stage_dir/etc/sudoers"
chmod 0644 "$stage_dir/etc/sudoers"

# Kernel setuid-exec probe (run by QEMU as uid/gid 1000).
if [ -f "$ROOT/build/userland/obj/crt0.o" ] && [ -f "$ROOT/build/userland/libsplibc.a" ]; then
    $CC $CFLAGS -c "$ROOT/packages/spk-tests/setuid-probe.c" -o "$OUT/setuid-probe.o"
    $LD -m elf_x86_64 -T "$ROOT/user/userland.ld" --no-dynamic-linker -static \
        -o "$OUT/setuid-probe" "$ROOT/build/userland/obj/crt0.o" \
        "$OUT/setuid-probe.o" "$ROOT/build/userland/libsplibc.a"
    printf 'built setuid-probe in %s\n' "$OUT"
fi

# Sandbox syscall probe used by P3b scriptlet tests.
if [ -f "$ROOT/build/userland/obj/crt0.o" ] && [ -f "$ROOT/build/userland/libsplibc.a" ]; then
    $CC $CFLAGS -I"$ROOT/user/test/include" -c "$ROOT/packages/spk-tests/sandbox-probe.c" -o "$OUT/sandbox-probe.o"
    $LD -m elf_x86_64 -T "$ROOT/user/userland.ld" --no-dynamic-linker -static \
        -o "$OUT/sandbox-probe" "$ROOT/build/userland/obj/crt0.o" \
        "$OUT/sandbox-probe.o" "$ROOT/build/userland/libsplibc.a"
    printf 'built sandbox-probe in %s\n' "$OUT"
fi

# sudoedit probe used by the QEMU sudo test.
if [ -f "$ROOT/build/userland/obj/crt0.o" ] && [ -f "$ROOT/build/userland/libsplibc.a" ]; then
    $CC $CFLAGS -c "$ROOT/packages/spk-tests/sudo-edit-probe.c" -o "$OUT/sudo-edit-probe.o"
    $LD -m elf_x86_64 -T "$ROOT/user/userland.ld" --no-dynamic-linker -static \
        -o "$OUT/sudo-edit-probe" "$ROOT/build/userland/obj/crt0.o" \
        "$OUT/sudo-edit-probe.o" "$ROOT/build/userland/libsplibc.a"
    printf 'built sudo-edit-probe in %s\n' "$OUT"
fi
