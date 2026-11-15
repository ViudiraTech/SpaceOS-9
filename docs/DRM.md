# DRM/KMS 子系统

日期：2026-11-15。实现：`src/drm/`（核心）、`src/drm/virtio/`（virtio-gpu 驱动）、`src/hal/display.rs` + `modules/simplefb/`（simpledrm 模块）。

## 分层

参照 Linux `drivers/gpu/drm`：**核心拥有全部用户态 ABI**，驱动只描述硬件并按核心校验过的状态编程硬件。

```
用户态 (libdrm / Mesa / Xorg modesetting / Wayland)
        │  /dev/dri/cardN   /dev/dri/renderD(128+N)
┌───────┴────────────────────────────────────────────────────────┐
│ DRM 核心 src/drm                                                 │
│  file.rs     节点、每文件状态、master/lastclose、ioctl 分发、PRIME │
│  kms.rs      对象/属性模型、atomic check/commit、传统接口转 atomic │
│  gem.rs      GEM 对象（可插拔后端）、句柄表、mmap 偏移           │
│  syncobj.rs  fence、sync_file、binary/timeline syncobj           │
│  vblank.rs   定时器模拟 vblank、翻页/VBLANK/SEQUENCE 事件         │
│  ioctl.rs    ioctl 编号/尺寸表与参数拷贝                          │
└───────┬───────────────────────────────┬────────────────────────┘
        │ Driver / DriverFile trait     │ HAL 显示类 (C ABI)
┌───────┴─────────────┐       ┌─────────┴──────────────┐
│ virtio-gpu (内建)    │       │ simplefb.ko (模块)      │
│ src/drm/virtio       │       │ modules/simplefb        │
└─────────────────────┘       └────────────────────────┘
```

### 核心职责

- **节点**：`cardN`（首个打开者成为 master）与可选的 `renderDN`（render 节点永不成为 master，禁止 modeset、dumb、master/auth ioctl）。sysfs 发布连接器 `status/enabled/modes/dpms/edid`，以及可选的 `device` 父设备链接和驱动给出的属性（如 vendor/device）。
- **KMS**：每个管线是 connector → encoder → CRTC，带主平面和可选光标平面；属性名、标志、对象类型与 Linux 一致（用户态按名字查找）。所有传统入口（SETCRTC、SETPLANE、PAGE_FLIP、DIRTYFB、DPMS、CURSOR）都转换成一次 atomic 提交，驱动只实现一条更新路径。支持 `TEST_ONLY`、`NONBLOCK`、`ALLOW_MODESET`、`PAGE_FLIP_EVENT`、`FB_DAMAGE_CLIPS`、`IN_FORMATS`、`IN_FENCE_FD`、`OUT_FENCE_PTR`。
- **GEM**：对象持有 `GemBacking`；核心自带 shmem 后端（逐页分配 + 连续内核映射）。句柄按文件；对象在最后一个句柄、帧缓冲、dma-buf 和用户映射消失后释放。mmap 偏移只对持有该对象句柄的文件有效。
- **PRIME**：导出为 dma-buf fd（可 mmap、poll），只允许导入同一设备的对象。
- **同步**：`Fence` 由手动完成或驱动的 `FenceSource` 驱动；sync_file、binary/timeline syncobj、`SYNCOBJ_WAIT/TIMELINE_WAIT`（ALL/FOR_SUBMIT/AVAILABLE）。等待者睡在统一的 fence 事件等待队列上。
- **vblank**：无硬件 vblank 中断的设备按模式刷新率模拟（Linux `drm_crtc_vblank_start_timer`），驱动翻页事件、`WAIT_VBLANK`、`CRTC_QUEUE_SEQUENCE`。时间戳来自 `CLOCK_MONOTONIC`，不是硬件扫描时间。
- **事件**：变长 `struct drm_event`，每文件最多 256 个；读支持阻塞/非阻塞，poll 与等待队列唤醒。
- **控制台交接**：`takes_over_console` 的设备在有活动 CRTC 时让文本控制台停止绘制，lastclose 时归还并重绘。
- **固件帧缓冲**：注册了 `claims_firmware_fb` 的原生驱动之后，`spk_sysfb_get` 返回 `-EBUSY`，simpledrm 不再绑定（对应 Linux aperture 移除 sysfb 设备）。

### 锁

提交在设备的可睡眠 modeset 锁下进行；`kms::State` 在自旋锁后面，只在调用驱动前后克隆/写回，**从不跨驱动调用持有**，且不可重入（`object_props` 等辅助函数接收调用者已持有的 `&State`）。驱动回调在进程上下文，可以睡眠。

## 驱动接口

```rust
pub trait Driver: Send + Sync {
    fn check(&self, pipe, mode, update) -> Result<()>;        // 额外的硬件约束
    fn set_mode(&self, pipe, Option<&Mode>) -> Result<()>;    // 开/关管线
    fn update_plane(&self, pipe, &PlaneUpdate) -> Result<()>; // 呈现（含 damage 矩形）
    fn update_cursor(&self, pipe, &CursorUpdate) -> Result<()>;
    fn lastclose(&self);  fn release(&self);
    fn create_dumb(&self, w, h, bpp) -> Result<(Arc<dyn GemBacking>, pitch)>; // 默认 shmem
    fn check_framebuffer(&self, &Framebuffer) -> Result<()>;  // ADDFB 时
    fn ioctl_descriptor(&self, nr) -> Option<(size, dir)>;    // 驱动私有 ioctl 0x40..0xa0
    fn open(&self, &Arc<File>) -> Result<Option<Arc<dyn DriverFile>>>; // 每文件驱动状态
    fn kick(&self);  // 等待 fence 时推进设备
}
pub trait DriverFile { fn ioctl(&self, &File, &mut Ioctl); fn poll(&self, &File); fn needs_tick(&self); }
```

`DeviceConfig` 描述名字/版本、管线（连接器类型、是否连接、模式、EDID、格式、光标尺寸）、尺寸范围以及 `prefer_shadow`、`takes_over_console`、`claims_firmware_fb`、`render`、`prime`、`syncobj`、`parent`、`attributes`。

## simpledrm（`modules/simplefb`）

通过 HAL 显示类（`spk_sysfb_get`、`spk_display_register`，HAL v2）把固件帧缓冲（UEFI GOP）暴露为完整 DRM 设备，没有 GPU 驱动也能亮屏。

- 影子缓冲：客户端在系统内存绘制，扫描输出内存（写合并/非缓存）从不回读；只拷贝 damage 矩形。
- 格式：原生格式 + 模拟 XRGB8888/ARGB8888（原生为 XRGB8888 时另加 RGB565）；目标格式 XRGB8888、XBGR8888、RGB565、RGB888、BGR888、XRGB2101010，以及按掩码通用打包。
- 转换逐行进行，每次处理 2/4 个像素并组装 64 位写入；模块以 `-fno-builtin` 编译，小拷贝用 `__builtin_memcpy` 内联，只有整行同格式拷贝调用内核 `memcpy`。

## virtio-gpu（`src/drm/virtio`）

| 文件 | 内容 |
| --- | --- |
| `transport.rs` | 现代 VirtIO PCI、split virtqueue、DMA 保留、IRQ 完成、超时复位 |
| `mod.rs` | 资源（2D、3D、blob：guest 页或 host-visible BAR）、3D 上下文、VIRTGPU ioctl（MAP、EXECBUFFER、GETPARAM、RESOURCE_CREATE[_BLOB]、RESOURCE_INFO、TRANSFER_{TO,FROM}_HOST、WAIT、GET_CAPS、CONTEXT_INIT）、ring fence 事件、资源回收、无固件帧缓冲时的文本控制台扫描输出 |
| `display.rs` | 输出发现（DISPLAY_INFO、EDID）、扫描输出编程（SET_SCANOUT[_BLOB]）、damage 传输与 RESOURCE_FLUSH、硬件光标（UPDATE/MOVE_CURSOR） |

驱动以核心提供的 card0/renderD128、KMS、GEM、PRIME、syncobj 为基础，只实现 virtio 特有的部分。资源对象实现 `GemBacking`（DMA 页或 BAR 映射），提交命令返回由 `FenceSource` 驱动的 fence；用户态呈现前等待资源上最后一次 GPU 命令完成（隐式同步）。

## ABI 核对

`make test-drm-abi` 用宿主 Linux UAPI 头文件生成编号/尺寸/方向（`tools/drm_abi_probe.c`），由 `tools/check_drm_abi.py` 与 `src/drm/ioctl.rs` 和 `src/drm/virtio/mod.rs` 的表逐条比对：方向必须相同，内核结构尺寸不得小于头文件（较短的用户结构按 Linux `drm_ioctl` 规则补零，多出的尾部必须为 0）。当前 56 个核心 + 11 个 virtio-gpu ioctl 通过；SYNCOBJ_HANDLE_TO_FD/FD_TO_HANDLE 与 VIRTGPU_RESOURCE_CREATE_BLOB 的内核结构比宿主头文件长。

## 验证状态（2026-11-15）

| 范围 | 状态 |
| --- | --- |
| 三目标编译 | `cargo check --all-features` x86_64/aarch64/riscv64 无错误无告警；`cargo fmt --check` 通过 |
| ABI 表 | `make test-drm-abi` 通过 |
| simpledrm | x86_64 `selftest=drm` + `qemu_test.py --simplefb` 通过（提交 9b99014）：模式设置、DIRTYFB 只呈现剪裁区、RGB565→XRGB8888、PAGE_FLIP + 事件、atomic TEST_ONLY 与 FB_DAMAGE_CLIPS、WAIT_VBLANK、lastclose 归还控制台、模块卸载，均有扫描输出像素回读。转换函数另在宿主机对全部目标格式与参考实现比对。**此后核心为 virtio 迁移扩展了 PRIME/syncobj/render/显式 fence，未复跑。** aarch64/riscv64（ramfb）未跑 |
| virtio-gpu | **迁移到核心后尚未在 QEMU 中运行。** 2026-10-25 的 2D/KMS、VirGL、Venus 内核 ABI 通过记录属于已删除的 `src/gpu` 实现，只能作为功能基线，不能作为现实现的证据。自测（`src/drm/virtio/self_test.rs`）已改为按名字查找对象和属性，能编译，未执行 |
| 真机、Mesa/Vulkan 应用 | 未验收 |

## 已知限制

- 无 gamma LUT（GETGAMMA/SETGAMMA 返回 EINVAL）、无 overlay 平面、无 writeback、无异步翻页。
- vblank 为模拟；`OUT_FENCE_PTR` 在硬件编程完成时发信号，不等到扫描输出。
- virtio-gpu：blob 扫描输出的 damage 更新不发 RESOURCE_FLUSH（沿用原实现）；非阻塞提交在 ioctl 上下文同步编程硬件，只把完成事件推迟到 vblank。
- GETFB 只向帧缓冲所有者或 master 返回句柄。
