# 驱动硬件抽象层（HAL）

日期：2026-11-15。实现：`src/hal/`、`src/module/hw.rs`、`src/module/api.rs`；头文件：`modules/include/spk/module.h`。

## 目标

可加载驱动模块只通过 HAL 访问内核，不依赖任何内核内部类型。HAL 全部是 C ABI 函数，C 与 Rust 模块都能调用；导出表（`module::api::EXPORTS`）之外的符号无法被模块重定位解析。

## 分层

| 层 | 内容 | 实现 |
| --- | --- | --- |
| 总线与 I/O | PCI 驱动注册/匹配（含 `SPK_PCI_ANY_ID` 与类码匹配）、配置空间、BAR、MSI/MSI-X/INTx、一致性与流式 DMA、MMIO、端口 I/O | `module/hw.rs` |
| 执行上下文 | 堆、时间、自旋锁、定时器、工作队列、内核线程；可睡眠的完成量与互斥锁 | `module/api.rs`、`hal/sync.rs` |
| 设备类 | 网络（netdev/skb/NAPI）、声音（ALSA `snd_*`）、输入（evdev）、块设备 | `net/module_api.rs`、`sound/`、`hal/input.rs`、`hal/block.rs` |

## 版本

`SPK_HAL_VERSION` / `spk_hal_version()` 只增不减；新增调用不改变既有调用，因此模块用 `spk_hal_version() >= N` 判断即可。当前为 1。

## 上下文约定

- `spk_complete`、`spk_input_event`、`spk_input_sync`、自旋锁可在中断上下文调用。
- 等待完成量、互斥锁、块设备回调、注册/注销只在进程上下文。
- 完成量为计数语义：每次 `spk_complete` 满足一次等待。

## 设备类

**输入**：`spk_input_alloc` → `spk_input_set_capability`/`spk_input_set_abs` → `spk_input_register`；事件码与 Linux 一致。`spk_input_unregister` 撤销所有打开的 evdev 客户端（阻塞读返回 ENODEV）并删除 `/dev/input/eventN` 与 sysfs 节点。

**块设备**：`spk_blkdev_register(prefix, ...)` 由内核分配名字（如 `sdb`）、扫描 MBR/GPT 分区并立即发布 `/dev` 与 sysfs 节点。设备 id 为稳定槽位，注销后不复用。`spk_blkdev_unregister` 先拒绝新 I/O，再等待进行中的回调返回，之后驱动才可释放私有数据。当前块层缓存与分区扫描按 512 字节扇区工作，v1 只接受 512 字节逻辑块。

## 示例：`modules/usb`

`usb.ko` 只使用 HAL：xHCI 主控（命令/事件环由中断驱动，端口复位等待端口状态变化事件）、设备枚举、HID boot 键盘/鼠标（输入类）、Bulk-Only 大容量存储（块设备类，SCSI READ/WRITE(10)）。热拔插经工作队列在进程上下文处理。

验证：`python3 tools/qemu_test.py x86_64 ... --usb`（`make test-single` 在启用 `MODULE_USB` 与自测时自动加上）挂载 qemu-xhci、usb-kbd、usb-mouse 与带 FAT32 分区的 usb-storage；自测加载模块、挂载分区读取文件、卸载模块并确认设备移除，日志标记 `USB_HID_OK`、`USB_MSC_OK`、`USB_MSC_FS_OK`、`USB_UNLOAD_OK`。

已知限制：每个控制器单 LUN；未实现 USB 集线器、等时传输、UAS、SuperSpeed 端点的 burst 参数；MSI 不可用时回退 INTx。
