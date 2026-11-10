/*
 *
 *       src/sound/uapi.rs
 *       Linux ALSA UAPI mirror: ioctl numbers, structures and versions
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Byte-exact mirror of `include/uapi/sound/asound.h` for the interfaces the
//! kernel implements.  Every type here is `repr(C)` and every command number is
//! computed with the Linux `_IOC` encoding, because alsa-lib compares the raw
//! 32-bit request words and passes these structures across the syscall
//! boundary.  Sizes and the fields that matter are checked at compile time.
//!
//! Padding is spelled out explicitly where the C compiler would insert it, so
//! every value can be copied with `uaccess::read_value`/`write_value` without
//! leaking uninitialised bytes.

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use core::mem::size_of;

// ---- ioctl encoding ---------------------------------------------------------

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;
const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << IOC_DIRSHIFT)
        | ((ty as u32) << IOC_TYPESHIFT)
        | ((nr as u32) << IOC_NRSHIFT)
        | ((size as u32) << IOC_SIZESHIFT)
}

pub const fn io(ty: u8, nr: u8) -> u32 {
    ioc(IOC_NONE, ty, nr, 0)
}
pub const fn ior<T>(ty: u8, nr: u8) -> u32 {
    ioc(IOC_READ, ty, nr, size_of::<T>())
}
pub const fn iow<T>(ty: u8, nr: u8) -> u32 {
    ioc(IOC_WRITE, ty, nr, size_of::<T>())
}
pub const fn iowr<T>(ty: u8, nr: u8) -> u32 {
    ioc(IOC_READ | IOC_WRITE, ty, nr, size_of::<T>())
}

pub const fn protocol_version(major: u32, minor: u32, subminor: u32) -> u32 {
    (major << 16) | (minor << 8) | subminor
}

pub const SNDRV_PCM_VERSION: u32 = protocol_version(2, 0, 18);
pub const SNDRV_CTL_VERSION: u32 = protocol_version(2, 0, 10);
pub const SNDRV_TIMER_VERSION: u32 = protocol_version(2, 0, 8);
pub const SNDRV_RAWMIDI_VERSION: u32 = protocol_version(2, 0, 5);
pub const SNDRV_HWDEP_VERSION: u32 = protocol_version(1, 0, 1);

/// ALSA character device major.
pub const SNDRV_MAJOR: u64 = 116;
pub const SNDRV_CARDS: u32 = 32;
pub const SNDRV_DEVICE_TYPE_CONTROL: u32 = 0;
pub const SNDRV_DEVICE_TYPE_SEQUENCER: u32 = 1;
pub const SNDRV_DEVICE_TYPE_TIMER: u32 = 2;
pub const SNDRV_DEVICE_TYPE_HWDEP: u32 = 3;
pub const SNDRV_DEVICE_TYPE_RAWMIDI: u32 = 4;
pub const SNDRV_DEVICE_TYPE_PCM_PLAYBACK: u32 = 5;
pub const SNDRV_DEVICE_TYPE_PCM_CAPTURE: u32 = 6;
pub const SNDRV_DEVICE_TYPE_JACK: u32 = 7;

/// Fixed global minors, as in `include/sound/minors.h`.
pub const SNDRV_MINOR_SEQUENCER: u64 = 1;
pub const SNDRV_MINOR_TIMER: u64 = 33;

// ---- PCM --------------------------------------------------------------------

pub type pcm_uframes_t = u64;
pub type pcm_sframes_t = i64;

pub const SNDRV_PCM_STREAM_PLAYBACK: i32 = 0;
pub const SNDRV_PCM_STREAM_CAPTURE: i32 = 1;

pub const SNDRV_PCM_CLASS_GENERIC: i32 = 0;
pub const SNDRV_PCM_SUBCLASS_GENERIC_MIX: i32 = 0;

// Access types.
pub const SNDRV_PCM_ACCESS_MMAP_INTERLEAVED: u32 = 0;
pub const SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED: u32 = 1;
pub const SNDRV_PCM_ACCESS_MMAP_COMPLEX: u32 = 2;
pub const SNDRV_PCM_ACCESS_RW_INTERLEAVED: u32 = 3;
pub const SNDRV_PCM_ACCESS_RW_NONINTERLEAVED: u32 = 4;

// Formats.
pub const SNDRV_PCM_FORMAT_S8: u32 = 0;
pub const SNDRV_PCM_FORMAT_U8: u32 = 1;
pub const SNDRV_PCM_FORMAT_S16_LE: u32 = 2;
pub const SNDRV_PCM_FORMAT_S16_BE: u32 = 3;
pub const SNDRV_PCM_FORMAT_U16_LE: u32 = 4;
pub const SNDRV_PCM_FORMAT_U16_BE: u32 = 5;
pub const SNDRV_PCM_FORMAT_S24_LE: u32 = 6;
pub const SNDRV_PCM_FORMAT_S24_BE: u32 = 7;
pub const SNDRV_PCM_FORMAT_U24_LE: u32 = 8;
pub const SNDRV_PCM_FORMAT_U24_BE: u32 = 9;
pub const SNDRV_PCM_FORMAT_S32_LE: u32 = 10;
pub const SNDRV_PCM_FORMAT_S32_BE: u32 = 11;
pub const SNDRV_PCM_FORMAT_U32_LE: u32 = 12;
pub const SNDRV_PCM_FORMAT_U32_BE: u32 = 13;
pub const SNDRV_PCM_FORMAT_FLOAT_LE: u32 = 14;
pub const SNDRV_PCM_FORMAT_FLOAT_BE: u32 = 15;
pub const SNDRV_PCM_FORMAT_FLOAT64_LE: u32 = 16;
pub const SNDRV_PCM_FORMAT_FLOAT64_BE: u32 = 17;
pub const SNDRV_PCM_FORMAT_IEC958_SUBFRAME_LE: u32 = 18;
pub const SNDRV_PCM_FORMAT_IEC958_SUBFRAME_BE: u32 = 19;
pub const SNDRV_PCM_FORMAT_MU_LAW: u32 = 20;
pub const SNDRV_PCM_FORMAT_A_LAW: u32 = 21;
pub const SNDRV_PCM_FORMAT_IMA_ADPCM: u32 = 22;
pub const SNDRV_PCM_FORMAT_MPEG: u32 = 23;
pub const SNDRV_PCM_FORMAT_GSM: u32 = 24;
pub const SNDRV_PCM_FORMAT_S20_LE: u32 = 25;
pub const SNDRV_PCM_FORMAT_S20_BE: u32 = 26;
pub const SNDRV_PCM_FORMAT_U20_LE: u32 = 27;
pub const SNDRV_PCM_FORMAT_U20_BE: u32 = 28;
pub const SNDRV_PCM_FORMAT_SPECIAL: u32 = 31;
pub const SNDRV_PCM_FORMAT_S24_3LE: u32 = 32;
pub const SNDRV_PCM_FORMAT_S24_3BE: u32 = 33;
pub const SNDRV_PCM_FORMAT_U24_3LE: u32 = 34;
pub const SNDRV_PCM_FORMAT_U24_3BE: u32 = 35;
pub const SNDRV_PCM_FORMAT_S20_3LE: u32 = 36;
pub const SNDRV_PCM_FORMAT_S20_3BE: u32 = 37;
pub const SNDRV_PCM_FORMAT_U20_3LE: u32 = 38;
pub const SNDRV_PCM_FORMAT_U20_3BE: u32 = 39;
pub const SNDRV_PCM_FORMAT_S18_3LE: u32 = 40;
pub const SNDRV_PCM_FORMAT_S18_3BE: u32 = 41;
pub const SNDRV_PCM_FORMAT_U18_3LE: u32 = 42;
pub const SNDRV_PCM_FORMAT_U18_3BE: u32 = 43;
pub const SNDRV_PCM_FORMAT_DSD_U8: u32 = 48;
pub const SNDRV_PCM_FORMAT_DSD_U16_LE: u32 = 49;
pub const SNDRV_PCM_FORMAT_DSD_U32_LE: u32 = 50;
pub const SNDRV_PCM_FORMAT_DSD_U16_BE: u32 = 51;
pub const SNDRV_PCM_FORMAT_DSD_U32_BE: u32 = 52;
pub const SNDRV_PCM_FORMAT_LAST: u32 = SNDRV_PCM_FORMAT_DSD_U32_BE;

pub const fn format_bit(format: u32) -> u64 {
    1u64 << (format & 63)
}

/// Frame width in bits for the linear formats the core implements.  Returns
/// `None` for formats a software ring buffer cannot describe.
pub const fn format_bits(format: u32) -> Option<u32> {
    match format {
        SNDRV_PCM_FORMAT_S8 | SNDRV_PCM_FORMAT_U8 => Some(8),
        SNDRV_PCM_FORMAT_S16_LE
        | SNDRV_PCM_FORMAT_S16_BE
        | SNDRV_PCM_FORMAT_U16_LE
        | SNDRV_PCM_FORMAT_U16_BE => Some(16),
        SNDRV_PCM_FORMAT_S24_LE
        | SNDRV_PCM_FORMAT_S24_BE
        | SNDRV_PCM_FORMAT_U24_LE
        | SNDRV_PCM_FORMAT_U24_BE
        | SNDRV_PCM_FORMAT_S20_LE
        | SNDRV_PCM_FORMAT_S20_BE
        | SNDRV_PCM_FORMAT_U20_LE
        | SNDRV_PCM_FORMAT_U20_BE
        | SNDRV_PCM_FORMAT_S18_3LE
        | SNDRV_PCM_FORMAT_S18_3BE
        | SNDRV_PCM_FORMAT_U18_3LE
        | SNDRV_PCM_FORMAT_U18_3BE => Some(24),
        SNDRV_PCM_FORMAT_S32_LE
        | SNDRV_PCM_FORMAT_S32_BE
        | SNDRV_PCM_FORMAT_U32_LE
        | SNDRV_PCM_FORMAT_U32_BE
        | SNDRV_PCM_FORMAT_FLOAT_LE
        | SNDRV_PCM_FORMAT_FLOAT_BE => Some(32),
        SNDRV_PCM_FORMAT_FLOAT64_LE | SNDRV_PCM_FORMAT_FLOAT64_BE => Some(64),
        _ => None,
    }
}

/// Stored width in bytes, which differs from [`format_bits`] for the
/// LSB-justified 24-bit formats carried in four-byte containers.
pub const fn format_bytes(format: u32) -> Option<usize> {
    match format {
        SNDRV_PCM_FORMAT_S8 | SNDRV_PCM_FORMAT_U8 => Some(1),
        SNDRV_PCM_FORMAT_S16_LE
        | SNDRV_PCM_FORMAT_S16_BE
        | SNDRV_PCM_FORMAT_U16_LE
        | SNDRV_PCM_FORMAT_U16_BE => Some(2),
        SNDRV_PCM_FORMAT_S24_3LE
        | SNDRV_PCM_FORMAT_S24_3BE
        | SNDRV_PCM_FORMAT_U24_3LE
        | SNDRV_PCM_FORMAT_U24_3BE
        | SNDRV_PCM_FORMAT_S20_3LE
        | SNDRV_PCM_FORMAT_S20_3BE
        | SNDRV_PCM_FORMAT_U20_3LE
        | SNDRV_PCM_FORMAT_U20_3BE
        | SNDRV_PCM_FORMAT_S18_3LE
        | SNDRV_PCM_FORMAT_S18_3BE
        | SNDRV_PCM_FORMAT_U18_3LE
        | SNDRV_PCM_FORMAT_U18_3BE => Some(3),
        SNDRV_PCM_FORMAT_S24_LE
        | SNDRV_PCM_FORMAT_S24_BE
        | SNDRV_PCM_FORMAT_U24_LE
        | SNDRV_PCM_FORMAT_U24_BE
        | SNDRV_PCM_FORMAT_S20_LE
        | SNDRV_PCM_FORMAT_S20_BE
        | SNDRV_PCM_FORMAT_U20_LE
        | SNDRV_PCM_FORMAT_U20_BE
        | SNDRV_PCM_FORMAT_S32_LE
        | SNDRV_PCM_FORMAT_S32_BE
        | SNDRV_PCM_FORMAT_U32_LE
        | SNDRV_PCM_FORMAT_U32_BE
        | SNDRV_PCM_FORMAT_FLOAT_LE
        | SNDRV_PCM_FORMAT_FLOAT_BE => Some(4),
        SNDRV_PCM_FORMAT_FLOAT64_LE | SNDRV_PCM_FORMAT_FLOAT64_BE => Some(8),
        _ => None,
    }
}

pub const SNDRV_PCM_SUBFORMAT_STD: u32 = 0;

// Runtime info flags.
pub const SNDRV_PCM_INFO_MMAP: u32 = 0x0000_0001;
pub const SNDRV_PCM_INFO_MMAP_VALID: u32 = 0x0000_0002;
pub const SNDRV_PCM_INFO_DOUBLE: u32 = 0x0000_0004;
pub const SNDRV_PCM_INFO_BATCH: u32 = 0x0000_0010;
pub const SNDRV_PCM_INFO_SYNC_APPLPTR: u32 = 0x0000_0020;
pub const SNDRV_PCM_INFO_PERFECT_DRAIN: u32 = 0x0000_0040;
pub const SNDRV_PCM_INFO_INTERLEAVED: u32 = 0x0000_0100;
pub const SNDRV_PCM_INFO_NONINTERLEAVED: u32 = 0x0000_0200;
pub const SNDRV_PCM_INFO_COMPLEX: u32 = 0x0000_0400;
pub const SNDRV_PCM_INFO_BLOCK_TRANSFER: u32 = 0x0001_0000;
pub const SNDRV_PCM_INFO_OVERRANGE: u32 = 0x0002_0000;
pub const SNDRV_PCM_INFO_RESUME: u32 = 0x0004_0000;
pub const SNDRV_PCM_INFO_PAUSE: u32 = 0x0008_0000;
pub const SNDRV_PCM_INFO_HALF_DUPLEX: u32 = 0x0010_0000;
pub const SNDRV_PCM_INFO_JOINT_DUPLEX: u32 = 0x0020_0000;
pub const SNDRV_PCM_INFO_SYNC_START: u32 = 0x0040_0000;
pub const SNDRV_PCM_INFO_NO_PERIOD_WAKEUP: u32 = 0x0080_0000;

// State.
pub const SNDRV_PCM_STATE_OPEN: i32 = 0;
pub const SNDRV_PCM_STATE_SETUP: i32 = 1;
pub const SNDRV_PCM_STATE_PREPARED: i32 = 2;
pub const SNDRV_PCM_STATE_RUNNING: i32 = 3;
pub const SNDRV_PCM_STATE_XRUN: i32 = 4;
pub const SNDRV_PCM_STATE_DRAINING: i32 = 5;
pub const SNDRV_PCM_STATE_PAUSED: i32 = 6;
pub const SNDRV_PCM_STATE_SUSPENDED: i32 = 7;
pub const SNDRV_PCM_STATE_DISCONNECTED: i32 = 8;

pub const SNDRV_PCM_MMAP_OFFSET_DATA: u64 = 0x0000_0000;
pub const SNDRV_PCM_MMAP_OFFSET_STATUS_OLD: u64 = 0x8000_0000;
pub const SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD: u64 = 0x8100_0000;
pub const SNDRV_PCM_MMAP_OFFSET_STATUS_NEW: u64 = 0x8200_0000;
pub const SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW: u64 = 0x8300_0000;
pub const SNDRV_PCM_MMAP_OFFSET_STATUS: u64 = SNDRV_PCM_MMAP_OFFSET_STATUS_OLD;
pub const SNDRV_PCM_MMAP_OFFSET_CONTROL: u64 = SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD;

// Hardware parameter indices.
pub const SNDRV_PCM_HW_PARAM_ACCESS: u32 = 0;
pub const SNDRV_PCM_HW_PARAM_FORMAT: u32 = 1;
pub const SNDRV_PCM_HW_PARAM_SUBFORMAT: u32 = 2;
pub const SNDRV_PCM_HW_PARAM_FIRST_MASK: u32 = 0;
pub const SNDRV_PCM_HW_PARAM_LAST_MASK: u32 = 2;
pub const SNDRV_PCM_HW_PARAM_SAMPLE_BITS: u32 = 8;
pub const SNDRV_PCM_HW_PARAM_FRAME_BITS: u32 = 9;
pub const SNDRV_PCM_HW_PARAM_CHANNELS: u32 = 10;
pub const SNDRV_PCM_HW_PARAM_RATE: u32 = 11;
pub const SNDRV_PCM_HW_PARAM_PERIOD_TIME: u32 = 12;
pub const SNDRV_PCM_HW_PARAM_PERIOD_SIZE: u32 = 13;
pub const SNDRV_PCM_HW_PARAM_PERIOD_BYTES: u32 = 14;
pub const SNDRV_PCM_HW_PARAM_PERIODS: u32 = 15;
pub const SNDRV_PCM_HW_PARAM_BUFFER_TIME: u32 = 16;
pub const SNDRV_PCM_HW_PARAM_BUFFER_SIZE: u32 = 17;
pub const SNDRV_PCM_HW_PARAM_BUFFER_BYTES: u32 = 18;
pub const SNDRV_PCM_HW_PARAM_TICK_TIME: u32 = 19;
pub const SNDRV_PCM_HW_PARAM_FIRST_INTERVAL: u32 = 8;
pub const SNDRV_PCM_HW_PARAM_LAST_INTERVAL: u32 = 19;

pub const SNDRV_PCM_HW_PARAMS_NORESAMPLE: u32 = 1 << 0;
pub const SNDRV_PCM_HW_PARAMS_EXPORT_BUFFER: u32 = 1 << 1;
pub const SNDRV_PCM_HW_PARAMS_NO_PERIOD_WAKEUP: u32 = 1 << 2;
pub const SNDRV_PCM_HW_PARAMS_NO_DRAIN_SILENCE: u32 = 1 << 3;

pub const SNDRV_MASK_MAX: usize = 256;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_mask {
    pub bits: [u32; (SNDRV_MASK_MAX + 31) / 32],
}

impl snd_mask {
    pub fn zeroed() -> Self {
        Self { bits: [0; 8] }
    }
    pub fn any() -> Self {
        Self {
            bits: [u32::MAX; 8],
        }
    }
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|word| *word == 0)
    }
    pub fn test(&self, bit: u32) -> bool {
        let index = bit as usize;
        if index >= SNDRV_MASK_MAX {
            return false;
        }
        self.bits[index / 32] & (1 << (index % 32)) != 0
    }
    pub fn set(&mut self, bit: u32) {
        let index = bit as usize;
        if index < SNDRV_MASK_MAX {
            self.bits[index / 32] |= 1 << (index % 32);
        }
    }
    pub fn clear(&mut self, bit: u32) {
        let index = bit as usize;
        if index < SNDRV_MASK_MAX {
            self.bits[index / 32] &= !(1 << (index % 32));
        }
    }
    pub fn intersect(&mut self, other: &Self) {
        for (left, right) in self.bits.iter_mut().zip(other.bits.iter()) {
            *left &= *right;
        }
    }
    pub fn union_with(&mut self, other: &Self) {
        for (left, right) in self.bits.iter_mut().zip(other.bits.iter()) {
            *left |= *right;
        }
    }
    pub fn first(&self) -> Option<u32> {
        for (word_index, word) in self.bits.iter().enumerate() {
            if *word != 0 {
                return Some((word_index as u32) * 32 + word.trailing_zeros());
            }
        }
        None
    }
    pub fn count(&self) -> u32 {
        self.bits.iter().map(|word| word.count_ones()).sum()
    }
    pub fn single(&self) -> Option<u32> {
        if self.count() == 1 {
            self.first()
        } else {
            None
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_interval {
    pub min: u32,
    pub max: u32,
    /// openmin:1, openmax:1, integer:1, empty:1 in little-endian bit order.
    pub flags: u32,
}

pub const INTERVAL_OPENMIN: u32 = 1 << 0;
pub const INTERVAL_OPENMAX: u32 = 1 << 1;
pub const INTERVAL_INTEGER: u32 = 1 << 2;
pub const INTERVAL_EMPTY: u32 = 1 << 3;

impl snd_interval {
    pub const fn empty() -> Self {
        Self {
            min: 0,
            max: u32::MAX,
            flags: INTERVAL_EMPTY | INTERVAL_INTEGER,
        }
    }
    pub const fn any() -> Self {
        Self {
            min: 0,
            max: u32::MAX,
            flags: INTERVAL_INTEGER,
        }
    }
    pub const fn exact(value: u32) -> Self {
        Self {
            min: value,
            max: value,
            flags: INTERVAL_INTEGER,
        }
    }
    pub const fn range(min: u32, max: u32) -> Self {
        Self {
            min,
            max,
            flags: INTERVAL_INTEGER,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.flags & INTERVAL_EMPTY != 0 || self.min > self.max
    }
    pub fn is_exact(&self) -> bool {
        !self.is_empty() && self.min == self.max
    }
    pub fn exact_value(&self) -> Option<u32> {
        if self.is_exact() {
            Some(self.min)
        } else {
            None
        }
    }
    pub fn contains(&self, value: u32) -> bool {
        !self.is_empty()
            && value >= self.min
            && value <= self.max
            && !(self.flags & INTERVAL_OPENMIN != 0 && value == self.min)
            && !(self.flags & INTERVAL_OPENMAX != 0 && value == self.max)
    }
    pub fn intersect(&mut self, other: &Self) {
        if other.flags & INTERVAL_EMPTY != 0 {
            self.flags |= INTERVAL_EMPTY;
            return;
        }
        if self.min < other.min {
            self.min = other.min;
            self.flags &= !INTERVAL_OPENMIN;
            self.flags |= other.flags & INTERVAL_OPENMIN;
        } else if self.min == other.min {
            self.flags |= other.flags & INTERVAL_OPENMIN;
        }
        if self.max > other.max {
            self.max = other.max;
            self.flags &= !INTERVAL_OPENMAX;
            self.flags |= other.flags & INTERVAL_OPENMAX;
        } else if self.max == other.max {
            self.flags |= other.flags & INTERVAL_OPENMAX;
        }
        if self.min > self.max {
            self.flags |= INTERVAL_EMPTY;
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_hw_params {
    pub flags: u32,
    pub masks: [snd_mask; 3],
    pub mres: [snd_mask; 5],
    pub intervals: [snd_interval; 12],
    pub ires: [snd_interval; 9],
    pub rmask: u32,
    pub cmask: u32,
    pub info: u32,
    pub msbits: u32,
    pub rate_num: u32,
    pub rate_den: u32,
    pub fifo_size: pcm_uframes_t,
    pub sync: [u8; 16],
    pub reserved: [u8; 48],
}

impl snd_pcm_hw_params {
    pub fn zeroed() -> Self {
        Self {
            flags: 0,
            masks: [snd_mask::zeroed(); 3],
            mres: [snd_mask::zeroed(); 5],
            intervals: [snd_interval::empty(); 12],
            ires: [snd_interval::empty(); 9],
            rmask: 0,
            cmask: 0,
            info: 0,
            msbits: 0,
            rate_num: 0,
            rate_den: 0,
            fifo_size: 0,
            sync: [0; 16],
            reserved: [0; 48],
        }
    }
    pub fn mask(&self, param: u32) -> &snd_mask {
        &self.masks[param as usize]
    }
    pub fn mask_mut(&mut self, param: u32) -> &mut snd_mask {
        &mut self.masks[param as usize]
    }
    pub fn interval(&self, param: u32) -> &snd_interval {
        &self.intervals[(param - SNDRV_PCM_HW_PARAM_FIRST_INTERVAL) as usize]
    }
    pub fn interval_mut(&mut self, param: u32) -> &mut snd_interval {
        &mut self.intervals[(param - SNDRV_PCM_HW_PARAM_FIRST_INTERVAL) as usize]
    }
    fn mark(&mut self, param: u32) {
        self.cmask |= 1 << param;
    }
    pub fn set_mask(&mut self, param: u32, mask: snd_mask) {
        *self.mask_mut(param) = mask;
        self.mark(param);
    }
    pub fn set_interval(&mut self, param: u32, interval: snd_interval) {
        *self.interval_mut(param) = interval;
        self.mark(param);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_sw_params {
    pub tstamp_mode: i32,
    pub period_step: u32,
    pub sleep_min: u32,
    pub _pad0: u32,
    pub avail_min: pcm_uframes_t,
    pub xfer_align: pcm_uframes_t,
    pub start_threshold: pcm_uframes_t,
    pub stop_threshold: pcm_uframes_t,
    pub silence_threshold: pcm_uframes_t,
    pub silence_size: pcm_uframes_t,
    pub boundary: pcm_uframes_t,
    pub proto: u32,
    pub tstamp_type: u32,
    pub reserved: [u8; 56],
}

impl snd_pcm_sw_params {
    pub fn zeroed() -> Self {
        Self {
            tstamp_mode: 0,
            period_step: 1,
            sleep_min: 0,
            _pad0: 0,
            avail_min: 0,
            xfer_align: 0,
            start_threshold: 0,
            stop_threshold: 0,
            silence_threshold: 0,
            silence_size: 0,
            boundary: 0,
            proto: 0,
            tstamp_type: 0,
            reserved: [0; 56],
        }
    }
}

pub const SNDRV_PCM_TSTAMP_NONE: i32 = 0;
pub const SNDRV_PCM_TSTAMP_ENABLE: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_channel_info {
    pub channel: u32,
    pub _pad0: u32,
    pub offset: i64,
    pub first: u32,
    pub step: u32,
}

impl snd_pcm_channel_info {
    pub fn zeroed() -> Self {
        Self {
            channel: 0,
            _pad0: 0,
            offset: 0,
            first: 0,
            step: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

impl timespec {
    pub const fn zero() -> Self {
        Self {
            tv_sec: 0,
            tv_nsec: 0,
        }
    }
    pub fn from_micros(micros: u64) -> Self {
        Self {
            tv_sec: (micros / 1_000_000) as i64,
            tv_nsec: ((micros % 1_000_000) * 1_000) as i64,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_status {
    pub state: i32,
    pub pad1: [u8; 4],
    pub trigger_tstamp: timespec,
    pub tstamp: timespec,
    pub appl_ptr: pcm_uframes_t,
    pub hw_ptr: pcm_uframes_t,
    pub delay: pcm_sframes_t,
    pub avail: pcm_uframes_t,
    pub avail_max: pcm_uframes_t,
    pub overrange: pcm_uframes_t,
    pub suspended_state: i32,
    pub audio_tstamp_data: u32,
    pub audio_tstamp: timespec,
    pub driver_tstamp: timespec,
    pub audio_tstamp_accuracy: u32,
    pub reserved: [u8; 20],
}

impl snd_pcm_status {
    pub fn zeroed() -> Self {
        Self {
            state: 0,
            pad1: [0; 4],
            trigger_tstamp: timespec::zero(),
            tstamp: timespec::zero(),
            appl_ptr: 0,
            hw_ptr: 0,
            delay: 0,
            avail: 0,
            avail_max: 0,
            overrange: 0,
            suspended_state: 0,
            audio_tstamp_data: 0,
            audio_tstamp: timespec::zero(),
            driver_tstamp: timespec::zero(),
            audio_tstamp_accuracy: 0,
            reserved: [0; 20],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_mmap_status {
    pub state: i32,
    pub pad1: u32,
    pub hw_ptr: pcm_uframes_t,
    pub tstamp: timespec,
    pub suspended_state: i32,
    pub pad2: u32,
    pub audio_tstamp: timespec,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_mmap_control {
    pub appl_ptr: pcm_uframes_t,
    pub avail_min: pcm_uframes_t,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union snd_pcm_sync_status {
    pub status: snd_pcm_mmap_status,
    pub reserved: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union snd_pcm_sync_control {
    pub control: snd_pcm_mmap_control,
    pub reserved: [u8; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_sync_ptr {
    pub flags: u32,
    pub pad0: u32,
    pub s: snd_pcm_sync_status,
    pub c: snd_pcm_sync_control,
}

pub const SNDRV_PCM_SYNC_PTR_HWSYNC: u32 = 1 << 0;
pub const SNDRV_PCM_SYNC_PTR_APPL: u32 = 1 << 1;
pub const SNDRV_PCM_SYNC_PTR_AVAIL_MIN: u32 = 1 << 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_xferi {
    pub result: pcm_sframes_t,
    pub buf: u64,
    pub frames: pcm_uframes_t,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_xfern {
    pub result: pcm_sframes_t,
    pub bufs: u64,
    pub frames: pcm_uframes_t,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_pcm_info {
    pub device: u32,
    pub subdevice: u32,
    pub stream: i32,
    pub card: i32,
    pub id: [u8; 64],
    pub name: [u8; 80],
    pub subname: [u8; 32],
    pub dev_class: i32,
    pub dev_subclass: i32,
    pub subdevices_count: u32,
    pub subdevices_avail: u32,
    pub pad1: [u8; 16],
    pub reserved: [u8; 64],
}

impl snd_pcm_info {
    pub fn zeroed() -> Self {
        Self {
            device: 0,
            subdevice: 0,
            stream: 0,
            card: 0,
            id: [0; 64],
            name: [0; 80],
            subname: [0; 32],
            dev_class: 0,
            dev_subclass: 0,
            subdevices_count: 0,
            subdevices_avail: 0,
            pad1: [0; 16],
            reserved: [0; 64],
        }
    }
}

// PCM ioctls.
pub const SNDRV_PCM_IOCTL_PVERSION: u32 = ior::<i32>(b'A', 0x00);
pub const SNDRV_PCM_IOCTL_INFO: u32 = ior::<snd_pcm_info>(b'A', 0x01);
pub const SNDRV_PCM_IOCTL_TSTAMP: u32 = iow::<i32>(b'A', 0x02);
pub const SNDRV_PCM_IOCTL_TTSTAMP: u32 = iow::<i32>(b'A', 0x03);
pub const SNDRV_PCM_IOCTL_USER_PVERSION: u32 = iow::<i32>(b'A', 0x04);
pub const SNDRV_PCM_IOCTL_HW_REFINE: u32 = iowr::<snd_pcm_hw_params>(b'A', 0x10);
pub const SNDRV_PCM_IOCTL_HW_PARAMS: u32 = iowr::<snd_pcm_hw_params>(b'A', 0x11);
pub const SNDRV_PCM_IOCTL_HW_FREE: u32 = io(b'A', 0x12);
pub const SNDRV_PCM_IOCTL_SW_PARAMS: u32 = iowr::<snd_pcm_sw_params>(b'A', 0x13);
pub const SNDRV_PCM_IOCTL_STATUS: u32 = ior::<snd_pcm_status>(b'A', 0x20);
pub const SNDRV_PCM_IOCTL_DELAY: u32 = ior::<pcm_sframes_t>(b'A', 0x21);
pub const SNDRV_PCM_IOCTL_HWSYNC: u32 = io(b'A', 0x22);
pub const SNDRV_PCM_IOCTL_SYNC_PTR: u32 = iowr::<snd_pcm_sync_ptr>(b'A', 0x23);
pub const SNDRV_PCM_IOCTL_STATUS_EXT: u32 = iowr::<snd_pcm_status>(b'A', 0x24);
pub const SNDRV_PCM_IOCTL_CHANNEL_INFO: u32 = ior::<snd_pcm_channel_info>(b'A', 0x32);
pub const SNDRV_PCM_IOCTL_PREPARE: u32 = io(b'A', 0x40);
pub const SNDRV_PCM_IOCTL_RESET: u32 = io(b'A', 0x41);
pub const SNDRV_PCM_IOCTL_START: u32 = io(b'A', 0x42);
pub const SNDRV_PCM_IOCTL_DROP: u32 = io(b'A', 0x43);
pub const SNDRV_PCM_IOCTL_DRAIN: u32 = io(b'A', 0x44);
pub const SNDRV_PCM_IOCTL_PAUSE: u32 = iow::<i32>(b'A', 0x45);
pub const SNDRV_PCM_IOCTL_REWIND: u32 = iow::<pcm_uframes_t>(b'A', 0x46);
pub const SNDRV_PCM_IOCTL_RESUME: u32 = io(b'A', 0x47);
pub const SNDRV_PCM_IOCTL_XRUN: u32 = io(b'A', 0x48);
pub const SNDRV_PCM_IOCTL_FORWARD: u32 = iow::<pcm_uframes_t>(b'A', 0x49);
pub const SNDRV_PCM_IOCTL_WRITEI_FRAMES: u32 = iow::<snd_xferi>(b'A', 0x50);
pub const SNDRV_PCM_IOCTL_READI_FRAMES: u32 = ior::<snd_xferi>(b'A', 0x51);
pub const SNDRV_PCM_IOCTL_WRITEN_FRAMES: u32 = iow::<snd_xfern>(b'A', 0x52);
pub const SNDRV_PCM_IOCTL_READN_FRAMES: u32 = ior::<snd_xfern>(b'A', 0x53);
pub const SNDRV_PCM_IOCTL_LINK: u32 = iow::<i32>(b'A', 0x60);
pub const SNDRV_PCM_IOCTL_UNLINK: u32 = io(b'A', 0x61);

// Trigger commands, current mainline numbering.
pub const SNDRV_PCM_TRIGGER_STOP: i32 = 0;
pub const SNDRV_PCM_TRIGGER_START: i32 = 1;
pub const SNDRV_PCM_TRIGGER_PAUSE_PUSH: i32 = 2;
pub const SNDRV_PCM_TRIGGER_PAUSE_RELEASE: i32 = 3;
pub const SNDRV_PCM_TRIGGER_SUSPEND: i32 = 4;
pub const SNDRV_PCM_TRIGGER_RESUME: i32 = 5;
pub const SNDRV_PCM_TRIGGER_DRAIN: i32 = 6;

// ---- Control ----------------------------------------------------------------

pub const SNDRV_CTL_ELEM_ID_NAME_MAXLEN: usize = 44;

pub const SNDRV_CTL_ELEM_TYPE_NONE: u32 = 0;
pub const SNDRV_CTL_ELEM_TYPE_BOOLEAN: u32 = 1;
pub const SNDRV_CTL_ELEM_TYPE_INTEGER: u32 = 2;
pub const SNDRV_CTL_ELEM_TYPE_ENUMERATED: u32 = 3;
pub const SNDRV_CTL_ELEM_TYPE_BYTES: u32 = 4;
pub const SNDRV_CTL_ELEM_TYPE_IEC958: u32 = 5;
pub const SNDRV_CTL_ELEM_TYPE_INTEGER64: u32 = 6;

pub const SNDRV_CTL_ELEM_IFACE_CARD: u32 = 0;
pub const SNDRV_CTL_ELEM_IFACE_HWDEP: u32 = 1;
pub const SNDRV_CTL_ELEM_IFACE_MIXER: u32 = 2;
pub const SNDRV_CTL_ELEM_IFACE_PCM: u32 = 3;
pub const SNDRV_CTL_ELEM_IFACE_RAWMIDI: u32 = 4;
pub const SNDRV_CTL_ELEM_IFACE_TIMER: u32 = 5;
pub const SNDRV_CTL_ELEM_IFACE_SEQUENCER: u32 = 6;

pub const SNDRV_CTL_ELEM_ACCESS_READ: u32 = 1 << 0;
pub const SNDRV_CTL_ELEM_ACCESS_WRITE: u32 = 1 << 1;
pub const SNDRV_CTL_ELEM_ACCESS_READWRITE: u32 =
    SNDRV_CTL_ELEM_ACCESS_READ | SNDRV_CTL_ELEM_ACCESS_WRITE;
pub const SNDRV_CTL_ELEM_ACCESS_VOLATILE: u32 = 1 << 2;
pub const SNDRV_CTL_ELEM_ACCESS_TLV_READ: u32 = 1 << 4;
pub const SNDRV_CTL_ELEM_ACCESS_TLV_WRITE: u32 = 1 << 5;
pub const SNDRV_CTL_ELEM_ACCESS_TLV_READWRITE: u32 =
    SNDRV_CTL_ELEM_ACCESS_TLV_READ | SNDRV_CTL_ELEM_ACCESS_TLV_WRITE;
pub const SNDRV_CTL_ELEM_ACCESS_TLV_COMMAND: u32 = 1 << 6;
pub const SNDRV_CTL_ELEM_ACCESS_INACTIVE: u32 = 1 << 8;
pub const SNDRV_CTL_ELEM_ACCESS_LOCK: u32 = 1 << 9;
pub const SNDRV_CTL_ELEM_ACCESS_OWNER: u32 = 1 << 10;
pub const SNDRV_CTL_ELEM_ACCESS_USER: u32 = 1 << 29;

pub const SNDRV_CTL_POWER_D0: u32 = 0x0000;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_elem_id {
    pub numid: u32,
    pub iface: u32,
    pub device: u32,
    pub subdevice: u32,
    pub name: [u8; SNDRV_CTL_ELEM_ID_NAME_MAXLEN],
    pub index: u32,
}

impl snd_ctl_elem_id {
    pub fn zeroed() -> Self {
        Self {
            numid: 0,
            iface: 0,
            device: 0,
            subdevice: 0,
            name: [0; SNDRV_CTL_ELEM_ID_NAME_MAXLEN],
            index: 0,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.numid == 0 && self.name[0] == 0
    }
    pub fn name_str(&self) -> &[u8] {
        let length = self
            .name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.name.len());
        &self.name[..length]
    }
    /// Identity without the numeric id, as used to reject duplicate elements.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.iface == other.iface
            && self.device == other.device
            && self.subdevice == other.subdevice
            && self.name == other.name
            && self.index == other.index
    }
    /// An element matches a lookup id when `numid` is set, otherwise by the
    /// remaining identity fields. This is Linux's `snd_ctl_find_id` rule.
    pub fn matches(&self, filter: &Self) -> bool {
        if filter.numid != 0 {
            return self.numid == filter.numid;
        }
        self.same_identity(filter)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_elem_list {
    pub offset: u32,
    pub space: u32,
    pub used: u32,
    pub count: u32,
    pub pids: u64,
    pub reserved: [u8; 50],
    pub _pad: [u8; 6],
}

impl snd_ctl_elem_list {
    pub fn zeroed() -> Self {
        Self {
            offset: 0,
            space: 0,
            used: 0,
            count: 0,
            pids: 0,
            reserved: [0; 50],
            _pad: [0; 6],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_elem_info_integer {
    pub min: i64,
    pub max: i64,
    pub step: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_elem_info_integer64 {
    pub min: i64,
    pub max: i64,
    pub step: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_elem_info_enumerated {
    pub items: u32,
    pub item: u32,
    pub name: [u8; 64],
    pub names_ptr: u64,
    pub names_length: u32,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union snd_ctl_elem_info_value {
    pub integer: snd_ctl_elem_info_integer,
    pub integer64: snd_ctl_elem_info_integer64,
    pub enumerated: snd_ctl_elem_info_enumerated,
    pub reserved: [u8; 128],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_elem_info {
    pub id: snd_ctl_elem_id,
    pub kind: u32,
    pub access: u32,
    pub count: u32,
    pub owner: i32,
    pub value: snd_ctl_elem_info_value,
    pub reserved: [u8; 64],
}

impl snd_ctl_elem_info {
    pub fn zeroed() -> Self {
        Self {
            id: snd_ctl_elem_id::zeroed(),
            kind: 0,
            access: 0,
            count: 0,
            owner: 0,
            value: snd_ctl_elem_info_value { reserved: [0; 128] },
            reserved: [0; 64],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_aes_iec958 {
    pub status: [u8; 24],
    pub subcode: [u8; 147],
    pub pad: u8,
    pub dig_subframe: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union snd_ctl_elem_value_data {
    pub integer: [i64; 128],
    pub integer64: [i64; 64],
    pub enumerated: [u32; 128],
    pub bytes: [u8; 512],
    pub iec958: snd_aes_iec958,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_elem_value {
    pub id: snd_ctl_elem_id,
    pub indirect: u32,
    pub _pad0: u32,
    pub value: snd_ctl_elem_value_data,
    pub reserved: [u8; 128],
}

impl snd_ctl_elem_value {
    pub fn zeroed() -> Self {
        Self {
            id: snd_ctl_elem_id::zeroed(),
            indirect: 0,
            _pad0: 0,
            value: snd_ctl_elem_value_data { bytes: [0; 512] },
            reserved: [0; 128],
        }
    }
    pub fn integer(&self) -> [i64; 128] {
        // SAFETY: every union member is plain data; reading the widest one is
        // always in bounds and all-zero is a valid value.
        unsafe { self.value.integer }
    }
    pub fn set_integer(&mut self, values: &[i64]) {
        // SAFETY: the union is plain data and `integer` is the largest member.
        unsafe {
            self.value.integer = [0; 128];
            for (slot, value) in self.value.integer.iter_mut().zip(values.iter()) {
                *slot = *value;
            }
        }
    }
    pub fn enum_items(&self) -> [u32; 128] {
        unsafe { self.value.enumerated }
    }
    pub fn set_enum_items(&mut self, values: &[u32]) {
        unsafe {
            self.value.enumerated = [0; 128];
            for (slot, value) in self.value.enumerated.iter_mut().zip(values.iter()) {
                *slot = *value;
            }
        }
    }
    pub fn bytes(&self) -> [u8; 512] {
        unsafe { self.value.bytes }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_card_info {
    pub card: i32,
    pub pad: i32,
    pub id: [u8; 16],
    pub driver: [u8; 16],
    pub name: [u8; 32],
    pub longname: [u8; 80],
    pub reserved_: [u8; 16],
    pub mixername: [u8; 80],
    pub components: [u8; 128],
}

impl snd_ctl_card_info {
    pub fn zeroed() -> Self {
        Self {
            card: 0,
            pad: 0,
            id: [0; 16],
            driver: [0; 16],
            name: [0; 32],
            longname: [0; 80],
            reserved_: [0; 16],
            mixername: [0; 80],
            components: [0; 128],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_card_bytes {
    pub kind: u32,
    pub data_allocated: u32,
    pub data_len: u32,
    pub reserved: u32,
    pub data: u64,
}

pub const SND_CTL_CARD_BTYPE_COMPONENTS: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_tlv {
    pub numid: u32,
    pub length: u32,
}

pub const SNDRV_CTL_IOCTL_PVERSION: u32 = ior::<i32>(b'U', 0x00);
pub const SNDRV_CTL_IOCTL_CARD_INFO: u32 = ior::<snd_ctl_card_info>(b'U', 0x01);
pub const SNDRV_CTL_IOCTL_CARD_BYTES: u32 = iowr::<snd_ctl_card_bytes>(b'U', 0x02);
pub const SNDRV_CTL_IOCTL_ELEM_LIST: u32 = iowr::<snd_ctl_elem_list>(b'U', 0x10);
pub const SNDRV_CTL_IOCTL_ELEM_INFO: u32 = iowr::<snd_ctl_elem_info>(b'U', 0x11);
pub const SNDRV_CTL_IOCTL_ELEM_READ: u32 = iowr::<snd_ctl_elem_value>(b'U', 0x12);
pub const SNDRV_CTL_IOCTL_ELEM_WRITE: u32 = iowr::<snd_ctl_elem_value>(b'U', 0x13);
pub const SNDRV_CTL_IOCTL_ELEM_LOCK: u32 = iow::<snd_ctl_elem_id>(b'U', 0x14);
pub const SNDRV_CTL_IOCTL_ELEM_UNLOCK: u32 = iow::<snd_ctl_elem_id>(b'U', 0x15);
pub const SNDRV_CTL_IOCTL_SUBSCRIBE_EVENTS: u32 = iowr::<i32>(b'U', 0x16);
pub const SNDRV_CTL_IOCTL_ELEM_ADD: u32 = iowr::<snd_ctl_elem_info>(b'U', 0x17);
pub const SNDRV_CTL_IOCTL_ELEM_REPLACE: u32 = iowr::<snd_ctl_elem_info>(b'U', 0x18);
pub const SNDRV_CTL_IOCTL_ELEM_REMOVE: u32 = iow::<snd_ctl_elem_id>(b'U', 0x19);
pub const SNDRV_CTL_IOCTL_TLV_READ: u32 = iowr::<snd_ctl_tlv>(b'U', 0x1a);
pub const SNDRV_CTL_IOCTL_TLV_WRITE: u32 = iowr::<snd_ctl_tlv>(b'U', 0x1b);
pub const SNDRV_CTL_IOCTL_TLV_COMMAND: u32 = iowr::<snd_ctl_tlv>(b'U', 0x1c);
pub const SNDRV_CTL_IOCTL_HWDEP_NEXT_DEVICE: u32 = iowr::<i32>(b'U', 0x20);
pub const SNDRV_CTL_IOCTL_HWDEP_INFO: u32 = ior::<u8>(b'U', 0x21);
pub const SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE: u32 = ior::<i32>(b'U', 0x30);
pub const SNDRV_CTL_IOCTL_PCM_INFO: u32 = iowr::<snd_pcm_info>(b'U', 0x31);
pub const SNDRV_CTL_IOCTL_PCM_PREFER_SUBDEVICE: u32 = iow::<i32>(b'U', 0x32);
pub const SNDRV_CTL_IOCTL_RAWMIDI_NEXT_DEVICE: u32 = iowr::<i32>(b'U', 0x40);
pub const SNDRV_CTL_IOCTL_RAWMIDI_INFO: u32 = iowr::<u8>(b'U', 0x41);
pub const SNDRV_CTL_IOCTL_RAWMIDI_PREFER_SUBDEVICE: u32 = iow::<i32>(b'U', 0x42);
pub const SNDRV_CTL_IOCTL_POWER: u32 = iowr::<i32>(b'U', 0xd0);
pub const SNDRV_CTL_IOCTL_POWER_STATE: u32 = ior::<i32>(b'U', 0xd1);

pub const SNDRV_CTL_EVENT_ELEM: i32 = 0;
pub const SNDRV_CTL_EVENT_MASK_VALUE: u32 = 1 << 0;
pub const SNDRV_CTL_EVENT_MASK_INFO: u32 = 1 << 1;
pub const SNDRV_CTL_EVENT_MASK_ADD: u32 = 1 << 2;
pub const SNDRV_CTL_EVENT_MASK_TLV: u32 = 1 << 3;
pub const SNDRV_CTL_EVENT_MASK_REMOVE: u32 = !0;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_event_elem {
    pub mask: u32,
    pub id: snd_ctl_elem_id,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union snd_ctl_event_data {
    pub elem: snd_ctl_event_elem,
    pub data8: [u8; 60],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_ctl_event {
    pub kind: i32,
    pub data: snd_ctl_event_data,
}

impl snd_ctl_event {
    pub fn zeroed() -> Self {
        Self {
            kind: 0,
            data: snd_ctl_event_data { data8: [0; 60] },
        }
    }
}

// ---- Timer ------------------------------------------------------------------

pub const SNDRV_TIMER_CLASS_NONE: i32 = -1;
pub const SNDRV_TIMER_CLASS_SLAVE: i32 = 0;
pub const SNDRV_TIMER_CLASS_GLOBAL: i32 = 1;
pub const SNDRV_TIMER_CLASS_CARD: i32 = 2;
pub const SNDRV_TIMER_CLASS_PCM: i32 = 3;

pub const SNDRV_TIMER_SCLASS_NONE: i32 = 0;
pub const SNDRV_TIMER_SCLASS_APPLICATION: i32 = 1;
pub const SNDRV_TIMER_SCLASS_SEQUENCER: i32 = 2;
pub const SNDRV_TIMER_SCLASS_OSS_SEQUENCER: i32 = 3;

pub const SNDRV_TIMER_GLOBAL_SYSTEM: i32 = 0;
pub const SNDRV_TIMER_GLOBAL_RTC: i32 = 1;
pub const SNDRV_TIMER_GLOBAL_HPET: i32 = 2;
pub const SNDRV_TIMER_GLOBAL_HRTIMER: i32 = 3;
pub const SNDRV_TIMER_GLOBAL_UDRIVEN: i32 = 4;

pub const SNDRV_TIMER_FLG_SLAVE: u32 = 1 << 0;

pub const SNDRV_TIMER_PSFLG_AUTO: u32 = 1 << 0;
pub const SNDRV_TIMER_PSFLG_EXCLUSIVE: u32 = 1 << 1;
pub const SNDRV_TIMER_PSFLG_EARLY_EVENT: u32 = 1 << 2;

pub const SNDRV_TIMER_EVENT_RESOLUTION: i32 = 0;
pub const SNDRV_TIMER_EVENT_TICK: i32 = 1;
pub const SNDRV_TIMER_EVENT_START: i32 = 2;
pub const SNDRV_TIMER_EVENT_STOP: i32 = 3;
pub const SNDRV_TIMER_EVENT_CONTINUE: i32 = 4;
pub const SNDRV_TIMER_EVENT_PAUSE: i32 = 5;
pub const SNDRV_TIMER_EVENT_EARLY: i32 = 6;
pub const SNDRV_TIMER_EVENT_SUSPEND: i32 = 7;
pub const SNDRV_TIMER_EVENT_RESUME: i32 = 8;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_id {
    pub dev_class: i32,
    pub dev_sclass: i32,
    pub card: i32,
    pub device: i32,
    pub subdevice: i32,
}

impl snd_timer_id {
    pub fn zeroed() -> Self {
        Self {
            dev_class: SNDRV_TIMER_CLASS_NONE,
            dev_sclass: SNDRV_TIMER_SCLASS_NONE,
            card: -1,
            device: -1,
            subdevice: -1,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_ginfo {
    pub tid: snd_timer_id,
    pub flags: u32,
    pub card: i32,
    pub id: [u8; 64],
    pub name: [u8; 80],
    pub _pad0: [u8; 4],
    pub reserved0: u64,
    pub resolution: u64,
    pub resolution_min: u64,
    pub resolution_max: u64,
    pub clients: u32,
    pub reserved: [u8; 32],
}

impl snd_timer_ginfo {
    pub fn zeroed() -> Self {
        Self {
            tid: snd_timer_id::zeroed(),
            flags: 0,
            card: -1,
            id: [0; 64],
            name: [0; 80],
            _pad0: [0; 4],
            reserved0: 0,
            resolution: 0,
            resolution_min: 0,
            resolution_max: 0,
            clients: 0,
            reserved: [0; 32],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_gparams {
    pub tid: snd_timer_id,
    pub _pad0: [u8; 4],
    pub period_num: u64,
    pub period_den: u64,
    pub reserved: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_gstatus {
    pub tid: snd_timer_id,
    pub _pad0: [u8; 4],
    pub resolution: u64,
    pub resolution_num: u64,
    pub resolution_den: u64,
    pub reserved: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_select {
    pub id: snd_timer_id,
    pub reserved: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_info {
    pub flags: u32,
    pub card: i32,
    pub id: [u8; 64],
    pub name: [u8; 80],
    pub reserved0: u64,
    pub resolution: u64,
    pub reserved: [u8; 64],
}

impl snd_timer_info {
    pub fn zeroed() -> Self {
        Self {
            flags: 0,
            card: -1,
            id: [0; 64],
            name: [0; 80],
            reserved0: 0,
            resolution: 0,
            reserved: [0; 64],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_params {
    pub flags: u32,
    pub ticks: u32,
    pub queue_size: u32,
    pub reserved0: u32,
    pub filter: u32,
    pub reserved: [u8; 60],
}

impl snd_timer_params {
    pub fn zeroed() -> Self {
        Self {
            flags: 0,
            ticks: 0,
            queue_size: 0,
            reserved0: 0,
            filter: 0,
            reserved: [0; 60],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_status {
    pub tstamp: timespec,
    pub resolution: u32,
    pub lost: u32,
    pub overrun: u32,
    pub queue: u32,
    pub reserved: [u8; 64],
}

impl snd_timer_status {
    pub fn zeroed() -> Self {
        Self {
            tstamp: timespec::zero(),
            resolution: 0,
            lost: 0,
            overrun: 0,
            queue: 0,
            reserved: [0; 64],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_read {
    pub resolution: u32,
    pub ticks: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_tread {
    pub event: i32,
    pub pad1: [u8; 4],
    pub tstamp: timespec,
    pub val: u32,
    pub pad2: [u8; 4],
}

pub const SNDRV_TIMER_IOCTL_PVERSION: u32 = ior::<i32>(b'T', 0x00);
pub const SNDRV_TIMER_IOCTL_NEXT_DEVICE: u32 = iowr::<snd_timer_id>(b'T', 0x01);
pub const SNDRV_TIMER_IOCTL_TREAD_OLD: u32 = iow::<i32>(b'T', 0x02);
pub const SNDRV_TIMER_IOCTL_GINFO: u32 = iowr::<snd_timer_ginfo>(b'T', 0x03);
pub const SNDRV_TIMER_IOCTL_GPARAMS: u32 = iow::<snd_timer_gparams>(b'T', 0x04);
pub const SNDRV_TIMER_IOCTL_GSTATUS: u32 = iowr::<snd_timer_gstatus>(b'T', 0x05);
pub const SNDRV_TIMER_IOCTL_SELECT: u32 = iow::<snd_timer_select>(b'T', 0x10);
pub const SNDRV_TIMER_IOCTL_INFO: u32 = ior::<snd_timer_info>(b'T', 0x11);
pub const SNDRV_TIMER_IOCTL_PARAMS: u32 = iow::<snd_timer_params>(b'T', 0x12);
pub const SNDRV_TIMER_IOCTL_STATUS: u32 = ior::<snd_timer_status>(b'T', 0x14);
pub const SNDRV_TIMER_IOCTL_START: u32 = io(b'T', 0xa0);
pub const SNDRV_TIMER_IOCTL_STOP: u32 = io(b'T', 0xa1);
pub const SNDRV_TIMER_IOCTL_CONTINUE: u32 = io(b'T', 0xa2);
pub const SNDRV_TIMER_IOCTL_PAUSE: u32 = io(b'T', 0xa3);
pub const SNDRV_TIMER_IOCTL_TREAD64: u32 = iow::<i32>(b'T', 0xa4);
pub const SNDRV_TIMER_IOCTL_TREAD: u32 = SNDRV_TIMER_IOCTL_TREAD_OLD;
pub const SNDRV_TIMER_IOCTL_CREATE: u32 = iowr::<snd_timer_uinfo>(b'T', 0xa5);
pub const SNDRV_TIMER_IOCTL_TRIGGER: u32 = io(b'T', 0xa6);

// ---- audio timestamp --------------------------------------------------------

pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_COMPAT: i32 = 0;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_DEFAULT: i32 = 1;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK: i32 = 2;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK_ABSOLUTE: i32 = 3;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK_ESTIMATED: i32 = 4;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK_SYNCHRONIZED: i32 = 5;

pub const SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY: i32 = 0;
pub const SNDRV_PCM_TSTAMP_TYPE_MONOTONIC: i32 = 1;
pub const SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW: i32 = 2;

// ---- RawMIDI ----------------------------------------------------------------

pub const SNDRV_RAWMIDI_STREAM_OUTPUT: i32 = 0;
pub const SNDRV_RAWMIDI_STREAM_INPUT: i32 = 1;

pub const SNDRV_RAWMIDI_INFO_OUTPUT: u32 = 0x0000_0001;
pub const SNDRV_RAWMIDI_INFO_INPUT: u32 = 0x0000_0002;
pub const SNDRV_RAWMIDI_INFO_DUPLEX: u32 = 0x0000_0004;

pub const SNDRV_RAWMIDI_MODE_FRAMING_MASK: u32 = 7 << 0;
pub const SNDRV_RAWMIDI_MODE_FRAMING_NONE: u32 = 0;
pub const SNDRV_RAWMIDI_MODE_FRAMING_TSTAMP: u32 = 1;
pub const SNDRV_RAWMIDI_MODE_CLOCK_MASK: u32 = 7 << 3;
pub const SNDRV_RAWMIDI_MODE_CLOCK_NONE: u32 = 0;
pub const SNDRV_RAWMIDI_MODE_CLOCK_REALTIME: u32 = 1 << 3;
pub const SNDRV_RAWMIDI_MODE_CLOCK_MONOTONIC: u32 = 2 << 3;
pub const SNDRV_RAWMIDI_MODE_CLOCK_MONOTONIC_RAW: u32 = 3 << 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_rawmidi_info {
    pub device: u32,
    pub subdevice: u32,
    pub stream: i32,
    pub card: i32,
    pub flags: u32,
    pub id: [u8; 64],
    pub name: [u8; 80],
    pub subname: [u8; 32],
    pub subdevices_count: u32,
    pub subdevices_avail: u32,
    pub tied_device: i32,
    pub reserved: [u8; 60],
}

impl snd_rawmidi_info {
    pub fn zeroed() -> Self {
        Self {
            device: 0,
            subdevice: 0,
            stream: 0,
            card: 0,
            flags: 0,
            id: [0; 64],
            name: [0; 80],
            subname: [0; 32],
            subdevices_count: 0,
            subdevices_avail: 0,
            tied_device: 0,
            reserved: [0; 60],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_rawmidi_params {
    pub stream: i32,
    pub _pad0: u32,
    pub buffer_size: usize,
    pub avail_min: usize,
    pub no_active_sensing: u32,
    pub mode: u32,
    pub reserved: [u8; 12],
    pub _pad1: [u8; 4],
}

impl snd_rawmidi_params {
    pub fn zeroed() -> Self {
        Self {
            stream: 0,
            _pad0: 0,
            buffer_size: 0,
            avail_min: 0,
            no_active_sensing: 0,
            mode: 0,
            reserved: [0; 12],
            _pad1: [0; 4],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_rawmidi_status {
    pub stream: i32,
    pub pad1: [u8; 4],
    pub tstamp: timespec,
    pub avail: usize,
    pub xruns: usize,
    pub reserved: [u8; 16],
}

impl snd_rawmidi_status {
    pub fn zeroed() -> Self {
        Self {
            stream: 0,
            pad1: [0; 4],
            tstamp: timespec::zero(),
            avail: 0,
            xruns: 0,
            reserved: [0; 16],
        }
    }
}

pub const SNDRV_RAWMIDI_IOCTL_PVERSION: u32 = ior::<i32>(b'W', 0x00);
pub const SNDRV_RAWMIDI_IOCTL_INFO: u32 = ior::<snd_rawmidi_info>(b'W', 0x01);
pub const SNDRV_RAWMIDI_IOCTL_USER_PVERSION: u32 = iow::<i32>(b'W', 0x02);
pub const SNDRV_RAWMIDI_IOCTL_PARAMS: u32 = iowr::<snd_rawmidi_params>(b'W', 0x10);
pub const SNDRV_RAWMIDI_IOCTL_STATUS: u32 = iowr::<snd_rawmidi_status>(b'W', 0x20);
pub const SNDRV_RAWMIDI_IOCTL_DROP: u32 = iow::<i32>(b'W', 0x30);
pub const SNDRV_RAWMIDI_IOCTL_DRAIN: u32 = iow::<i32>(b'W', 0x31);

// ---- hwdep ------------------------------------------------------------------

pub const SNDRV_HWDEP_IFACE_OPL2: i32 = 0;
pub const SNDRV_HWDEP_IFACE_OPL3: i32 = 1;
pub const SNDRV_HWDEP_IFACE_OPL4: i32 = 2;
pub const SNDRV_HWDEP_IFACE_SB16CSP: i32 = 3;
pub const SNDRV_HWDEP_IFACE_EMU10K1: i32 = 4;
pub const SNDRV_HWDEP_IFACE_YSS225: i32 = 5;
pub const SNDRV_HWDEP_IFACE_ICS2115: i32 = 6;
pub const SNDRV_HWDEP_IFACE_SSCAPE: i32 = 7;
pub const SNDRV_HWDEP_IFACE_VX: i32 = 8;
pub const SNDRV_HWDEP_IFACE_MIXART: i32 = 9;
pub const SNDRV_HWDEP_IFACE_USX2Y: i32 = 10;
pub const SNDRV_HWDEP_IFACE_EMUX_WAVETABLE: i32 = 11;
pub const SNDRV_HWDEP_IFACE_BLUETOOTH: i32 = 12;
pub const SNDRV_HWDEP_IFACE_USX2Y_PCM: i32 = 13;
pub const SNDRV_HWDEP_IFACE_PCXHR: i32 = 14;
pub const SNDRV_HWDEP_IFACE_SB_RC: i32 = 15;
pub const SNDRV_HWDEP_IFACE_HDA: i32 = 16;
pub const SNDRV_HWDEP_IFACE_USB_STREAM: i32 = 17;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_hwdep_info {
    pub device: u32,
    pub card: i32,
    pub id: [u8; 64],
    pub name: [u8; 80],
    pub iface: i32,
    pub reserved: [u8; 64],
}

impl snd_hwdep_info {
    pub fn zeroed() -> Self {
        Self {
            device: 0,
            card: 0,
            id: [0; 64],
            name: [0; 80],
            iface: 0,
            reserved: [0; 64],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_hwdep_dsp_status {
    pub version: u32,
    pub id: [u8; 32],
    pub num_dsps: u32,
    pub dsp_loaded: u32,
    pub chip_ready: u32,
    pub reserved: [u8; 16],
}

impl snd_hwdep_dsp_status {
    pub fn zeroed() -> Self {
        Self {
            version: 0,
            id: [0; 32],
            num_dsps: 0,
            dsp_loaded: 0,
            chip_ready: 0,
            reserved: [0; 16],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_hwdep_dsp_image {
    pub index: u32,
    pub name: [u8; 64],
    pub _pad0: [u8; 4],
    pub image: u64,
    pub length: usize,
    pub driver_data: u64,
}

pub const SNDRV_HWDEP_IOCTL_PVERSION: u32 = ior::<i32>(b'H', 0x00);
pub const SNDRV_HWDEP_IOCTL_INFO: u32 = ior::<snd_hwdep_info>(b'H', 0x01);
pub const SNDRV_HWDEP_IOCTL_DSP_STATUS: u32 = ior::<snd_hwdep_dsp_status>(b'H', 0x02);
pub const SNDRV_HWDEP_IOCTL_DSP_LOAD: u32 = iow::<snd_hwdep_dsp_image>(b'H', 0x03);

// ---- userspace-driven timer -------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct snd_timer_uinfo {
    pub resolution: u64,
    pub fd: i32,
    pub id: u32,
    pub reserved: [u8; 16],
}

impl snd_timer_uinfo {
    pub fn zeroed() -> Self {
        Self {
            resolution: 0,
            fd: -1,
            id: 0,
            reserved: [0; 16],
        }
    }
}

// ---- compile-time ABI checks ------------------------------------------------

const _: () = assert!(size_of::<snd_mask>() == 32);
const _: () = assert!(size_of::<snd_interval>() == 12);
const _: () = assert!(size_of::<snd_pcm_hw_params>() == 608);
const _: () = assert!(size_of::<snd_pcm_sw_params>() == 136);
const _: () = assert!(size_of::<snd_pcm_info>() == 288);
const _: () = assert!(size_of::<snd_pcm_status>() == 152);
const _: () = assert!(size_of::<snd_pcm_mmap_status>() == 56);
const _: () = assert!(size_of::<snd_pcm_mmap_control>() == 16);
const _: () = assert!(size_of::<snd_pcm_sync_ptr>() == 136);
const _: () = assert!(size_of::<snd_pcm_channel_info>() == 24);
const _: () = assert!(size_of::<snd_xferi>() == 24);
const _: () = assert!(size_of::<snd_ctl_card_info>() == 376);
const _: () = assert!(size_of::<snd_ctl_card_bytes>() == 24);
const _: () = assert!(size_of::<snd_ctl_elem_id>() == 64);
const _: () = assert!(size_of::<snd_ctl_elem_list>() == 80);
const _: () = assert!(size_of::<snd_ctl_elem_info>() == 272);
const _: () = assert!(size_of::<snd_ctl_elem_value>() == 1224);
const _: () = assert!(size_of::<snd_ctl_event>() == 72);
const _: () = assert!(size_of::<snd_timer_id>() == 20);
const _: () = assert!(size_of::<snd_timer_ginfo>() == 248);
const _: () = assert!(size_of::<snd_timer_gparams>() == 72);
const _: () = assert!(size_of::<snd_timer_gstatus>() == 80);
const _: () = assert!(size_of::<snd_timer_select>() == 52);
const _: () = assert!(size_of::<snd_timer_info>() == 232);
const _: () = assert!(size_of::<snd_timer_params>() == 80);
const _: () = assert!(size_of::<snd_timer_status>() == 96);
const _: () = assert!(size_of::<snd_timer_read>() == 8);
const _: () = assert!(size_of::<snd_timer_tread>() == 32);
const _: () = assert!(size_of::<snd_rawmidi_info>() == 268);
const _: () = assert!(size_of::<snd_rawmidi_params>() == 48);
const _: () = assert!(size_of::<snd_rawmidi_status>() == 56);
const _: () = assert!(size_of::<snd_hwdep_info>() == 220);
const _: () = assert!(size_of::<snd_hwdep_dsp_status>() == 64);
const _: () = assert!(size_of::<snd_hwdep_dsp_image>() == 96);
const _: () = assert!(size_of::<snd_timer_uinfo>() == 32);

const _: () = assert!(core::mem::offset_of!(snd_pcm_hw_params, masks) == 4);
const _: () = assert!(core::mem::offset_of!(snd_pcm_hw_params, intervals) == 260);
const _: () = assert!(core::mem::offset_of!(snd_pcm_hw_params, rmask) == 512);
const _: () = assert!(core::mem::offset_of!(snd_pcm_hw_params, fifo_size) == 536);
const _: () = assert!(core::mem::offset_of!(snd_pcm_sw_params, avail_min) == 16);
const _: () = assert!(core::mem::offset_of!(snd_pcm_sw_params, boundary) == 64);
const _: () = assert!(core::mem::offset_of!(snd_pcm_status, tstamp) == 24);
const _: () = assert!(core::mem::offset_of!(snd_pcm_status, appl_ptr) == 40);
const _: () = assert!(core::mem::offset_of!(snd_ctl_elem_value, value) == 72);
const _: () = assert!(core::mem::offset_of!(snd_ctl_event, data) == 4);
const _: () = assert!(core::mem::offset_of!(snd_timer_ginfo, reserved0) == 176);
const _: () = assert!(core::mem::offset_of!(snd_timer_info, reserved0) == 152);

// The command words themselves are part of the ABI; spot-check the ones
// alsa-lib compares against values generated by the C compiler.
const _: () = assert!(SNDRV_PCM_IOCTL_PVERSION == 0x8004_4100);
const _: () = assert!(SNDRV_PCM_IOCTL_HW_PARAMS == 0xc260_4111);
const _: () = assert!(SNDRV_PCM_IOCTL_SW_PARAMS == 0xc088_4113);
const _: () = assert!(SNDRV_PCM_IOCTL_STATUS == 0x8098_4120);
const _: () = assert!(SNDRV_CTL_IOCTL_ELEM_LIST == 0xc050_5510);
const _: () = assert!(SNDRV_CTL_IOCTL_ELEM_INFO == 0xc110_5511);
const _: () = assert!(SNDRV_CTL_IOCTL_ELEM_READ == 0xc4c8_5512);
const _: () = assert!(SNDRV_TIMER_IOCTL_GINFO == 0xc0f8_5403);
const _: () = assert!(SNDRV_TIMER_IOCTL_PARAMS == 0x4050_5412);
const _: () = assert!(SNDRV_RAWMIDI_IOCTL_INFO == 0x810c_5701);
const _: () = assert!(SNDRV_RAWMIDI_IOCTL_PARAMS == 0xc030_5710);
const _: () = assert!(SNDRV_RAWMIDI_IOCTL_STATUS == 0xc038_5720);
const _: () = assert!(SNDRV_HWDEP_IOCTL_INFO == 0x80dc_4801);
const _: () = assert!(SNDRV_HWDEP_IOCTL_DSP_LOAD == 0x4060_4803);
const _: () = assert!(SNDRV_TIMER_IOCTL_CREATE == 0xc020_54a5);
const _: () = assert!(SNDRV_TIMER_IOCTL_TRIGGER == 0x54a6);

// ---- user-copy markers ------------------------------------------------------
//
// Padding is explicit in these types, so every byte of a value copied from or
// to userspace is initialised.  All bit patterns are valid.

unsafe impl crate::user::uaccess::UserPod for snd_mask {}
unsafe impl crate::user::uaccess::UserPod for snd_interval {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_hw_params {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_sw_params {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_info {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_channel_info {}
unsafe impl crate::user::uaccess::UserPod for timespec {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_status {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_mmap_status {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_mmap_control {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_sync_status {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_sync_control {}
unsafe impl crate::user::uaccess::UserPod for snd_pcm_sync_ptr {}
unsafe impl crate::user::uaccess::UserPod for snd_xferi {}
unsafe impl crate::user::uaccess::UserPod for snd_xfern {}
unsafe impl crate::user::uaccess::UserPod for snd_ctl_elem_id {}
unsafe impl crate::user::uaccess::UserPod for snd_ctl_elem_list {}
unsafe impl crate::user::uaccess::UserPod for snd_ctl_elem_info {}
unsafe impl crate::user::uaccess::UserPod for snd_ctl_elem_value {}
unsafe impl crate::user::uaccess::UserPod for snd_ctl_card_info {}
unsafe impl crate::user::uaccess::UserPod for snd_ctl_card_bytes {}
unsafe impl crate::user::uaccess::UserPod for snd_ctl_tlv {}
unsafe impl crate::user::uaccess::UserPod for snd_ctl_event {}
unsafe impl crate::user::uaccess::UserPod for snd_aes_iec958 {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_id {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_ginfo {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_gparams {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_gstatus {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_select {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_info {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_params {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_status {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_read {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_tread {}
unsafe impl crate::user::uaccess::UserPod for snd_rawmidi_info {}
unsafe impl crate::user::uaccess::UserPod for snd_rawmidi_params {}
unsafe impl crate::user::uaccess::UserPod for snd_rawmidi_status {}
unsafe impl crate::user::uaccess::UserPod for snd_hwdep_info {}
unsafe impl crate::user::uaccess::UserPod for snd_hwdep_dsp_status {}
unsafe impl crate::user::uaccess::UserPod for snd_hwdep_dsp_image {}
unsafe impl crate::user::uaccess::UserPod for snd_timer_uinfo {}
