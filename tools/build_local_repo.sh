#!/bin/sh
#       tools/build_local_repo.sh
#       Build the P0 local repository (hello package, index, signature)
#
#       2026/11/20 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
REPO_DIR=${SPK_REPO_DIR:-$ROOT/repo/x86_64}
KEY=${SPK_KEY:-$HOME/.config/spaceos/spk/keys/spaceos-main.key}
SPK_HOST=${SPK_HOST_BIN:-}
ARCH=x86_64

case "$KEY" in
    *.key) PUB=${SPK_PUB:-${KEY%.key}.pub} ;;
    *) PUB=${SPK_PUB:-} ;;
esac
if [ -n "$PUB" ] && [ "$PUB" = "$KEY" ]; then
    printf 'error: public key path must differ from the private key\n' >&2
    exit 1
fi

if [ -z "$SPK_HOST" ]; then
    cd /tmp
    RUSTUP_TOOLCHAIN=nightly-2026-05-24 \
        cargo build --manifest-path "$ROOT/user/spk/Cargo.toml" \
        -p spk-host --release --target-dir "$ROOT/target"
    cd "$ROOT"
    SPK_HOST=$ROOT/target/release/spk-host
fi

if [ ! -x "$SPK_HOST" ]; then
    printf 'error: spk-host binary not found at %s\n' "$SPK_HOST" >&2
    exit 1
fi

if [ ! -f "$KEY" ]; then
    printf 'error: signing key %s not found\n' "$KEY" >&2
    printf '       run: %s keygen --name spaceos-main --out %s\n' \
        "$SPK_HOST" "$(dirname -- "$KEY")" >&2
    exit 1
fi

if [ -z "$PUB" ] || [ ! -f "$PUB" ]; then
    printf 'error: public key not found; set SPK_PUB to the .pub file\n' >&2
    exit 1
fi

mkdir -p "$REPO_DIR"
for version in 0.1.0 0.2.0; do
    case "$version" in
        0.1.0) manifest=$ROOT/packages/hello/manifest.toml ;;
        0.2.0) manifest=$ROOT/packages/hello/manifest-0.2.0.toml ;;
    esac
    "$ROOT/packages/hello/build.sh" "$version" "$ROOT/build/packages/hello-$version"
    "$SPK_HOST" pack \
        --manifest "$manifest" \
        --root "$ROOT/build/packages/hello-$version" \
        --output "$REPO_DIR/hello-$version-1-$ARCH.spk"
done
"$SPK_HOST" index \
    --repo-dir "$REPO_DIR" \
    --repo spaceos-main \
    --arch "$ARCH" \
    --key "$KEY"

# Publish a copy of the trust key next to the repository. The filename is the
# key id so it can be dropped into /etc/spk/trusted.d/ unchanged.
KEY_ID=$("$SPK_HOST" keyid --key "$PUB")
mkdir -p "$ROOT/repo/keys"
install -m 0644 "$PUB" "$ROOT/repo/keys/$KEY_ID.pub"

"$SPK_HOST" verify --repo-dir "$REPO_DIR" --key "$PUB"
