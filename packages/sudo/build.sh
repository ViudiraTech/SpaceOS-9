#!/bin/sh
#       packages/sudo/build.sh
#       Stage the setuid sudo/doas binaries and example policies
#
#       2026/11/21 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/sudo}
BIN=$ROOT/target/x86_64-unknown-linux-musl/release

for name in sudo doas; do
    if [ ! -x "$BIN/$name" ]; then
        printf 'error: %s missing; build user/sudo for x86_64-unknown-linux-musl first\n' "$BIN/$name" >&2
        exit 1
    fi
done

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin" "$STAGE/etc"
install -m 4755 "$BIN/sudo" "$STAGE/usr/bin/sudo"
install -m 4755 "$BIN/doas" "$STAGE/usr/bin/doas"
cat > "$STAGE/etc/sudoers.example" <<'POLICY'
# SpaceOS-9 sudoers example. Install as /etc/sudoers mode 0440 root.
Defaults lecture=once,timestamp_timeout=15,secure_path="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
%wheel ALL=(ALL) ALL
POLICY
cat > "$STAGE/etc/doas.conf.example" <<'POLICY'
# SpaceOS-9 doas example. Install as /etc/doas.conf mode 0440 root.
permit :wheel
POLICY
chmod 0440 "$STAGE/etc/sudoers.example" "$STAGE/etc/doas.conf.example"
printf 'staged sudo in %s\n' "$STAGE"
