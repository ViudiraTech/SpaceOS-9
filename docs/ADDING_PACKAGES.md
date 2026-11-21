# 向 SpaceOS 9 增加软件包

本文面向要给 `spk` 仓库新增软件包的人。内容包括包格式、manifest 字段、staging、脚本/钩子/触发器、构建、签名仓库、测试和发布。

约定：

- 包管理器二进制：`spk`（目标端）、`spk-host`（构建端）。
- 包文件：`.spk`，tar + zstd。
- 仓库：`index.json` + `index.json.sig` + 包文件，按架构分目录。
- 当前 ABI：`spaceos-1`；当前架构：`x86_64`，索引也支持 `aarch64` / `riscv64`。
- 源码树根目录：`SpaceKernel/`（也就是 SpaceOS-9 仓库）。

---

## 1. 五分钟新增一个包

以 `hello` 为模板，新增 `foo`：

```sh
mkdir -p packages/foo
```

`packages/foo/manifest.toml`：

```toml
name = "foo"
epoch = 0
version = "1.0.0"
release = 1
arch = "x86_64"
abi = "spaceos-1"
summary = "Foo utility"
description = "Example package for SpaceOS 9."
license = "MIT"
url = "https://example.invalid/foo"
packager = "SpaceOS Build <build@spaceos>"
build_date = 0

depends = []
provides = []
conflicts = []
replaces = []
conffiles = []
scripts = false
allow_setuid = false
```

`packages/foo/build.sh`：

```sh
#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
STAGE=${1:-$ROOT/build/packages/foo}
BIN=${FOO_BIN:-$ROOT/target/x86_64-unknown-linux-musl/release/foo}

if [ ! -x "$BIN" ]; then
    printf 'error: %s missing; build foo first\n' "$BIN" >&2
    exit 1
fi

rm -rf "$STAGE"
install -d -m 0755 "$STAGE/usr/bin"
install -m 0755 "$BIN" "$STAGE/usr/bin/foo"
```

在 `tools/build_packages.sh` 末尾增加一行：

```sh
pack_one foo 1.0.0-1 "$ROOT/packages/foo/manifest.toml" "$BUILD/packages/foo"
```

然后：

```sh
chmod +x packages/foo/build.sh
make packages
make repo
```

`make packages` 会调用 `packages/foo/build.sh`，再用 `spk-host pack` 生成：

```text
build/repo/x86_64/foo-1.0.0-1-x86_64.spk
```

`make repo` 会扫描 `build/repo/x86_64`、生成 `index.json` 并用构建密钥签名。

本地测试（以 root）：

```sh
spk --repo /home/jitianyu/Project/SpaceKernel/build/repo/x86_64 install foo
spk files foo
spk check
spk remove foo
```

---

## 2. 目录与包格式

### 2.1 源码目录

```text
packages/foo/
├── manifest.toml        # 元数据；源文件通常不写 [[files]]
├── build.sh             # 把文件放进 staging
├── .INSTALL             # 可选：安装/升级/卸载脚本
└── ...                  # 源码、配置模板、trigger 脚本等
```

`packages/hello`、`packages/sudo`、`packages/base` 是可直接参考的示例。

### 2.2 生成后的 `.spk`

```text
foo-1.0.0-1-x86_64.spk
├── manifest.toml        # 最终清单，含 files
├── .BUILDINFO           # 构建信息
├── usr/bin/foo
└── ...
```

`spk-host pack` 会：

- 扫描 staging，生成 `[[files]]`；
- 拒绝绝对路径、`..`、设备/FIFO、越界符号链接；
- 规范化属主为 `0:0`；
- 记录文件类型（`file` / `dir` / `symlink`）、模式、SHA-256、大小和符号链接目标；
- 校验 `conffiles`、`scripts`、`triggers` 与实际文件一致。

### 2.3 包文件名

```text
epoch = 0:   name-version-release-arch.spk
epoch != 0:  name-epoch:version-release-arch.spk
```

---

## 3. manifest 字段参考

```toml
name = "foo"
epoch = 0
version = "1.0.0"
release = 1
arch = "x86_64"
abi = "spaceos-1"
summary = "一句话说明"
description = "更长说明"
license = "MIT"
url = "https://example.invalid/foo"
packager = "SpaceOS Build <build@spaceos>"
build_date = 0

depends = ["bar>=2.0", "baz"]
provides = ["foo-abi=1"]
conflicts = ["foo-legacy"]
replaces = ["foo-old"]

conffiles = ["/etc/foo.conf"]
scripts = true
triggers = ["/usr/share/foo/*"]
allow_setuid = false
```

### 3.1 必填

- `name`：包名，必须以字母或数字开头，只能包含字母、数字、`.` `_` `+` `@` `-`，最长 128 字节。
- `version`：上游版本，首尾必须是字母数字，中间只允许字母、数字、`.` `_` `+` `~`；合法例子 `1.0.0`、`1.2.3rc1`。
- `release`：打包修订号，从 1 开始。**只改打包内容、不改上游版本时，必须增加 `release`**。
- `arch`：`x86_64`、`aarch64` 或 `riscv64`。
- `abi`：`spaceos-1`。
- `summary`、`license` 不能为空。

### 3.2 依赖

- `depends`：`name`、`name=1.2.3-1`、`name>=1.2`、`name<2`、`name>1`、`name<=2`。
- `provides`：虚拟包或 ABI，如 `foo-abi=1`。
- `conflicts`：不能与哪些包共存。
- `replaces`：本包接管旧包；同一次事务中删除旧包并转移文件归属。

依赖由求解器处理；文件冲突由事务阶段处理。

### 3.3 conffiles

```toml
conffiles = ["/etc/foo.conf"]
```

- 必须是绝对路径，且必须出现在最终 `files` 中。
- 升级时用户修改过的 conffile 会保留，上游新版本存为 `.spksave`。
- 不要把整个 `/etc` 目录设为 conffile。

### 3.4 脚本

`scripts = true` 才允许包内 `.INSTALL`。签名索引必须覆盖脚本。

### 3.5 触发器

```toml
triggers = ["/usr/share/foo/*"]
```

- 每条 pattern 必须是绝对路径 glob；事务命中后每个包每事务执行一次。
- 包内必须有 `/usr/lib/spk/triggers/<name>.trigger` 普通文件。

### 3.6 setuid

```toml
allow_setuid = true
```

同时：

1. 文件模式必须是 `4755`/`2755`/`6755`；
2. 目标机 `/etc/spk/setuid.allow` 必须包含该路径，且为 `root:root 0644`；
3. 安装后 spk 会校验文件确实是 `root` 所有；否则回滚。

示例见 `packages/sudo`。

---

## 4. staging 与 build.sh 规范

### 4.1 基本原则

- 只写 staging 下的路径，不写真实根。
- 用 `install -m` 而不是 `cp`，显式设置模式。
- 目录用 `install -d -m 0755`。
- 所有包文件最终必须是 `root:root`；`spk-host pack` 会规范化，但不要在脚本里创建非 root 文件。
- 不要把 `build/`、`target/`、私钥、测试产物放进 staging。

### 4.2 普通文件

```sh
install -d -m 0755 "$STAGE/usr/bin"
install -m 0755 "$BIN/foo" "$STAGE/usr/bin/foo"
install -m 0644 "$ROOT/packages/foo/foo.conf" "$STAGE/etc/foo.conf"
```

### 4.3 符号链接

```sh
ln -s foo "$STAGE/usr/bin/foo-alias"
```

`pack` 会记录 `type = "symlink"` 和 `target`；符号链接不得逃出安装根。

### 4.4 可复现

`tools/build_packages.sh` 已经做了：

- 固定 `SOURCE_DATE_EPOCH`（默认取最后一次 git commit 时间）；
- staging 排序、固定 mtime；
- `.BUILDINFO`；
- `spk-host pack --level 19`。

如果手工打包，保持同样输入和 `SOURCE_DATE_EPOCH`，输出应该可复现。

---

## 5. 安装脚本 `.INSTALL`

放在 staging 根，`scripts = true`。

支持函数：

| 操作 | pre | post |
|---|---|---|
| 安装 | `pre_install` | `post_install` |
| 升级 | `pre_upgrade` | `post_upgrade` |
| 卸载 | `pre_remove` | `post_remove` |

参数：

- install：`$1 = 新版本`
- upgrade：`$1 = 新版本`，`$2 = 旧版本`
- remove：`$1 = 旧版本`

示例：

```sh
pre_install() {
    : "$1"
}

post_install() {
    : "$1"
}

pre_upgrade() {
    : "$1" "$2"
}

post_upgrade() {
    : "$1" "$2"
}

pre_remove() {
    : "$1"
}

post_remove() {
    : "$1"
}
```

运行环境：

- `/bin/sh`（BusyBox ash）；
- 非 root 的 `spk-script` uid/gid 61111；
- 私有 mount namespace、`/stage` 可写、`/source` 只读；
- 无网络、无 mount/chroot/ptrace/mknod 等；
- 60 秒超时、输出上限、独立进程组；
- 只能修改已声明文件；新增/删除/改类型会被拒绝。

需要 root 的系统动作不要放在 `.INSTALL`，放到 `/etc/spk/hooks.d` 的系统 hook（root 受信配置）。

失败策略：

- 默认 `pre_*`/`post_*` 失败都中止并回滚；
- `--script-failure=keep` 仅调试；
- `--no-sandbox-scripts` 仅调试，会打印警告。

---

## 6. Hook 与 trigger

### 6.1 系统 hook（root）

`/etc/spk/hooks.d/*.hook`：

```ini
[Trigger]
Operation = Install
Operation = Upgrade
Type = Path
Target = usr/share/foo/*

[Action]
Description = Updating foo index...
When = PostTransaction
Exec = /usr/bin/foo-update-index
```

- 系统 hook 由 root 执行，适合 `ldconfig`、索引更新、权限修复等。
- `Operation`：Install / Upgrade / Remove。
- `Type`：Package / Path。
- `When`：PreTransaction / PostTransaction。

### 6.2 包 hook（沙箱）

包内：

```text
/usr/lib/spk/hooks/foo/foo.hook
```

格式同上，但由沙箱以非 root 执行。需要 root 的动作不要放包 hook。

### 6.3 文件 trigger（apk 风格）

manifest：

```toml
triggers = ["/usr/share/foo/*"]
```

包内：

```text
/usr/lib/spk/triggers/foo.trigger
```

事务后按包名排序，每包每事务执行一次，参数：

```text
$1 = install | upgrade | remove
$2.. = 命中的路径
```

输出类似：

```text
Executing foo-1.0.0-1.trigger
```

失败只记录 warning；journal marker 防止 `repair` 重复执行。

---

## 7. 构建与本地仓库

### 7.1 构建全部包

```sh
make packages
```

实际调用 `tools/build_packages.sh`，它负责：

- 构建 musl 静态 `spk` / `spk-sandbox`；
- 构建各包 staging；
- 写 `.BUILDINFO`；
- `spk-host pack` 到 `build/repo/x86_64`。

### 7.2 只打一个包

```sh
SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) \
  packages/foo/build.sh build/packages/foo

./target/release/spk-host pack \
  --manifest packages/foo/manifest.toml \
  --root build/packages/foo \
  --output build/repo/x86_64 \
  --level 19
```

### 7.3 签本地仓库

```sh
make repo
```

等价于：

```sh
./target/release/spk-host index \
  --repo-dir build/repo/x86_64 \
  --repo spaceos-main \
  --arch x86_64 \
  --key build/keys/repo.key

./target/release/spk-host verify \
  --repo-dir build/repo/x86_64 \
  --key build/keys/repo.pub
```

构建密钥在 `build/keys/`，只在本地使用，永远不要提交。

### 7.4 生成新密钥

```sh
./target/release/spk-host keygen --name spaceos-main --out ~/.config/spaceos/spk/keys/spaceos-main
```

同时得到 `.key`（私钥，保密）和 `.pub`（公钥，可发布）。

---

## 8. 测试

### 8.1 host 测试

```sh
cd /tmp
RUSTUP_TOOLCHAIN=nightly-2026-05-24 \
  cargo test --manifest-path /home/jitianyu/Project/SpaceKernel/user/spk/Cargo.toml
RUSTUP_TOOLCHAIN=nightly-2026-05-24 \
  cargo clippy --manifest-path /home/jitianyu/Project/SpaceKernel/user/spk/Cargo.toml --all-targets -- -D warnings
```

### 8.2 QEMU 矩阵

```sh
python3 -I tools/spk_qemu_test.py --timeout 600
```

覆盖安装/升级/降级/卸载、文件冲突、`--overwrite`、脚本、trigger、hook、sudo setuid、repair、HTTP 仓库等。

### 8.3 手工验收

在 guest 里：

```sh
doas spk repo list
doas spk sync
doas spk install foo
spk files foo
spk owns /usr/bin/foo
spk check
doas spk remove foo
```

升级：

1. 修改源码；
2. `version` 或 `release` 增加；
3. `make packages repo`；
4. 发布新 `index.json`/`.sig`/包；
5. `doas spk sync && doas spk upgrade`。

文件冲突：

- 目标文件属于其他包：默认拒绝；用 `--overwrite <glob>` 或 `replaces`；
- 目标文件不属于任何包：默认拒绝；用 `--overwrite <glob>` 或先删除；
- 交互模式会提示 `Overwrite? [y/N]`。

---

## 9. 发布到生产仓库

当前生产仓库：

```text
https://goutoustdio.top/spaceos/repo/x86_64
key_id: ed25519:b4280a08406a53c5
public key: packages/keys/ed25519:b4280a08406a53c5.pub
```

### 9.1 用生产密钥重签

```sh
rm -rf build/repo-prod && mkdir -p build/repo-prod/x86_64

# 保留线上已有包，再加入新包
cp repo/x86_64/hello-0.1.0-1-x86_64.spk build/repo-prod/x86_64/
cp repo/x86_64/hello-0.2.0-1-x86_64.spk build/repo-prod/x86_64/
cp build/repo/x86_64/foo-1.0.0-1-x86_64.spk build/repo-prod/x86_64/

./target/release/spk-host index \
  --repo-dir build/repo-prod/x86_64 \
  --repo spaceos-main \
  --arch x86_64 \
  --key ~/.config/spaceos/spk/keys/spaceos-main.key

./target/release/spk-host verify \
  --repo-dir build/repo-prod/x86_64 \
  --key ~/.config/spaceos/spk/keys/spaceos-main.pub
```

### 9.2 上传顺序

1. 先上传新的 `.spk`；
2. 再上传 `index.json.sig`；
3. **最后**上传 `index.json`（原子 `mv`）。

这样客户端不会看到“新索引指向不存在的包”。

缓存规则：

- `index.json`、`index.json.sig`：`Cache-Control: no-cache`；
- `.spk`：长期 immutable；
- 保留 Range/206，支持断点续传。

### 9.3 上线验收

```sh
spk-host verify --repo-dir <下载目录> --key spaceos-main.pub
jq -r '.packages[] | "\(.name) \(.version)-\(.release)"' index.json
```

再从 guest：

```sh
doas spk sync
doas spk install foo
```

### 9.4 安全

- 私钥永不进入 Git、镜像、构建产物或日志。
- 只发布公钥；私钥只放在受控机器的 `~/.config/spaceos/spk/keys/`。
- 轮换时使用双签：新旧 key 同时签同一 `index.json.sig`，客户端信任新 key 后再停用旧 key。
- 吊销用 `revoked_keys` / `revoked.d`，不要删除历史记录。

---

## 10. 规范与检查清单

### 10.1 命名与版本

- 包名小写，避免与系统包冲突。
- 上游版本变化改 `version`；只改打包内容改 `release`。
- 不要复用已发布的 `version-release`；需要替换就增加 release。
- epoch 只在版本排序规则变化时增加。

### 10.2 许可

- `license` 使用 SPDX 表达式。
- 包含第三方二进制时，增加 `THIRD_PARTY_NOTICES.md` 并在描述中说明。
- 不要把 GPL/MPL 组件标成单一 Proprietary；参考 `packages/base` 的写法。

### 10.3 路径

- 只允许绝对安装路径；单路径不超过 511 字节。
- 不得写入 `/var/lib/spk`、`/etc/spk/trusted.d`。
- 不要创建设备、FIFO、socket。
- 符号链接目标必须留在安装根内。

### 10.4 发布前检查

```text
[ ] manifest 字段完整，summary/license 非空
[ ] version-release 已递增
[ ] build.sh 使用 install -m，staging 无临时文件
[ ] conffiles 都在 files 中
[ ] scripts=true 时 .INSTALL 存在且函数正确
[ ] triggers 有对应 /usr/lib/spk/triggers/<name>.trigger
[ ] setuid 包有 allow_setuid + setuid.allow + root 属主
[ ] 包含第三方代码时有 notices/license
[ ] make packages repo 通过
[ ] spk-host verify 通过
[ ] QEMU/host 测试通过
[ ] 生产 index 用生产 key 重签
[ ] 上传顺序：包 → sig → index
[ ] 私钥没有进入提交
```

---

## 11. 常见错误

### `error: no repositories configured`

目标机没有 `/var/lib/spk/repos.json`。桌面镜像通过 overlay 配置远程源；P5 镜像配置本地源。

### `exists in filesystem`

目标路径存在但不属于任何包。使用：

```sh
spk install --overwrite '/usr/bin/foo' foo
```

或先删除该文件。属于其他包时，用 `replaces` 或 `--overwrite`。

### `package foo ... differs between repositories`

两个仓库有同名同版本但字节不同的包。只保留一个仓库，或让本地仓库镜像生产包；不要同时启用不同构建的同一版本。

### `setuid ... is owned by uid N`

setuid 文件不是 root 所有。安装必须以 root 运行；spk 会校验并回滚。检查 `/etc/spk/setuid.allow` 和包内模式。

### `bad signature` / exit 3

索引签名与信任 key 不匹配。确认 `index.json.sig` 是最新索引的签名，且客户端信任了对应公钥。

### `scripts=true` 但 `.INSTALL` 缺失

`spk-host pack` 会拒绝。检查 staging 根下是否有 `.INSTALL`，manifest 是否写了 `scripts = true`。

### trigger 不执行

- manifest `triggers` 是否匹配事务改动的路径；
- `/usr/lib/spk/triggers/<name>.trigger` 是否是普通文件；
- 事务是否真的改动命中路径；
- `spk repair` 是否已重跑 marker。

---

## 12. 参考

- 计划与阶段：`docs/PACKAGE_MANAGER_PLAN.md`
- 示例包：`packages/hello`、`packages/sudo`、`packages/base`
- 构建脚本：`tools/build_packages.sh`、`tools/build_repo.sh`
- 打包/签名：`user/spk/spk-host`
- 目标端：`user/spk/spk`
- QEMU 矩阵：`tools/spk_qemu_test.py`
- 生产公钥：`packages/keys/ed25519:b4280a08406a53c5.pub`
