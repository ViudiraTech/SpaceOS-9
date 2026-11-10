# ALSA 兼容音频子系统设计与验收契约

2026/11/10 By JiTianYu391

目标：以 Linux ALSA 内核 ABI 为兼容面，自研 PCM、Control 与 Timer 核心，使未修改的 alsa-lib/tinyalsa 程序可以直接访问 `/dev/snd`；同时向可加载驱动模块导出 Linux 形态的 `snd_*` C ABI。三架构共享全部核心代码，架构相关部分只存在于已有的 DMA/IRQ/PCI 后端。

## 选型（已确认）

| 项 | 决定 |
| --- | --- |
| 已实现接口 | PCM + Control + Timer + RawMIDI + Hwdep + Jack；Seq / UMP 预留接入点 |
| 用户态 ABI | 二进制兼容 Linux：`/dev/snd`、主设备号 116、原始 ioctl 命令字、结构体逐字节一致、版本号一致 |
| 驱动 API | 对外 Linux 风格 `snd_*` C ABI；内部 Rust trait `PcmOps` / `ControlOps` |
| 数据通路 | mmap 环形缓冲 + status/control 页 + interleaved/non-interleaved RW 与 mmap + 周期唤醒 + xrun/drain/pause |
| 验证后端 | 内置 dummy 全双工声卡；启动自测覆盖 ABI、约束求解、状态机、定时器 |

## 参照的 Linux 源码

- `include/uapi/sound/asound.h`：ioctl 编码、`snd_pcm_*`、`snd_ctl_*`、`snd_timer_*` 结构与版本号。
- `include/sound/minors.h`：主设备号 116、`minor = card << 5 | dev`、timer 固定 minor 33、sequencer 固定 minor 1。
- `sound/core/pcm_native.c`、`sound/core/pcm_lib.c`：`HW_REFINE` / `HW_PARAMS` 约束求解、状态机、`hw_ptr/appl_ptr`、xrun。
- `sound/core/control.c`：元素列表、`ELEM_*`、TLV、订阅事件。
- `sound/core/timer.c`：`NEXT_DEVICE`、`SELECT`、`PARAMS`、START/STOP/CONTINUE/PAUSE、tread 记录。
- `sound/core/sound.c`：动态 minor 分配与 `controlC*` 先于 PCM 注册的顺序。

## 分层与源码

| 文件 | 内容 |
| --- | --- |
| `src/sound/uapi.rs` | UAPI 镜像：`_IOC` 编码、全部结构体、版本常量；编译期 `size_of` / `offset_of` / ioctl 值断言 |
| `src/sound/card.rs` | 卡编号、minor 分配、`controlC*` / `pcmC*D*p/c` 节点、`/sys/class/sound`、卡生命周期 |
| `src/sound/pcm.rs` | 约束求解、环形缓冲、status/control 页、状态机、RW、mmap、xrun、poll、drain |
| `src/sound/control.rs` | 元素表、`ELEM_LIST/INFO/READ/WRITE/ADD/REPLACE/REMOVE`、LOCK、TLV、通知队列 |
| `src/sound/timer.rs` | 全局 `/dev/snd/timer`、系统/每卡/用户驱动定时器、tick 队列、tread |
| `src/sound/rawmidi.rs` | RawMIDI 字节队列、参数/状态/poll、`'W'` ioctl |
| `src/sound/hwdep.rs` | Hwdep 标准 ioctl、DSP 加载与驱动私有 ioctl |
| `src/sound/module_api.rs` | 对 `.ko` 导出的 `snd_*` C ABI 与 C ops 适配 |
| `src/sound/dummy.rs` | 内置全双工 dummy 卡，也是驱动参考实现 |
| `src/sound/procfs.rs` | `/proc/asound/{cards,pcm,devices,timers,version,cardN/...}` |
| `src/sound/self_test.rs` | 启动自测：早期 ABI 测试 + 晚期流式测试 |

## 用户态 ABI

### 设备节点

- 主设备号 **116**；minor 从 0 起首次适配分配，保留 1（sequencer）与 33（timer）。
- 卡注册顺序与 Linux 一致：先 `controlC<n>`，随后每个方向的 `pcmC<n>D<m>p` / `pcmC<n>D<m>c`。
- 节点通过 `device::register` 发布，自动获得 `/dev/snd/...`、`/sys/class/sound/<name>`、`uevent` 与 `dev` 属性。
- `/proc/asound` 由 `procfs` 的 `asound` 入口挂接，内容实时读取卡注册表。

### ioctl 与结构

- PCM `'A'`：`PVERSION/INFO/TSTAMP/TTSTAMP/USER_PVERSION/HW_REFINE/HW_PARAMS/HW_FREE/SW_PARAMS/STATUS/STATUS_EXT/DELAY/HWSYNC/SYNC_PTR/CHANNEL_INFO/PREPARE/RESET/START/DROP/DRAIN/PAUSE/REWIND/RESUME/XRUN/FORWARD/WRITEI_FRAMES/READI_FRAMES/WRITEN_FRAMES/READN_FRAMES/UNLINK/LINK`。
- Control `'U'`：`CARD_INFO/CARD_BYTES/ELEM_LIST/ELEM_INFO/ELEM_READ/ELEM_WRITE/ELEM_ADD/ELEM_REPLACE/ELEM_REMOVE/ELEM_LOCK/ELEM_UNLOCK/SUBSCRIBE_EVENTS/TLV_READ/TLV_WRITE/TLV_COMMAND/PCM_NEXT_DEVICE/PCM_INFO/PCM_PREFER_SUBDEVICE/POWER/POWER_STATE`。
- Timer `'T'`：`PVERSION/NEXT_DEVICE/TREAD/GINFO/GPARAMS/GSTATUS/SELECT/INFO/PARAMS/STATUS/START/STOP/CONTINUE/PAUSE`。
- 版本：PCM `2.0.18`、CTL `2.0.10`、TIMER `2.0.8`。
- 关键结构尺寸在 `uapi.rs` 中编译期断言：`snd_pcm_hw_params=608`、`snd_pcm_sw_params=136`、`snd_pcm_status=152`、`snd_pcm_sync_ptr=136`、`snd_ctl_elem_info=272`、`snd_ctl_elem_value=1224`、`snd_timer_ginfo=248` 等。ioctl 命令字也与 C 头文件生成值逐项比对。

### mmap 页

- 数据区：offset 0，长度不超过环形缓冲按页向上取整的大小。
- status 页：`0x80000000`（兼容）/ `0x82000000`（time64）；control 页：`0x81000000` / `0x83000000`。
- 环形缓冲、status、control 属于同一个 `BufferSet`。映射时把 `BufferSet` 的 `Arc` 交给 `MmapPin`，用户映射持有引用；`HW_FREE` 或重新 `HW_PARAMS` 只丢弃 runtime 引用，页表仍指向有效物理页，直到最后一个映射被 `munmap`。

## PCM 运行时

### 子设备与打开语义

每个方向可有多个 substream（`snd_pcm_new` 的 playback/capture count）。方向节点
`pcmC<n>D<m>{p,c}` 上的 open 选择第一个空闲 substream；全部占用时返回 `EAGAIN`，
与 Linux `snd_pcm_attach_substream` 一致。`snd_pcm_info` 报告
`subdevices_count/subdevices_avail` 与所选 `subdevice`。

### 链接组

`LINK` 通过 fd 找到目标 substream，双方进入同一组；`START/STOP/PAUSE/RESUME/DRAIN`
与 `prepare/reset` 对组内成员一起执行，`UNLINK` 解散组。自链接与重复链接返回
`EINVAL/EBUSY`。

### 约束求解

- `HW_REFINE` 以驱动 caps 为基础填满 mask/interval，再与调用者已固定的值求交并设置 `cmask`。
- `HW_PARAMS` 要求 access/format/subformat 唯一，求解 channels、rate、period_size、periods、buffer_size，派生 period_time、buffer_time、period_bytes、buffer_bytes；校验 caps 与 `buffer_bytes_max`。
- 默认 caps：S16_LE/S32_LE/U8、8 kHz–192 kHz、1–2 声道、2–32 周期、1 MiB 缓冲；驱动可在 `open` 中通过 `snd_pcm_hw_constraint_*` 收窄。
- 仅通告 `MMAP_INTERLEAVED` 与 `RW_INTERLEAVED`；非交错格式返回 `EINVAL`，不向用户态虚报能力。

### 状态机

`OPEN → SETUP(hw_params) → PREPARED(prepare) → RUNNING(start) → PAUSED(pause) / XRUN / DRAINING(drain) → SETUP(stop)`。
`STATUS`、`HWSYNC`、`DELAY` 与阻塞读写都会先读取驱动指针；播放侧 hw_ptr 越过 appl_ptr 或捕获侧超出缓冲即进入 `XRUN`，并停止驱动。

### 非交错与时间戳

- 访问方式通告 `MMAP_INTERLEAVED/RW_INTERLEAVED/MMAP_NONINTERLEAVED/RW_NONINTERLEAVED`；
  非交错时缓冲按声道平面布局，`CHANNEL_INFO.offset` 给出每声道平面偏移，
  `READN/WRITEN_FRAMES` 在平面与用户声道指针之间搬运。
- 记录 `tstamp_type`；`STATUS_EXT` 的 `audio_tstamp_data` 解析请求类型并回填
  valid/actual_type 报告位；`audio_tstamp` 默认按 `trigger_tstamp + hw_ptr/rate`
  计算 DMA 时间。`EXPORT_BUFFER` 作为用户态插件提示原样携带。

### 环形缓冲与唤醒

- `hw_ptr/appl_ptr` 在 `boundary`（默认 `buffer_size * 2`）内回绕，环形偏移取 `ptr % buffer_size`。
- 状态、可用帧数和 generation 以原子变量发布；等待谓词只读原子，不获取 runtime 互斥锁。
- `period_elapsed` 由驱动周期中断调用；dummy 用内核软件定时器按 period 驱动。
- `poll`：播放可写返回 `POLLOUT`，捕获可读返回 `POLLIN`，`XRUN` 返回 `POLLERR`；`poll_sources` 绑定 substream 对象并保留 1 ms 兜底 tick。

## Control、RawMIDI、Hwdep 与 Jack

- 元素支持 `BOOLEAN/INTEGER/INTEGER64/ENUMERATED/BYTES/IEC958`，驱动元素通过 `ControlOps` 回调，用户元素带 `SNDRV_CTL_ELEM_ACCESS_USER`。
- 事件队列按卡维护序号，每个打开文件有独立游标；`read()` 返回 `snd_ctl_event`，`poll`/阻塞读由 `WaitQueue` 与对象唤醒支撑。
- TLV 支持读写和标准 `DB_MINMAX` 音量树；`ELEM_LOCK` 以进程 pid 记录所有者。
- RawMIDI：`/dev/snd/midiC<n>D<m>`，按 open 方向选择输入/输出流；参数、状态、DROP/DRAIN、阻塞/poll 与 `snd_rawmidi_*` 驱动接口齐备，dummy 卡提供回环。
- Hwdep：`/dev/snd/hwC<n>D<m>`，标准 PVERSION/INFO/DSP_STATUS/DSP_LOAD 与驱动私有 ioctl；dummy 卡提供可加载 DSP 镜像测试。
- Jack：以只读易变控制元素 `"<id> Jack"` 暴露插孔状态，`snd_jack_report` 更新并发送控制事件。
- Timer 同时提供全局系统定时器、每卡应用定时器与用户驱动定时器（`CREATE/TRIGGER`，匿名 fd）；分辨率默认 1 ms，可通过 `GPARAMS` 调整；tick 在软件定时器线程生成，`read()` 返回 `snd_timer_read` 或启用 tread 后返回 `snd_timer_tread`。

## 可加载驱动 API

头文件：`modules/include/spk/sound.h`。典型流程：

```c
static int my_open(struct snd_pcm_substream *sub)
{
    snd_pcm_hw_constraint_minmax(sub, SNDRV_PCM_HW_PARAM_RATE, 8000, 48000);
    snd_pcm_set_formats(sub, 1ULL << SNDRV_PCM_FORMAT_S16_LE);
    return 0;
}

static int my_hw_params(struct snd_pcm_substream *sub,
                        struct snd_pcm_hw_params *params)
{
    snd_pcm_lib_malloc_pages(sub, snd_pcm_get_buffer_size(sub) * 4);
    /* program rate, format and the DMA address from snd_pcm_get_dma_addr() */
    return 0;
}

static int my_trigger(struct snd_pcm_substream *sub, int cmd)
{
    if (cmd == SNDRV_PCM_TRIGGER_START) start_hardware(sub);
    else if (cmd == SNDRV_PCM_TRIGGER_STOP) stop_hardware(sub);
    return 0;
}

static snd_pcm_uframes_t my_pointer(struct snd_pcm_substream *sub)
{
    return read_position_register(sub) / frame_bytes;
}

static const struct snd_pcm_ops my_ops = {
    .open = my_open, .hw_params = my_hw_params,
    .trigger = my_trigger, .pointer = my_pointer,
};

/* init(): snd_card_new(-1, "MyCard", ...); snd_pcm_new(...);
 * snd_pcm_set_ops(pcm, SNDRV_PCM_STREAM_PLAYBACK, &my_ops);
 * snd_card_register(card); */
```

周期中断中调用 `snd_pcm_period_elapsed(substream)`，出错时调用 `snd_pcm_stop_xrun(substream)`。导出符号通过 `module::api::lookup` 的白名单解析，与 PCI/DMA/IRQ/网络 API 使用同一模块 ABI。

## 构建与配置

```
CONFIG_SOUND=y          # 编译 sound 核心
CONFIG_SOUND_DUMMY=y    # 内置 dummy 卡与启动自测后端
```

Makefile 将其映射为 Cargo feature `sound` / `sound-dummy`。禁用 `CONFIG_SOUND` 时整个子系统不参与编译，不导出 `snd_*` 符号。

## 验证

- 编译期：三架构 `cargo check`；UAPI 尺寸、字段偏移、ioctl 命令字静态断言。
- 启动早期（调度器之前，`kernel_test_scope`）：真实 ioctl 入口 + 内核缓冲，覆盖 `CARD_INFO`、`ELEM_LIST/INFO/READ/WRITE`、`PCM_INFO`、`PCM_NEXT_DEVICE`、`HW_REFINE`、`HW_PARAMS` 求解、`SW_PARAMS`、`PREPARE`、`STATUS`、`CHANNEL_INFO`、timer `NEXT_DEVICE/GINFO/PARAMS/INFO/STATUS`。
- 启动晚期（内核线程）：dummy 播放/捕获启动、周期指针推进、pause/resume、timer 连续 tick。
- 自测成功后 `SELFTEST_OK` 行包含 `sound=ok`；任何断言失败都会在串口打印并中止自测。

## 当前边界与后续阶段

1. Sequencer 与 UMP 尚未实现；Control 对应枚举 ioctl 返回 `ENODEV`。这两者需要独立的 client/port/event 模型，留作下一阶段。
2. 未提供 OSS `/dev/dsp`、`/dev/mixer` 模拟与 32 位 compat ioctl；当前用户态仅 64 位。
3. `MMAP_COMPLEX`、`AUDIO_TSTAMP` 的 LINK_* 类型目前回退为 DEFAULT 报告；驱动可通过后续 `get_time_info` 扩展精确链路时间。
4. 多 substream 已支持；同一 substream 的重复 open 仍返回 `EAGAIN`（Linux 也只在有空闲 substream 时成功）。
5. 后续驱动阶段：QEMU HDA（intel-hda）、AC'97、virtio-sound、USB Audio；每个驱动只需实现 `snd_pcm_ops`/`snd_rawmidi_ops`/`snd_hwdep_ops`，核心与自测无需改动。
