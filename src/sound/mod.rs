/*
 *
 *       src/sound/mod.rs
 *       ALSA-compatible sound subsystem root
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux ALSA compatible sound core.
//!
//! The subsystem mirrors the kernel-facing ABI defined by
//! `include/uapi/sound/asound.h`: `/dev/snd` character devices under major 116,
//! the raw ioctl command words, the byte layouts of the parameter, status,
//! control and timer structures, and the `/proc/asound` enumeration files.
//! Userspace therefore talks to unmodified alsa-lib or tinyalsa binaries.
//!
//! Layering:
//!
//! * [`uapi`] is the ABI mirror and holds no policy.
//! * [`card`] owns card numbering, device-node publication and `/proc/asound`.
//! * [`pcm`] implements the PCM runtime: constraint solving, the ring buffer,
//!   mmap pages, the stream state machine, period wakeups and xrun handling.
//! * [`control`] implements the mixer element list, TLV and event queues.
//! * [`timer`] implements the global `/dev/snd/timer` device.
//! * [`module_api`] exports the Linux-shaped `snd_*` C ABI to `.ko` drivers.
//!
//! Hardware and loadable drivers implement [`pcm::PcmOps`] (Rust) or fill a
//! `snd_pcm_ops` table (C) and call `snd_pcm_period_elapsed` from their period
//! interrupt.  The core owns allocation, locking and userspace semantics.

pub mod card;
pub mod control;
#[cfg(feature = "sound-dummy")]
pub mod dummy;
pub mod hwdep;
pub mod mixer;
pub mod module_api;
pub mod pcm;
pub mod procfs;
pub mod rawmidi;
#[cfg(all(feature = "boot-self-test", feature = "sound-dummy"))]
pub mod self_test;
pub mod timer;
pub mod uapi;

use alloc::sync::Arc;
use core::ffi::c_int;

use crate::vfs::VfsError;

pub const SUBSYSTEM: &str = "sound";

/// Map a driver's negative errno to the VFS error the syscall layer returns.
pub fn from_errno(code: c_int) -> VfsError {
    match -code {
        1 => VfsError::NotPermitted,
        2 => VfsError::NotFound,
        4 => VfsError::Interrupted,
        5 => VfsError::IoError,
        6 => VfsError::NoDevice,
        9 => VfsError::BadFileDescriptor,
        11 => VfsError::WouldBlock,
        12 => VfsError::NoMemory,
        13 => VfsError::PermissionDenied,
        16 => VfsError::Busy,
        19 => VfsError::NoDevice,
        22 => VfsError::InvalidArgument,
        28 => VfsError::NoSpace,
        32 => VfsError::BrokenPipe,
        38 => VfsError::NotSupported,
        88 => VfsError::NotASocket,
        95 => VfsError::NotSupported,
        110 => VfsError::TimedOut,
        _ => VfsError::IoError,
    }
}

/// Copy a NUL-terminated string into a fixed-size UAPI field.
pub fn set_string(destination: &mut [u8], text: &str) {
    destination.fill(0);
    let length = text.len().min(destination.len().saturating_sub(1));
    destination[..length].copy_from_slice(&text.as_bytes()[..length]);
}

/// ISA DMA ring buffers preallocated for legacy cards (one playback and one
/// capture stream, as Linux `sb16` preallocates per substream at probe).
#[cfg(target_arch = "x86_64")]
const ISA_DMA_PREALLOC_BLOCKS: usize = 2;

pub fn init() {
    #[cfg(target_arch = "x86_64")]
    {
        let held = crate::mm::dma::reserve_isa_blocks(ISA_DMA_PREALLOC_BLOCKS);
        if held < ISA_DMA_PREALLOC_BLOCKS {
            crate::kwarn!("sound: only {held} ISA DMA buffer(s) reserved below 16 MiB");
        }
    }
    timer::init();
    #[cfg(feature = "sound-dummy")]
    dummy::init();
}

pub fn cards() -> alloc::vec::Vec<Arc<card::Card>> {
    card::cards()
}

pub fn find_card(number: u32) -> Option<Arc<card::Card>> {
    card::find(number)
}
