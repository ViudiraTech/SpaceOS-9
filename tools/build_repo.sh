#!/bin/sh
#       tools/build_repo.sh
#       Build and sign the local SpaceOS package repository
#
#       2026/11/23 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BUILD=${BUILD:-$ROOT/build}
REPO=${REPO:-$BUILD/repo/x86_64}
HOST_BIN=${HOST_BIN:-$ROOT/target/release/spk-host}
KEY=${SPK_REPO_KEY:-$BUILD/keys/repo.key}
PUBLIC=${SPK_REPO_PUBLIC:-$BUILD/keys/repo.pub}
SECURITY=${SPK_SECURITY:-}
ARCH=${ARCH:-x86_64}
REPO_NAME=${SPK_REPO_NAME:-spaceos-main}

if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH=$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || echo 0)
fi
export SOURCE_DATE_EPOCH
TOOLCHAIN=${RUSTUP_TOOLCHAIN:-nightly-2026-05-24}
export RUSTUP_TOOLCHAIN="$TOOLCHAIN"

if [ ! -x "$HOST_BIN" ]; then
    ( cd /tmp && cargo build \
        --manifest-path "$ROOT/user/spk/Cargo.toml" \
        -p spk-host --release --target-dir "$ROOT/target" )
fi

if [ ! -f "$KEY" ]; then
    install -d -m 0700 "$(dirname "$KEY")"
    "$HOST_BIN" keygen --name repo --out "$(dirname "$KEY")" \
        --comment "SpaceOS reproducible repository <repo@spaceos>"
    if [ -f "$(dirname "$KEY")/repo.key" ]; then
        KEY="$(dirname "$KEY")/repo.key"
        PUBLIC="$(dirname "$KEY")/repo.pub"
    fi
fi
if [ ! -f "$PUBLIC" ]; then
    case "$KEY" in
        *.key) PUBLIC=${KEY%.key}.pub ;;
        *) PUBLIC=$KEY.pub ;;
    esac
fi
if [ ! -f "$PUBLIC" ]; then
    printf 'error: public key %s is missing\n' "$PUBLIC" >&2
    exit 1
fi

SECURITY_ARGS=
if [ -n "$SECURITY" ]; then
    if [ ! -f "$SECURITY" ]; then
        printf 'error: security metadata %s is missing\n' "$SECURITY" >&2
        exit 1
    fi
    SECURITY_ARGS="--security $SECURITY"
fi

# Editorial half of store.json (hero slides, sections, category labels). The
# per-package half is generated from each manifest's [store] table.
STORE_SOURCE=${SPK_STORE_SOURCE:-$ROOT/packages/store.in.json}
if [ -f "$STORE_SOURCE" ]; then
    install -m 0644 "$STORE_SOURCE" "$REPO/store.in.json"
fi

# shellcheck disable=SC2086
"$HOST_BIN" index \
    --repo-dir "$REPO" \
    --repo "$REPO_NAME" \
    --arch "$ARCH" \
    --key "$KEY" \
    --generated-at "$SOURCE_DATE_EPOCH" \
    $SECURITY_ARGS
"$HOST_BIN" verify --repo-dir "$REPO" --key "$PUBLIC"

KEY_ID=$("$HOST_BIN" keyid --key "$PUBLIC")
TRUSTED=$BUILD/repo/trusted
install -d -m 0755 "$TRUSTED"
cp "$PUBLIC" "$TRUSTED/$KEY_ID.pub"
chmod 0644 "$TRUSTED/$KEY_ID.pub"
printf 'repository %s signed by %s\n' "$REPO" "$KEY_ID"
printf 'trusted key: %s\n' "$TRUSTED/$KEY_ID.pub"
