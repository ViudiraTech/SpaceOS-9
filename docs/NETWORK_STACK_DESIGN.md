# 网络栈设计与分阶段验收契约

2026/11/6 By JiTianYu391

目标：按 Linux 的分层与语义自研 IPv4/IPv6 网络栈，提供 BSD socket 与完整 netlink，统一的网络设备注册接口，并让 e1000/e1000e 以可加载 `.ko` 模块形态运行。三架构共享协议栈；架构相关部分（MSI/IRQ、DMA 一致性）只出现在后端。

## 选型（已确认）

| 项 | 决定 |
| --- | --- |
| 协议栈 | 自研，对照 Linux `net/core`、`net/ipv4`、`net/ipv6`、`net/netlink` |
| 协议族 | IPv4 + IPv6（ND、ICMPv6、MLD 后置） |
| Netlink | `af_netlink` 通用核心重构；`NETLINK_ROUTE` 全套；Generic Netlink；`NETLINK_NETFILTER` / `sock_diag` |
| 驱动形态 | e1000/e1000e 为可加载 `.ko`，只能经 `module/api.rs` 导出的 C ABI 访问内核 |

## 参照的 Linux 源码

- `include/linux/netdevice.h`：`net_device_ops`（`ndo_open/stop/start_xmit/set_rx_mode/get_stats64/set_mac_address/change_mtu/tx_timeout`）、`napi_struct`（`poll(napi, budget)`，默认 weight 64，`napi_schedule` / `napi_complete_done`）、`NETDEV_TX_OK/BUSY`、`__LINK_STATE_*`。
- `net/netlink/af_netlink.c`：`nl_table[protocol]`、`netlink_kernel_create(cfg: input, groups, bind, unbind)`、`unicast/broadcast`、`NETLINK_ADD/DROP_MEMBERSHIP`、portid 自动绑定、`rcvbuf` 记账与 `ENOBUFS`、`netlink_dump_start`、`NLM_F_MULTI` / `NLMSG_DONE`、`NLM_F_ACK` / ext-ack。
- `drivers/net/ethernet/intel/e1000/e1000_main.c`（及 `e1000e/netdev.c`）：probe → BAR0 映射 → `reset_hw` → EEPROM 读 MAC → 环形描述符（`TDBAL/TDLEN/TDH/TDT/TCTL/TIPG`，`RDBAL/RDLEN/RDH/RDT/RCTL`）→ `IMS/IMC/ICR` 中断 → 2 秒 watchdog 处理链路与 `TCTL_EN`。

## 分层与模块划分（`src/net/`）

| 层 | 内容 |
| --- | --- |
| `skb` | `SkBuff`：头/数据/尾指针，headroom/tailroom，clone/copy，引用计数；协议头 push/pull |
| `dev` | `NetDevice` 注册表、ifindex、flags/operstate/carrier、MTU、统计、`NetDeviceOps` trait、NAPI 调度（软中断/内核线程）、`netif_rx`、`dev_queue_xmit` + 基础 qdisc（pfifo_fast 风格） |
| `ether` | 以太网封装/解析、`eth_type_trans`、多播地址 |
| `neigh` | ARP / NDISC 邻居表（状态机、重试、老化、gc） |
| `ipv4` / `ipv6` | 收发、分片重组、FIB（最长前缀匹配、多表）、ICMP/ICMPv6、转发开关 |
| `udp` / `tcp` | UDP；TCP 状态机、重传定时器、滑窗、拥塞控制（Reno → CUBIC） |
| `socket` | `net_proto_family` / `proto_ops` 注册，`AF_INET/INET6/NETLINK/UNIX` 统一入口；`SO_*` / `IP_*` / `TCP_*` 选项；`poll/epoll` 接入 |
| `netlink` | 通用核心 + `route` / `genl` / `netfilter` / `sock_diag` 协议 |
| `module_api` | 对 `.ko` 导出的网络设备、NAPI、skb、PCI/DMA/IRQ API |

## 统一设备注册接口

内核内部是 Rust trait；对模块是 C ABI 的 `struct spk_net_ops` + `spk_netdev_alloc/register/unregister`、`spk_napi_*`、`spk_netif_*`，由适配层把 C 回调包装成 `NetDeviceOps`。注册时分配 ifindex、创建 `/sys/class/net/<if>`、发 `RTM_NEWLINK` 与 uevent。注销与 `ndo_stop` 配合，保证无在途 xmit/poll。

## 模块 API 扩展（e1000 `.ko` 所需）

当前只导出 `printk/kmalloc/msleep/udelay`。需新增并保持 ABI 版本递增：

- PCI：`spk_pci_driver_register/unregister`（vendor/device id 表 + probe/remove）、配置空间读写、`enable_device`、`set_master`、BAR 信息与 `ioremap`/`iounmap`、`readl/writel` 内存屏障语义。
- DMA：`spk_dma_alloc_coherent/free_coherent`（返回虚拟+总线地址）、`map_single/unmap_single`、64KB 边界对齐保证。
- IRQ：`spk_request_msi/free_msi`（回调在中断上下文）、`spk_irq_enable/disable`。
- 同步与延迟：自旋锁、`spk_timer_*`、`spk_work_*` / 内核线程、`spk_mdelay`。
- 网络：上节接口。
- 模块卸载：driver 解绑 → 关设备 → 注销 netdev，失败路径全部回滚。

模块 ABI 版本在不兼容变化时递增，`vermagic` 保持校验。

## 阶段划分与验收

1. **基础设施**：`SkBuff`、`NetDevice` 注册表、loopback 设备、`/sys/class/net`、统一 socket 入口，重构 `af_netlink` 核心（保持现有 UEVENT 行为与自测不变）。
2. **L2/L3**：ARP、IPv4、ICMP echo、FIB；`NETLINK_ROUTE` 的 link/addr/route/neigh 与 dump。
3. **传输层**：UDP、TCP（含 epoll/poll），loopback 上的自测。
4. **模块 API + e1000**：扩展导出，e1000 `.ko` 在 QEMU `-device e1000` 下完成 probe、DHCP/ping、收发压力。
5. **e1000e**：82574L 等（MSI-X、`e1000e` 的 PHY/NVM 差异），QEMU `-device e1000e`。
6. **IPv6、genl、netfilter、sock_diag**。

每阶段验收：三架构 `cargo check`、`cargo fmt --check`、`make test` 启动矩阵；QEMU 端到端用例（x86_64 先行，aarch64/riscv64 用 virtio-net 或同一 PCI 路径）；资源回收检查（卸载模块后 skb/DMA/IRQ 无泄漏）。

## 已知前置条件

- 目前 MSI 请求仅存在于 x86_64 的 `arch::apic`；aarch64/riscv64 需要在各自后端补 GIC ITS/ APLIC-IMSIC 或回退到线中断，阶段 4 开始前需要确认。
- 内核尚无通用 DMA 分配接口与软中断/工作队列；阶段 1 先补这两项公共设施。

---

## 实施状态与交接（给后续接手的 AI）

更新：2026/11/7。本节记录当前已完成内容、架构决策、已知边界与标准验证命令。

### 工作区纪律（必读）

- **工作区里存在与网络无关的改动，提交时严禁纳入：** `src/input/mod.rs`、`src/printk/mod.rs`、`src/user/address_space.rs`、`src/user/uaccess.rs`、`src/vfs/pseudo_test.rs`、`pelican-bicycle/`。
- **不要对整个仓库运行 `cargo fmt --all`**：内核文件单独执行 `rustfmt --edition 2024 <file>`，`user/desktop/` 下文件单独执行 `rustfmt --edition 2021 <file>`。
- 编辑任何共享文件前先读取磁盘最新内容，避免覆盖并行改动。

### 已完成内容（阶段 1–6 全量落地 + 用户态 `NetworkManager`）

| 阶段 / 子系统 | 主要文件 | 已实现能力 |
| --- | --- | --- |
| 阶段 1：基础设施 | `src/net/{skb,ether,dev,loopback,timer,socket}.rs`、`src/mm/dma.rs`、`src/net/netlink/{mod,msg,uevent}.rs` | 线性 `SkBuff`；`NetDevice`/`NetDeviceOps`、`/sys/class/net/<if>`、NAPI 软中断线程、发送队列与通知链；`lo` 回环；软件定时器线程；DMA 一致性分配（含 64 KiB 边界约束与 `coherent_bytes` 泄漏记账）与流式映射；`net_proto_family` 统一 `socket(2)` 分派；通用 `af_netlink` 核心与 `NETLINK_KOBJECT_UEVENT` |
| 阶段 2：L2/L3 与路由 | `src/net/{ip,fib,neigh}.rs`、`src/net/ipv4/{header,input,output,fragment,icmp,arp,raw,addr}.rs`、`src/net/netlink/route.rs` | 双栈最长前缀匹配多表 FIB（`local`/`main`）；通用 NUD 邻居状态机（ARP/NDISC 共用队列、重试与老化）；IPv4 收发、源地址选择、MTU 分片与重组、IPv4 选项校验/RR 打标/逆向回传（`ip_options_compile`/`ip_options_echo`/`ip_options_undo`）、ICMP echo/差错、`SOCK_RAW`（含 `IP_HDRINCL`、`ICMP_FILTER`）与非特权 `SOCK_DGRAM`(`IPPROTO_ICMP`) ping 套接字；`NETLINK_ROUTE` 全套 link/addr/route/neigh 增删改查、dump 与 `RTNLGRP_*` 多播 |
| 阶段 3：传输层与 BSD Socket | `src/net/udp.rs`、`src/net/tcp/mod.rs`、`src/net/socket.rs`、`src/ipc/mod.rs` | UDP（IPv4/IPv6、校验和、未配地址时的 DHCPv4 受限广播收发、ICMP 不可达）；TCP（完整状态机、三次握手、Reno 拥塞控制、RTO/Karn、乱序重组、`shutdown`/FIN/`TIME_WAIT`）；`SOL_IP`（`IP_RETOPTS`/`IP_RECVOPTS`/`IP_RECVTTL`/`IP_RECVTOS`/`IP_PKTINFO`/`IP_OPTIONS`/`IP_MTU_DISCOVER`/`IP_ORIGDSTADDR` 等）、`SOL_IPV6`（`IPV6_RECVHOPLIMIT`/`IPV6_2292HOPLIMIT`/`IPV6_RECVPKTINFO`/`IPV6_2292PKTINFO` 等）与 `SOL_SOCKET`（`SO_TIMESTAMP*`/`SO_BINDTODEVICE`/`SO_RCVTIMEO*`/`SO_SNDTIMEO*` 等）选项及 `recvmsg` `cmsghdr` 辅助数据投递（`put_cmsg` 对齐与 `MSG_CTRUNC`）；全套 BSD socket 系统调用及 `poll`/`epoll` 唤醒 |
| 阶段 4 & 5：模块 ABI v2 与 `e1000`/`e1000e` | `src/module/{hw,api,elf,mod}.rs`、`src/net/module_api.rs`、`modules/include/spk/{e1000_regs,e1000_core}.h`、`modules/e1000/e1000.c`、`modules/e1000e/e1000e.c` | 模块 ABI v2（PCI driver/config/BAR/MMIO、DMA、x86_64 MSI-X/MSI/INTx、自旋锁/timer/work/kthread、`spk_netdev_*`/`spk_napi_*`/`spk_skb_*`）；`e1000.ko`（82540EM `8086:100e`，8254x EERD，MSI→INTx）与 `e1000e.ko`（82574L `8086:10d3`，82574 EERD，MSI-X→MSI→INTx，PCIe Master Disable 握手）；卸载再加载与 DMA 零泄漏校验 |
| 模块托管职责分离 | `build.rs`、`src/module/self_test.rs`、`user/systemg/systemg.c`、`tools/build_desktop_rootfs.py` | 常规/桌面镜像不内嵌任何测试 `.ko`（0 字节占位），且只要存在 `init=` 参数内核绝不自行加载模块；所有 `.ko`（`/lib/modules/*.ko`）统一由 PID 1 `systemg` 读取 `/etc/modules` 经 `finit_module(2)` 加载 |
| 阶段 6：IPv6 与扩展 Netlink | `src/net/ipv6/{mod,header,addr,icmpv6,ndisc,input,output,raw}.rs`、`src/net/netlink/{genl,netfilter,sock_diag}.rs` | IPv6 收发、EUI-64 `fe80::/64` 链路本地地址自动配置、ICMPv6 Echo、NDISC（NS/NA 与被请求节点组播）；`NETLINK_GENERIC`（`nlctrl` 与 `CTRL_CMD_GETFAMILY` dump）；`NETLINK_NETFILTER`（batch begin/end 与子系统分发）；`NETLINK_SOCK_DIAG`（`SOCK_DIAG_BY_FAMILY` IPv4/IPv6 TCP/UDP 枚举） |
| 用户态网络管理服务与桌面 UI | `user/desktop/services/NetworkManager.service`、`user/desktop/src/uxconf/net.rs`、`user/desktop/src/bin/{network_manager,nmcli}.rs`、`user/desktop/src/{netmgr,shell,apps/settings}.rs` | `systemg` 托管 `/usr/bin/NetworkManager` 守护进程：`NMDeviceState`（10..120）状态机、1500ms 载波防抖、非阻塞 `poll(2)` 驱动的 `NETLINK_ROUTE` 监听与 RFC 2131 DHCPv4（`Selecting`/`Requesting`/`Bound`/`Renewing` T1/`Rebinding` T2 + SLIRP 兜底）、`/etc/NetworkManager/system-connections/*.nmconnection` keyfile 与 `uxconfd` 双向持久化、`/run/NetworkManager/nm.sock` IPC、原子写 `/etc/resolv.conf`；配套 `nmcli` CLI、SPUI 任务栏状态胶囊与系统设置“网络”页面 |

### 与原设计的约定及边界说明

1. **`lo` 启动即 UP**：`net::init()` 初始化阶段直接拉起 `lo` 并配置 `127.0.0.1/8` 与 `::1/128`，保证早期自测与无网络管理器的精简环境可用。
2. **模块加载权归属 `systemg`**：内核自身不在用户态启动路径中自动加载任何 `.ko`；`src/module/self_test.rs` 仅在无 `init=` 的纯内核自测（`CONFIG_BOOT_SELF_TEST=y`）中运行驱动加载/ping/卸载回归。
3. **权限检查**：当前以 `euid == 0` 代替 `CAP_NET_ADMIN` / `CAP_NET_RAW`；非特权桌面会话（`spui` / `nmcli`）通过 `/run/NetworkManager/nm.sock` 请求 root 身份运行的 `NetworkManager.service` 执行底层 `NETLINK_ROUTE` 与 DHCPv4 操作。
4. **跨架构中断边界（已知待扩展项）**：协议栈、Netlink、Socket 与模块 API 在 `x86_64`、`aarch64`、`riscv64` 三架构共享并通过 `--all-features` 编译检查；但 `src/module/hw.rs` 的 PCI MSI/MSI-X/INTx 硬件中断接线目前仅在 `x86_64`（APIC/IOAPIC）实现，`aarch64`/`riscv64` 当前返回 `-ENODEV`，后续需补充 GIC ITS / APLIC-IMSIC 或 `virtio-net` 后端以在非 x86 平台做实卡收发验收。

### 标准验证命令

1. **三架构编译检查与可加载模块构建**：
   ```bash
   make modules ARCH=x86_64
   cargo check --target x86_64-unknown-none --features boot-self-test,modules
   cargo check --target aarch64-unknown-none-softfloat --all-features
   cargo check --target riscv64imac-unknown-none-elf --all-features
   ```
2. **内核协议栈 + `e1000e` / `e1000` QEMU 端到端自测（含收发、卸载重装与 DMA 泄漏检查）**：
   ```bash
   make test-single ARCH=x86_64 CONFIG_BOOT_SELF_TEST=y
   make test-single ARCH=x86_64 CONFIG_BOOT_SELF_TEST=y QEMU_NIC='user,model=e1000'
   ```
3. **桌面镜像与 `systemg` + `NetworkManager.service` 端到端验证**：
   ```bash
   make desktop-spui
   make desktop-iso
   tools/build_userland.sh && python3 tools/build_desktop_rootfs.py
   python3 tools/desktop_vm.py start
   python3 tools/desktop_vm.py run "cat /proc/modules && systemctl status NetworkManager && nmcli device show eth0 && ping -c 2 10.0.2.2"
   python3 tools/desktop_vm.py stop
   ```
