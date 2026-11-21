# SpaceOS-9 原生包管理器（spk）完整执行计划

> 状态：设计定稿草案，未开始实现。
> 来源：本文件由 Uinxed-Kernel 会话整理，供 SpaceKernel 会话继续执行。
> 目标仓库：`~/Project/SpaceKernel`。
> 托管：FadeHost Standard（app `goutoustdio`，域名 `https://goutoustdio.top`）。

## 0. 定位与原则

`spk` 是 SpaceOS-9 的原生包管理器，目标是成为**完整、可长期维护**的系统组件，而不是最小演示。

设计原则：

1. **语义完整**：依赖求解、冲突/替代/提供、文件冲突、conffile、安装脚本、钩子/触发器、签名信任、镜像、回滚、修复、多架构、离线仓库，全部纳入设计。
2. **风格向 pacman / apk 看齐**：输出短、直接、可脚本化；不使用 apt 的冗长 “Reading package lists... / The following NEW packages will be installed...” 文案。
3. **事务正确性优先**：任何失败都必须能回滚；文件数据库与磁盘状态一致；崩溃可恢复。
4. **可复现**：包、索引、ISO 都能从源码重复构建。
5. **安全默认**：签名索引、逐文件哈希、路径逃逸防护、脚本沙箱、setuid 白名单。

暂不规划：A/B 系统 OTA；先完成 `spk upgrade` 与依赖求解。

## 1. 能力清单（完整目标）

仓库与同步：

- 多仓库、优先级、镜像与故障切换；本地目录仓库（ISO/离线）。
- 同步数据库、增量更新、索引签名与信任链。
- 包检索、按名称/提供者/文件查询。

安装与变更：

- install / remove / upgrade / downgrade / reinstall。
- 依赖求解（含冲突、替代、提供、虚拟包）、事务集合计算。
- 文件冲突检测、目录共享、conffile 三向合并策略。
- 安装脚本、post-transaction hooks、文件触发器（triggers）。
- 原子安装、失败回滚、断电/中断恢复、`repair`。
- 升级前备份、降级、保留旧包。

质量与安全：

- 逐文件 SHA-256、包签名、索引签名、密钥轮换/吊销。
- `verify`（文件数据库校验）、`check`（依赖完整性）、`audit`（历史记录）。
- 路径逃逸/符号链接逃逸/设备文件防护；setuid 默认拒绝。
- 脚本沙箱（命名空间/只读源/超时/禁网，按需）。

其他：

- 多架构（x86_64 / aarch64 / riscv64）独立索引与交叉构建。
- 并行下载、Range 断点续传、缓存与保留策略。
- 完整日志、事务 ID、可解析输出（`--json`）、退出码。
- 本地化（默认英文，提供 zh_CN 等 gettext 目录）。
- 前端接口：CLI 为第一前端；GUI/系统更新器通过稳定 CLI/daemon 接口接入。

## 2. 术语与命名

- `spk`：包管理器二进制；`spkd`（可选）：常驻下载/事务代理。
- `.spk`：包文件，tar + zstd。
- `index.json`：每个仓库/架构的包索引；`index.json.sig`：分离签名。
- `world`：用户显式安装的包集合（类似 pacman 的 world / apk 的 world）。
- `transaction`：一次安装/卸载/升级的完整事务，带唯一 `txid`。
- `conffile`：配置文件，升级时保留用户修改。
- `hook`/`trigger`：包安装后按条件执行的系统动作。

## 3. 包格式规范

### 3.1 结构

```
foo-1.2.3-1-x86_64.spk
├── manifest.toml          # 必需
├── .INSTALL               # 可选：pre/post install/upgrade/remove 脚本
├── .CHANGELOG             # 可选
├── .BUILDINFO             # 可选：构建环境、源码、编译器版本
├── usr/bin/foo
├── usr/lib/libfoo.so
└── usr/share/foo/...
```

- tar 条目排序、mtime、uid/gid 固定，保证可复现。
- `.spk` 为 tar + zstd。目标端 `spk` 只依赖纯 Rust `ruzstd` 解码；host 侧打包使用 bundled C zstd，C zstd 不进入目标端依赖树。
- 路径必须相对、无 `..`、无绝对路径、无设备/FIFO；符号链接目标不得逃逸安装根。
- 路径总长不超过 511 字节：SpaceOS syscall 的路径缓冲为 512 字节（含 NUL），Linux 的 PATH_MAX 4096 不适用。`/var/lib/spk` 与 `/etc/spk/trusted.d` 为保留前缀，包不得写入。
- manifest 文件表带 `type`（`file`/`dir`/`symlink`）与 symlink 的 `target`；目录 `sha256="" size=0`，symlink 的 `sha256`/`size` 对目标字符串计算。
- 文件模式/属主记录在 manifest；setuid/setgid 默认剥离，manifest 显式 `allow_setuid` 才保留。
- 硬链接安排在 P2 之后支持；P1 先按普通文件处理并告警。

### 3.2 manifest.toml

```toml
name = "foo"
epoch = 0
version = "1.2.3"
release = 1
arch = "x86_64"
abi = "spaceos-1"
summary = "..."
description = "..."
license = "MIT"
url = "https://..."
packager = "..."
build_date = 1760000000

depends = ["bar>=2.0", "baz"]
provides = ["foo-abi=1"]
conflicts = ["foo-legacy"]
replaces = ["foo-old"]

conffiles = ["/etc/foo.conf"]
scripts = true
allow_setuid = false

[[files]]
path = "/usr"
type = "dir"
mode = "0755"
uid = 0
gid = 0
sha256 = ""
size = 0

[[files]]
path = "/usr/bin/foo"
mode = "0755"
uid = 0
gid = 0
sha256 = "..."
size = 12345

[[files]]
path = "/usr/bin/foo-link"
type = "symlink"
target = "/usr/bin/foo"
mode = "0777"
sha256 = "<SHA-256 of the target string>"
size = 11
```

- `type` 缺省为 `file`。目录条目没有摘要与大小；symlink 的 `sha256`/`size` 对 `target` 字符串计算。
- `files` 由 `spk build` 扫描 staging 自动生成；源 manifest 可省略 `files`，生成后再次校验，`conffiles` 也在生成后校验。手工清单仅用于校验。
- 包文件名：`epoch = 0` 时为 `name-version-release-arch.spk`；`epoch != 0` 时为 `name-epoch:version-release-arch.spk`（与 Arch 包文件名一致），因此 epoch 不会撞名。
- 依赖语法：`name`、`name=1.2.3-1`、`name>=1.2`、`name<2`、`name>1`、`name<=2`；`provides` 支持虚拟包与 ABI（`splibc-abi=1`）。
- `conflicts`/`replaces` 进入求解器；`replaces` 可在同一事务中替换旧包。
- 可选 `optdepends`（仅展示，不强制）。

### 3.3 脚本约定

`.INSTALL` 支持：

- `pre_install` / `post_install`
- `pre_upgrade` / `post_upgrade`
- `pre_remove` / `post_remove`

约定：

- 脚本以 `/bin/sh` 执行，传入 `$1`（新版本）等参数，风格类似 pacman。
- 默认在受限环境运行：只读源根 + staging 根、无网络、超时、独立进程组。
- manifest `scripts = true` 才允许；签名索引必须覆盖脚本。
- 失败策略：`pre_*` 失败中止事务；`post_*` 失败记录并回滚（可配置）。

## 4. 仓库规范

### 4.1 布局

```
/spaceos/repo/
├── x86_64/
│   ├── index.json
│   ├── index.json.sig
│   ├── foo-1.2.3-1-x86_64.spk
│   └── ...
├── aarch64/
└── riscv64/
```

可选的 per-repo 元数据：

```
/pool/<name>/<filename>        # 大仓库可用 pool 布局
/deltas/<from>-<to>.spkdelta  # 可选增量
```

### 4.2 index.json

```json
{
  "schema": 1,
  "repo": "spaceos-main",
  "arch": "x86_64",
  "generated_at": 1760000000,
  "key_id": "ed25519:...",
  "packages": [
    {
      "name": "foo",
      "epoch": 0,
      "version": "1.2.3",
      "release": 1,
      "arch": "x86_64",
      "abi": "spaceos-1",
      "filename": "foo-1.2.3-1-x86_64.spk",
      "size": 12345,
      "sha256": "...",
      "depends": ["bar>=2.0"],
      "provides": ["foo-abi=1"],
      "conflicts": [],
      "replaces": [],
      "summary": "..."
    }
  ]
}
```

- `index.json.sig` 为分离签名，覆盖 index 原文；客户端按 `key_id` 查信任库。
- 仓库可提供 `index.json.zst` 压缩版，客户端优先取压缩版。
- 镜像：同一仓库可有多个 URL；客户端按优先级/延迟切换，签名保证一致性。
- 保留策略：每个包保留最近 N 个版本（默认 3），`.spk` 内容不可变，长缓存。

## 5. 依赖与求解

### 5.1 版本比较

- 顺序：`epoch` > `version` > `release`。
- `version` 按点分段：数字段按数值比较，字母段按字典序，数字段大于字母段（类 Arch/alpm 规则）。
- 提供者约束：`provides` 可带版本；虚拟包版本参与比较。

### 5.2 求解器

目标：完整、可解释、可回滚的依赖求解。

- 输入：world + 目标操作 + 仓库集合。
- 约束：depends、conflicts、replaces、provides、已安装包、显式版本约束。
- 算法：回溯 + 冲突驱动（类似 SAT/DPLL 思路）；对每个候选版本尝试，冲突时回溯；输出完整事务集合。
- 失败时输出冲突链（pacman 风格）：
  ```
  :: installing foo (1.2.3-1) breaks dependency 'foo<1.2' required by bar
  ```
- 循环依赖：允许，按 SCC 处理（安装顺序内成环的包一起提交）。
- 部分升级策略：默认拒绝（类似 pacman `-Syu` 的全量升级）；`--allow-partial` 仅调试用。
- 求解结果可 `--print` 输出事务计划，供脚本消费。

## 6. 数据库与事务

### 6.1 状态文件

```
/var/lib/spk/
├── installed.json        # name -> {version, arch, files, depends, ...}
├── files.json            # path -> {pkg, sha256, mode, size}；只记录非目录路径
├── world.json            # 用户显式安装集合
├── repos.json            # 仓库列表、优先级、最后同步时间
├── journal               # 当前/最近事务日志（txid、阶段、已写文件）
├── backup/<txid>/        # 升级前旧文件备份
└── staging/<txid>/       # 解包暂存
```

- 所有状态文件：写临时文件 → `fsync` → `rename`；目录 fsync。
- `files.json` 不记录目录：目录归属从 `installed.json` 各包的 `files` 派生；共享目录只有在为空时才移除。
- `journal` 为 JSONL 追加日志，每条意图在动作前写入并 `fsync`；`spk repair` 依据 journal 逆序回滚或前滚。
- 锁：`/var/lib/spk/lock`（flock），同一时间只允许一个写事务。

### 6.2 事务阶段

1. `resolve`：求解、生成计划、用户确认。
2. `download`：并行下载到 `/var/cache/spk`，校验 sha256。
3. `verify`：校验包签名/索引签名、manifest 与文件哈希。
4. `stage`：解包到 staging，做路径安全检查。
5. `conflict`：文件冲突、目录/文件类型冲突、conffile 冲突。
6. `backup`：备份将被覆盖/删除的旧文件。
7. `commit`：逐文件原子写入（临时文件 + rename），更新权限/属主。
8. `scripts`：按顺序执行 pre/post 脚本。
9. `db`：更新 `files.json`/`installed.json`/`world.json`。
10. `hooks`：运行 post-transaction hooks 与文件触发器。
11. `cleanup`：清理 staging、旧缓存、写审计日志。

失败处理：

- 阶段 1–4 失败：无系统变更。
- 阶段 5–9 失败：按 journal 逆序回滚，恢复 backup。
- 阶段 10 失败：记录错误，事务仍视为已提交；提供 `spk repair --hooks` 重跑。

### 6.3 conffile 策略

- 首次安装：写入文件，记录 hash。
- 升级：用户未改 → 覆盖；用户改过且新版本也改 → 生成 `.spksave` 并提示；用户改过但新版本未改 → 保留。
- 卸载：默认保留 conffile（改名 `.spksave`），`--purge` 删除。
- 提供 `--diff` 查看差异。

### 6.4 卸载与共享目录

- 反序删除文件；目录仅在空时删除。
- 多包共享的目录不删除；文件所有权数据库精确到路径。
- `--recursive` 卸载无依赖者的孤立依赖（类似 `pacman -Rs`）。

## 7. 钩子与触发器

### 7.1 post-transaction hooks（pacman 风格）

声明式钩子，放在 `/etc/spk/hooks.d/*.hook` 或包内：

```ini
[Trigger]
Operation = Install
Operation = Upgrade
Type = Path
Target = usr/lib/firmware/*

[Action]
Description = Updating firmware index...
When = PostTransaction
Exec = /usr/bin/spk-firmware-index
```

- `Operation`：Install/Upgrade/Remove。
- `Type`：Package/Path。
- 按 `When`（PreTransaction/PostTransaction）执行；输出风格类似 pacman “Running post-transaction hooks...”。

### 7.2 文件触发器（apk 风格）

包可声明 trigger（例如 `busybox` 的 applet 链接）：

```
trigger = ["/bin/*"]
```

安装/卸载命中路径后，先收集，再统一执行一次，输出：

```
Executing busybox-1.36.1-r5.trigger
```

### 7.3 脚本沙箱

- 默认：`chroot` 到 staging + 只读源根，禁用网络，超时 60s，限制输出。
- `--no-sandbox-scripts` 仅调试；签名索引 + 显式 `scripts = true` 才能运行。

## 8. 签名与信任

- 仓库密钥：ed25519；`index.json.sig` 分离签名；包本身可选内嵌签名。
- 客户端信任库：`/etc/spk/trusted.d/<key_id>.pub`；ISO 内置初始公钥。
- 信任级别：`trusted` / `marginal` / `revoked`；`require_signature` 默认 true。
- 密钥轮换：新旧双签过渡；吊销列表随索引发布。
- 防降级：索引可带 `min_version`/`revoked_versions`；客户端拒绝已知坏版本。
- 首次信任（TOFU）仅允许在显式 `--trust-key` 时使用，并记录告警。

## 9. CLI 规格

### 9.1 命令

```
spk sync / update                 # 同步索引
spk install <pkg>...              # 安装（可带版本约束）
spk remove <pkg>...               # 卸载
spk upgrade [<pkg>...]            # 升级
spk reinstall <pkg>...
spk downgrade <pkg>=<ver>
spk list / info / search / files / owns
spk verify / check / repair / audit
spk repo add|remove|list|priority
spk build <dir> -o <out.spk>
spk index <repo-dir> --sign <key>
spk keygen / sign / trust
spk clean / cache
spk --print / --json / --noconfirm / --dry-run
```

### 9.2 退出码

- 0 成功
- 1 一般错误
- 2 用法错误
- 3 签名/校验失败
- 4 依赖/冲突失败
- 5 文件冲突
- 6 事务回滚（系统已恢复）

### 9.3 输出原则

- 短、可解析、可脚本化；默认英文，gettext 支持 zh_CN。
- 面向人的进度用 `::` 前缀（pacman 风格），包操作用 `(n/m)` 计数（apk/pacman 风格）。
- 错误用 `error:` / `ERROR:`，警告用 `warning:` / `WARNING:`，保持与经典工具一致的辨识度。
- 不使用 apt 的 “Reading package lists... / Building dependency tree... / The following NEW packages will be installed:” 风格。

## 10. 经典文案（pacman / apk 风格）

以下为 `spk` 输出文案基线；中文本地化保持同样结构。

### 10.0 文案混合规则

`spk` 的文案是**有意混合** pacman 与 apk 的经典句式，按场景分工，避免同一类信息出现两种风格：

| 场景 | 采用 | 基线示例 |
| --- | --- | --- |
| 同步、升级摘要、事务确认、post-transaction hooks | pacman | `:: Synchronizing package databases...`、`Packages (n)`、`:: Proceed with installation? [Y/n]`、`:: Running post-transaction hooks...` |
| 单包安装/升级/删除、完成语、文件触发器 | apk | `(1/1) Installing foo (1.2.3-r0)`、`Executing foo-1.2.3-r0.trigger`、`OK: 123 MiB in 45 packages` |
| 索引缺失、依赖不可满足 | apk | `WARNING: Ignoring index.json: No such file or directory`、`ERROR: unsatisfiable constraints:` |
| 目标不存在、事务/文件冲突、签名未知信任 | pacman | `error: target not found: foo`、`error: failed to prepare transaction`、`error: signature from "..." is unknown trust` |
| sudo 首次训诫 | sudo / ReliefOS zh_CN | 见 10.6 |
| zh_CN 同步与无变更 | pacman zh_CN | `:: 正在同步软件包数据库...`、` 今日无事可做` |

规则：

- `::` 只用于阶段标题（pacman 风格）；包级进度统一 `(n/m)`（apk/pacman 共用）。
- 包操作动词固定为 `Installing` / `Upgrading` / `Removing` / `Reinstalling`（apk 风格）；结束语统一 `OK: N MiB in M packages`。
- 事务类错误用 `error:` / `warning:`；索引/约束类用 `ERROR:` / `WARNING:`。同一类信息不要混用两种前缀。
- 中文：同步、升级摘要、无变更沿用 pacman zh_CN；apk 风格的行可以保留英文，或按统一术语表翻译，但不得逐条混搭。
- 不使用 apt 的 “Reading package lists...” / “The following NEW packages will be installed” 风格。

### 10.1 同步与事务摘要（pacman 风格）

```
:: Synchronizing package databases...
 core is up to date
 extra is up to date
:: Starting full system upgrade...
resolving dependencies...
looking for conflicting packages...

Packages (2) foo-1.2.3-1  bar-2.0-1

Total Download Size:    1.23 MiB
Total Installed Size:   4.56 MiB
Net Upgrade Size:       0.78 MiB

:: Proceed with installation? [Y/n]
(2/2) checking keys in keyring
(2/2) checking package integrity
(2/2) loading package files
(2/2) checking for file conflicts
(2/2) checking available disk space
:: Processing package changes...
(1/2) installing foo
(2/2) installing bar
:: Running post-transaction hooks...
(1/1) Arming ConditionNeedsUpdate...
```

### 10.2 安装与结束语（apk 风格）

```
fetch https://goutoustdio.top/spaceos/repo/x86_64/index.json
(1/1) Installing foo (1.2.3-r0)
Executing foo-1.2.3-r0.trigger
OK: 123 MiB in 45 packages
```

### 10.3 错误与警告（两家的经典句式）

```
error: target not found: foo
error: failed to prepare transaction (could not satisfy dependencies)
:: installing foo (1.2.3-1) breaks dependency 'foo<1.2' required by bar
error: package 'foo' is not installed
error: signature from "SpaceOS Build <build@spaceos>" is unknown trust
warning: /etc/foo.conf installed as /etc/foo.conf.spksave

WARNING: Ignoring index.json: No such file or directory
ERROR: unsatisfiable constraints:
  foo (missing):
    required by: world[foo]
WARNING: This spk-tools is OLD! Some packages might not function properly
```

### 10.4 卸载/查询

```
checking dependencies...

Packages (1) foo-1.2.3-1

Total Removed Size:  4.56 MiB

:: Do you want to remove these packages? [Y/n]
(1/1) removing foo
```

```
Name            : foo
Version         : 1.2.3-1
Architecture    : x86_64
Depends On      : bar>=2.0  baz
Provides        : foo-abi=1
Installed Size  : 4.56 MiB
Install Date    : ...
Install Reason  : Explicitly installed
```

### 10.5 约定

- 确认提示统一 `[Y/n]`；`--noconfirm` 跳过。
- 进度计数 `(n/m)`；长操作可 append 百分比。
- `::` 仅用于阶段标题；`error:`/`warning:` 前缀用于诊断。
- 所有面向机器的信息支持 `--json`。

### 10.6 sudo 首次训诫

```
We trust you have received the usual lecture from the local System Administrator.
It usually boils down to these three things:

    #1) Respect the privacy of others.
    #2) Think before you type.
    #3) With great power comes great responsibility.
```

中文（zh_CN，取自 ReliefOS / Alpine 系发行版的经典本地化文案，作为 `spk` 与 `sudo` 的中文基线）：

```
我们信任您已经从系统管理员那里了解了日常注意事项。
总结起来无外乎这三点：

    #1) 尊重别人的隐私。
    #2) 输入前要先考虑（后果和风险）。
    #3) 权力越大，责任越大。

为安全起见，您键入的密码将不可见。

密码：
```

- `lecture = once|always|never`，默认 once（按用户）。
- `lecture_file` 可覆盖默认文案；`sudo -k` 不清除训诫记录。
- 实现细节见第 20 节。

### 10.7 pacman 中文基线（zh_CN）

`spk` 的 zh_CN 文案直接对齐 pacman 的经典翻译，保持“短、直接”的风格：

```
:: 正在同步软件包数据库...
 core 已经是最新版本
 extra 已经是最新版本
:: 正在进行全面系统更新...
 今日无事可做
```

完整事务：

```
正在解析依赖关系...
正在查找软件包冲突...

软件包 (2) foo-1.2.3-1  bar-2.0-1

全部下载大小：    1.23 MiB
全部安装大小：    4.56 MiB
净更新大小：      0.78 MiB

:: 进行安装吗？[Y/n]
(1/1) 正在检查密钥环中的密钥
(1/1) 正在检查软件包完整性
(1/1) 正在加载软件包文件
(1/1) 正在检查文件冲突
(1/1) 正在检查可用磁盘空间
:: 正在处理软件包的变化...
(1/1) 正在安装 foo
:: 正在运行事务后钩子...
```

约定：

- “今日无事可做”是 pacman zh_CN 对 “ there is nothing to do” 的经典译法，`spk` 在无变更时沿用。
- 包名后的状态用 “已经是最新版本”；错误用 `错误：`/`警告：` 或保留 `error:`/`warning:`，两者择一后全局统一。
- 不使用 apt 的 “正在读取软件包列表... / 正在分析软件包的依赖关系树...” 风格。

## 11. 构建集成

- 包定义：`packages/<name>/manifest.toml` + 可选 `build.sh`；产物进 staging。
- `tools/build_userland.sh` 产出 staging；`spk build` 生成 `files` 与 `.spk`。
- Makefile：
  - `make packages`：构建所有包。
  - `make repo`：生成并签名索引。
  - `make image`：用 `spk` 把 base 包装入 rootfs，生成文件数据库。
- 可复现：`SOURCE_DATE_EPOCH`、排序 tar、固定 mtime/uid/gid、记录编译器与源码版本。
- ISO 内置本地仓库（只读目录或 `/repo`），离线可安装；首次联网后加在线仓库。
- CI：构建包 → 生成索引 → QEMU 安装测试 → 发布。

## 12. FadeHost 托管（goutoustdio）

`goutoustdio-site` 是 Express 应用，`server/index.js` 已有 `app.use(express.static(pub))`。增加持久盘路由：

```js
app.use("/spaceos/repo", express.static("/data/spaceos/repo", {
  index: false,
  etag: true,
  lastModified: true,
  setHeaders(res, filePath) {
    if (filePath.endsWith("index.json") || filePath.endsWith(".sig"))
      res.set("Cache-Control", "no-cache");
    else
      res.set("Cache-Control", "public, max-age=31536000, immutable");
  },
}));
```

- `/data` 为 FadeHost 持久盘（Standard 10 GB，当前仅用 26 MB）。
- URL：`https://goutoustdio.top/spaceos/repo/<arch>`。
- 包文件放 `/data/spaceos/repo/`，不提交 Git。
- 发布脚本：构建 → 签名 → 上传 GitHub Releases/R2 → app 内 `curl` 到 `/data` → 原子替换 index。
- 10 GB 预算：只放 `.spk`/index/delta；保留 N-2 版本；70% 告警；大文件走 R2。
- `public/CNAME` 目前是 `goutoustdio.rainyland.top`，使用 `goutoustdio.top` 时同步。
- 备份 `/data/spaceos/repo` 到 R2；恢复流程写入运维文档。

### 12.1 发布记录

- 2026-11-21：P0 仓库发布到 `https://goutoustdio.top/spaceos/repo/x86_64`。
  - `hello-0.1.0-1-x86_64.spk`：sha256 `fbd694046e2c82917628347c175c48266396c7cd39deb2db8ac0752d83663399`
  - `index.json`：sha256 `2efbe0d047420d9cdcf4cd33c1e8d7b0d46e292c73f4f6624bc0d6695b704e43`
  - `index.json.sig`：sha256 `6e00275464758c6df0abf2dadf73dcdf110a052c82da5c1bafb34c10c3437b77`
  - 公钥：`https://goutoustdio.top/spaceos/repo/keys/ed25519:b4280a08406a53c5.pub`，key_id `ed25519:b4280a08406a53c5`
  - 目录浏览：`https://goutoustdio.top/spaceos/repo/`（serve-index details 视图，隐藏点文件）
  - 线上 `spk-host verify`：`hello 0.1.0-1 ... OK`、`index.json: OK`
  - 站点已加入“SpaceOS 9 软件源”入口（下载页按钮、导航、页脚、首页卡片、介绍页）
  - `index.json`/`.sig` no-cache；`.spk` immutable；Range 206 已验证

- 2026-11-21：P1 发布 `hello 0.2`，`index.json` 同时包含 0.1 / 0.2。
  - `hello-0.2.0-1-x86_64.spk`：sha256 `d8d8ccf956c5b12e61554a69f6b56588f0167ff3c1f128a16ecaa4fa55524c6d`
  - `index.json`：sha256 `ac9c15d90cb6b9a0684d02f8aa5a89bca6af794d1e0bf7cea3f9e8e4d0c1488c`
  - `index.json.sig`：sha256 `c4bfb44c511949dc5511ae490633118af06fa4c6480f6b1acc84bfec7a8987cd`
  - 线上 `spk-host verify`：hello 0.1.0-1 OK、hello 0.2.0-1 OK、index.json OK
  - 公钥未变；0.1 保留，支持 downgrade

- 2026-11-21：P2 发布 `sudo 0.1.0`，索引同时包含 hello 0.1/0.2 与 sudo。
  - `sudo-0.1.0-1-x86_64.spk`：sha256 `67931fc98e552994ef7903bf8be99f64f7fa3cd91a4dcbdedfdaf02912943b99`
  - `index.json`：sha256 `78cefc4c62ff6c24ce966d47fa2778fce3ab11c716649c6d7e4452c14e341e75`
  - `index.json.sig`：sha256 `c2a17fea6216a8e59c6965bed322d6d9cc2d97aedad646d61232fb7260c853a8`
  - 线上 `spk-host verify`：hello 0.1/0.2、sudo 0.1、index.json 全部 OK
  - sudo 安装前置：镜像需提供 root:root、0644 的 `/etc/spk/setuid.allow`，包含 `/usr/bin/sudo`、`/usr/bin/doas`

- 2026-11-21：P5 发布 `base 1.0.0`，生产索引加入 base，hello 0.1/0.2 与 sudo 0.1 保持不变。
  - `base-1.0.0-1-x86_64.spk`：sha256 `0768cab88e0ddfe9f05f64575b0fe0efc9b533545501d5f087a497a54ad0368c`
  - `index.json`：sha256 `6e24cbae45f857153ae7ba32936c42ba51db7db8e18e2c6ee68e2a23ea615ff5`
  - `index.json.sig`：sha256 `07520436d2aad7d144d5afdd626406ad1a53f8ad2d120fd217103a2193204e9b`
  - 线上 `spk-host verify`：base 1.0、hello 0.1/0.2、sudo 0.1、index.json 全部 OK
  - base 包含 spk、spk-sandbox、BusyBox、musl、systemg、CA bundle；安装前置 `/etc/spk/setuid.allow`
  - P0–P5 已全部合并到 master，HEAD `1059e74bb6457d1df6c091ad23142e1f13f2bf72`

- 2026-11-21：`base 1.0.0-2` 修正 license metadata 与重复文件；生产索引替换 1.0.0-1。
  - `base-1.0.0-2-x86_64.spk`：sha256 `f1e429fa6c75d113c83164a3a07198507e81d5089e7081efe84bf14c36d04a7f`
  - `index.json`：sha256 `22771ac7c32ec83eecbe5a0ee7d0bb9647b553f734d5a78563a28868e8385d07`
  - `index.json.sig`：sha256 `94c70a9bc082eddf2c1384a2db2cb407e0d3be6216a0833436b44a93cbd05b2e`
  - `/bin/sh`、`/lib/libc.musl-x86_64.so.1` 改为符号链接；解包体积 10.20 MiB → 8.48 MiB
  - license：`LicenseRef-Proprietary AND GPL-2.0-only AND MIT AND MPL-2.0`；包内新增 `/usr/share/doc/base/THIRD_PARTY_NOTICES.md`
  - 旧 `base-1.0.0-1` 已从生产仓库删除；`SOURCE_DATE_EPOCH=1764000000`

- 2026-11-21：`base 1.0.0-3` 修正 spk 属主/setuid 安装；生产索引替换 1.0.0-2。
  - `base-1.0.0-3-x86_64.spk`：sha256 `d2c9b88ec64c3ab5b578c4f9b9086212d54ed04328bd38644ce00de1993c4e96`
  - `index.json`：sha256 `99d9a06bae58539af92e8271231c4d5242a566db322c8295be277f169e1ed87b`
  - `index.json.sig`：sha256 `ce81af6ec07290b8805033ba9942f1a950f0b24fd115bc9509e2e9a2ecb9f764`
  - spk 现在要求 live 事务 euid 0，按 manifest uid/gid chown 文件/目录/符号链接，并校验 setuid 文件必须是 root 所有
  - `spk check` 校验磁盘属主/权限，`spk repair` 修正；QEMU 覆盖非 root 拒绝、sudo root:root 4755、repair
  - 旧 `base-1.0.0-2` 已从生产仓库删除；`SOURCE_DATE_EPOCH=1795255334`

## 13. 安全模型

- 威胁：恶意仓库、篡改镜像、坏包、路径逃逸、脚本提权、降级攻击、磁盘耗尽。
- 对策：签名索引 + 逐文件 hash + 路径/符号链接校验 + setuid 白名单 + 脚本沙箱 + 防降级 + 磁盘配额 + 事务回滚。
- 信任边界：`spk` 以 root 运行；所有输入（index、manifest、tar、脚本）视为不可信。
- 日志与审计：事务 ID、操作者、时间、包版本、结果；`spk audit` 可查。

## 14. 性能与规模

- 并行下载（默认 4 连接）、Range 续传、缓存校验。
- 索引按架构拆分；大型仓库支持 zstd 压缩索引。
- 文件数据库按路径排序存储，查找 O(log n) 或哈希索引。
- 大量小文件安装使用批量 rename + 目录 fsync，避免逐文件全局锁。
- 包解压流式处理，避免整包读入内存。

## 15. 多架构

- 每个 arch 独立 `index.json`；包名/版本相同但 arch 不同。
- manifest 必填 `arch`；`abi` 与 `splibc` 版本参与依赖。
- 交叉构建：`tools/build_*.sh` 产出对应 staging；`spk build --arch`。
- 客户端只接受匹配当前 arch 的包；`--allow-foreign` 仅用于调试。

## 16. 测试计划

Host 侧（Rust 单元/集成）：

- 版本比较、依赖求解、冲突/替代/提供、环检测。
- 索引生成/签名/验签、manifest 校验、JSON schema。
- tar 安全：`..`、绝对路径、符号链接逃逸、设备文件、超长路径、稀疏文件。
- 事务状态机：每个阶段注入失败，验证回滚与 journal 恢复。

QEMU 侧（扩展现有 `tools/qemu_test.py`）：

- 安装/卸载/升级/降级/重装；孤立依赖清理。
- 缺失依赖、冲突、文件冲突、conffile 三向合并。
- 坏包（hash/签名/截断）、磁盘满、只读根、安装中 kill -9。
- hooks/triggers 顺序与幂等；脚本沙箱逃逸测试。
- 多架构镜像安装测试。

模糊测试：

- index.json/manifest.toml 解析；tar 路径；版本字符串。

CI：

- 构建 → 生成包/索引 → QEMU 全矩阵 → 发布仓库。

## 17. 实施阶段（完整目标）

### P0 规范与骨架

- 定稿 manifest/index/DB schema；Rust workspace；`spk-host` 的 keygen/index/sign；单元测试。
- 验收：本地仓库可生成、签名、验签。

### P1 包、事务与完整升级

- tar/zstd、sha256、staging、文件 DB、锁、原子安装/回滚、journal。
- `install/remove/list/info/files/owns`。
- 完整 `upgrade`：版本比较、depends/provides/conflicts/replaces 求解、事务集合、`downgrade/reinstall`、world 一致、冲突链输出；不做“仅无冲突时可用”的简化版。
- 验收：QEMU 本地仓库 hello 0.1 → 0.2 升级成功；构造依赖冲突/文件冲突/升级失败，事务回滚；中断后 repair；world 与 DB/文件一致。
- 实现记录（2026-11-21）：`user/spk/spk`（musl 静态）、信任库与本地/file 仓库、回溯求解、install/upgrade/downgrade/reinstall/remove(-Rs)/check/repair；host 集成 10 项与 QEMU 三阶段矩阵通过（含升级中 SIGKILL + repair、ext4 回放）。

### P2 求解器边界与高级变更

- 复杂依赖图、循环依赖、虚拟包/提供者、hold/pin、部分升级策略、多仓库优先级下的求解。
- 冲突诊断、求解性能、`--print`/`--json` 事务计划。
- 验收：复杂图、冲突、循环、hold、多仓库测试全过；求解结果可复现。
- 实现记录（2026-11-21）：hold/pin 与 held-back 警告、`--override-hold`；多仓库配置（priority、同一仓库多镜像 failover、跨仓库同版本一致性诊断、坏签名 fail-closed）；`--print`/`--json` 确定性事务计划与 JSON 错误；环/虚拟提供者/replaces/文件与 conffile 边界；proptest fuzz（版本、manifest、index、tar 路径）与 120 包链性能测试；`pkgver` 收紧为首尾字母数字，保证 alpm `rpmvercmp` 在合法域内为全序。host 测试 78 项、QEMU 46 步通过：hold 阻止升级→unhold→升级、base+update 多仓库、sudo/doas 全链路。

### P3 脚本、钩子、触发器

- `.INSTALL`、hooks.d、文件 triggers、沙箱、幂等与失败策略。
- 验收：busybox 式 trigger、升级脚本、沙箱逃逸测试。
- P3b 实现记录（2026-11-22，spk/sudo/CI）：`spk-sandbox` 在 private mount namespace 中挂 ramfs 根，`/stage` 可写 bind、`/source` 只读 bind、`/bin`/`/lib`/`/usr` 只读 bind（BusyBox ash 固定为 `/bin/sh`）、`/dev` devfs、`/proc hidepid=2`；优先 `pivot_root`、失败回退 `chroot`，fd>2 关闭、rlimit、setsid/进程组、60s、输出上限、清空环境、`spk-script` uid/gid 61111 无附加组/无 passwd。seccomp 拒绝网络、mount/umount/pivot/chroot/setns/unshare、mknod、ptrace、bpf/perf/userfaultfd/io_uring、keyctl、process_vm、open_by_handle_at、reboot/kexec/module、clone 带 `CLONE_NEW*`（clone3 直接拒绝），允许 execve。`.INSTALL` 支持 pre/post install/upgrade/remove、staging 哈希与未声明路径拒绝、失败回滚、`--script-failure=keep` 与 `--no-sandbox-scripts` 调试；hooks.d Pre/PostTransaction、系统 hook root、包 hook 沙箱、描述与输出、marker 去重；文件 triggers glob、每包每事务一次、journal marker；sudo `--check-config`/`--check-doas` 在文件冲突前校验；`/etc/spk/setuid.allow` 进入 `build_rootfs.py` 与 QEMU rootfs 并收紧为 root:root 0644；QEMU 矩阵与 CI job 接入。
- P3a 实现记录（2026-11-22，内核）：`openat2(437)`（`open_how`/RESOLVE_* /OPENAT2_REGULAR/O_NOFOLLOW/O_PATH）、`chroot(161)` 与 per-task root、private mount namespace（`clone(CLONE_NEWNS)`/`unshare(272)`）、`mount(2)` 5 参数/MS_BIND/MS_REC/MS_RDONLY/MS_REMOUNT、bind 子树、只读写拒绝、`pivot_root(155)`、procfs `hidepid=1|2`、`seccomp(317)` cBPF（ALLOW/ERRNO/KILL/TRAP/LOG、TSYNC/LOG、no_new_privs、fork/exec 继承）。新增用户态 P3 探针与 boot-self-test 通过；P3b 将在此之上实现脚本/hooks/triggers/sudo 校验。

### P4 仓库与信任

- 多仓库/镜像/优先级、索引压缩、签名信任库、密钥轮换/吊销、防降级。
- 验收：坏签名/坏镜像/降级攻击被拒绝。
- P4 实现记录（2026-11-23，spk/spk-host/内核/QEMU）：
  - 传输层为内置 HTTP/1.1（`std::net::TcpStream`）+ `rustls` 0.23/ring，支持重定向（拒绝 HTTPS→HTTP 降级）、`Content-Length`/chunked、连接/读写超时、CA bundle（`/etc/ssl/certs/ca-certificates.crt`，`SPK_CA_BUNDLE` 仅测试）、Range/206/416；目标端静态 musl 构建时需要 `CC_x86_64_unknown_linux_musl=clang AR_x86_64_unknown_linux_musl=ar`。
  - 远程索引缓存位于 `var/cache/spk/repos/<repo-id>/`：`snapshots/<index-sha>/index.json|index.json.sig|meta.json`、原子 `current`、`security.json`；索引/签名请求带 `Cache-Control: no-cache`，支持 ETag/If-None-Match 304；失败保留旧索引并回退（`stale`），坏签名/坏索引/安全回退 fail-closed。
  - 包缓存 `var/cache/spk/packages/<sha256>.spk`，下载使用 `.spk.part` + Range 续传，校验大小与 sha256 后原子 rename；镜像 failover、重试/退避、24 小时 `.part` 清理、30 天/2 GiB 保留策略。
  - 信任库 `/etc/spk/trusted.d/<key_id>.pub`，`verify_any` 支持主签名 + `signatures` 多签格式（生产仍是单 key `ed25519:b4280a08406a53c5`）；本地 `/etc/spk/revoked.d/<key_id>[.revoked]` 与签名内 `revoked_keys` 均拒绝。host `spk-host index --key ... --key ...` 写多签，`--security <json>` 写可选 `min_version`/`revoked_versions`/`revoked_keys`；空字段使用 `skip_serializing_if`，生产现有 index/sig 字节不变。
  - 防降级：本地 `security.json.generated_at` 形成回滚下限；签名内 `min_version`/`revoked_versions` 被求解器过滤，显式安装/降级目标版本直接拒绝；`--allow-unsigned` 仅用于调试，缺失签名可接受但存在的坏签名绝不接受。
  - CLI：`spk sync`（别名 `update`）、HTTP(S) `--repo`、`repo add`、全局 `--offline`/`--allow-unsigned`；配置仓库名作为缓存 id，支持同一仓库多 URL 镜像。
  - 测试：host `http_remote` 13 项（200/304/404/500 重试/超时/重定向/Range/ETag/坏签名/坏索引/CA/多镜像/多签/CLI HTTP 安装/sync/offline/安全回退）；host 总计 86 项；QEMU 71 步（63 P3 + 8 P4）使用 `-nic user,model=e1000e`，加载 `e1000e.ko`，通过 rtnetlink 配置 `eth0`，本地 HTTP 仓库跑 install/upgrade/remove/offline。内核补 `RTM_NEWLINK` 路由请求（复用已有 `link_set`），不改 syscall 编号/结构。

### P5 构建、镜像与运维（含文件冲突覆盖）

- P5 实现记录（2026-11-23，构建/镜像/运维）：
  - `tools/build_packages.sh`：统一 `SOURCE_DATE_EPOCH`，排序 staging、0:0、固定 mtime，生成 `.BUILDINFO`，产出 hello 0.1/0.2、sudo 0.1 和 `base` 1.0；相同 `SOURCE_DATE_EPOCH` 重建 sha256 一致。
  - `packages/base`：`spk`、`spk-sandbox`、BusyBox/`/bin/sh`、musl loader/libc、systemg、CA bundle；保留路径 `/etc/spk/setuid.allow`、`trusted.d` 不入包，由镜像写入。
  - `tools/build_repo.sh`：`spk-host index` 签名、可选 `SPK_SECURITY`、生产 key 通过 `SPK_REPO_KEY`/`SPK_REPO_PUBLIC` 注入；本地默认 key 只放 `build/keys`。
  - `tools/build_p5_image.py`：以 host `spk` 把 base 装进镜像，写 `/etc/spk/setuid.allow`（root:root 0644）、trusted bundle、`/var/lib/spk/repos.json`、内嵌 `var/lib/spk/repo/x86_64`；生成确定性 ext4（固定 UUID/hash seed）和 FAT32（固定 volume id）；`tools/build_p5_image.py` 复制 P5 init 驱动。
  - `make packages`、`make repo`、`make image`、`make p5-isos`；ISO 通过 Limine 内嵌 FAT32 rootfs，rootfs 也作为 QEMU 可写 ext4 盘。
  - 运维：`spk cache info|clean`（保留当前索引引用、`.part`、年龄/容量策略）；`spk status --json` 稳定 GUI JSON；事务成功写 `/var/log/spk/audit.log` JSONL；`spk verify` 校验索引和全部包，新增中文输出；P5 host 测试 90 项（含 2000 包索引、并发审计、缓存保留）。
  - `tools/spk_p5_qemu_test.py`：ISO 内 `p5.mode=verify` 校验 + ext4 rootfs 离线 install/upgrade/remove；CI `spk-p5` job 构建镜像并跑离线矩阵。
  - 桌面发布镜像（`release-try`）通过 `tools/overlay_p5_packages.py` 写入 `spk`/`spk-sandbox`、生产公钥和远程 `https://goutoustdio.top/spaceos/repo/x86_64` 配置，不嵌入本地 repo；`make all` 只构建该桌面候选，P5 本地验证镜像需显式 `make image`。`spk --help`/常见 Clap 错误在中文环境使用中文文本。
- 文件冲突覆盖（2026-11-21）：`install/upgrade/downgrade/reinstall --overwrite <绝对 glob>` 可重复；无 owner 覆盖、有 owner 转移 files DB 归属并从旧包 files/conffiles 移除、`replaces` 合法接管、目录存在不冲突；无 flag 且非交互/`--noconfirm` 保持 exit 5，交互终端提供 y/N；相对路径、`..`、坏 `[` glob 退出 2。文案含 `spk owns`/`replaces=` 提示，`spk --help` 与常见 Clap 错误在中文环境使用中文。
- 验收：从零 `make image` 后 ISO 内 `spk verify` 全绿；QEMU 离线仓库安装/升级/卸载；现有 86 host + 71 QEMU 不回归。

## 18. 目录结构建议

```
SpaceKernel/
├── docs/PACKAGE_MANAGER_PLAN.md
├── user/spk/                 # Rust workspace
│   ├── spk-core/             # schema、版本、签名、tar 安全/解包（目标端可直接用）
│   ├── spk-host/             # keygen/pack/index/sign/verify
│   └── spk/                  # 目标端 CLI + 求解/事务/repair/check
├── packages/
│   ├── hello/                # 0.1.0 / 0.2.0
│   ├── sudo/                 # setuid sudo/doas 包
│   └── spk-tests/            # solver/file-conflict/setuid fixtures
├── user/sudo/                # sudo/doas（setuid 路线）
├── docs/SUDO_DOAS_DESIGN.md
├── tools/
│   ├── build_local_repo.sh
│   └── spk_qemu_test.py
└── repo/                     # 构建产物（gitignore）
```

## 19. 风险与开放问题

- ABI 稳定性：`spaceos-1` 的升级策略；splibc 与 musl 两条用户态路线的边界。
- `spk` 自身链接 splibc 还是 musl；若要求全原生，需要自定义 Rust target/sysroot。
- zstd 已定稿：目标端只用 `ruzstd` 解码，host 侧打包用 bundled C zstd；目标 `spk` 正常依赖树不含 `zstd`/`zstd-sys`。`ruzstd` 对超大/多帧包的性能仍需在 P4 压测。
- epoch 命名已定稿：非 0 时进入文件名（见 §3.2），不再保留为开放问题。
- SpaceOS syscall 路径缓冲为 512 字节（含 NUL），包路径上限 511 字节；build/index/install 都会拒绝超长路径。
- VFS `openat` 尚未实现 `O_NOFOLLOW`，祖先符号链接会被跟随；`spk` 用 lstat 级联解析把写入限制在 `--root` 内，但仍存在 TOCTOU 窗口，待内核补齐后收紧。
- `statvfs` 没有独立 syscall；`spk` 的磁盘空间检查使用 `statfs`。
- `copy_file_range`（x86_64 syscall 326）未实现；std 会回退并打印 unimplemented 告警。`spk` 的备份/恢复改用 read/write 复制，目标端不再触发该路径。
- 已实现（2026-11-21）：内核 setuid/setgid exec（euid/suid、egid/sgid、fsuid/fsgid 重置、`no_new_privs` 与 `MS_NOSUID` 抑制、`AT_SECURE`、dumpable=0、`MS_NOEXEC`/`MS_NOSUID` 挂载标志）。修复 `setgid`/`setregid`/`setresgid`/`setfsgid` 以 egid==0 判断特权、导致 4755 setuid-root sudo（egid 仍为调用者）无法切换主组的问题；按 Linux 语义改为 euid==0。QEMU sudo/doas 全链路（含 `-u` 非 root 目标）通过。
- OOBE 空密码已支持：DDM 对空字符串生成 `$6$` 哈希并验证；sudo 接受空 shadow 哈希/空密码；首次授权成功提示包含空密码警告；OOBE 界面注明“空密码账户首次 sudo/doas 授权会收到一次性安全提示”。
- P3a 内核原语已落地：`openat2` + `O_NOFOLLOW/O_PATH` + `RESOLVE_BENEATH/IN_ROOT/NO_SYMLINKS/NO_XDEV/NO_MAGICLINKS`；`chroot` 使 `FsContext.root` 参与绝对路径/绝对符号链接/`..` 解析；private mount namespace 与 bind/只读挂载；procfs `hidepid`；seccomp cBPF。沙箱仍按 P3b 以非 root `spk-script` + 只读 source + staging 视图组合使用。
- P3a 的 mount namespace 目前是 private 传播语义；shared/slave peer group 传播、seccomp `USER_NOTIF/NEW_LISTENER/TRACE` 未接入（沙箱不使用），P4 仍未接入，列入后续。
- P4 的线上索引暂不写 `min_version`/`revoked_versions`/`revoked_keys`，也不启用多签/轮换；客户端已兼容缺失字段。需要撤销或轮换时由维护者用 `spk-host index --security` 生成新 index/sig，并更新信任库。
- SpaceKernel 尚未实现 `SIOCGIF*`/`SIOCSIF*` 网络 ioctl；QEMU HTTP 测试改用 rtnetlink（`RTM_NEWLINK`/`RTM_NEWADDR`/`RTM_NEWROUTE`），并在内核补齐 `RTM_NEWLINK` 分发。BusyBox `ip` 的 ioctl 路径仍不可用。
- P3b 沙箱以非 root `spk-script` 运行 `.INSTALL`/包 hook/trigger，系统级动作由 root-owned `/etc/spk/hooks.d` 承担；`.INSTALL` 在文件提交前执行 pre/post，失败时无系统写入可回滚（等价回滚结果）；`--no-sandbox-scripts` 仅调试并打印警告。
- `spk-sandbox` 及其脚本/沙箱依赖进入 P5 base 镜像；P3b 通过 QEMU rootfs 验证，生产 ISO 仍需打包 `spk`/`spk-sandbox` 与 BusyBox。
- `pkgver` 现要求首尾为字母数字；alpm `rpmvercmp` 对前导/尾随分隔符的字符串不满足传递性（与上游一致），fuzz 在合法 `pkgver` 域验证全序，避免求解器排序出现不一致。
- 已修复（2026-11-21）：ext4 `extent_truncate()` 释放目录数据块时使用 `metadata=false`，没有写 revoke；掉电后 jbd2 回放会把旧目录块覆盖到复用该块的文件（QEMU 第三阶段复现 `files.json` 指向目录块）。修复为目录数据块按元数据释放并生成 revoke。QEMU 三阶段在阶段 1↔2 不调用 `sync`、强制日志回放：boot 2/3 分别回放 14/98 个事务、6/28 条 revoke，最终阶段 sync 供离线检查，矩阵全部通过。
- TLS 根证书与离线信任链；`rustls` 在 SpaceOS 上的 ring 后端可用性。
- FadeHost `/data` 在 redeploy/迁移/备份恢复后的持久性需实测。
- 10 GB 仓库容量与保留策略；R2 作为大文件后端的接入。
- 多架构 CI 成本；交叉编译工具链。
- 脚本沙箱在 SpaceOS 上的实现（命名空间/seccomp 等价物是否具备）。
- `optdepends` 的展示语义尚未定稿；`replaces` 已在 P1 求解器中实现（同事务移除被替换包）。

## 20. 特权工具：sudo / doas（与 spk 同期）

实现状态（2026-11-21）：采用 setuid 路线。内核 setuid/setgid exec 已落地；`user/sudo` 实现 sudo/doas、sudoers/doas.conf 子集、sha512-crypt/yescrypt、ticket/faillock、10.6 lecture、首次授权提示、审计、sudoedit、环境清理；`packages/sudo` 通过 spk 的 `/etc/spk/setuid.allow` 白名单以 4755 安装。QEMU 全链路（密码、ticket、NOPASSWD、`-u empty`、sudoedit、`-l`、doas、空密码、`-k`）通过。

目标：SpaceOS 提供原生特权提升工具，兼容 sudo 常用用法，同时支持 doas 式极简配置；与 spk 的 setuid 白名单、conffile、包事务集成。

### 20.1 前置条件

- 内核支持 setuid exec，且 `spk` 允许 `sudo` 保留 setuid 位；若内核暂不支持，改用 `sudod` 守护进程 + Unix socket + `SO_PEERCRED`/`SCM_CREDENTIALS` 的模型。
- `/etc/shadow`、密码哈希（sha512-crypt/yescrypt）、`/etc/passwd`、`/etc/group`。
- TTY/会话标识、审计日志。

### 20.2 CLI

```
sudo [-u user] [-g group] [-i|-s] [-E] [-k|-K] [-v] [-l] command [args...]
sudo -e file...
```

- 默认只允许 `wheel`/`sudo` 组；策略来自 `/etc/sudoers` 子集或 `/etc/doas.conf`。
- `-k` 清除 ticket，`-v` 延长，`-l` 列出规则，`-e` 安全编辑。
- 环境：重置 PATH、清除 LD_*；`--preserve-env` 走白名单。

### 20.3 认证与 ticket

- 读取 shadow 校验密码；失败计数 + 指数退避 + 可配置锁定。
- ticket 默认按 (user, tty) 15 分钟；`timestamp_type=tty|global`；文件放 `/run/sudo/ts/`，0600。
- 无 TTY 默认拒绝交互式认证（可配置）。

### 20.4 配置

- `/etc/sudoers` 子集：`Defaults`、`User_Alias`、`Host_Alias`、`Cmnd_Alias`、`user host=(runas) NOPASSWD: cmd`。
- doas 兼容：`/etc/doas.conf` 的 `permit|deny [nopass] [as user] cmd`。
- 解析失败 fail closed；配置权限 0440 root。

### 20.5 首次训诫（lecture）

- `lecture = once|always|never`，默认 once；`lecture_file` 可覆盖。
- 首次运行输出第 10.6 节的英文/中文训诫，并写入 timestamp。
- `sudo -k` 不清除训诫记录（与 sudo 一致，可配置）。

### 20.6 安全

- setuid 二进制的环境、文件描述符、信号处理；禁止 PATH/LD_* 注入。
- 首次成功授权输出一次性 notice；空密码账户在同一 notice 中显示安全警告；marker `lectured/<uid>` 与 10.6 lecture 独立，`sudo -k` 不清除。
- 命令使用绝对路径解析；`secure_path`。
- 审计记录 user/tty/cwd/command/结果；可选日志后端。
- 密码不回显；超时清理；Ctrl-C 不泄漏。

### 20.7 打包与 spk 集成

- `packages/sudo/`：二进制、`/etc/sudoers` 与 `/etc/doas.conf` 示例。
- manifest `allow_setuid = true`，spk 安装时告警并记录到 DB。
- hook：安装/升级前做 sudoers 语法检查（等价 `visudo -c`）。
- 升级 sudo 时保留旧二进制备份，失败回滚。

### 20.8 测试

- 认证成功/失败/锁定/超时；ticket 过期与 `-k`。
- sudoers/doas 规则矩阵、NOPASSWD、runas、命令别名。
- 环境注入、PATH、fd 泄漏、无 TTY。
- lecture once/always/never；zh_CN 文案。
- setuid 安装/卸载/升级回滚。

### 20.9 里程碑

- 与 spk P1/P3 同期：先落地 `sudod`（或 setuid 版本），再做 sudoers/doas 解析、lecture、ticket、审计。
- 验收：普通用户按规则执行命令；未授权拒绝；审计完整；spk 事务可回滚。

### 20.7 空密码与首次授权

- OOBE/DDM 允许空密码：`hash_password(b"")` 生成 `$6$` 哈希，`verify_password(b"", hash)` 通过；登录与 sudo 共用该语义。
- shadow 哈希为空（OOBE 空密码）或为 `$6$`/`$5$`/`$y$` 时，sudo 只接受空密码；`!`/`*` 锁定始终拒绝。
- 首次成功授权（密码或 NOPASSWD 规则）输出“First successful authorization...”一次性提示；空密码账户追加警告；lecture 10.6 仍按 `lecture=once|always|never` 独立控制。
- 失败计数/锁定与首次授权 marker 分离，避免认证失败被当成“首次成功”。

## 21. 任务清单

- [x] 定稿 manifest/index/DB schema（第 3、4、6 节）。
- [x] 建 Rust workspace 与 `spk-host`：keygen/index/sign。
- [x] 实现 `.spk` 打包/解包、sha256、tar 安全检查。
- [x] 实现文件 DB、锁、原子安装、journal/repair。
- [x] 实现 `install/remove/list/info/files/owns` 并 QEMU 验证。
- [x] 实现完整 `upgrade`（求解/替换/回滚）并 QEMU 验证。
- [x] 实现版本比较与依赖求解器、冲突链输出。
- [ ] 实现 `.INSTALL`、hooks.d、triggers、脚本沙箱。
- [x] 实现多仓库/镜像优先级/故障切换/签名信任。
- [x] 实现防降级、密钥轮换/吊销格式（P4；线上暂不启用安全字段/多签）。
- [x] 给 `goutoustdio-site` 提 `/spaceos/repo` 路由 PR，部署 `hello` 测试仓库。
- [ ] 接入 `make packages/repo/image`，ISO 用包构建。
- [x] 接入 QEMU 三阶段测试矩阵（含 P2 hold/多仓库与 sudo/doas）。
- [ ] 接入 CI。
- [ ] 多架构与 R2 备份/大文件后端。
- [x] 实现 sudo/doas（认证、ticket、sudoers/doas.conf、lecture、审计、sudoedit）。
- [x] spk 支持 setuid 白名单（`/etc/spk/setuid.allow`，root 所有、非组/其他可写）。
- [ ] sudoers/doas.conf 校验钩子（随 P3 hooks）。
- [x] sudo setuid 安装/卸载与凭据矩阵的 QEMU 测试。
- [ ] sudo 包升级/事务回滚的 QEMU 测试（P3）。
- [x] P2 hold/pin、多仓库、`--print`/`--json`、复杂图/环/provides/replaces、fuzz 与性能测试。
- [x] P3a 内核：openat2/O_NOFOLLOW/O_PATH、chroot、mount namespace/bind/RO/pivot_root、proc hidepid、seccomp cBPF 与内核/QEMU 测试。
- [x] P3b：.INSTALL/hooks.d/triggers、非 root 沙箱、sudoers/doas 校验、setuid 包升级/降级、QEMU 矩阵与 CI。
- [x] P4：HTTP(S) 仓库、镜像优先级、防降级、密钥轮换/吊销格式；seccomp USER_NOTIF/TRACE 与完整 shared mount propagation 仍留待后续。
