/*
 *
 *       src/sound/hwdep.rs
 *       ALSA hardware-dependent device interface (/dev/snd/hwC*D*)
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! `snd_hwdep_new()` equivalent.  A driver publishes an opaque interface
//! (`SNDRV_HWDEP_IFACE_*`) and may implement custom ioctls, DSP status and DSP
//! image loading.  The core serves the standard `'H'` ioctls and forwards
//! everything else to the driver.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::{
    sync::Mutex,
    user::uaccess,
    vfs::{Inode, InodeKind, Stat, VfsError, tree::ino},
};

use super::{card::Card, set_string, uapi};

pub trait HwdepOps: Send + Sync {
    fn ioctl(&self, _cmd: u32, _arg: u64) -> Result<u64, VfsError> {
        Err(VfsError::NotATty)
    }
    fn dsp_status(&self, status: &mut uapi::snd_hwdep_dsp_status) -> Result<(), VfsError> {
        let _ = status;
        Err(VfsError::NotSupported)
    }
    fn dsp_load(
        &self,
        _index: u32,
        _name: &[u8],
        _data: &[u8],
        _driver_data: u64,
    ) -> Result<(), VfsError> {
        Err(VfsError::NotSupported)
    }
}

pub struct Hwdep {
    pub card: u32,
    pub device: u32,
    pub id: String,
    pub name: String,
    pub iface: i32,
    ops: Mutex<Option<Arc<dyn HwdepOps>>>,
    dsp_loaded: AtomicU32,
}

impl Hwdep {
    pub fn new(card: u32, device: u32, id: &str, name: &str, iface: i32) -> Arc<Self> {
        Arc::new(Self {
            card,
            device,
            id: String::from(id),
            name: String::from(name),
            iface,
            ops: Mutex::new(None),
            dsp_loaded: AtomicU32::new(0),
        })
    }

    pub fn set_ops(&self, ops: Arc<dyn HwdepOps>) {
        *self.ops.lock() = Some(ops);
    }

    fn ops(&self) -> Option<Arc<dyn HwdepOps>> {
        self.ops.lock().clone()
    }

    pub fn dsp_loaded(&self) -> u32 {
        self.dsp_loaded.load(Ordering::Acquire)
    }

    pub fn info(&self) -> uapi::snd_hwdep_info {
        let mut info = uapi::snd_hwdep_info::zeroed();
        info.device = self.device;
        info.card = self.card as i32;
        set_string(&mut info.id, &self.id);
        set_string(&mut info.name, &self.name);
        info.iface = self.iface;
        info
    }
}

pub struct HwdepDeviceNode {
    hwdep: Arc<Hwdep>,
}

impl HwdepDeviceNode {
    pub fn new(hwdep: Arc<Hwdep>) -> Arc<Self> {
        Arc::new(Self { hwdep })
    }
}

pub struct HwdepSession {
    hwdep: Arc<Hwdep>,
    _nonblock: AtomicBool,
}

impl HwdepSession {
    fn session(hwdep: Arc<Hwdep>, nonblock: bool) -> Arc<Self> {
        Arc::new(Self {
            hwdep,
            _nonblock: AtomicBool::new(nonblock),
        })
    }
}

fn hwdep_ioctl(hwdep: &Hwdep, cmd: u32, arg: u64) -> Result<u64, VfsError> {
    match cmd {
        uapi::SNDRV_HWDEP_IOCTL_PVERSION => {
            uaccess::write_value(arg, &(uapi::SNDRV_HWDEP_VERSION as i32))?;
            Ok(0)
        }
        uapi::SNDRV_HWDEP_IOCTL_INFO => {
            let info = hwdep.info();
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_HWDEP_IOCTL_DSP_STATUS => {
            let ops = hwdep.ops().ok_or(VfsError::NotSupported)?;
            let mut status = uapi::snd_hwdep_dsp_status::zeroed();
            ops.dsp_status(&mut status)?;
            status.dsp_loaded = hwdep.dsp_loaded();
            uaccess::write_value(arg, &status)?;
            Ok(0)
        }
        uapi::SNDRV_HWDEP_IOCTL_DSP_LOAD => {
            let image = uaccess::read_value::<uapi::snd_hwdep_dsp_image>(arg)?;
            if image.index >= 32 {
                return Err(VfsError::InvalidArgument);
            }
            if hwdep.dsp_loaded() & (1 << image.index) != 0 {
                return Err(VfsError::Busy);
            }
            let length = image.length.min(16 * 1024 * 1024);
            let mut data = Vec::new();
            data.resize(length, 0);
            if length != 0 {
                if image.image == 0 {
                    return Err(VfsError::InvalidArgument);
                }
                uaccess::read(image.image, &mut data)?;
            }
            let name_length = image
                .name
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(image.name.len());
            let ops = hwdep.ops().ok_or(VfsError::NotSupported)?;
            ops.dsp_load(
                image.index,
                &image.name[..name_length],
                &data,
                image.driver_data,
            )?;
            hwdep
                .dsp_loaded
                .fetch_or(1 << image.index, Ordering::AcqRel);
            Ok(0)
        }
        _ => match hwdep.ops() {
            Some(ops) => ops.ioctl(cmd, arg),
            None => Err(VfsError::NotATty),
        },
    }
}

impl Inode for HwdepDeviceNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }
    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        let nonblock = flags & crate::vfs::abi::O_NONBLOCK != 0;
        Ok(Some(HwdepSession::session(self.hwdep.clone(), nonblock)))
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        hwdep_ioctl(&self.hwdep, cmd as u32, arg)
    }
    fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
}

impl Inode for HwdepSession {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        hwdep_ioctl(&self.hwdep, cmd as u32, arg)
    }
    fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self._nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }
}

/// Locate a hwdep on a card for the control `HWDEP_*` enumeration ioctls.
pub fn find(card: &Card, device: u32) -> Option<Arc<Hwdep>> {
    card.hwdeps()
        .into_iter()
        .find(|hwdep| hwdep.device == device)
}
