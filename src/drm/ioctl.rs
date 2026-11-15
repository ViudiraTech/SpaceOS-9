/*
 *
 *       src/drm/ioctl.rs
 *       DRM ioctl argument marshalling and the core ioctl table
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Sizes and directions follow Linux `include/uapi/drm/drm.h` and
//! `drm_mode.h` (checked against `tools/drm_abi_probe.c`). As in Linux
//! `drm_ioctl`, a shorter user struct is zero-extended and a longer one must
//! have zero tail bytes, so userspace built against other UAPI versions
//! keeps working.

use crate::{user::uaccess, vfs::VfsError};
use alloc::{vec, vec::Vec};

pub const VERSION: u8 = 0x00;
pub const GET_UNIQUE: u8 = 0x01;
pub const GET_MAGIC: u8 = 0x02;
pub const SET_VERSION: u8 = 0x07;
pub const GEM_CLOSE: u8 = 0x09;
pub const GET_CAP: u8 = 0x0c;
pub const SET_CLIENT_CAP: u8 = 0x0d;
pub const AUTH_MAGIC: u8 = 0x11;
pub const SET_MASTER: u8 = 0x1e;
pub const DROP_MASTER: u8 = 0x1f;
pub const WAIT_VBLANK: u8 = 0x3a;
pub const CRTC_GET_SEQUENCE: u8 = 0x3b;
pub const CRTC_QUEUE_SEQUENCE: u8 = 0x3c;
pub const MODE_GETRESOURCES: u8 = 0xa0;
pub const MODE_GETCRTC: u8 = 0xa1;
pub const MODE_SETCRTC: u8 = 0xa2;
pub const MODE_CURSOR: u8 = 0xa3;
pub const MODE_GETGAMMA: u8 = 0xa4;
pub const MODE_SETGAMMA: u8 = 0xa5;
pub const MODE_GETENCODER: u8 = 0xa6;
pub const MODE_GETCONNECTOR: u8 = 0xa7;
pub const MODE_GETPROPERTY: u8 = 0xaa;
pub const MODE_SETPROPERTY: u8 = 0xab;
pub const MODE_GETPROPBLOB: u8 = 0xac;
pub const MODE_GETFB: u8 = 0xad;
pub const MODE_ADDFB: u8 = 0xae;
pub const MODE_RMFB: u8 = 0xaf;
pub const MODE_PAGE_FLIP: u8 = 0xb0;
pub const MODE_DIRTYFB: u8 = 0xb1;
pub const MODE_CREATE_DUMB: u8 = 0xb2;
pub const MODE_MAP_DUMB: u8 = 0xb3;
pub const MODE_DESTROY_DUMB: u8 = 0xb4;
pub const MODE_GETPLANERESOURCES: u8 = 0xb5;
pub const MODE_GETPLANE: u8 = 0xb6;
pub const MODE_SETPLANE: u8 = 0xb7;
pub const MODE_ADDFB2: u8 = 0xb8;
pub const MODE_OBJ_GETPROPERTIES: u8 = 0xb9;
pub const MODE_OBJ_SETPROPERTY: u8 = 0xba;
pub const MODE_CURSOR2: u8 = 0xbb;
pub const MODE_ATOMIC: u8 = 0xbc;
pub const MODE_CREATEPROPBLOB: u8 = 0xbd;
pub const MODE_DESTROYPROPBLOB: u8 = 0xbe;
pub const MODE_GETFB2: u8 = 0xce;

const READ: u32 = 1; // user -> kernel (_IOW)
const WRITE: u32 = 2; // kernel -> user (_IOR)

/// `(size, direction)` of a core ioctl.
pub fn descriptor(nr: u8) -> Option<(usize, u32)> {
    let rw = READ | WRITE;
    Some(match nr {
        VERSION => (64, rw),
        GET_UNIQUE => (16, rw),
        GET_MAGIC => (4, WRITE),
        SET_VERSION => (16, rw),
        GEM_CLOSE => (8, READ),
        GET_CAP => (16, rw),
        SET_CLIENT_CAP => (16, READ),
        AUTH_MAGIC => (4, READ),
        SET_MASTER | DROP_MASTER => (0, 0),
        WAIT_VBLANK => (24, rw),
        CRTC_GET_SEQUENCE => (24, rw),
        CRTC_QUEUE_SEQUENCE => (24, rw),
        MODE_GETRESOURCES => (64, rw),
        MODE_GETCRTC | MODE_SETCRTC => (104, rw),
        MODE_CURSOR => (28, rw),
        MODE_CURSOR2 => (36, rw),
        MODE_GETGAMMA | MODE_SETGAMMA => (32, rw),
        MODE_GETENCODER => (20, rw),
        MODE_GETCONNECTOR => (80, rw),
        MODE_GETPROPERTY => (64, rw),
        MODE_SETPROPERTY => (16, rw),
        MODE_GETPROPBLOB => (16, rw),
        MODE_GETFB | MODE_ADDFB => (28, rw),
        MODE_RMFB => (4, rw),
        MODE_PAGE_FLIP | MODE_DIRTYFB => (24, rw),
        MODE_CREATE_DUMB => (32, rw),
        MODE_MAP_DUMB => (16, rw),
        MODE_DESTROY_DUMB => (4, rw),
        MODE_GETPLANERESOURCES => (16, rw),
        MODE_GETPLANE => (32, rw),
        MODE_SETPLANE => (48, rw),
        MODE_ADDFB2 | MODE_GETFB2 => (104, rw),
        MODE_OBJ_GETPROPERTIES => (32, rw),
        MODE_OBJ_SETPROPERTY => (24, rw),
        MODE_ATOMIC => (56, rw),
        MODE_CREATEPROPBLOB => (16, rw),
        MODE_DESTROYPROPBLOB => (4, rw),
        _ => return None,
    })
}

/// One ioctl's argument block, copied in before and out after the handler.
pub struct Ioctl {
    pub nr: u8,
    pub data: Vec<u8>,
    address: u64,
    user_size: usize,
    direction: u32,
}

impl Ioctl {
    pub fn new(cmd: u64, address: u64) -> Result<Self, VfsError> {
        if (cmd >> 8) & 0xff != b'd' as u64 || cmd >> 32 != 0 {
            return Err(VfsError::NotATty);
        }
        let nr = cmd as u8;
        let (size, direction) = descriptor(nr).ok_or(VfsError::InvalidArgument)?;
        let user_size = ((cmd >> 16) & 0x3fff) as usize;
        if size == 0 && user_size != 0 {
            return Err(VfsError::InvalidArgument);
        }
        let mut data = vec![0u8; size];
        let copy = user_size.min(size);
        if direction & READ != 0 && copy != 0 {
            uaccess::read(address, &mut data[..copy])?;
        }
        if direction & READ != 0 && user_size > size {
            let tail = uaccess::read_vec(address + size as u64, user_size - size, 4096)?;
            if tail.iter().any(|&byte| byte != 0) {
                return Err(VfsError::InvalidArgument);
            }
        }
        Ok(Self {
            nr,
            data,
            address,
            user_size,
            direction,
        })
    }

    pub fn finish(&self) -> Result<(), VfsError> {
        if self.direction & WRITE != 0 && self.user_size != 0 {
            let copy = self.user_size.min(self.data.len());
            uaccess::write(self.address, &self.data[..copy])?;
        }
        Ok(())
    }

    pub fn u32(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap())
    }
    pub fn i32(&self, offset: usize) -> i32 {
        self.u32(offset) as i32
    }
    pub fn u64(&self, offset: usize) -> u64 {
        u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
    }
    pub fn set_u32(&mut self, offset: usize, value: u32) {
        self.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    pub fn set_u64(&mut self, offset: usize, value: u64) {
        self.data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}

/// Copy `values` to the user array at `pointer` when `capacity` allows (the
/// Linux two-call convention: first call learns counts, second fills).
pub fn copy_out_u32(pointer: u64, capacity: u32, values: &[u32]) -> Result<(), VfsError> {
    if pointer == 0 || (capacity as usize) < values.len() || values.is_empty() {
        return Ok(());
    }
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    uaccess::write(pointer, &bytes)
}

pub fn copy_out_u64(pointer: u64, capacity: u32, values: &[u64]) -> Result<(), VfsError> {
    if pointer == 0 || (capacity as usize) < values.len() || values.is_empty() {
        return Ok(());
    }
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    uaccess::write(pointer, &bytes)
}

pub fn read_u32s(pointer: u64, count: usize) -> Result<Vec<u32>, VfsError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > 1 << 16 {
        return Err(VfsError::InvalidArgument);
    }
    let bytes = uaccess::read_vec(pointer, count * 4, count * 4)?;
    Ok(bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect())
}

pub fn read_u64s(pointer: u64, count: usize) -> Result<Vec<u64>, VfsError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > 1 << 16 {
        return Err(VfsError::InvalidArgument);
    }
    let bytes = uaccess::read_vec(pointer, count * 8, count * 8)?;
    Ok(bytes
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
        .collect())
}
