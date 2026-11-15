/*
 *
 *       src/drm/mod.rs
 *       Hardware-independent DRM core: devices, files, GEM, KMS, vblank
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Modelled on Linux `drivers/gpu/drm`: the core owns the whole userspace
//! ABI (ioctls, object ids, properties, atomic state, GEM handles, mmap,
//! events, vblank accounting and master/lastclose rules); a driver only
//! describes its pipes and programs hardware from the validated state.
//!
//! * [`Driver`] is the Rust driver interface; `hal::display` wraps it for
//!   loadable C modules (simplefb) and [`virtio`] is the in-tree virtio-gpu
//!   driver (3D contexts, blobs and execbuffer through driver ioctls).
//! * The core also provides GEM handles with pluggable backing storage,
//!   PRIME dma-bufs, syncobj/sync_file fences, render nodes and
//!   driver-private per-file state, so a render-capable driver only adds
//!   its own command submission.
//! * Every legacy modeset entry point (SETCRTC, SETPLANE, PAGE_FLIP,
//!   DIRTYFB, DPMS) is translated into an atomic commit, as Linux's atomic
//!   helpers do, so drivers implement one update path.
//! * Devices without a vblank interrupt get a timer-simulated vblank at the
//!   mode's refresh rate (Linux `drm_crtc_vblank_start_timer`), which paces
//!   page-flip events, WAIT_VBLANK and CRTC_QUEUE_SEQUENCE.

pub mod file;
pub mod gem;
pub mod ioctl;
pub mod kms;
#[cfg(feature = "boot-self-test")]
pub mod self_test;
pub mod syncobj;
pub mod vblank;
pub mod virtio;

use crate::{sync::SpinLock, vfs::VfsError};
use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

pub use file::File;
pub use gem::{GemBacking, GemObject};
pub use ioctl::Ioctl;
pub use kms::{Framebuffer, Rect};

/// DRM fourcc codes the core knows how to describe.
pub mod fourcc {
    const fn code(a: u8, b: u8, c: u8, d: u8) -> u32 {
        a as u32 | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
    }
    pub const XRGB8888: u32 = code(b'X', b'R', b'2', b'4');
    pub const ARGB8888: u32 = code(b'A', b'R', b'2', b'4');
    pub const XBGR8888: u32 = code(b'X', b'B', b'2', b'4');
    pub const ABGR8888: u32 = code(b'A', b'B', b'2', b'4');
    pub const RGB565: u32 = code(b'R', b'G', b'1', b'6');
    pub const RGB888: u32 = code(b'R', b'G', b'2', b'4');
    pub const BGR888: u32 = code(b'B', b'G', b'2', b'4');
    pub const XRGB2101010: u32 = code(b'X', b'R', b'3', b'0');
    pub const ARGB2101010: u32 = code(b'A', b'R', b'3', b'0');

    /// Bytes per pixel of a single-plane format, or None if unknown.
    pub fn cpp(format: u32) -> Option<u32> {
        Some(match format {
            XRGB8888 | ARGB8888 | XBGR8888 | ABGR8888 | XRGB2101010 | ARGB2101010 => 4,
            RGB888 | BGR888 => 3,
            RGB565 => 2,
            _ => return None,
        })
    }

    /// Legacy ADDFB (bpp, depth) to fourcc, as `drm_mode_legacy_fb_format`.
    pub fn from_legacy(bpp: u32, depth: u32) -> Option<u32> {
        Some(match (bpp, depth) {
            (16, 16) => RGB565,
            (24, 24) => RGB888,
            (32, 24) => XRGB8888,
            (32, 30) => XRGB2101010,
            (32, 32) => ARGB8888,
            _ => return None,
        })
    }
}

/// A display timing (`struct drm_mode_modeinfo` without the name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub clock_khz: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vrefresh: u32,
    pub preferred: bool,
}

impl Mode {
    /// A fixed mode with CVT-like blanking for `width`x`height`@`refresh`,
    /// as simpledrm reports for a firmware framebuffer.
    pub fn fixed(width: u16, height: u16, refresh: u32) -> Self {
        let htotal = width.saturating_add(160);
        let vtotal = height.saturating_add(45);
        Self {
            clock_khz: (htotal as u32 * vtotal as u32 * refresh).div_ceil(1000),
            hdisplay: width,
            hsync_start: width.saturating_add(48),
            hsync_end: width.saturating_add(80),
            htotal,
            vdisplay: height,
            vsync_start: height.saturating_add(3),
            vsync_end: height.saturating_add(8),
            vtotal,
            vrefresh: refresh,
            preferred: true,
        }
    }

    /// Frame period in nanoseconds (60 Hz when the timing is incomplete).
    pub fn frame_ns(&self) -> u64 {
        let pixels = self.htotal as u64 * self.vtotal as u64;
        if self.clock_khz == 0 || pixels == 0 {
            return 16_666_667;
        }
        pixels * 1_000_000 / self.clock_khz as u64
    }

    pub(crate) fn encode(&self) -> [u8; 68] {
        let mut bytes = [0u8; 68];
        bytes[0..4].copy_from_slice(&self.clock_khz.to_le_bytes());
        for (offset, value) in [
            (4, self.hdisplay),
            (6, self.hsync_start),
            (8, self.hsync_end),
            (10, self.htotal),
            (12, 0),
            (14, self.vdisplay),
            (16, self.vsync_start),
            (18, self.vsync_end),
            (20, self.vtotal),
            (22, 0),
        ] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[24..28].copy_from_slice(&self.vrefresh.to_le_bytes());
        // flags: -hsync -vsync; type: DRIVER (| PREFERRED).
        bytes[28..32].copy_from_slice(&(2u32 | 8).to_le_bytes());
        let kind: u32 = 1 << 6 | if self.preferred { 1 << 3 } else { 0 };
        bytes[32..36].copy_from_slice(&kind.to_le_bytes());
        let name = alloc::format!("{}x{}", self.hdisplay, self.vdisplay);
        let length = name.len().min(31);
        bytes[36..36 + length].copy_from_slice(&name.as_bytes()[..length]);
        bytes
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 68 {
            return None;
        }
        let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let mode = Self {
            clock_khz: u32_at(0),
            hdisplay: u16_at(4),
            hsync_start: u16_at(6),
            hsync_end: u16_at(8),
            htotal: u16_at(10),
            vdisplay: u16_at(14),
            vsync_start: u16_at(16),
            vsync_end: u16_at(18),
            vtotal: u16_at(20),
            vrefresh: u32_at(24),
            preferred: u32_at(32) & (1 << 3) != 0,
        };
        (mode.hdisplay != 0 && mode.vdisplay != 0).then_some(mode)
    }

    /// Same timing, ignoring the preferred flag.
    pub fn same_timing(&self, other: &Mode) -> bool {
        Mode {
            preferred: false,
            ..*self
        } == Mode {
            preferred: false,
            ..*other
        }
    }
}

/// Linux `DRM_MODE_CONNECTOR_*` values used here.
#[derive(Clone, Copy, Debug)]
pub enum ConnectorType {
    Unknown = 0,
    Vga = 1,
    Dvi = 3,
    DisplayPort = 10,
    Hdmi = 11,
    Virtual = 15,
    Edp = 14,
}

/// One display pipe: connector -> encoder -> CRTC with a primary plane and
/// an optional cursor plane.
#[derive(Clone)]
pub struct PipeConfig {
    pub connector_type: ConnectorType,
    /// A monitor is attached (modes are only reported when connected).
    pub connected: bool,
    pub modes: Vec<Mode>,
    pub width_mm: u32,
    pub height_mm: u32,
    pub edid: Vec<u8>,
    /// Formats the primary plane accepts (native and emulated).
    pub formats: Vec<u32>,
    /// Cursor plane size, if the hardware has one.
    pub cursor: Option<(u32, u32)>,
}

/// Static description of a device, given at registration.
#[derive(Clone)]
pub struct DeviceConfig {
    pub name: String,
    pub description: String,
    pub date: String,
    pub version: (i32, i32, i32),
    pub pipes: Vec<PipeConfig>,
    pub min_size: (u32, u32),
    pub max_size: (u32, u32),
    /// The framebuffer is a copy target in system memory; userspace should
    /// render to its own shadow buffer and call DIRTYFB (DRM_CAP_DUMB_
    /// PREFER_SHADOW), as for simpledrm.
    pub prefer_shadow: bool,
    /// Scanout comes from the firmware framebuffer the text console uses;
    /// the console yields while userspace drives the display.
    pub takes_over_console: bool,
    /// A native display driver: from now on the firmware framebuffer is not
    /// available to simpledrm (Linux aperture removal of sysfb devices).
    pub claims_firmware_fb: bool,
    /// Publish /dev/dri/renderD(128+N) (DRIVER_RENDER).
    pub render: bool,
    /// PRIME import/export of GEM objects as dma-buf fds (DRIVER_PRIME).
    pub prime: bool,
    /// syncobj ioctls, binary and timeline (DRIVER_SYNCOBJ[_TIMELINE]).
    pub syncobj: bool,
    /// Bus parent for /sys/class/drm/*/device (e.g. a PCI address).
    pub parent: Option<String>,
    /// Extra read-only sysfs attributes of the card (vendor, device, ...).
    pub attributes: Vec<(&'static str, String)>,
}

impl DeviceConfig {
    /// A configuration with the core defaults for everything but the name
    /// and pipes.
    pub fn new(name: &str, description: &str, date: &str, pipes: Vec<PipeConfig>) -> Self {
        Self {
            name: String::from(name),
            description: String::from(description),
            date: String::from(date),
            version: (1, 0, 0),
            pipes,
            min_size: (1, 1),
            max_size: (8192, 8192),
            prefer_shadow: false,
            takes_over_console: false,
            claims_firmware_fb: false,
            render: false,
            prime: false,
            syncobj: false,
            parent: None,
            attributes: Vec::new(),
        }
    }
}

/// What one pipe should display after a commit (all values validated).
pub struct PlaneUpdate<'a> {
    pub framebuffer: Option<&'a Framebuffer>,
    /// Source rectangle in framebuffer pixels.
    pub source: Rect,
    /// Destination on the CRTC.
    pub destination: Rect,
    /// Changed regions in framebuffer pixels (clipped to `source`); the
    /// whole source when the framebuffer itself changed.
    pub damage: &'a [Rect],
}

/// Cursor image and position for drivers with a cursor plane.
pub struct CursorUpdate<'a> {
    pub framebuffer: Option<&'a Framebuffer>,
    pub x: i32,
    pub y: i32,
    pub hot_x: i32,
    pub hot_y: i32,
}

/// The hardware side of a DRM device (Linux `drm_driver` plus the plane/CRTC
/// helper callbacks a simple display needs). Calls happen in process
/// context with the device's modeset lock held; they may sleep.
pub trait Driver: Send + Sync {
    /// Reject a configuration the hardware cannot show (beyond the core's
    /// generic checks of formats, sizes and coverage).
    fn check(
        &self,
        _pipe: usize,
        _mode: Option<&Mode>,
        _update: &PlaneUpdate,
    ) -> Result<(), VfsError> {
        Ok(())
    }
    /// Turn a pipe on with `mode`, or off (`None`).
    fn set_mode(&self, pipe: usize, mode: Option<&Mode>) -> Result<(), VfsError>;
    /// Show `update` on an enabled pipe.
    fn update_plane(&self, pipe: usize, update: &PlaneUpdate) -> Result<(), VfsError>;
    fn update_cursor(&self, _pipe: usize, _update: &CursorUpdate) -> Result<(), VfsError> {
        Err(VfsError::NoSuchDeviceOrAddress)
    }
    /// The last file closed (Linux `lastclose`): give the display back to
    /// the console or a default configuration.
    fn lastclose(&self) {}
    /// The device is being unregistered.
    fn release(&self) {}

    /// Allocate a dumb buffer; returns the backing and the row pitch. The
    /// default is shmem with 64-byte aligned rows, which keeps every line
    /// start cache-line aligned for line-by-line copies.
    fn create_dumb(
        &self,
        width: u32,
        height: u32,
        bpp: u32,
    ) -> Result<(Arc<dyn GemBacking>, u32), VfsError> {
        let pitch = (width * bpp.div_ceil(8)).div_ceil(64) * 64;
        Ok((
            Arc::new(gem::Shmem::new(pitch as u64 * height as u64)?),
            pitch,
        ))
    }
    /// Reject a framebuffer the hardware cannot scan out (ADDFB time).
    fn check_framebuffer(&self, _fb: &Framebuffer) -> Result<(), VfsError> {
        Ok(())
    }
    /// Size and direction of driver-specific ioctl `nr` (0x40..0xa0).
    fn ioctl_descriptor(&self, _nr: u8) -> Option<(usize, u32)> {
        None
    }
    /// Per-open driver state; it handles the driver-specific ioctls.
    fn open(&self, _file: &Arc<File>) -> Result<Option<Arc<dyn DriverFile>>, VfsError> {
        Ok(None)
    }
    /// Make progress on outstanding fences (called while waiting on them).
    fn kick(&self) {}
}

/// Driver state of one open file (Linux `drm_file::driver_priv` plus the
/// driver's ioctl table).
pub trait DriverFile: Send + Sync {
    /// A driver-specific ioctl (`arg.nr` in 0x40..0xa0).
    fn ioctl(&self, file: &File, arg: &mut Ioctl) -> Result<(), VfsError>;
    /// Called before the file reports poll/read readiness: turn completed
    /// work into events.
    fn poll(&self, _file: &File) {}
    /// Whether `poll` has work that completes without an event wake-up, so
    /// pollers must re-check periodically.
    fn needs_tick(&self) -> bool {
        false
    }
}

/// Set once a native display driver owns the firmware framebuffer.
static FIRMWARE_FB_CLAIMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Whether a native driver took the display from the firmware framebuffer
/// (simpledrm must not bind then).
pub fn firmware_fb_claimed() -> bool {
    FIRMWARE_FB_CLAIMED.load(Ordering::Acquire)
}

pub struct Device {
    pub id: u64,
    pub minor: u32,
    pub config: DeviceConfig,
    pub driver: Arc<dyn Driver>,
    pub(crate) kms: SpinLock<kms::State>,
    /// Serializes commits (Linux modeset locks); commits may sleep.
    pub(crate) modeset: crate::sched::WaitQueue,
    pub(crate) modeset_busy: core::sync::atomic::AtomicBool,
    pub(crate) vblank: Vec<vblank::Crtc>,
    pub(crate) gem: gem::Registry,
    pub(crate) files: SpinLock<Vec<alloc::sync::Weak<file::File>>>,
    pub(crate) master: AtomicU64,
    pub(crate) next_file: AtomicU64,
    pub(crate) nodes: SpinLock<Vec<u64>>,
    pub(crate) gone: core::sync::atomic::AtomicBool,
}

static DEVICES: SpinLock<Vec<Arc<Device>>> = SpinLock::new(Vec::new());
static NEXT_DEVICE: AtomicU64 = AtomicU64::new(1);
/// Minor numbers in use (card N, renderD 128+N).
static MINORS: SpinLock<u64> = SpinLock::new(0);

fn allocate_minor() -> Option<u32> {
    let mut used = MINORS.lock();
    let minor = (0..64).find(|&m| *used & (1 << m) == 0)?;
    *used |= 1 << minor;
    Some(minor as u32)
}

/// Register a device and publish /dev/dri/cardN with its sysfs connectors.
pub fn register(config: DeviceConfig, driver: Arc<dyn Driver>) -> Result<Arc<Device>, VfsError> {
    if config.pipes.is_empty()
        || config
            .pipes
            .iter()
            .any(|p| p.modes.is_empty() || p.formats.is_empty())
    {
        return Err(VfsError::InvalidArgument);
    }
    if config.claims_firmware_fb {
        FIRMWARE_FB_CLAIMED.store(true, Ordering::Release);
    }
    let minor = allocate_minor().ok_or(VfsError::NoSpace)?;
    let pipes = config.pipes.len();
    let device = Arc::new(Device {
        id: NEXT_DEVICE.fetch_add(1, Ordering::Relaxed),
        minor,
        kms: SpinLock::new(kms::State::new(&config)),
        vblank: (0..pipes).map(|_| vblank::Crtc::new()).collect(),
        config,
        driver,
        modeset: crate::sched::WaitQueue::new(),
        modeset_busy: core::sync::atomic::AtomicBool::new(false),
        gem: gem::Registry::new(),
        files: SpinLock::new(Vec::new()),
        master: AtomicU64::new(0),
        next_file: AtomicU64::new(1),
        nodes: SpinLock::new(Vec::new()),
        gone: core::sync::atomic::AtomicBool::new(false),
    });
    if let Err(error) = file::publish(&device) {
        for node in device.nodes.lock().drain(..) {
            let _ = crate::device::unregister(node);
        }
        *MINORS.lock() &= !(1u64 << minor);
        return Err(error);
    }
    vblank::start_thread();
    DEVICES.lock().push(device.clone());
    crate::kinfo!(
        "drm: card{} '{}' ({}), {} pipe(s), {}x{}",
        minor,
        device.config.name,
        device.config.description,
        pipes,
        device.config.pipes[0].modes[0].hdisplay,
        device.config.pipes[0].modes[0].vdisplay
    );
    Ok(device)
}

/// Remove a device: open files see ENODEV, nodes disappear, scanout stops.
pub fn unregister(device: &Arc<Device>) {
    device.gone.store(true, Ordering::Release);
    DEVICES.lock().retain(|d| !Arc::ptr_eq(d, device));
    for node in device.nodes.lock().drain(..) {
        let _ = crate::device::unregister(node);
    }
    for pipe in 0..device.config.pipes.len() {
        let _ = device.driver.set_mode(pipe, None);
    }
    vblank::cancel_device(device);
    file::wake_all(device);
    if device.config.takes_over_console {
        crate::tty::vt::release_scanout();
    }
    device.driver.release();
    *MINORS.lock() &= !(1u64 << device.minor);
}

pub fn devices() -> Vec<Arc<Device>> {
    DEVICES.lock().clone()
}

impl Device {
    /// Run `f` with the modeset lock (sleeping, process context).
    pub(crate) fn modeset<R>(&self, f: impl FnOnce() -> R) -> R {
        while self
            .modeset_busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.modeset
                .wait_until(|| !self.modeset_busy.load(Ordering::Acquire));
        }
        let result = f();
        self.modeset_busy.store(false, Ordering::Release);
        self.modeset.wake_one();
        result
    }
}
