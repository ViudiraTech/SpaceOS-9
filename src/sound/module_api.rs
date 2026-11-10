/*
 *
 *       src/sound/module_api.rs
 *       Linux-shaped snd_* C ABI exported to loadable sound drivers
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Loadable drivers see the familiar Linux names.  The signatures are trimmed
//! where SpaceKernel has no equivalent object (`struct device`, `struct module`),
//! but the operations, callback table and lifetime rules follow
//! `sound/core/*.c`: create a card, add PCM and controls, register, then free.
//!
//! Every entry is reached through the module symbol whitelist in
//! `module::api::lookup`; nothing here is called by the kernel core.

use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    ffi::{CStr, c_char, c_int, c_void},
    ptr,
};

use crate::sound::{
    self,
    card::Card,
    control::{ControlElem, ControlOps},
    pcm::{self, Direction, Pcm, PcmOps, Substream},
    uapi,
};

macro_rules! export {
    ($name:literal, $function:path) => {
        ($name, $function as *const () as usize as u64)
    };
}

pub fn lookup(name: &str) -> Option<u64> {
    let entries: &[(&str, u64)] = &[
        export!("snd_card_new", snd_card_new),
        export!("snd_card_register", snd_card_register),
        export!("snd_card_free", snd_card_free),
        export!("snd_pcm_new", snd_pcm_new),
        export!("snd_pcm_set_ops", snd_pcm_set_ops),
        export!("snd_pcm_period_elapsed", snd_pcm_period_elapsed),
        export!("snd_pcm_stop", snd_pcm_stop),
        export!("snd_pcm_stop_xrun", snd_pcm_stop_xrun),
        export!("snd_pcm_hw_constraint_minmax", snd_pcm_hw_constraint_minmax),
        export!("snd_pcm_hw_constraint_mask64", snd_pcm_hw_constraint_mask64),
        export!("snd_pcm_set_formats", snd_pcm_set_formats),
        export!("snd_pcm_lib_malloc_pages", snd_pcm_lib_malloc_pages),
        export!("snd_pcm_lib_free_pages", snd_pcm_lib_free_pages),
        export!("snd_pcm_get_dma_addr", snd_pcm_get_dma_addr),
        export!("snd_pcm_get_buffer", snd_pcm_get_buffer),
        export!("snd_pcm_get_period_size", snd_pcm_get_period_size),
        export!("snd_pcm_get_buffer_size", snd_pcm_get_buffer_size),
        export!("snd_pcm_get_rate", snd_pcm_get_rate),
        export!("snd_pcm_get_channels", snd_pcm_get_channels),
        export!("snd_pcm_get_format", snd_pcm_get_format),
        export!("snd_ctl_new1", snd_ctl_new1),
        export!("snd_ctl_add", snd_ctl_add),
        export!("snd_ctl_notify", snd_ctl_notify),
        export!("snd_ctl_free_one", snd_ctl_free_one),
    ];
    entries
        .iter()
        .find(|(symbol, _)| *symbol == name)
        .map(|(_, address)| *address)
}

#[repr(C)]
pub struct snd_card {
    _private: [u8; 0],
}

#[repr(C)]
pub struct snd_pcm {
    _private: [u8; 0],
}

#[repr(C)]
pub struct snd_pcm_substream {
    _private: [u8; 0],
}

struct CardHandle {
    card: Arc<Card>,
}

struct PcmHandle {
    pcm: Arc<Pcm>,
}

fn c_string(pointer: *const c_char, fallback: &str) -> String {
    if pointer.is_null() {
        return String::from(fallback);
    }
    // SAFETY: the module contract requires a NUL-terminated string; the scan is
    // bounded by CStr and converted immediately.
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
    String::from(&*text)
}

fn card_from(raw: *mut snd_card) -> Option<Arc<Card>> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the pointer came from `snd_card_new` and stays owned by the
    // module until `snd_card_free`.
    Some(unsafe { &*(raw as *const CardHandle) }.card.clone())
}

fn pcm_from(raw: *mut snd_pcm) -> Option<Arc<Pcm>> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the pointer came from `snd_pcm_new`.
    Some(unsafe { &*(raw as *const PcmHandle) }.pcm.clone())
}

fn substream_from(raw: *mut snd_pcm_substream) -> Option<&'static Substream> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the pointer is the `Arc::as_ptr` value of the substream owned by
    // the card; it remains valid until the card is freed.
    Some(unsafe { &*(raw as *const Substream) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_card_new(
    index: c_int,
    id: *const c_char,
    driver: *const c_char,
    shortname: *const c_char,
    longname: *const c_char,
    card_out: *mut *mut snd_card,
) -> c_int {
    if card_out.is_null() {
        return -22;
    }
    let id = c_string(id, "Card");
    let driver = c_string(driver, "SpaceKernel");
    let shortname = c_string(shortname, &id);
    let longname = c_string(longname, &shortname);
    match Card::new(index, &id, &driver, &shortname, &longname) {
        Ok(card) => {
            let handle = Box::into_raw(Box::new(CardHandle { card }));
            // SAFETY: the caller supplied a writable out-parameter.
            unsafe { *card_out = handle as *mut snd_card };
            0
        }
        Err(error) => -(error.to_errno() as c_int),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_card_register(card: *mut snd_card) -> c_int {
    let Some(card) = card_from(card) else {
        return -22;
    };
    match card.register() {
        Ok(()) => 0,
        Err(error) => -(error.to_errno() as c_int),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_card_free(card: *mut snd_card) {
    if card.is_null() {
        return;
    }
    // SAFETY: ownership of the handle returns to the kernel exactly once.
    let handle = unsafe { Box::from_raw(card as *mut CardHandle) };
    handle.card.unregister();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_new(
    card: *mut snd_card,
    id: *const c_char,
    device: c_int,
    playback_count: c_int,
    capture_count: c_int,
    pcm_out: *mut *mut snd_pcm,
) -> c_int {
    let Some(card) = card_from(card) else {
        return -22;
    };
    if pcm_out.is_null() || device < 0 {
        return -22;
    }
    let id = c_string(id, "PCM");
    let pcm = Pcm::new(
        card.number,
        device as u32,
        &id,
        &id,
        playback_count > 0,
        capture_count > 0,
        uapi::SNDRV_PCM_INFO_MMAP
            | uapi::SNDRV_PCM_INFO_MMAP_VALID
            | uapi::SNDRV_PCM_INFO_INTERLEAVED
            | uapi::SNDRV_PCM_INFO_BLOCK_TRANSFER
            | uapi::SNDRV_PCM_INFO_PAUSE,
    );
    match card.add_pcm(pcm.clone()) {
        Ok(()) => {
            let handle = Box::into_raw(Box::new(PcmHandle { pcm }));
            // SAFETY: the caller supplied a writable out-parameter.
            unsafe { *pcm_out = handle as *mut snd_pcm };
            0
        }
        Err(error) => -(error.to_errno() as c_int),
    }
}

pub type PcmOpenFn = Option<unsafe extern "C" fn(*mut snd_pcm_substream) -> c_int>;
pub type PcmHwParamsFn =
    Option<unsafe extern "C" fn(*mut snd_pcm_substream, *mut uapi::snd_pcm_hw_params) -> c_int>;
pub type PcmTriggerFn = Option<unsafe extern "C" fn(*mut snd_pcm_substream, c_int) -> c_int>;
pub type PcmPointerFn = Option<unsafe extern "C" fn(*mut snd_pcm_substream) -> u64>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_ops {
    pub open: PcmOpenFn,
    pub close: PcmOpenFn,
    pub ioctl: Option<unsafe extern "C" fn(*mut snd_pcm_substream, u32, *mut c_void) -> c_int>,
    pub hw_params: PcmHwParamsFn,
    pub hw_free: PcmOpenFn,
    pub prepare: PcmOpenFn,
    pub trigger: PcmTriggerFn,
    pub pointer: PcmPointerFn,
}

struct CPcmOps {
    table: snd_pcm_ops,
}

fn opaque_substream(substream: &Substream) -> *mut snd_pcm_substream {
    substream as *const Substream as *mut snd_pcm_substream
}

impl PcmOps for CPcmOps {
    fn open(&self, substream: &Substream) -> Result<(), i32> {
        match self.table.open {
            // SAFETY: callback pointers come from a loaded module and the
            // substream pointer is the stable Arc allocation.
            Some(callback) => match unsafe { callback(opaque_substream(substream)) } {
                0 => Ok(()),
                error => Err(error),
            },
            None => Ok(()),
        }
    }
    fn close(&self, substream: &Substream) {
        if let Some(callback) = self.table.close {
            // SAFETY: as above.
            let _ = unsafe { callback(opaque_substream(substream)) };
        }
    }
    fn hw_params(
        &self,
        substream: &Substream,
        params: &uapi::snd_pcm_hw_params,
    ) -> Result<(), i32> {
        match self.table.hw_params {
            // SAFETY: the params pointer is valid for the duration of the call.
            Some(callback) => {
                match unsafe { callback(opaque_substream(substream), params as *const _ as *mut _) }
                {
                    0 => Ok(()),
                    error => Err(error),
                }
            }
            None => Ok(()),
        }
    }
    fn hw_free(&self, substream: &Substream) {
        if let Some(callback) = self.table.hw_free {
            // SAFETY: as above.
            let _ = unsafe { callback(opaque_substream(substream)) };
        }
    }
    fn prepare(&self, substream: &Substream) {
        if let Some(callback) = self.table.prepare {
            // SAFETY: as above.
            let _ = unsafe { callback(opaque_substream(substream)) };
        }
    }
    fn trigger(&self, substream: &Substream, cmd: i32) -> Result<(), i32> {
        match self.table.trigger {
            Some(callback) => match unsafe { callback(opaque_substream(substream), cmd) } {
                0 => Ok(()),
                error => Err(error),
            },
            None => Ok(()),
        }
    }
    fn pointer(&self, substream: &Substream) -> u64 {
        match self.table.pointer {
            // SAFETY: as above.
            Some(callback) => unsafe { callback(opaque_substream(substream)) },
            None => 0,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_set_ops(
    pcm: *mut snd_pcm,
    stream: c_int,
    ops: *const snd_pcm_ops,
) -> c_int {
    let Some(pcm) = pcm_from(pcm) else {
        return -22;
    };
    if ops.is_null() {
        return -22;
    }
    let direction = match stream {
        uapi::SNDRV_PCM_STREAM_PLAYBACK => Direction::Playback,
        uapi::SNDRV_PCM_STREAM_CAPTURE => Direction::Capture,
        _ => return -22,
    };
    let Some(substream) = pcm.substream(direction) else {
        return -19;
    };
    // SAFETY: the module guarantees a readable ops table for the call.
    let table = unsafe { *ops };
    substream.set_ops(Arc::new(CPcmOps { table }));
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_period_elapsed(substream: *mut snd_pcm_substream) {
    if let Some(substream) = substream_from(substream) {
        pcm::period_elapsed(substream);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_stop(substream: *mut snd_pcm_substream, state: c_int) -> c_int {
    let Some(substream) = substream_from(substream) else {
        return -22;
    };
    if state < uapi::SNDRV_PCM_STATE_OPEN || state > uapi::SNDRV_PCM_STATE_DISCONNECTED {
        return -22;
    }
    substream.stop(state);
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_stop_xrun(substream: *mut snd_pcm_substream) -> c_int {
    // SAFETY: forwarded to the checked path.
    unsafe { snd_pcm_stop(substream, uapi::SNDRV_PCM_STATE_XRUN) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_hw_constraint_minmax(
    substream: *mut snd_pcm_substream,
    param: u32,
    min: u32,
    max: u32,
) -> c_int {
    let Some(substream) = substream_from(substream) else {
        return -22;
    };
    let mut caps = substream.caps.lock();
    match param {
        uapi::SNDRV_PCM_HW_PARAM_RATE => {
            caps.rate_min = min;
            caps.rate_max = max;
        }
        uapi::SNDRV_PCM_HW_PARAM_CHANNELS => {
            caps.channels_min = min;
            caps.channels_max = max;
        }
        uapi::SNDRV_PCM_HW_PARAM_PERIODS => {
            caps.periods_min = min;
            caps.periods_max = max;
        }
        uapi::SNDRV_PCM_HW_PARAM_BUFFER_BYTES => {
            caps.buffer_bytes_max = max as usize;
        }
        _ => return -22,
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_hw_constraint_mask64(
    substream: *mut snd_pcm_substream,
    param: u32,
    mask: u64,
) -> c_int {
    let Some(substream) = substream_from(substream) else {
        return -22;
    };
    if param != uapi::SNDRV_PCM_HW_PARAM_FORMAT {
        return -22;
    }
    substream.caps.lock().formats &= mask;
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_set_formats(
    substream: *mut snd_pcm_substream,
    formats: u64,
) -> c_int {
    let Some(substream) = substream_from(substream) else {
        return -22;
    };
    substream.caps.lock().formats = formats;
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_lib_malloc_pages(
    substream: *mut snd_pcm_substream,
    _size: usize,
) -> c_int {
    match substream_from(substream) {
        Some(substream) if substream.buffer_pointer().is_some() => 1,
        _ => -22,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_lib_free_pages(substream: *mut snd_pcm_substream) -> c_int {
    match substream_from(substream) {
        Some(_) => 0,
        None => -22,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_get_dma_addr(substream: *mut snd_pcm_substream) -> u64 {
    substream_from(substream).map_or(0, Substream::dma_address)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_get_buffer(substream: *mut snd_pcm_substream) -> *mut c_void {
    substream_from(substream)
        .and_then(Substream::buffer_pointer)
        .map_or(ptr::null_mut(), |pointer| pointer as *mut c_void)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_get_period_size(substream: *mut snd_pcm_substream) -> u64 {
    substream_from(substream).map_or(0, Substream::period_size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_get_buffer_size(substream: *mut snd_pcm_substream) -> u64 {
    substream_from(substream).map_or(0, Substream::buffer_size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_get_rate(substream: *mut snd_pcm_substream) -> u32 {
    substream_from(substream).map_or(0, Substream::rate)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_get_channels(substream: *mut snd_pcm_substream) -> u32 {
    substream_from(substream).map_or(0, Substream::channels)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_pcm_get_format(substream: *mut snd_pcm_substream) -> u32 {
    substream_from(substream).map_or(0, Substream::format)
}

/// Kernel-side layout seen by `snd_kcontrol` callbacks.  The first fields
/// mirror the public header so `kcontrol->private_data` and `kcontrol->id`
/// resolve correctly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_kcontrol {
    pub private_data: *mut c_void,
    pub id: uapi::snd_ctl_elem_id,
    pub access: u32,
    pub count: u32,
}

type KControlInfoFn =
    Option<unsafe extern "C" fn(*mut snd_kcontrol, *mut uapi::snd_ctl_elem_info) -> c_int>;
type KControlValueFn =
    Option<unsafe extern "C" fn(*mut snd_kcontrol, *mut uapi::snd_ctl_elem_value) -> c_int>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_kcontrol_new {
    pub iface: i32,
    pub device: u32,
    pub subdevice: u32,
    pub index: u32,
    pub name: *const c_char,
    pub access: u32,
    pub count: c_int,
    pub info: KControlInfoFn,
    pub get: KControlValueFn,
    pub put: KControlValueFn,
    pub private_data: *mut c_void,
}

struct CKControl {
    raw: snd_kcontrol,
    info: KControlInfoFn,
    get: KControlValueFn,
    put: KControlValueFn,
}

unsafe impl Send for CKControl {}
unsafe impl Sync for CKControl {}

impl ControlOps for CKControl {
    fn info(&self, info: &mut uapi::snd_ctl_elem_info) -> Result<(), crate::vfs::VfsError> {
        match self.info {
            // SAFETY: module callback with kernel-owned structures.
            Some(callback) => match unsafe {
                callback(&self.raw as *const _ as *mut snd_kcontrol, info as *mut _)
            } {
                0 => Ok(()),
                _ => Err(crate::vfs::VfsError::IoError),
            },
            None => Ok(()),
        }
    }
    fn get(&self, value: &mut uapi::snd_ctl_elem_value) -> Result<(), crate::vfs::VfsError> {
        match self.get {
            // SAFETY: module callback with kernel-owned structures.
            Some(callback) => match unsafe {
                callback(&self.raw as *const _ as *mut snd_kcontrol, value as *mut _)
            } {
                0 => Ok(()),
                _ => Err(crate::vfs::VfsError::IoError),
            },
            None => Err(crate::vfs::VfsError::NotSupported),
        }
    }
    fn put(&self, value: &uapi::snd_ctl_elem_value) -> Result<bool, crate::vfs::VfsError> {
        match self.put {
            // SAFETY: module callback with kernel-owned structures.
            Some(callback) => match unsafe {
                callback(
                    &self.raw as *const _ as *mut snd_kcontrol,
                    value as *const _ as *mut _,
                )
            } {
                changed if changed > 0 => Ok(true),
                0 => Ok(false),
                _ => Err(crate::vfs::VfsError::IoError),
            },
            None => Err(crate::vfs::VfsError::NotSupported),
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_ctl_new1(
    template: *const snd_kcontrol_new,
    private_data: *mut c_void,
) -> *mut snd_kcontrol {
    if template.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: the caller guarantees a readable template.
    let template = unsafe { *template };
    let private = if private_data.is_null() {
        template.private_data
    } else {
        private_data
    };
    let name = c_string(template.name, "");
    let mut id = uapi::snd_ctl_elem_id::zeroed();
    id.iface = template
        .iface
        .clamp(0, uapi::SNDRV_CTL_ELEM_IFACE_SEQUENCER as i32) as u32;
    id.device = template.device;
    id.subdevice = template.subdevice;
    id.index = template.index;
    sound::set_string(&mut id.name, &name);
    let control = CKControl {
        raw: snd_kcontrol {
            private_data: private,
            id,
            access: template.access,
            count: template.count.max(1) as u32,
        },
        info: template.info,
        get: template.get,
        put: template.put,
    };
    Box::into_raw(Box::new(control)) as *mut snd_kcontrol
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_ctl_add(card: *mut snd_card, kcontrol: *mut snd_kcontrol) -> c_int {
    let Some(card) = card_from(card) else {
        return -22;
    };
    if kcontrol.is_null() {
        return -22;
    }
    // SAFETY: the handle came from `snd_ctl_new1`; ownership transfers here.
    let ckcontrol = unsafe { Box::from_raw(kcontrol as *mut CKControl) };
    let raw = ckcontrol.raw;
    let control = ControlElem::with_ops(
        raw.id.iface,
        raw.id.device,
        raw.id.subdevice,
        &String::from_utf8_lossy(
            &raw.id.name[..raw.id.name.iter().position(|b| *b == 0).unwrap_or(44)],
        ),
        raw.id.index,
        raw.access,
        Arc::new(*ckcontrol),
    );
    match card.add_control(control) {
        Ok(()) => {
            let id = {
                let elements = card.controls();
                elements
                    .last()
                    .map(|element| element.id())
                    .unwrap_or(raw.id)
            };
            card.notify(uapi::SNDRV_CTL_EVENT_MASK_ADD, &id);
            0
        }
        Err(error) => -(error.to_errno() as c_int),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_ctl_notify(
    card: *mut snd_card,
    mask: u32,
    id: *const uapi::snd_ctl_elem_id,
) {
    let Some(card) = card_from(card) else {
        return;
    };
    if id.is_null() {
        return;
    }
    // SAFETY: caller supplies a readable id.
    card.notify(mask, unsafe { &*id });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn snd_ctl_free_one(kcontrol: *mut snd_kcontrol) {
    if kcontrol.is_null() {
        return;
    }
    // SAFETY: the handle came from `snd_ctl_new1` and was not added.
    drop(unsafe { Box::from_raw(kcontrol as *mut CKControl) });
}
