# spk — SpaceOS-9 native package manager

P2 status: target-side `spk` can install, upgrade, downgrade, reinstall and
remove from signed local repositories, with a backtracking dependency solver,
holds/pins, multi-repository priority and mirror failover, deterministic
`--print`/`--json` transaction plans, file/transaction journal and crash
recovery. Design: `docs/PACKAGE_MANAGER_PLAN.md`; privileged tools:
`docs/SUDO_DOAS_DESIGN.md`.

## Layout

```
user/spk/
├── Cargo.toml        virtual workspace
├── spk-core/         schemas, alpm version comparison, keys, tar safety,
│                     zstd decode (ruzstd) and safe extraction
├── spk-host/         host CLI: keygen / pack / index / sign / verify
└── spk/              target CLI and library: install / upgrade / downgrade /
                      reinstall / remove / list / info / files / owns /
                      check / repair / trust / hold / repo
user/sudo/            sudo + doas (setuid root, shadow auth, tickets, audit)
packages/hello/       hello 0.1.0 and 0.2.0 upgrade fixtures
packages/sudo/        setuid sudo/doas package fixtures
packages/spk-tests/   solver/conflict/setuid fixtures (dep-a/dep-b, file-c/d,
                      app/lib, setuid-probe, sudo-edit-probe)
tools/build_local_repo.sh
tools/spk_qemu_test.py
repo/                 generated artifacts, gitignored
```

`spk-core` and the target `spk` have no C dependencies: zstd is decoded with
`ruzstd`. `spk-host` uses the bundled C zstd encoder for package creation
because `ruzstd` 0.9 is decode-only (plan §19).

## Build and test

Run from the repository root; `cd /tmp` keeps rustup from reading an
unrelated toolchain file while `RUSTUP_TOOLCHAIN` pins the repository
nightly.

```sh
REPO=$(git rev-parse --show-toplevel)

# Host tool
cd /tmp && RUSTUP_TOOLCHAIN=nightly-2026-05-24 \
  cargo build --manifest-path "$REPO/user/spk/Cargo.toml" \
  -p spk-host --release --target-dir "$REPO/target"

# Target tool (static musl)
cd /tmp && RUSTUP_TOOLCHAIN=nightly-2026-05-24 \
  cargo build --manifest-path "$REPO/user/spk/Cargo.toml" \
  -p spk --bins --release --target x86_64-unknown-linux-musl \
  --target-dir "$REPO/target"

# All unit and integration tests
cd /tmp && RUSTUP_TOOLCHAIN=nightly-2026-05-24 \
  cargo test --manifest-path "$REPO/user/spk/Cargo.toml" \
  --target-dir "$REPO/target"

# sudo / doas (static musl)
cd /tmp && RUSTUP_TOOLCHAIN=nightly-2026-05-24 \
  cargo build --manifest-path "$REPO/user/sudo/Cargo.toml" --bins --release \
  --target x86_64-unknown-linux-musl --target-dir "$REPO/target"
cd /tmp && RUSTUP_TOOLCHAIN=nightly-2026-05-24 \
  cargo test --manifest-path "$REPO/user/sudo/Cargo.toml" \
  --target-dir "$REPO/target"

# QEMU matrix (kernel ISO, ext4 rootfs, three boots; includes spk P2,
# sudo/doas, setuid probes and ext4 journal replay)
# needs qemu-system-x86_64, OVMF, fakeroot, e2fsprogs, xorriso and KVM
# (falls back to TCG when KVM is unavailable)
tools/spk_qemu_test.py
```

Binaries: `target/release/spk-host`,
`target/x86_64-unknown-linux-musl/release/spk` and `spk-qemu-test`
(static-pie, the same musl ELF form as the SPUI/browser userland).

## Repository workflow

```sh
# 1. Key pair (private key stays local, mode 0600)
target/release/spk-host keygen --name spaceos-main \
  --out ~/.config/spaceos/spk/keys \
  --comment "SpaceOS Build <build@spaceos>"

# 2. Stage and pack hello 0.1 and 0.2
packages/hello/build.sh 0.1.0
packages/hello/build.sh 0.2.0
target/release/spk-host pack --manifest packages/hello/manifest.toml \
  --root build/packages/hello-0.1.0 \
  --output repo/x86_64/hello-0.1.0-1-x86_64.spk
target/release/spk-host pack --manifest packages/hello/manifest-0.2.0.toml \
  --root build/packages/hello-0.2.0 \
  --output repo/x86_64/hello-0.2.0-1-x86_64.spk

# 3. Index and sign
target/release/spk-host index \
  --repo-dir repo/x86_64 --repo spaceos-main --arch x86_64 \
  --key ~/.config/spaceos/spk/keys/spaceos-main.key

# 4. Verify
target/release/spk-host verify \
  --repo-dir repo/x86_64 --key ~/.config/spaceos/spk/keys/spaceos-main.pub
```

`tools/build_local_repo.sh` runs steps 2–4 for both hello versions and
publishes `repo/keys/<key_id>.pub` for out-of-band trust distribution. Clients
must never auto-trust a key fetched from the repository itself.

## Target usage

```sh
spk install --repo /repo/x86_64 hello            # latest version
spk install --repo /repo/x86_64 hello=0.1.0-1    # exact version
spk upgrade --repo /repo/update                  # full system upgrade
spk upgrade --repo /repo/update --print          # show the plan, do not apply
spk --json upgrade --repo /repo/update           # deterministic JSON plan
spk downgrade --repo /repo/update hello=0.1.0-1
spk reinstall --repo /repo/x86_64 hello
spk hold hello                                   # pin the installed version
spk hold                                         # list holds
spk unhold hello
spk install --override-hold hello=0.2.0-1        # one-shot override, clears hold
spk remove hello
spk remove -Rs app                               # orphan dependency cleanup
spk list / info hello / files hello / owns /usr/bin/hello
spk check                                        # DB + filesystem consistency
spk repair                                       # recover an interrupted transaction
spk trust add <public-key.pub> / trust list
spk repo add main /repo/x86_64 --priority 10
spk repo add main /repo/mirror --priority 10     # same repo, fallback mirror
spk repo list / repo priority main 20 / repo remove main
```

`--print` emits a human-readable plan (actions and removals) and exits without
prompting; `--json` emits schema-1 JSON with `operation`, `packages`,
`removals`, `held_back`, `nothing_to_do` and `final_package_count`, and prints
errors as JSON too. Both are deterministic for identical state and repositories.
`--json` takes precedence when combined with `--print`.

Global options: `--root <dir>` (default `/`), `--noconfirm`, `--lang en|zh_CN`,
`--json`. Local paths and `file://` repositories are supported; repository
priority and mirror failover work for local repositories. HTTP(S), key rotation
and anti-downgrade arrive in P4.

Exit codes follow plan §9.2: 0 OK, 1 general, 2 usage, 3 signature, 4
dependency, 5 file conflict, 6 rolled back.

## Transaction model

- `/var/lib/spk/{installed,files,world,repos,holds}.json`, `lock`, `journal`,
  `backup/<txid>`, `staging/<txid>`.
- `files.json` tracks non-directory paths only; directories are derived from
  each package's file list, so shared directories are never removed while
  non-empty.
- Before touching the filesystem, the transaction writes an append-only JSONL
  journal and `fsync`s each intent. Existing files are copied to
  `backup/<txid>` and verified before overwrite/removal.
- Files are installed through a same-directory temporary file and an atomic
  rename; database files use temp + rename after the file phase. A crash at
  any point is rolled back (or rolled forward after `DbDone`) by `spk repair`.
- `SPK_TEST_CRASH=<point>` is a test-only hook that SIGKILLs `spk` at a named
  commit point; the QEMU and host tests use it for crash recovery.
- Conffiles: first install records the hash; upgrade keeps user edits and
  writes new upstream content as `.spksave`; removal renames the conffile to
  a free `.spksave[.N]` unless `--purge`.

### Solver

Backtracking over installed and repository candidates. Supports versioned
depends, versioned `provides`, `conflicts`, `replaces`, dependency providers,
cycles (the SCC is installed together), full upgrades, exact downgrades,
reinstalls and holds. A hold pins the installed version; full upgrades report
it as held back, explicit changes fail with exit 4 unless `--override-hold`,
and an override clears the hold. Partial upgrades (`upgrade <pkg>` without
`--allow-partial`) are refused. Multiple repositories are merged newest-first;
same-version metadata must agree across repositories or the transaction fails.
A configured repository may list mirrors: transient I/O failures fall through
to the next mirror, while a bad signature is always fatal. Failures print
pacman-style chains, e.g.:

```
error: 无法准备事务（无法满足依赖关系）
:: 安装 dep-b (2.0.0-1) 会破坏由 dep-a 要求的依赖 'dep-b<2'
```

## Frozen schemas

All state files carry `"schema": 1`.

### manifest.toml

Fields follow plan §3.2. Required additions:

- `[[files]]` has optional `type = "file" | "dir" | "symlink"` (default
  `file`) and `target` for symlinks.
- Directory entries carry `sha256 = ""` and `size = 0`; symlink entries hash
  the target string.
- `release` is a positive integer. `files` is generated by `spk build/pack`
  when omitted; if present it is verified against staging, not trusted.
- Source manifests may list `conffiles` before `files` exists; validation is
  repeated after the generated table is filled.
- Filename: `name-version-release-arch.spk`, or
  `name-epoch:version-release-arch.spk` when `epoch != 0`.
- Paths are absolute in the manifest and relative inside the payload tar.
  The target-side limit is 511 bytes and `/var/lib/spk` plus
  `/etc/spk/trusted.d` are reserved.

Package symlinks may use absolute targets (interpreted inside the install
root) or relative targets that do not climb above `/`. Entries nested below a
file or symlink entry are rejected, as are devices, FIFOs, sparse entries and
duplicate paths. Hard links are stored as regular files with a warning; full
hard-link support is P2.

### index.json and signatures

Exactly plan §4.2; packages are sorted by name and version. Pretty JSON with a
trailing newline, signed byte-for-byte. `key_id = ed25519:<first 16 hex chars
of SHA-256(public key)>`; trust files are `<key_id>.pub`. `index.json.sig`
contains `schema`/`algorithm`/`key_id`/`sha256`/`signature` (base64 ed25519).

### holds.json

`{"schema":1,"holds":{"hello":{"name":"hello","epoch":0,"version":"0.1.0",
"release":1,"reason":"user"}}}`. `spk check` verifies every hold names an
installed package; removing a package drops its hold; `--override-hold` clears
the holds it overrides. `holds.json` participates in the transaction backup,
journal and repair set like the other databases.

### Versions

`epoch:version-release` follows libalpm `rpmvercmp`. `pkgver` starts and ends
with an ASCII alphanumeric and may contain `.`, `_`, `+`, `~` in between.
libalpm's comparator is not a total order for strings with leading/trailing
separators; the schema keeps versions inside the total-order domain, and the
proptest fuzz checks antisymmetry, transitivity and parse/display round-trips.

### Reproducibility

Tar entries are sorted, owned by 0:0, and stamped from `SOURCE_DATE_EPOCH`
(falling back to `manifest.build_date`). zstd level defaults to 19. Packing the
same staging twice yields the same sha256.

## Kernel notes

- The target uses `openat`/`mkdirat`/`unlinkat`/`symlinkat`/`flock`/`fsync`/
  `renameat2`; all are implemented by SpaceOS. VFS does not yet honor
  `O_NOFOLLOW` for ancestors, so `spk` resolves components with `lstat` and
  stays inside `--root`; a TOCTOU window remains until the kernel catches up.
- Path syscalls use a 512-byte buffer (511-byte paths).
- ext4 `extent_truncate()` now frees directory data blocks as metadata and
  writes a jbd2 revoke; without it, replay after power loss could clobber a
  reused block (found by the QEMU phase-3 test).
- The QEMU harness leaves the ext4 journal for the next boot to replay between
  phases 1↔2, which is the regression for the directory-block revoke fix
  (boot 2/3 replay 6/28 revokes). The final phase calls `sync()` so the host
  can inspect the image offline. The explicit SIGKILL recovery is the separate
  spk process crash in phase 2.
- SpaceOS does not implement `copy_file_range` (syscall 326). `spk` copies
  package data and database backups with read/write loops so std's fallback
  never has to log an unimplemented syscall.
- The kernel now implements setuid/setgid exec (euid/suid, egid/sgid,
  fsuid/fsgid reset, `no_new_privs`/`nosuid` suppression, `AT_SECURE`,
  `dumpable=0`) and honors `MS_NOSUID`/`MS_NOEXEC`. `setgid` group privilege is
  derived from euid 0 (Linux semantics), so a 4755 setuid-root sudo whose egid
  is still the caller can switch groups. `packages/sudo` is installed with
  mode 4755 only after spk verifies `/etc/spk/setuid.allow` is root-owned and
  lists the path.
- OOBE empty passwords use a `$6$` hash of the empty string; sudo accepts the
  empty shadow hash/password and prints a one-time empty-password warning in
  its first-authorization notice.

## Output style

Plan §10.0/§10.5/§10.7: `::` only for stage headers, `(n/m)` for package
progress, `error:`/`warning:` for transaction diagnostics, `ERROR:`/`WARNING:`
for index/constraint diagnostics. zh_CN follows pacman zh_CN, including
`今日无事可做` for a no-op install/upgrade. `spk-host` is English-only.
