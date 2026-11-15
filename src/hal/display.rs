/*
 *
 *       src/hal/display.rs
 *       HAL display class: firmware framebuffer description and DRM devices
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A loadable display driver registers one pipe with a fixed mode and the
//! formats it can present; the DRM core (`crate::drm`) provides the whole
//! userspace ABI and calls back with validated framebuffers and damage.
//! `spk_sysfb_get` describes the framebuffer the bootloader set up (Linux
//! `screen_info`/sysfb), and refuses while a native GPU driver owns the
//! display, as Linux removes simpledrm once a real driver takes the aperture.

use crate::drm::{self, ConnectorType, DeviceConfig, Driver, Mode, PipeConfig, PlaneUpdate};
use crate::vfs::VfsError;
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::ffi::{c_char, c_int, c_uint, c_void};

#[repr(C)]
pub struct Sysfb {
    physical: u64,
    virt: *mut c_void,
    size: u64,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u32,
    red_size: u8,
    red_shift: u8,
    green_size: u8,
    green_shift: u8,
    blue_size: u8,
    blue_shift: u8,
    reserved: [u8; 2],
}

/// # Safety
/// `out` points to writable `struct spk_sysfb`.
pub unsafe extern "C" fn spk_sysfb_get(out: *mut Sysfb) -> c_int {
    if out.is_null() {
        return -(crate::vfs::abi::EINVAL as c_int);
    }
    if drm::firmware_fb_claimed() {
        return -(crate::vfs::abi::EBUSY as c_int);
    }
    let Some(fb) = crate::boot::framebuffer() else {
        return -(crate::vfs::abi::ENODEV as c_int);
    };
    let virt = fb.address() as u64;
    let physical = crate::mm::vmm::translate(virt).unwrap_or(0);
    let info = Sysfb {
        physical,
        virt: fb.address().cast(),
        size: fb.pitch * fb.height,
        width: fb.width as u32,
        height: fb.height as u32,
        pitch: fb.pitch as u32,
        bpp: fb.bpp as u32,
        red_size: fb.red_mask_size,
        red_shift: fb.red_mask_shift,
        green_size: fb.green_mask_size,
        green_shift: fb.green_mask_shift,
        blue_size: fb.blue_mask_size,
        blue_shift: fb.blue_mask_shift,
        reserved: [0; 2],
    };
    // SAFETY: caller contract.
    unsafe { out.write(info) };
    0
}

#[repr(C)]
pub struct DisplayMode {
    width: c_uint,
    height: c_uint,
    refresh_hz: c_uint,
    width_mm: c_uint,
    height_mm: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DisplayFb {
    pixels: *const c_void,
    pitch: c_uint,
    format: c_uint,
    width: c_uint,
    height: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DisplayOps {
    check: Option<unsafe extern "C" fn(*mut c_void, c_uint) -> c_int>,
    enable: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    disable: Option<unsafe extern "C" fn(*mut c_void)>,
    update: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const DisplayFb,
            *const drm::Rect,
            *const drm::Rect,
            c_uint,
        ),
    >,
    lastclose: Option<unsafe extern "C" fn(*mut c_void)>,
}

pub const DISPLAY_PREFER_SHADOW: c_int = 1;
pub const DISPLAY_TAKES_CONSOLE: c_int = 2;

struct HalDriver {
    ops: DisplayOps,
    private: *mut c_void,
}

// SAFETY: the HAL contract makes callbacks callable from any CPU in process
// context; the core serializes them under its modeset lock.
unsafe impl Send for HalDriver {}
unsafe impl Sync for HalDriver {}

fn errno(result: c_int) -> Result<(), VfsError> {
    match result {
        0 => Ok(()),
        error if error == -(crate::vfs::abi::ENOMEM as c_int) => Err(VfsError::NoMemory),
        error if error == -(crate::vfs::abi::EBUSY as c_int) => Err(VfsError::Busy),
        _ => Err(VfsError::InvalidArgument),
    }
}

impl Driver for HalDriver {
    fn check(
        &self,
        _pipe: usize,
        _mode: Option<&Mode>,
        update: &PlaneUpdate,
    ) -> Result<(), VfsError> {
        match (self.ops.check, update.framebuffer) {
            // SAFETY: driver callback with its own private pointer.
            (Some(check), Some(fb)) => errno(unsafe { check(self.private, fb.format) }),
            _ => Ok(()),
        }
    }

    fn set_mode(&self, _pipe: usize, mode: Option<&Mode>) -> Result<(), VfsError> {
        // SAFETY: as above.
        unsafe {
            match mode {
                Some(_) => self
                    .ops
                    .enable
                    .map_or(Ok(()), |enable| errno(enable(self.private))),
                None => {
                    if let Some(disable) = self.ops.disable {
                        disable(self.private);
                    }
                    Ok(())
                }
            }
        }
    }

    fn update_plane(&self, _pipe: usize, update: &PlaneUpdate) -> Result<(), VfsError> {
        let (Some(fb), Some(callback)) = (update.framebuffer, self.ops.update) else {
            return Ok(());
        };
        // HAL devices allocate shmem dumb buffers, which are CPU mapped.
        let pixels = fb.pixels().ok_or(VfsError::InvalidArgument)?;
        let view = DisplayFb {
            pixels: pixels.cast(),
            pitch: fb.pitch,
            format: fb.format,
            width: fb.width,
            height: fb.height,
        };
        // SAFETY: the framebuffer, source and clips stay valid for the call;
        // clips lie inside the source rectangle (validated by the core).
        unsafe {
            callback(
                self.private,
                &view,
                &update.source,
                update.damage.as_ptr(),
                update.damage.len() as c_uint,
            )
        };
        Ok(())
    }

    fn lastclose(&self) {
        if let Some(lastclose) = self.ops.lastclose {
            // SAFETY: as above.
            unsafe { lastclose(self.private) };
        }
    }
}

pub struct DisplayHandle {
    device: Arc<drm::Device>,
}

fn text(pointer: *const c_char) -> String {
    String::from_utf8_lossy(crate::module::hw::c_string(pointer)).into_owned()
}

/// Register a single-pipe display; returns null on failure.
///
/// # Safety
/// Strings are NUL-terminated; `mode`, `formats[count]` and `ops` are
/// readable; `ops` outlives the device.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spk_display_register(
    name: *const c_char,
    description: *const c_char,
    mode: *const DisplayMode,
    formats: *const c_uint,
    count: c_uint,
    flags: c_int,
    ops: *const DisplayOps,
    private: *mut c_void,
) -> *mut DisplayHandle {
    // SAFETY: caller contract.
    let (Some(mode), Some(ops)) = (unsafe { mode.as_ref() }, unsafe { ops.as_ref() }) else {
        return core::ptr::null_mut();
    };
    if formats.is_null() || count == 0 || count > 64 || mode.width == 0 || mode.height == 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: caller contract.
    let formats: Vec<u32> =
        unsafe { core::slice::from_raw_parts(formats, count as usize) }.to_vec();
    let fixed = Mode::fixed(
        mode.width.min(u16::MAX as u32) as u16,
        mode.height.min(u16::MAX as u32) as u16,
        if mode.refresh_hz == 0 {
            60
        } else {
            mode.refresh_hz
        },
    );
    let mut config = DeviceConfig::new(
        &text(name),
        &text(description),
        "20261115",
        alloc::vec![PipeConfig {
            connector_type: ConnectorType::Unknown,
            connected: true,
            modes: alloc::vec![fixed],
            width_mm: mode.width_mm,
            height_mm: mode.height_mm,
            edid: Vec::new(),
            formats,
            cursor: None,
        }],
    );
    // A fixed scanout size, as simpledrm: framebuffers may be larger
    // (panning via SRC_X/Y) but not smaller than the mode.
    config.min_size = (mode.width, mode.height);
    config.max_size = (mode.width.max(4096), mode.height.max(4096));
    config.prefer_shadow = flags & DISPLAY_PREFER_SHADOW != 0;
    config.takes_over_console = flags & DISPLAY_TAKES_CONSOLE != 0;
    let driver = Arc::new(HalDriver { ops: *ops, private });
    match drm::register(config, driver) {
        Ok(device) => Box::into_raw(Box::new(DisplayHandle { device })),
        Err(error) => {
            crate::kwarn!("hal: display registration failed: {:?}", error);
            core::ptr::null_mut()
        }
    }
}

/// # Safety
/// `handle` came from `spk_display_register`; process context.
pub unsafe extern "C" fn spk_display_unregister(handle: *mut DisplayHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller contract.
    let handle = unsafe { Box::from_raw(handle) };
    drm::unregister(&handle.device);
}
