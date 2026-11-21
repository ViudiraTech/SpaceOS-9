#!/bin/sh
#       packages/spk-tests/build.sh
#       Stage the small P1 solver/conflict fixtures
#
#       2026/11/20 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
OUT=${1:-$ROOT/build/spk-tests}

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
