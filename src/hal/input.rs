/*
 *
 *       src/hal/input.rs
 *       HAL input device class (evdev) for drivers
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! C ABI over `input::{Capabilities, register, InputDevice::report, sync,
//! unregister}`. Event types and codes are Linux's, so a driver ported from
//! Linux reports exactly what it would there.

use crate::input::{
    self, ABS_MAX, Capabilities, EV_ABS, EV_KEY, EV_LED, EV_MSC, EV_REL, InputDevice,
};
use alloc::{boxed::Box, string::String, sync::Arc};
use core::ffi::{c_char, c_int, c_uint, c_ushort};

pub struct InputHandle {
    capabilities: Option<Capabilities>,
    device: Option<Arc<InputDevice>>,
}

fn text(pointer: *const c_char) -> String {
    String::from_utf8_lossy(crate::module::hw::c_string(pointer)).into_owned()
}

/// # Safety
/// `name` and `phys` are NUL-terminated strings or null.
pub unsafe extern "C" fn spk_input_alloc(
    name: *const c_char,
    phys: *const c_char,
    bustype: c_ushort,
    vendor: c_ushort,
    product: c_ushort,
) -> *mut InputHandle {
    let capabilities = Capabilities::new(&text(name), &text(phys), bustype, vendor, product);
    Box::into_raw(Box::new(InputHandle {
        capabilities: Some(capabilities),
        device: None,
    }))
}

/// Declare that the device reports `code` of event `kind` (before register).
///
/// # Safety
/// `handle` came from `spk_input_alloc`.
pub unsafe extern "C" fn spk_input_set_capability(
    handle: *mut InputHandle,
    kind: c_uint,
    code: c_uint,
) -> c_int {
    // SAFETY: caller contract.
    let Some(capabilities) = (unsafe { handle.as_mut() }).and_then(|h| h.capabilities.as_mut())
    else {
        return -(crate::vfs::abi::EINVAL as c_int);
    };
    let code = code as usize;
    let map = match kind as u16 {
        EV_KEY => &mut capabilities.key,
        EV_REL => &mut capabilities.rel,
        EV_ABS => &mut capabilities.abs,
        EV_MSC => &mut capabilities.msc,
        EV_LED => &mut capabilities.led,
        _ => return -(crate::vfs::abi::EINVAL as c_int),
    };
    if code >= map.bytes().len() * 8 {
        return -(crate::vfs::abi::EINVAL as c_int);
    }
    map.set(code);
    capabilities.ev.set(kind as usize);
    0
}

/// # Safety
/// `handle` came from `spk_input_alloc`.
pub unsafe extern "C" fn spk_input_set_abs(
    handle: *mut InputHandle,
    code: c_uint,
    minimum: c_int,
    maximum: c_int,
) -> c_int {
    // SAFETY: caller contract.
    let Some(capabilities) = (unsafe { handle.as_mut() }).and_then(|h| h.capabilities.as_mut())
    else {
        return -(crate::vfs::abi::EINVAL as c_int);
    };
    let code = code as usize;
    if code > ABS_MAX {
        return -(crate::vfs::abi::EINVAL as c_int);
    }
    capabilities.ev.set(EV_ABS as usize);
    capabilities.abs.set(code);
    capabilities.abs_info[code].minimum = minimum;
    capabilities.abs_info[code].maximum = maximum;
    0
}

/// Publish the device (`/dev/input/eventN`, sysfs, uevent).
///
/// # Safety
/// `handle` came from `spk_input_alloc` and is not yet registered.
pub unsafe extern "C" fn spk_input_register(handle: *mut InputHandle) -> c_int {
    // SAFETY: caller contract.
    let Some(handle) = (unsafe { handle.as_mut() }) else {
        return -(crate::vfs::abi::EINVAL as c_int);
    };
    let Some(capabilities) = handle.capabilities.take() else {
        return -(crate::vfs::abi::EINVAL as c_int);
    };
    match input::register(capabilities) {
        Ok(device) => {
            handle.device = Some(device);
            0
        }
        Err(error) => error.to_errno() as c_int,
    }
}

/// Report one event; callable from interrupt context.
///
/// # Safety
/// `handle` is a registered input device.
pub unsafe extern "C" fn spk_input_event(
    handle: *mut InputHandle,
    kind: c_uint,
    code: c_uint,
    value: c_int,
) {
    // SAFETY: caller contract.
    if let Some(device) = unsafe { handle.as_ref() }.and_then(|h| h.device.as_ref()) {
        device.report(kind as u16, code as u16, value);
    }
}

/// End an event frame (EV_SYN/SYN_REPORT); callable from interrupt context.
///
/// # Safety
/// `handle` is a registered input device.
pub unsafe extern "C" fn spk_input_sync(handle: *mut InputHandle) {
    // SAFETY: caller contract.
    if let Some(device) = unsafe { handle.as_ref() }.and_then(|h| h.device.as_ref()) {
        device.sync();
    }
}

/// Remove a registered device and free the handle.
///
/// # Safety
/// `handle` came from `spk_input_alloc`; no event calls may follow.
pub unsafe extern "C" fn spk_input_unregister(handle: *mut InputHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller contract.
    let handle = unsafe { Box::from_raw(handle) };
    if let Some(device) = &handle.device {
        input::unregister(device);
    }
}

/// Free a handle that was never registered.
///
/// # Safety
/// `handle` came from `spk_input_alloc`.
pub unsafe extern "C" fn spk_input_free(handle: *mut InputHandle) {
    // SAFETY: same contract as unregister; unregistering is harmless.
    unsafe { spk_input_unregister(handle) }
}
