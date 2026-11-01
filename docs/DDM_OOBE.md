# DDM 与 OOBE 设计说明

日期：2026-11-01。状态：随 SPUI 4.0 / SpaceOS 9 桌面镜像交付。

本文说明 SpaceOS 9 的图形登录栈。目标不是做一个“能输入密码的窗口”，而是把显示管理器的特权边界、首次启动事务、账户数据库写入、X11 授权和会话生命周期都固定下来，避免继续使用 root 自动登录。

## 1. 交付范围

| 组件 | 运行身份 | 职责 |
| --- | --- | --- |
| `spui ddm` | root | 启动 Xorg、生成 Xauthority、校验 shadow、创建账户、启动/回收会话、处理电源操作 |
| `spui greeter` | `ddm`（系统账户，uid 990） | 全屏登录界面、用户/会话选择、密码输入、错误提示、电源确认 |
| `spui oobe` | `ddm` | 首次启动向导：语言/键盘/时区、管理员账户、主机名、自动登录 |
| `ddm.service` | root（systemg） | 启动 DDM，失败时由 systemg 重新拉起 |
| `console.service` | root（getty） | 串口 `getty` + `login`，不再直接启动 root shell |
| `/usr/share/xsessions/spui.desktop` | root-owned | 告诉 DDM 用户的桌面会话命令 |

`root` 的 shadow 条目在镜像中就是 `!`（锁定）。OOBE 创建的第一个人类账户加入 `wheel` 组，通过 setuid 的 `doas` 执行管理员命令；图形界面不允许 uid 0 登录，即使 `AllowRoot` 被误设为 true，DDM 也只在显式配置后才会考虑，而默认配置和发布镜像都是关闭的。

## 2. 参考的成熟实现

这些参考用于确定组件边界和状态机，不是把它们的代码直接搬进来：

- [DDM（linuxdeepin/ddm）架构](https://github.com/linuxdeepin/ddm/blob/master/docs/ARCHITECTURE.md)：daemon 管理 seat、显示服务器、认证和会话；helper 单独执行 PAM 事务；greeter 是独立进程，不接触 shadow。SpaceOS 采用同样的“特权 daemon + 非特权 UI”边界，但把 PAM 替换为本地 shadow 校验，把 Wayland/Treeland 替换为 Xorg。
- [SDDM 架构](https://github.com/sddm/sddm/blob/develop/docs/ARCHITECTURE.md)：daemon 负责应用生命周期、seat、显示服务器、认证和电源；greeter 只负责界面。
- [LightDM](https://github.com/canonical/lightdm)：greeter 与 display server 分享 X server 时使用 root-only 的 Xauthority 文件，用户会话拿到自己的 cookie；本实现使用同样的分层文件，并在注销时停止 Xorg、以新 cookie 重启，完成会话间的轮换。
- [GDM](https://gitlab.gnome.org/GNOME/gdm)：登录管理器可以拉起首次设置；本实现的 OOBE 由 DDM 在“没有人类账户且 marker 不存在”时拉起。
- [GNOME Initial Setup](https://gitlab.gnome.org/GNOME/gnome-initial-setup)：首次启动创建第一个用户；Arch Wiki 的 [Out-of-the-Box Experience](https://wiki.archlinux.org/title/Out-of-the-Box_Experience) 也采用“没有用户时由显示管理器触发”的模式。
- [Windows 11 OOBE](https://learn.microsoft.com/en-us/windows-hardware/customize/desktop/customize-oobe-in-windows-11)：把首次设置拆成离散页面、明确重启/恢复行为，并在完成后进入安静期。SpaceOS 的 OOBE 采用分步向导和“完成标记最后写入”的恢复策略。
- [shadow(5)](https://man7.org/linux/man-pages/man5/shadow.5.html) 与 [crypt(3)](https://man7.org/linux/man-pages/man3/crypt.3.html)：密码 aging 字段与 SHA-512 crypt 的语义。

## 3. 信任边界

```text
                    root
┌──────────────────────────────────────────────┐
│ spui ddm                                     │
│  ├─ Xorg（root）                             │
│  ├─ 读取 /etc/shadow                         │
│  ├─ /run/ddm/Xauthority（0600 root）         │
│  ├─ /run/ddm/greeter.sock（0660 root:ddm）   │
│  ├─ 账户事务（passwd/shadow/group）          │
│  └─ fork/setuid/initgroups → 用户会话        │
└───────────────▲──────────────────────────────┘
                │ 有界 TLV 协议 + SO_PEERCRED(uid=ddm)
┌───────────────┴──────────────────────────────┐
│ spui greeter / spui oobe（uid 990, ddm）      │
│  ├─ 只读 UI 配置、字体、壁纸、图标           │
│  ├─ 只持有 greeter 的 Xauthority             │
│  ├─ 不读 /etc/shadow                         │
│  └─ 不能执行 setuid/启动其他用户             │
└──────────────────────────────────────────────┘
```

关键约束：

1. greeter/OOBE 以专用 `ddm` 账户运行，`HOME` 和 `XDG_RUNTIME_DIR` 位于 `/run/ddm` 下，由 DDM 在启动时 chown。
2. `/run/ddm` 为 `0711 root:root`，socket 为 `0660 root:ddm`。DDM 接受连接后用 `SO_PEERCRED` 检查 uid，其他本地用户不能提交认证请求。
3. `/run/ddm/Xauthority` 只允许 root 读取；greeter 拿到的 `greeter.Xauthority` 只包含自己的 entry。用户登录后拿到 `$HOME/.Xauthority` 中的同一份会话 cookie。注销时会话进程组被回收，Xorg 一并停止；下一次 greeter 启动时重新生成 cookie 并重启 Xorg，因此旧会话的 cookie 不能用于下一次登录界面。
4. 会话进程在 fork 后依次执行 `setsid()`、`initgroups()`、`setgid()`、`setuid()`；环境变量由 DDM 重建，不继承 greeter 的环境。
5. root shadow 锁定；控制台使用 `getty`/`login`，不再 `ExecStart=/bin/sh -l`。

## 4. 登录流程

1. systemg 启动 `uxconfd` 和 `ddm`。
2. `ddm` 检查 `/var/lib/spaceos/oobe-complete` 与 `/etc/passwd`：
   - 没有 marker 且没有 uid ≥ 1000 的人类账户 → OOBE 模式；
   - 否则 → greeter 模式。
3. `ddm` 启动 Xorg，写入 root-only server auth，拉起 `spui greeter`（uid 990）。
4. greeter 通过 `/run/ddm/greeter.sock` 发送 `Hello`，DDM 回 `ServerConfig`：主机名、上次用户/会话、会话列表、用户列表、电源能力、OOBE 建议用户名、壁纸。
5. 用户提交 `Login { username, password, session }`。
6. DDM 执行：
   - 用户名/长度检查；
   - 按用户名维度的失败计数和锁定；
   - `getpwnam_r` / `getspnam_r`；
   - 账户 shell、uid 政策、shadow lock、过期/失效字段；
   - `crypt(password, hash)` 恒定时间比较；
   - 成功后才启动会话，并写 `last_user`/`last_session` 与审计日志。
7. DDM 启动会话（`/bin/sh -c <session Exec>`），把 `DISPLAY`、`XAUTHORITY`、`HOME`、`USER`、`LOGNAME`、`SHELL`、`XDG_*`、`LANG`、`TZ` 传入。会话退出后，DDM 回收进程组、撤销 cookie、回到 greeter。

失败路径：

- 未知用户也执行一次 dummy crypt，并施加相同的基础延迟，降低用户名枚举的时序差异。
- 连续失败达到 `MaxFailures` 后锁定该用户名 `LockoutSeconds` 秒；日志只记录用户名和原因，不记录密码。
- 会话启动失败会向 greeter 返回 `SessionFailed`，不会留下半启动的会话。

## 5. OOBE 流程与崩溃一致性

OOBE 只在“没有人类账户且没有完成标记”时出现。它分四步：

1. 语言、键盘布局、时区；
2. 管理员账户：显示名称、用户名、密码、确认密码、密码强度；
3. 系统选项：主机名、自动登录（默认关闭）；
4. 摘要与完成。

DDM 对 `OobeCommit` 做最终校验，然后按以下顺序落盘：

1. 写系统设置（hostname、localtime、`/etc/TZ`、locale、Xorg 键盘配置）；
2. 创建 `/home/<user>`，建立 `.config`、`.local`、`.cache`，chown 给新 uid/gid；
3. 原子替换 `/etc/shadow`（0600）；
4. 原子替换 `/etc/group`（把用户加入 `wheel`、`video`、`input`）；
5. **最后**原子替换 `/etc/passwd`；
6. 最后写 `/var/lib/spaceos/oobe-complete`。

这样：

- 在 passwd 发布前崩溃：没有可见账户，OOBE 可以安全重试；
- passwd 已发布但 marker 未写：DDM 启动时检测到人类账户存在，会把它视为“已中断的 OOBE”，不再显示账户创建界面，而是进入 greeter；
- marker 存在则永不再进入 OOBE。

密码使用 musl `crypt(3)` 的 `$6$`（SHA-512）格式，随机 16 字符 salt。密码策略拒绝常见密码、用户名/显示名称包含、连续字符、过度重复，并对 8–9 位密码要求至少两类字符。OOBE 只把密码保存在进程内存中，提交后立即清零相关缓冲区。

## 6. 会话与电源

- 会话从 `/usr/share/xsessions/*.desktop` 发现；greeter 只能提交 session id，DDM 在 root-owned 目录里解析 `Exec`，不会执行 greeter 传来的命令。
- 会话日志：`/var/log/ddm/session-<user>.log`，0600，归会话用户所有。
- 注销：SPUI 面板的“注销”让 shell 退出；DDM 收到会话子进程退出后，向整个进程组发送 SIGTERM，5 秒后 SIGKILL，撤销 X cookie，并停止 Xorg。下一次 greeter 使用新的 X cookie 和新的 Xorg 实例，旧 cookie 失效。
- 电源：greeter 先弹确认框，DDM 校验 `AllowPower`，写审计日志，关闭子进程后调用 `reboot(2)`；失败才回退到 `/sbin/reboot -f` 或 `/sbin/poweroff -f`。
- 自动登录：默认关闭；OOBE 或 `/var/lib/ddm/state` 显式开启时，DDM 在启动后直接启动会话。它仍然要求账户是 uid ≥ `MinimumUid` 的正常账户，root 永远不会成为自动登录目标。

## 7. 恢复出厂设置

桌面「系统设置 → 关于 → 系统维护 → 恢复出厂设置」提供管理员可通过的恢复入口。第一次点击进入 15 秒确认窗口，第二次点击才向 `uxconfd` 发送 `FACTORY_RESET` 请求。

`uxconfd` 以 root 运行，用 `SO_PEERCRED` 检查请求者 uid，并确认该账户属于 `wheel` 组；通过后写入 root-owned 的 `/var/lib/spaceos/factory-reset`。DDM 在下一次启动时执行恢复：

- 删除所有 uid ≥ 1000 的本地账户及 `/etc/shadow`、`/etc/group` 中对应条目；
- 删除这些账户的 `/home/<name>`；
- 清理 `/var/lib/ddm/state`、OOBE 完成标记和 uxconf 用户覆盖；
- 写入可恢复的 `factory-reset-in-progress` 标记，若中途断电，下次启动会重试；
- 回到 OOBE，由用户重新创建管理员账户。

不会格式化根文件系统，也不会删除内核、系统程序或已安装软件。因此它不是“重装系统”，而是“恢复首次启动状态”。非 `wheel` 用户无法发起该请求。

## 8. 配置

`/etc/ddm/ddm.conf` 的关键项：

```ini
[General]
Display = :0
Xserver = /usr/bin/Xorg
GreeterUser = ddm
SessionDir = /usr/share/xsessions
DefaultSession = spui
AllowRoot = false
MinimumUid = 1000
MaxFailures = 5
LockoutSeconds = 300
FailureDelayMs = 600
AllowPower = true
AutoLogin = false
AutoLoginUser =
```

未知键会直接报错，避免拼写错误悄悄削弱策略。可以用：

```sh
spui ddm --check
```

检查配置、会话文件、人类账户和 OOBE 状态，不启动 Xorg。

## 9. 安全边界与已知限制

- X11 不是同一用户内多个客户端之间的安全边界。DDM 解决的是“不同用户/未认证客户端不能连上显示”，不是同一用户内恶意程序的隔离。
- 同一次 Xorg 生命周期内 greeter 与用户会话使用同一枚 X cookie；注销时 DDM 会停止 Xorg 并以新 cookie 重启，避免旧会话继续访问下一次登录界面。若需要更细的每会话独立 cookie，应改为每次登录都重启 Xorg，或使用 `XCB`/`Xauth` 的在线授权更新接口。
- 当前是单 seat、单 X server、无用户切换；注销会结束整个会话。多 seat/Wayland 是后续工作。
- 本实现不引入 PAM。它覆盖本地 shadow 账户的认证和账户检查，但不支持 PAM 模块（指纹、智能卡、Kerberos、GNOME Keyring 解锁等）。未来接入 PAM 时，应继续把 PAM 事务放在独立 helper 中，避免在 DDM 主进程里执行会 fork 的模块。
- 物理访问者可以重启进入单用户/救援介质；这不是 DDM 能单独解决的问题，需要配合固件密码、磁盘加密和安全启动。
- `crypt(3)` 的 SHA-512 在今天的离线破解成本下不如 Argon2id/yescrypt；这是为了匹配当前 musl/Alpine 用户态的可用接口。后续可切换到 libxcrypt 或自带 KDF，并保留 shadow 格式兼容。
- `doas` 配置为 `permit persist :wheel`，缓存时间由 OpenDoas 管理；公共机器应关闭自动登录并缩短缓存。

## 10. 验收

```sh
# 主机侧单元测试（密码策略、shadow 解析、Xauthority、协议、配置）
cargo test --manifest-path user/desktop/Cargo.toml --target x86_64-unknown-linux-musl

# 配置检查（需要目标 rootfs 环境或 --config 指向测试配置）
spui ddm --check

# 构建 release 桌面镜像
make desktop PROFILE=release
cp build/x86_64/release/SpaceKernel-x86_64.iso build/spacekernel-release-try.iso
# 或一次完成 ISO 与根文件系统
make release-try
```

启动后应观察到：

- 首次启动直接进入 OOBE，四步完成后自动进入 SPUI 桌面；
- 重启后进入 DDM greeter，root 不在用户列表，root 密码无法通过图形界面登录；
- 串口控制台出现 `login:`，root 登录被 shadow 锁定拒绝；
- 注销后回到 greeter，旧会话的 X cookie 已从 server auth 中移除；
- `systemctl status ddm` 显示服务 active，`/var/log/ddm/auth.log` 有 `OOBE_COMPLETE` / `LOGIN_OK` 记录。
