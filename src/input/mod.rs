/*
 *
 *       src/input/mod.rs
 *       Linux input core and evdev character devices
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Implements the evdev interface from `include/uapi/linux/input.h` and
//! `Documentation/input/`. Drivers describe a device's capabilities and report
//! events followed by `SYN_REPORT`; every open file has its own bounded queue,
//! and an overrun is reported with `SYN_DROPPED` so clients can resynchronize
//! from the `EVIOCG*` state ioctls.
//!
//! Sysfs layout follows Linux because libudev derives `ID_INPUT_*` from it:
//! `/sys/devices/inputN/{name,phys,uniq,id/*,capabilities/*,properties}` with a
//! child `eventN` that owns the character device.

pub mod ps2;
#[cfg(target_arch = "x86_64")]
pub mod virtio;

use crate::{
    sync::SpinLock,
    user::uaccess,
    vfs::{
        Inode, InodeKind, Stat, VfsError,
        abi::makedev,
        tree::{Attribute, Directory},
    },
};
use alloc::{collections::VecDeque, format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const EV_MSC: u16 = 0x04;
pub const EV_SW: u16 = 0x05;
pub const EV_LED: u16 = 0x11;
pub const EV_SND: u16 = 0x12;
pub const EV_REP: u16 = 0x14;
pub const EV_FF: u16 = 0x15;
pub const EV_MAX: usize = 0x1f;
pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;
pub const KEY_MAX: usize = 0x2ff;
pub const REL_MAX: usize = 0x0f;
pub const ABS_MAX: usize = 0x3f;
pub const MSC_MAX: usize = 0x07;
pub const LED_MAX: usize = 0x0f;
pub const SW_MAX: usize = 0x10;
pub const SND_MAX: usize = 0x07;
pub const FF_MAX: usize = 0x7f;
pub const REP_MAX: usize = 0x01;
pub const PROP_MAX: usize = 0x1f;
pub const BUS_I8042: u16 = 0x11;
pub const BUS_VIRTUAL: u16 = 0x06;
const INPUT_MAJOR: u64 = 13;
const EVDEV_MINOR_BASE: u64 = 64;
const EVENT_SIZE: usize = 24;
const CLIENT_QUEUE: usize = 1024;
const EV_VERSION: i32 = 0x0001_0001;
const CLOCK_REALTIME: u32 = 0;
const CLOCK_MONOTONIC: u32 = 1;
const CLOCK_BOOTTIME: u32 = 7;

/// Byte-addressed bitmap sized for one event type's code range.
#[derive(Clone)]
pub struct Bitmap(Vec<u8>);

impl Bitmap {
    pub fn with_max(max_code: usize) -> Self {
        // Linux exposes bitmaps as arrays of native unsigned long words.
        Self(vec![0; (max_code / 64 + 1) * 8])
    }

    pub fn set(&mut self, code: usize) {
        if let Some(byte) = self.0.get_mut(code / 8) {
            *byte |= 1 << (code % 8);
        }
    }

    pub fn clear(&mut self, code: usize) {
        if let Some(byte) = self.0.get_mut(code / 8) {
            *byte &= !(1 << (code % 8));
        }
    }

    pub fn get(&self, code: usize) -> bool {
        self.0
            .get(code / 8)
            .is_some_and(|byte| byte & (1 << (code % 8)) != 0)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|&byte| byte == 0)
    }

    /// Sysfs capability text: hex words, most significant first, without
    /// leading empty words, exactly like `input_print_bitmap`.
    pub fn sysfs_text(&self) -> String {
        let mut words: Vec<u64> = self
            .0
            .chunks(8)
            .map(|chunk| {
                let mut raw = [0u8; 8];
                raw[..chunk.len()].copy_from_slice(chunk);
                u64::from_le_bytes(raw)
            })
            .collect();
        while words.len() > 1 && *words.last().unwrap() == 0 {
            words.pop();
        }
        let mut text = String::new();
        for (index, word) in words.iter().rev().enumerate() {
            if index != 0 {
                text.push(' ');
            }
            if index == 0 {
                text.push_str(&format!("{:x}", word));
            } else {
                text.push_str(&format!("{:016x}", word));
            }
        }
        text.push('\n');
        text
    }
}

#[derive(Clone, Copy, Default)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

/// Static description of a device, filled in by its driver before registration.
pub struct Capabilities {
    pub name: String,
    pub phys: String,
    pub uniq: String,
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub properties: Bitmap,
    pub ev: Bitmap,
    pub key: Bitmap,
    pub rel: Bitmap,
    pub abs: Bitmap,
    pub msc: Bitmap,
    pub led: Bitmap,
    pub sw: Bitmap,
    pub snd: Bitmap,
    pub ff: Bitmap,
    pub abs_info: Vec<AbsInfo>,
    pub repeat: [i32; 2],
}

impl Capabilities {
    pub fn new(name: &str, phys: &str, bustype: u16, vendor: u16, product: u16) -> Self {
        let mut ev = Bitmap::with_max(EV_MAX);
        ev.set(EV_SYN as usize);
        Self {
            name: String::from(name),
            phys: String::from(phys),
            uniq: String::new(),
            bustype,
            vendor,
            product,
            version: 1,
            properties: Bitmap::with_max(PROP_MAX),
            ev,
            key: Bitmap::with_max(KEY_MAX),
            rel: Bitmap::with_max(REL_MAX),
            abs: Bitmap::with_max(ABS_MAX),
            msc: Bitmap::with_max(MSC_MAX),
            led: Bitmap::with_max(LED_MAX),
            sw: Bitmap::with_max(SW_MAX),
            snd: Bitmap::with_max(SND_MAX),
            ff: Bitmap::with_max(FF_MAX),
            abs_info: vec![AbsInfo::default(); ABS_MAX + 1],
            repeat: [250, 33],
        }
    }

    fn bitmap_for(&self, kind: usize) -> Option<&Bitmap> {
        Some(match kind as u16 {
            EV_KEY => &self.key,
            EV_REL => &self.rel,
            EV_ABS => &self.abs,
            EV_MSC => &self.msc,
            EV_LED => &self.led,
            EV_SW => &self.sw,
            EV_SND => &self.snd,
            EV_FF => &self.ff,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
struct Event {
    /// Monotonic and wall-clock microseconds, so each client can pick a clock.
    monotonic: u64,
    realtime: u64,
    kind: u16,
    code: u16,
    value: i32,
}

struct ClientState {
    queue: VecDeque<Event>,
    clock: u32,
    /// True after an overrun until `SYN_DROPPED` has been queued.
    /// Per-type event masks from `EVIOCSMASK`; `None` passes everything.
    masks: Vec<Option<Bitmap>>,
    revoked: bool,
}

struct Live {
    key: Bitmap,
    led: Bitmap,
    sw: Bitmap,
    snd: Bitmap,
    abs: Vec<AbsInfo>,
    repeat: [i32; 2],
    grabbed_by: Option<u64>,
    pending_sync: bool,
    /// Timestamp shared by every event of the frame being built; cleared by
    /// `SYN_REPORT`. Clients (libinput) treat a time change inside one frame
    /// as a missing `SYN_REPORT`.
    frame_time: Option<(u64, u64)>,
}

pub struct InputDevice {
    index: u32,
    capabilities: Capabilities,
    live: SpinLock<Live>,
    clients: SpinLock<Vec<Arc<Client>>>,
    /// Driver hook for output events such as LED changes.
    output: SpinLock<Option<fn(u16, u16, i32)>>,
}

struct Client {
    id: u64,
    state: SpinLock<ClientState>,
}

static NEXT_DEVICE: AtomicU32 = AtomicU32::new(0);
static DEVICES: SpinLock<Vec<Arc<InputDevice>>> = SpinLock::new(Vec::new());

impl InputDevice {
    pub fn set_output_hook(&self, hook: fn(u16, u16, i32)) {
        *self.output.lock() = Some(hook);
    }

    /// Queue one event, updating device state. Callable from IRQ context.
    ///
    /// State-carrying events that do not change the value are dropped, which is
    /// the documented guarantee clients rely on ("events are only sent on change").
    pub fn report(&self, kind: u16, code: u16, value: i32) {
        let code_index = code as usize;
        {
            let mut live = self.live.lock();
            match kind {
                EV_KEY => {
                    if code_index > KEY_MAX || !self.capabilities.key.get(code_index) {
                        return;
                    }
                    let current = live.key.get(code_index);
                    if value != 2 && current == (value != 0) {
                        return;
                    }
                    if value == 0 {
                        live.key.clear(code_index);
                    } else {
                        live.key.set(code_index);
                    }
                }
                EV_ABS => {
                    if code_index > ABS_MAX || !self.capabilities.abs.get(code_index) {
                        return;
                    }
                    if live.abs[code_index].value == value {
                        return;
                    }
                    live.abs[code_index].value = value;
                }
                EV_REL => {
                    if code_index > REL_MAX || !self.capabilities.rel.get(code_index) || value == 0
                    {
                        return;
                    }
                }
                EV_LED => {
                    if code_index > LED_MAX || !self.capabilities.led.get(code_index) {
                        return;
                    }
                    if live.led.get(code_index) == (value != 0) {
                        return;
                    }
                    if value == 0 {
                        live.led.clear(code_index);
                    } else {
                        live.led.set(code_index);
                    }
                }
                EV_SW => {
                    if code_index > SW_MAX || !self.capabilities.sw.get(code_index) {
                        return;
                    }
                    if live.sw.get(code_index) == (value != 0) {
                        return;
                    }
                    if value == 0 {
                        live.sw.clear(code_index);
                    } else {
                        live.sw.set(code_index);
                    }
                }
                EV_MSC => {
                    if code_index > MSC_MAX || !self.capabilities.msc.get(code_index) {
                        return;
                    }
                }
                EV_SYN => {
                    if code == SYN_REPORT {
                        if !live.pending_sync {
                            return;
                        }
                        live.pending_sync = false;
                    }
                }
                _ => {}
            }
            if kind != EV_SYN {
                live.pending_sync = true;
            }
        }
        self.deliver(kind, code, value);
    }

    pub fn sync(&self) {
        self.report(EV_SYN, SYN_REPORT, 0);
    }

    fn deliver(&self, kind: u16, code: u16, value: i32) {
        let (monotonic, realtime, grabbed) = {
            let mut live = self.live.lock();
            let (monotonic, realtime) = live.frame_time.unwrap_or_else(|| {
                (crate::time::uptime_micros(), crate::time::realtime_micros())
            });
            live.frame_time = if kind == EV_SYN && code == SYN_REPORT {
                None
            } else {
                Some((monotonic, realtime))
            };
            (monotonic, realtime, live.grabbed_by)
        };
        let event = Event {
            monotonic,
            realtime,
            kind,
            code,
            value,
        };
        let clients = self.clients.lock();
        for client in clients.iter() {
            if grabbed.is_some_and(|owner| owner != client.id) {
                continue;
            }
            let mut state = client.state.lock();
            if state.revoked {
                continue;
            }
            if kind != EV_SYN
                && let Some(Some(mask)) = state.masks.get(kind as usize)
                && !mask.get(code as usize)
            {
                continue;
            }
            if state.queue.len() >= CLIENT_QUEUE {
                // Keep the newest frame's boundary: replace everything with a
                // single SYN_DROPPED that tells the client to re-read state.
                state.queue.clear();
                state.queue.push_back(Event {
                    kind: EV_SYN,
                    code: SYN_DROPPED,
                    value: 0,
                    ..event
                });
            }
            state.queue.push_back(event);
            drop(state);
            crate::ipc::wake_object(Arc::as_ptr(client));
        }
        drop(clients);
    }
}

/// Publish a driver's device: class nodes, sysfs tree and uevents.
pub fn register(capabilities: Capabilities) -> Result<Arc<InputDevice>, VfsError> {
    let index = NEXT_DEVICE.fetch_add(1, Ordering::Relaxed);
    let live = Live {
        key: Bitmap::with_max(KEY_MAX),
        led: Bitmap::with_max(LED_MAX),
        sw: Bitmap::with_max(SW_MAX),
        snd: Bitmap::with_max(SND_MAX),
        abs: capabilities.abs_info.clone(),
        repeat: capabilities.repeat,
        grabbed_by: None,
        pending_sync: false,
        frame_time: None,
    };
    let device = Arc::new(InputDevice {
        index,
        capabilities,
        live: SpinLock::new(live),
        clients: SpinLock::new(Vec::new()),
        output: SpinLock::new(None),
    });
    publish(&device)?;
    DEVICES.lock().push(device.clone());
    Ok(device)
}

fn publish(device: &Arc<InputDevice>) -> Result<(), VfsError> {
    let caps = &device.capabilities;
    let identifier = Directory::new(0o555, false);
    identifier.insert(
        "bustype",
        Attribute::text(format!("{:04x}\n", caps.bustype)),
    )?;
    identifier.insert("vendor", Attribute::text(format!("{:04x}\n", caps.vendor)))?;
    identifier.insert(
        "product",
        Attribute::text(format!("{:04x}\n", caps.product)),
    )?;
    identifier.insert(
        "version",
        Attribute::text(format!("{:04x}\n", caps.version)),
    )?;
    let capability_dir = Directory::new(0o555, false);
    for (name, map) in [
        ("ev", &caps.ev),
        ("key", &caps.key),
        ("rel", &caps.rel),
        ("abs", &caps.abs),
        ("msc", &caps.msc),
        ("led", &caps.led),
        ("sw", &caps.sw),
        ("snd", &caps.snd),
        ("ff", &caps.ff),
    ] {
        capability_dir.insert(name, Attribute::text(map.sysfs_text()))?;
    }
    let parent_name = format!("input{}", device.index);
    let parent = crate::device::register_with_uevent(
        &parent_name,
        "input",
        None,
        None,
        vec![
            (
                "name",
                Attribute::text(format!("{}\n", caps.name)) as Arc<dyn Inode>,
            ),
            ("phys", Attribute::text(format!("{}\n", caps.phys))),
            ("uniq", Attribute::text(format!("{}\n", caps.uniq))),
            ("properties", Attribute::text(caps.properties.sysfs_text())),
            ("id", identifier),
            ("capabilities", capability_dir),
        ],
        vec![
            (
                String::from("PRODUCT"),
                format!(
                    "{:x}/{:x}/{:x}/{:x}",
                    caps.bustype, caps.vendor, caps.product, caps.version
                ),
            ),
            (String::from("NAME"), format!("\"{}\"", caps.name)),
            (String::from("PHYS"), format!("\"{}\"", caps.phys)),
            (String::from("EV"), caps.ev.sysfs_text().trim().into()),
            (String::from("KEY"), caps.key.sysfs_text().trim().into()),
            (String::from("REL"), caps.rel.sysfs_text().trim().into()),
            (String::from("ABS"), caps.abs.sysfs_text().trim().into()),
            (
                String::from("PROP"),
                caps.properties.sysfs_text().trim().into(),
            ),
        ],
    )?;
    let minor = EVDEV_MINOR_BASE + u64::from(device.index);
    let backend: Arc<dyn Inode> = Arc::new(EvdevNode {
        device: device.clone(),
        ino: crate::vfs::tree::ino(),
        rdev: makedev(INPUT_MAJOR, minor),
    });
    let event_name = format!("event{}", device.index);
    let node = format!("input/{}", event_name);
    crate::device::register_child(
        &parent,
        &event_name,
        "input",
        Some(&node),
        Some(backend),
        vec![],
    )?;
    Ok(())
}

/// One open of `/dev/input/eventN`.
struct EvdevFile {
    device: Arc<InputDevice>,
    client: Arc<Client>,
    nonblock: SpinLock<bool>,
    ino: u64,
    rdev: u64,
}

/// The device node; each `open` creates an `EvdevFile` session.
struct EvdevNode {
    device: Arc<InputDevice>,
    ino: u64,
    rdev: u64,
}

impl Inode for EvdevNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(self.ino, InodeKind::CharDevice, 0o660, 0);
        stat.rdev = self.rdev;
        Ok(stat)
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        static NEXT_CLIENT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
        let client = Arc::new(Client {
            id: NEXT_CLIENT.fetch_add(1, Ordering::Relaxed),
            state: SpinLock::new(ClientState {
                queue: VecDeque::new(),
                clock: CLOCK_REALTIME,
                masks: vec![None; EV_MAX + 1],
                revoked: false,
            }),
        });
        self.device.clients.lock().push(client.clone());
        Ok(Some(Arc::new(EvdevFile {
            device: self.device.clone(),
            client,
            nonblock: SpinLock::new(flags & 0o4000 != 0),
            ino: self.ino,
            rdev: self.rdev,
        })))
    }
}

impl Drop for EvdevFile {
    fn drop(&mut self) {
        self.device
            .clients
            .lock()
            .retain(|client| !Arc::ptr_eq(client, &self.client));
        let mut live = self.device.live.lock();
        if live.grabbed_by == Some(self.client.id) {
            live.grabbed_by = None;
        }
    }
}

impl EvdevFile {
    fn copy_bits(map: &[u8], size: usize, pointer: u64) -> Result<u64, VfsError> {
        let count = size.min(map.len());
        uaccess::write(pointer, &map[..count])?;
        Ok(count as u64)
    }

    fn copy_text(text: &str, size: usize, pointer: u64) -> Result<u64, VfsError> {
        if text.is_empty() {
            return Err(VfsError::NotFound);
        }
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        let count = size.min(bytes.len());
        uaccess::write(pointer, &bytes[..count])?;
        Ok(count as u64)
    }

    fn encode(event: &Event, clock: u32) -> [u8; EVENT_SIZE] {
        let stamp = if clock == CLOCK_REALTIME {
            event.realtime
        } else {
            event.monotonic
        };
        let mut raw = [0u8; EVENT_SIZE];
        raw[0..8].copy_from_slice(&(stamp / 1_000_000).to_le_bytes());
        raw[8..16].copy_from_slice(&(stamp % 1_000_000).to_le_bytes());
        raw[16..18].copy_from_slice(&event.kind.to_le_bytes());
        raw[18..20].copy_from_slice(&event.code.to_le_bytes());
        raw[20..24].copy_from_slice(&event.value.to_le_bytes());
        raw
    }
}

impl Inode for EvdevFile {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(self.ino, InodeKind::CharDevice, 0o660, 0);
        stat.rdev = self.rdev;
        Ok(stat)
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        *self.nonblock.lock() = nonblock;
        Ok(())
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(Arc::as_ptr(&self.client));
    }

    fn poll(&self) -> u32 {
        let state = self.client.state.lock();
        if state.revoked {
            return 8 | 16;
        }
        // Output events can always be written.
        4 | u32::from(!state.queue.is_empty())
    }

    /// Reads return a whole number of events, never fewer than one.
    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        if buffer.len() < EVENT_SIZE {
            return Err(VfsError::InvalidArgument);
        }
        loop {
            {
                let mut state = self.client.state.lock();
                if state.revoked {
                    return Err(VfsError::NoDevice);
                }
                if !state.queue.is_empty() {
                    let clock = state.clock;
                    let mut written = 0;
                    while written + EVENT_SIZE <= buffer.len() {
                        let Some(event) = state.queue.pop_front() else {
                            break;
                        };
                        buffer[written..written + EVENT_SIZE]
                            .copy_from_slice(&Self::encode(&event, clock));
                        written += EVENT_SIZE;
                    }
                    return Ok(written);
                }
            }
            if *self.nonblock.lock() {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::ipc::block_briefly();
        }
    }

    /// Writes inject output events such as LED state, as on Linux.
    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        if buffer.len() < EVENT_SIZE {
            return Err(VfsError::InvalidArgument);
        }
        let mut written = 0;
        while written + EVENT_SIZE <= buffer.len() {
            let raw = &buffer[written..written + EVENT_SIZE];
            let kind = u16::from_le_bytes([raw[16], raw[17]]);
            let code = u16::from_le_bytes([raw[18], raw[19]]);
            let value = i32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]);
            if kind == EV_LED || kind == EV_SND {
                self.device.report(kind, code, value);
                if let Some(hook) = *self.device.output.lock() {
                    hook(kind, code, value);
                }
            }
            written += EVENT_SIZE;
        }
        Ok(written)
    }

    fn ioctl(&self, command: u64, arg: u64) -> Result<u64, VfsError> {
        let kind = ((command >> 8) & 0xff) as u8;
        if kind != b'E' {
            return Err(VfsError::NotATty);
        }
        let number = (command & 0xff) as usize;
        let size = ((command >> 16) & 0x3fff) as usize;
        let direction = (command >> 30) & 3;
        let caps = &self.device.capabilities;
        const READ: u64 = 2;
        const WRITE: u64 = 1;
        match number {
            0x01 => {
                uaccess::write_value(arg, &EV_VERSION)?;
                Ok(0)
            }
            0x02 => {
                let raw = [caps.bustype, caps.vendor, caps.product, caps.version];
                uaccess::write_value(arg, &raw)?;
                Ok(0)
            }
            0x03 if direction == READ => {
                let repeat = self.device.live.lock().repeat;
                uaccess::write_value(arg, &[repeat[0] as u32, repeat[1] as u32])?;
                Ok(0)
            }
            0x03 if direction == WRITE => {
                let raw: [u32; 2] = uaccess::read_value(arg)?;
                if !caps.ev.get(EV_REP as usize) {
                    return Err(VfsError::NotSupported);
                }
                self.device.live.lock().repeat = [raw[0] as i32, raw[1] as i32];
                Ok(0)
            }
            // Scancode remapping: no driver here has a mutable keymap.
            0x04 | 0x05 => Err(VfsError::InvalidArgument),
            0x06 => Self::copy_text(&caps.name, size, arg),
            0x07 => Self::copy_text(&caps.phys, size, arg),
            0x08 => Self::copy_text(&caps.uniq, size, arg),
            0x09 => Self::copy_bits(caps.properties.bytes(), size, arg),
            // ABS_MT slot values: no multitouch device is registered.
            0x0a => Err(VfsError::InvalidArgument),
            0x18 => {
                let bytes = self.device.live.lock().key.bytes().to_vec();
                Self::copy_bits(&bytes, size, arg)
            }
            0x19 => {
                let bytes = self.device.live.lock().led.bytes().to_vec();
                Self::copy_bits(&bytes, size, arg)
            }
            0x1a => {
                let bytes = self.device.live.lock().snd.bytes().to_vec();
                Self::copy_bits(&bytes, size, arg)
            }
            0x1b => {
                let bytes = self.device.live.lock().sw.bytes().to_vec();
                Self::copy_bits(&bytes, size, arg)
            }
            0x20..=0x3f => {
                let event = number - 0x20;
                if event == 0 {
                    return Self::copy_bits(caps.ev.bytes(), size, arg);
                }
                if event == EV_REP as usize {
                    let mut map = Bitmap::with_max(REP_MAX);
                    if caps.ev.get(EV_REP as usize) {
                        map.set(0);
                        map.set(1);
                    }
                    return Self::copy_bits(map.bytes(), size, arg);
                }
                match caps.bitmap_for(event) {
                    Some(map) => Self::copy_bits(map.bytes(), size, arg),
                    None => {
                        // Valid but empty event types report zero codes.
                        let zero = vec![0u8; size.min(128)];
                        Self::copy_bits(&zero, size, arg)
                    }
                }
            }
            0x40..=0x7f => {
                let axis = number - 0x40;
                if !caps.abs.get(axis) {
                    return Err(VfsError::InvalidArgument);
                }
                let info = self.device.live.lock().abs[axis];
                let raw = [
                    info.value,
                    info.minimum,
                    info.maximum,
                    info.fuzz,
                    info.flat,
                    info.resolution,
                ];
                uaccess::write_value(arg, &raw)?;
                Ok(0)
            }
            0x80 | 0x81 => Err(VfsError::InvalidArgument),
            0x84 => {
                uaccess::write_value(arg, &0i32)?;
                Ok(0)
            }
            0x90 => {
                let mut live = self.device.live.lock();
                if arg != 0 {
                    if live.grabbed_by.is_some() {
                        return Err(VfsError::Busy);
                    }
                    live.grabbed_by = Some(self.client.id);
                } else {
                    if live.grabbed_by != Some(self.client.id) {
                        return Err(VfsError::InvalidArgument);
                    }
                    live.grabbed_by = None;
                }
                Ok(0)
            }
            0x91 => {
                if arg != 0 {
                    return Err(VfsError::InvalidArgument);
                }
                self.client.state.lock().revoked = true;
                let mut live = self.device.live.lock();
                if live.grabbed_by == Some(self.client.id) {
                    live.grabbed_by = None;
                }
                drop(live);
                crate::ipc::wake_pollers();
                Ok(0)
            }
            0x92 | 0x93 => {
                // struct input_mask { u32 type; u32 codes_size; u64 codes_ptr; }
                let raw: [u32; 2] = uaccess::read_value(arg)?;
                let pointer: u64 = uaccess::read_value(arg + 8)?;
                let event = raw[0] as usize;
                let limit = match event as u16 {
                    0 => EV_MAX,
                    EV_KEY => KEY_MAX,
                    EV_REL => REL_MAX,
                    EV_ABS => ABS_MAX,
                    EV_MSC => MSC_MAX,
                    EV_LED => LED_MAX,
                    EV_SW => SW_MAX,
                    EV_SND => SND_MAX,
                    EV_FF => FF_MAX,
                    _ => return Err(VfsError::InvalidArgument),
                };
                let length = (limit + 8) / 8;
                if number == 0x92 {
                    let state = self.client.state.lock();
                    let bytes = state.masks[event]
                        .as_ref()
                        .map_or_else(|| vec![0xFFu8; length], |map| map.bytes().to_vec());
                    drop(state);
                    Self::copy_bits(&bytes, raw[1] as usize, pointer)?;
                } else {
                    let count = (raw[1] as usize).min(length);
                    let bytes = uaccess::read_vec(pointer, count, length)?;
                    let mut map = Bitmap::with_max(limit);
                    map.0[..count].copy_from_slice(&bytes);
                    self.client.state.lock().masks[event] = Some(map);
                }
                Ok(0)
            }
            0xa0 => {
                let clock: u32 = uaccess::read_value(arg)?;
                if !matches!(clock, CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME) {
                    return Err(VfsError::InvalidArgument);
                }
                self.client.state.lock().clock = clock;
                Ok(0)
            }
            _ => {
                if direction == WRITE && (0xc0..=0xff).contains(&number) {
                    let axis = number - 0xc0;
                    if !caps.abs.get(axis) {
                        return Err(VfsError::InvalidArgument);
                    }
                    let raw: [i32; 6] = uaccess::read_value(arg)?;
                    let mut live = self.device.live.lock();
                    let info = &mut live.abs[axis];
                    info.value = raw[0];
                    info.minimum = raw[1];
                    info.maximum = raw[2];
                    info.fuzz = raw[3];
                    info.flat = raw[4];
                    info.resolution = raw[5];
                    return Ok(0);
                }
                Err(VfsError::InvalidArgument)
            }
        }
    }
}

/// Bring up all input drivers. Runs once the device model is ready.
pub fn init() {
    ps2::register();
    #[cfg(target_arch = "x86_64")]
    virtio::init();
}
