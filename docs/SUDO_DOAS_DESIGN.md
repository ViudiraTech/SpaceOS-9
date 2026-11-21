# SpaceOS-9 sudo / doas 设计

> 状态：已实现（2026-11-21）。内核 setuid/setgid exec 已落地；`user/sudo`
> 与 `packages/sudo` 完成，QEMU 全链路通过。主路线是 setuid 二进制；
> `sudod` 仅作为无 setuid 内核的备用设计保留，不在当前构建中。

## 1. 方案选择

内核现在在 `execve` 中实现 Linux 语义：

- `S_ISUID` / `S_ISGID` 分别把 euid/suid、egid/sgid 设为文件属主/属组；
- `no_new_privs` 与 `nosuid` 挂载抑制提权；
- `fsuid/fsgid` 重置为新的 euid/egid；`AT_SECURE=1`；进程 `dumpable=0`；
- shebang 只按最终解释器取 setuid 位，`noexec` 挂载拒绝执行。

因此 `sudo`/`doas` 采用 **setuid root 二进制**，无需常驻守护进程。若未来移植到
没有 setuid exec 的内核，保留 `sudod` 设计：Unix socket +
`SO_PEERCRED`/`SCM_RIGHTS`，客户端把 uid/tty/stdio 交给 root 守护进程。

## 2. 组件

```
user/sudo/
├── src/lib.rs             共享：配置、认证、ticket、lecture、审计、执行
├── src/bin/sudo.rs         sudo CLI
└── src/bin/doas.rs         doas CLI
packages/sudo/
├── manifest.toml           allow_setuid = true
├── files/etc/sudoers.example
├── files/etc/doas.conf.example
└── files/usr/bin/sudo       mode 4755
```

安装后：

| 路径 | 模式 | 说明 |
|---|---|---|
| `/usr/bin/sudo`、`/usr/bin/doas` | 4755 root | setuid root |
| `/etc/sudoers` | 0440 root | 策略（不存在时 fail closed 或内置 %wheel/%sudo 规则） |
| `/etc/doas.conf` | 0440 root | doas 策略 |
| `/run/sudo/ts/<uid>` | 0600 root | ticket |
| `/var/lib/sudo/lectured/<uid>` | 0600 root | 已训诫标记（`-k` 不清除） |
| `/var/log/sudo.log` | 0600 root | 审计 |

## 3. 认证

- 直接读取 `/etc/passwd`、`/etc/shadow`（setuid root 下可读）。
- 支持 `$6$`（SHA-512 crypt，`sha-crypt` crate，纯 Rust）、`$5$`（SHA-256 crypt）
  与 `$y$`（yescrypt，`yescrypt` crate，纯 Rust）。
- 空密码：OOBE 对空字符串生成 `$6$` 哈希；shadow 哈希为空或为上述哈希时，
  只接受空密码。`!`/`*` 锁定账户、过期账户始终拒绝。
- 未知哈希算法 fail closed 并输出明确错误；不降级为明文。
- 密码从 `/dev/tty` 读取，关闭回显；Ctrl-C 清理并退出；`-S` 从 stdin 读取。
- 失败计数存 `/run/sudo/faillock/<uid>`：默认 3 次、指数退避（1s/2s/4s）、
  锁定 5 分钟；成功清零。计数文件 0600 root。

## 4. Ticket 与 lecture

- 默认 15 分钟，按 `(user, tty)`；`timestamp_type=global` 时忽略 tty。
- `-v` 刷新；`-k` 删除当前 ticket；`-K` 删除该用户全部 ticket。
- `lecture = once|always|never`，默认 once；`lecture_file` 可覆盖文案。
- 首次成功认证输出计划 10.6 原文（英文；LANG/SPK_LANG 为 zh 时输出 ReliefOS
  中文版），然后写 lectured 标记；`-k` 不清除该标记。
- 首次成功授权另有一次性 notice：“First successful authorization. This notice
  is shown once.”（中文对应文案）；空密码账户追加安全警告。该 marker 与
  lecture marker 独立，认证失败不会写成首次成功。

## 5. 策略

### 5.1 sudoers 子集

```
Defaults lecture=once,timestamp_timeout=15,secure_path="..."
User_Alias ADMINS = alice, bob
Host_Alias LOCAL = spaceos9, localhost, ALL
Cmnd_Alias PKG = /usr/bin/spk
alice LOCAL=(root) NOPASSWD: PKG
%wheel ALL=(ALL) ALL
```

- 支持 `Defaults`、`User_Alias`、`Host_Alias`、`Cmnd_Alias`、
  `user host=(runas) [NOPASSWD:] command`。
- 用户：名字、`#uid`、`%group`；主机：hostname/`ALL`；runas：用户、`%group`、`ALL`。
- 命令：绝对路径与尾部 `*` 通配；参数匹配留到后续；相对命令拒绝。
- 配置解析失败、文件非 root 属主或权限宽于 0440：**fail closed**。

### 5.2 doas.conf

```
permit nopass :wheel as root cmd /usr/bin/spk
deny :guest cmd /usr/bin/sh
```

- `permit|deny [nopass] [as user] cmd path`；组规则 `:wheel`。
- 与 sudoers 同时存在时，`doas` 只读 doas.conf，`sudo` 只读 sudoers。

## 6. 执行

- 目标用户/组：`-u`、`-g`；默认 root；`-i` 以登录 shell 运行，`-s` 以用户 shell。
- 子进程按 `setgroups` → `setgid` → `setuid` 顺序切换；root 目标直接设置
  real/effective/saved 为 0。父进程保持 euid root 直到退出。SpaceOS 内核的
  `setgid` 特权判断已按 Linux 修正为 `euid == 0`（4755 sudo 的 egid 仍是调用者）。
- 环境默认清空，只保留 `TERM/HOME/USER/LOGNAME/SHELL/LANG/LC_*`；
  `PATH` 使用 `secure_path`；`-E`/`Defaults env_keep` 按白名单保留。
- 命令必须为绝对路径或在 `secure_path` 中找到；不经过 shell 拼接，argv 原样传递。
- 命令退出码原样返回；被信号杀死时返回 128+signal。
- `-e`（安全编辑）已实现：临时目录 0700 归属调用者、编辑器以调用者身份运行、
  原子写回并保留原文件模式；`sudoedit` 策略按命令 `/usr/bin/sudoedit` 匹配。

## 7. 审计与安全

- 每次请求记录：时间、uid、目标用户、tty、cwd、argv、策略结果、退出码。
- setuid 进程：清理环境、关闭非标准 fd、使用绝对路径、校验配置文件属主/权限、
  `dumpable=0`（内核 setuid exec 已设置）、忽略危险的 `LD_*`/`PATH` 注入。
- 不实现 PAM；直接 shadow 校验。密码错误时统一文案，避免泄露账户状态。
- 配置缺失时：内置仅 `%wheel`/`%sudo` 需密码规则，或 fail closed（由
  `Defaults`/构建选项决定）；默认选择内置 `%wheel` 需密码，并告警。

## 8. spk 集成

- `packages/sudo/manifest.toml` 设置 `allow_setuid = true`；spk 安装时告警并记录。
- spk 读取 `/etc/spk/setuid.allow`：允许的 setuid 路径白名单（每行一个绝对路径，
  `#` 注释）。manifest 带 setuid 位但路径不在白名单：**拒绝安装**并回滚。
- sudo 包升级：spk 事务备份旧二进制；失败回滚保留旧 sudo。
- 已实现：安装/升级包含 setuid 文件的包时，spk 要求 `/etc/spk/setuid.allow`
  为 root 所有、非组/其他可写，且每个 setuid 路径在名单内；否则拒绝并回滚。
- P3 hooks：安装/升级前用 sudo 自身解析器校验 `/etc/sudoers`/`/etc/doas.conf`
  （等价 `visudo -c`），失败中止事务。当前版本尚未接入该 hook。

## 9. 测试矩阵

- 单元：sudoers/doas 解析、用户/主机/runas/命令匹配、shadow `$6$` 校验、
  lecture once/always/never、ticket 过期/`-k`/`-K`、环境白名单、拒绝锁定账户。
- QEMU（2026-11-21，46 步全通过）：uid 1000 测试用户 + shadow + sudoers；
  密码认证、首次授权 notice、ticket、`-k`、NOPASSWD、`-u empty`（非 root
  目标 setgroups/setgid/setuid）、sudoedit、`-l`、doas、OOBE 空密码用户、
  审计；spk 以 4755 安装 sudo 并校验 `/etc/spk/setuid.allow`，测试后卸载。
- 内核：setuid/setgid exec 探针（uid 1000 → euid/egid 0）；`nosuid` 挂载抑制；
  `no_new_privs` 抑制；非 setuid 对照；setuid-root 进程 setgid 到非 0 组。

## 10. 开放问题

- PAM 不实现；账号过期/密码时效字段的完整语义待补。
- 命令参数级匹配与通配语义（当前支持绝对路径与尾部 `*`）。
- `sudoers` 的 `Defaults`/别名已支持子集，完整 visudo 语法与 `@include` 待补。
- `sudod` 备用方案的 socket 权限与 fd 传递细节（仅无 setuid 内核时启用）。
- sudoers/doas.conf 校验 hook 随 P3 hooks；sudo 包升级/事务回滚的 QEMU 测试随 P3。
