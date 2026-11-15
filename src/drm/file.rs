/*
 *
 *       src/drm/file.rs
 *       /dev/dri/cardN nodes, per-open DRM files and ioctl dispatch
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux `drm_file` semantics: the first opener becomes master; modeset
//! ioctls require master; GEM handles, framebuffers, blobs and events are
//! per file and released on close; when the last file closes (`lastclose`)
//! the display is switched off and given back to the console.

use super::{
    Device, GemObject, gem,
    ioctl::{self, Ioctl},
    kms::{self, Caller},
};
use crate::{
    sched::WaitQueue,
    sync::SpinLock,
    user::UserAddressSpace,
    vfs::{Inode, InodeKind, Stat, VfsError, abi},
};
use alloc::{
    collections::VecDeque,
    format,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

const DRM_MAJOR: u64 = 226;
/// Events a file may have queued before new ones are dropped (Linux limits
/// event space to 4 KiB per file).
const MAX_EVENTS: usize = 128;

struct Node {
    device: Weak<Device>,
}

impl Inode for Node {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let device = self.device.upgrade().ok_or(VfsError::NoDevice)?;
        let mut stat = Stat::simple(0x900000 + device.minor as u64, InodeKind::CharDevice, 0o660, 0);
        stat.rdev = (DRM_MAJOR << 8) | device.minor as u64;
        Ok(stat)
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        let device = self.device.upgrade().ok_or(VfsError::NoDevice)?;
        if device.gone.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        let id = device.next_file.fetch_add(1, Ordering::Relaxed);
        // Linux: the first client of a primary node becomes master.
        let _ = device
            .master
            .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire);
        let file = Arc::new(File {
            device: device.clone(),
            id,
            handles: SpinLock::new(gem::Handles::new()),
            events: SpinLock::new(VecDeque::new()),
            wait: WaitQueue::new(),
            atomic: AtomicBool::new(false),
            universal_planes: AtomicBool::new(false),
            nonblock: AtomicBool::new(flags & abi::O_NONBLOCK != 0),
        });
        let mut files = device.files.lock();
        files.retain(|f| f.strong_count() != 0);
        files.push(Arc::downgrade(&file));
        Ok(Some(file))
    }
}

/// Publish /dev/dri/cardN and the connector entries under /sys/class/drm.
pub(crate) fn publish(device: &Arc<Device>) -> Result<(), VfsError> {
    use crate::{device::register, vfs::tree::Attribute};
    let name = format!("card{}", device.minor);
    let node: Arc<dyn Inode> = Arc::new(Node {
        device: Arc::downgrade(device),
    });
    let card = register(
        &name,
        "drm",
        Some(&format!("dri/{}", name)),
        Some(node),
        vec![],
    )?;
    let mut ids = vec![card.id];
    for (pipe, info) in device.config.pipes.iter().enumerate() {
        let kind = match info.connector_type as u32 {
            1 => "VGA",
            3 => "DVI-D",
            10 => "DP",
            11 => "HDMI-A",
            14 => "eDP",
            15 => "Virtual",
            _ => "Unknown",
        };
        let modes: alloc::string::String = info
            .modes
            .iter()
            .map(|m| format!("{}x{}\n", m.hdisplay, m.vdisplay))
            .collect();
        let weak = Arc::downgrade(device);
        let connector = register(
            &format!("{}-{}-{}", name, kind, pipe + 1),
            "drm",
            None,
            None,
            vec![
                ("status", Attribute::text("connected\n")),
                (
                    "enabled",
                    Attribute::new(0o444, move || {
                        let on = weak.upgrade().is_some_and(|d| d.kms.lock().active(pipe));
                        Ok(if on { b"enabled\n".to_vec() } else { b"disabled\n".to_vec() })
                    }),
                ),
                ("modes", Attribute::text(modes)),
                ("dpms", Attribute::text("On\n")),
            ],
        )?;
        ids.push(connector.id);
    }
    *device.nodes.lock() = ids;
    Ok(())
}

pub struct File {
    device: Arc<Device>,
    id: u64,
    handles: SpinLock<gem::Handles>,
    events: SpinLock<VecDeque<[u8; 32]>>,
    wait: WaitQueue,
    atomic: AtomicBool,
    universal_planes: AtomicBool,
    nonblock: AtomicBool,
}

/// Queue an event for `file` and wake its readers and pollers.
pub(crate) fn deliver(device: &Device, file: u64, event: &[u8; 32]) {
    let target = device
        .files
        .lock()
        .iter()
        .filter_map(Weak::upgrade)
        .find(|f| f.id == file);
    if let Some(target) = target {
        {
            let mut events = target.events.lock();
            if events.len() >= MAX_EVENTS {
                return;
            }
            events.push_back(*event);
        }
        target.wait.wake_all();
        crate::ipc::wake_object(Arc::as_ptr(&target));
    }
}

/// Wake every reader (device removal).
pub(crate) fn wake_all(device: &Device) {
    for file in device.files.lock().iter().filter_map(Weak::upgrade) {
        file.wait.wake_all();
        crate::ipc::wake_object(Arc::as_ptr(&file));
    }
}

impl File {
    fn is_master(&self) -> bool {
        self.device.master.load(Ordering::Acquire) == self.id
    }

    fn caller(&self) -> Caller<'_> {
        Caller {
            device: &self.device,
            file: self.id,
            atomic: self.atomic.load(Ordering::Acquire),
            universal_planes: self.universal_planes.load(Ordering::Acquire),
            handles: &self.handles,
        }
    }

    fn version(&self, arg: &mut Ioctl) -> Result<(), VfsError> {
        let config = &self.device.config;
        arg.set_u32(0, config.version.0 as u32);
        arg.set_u32(4, config.version.1 as u32);
        arg.set_u32(8, config.version.2 as u32);
        for (length_at, pointer_at, text) in [
            (16, 24, config.name.as_bytes()),
            (32, 40, config.date.as_bytes()),
            (48, 56, config.description.as_bytes()),
        ] {
            let capacity = arg.u64(length_at) as usize;
            let pointer = arg.u64(pointer_at);
            if pointer != 0 && capacity != 0 {
                crate::user::uaccess::write(pointer, &text[..capacity.min(text.len())])?;
            }
            arg.set_u64(length_at, text.len() as u64);
        }
        Ok(())
    }

    fn get_cap(&self, arg: &mut Ioctl) -> Result<(), VfsError> {
        let config = &self.device.config;
        let cursor = config.pipes[0].cursor.unwrap_or((64, 64));
        let value = match arg.u64(0) {
            0x1 => 1,                                // DUMB_BUFFER
            0x2 => 1,                                // VBLANK_HIGH_CRTC
            0x3 => 24,                               // DUMB_PREFERRED_DEPTH
            0x4 => config.prefer_shadow as u64,      // DUMB_PREFER_SHADOW
            0x5 => 0,                                // PRIME (not yet)
            0x6 => 1,                                // TIMESTAMP_MONOTONIC
            0x7 => 0,                                // ASYNC_PAGE_FLIP
            0x8 => cursor.0 as u64,                  // CURSOR_WIDTH
            0x9 => cursor.1 as u64,                  // CURSOR_HEIGHT
            0x10 => 1,                               // ADDFB2_MODIFIERS (linear)
            0x11 => 0,                               // PAGE_FLIP_TARGET
            0x12 => 1,                               // CRTC_IN_VBLANK_EVENT
            0x13 | 0x14 | 0x15 => 0,                 // SYNCOBJ*, ATOMIC_ASYNC_PAGE_FLIP
            _ => return Err(VfsError::InvalidArgument),
        };
        arg.set_u64(8, value);
        Ok(())
    }

    fn set_client_cap(&self, arg: &mut Ioctl) -> Result<(), VfsError> {
        let value = arg.u64(8);
        if value > 1 {
            return Err(VfsError::InvalidArgument);
        }
        match arg.u64(0) {
            1 | 4 | 6 => {} // STEREO_3D, ASPECT_RATIO, CURSOR_PLANE_HOTSPOT
            2 => self.universal_planes.store(value == 1, Ordering::Release),
            3 => {
                self.atomic.store(value == 1, Ordering::Release);
                if value == 1 {
                    self.universal_planes.store(true, Ordering::Release);
                }
            }
            5 if value == 0 => {} // no writeback connectors
            _ => return Err(VfsError::InvalidArgument),
        }
        Ok(())
    }

    fn create_dumb(&self, arg: &mut Ioctl) -> Result<(), VfsError> {
        let (height, width, bpp) = (arg.u32(0), arg.u32(4), arg.u32(8));
        let max = self.device.config.max_size;
        if width == 0 || height == 0 || width > max.0 || height > max.1 || !matches!(bpp, 8 | 16 | 24 | 32) || arg.u32(12) != 0
        {
            return Err(VfsError::InvalidArgument);
        }
        // 64-byte aligned rows keep every line start cache-line aligned for
        // the drivers' line-by-line copies.
        let pitch = (width * bpp.div_ceil(8)).div_ceil(64) * 64;
        let object = GemObject::new(pitch as u64 * height as u64)?;
        self.device.gem.insert(&object);
        let size = object.size;
        let handle = self.handles.lock().insert(object);
        arg.set_u32(16, handle);
        arg.set_u32(20, pitch);
        arg.set_u64(24, size);
        Ok(())
    }

    fn dispatch(&self, arg: &mut Ioctl) -> Result<(), VfsError> {
        use ioctl::*;
        if self.device.gone.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        let master_only = matches!(
            arg.nr,
            MODE_SETCRTC
                | MODE_CURSOR
                | MODE_CURSOR2
                | MODE_SETGAMMA
                | MODE_SETPROPERTY
                | MODE_PAGE_FLIP
                | MODE_DIRTYFB
                | MODE_SETPLANE
                | MODE_OBJ_SETPROPERTY
                | MODE_ATOMIC
                | AUTH_MAGIC
        );
        if master_only && !self.is_master() {
            return Err(VfsError::PermissionDenied);
        }
        let c = self.caller();
        match arg.nr {
            VERSION => self.version(arg),
            GET_UNIQUE => {
                let unique = format!("drm:card{}", self.device.minor);
                let capacity = arg.u64(0) as usize;
                if arg.u64(8) != 0 && capacity != 0 {
                    crate::user::uaccess::write(arg.u64(8), &unique.as_bytes()[..capacity.min(unique.len())])?;
                }
                arg.set_u64(0, unique.len() as u64);
                Ok(())
            }
            GET_MAGIC => {
                arg.set_u32(0, self.id as u32);
                Ok(())
            }
            AUTH_MAGIC => Ok(()),
            SET_VERSION => {
                if arg.i32(0) == -1 {
                    arg.set_u32(0, 1);
                    arg.set_u32(4, 4);
                }
                if arg.i32(8) == -1 {
                    arg.set_u32(8, self.device.config.version.0 as u32);
                    arg.set_u32(12, self.device.config.version.1 as u32);
                }
                Ok(())
            }
            GET_CAP => self.get_cap(arg),
            SET_CLIENT_CAP => self.set_client_cap(arg),
            SET_MASTER => self
                .device
                .master
                .compare_exchange(0, self.id, Ordering::AcqRel, Ordering::Acquire)
                .map(|_| ())
                .or_else(|holder| if holder == self.id { Ok(()) } else { Err(VfsError::Busy) }),
            DROP_MASTER => self
                .device
                .master
                .compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Acquire)
                .map(|_| ())
                .map_err(|_| VfsError::InvalidArgument),
            GEM_CLOSE | MODE_DESTROY_DUMB => self.handles.lock().remove(arg.u32(0)),
            MODE_CREATE_DUMB => self.create_dumb(arg),
            MODE_MAP_DUMB => {
                let object = self.handles.lock().get(arg.u32(0))?;
                arg.set_u64(8, object.mmap_offset());
                Ok(())
            }
            WAIT_VBLANK => super::vblank::wait_vblank(&self.device, self.id, arg),
            CRTC_GET_SEQUENCE => super::vblank::get_sequence(&self.device, arg),
            CRTC_QUEUE_SEQUENCE => super::vblank::queue_sequence(&self.device, self.id, arg),
            MODE_GETRESOURCES => kms::get_resources(&c, arg),
            MODE_GETCRTC => kms::get_crtc(&c, arg),
            MODE_SETCRTC => kms::set_crtc(&c, arg),
            MODE_CURSOR => kms::cursor(&c, arg, false),
            MODE_CURSOR2 => kms::cursor(&c, arg, true),
            // No gamma LUT: Linux returns EINVAL for gamma_size 0.
            MODE_GETGAMMA | MODE_SETGAMMA => Err(VfsError::InvalidArgument),
            MODE_GETENCODER => kms::get_encoder(&c, arg),
            MODE_GETCONNECTOR => kms::get_connector(&c, arg),
            MODE_GETPROPERTY => kms::get_property(&c, arg),
            MODE_SETPROPERTY => kms::set_property(&c, arg.u32(12), arg.u32(8), arg.u64(0)),
            MODE_GETPROPBLOB => kms::get_blob(&c, arg),
            MODE_GETFB => kms::get_fb(&c, arg, false),
            MODE_GETFB2 => kms::get_fb(&c, arg, true),
            MODE_ADDFB => kms::addfb(&c, arg),
            MODE_ADDFB2 => kms::addfb2(&c, arg),
            MODE_RMFB => kms::rmfb(&c, arg),
            MODE_PAGE_FLIP => kms::page_flip(&c, arg),
            MODE_DIRTYFB => kms::dirty_fb(&c, arg),
            MODE_GETPLANERESOURCES => kms::get_plane_resources(&c, arg),
            MODE_GETPLANE => kms::get_plane(&c, arg),
            MODE_SETPLANE => self.set_plane(&c, arg),
            MODE_OBJ_GETPROPERTIES => kms::obj_get_properties(&c, arg),
            MODE_OBJ_SETPROPERTY => kms::set_property(&c, arg.u32(12), arg.u32(8), arg.u64(0)),
            MODE_ATOMIC => kms::atomic(&c, arg),
            MODE_CREATEPROPBLOB => kms::create_blob(&c, arg),
            MODE_DESTROYPROPBLOB => kms::destroy_blob(&c, arg),
            _ => Err(VfsError::InvalidArgument),
        }
    }

    /// Legacy SETPLANE as an atomic commit on the plane alone.
    fn set_plane(&self, c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
        use kms::Change;
        let plane = arg.u32(0);
        let fb = arg.u32(8);
        // Property ids of FB_ID, CRTC_ID, SRC_*, CRTC_* (kms::Prop order).
        let (fb_id, crtc_id, src_x, src_y, src_w, src_h, crtc_x, crtc_y, crtc_w, crtc_h) =
            (8, 9, 10, 11, 12, 13, 14, 15, 16, 17);
        let mut changes = vec![
            Change { object: plane, prop: fb_id, value: fb as u64 },
            Change { object: plane, prop: crtc_id, value: if fb == 0 { 0 } else { arg.u32(4) as u64 } },
        ];
        if fb != 0 {
            changes.extend([
                Change { object: plane, prop: crtc_x, value: arg.i32(16) as i64 as u64 },
                Change { object: plane, prop: crtc_y, value: arg.i32(20) as i64 as u64 },
                Change { object: plane, prop: crtc_w, value: arg.u32(24) as u64 },
                Change { object: plane, prop: crtc_h, value: arg.u32(28) as u64 },
                Change { object: plane, prop: src_x, value: arg.u32(32) as u64 },
                Change { object: plane, prop: src_y, value: arg.u32(36) as u64 },
                Change { object: plane, prop: src_h, value: arg.u32(40) as u64 },
                Change { object: plane, prop: src_w, value: arg.u32(44) as u64 },
            ]);
        }
        kms::commit(c.device, c.file, &changes, 0, 0)
    }
}

impl Inode for File {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(0x900000 + self.device.minor as u64, InodeKind::CharDevice, 0o660, 0);
        stat.rdev = (DRM_MAJOR << 8) | self.device.minor as u64;
        Ok(stat)
    }

    fn ioctl(&self, cmd: u64, address: u64) -> Result<u64, VfsError> {
        let mut arg = Ioctl::new(cmd, address)?;
        self.dispatch(&mut arg)?;
        arg.finish()?;
        Ok(0)
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        loop {
            {
                let mut events = self.events.lock();
                if let Some(first) = events.front() {
                    if buffer.len() < first.len() {
                        return Err(VfsError::InvalidArgument);
                    }
                    let mut written = 0;
                    while let Some(event) = events.front() {
                        if written + event.len() > buffer.len() {
                            break;
                        }
                        buffer[written..written + event.len()].copy_from_slice(event);
                        written += event.len();
                        events.pop_front();
                    }
                    return Ok(written);
                }
            }
            if self.device.gone.load(Ordering::Acquire) {
                return Err(VfsError::NoDevice);
            }
            if self.nonblock.load(Ordering::Acquire) {
                return Err(VfsError::WouldBlock);
            }
            let ready = || !self.events.lock().is_empty() || self.device.gone.load(Ordering::Acquire);
            if self.wait.wait_interruptible(ready) == crate::sched::WaitResult::Interrupted {
                return Err(VfsError::Interrupted);
            }
        }
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(self as *const Self);
    }

    fn poll(&self) -> u32 {
        const POLLIN: u32 = 1;
        const POLLOUT: u32 = 4;
        const POLLERR: u32 = 8;
        if self.device.gone.load(Ordering::Acquire) {
            return POLLERR;
        }
        let readable = if self.events.lock().is_empty() { 0 } else { POLLIN };
        readable | POLLOUT
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }

    fn mmap(
        &self,
        space: &mut UserAddressSpace,
        address: u64,
        length: u64,
        offset: u64,
        prot: u32,
    ) -> Result<(), VfsError> {
        let (object, relative) = self.device.gem.by_offset(offset).ok_or(VfsError::InvalidArgument)?;
        // Like drm_vma_node_is_allowed: only files holding a handle may map.
        if !self.handles.lock().contains(&object) {
            return Err(VfsError::PermissionDenied);
        }
        object.map_user(space, address, length, relative, prot)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let device = self.device.clone();
        kms::close_file(&device, self.id);
        let _ = device
            .master
            .compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Acquire);
        let others = {
            let mut files = device.files.lock();
            files.retain(|f| f.strong_count() != 0);
            !files.is_empty()
        };
        if !others && !device.gone.load(Ordering::Acquire) {
            lastclose(&device);
        }
    }
}

/// Turn every pipe off and give the display back to the console.
fn lastclose(device: &Arc<Device>) {
    device.modeset(|| {
        let active: Vec<usize> = (0..device.config.pipes.len())
            .filter(|&p| device.kms.lock().active(p))
            .collect();
        *device.kms.lock() = kms::State::new(&device.config);
        for pipe in active {
            let _ = device.driver.set_mode(pipe, None);
            super::vblank::crtc_off(device, pipe);
        }
    });
    device.driver.lastclose();
    if device.config.takes_over_console {
        crate::tty::vt::release_scanout();
    }
}
