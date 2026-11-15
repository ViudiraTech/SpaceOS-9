/*
 *
 *       src/drm/kms.rs
 *       KMS object model, properties, atomic check/commit and legacy paths
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Objects per pipe: connector -> encoder -> CRTC, a primary plane and an
//! optional cursor plane. Property names, flags and object types match
//! Linux, since userspace finds properties by name.
//!
//! Locking: commits run under the device's sleeping modeset lock. `State`
//! (behind a spinlock) is only cloned and written back around driver calls,
//! never held across them.

use super::{
    CursorUpdate, Device, Mode, PlaneUpdate,
    ioctl::{self, Ioctl},
};
use crate::vfs::VfsError;
use alloc::{collections::BTreeMap, sync::Arc, vec, vec::Vec};

// ---- object and property identifiers --------------------------------------

pub const OBJ_CRTC: u32 = 0xcccc_cccc;
pub const OBJ_CONNECTOR: u32 = 0xc0c0_c0c0;
pub const OBJ_ENCODER: u32 = 0xe0e0_e0e0;
pub const OBJ_PROPERTY: u32 = 0xb0b0_b0b0;
pub const OBJ_FB: u32 = 0xfbfb_fbfb;
pub const OBJ_BLOB: u32 = 0xbbbb_bbbb;
pub const OBJ_PLANE: u32 = 0xeeee_eeee;

const PROP_RANGE: u32 = 1 << 1;
const PROP_IMMUTABLE: u32 = 1 << 2;
const PROP_ENUM: u32 = 1 << 3;
const PROP_BLOB: u32 = 1 << 4;
const PROP_OBJECT: u32 = 1 << 6;
const PROP_SIGNED_RANGE: u32 = 2 << 6;
const PROP_ATOMIC: u32 = 1 << 31;

/// Objects of pipe `p` are 0x100 + 8p + kind.
const PIPE_BASE: u32 = 0x100;
const KIND_CONNECTOR: u32 = 0;
const KIND_ENCODER: u32 = 1;
const KIND_CRTC: u32 = 2;
pub(crate) const KIND_CRTC_PUBLIC: u32 = KIND_CRTC;
const KIND_PRIMARY: u32 = 3;
const KIND_CURSOR: u32 = 4;

pub fn object_id(pipe: usize, kind: u32) -> u32 {
    PIPE_BASE + 8 * pipe as u32 + kind
}

fn decode_object(device: &Device, id: u32) -> Option<(usize, u32)> {
    let index = id.checked_sub(PIPE_BASE)?;
    let pipe = (index / 8) as usize;
    let kind = index % 8;
    if pipe >= device.config.pipes.len() || kind > KIND_CURSOR {
        return None;
    }
    if kind == KIND_CURSOR && device.config.pipes[pipe].cursor.is_none() {
        return None;
    }
    Some((pipe, kind))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
enum Prop {
    ConnectorCrtc = 1,
    Dpms,
    Edid,
    NonDesktop,
    Active,
    ModeId,
    Type,
    FbId,
    PlaneCrtc,
    SrcX,
    SrcY,
    SrcW,
    SrcH,
    CrtcX,
    CrtcY,
    CrtcW,
    CrtcH,
    InFormats,
    Damage,
    InFence,
}

const ALL_PROPS: [Prop; 20] = [
    Prop::ConnectorCrtc,
    Prop::Dpms,
    Prop::Edid,
    Prop::NonDesktop,
    Prop::Active,
    Prop::ModeId,
    Prop::Type,
    Prop::FbId,
    Prop::PlaneCrtc,
    Prop::SrcX,
    Prop::SrcY,
    Prop::SrcW,
    Prop::SrcH,
    Prop::CrtcX,
    Prop::CrtcY,
    Prop::CrtcW,
    Prop::CrtcH,
    Prop::InFormats,
    Prop::Damage,
    Prop::InFence,
];

impl Prop {
    fn from_id(id: u32) -> Option<Self> {
        ALL_PROPS.get(id.checked_sub(1)? as usize).copied()
    }

    fn name(self) -> &'static str {
        match self {
            Prop::ConnectorCrtc | Prop::PlaneCrtc => "CRTC_ID",
            Prop::Dpms => "DPMS",
            Prop::Edid => "EDID",
            Prop::NonDesktop => "non-desktop",
            Prop::Active => "ACTIVE",
            Prop::ModeId => "MODE_ID",
            Prop::Type => "type",
            Prop::FbId => "FB_ID",
            Prop::SrcX => "SRC_X",
            Prop::SrcY => "SRC_Y",
            Prop::SrcW => "SRC_W",
            Prop::SrcH => "SRC_H",
            Prop::CrtcX => "CRTC_X",
            Prop::CrtcY => "CRTC_Y",
            Prop::CrtcW => "CRTC_W",
            Prop::CrtcH => "CRTC_H",
            Prop::InFormats => "IN_FORMATS",
            Prop::Damage => "FB_DAMAGE_CLIPS",
            Prop::InFence => "IN_FENCE_FD",
        }
    }

    fn flags(self) -> u32 {
        match self {
            Prop::ConnectorCrtc | Prop::PlaneCrtc | Prop::FbId => PROP_OBJECT | PROP_ATOMIC,
            Prop::Dpms => PROP_ENUM,
            Prop::Edid | Prop::InFormats => PROP_BLOB | PROP_IMMUTABLE,
            Prop::NonDesktop => PROP_RANGE | PROP_IMMUTABLE,
            Prop::Active => PROP_RANGE | PROP_ATOMIC,
            Prop::ModeId | Prop::Damage => PROP_BLOB | PROP_ATOMIC,
            Prop::Type => PROP_ENUM | PROP_IMMUTABLE,
            Prop::SrcX | Prop::SrcY | Prop::SrcW | Prop::SrcH => PROP_RANGE | PROP_ATOMIC,
            Prop::CrtcX | Prop::CrtcY | Prop::InFence => PROP_SIGNED_RANGE | PROP_ATOMIC,
            Prop::CrtcW | Prop::CrtcH => PROP_RANGE | PROP_ATOMIC,
        }
    }

    /// Range bounds or object type, as GETPROPERTY reports them.
    fn values(self) -> Vec<u64> {
        match self {
            Prop::ConnectorCrtc | Prop::PlaneCrtc => vec![OBJ_CRTC as u64],
            Prop::FbId => vec![OBJ_FB as u64],
            Prop::NonDesktop | Prop::Active => vec![0, 1],
            Prop::SrcX | Prop::SrcY | Prop::SrcW | Prop::SrcH => vec![0, u32::MAX as u64],
            Prop::CrtcX | Prop::CrtcY => vec![i32::MIN as i64 as u64, i32::MAX as u64],
            Prop::CrtcW | Prop::CrtcH => vec![0, i32::MAX as u64],
            Prop::InFence => vec![-1i64 as u64, i32::MAX as u64],
            Prop::Dpms => vec![0, 1, 2, 3],
            Prop::Type => vec![0, 1, 2],
            _ => Vec::new(),
        }
    }

    fn enums(self) -> &'static [(u64, &'static str)] {
        match self {
            Prop::Dpms => &[(0, "On"), (1, "Standby"), (2, "Suspend"), (3, "Off")],
            Prop::Type => &[(0, "Overlay"), (1, "Primary"), (2, "Cursor")],
            _ => &[],
        }
    }
}

// ---- framebuffers, blobs and rectangles -----------------------------------

/// A half-open rectangle `[x1, x2) x [y1, y2)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Rect {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x1: x,
            y1: y,
            x2: x.saturating_add(width as i32),
            y2: y.saturating_add(height as i32),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.x2 <= self.x1 || self.y2 <= self.y1
    }
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let r = Rect {
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
            x2: self.x2.min(other.x2),
            y2: self.y2.min(other.y2),
        };
        (!r.is_empty()).then_some(r)
    }
    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
            x2: self.x2.max(other.x2),
            y2: self.y2.max(other.y2),
        }
    }
    fn area(&self) -> i64 {
        (self.x2 - self.x1) as i64 * (self.y2 - self.y1) as i64
    }
}

/// Merge damage the way Linux's damage helpers end up behaving for simple
/// displays: drop empty/outside clips and, when many small clips cover most
/// of their bounding box, present the box once (fewer driver calls, each a
/// long sequential copy).
pub fn coalesce(mut clips: Vec<Rect>, bounds: &Rect) -> Vec<Rect> {
    clips.retain_mut(|clip| match clip.intersect(bounds) {
        Some(inside) => {
            *clip = inside;
            true
        }
        None => false,
    });
    if clips.len() <= 1 {
        return clips;
    }
    let mut boxed = clips[0];
    let mut covered = 0i64;
    for clip in &clips {
        boxed = boxed.union(clip);
        covered += clip.area();
    }
    if clips.len() > 8 || covered * 4 >= boxed.area() * 3 {
        vec![boxed]
    } else {
        clips
    }
}

pub struct Framebuffer {
    pub id: u32,
    pub owner: u64,
    pub object: Arc<super::GemObject>,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub pitch: u32,
    pub offset: u32,
}

impl Framebuffer {
    /// Kernel address of pixel (0, 0).
    pub fn pixels(&self) -> *const u8 {
        // SAFETY: `offset` was validated against the object size.
        unsafe { self.object.vaddr().add(self.offset as usize) }
    }
}

pub(crate) struct Blob {
    pub data: Vec<u8>,
    owner: u64,
}

// ---- state ----------------------------------------------------------------

#[derive(Clone, Default)]
pub(crate) struct PlaneState {
    crtc: Option<usize>,
    fb: Option<Arc<Framebuffer>>,
    src: [u32; 4],
    crtc_pos: [i32; 2],
    crtc_size: [u32; 2],
    damage: Option<Arc<Blob>>,
}

#[derive(Clone, Default)]
pub(crate) struct CrtcState {
    active: bool,
    mode: Option<Arc<Blob>>,
}

#[derive(Clone, Default)]
pub(crate) struct ConnectorState {
    crtc: Option<usize>,
    dpms: u32,
}

#[derive(Clone)]
pub(crate) struct Config {
    /// Index 2p: primary plane of pipe p; 2p+1: cursor plane.
    planes: Vec<PlaneState>,
    crtcs: Vec<CrtcState>,
    connectors: Vec<ConnectorState>,
}

pub(crate) struct State {
    config: Config,
    cursor_hot: Vec<(i32, i32)>,
    pub(crate) blobs: BTreeMap<u32, Arc<Blob>>,
    next_blob: u32,
    pub(crate) fbs: BTreeMap<u32, Arc<Framebuffer>>,
    next_fb: u32,
    /// Immutable per-pipe blobs: IN_FORMATS (primary, cursor) and EDID.
    formats_blob: Vec<(u32, u32)>,
    edid_blob: Vec<u32>,
}

fn format_blob(formats: &[u32]) -> Vec<u8> {
    // struct drm_format_modifier_blob + formats + one LINEAR modifier.
    let formats_offset = 24u32;
    let modifiers_offset = (formats_offset + 4 * formats.len() as u32).div_ceil(8) * 8;
    let mut blob = vec![0u8; modifiers_offset as usize + 24];
    for (offset, value) in [
        (0, 1u32),
        (4, 0),
        (8, formats.len() as u32),
        (12, formats_offset),
        (16, 1),
        (20, modifiers_offset),
    ] {
        blob[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (i, format) in formats.iter().enumerate() {
        let at = formats_offset as usize + 4 * i;
        blob[at..at + 4].copy_from_slice(&format.to_le_bytes());
    }
    let mask: u64 = if formats.len() >= 64 { u64::MAX } else { (1u64 << formats.len()) - 1 };
    let at = modifiers_offset as usize;
    blob[at..at + 8].copy_from_slice(&mask.to_le_bytes());
    // offset 0, modifier 0 (DRM_FORMAT_MOD_LINEAR) already zero.
    blob
}

impl State {
    pub(crate) fn new(config: &super::DeviceConfig) -> Self {
        let pipes = config.pipes.len();
        let mut state = Self {
            config: Config {
                planes: vec![PlaneState::default(); pipes * 2],
                crtcs: vec![CrtcState::default(); pipes],
                connectors: vec![ConnectorState::default(); pipes],
            },
            cursor_hot: vec![(0, 0); pipes],
            blobs: BTreeMap::new(),
            next_blob: 0x1_0000,
            fbs: BTreeMap::new(),
            next_fb: 0x2_0000,
            formats_blob: Vec::new(),
            edid_blob: Vec::new(),
        };
        for pipe in &config.pipes {
            let primary = state.add_blob(format_blob(&pipe.formats), 0);
            let cursor = state.add_blob(format_blob(&[super::fourcc::ARGB8888]), 0);
            state.formats_blob.push((primary, cursor));
            let edid = if pipe.edid.is_empty() {
                0
            } else {
                state.add_blob(pipe.edid.clone(), 0)
            };
            state.edid_blob.push(edid);
        }
        state
    }

    fn add_blob(&mut self, data: Vec<u8>, owner: u64) -> u32 {
        let id = self.next_blob;
        self.next_blob += 1;
        self.blobs.insert(id, Arc::new(Blob { data, owner }));
        id
    }

    fn blob_id(&self, blob: &Option<Arc<Blob>>) -> u64 {
        blob.as_ref().map_or(0, |blob| {
            self.blobs
                .iter()
                .find(|(_, b)| Arc::ptr_eq(b, blob))
                .map_or(0, |(&id, _)| id as u64)
        })
    }

    pub(crate) fn mode(&self, pipe: usize) -> Option<Mode> {
        self.config.crtcs[pipe]
            .mode
            .as_ref()
            .and_then(|blob| Mode::decode(&blob.data))
    }

    pub(crate) fn active(&self, pipe: usize) -> bool {
        self.config.crtcs[pipe].active
    }
}

// ---- property values --------------------------------------------------------

fn object_props(device: &Device, pipe: usize, kind: u32, atomic: bool) -> Vec<Prop> {
    let props: &[Prop] = match kind {
        KIND_CONNECTOR => &[Prop::Dpms, Prop::Edid, Prop::NonDesktop, Prop::ConnectorCrtc],
        KIND_CRTC => &[Prop::Active, Prop::ModeId],
        KIND_PRIMARY | KIND_CURSOR => &[
            Prop::Type,
            Prop::FbId,
            Prop::PlaneCrtc,
            Prop::SrcX,
            Prop::SrcY,
            Prop::SrcW,
            Prop::SrcH,
            Prop::CrtcX,
            Prop::CrtcY,
            Prop::CrtcW,
            Prop::CrtcH,
            Prop::InFormats,
            Prop::Damage,
            Prop::InFence,
        ],
        _ => &[],
    };
    props
        .iter()
        .copied()
        .filter(|p| atomic || p.flags() & PROP_ATOMIC == 0)
        .filter(|p| *p != Prop::Edid || device.kms.lock().edid_blob[pipe] != 0)
        .collect()
}

fn prop_value(state: &State, pipe: usize, kind: u32, prop: Prop) -> u64 {
    fn plane(s: &State, pipe: usize, kind: u32) -> &PlaneState {
        &s.config.planes[2 * pipe + (kind == KIND_CURSOR) as usize]
    }
    match (kind, prop) {
        (KIND_CONNECTOR, Prop::ConnectorCrtc) => state.config.connectors[pipe]
            .crtc
            .map_or(0, |c| object_id(c, KIND_CRTC) as u64),
        (KIND_CONNECTOR, Prop::Dpms) => state.config.connectors[pipe].dpms as u64,
        (KIND_CONNECTOR, Prop::Edid) => state.edid_blob[pipe] as u64,
        (KIND_CRTC, Prop::Active) => state.config.crtcs[pipe].active as u64,
        (KIND_CRTC, Prop::ModeId) => state.blob_id(&state.config.crtcs[pipe].mode),
        (_, Prop::Type) => if kind == KIND_PRIMARY { 1 } else { 2 },
        (_, Prop::FbId) => plane(state, pipe, kind).fb.as_ref().map_or(0, |fb| fb.id as u64),
        (_, Prop::PlaneCrtc) => plane(state, pipe, kind).crtc.map_or(0, |c| object_id(c, KIND_CRTC) as u64),
        (_, Prop::SrcX) => plane(state, pipe, kind).src[0] as u64,
        (_, Prop::SrcY) => plane(state, pipe, kind).src[1] as u64,
        (_, Prop::SrcW) => plane(state, pipe, kind).src[2] as u64,
        (_, Prop::SrcH) => plane(state, pipe, kind).src[3] as u64,
        (_, Prop::CrtcX) => plane(state, pipe, kind).crtc_pos[0] as i64 as u64,
        (_, Prop::CrtcY) => plane(state, pipe, kind).crtc_pos[1] as i64 as u64,
        (_, Prop::CrtcW) => plane(state, pipe, kind).crtc_size[0] as u64,
        (_, Prop::CrtcH) => plane(state, pipe, kind).crtc_size[1] as u64,
        (_, Prop::InFormats) => {
            let (primary, cursor) = state.formats_blob[pipe];
            if kind == KIND_PRIMARY { primary as u64 } else { cursor as u64 }
        }
        (_, Prop::InFence) => -1i64 as u64,
        _ => 0,
    }
}

// ---- atomic commit -------------------------------------------------------------

pub const ATOMIC_PAGE_FLIP_EVENT: u32 = 0x1;
pub const ATOMIC_TEST_ONLY: u32 = 0x100;
pub const ATOMIC_NONBLOCK: u32 = 0x200;
pub const ATOMIC_ALLOW_MODESET: u32 = 0x400;

pub(crate) struct Change {
    pub object: u32,
    pub prop: u32,
    pub value: u64,
}

/// Apply one property change to `config` (state lock held).
fn apply(device: &Device, state: &State, config: &mut Config, change: &Change) -> Result<(), VfsError> {
    let (pipe, kind) = decode_object(device, change.object).ok_or(VfsError::NotFound)?;
    let prop = Prop::from_id(change.prop).ok_or(VfsError::NotFound)?;
    if !object_props(device, pipe, kind, true).contains(&prop) {
        return Err(VfsError::InvalidArgument);
    }
    if prop.flags() & PROP_IMMUTABLE != 0 {
        return Err(VfsError::InvalidArgument);
    }
    let crtc_of = |value: u64| -> Result<Option<usize>, VfsError> {
        if value == 0 {
            return Ok(None);
        }
        match decode_object(device, value as u32) {
            Some((p, KIND_CRTC)) if value <= u32::MAX as u64 => Ok(Some(p)),
            _ => Err(VfsError::InvalidArgument),
        }
    };
    let blob_of = |value: u64| -> Result<Option<Arc<Blob>>, VfsError> {
        if value == 0 {
            return Ok(None);
        }
        state
            .blobs
            .get(&(value as u32))
            .cloned()
            .map(Some)
            .ok_or(VfsError::InvalidArgument)
    };
    let value = change.value;
    let range = |value: u64, low: i64, high: i64| -> Result<u64, VfsError> {
        let signed = value as i64;
        if signed < low || signed > high {
            Err(VfsError::InvalidArgument)
        } else {
            Ok(value)
        }
    };
    match kind {
        KIND_CONNECTOR => match prop {
            Prop::ConnectorCrtc => config.connectors[pipe].crtc = crtc_of(value)?,
            Prop::Dpms => {
                config.connectors[pipe].dpms = range(value, 0, 3)? as u32;
            }
            _ => return Err(VfsError::InvalidArgument),
        },
        KIND_CRTC => match prop {
            Prop::Active => config.crtcs[pipe].active = range(value, 0, 1)? == 1,
            Prop::ModeId => {
                let blob = blob_of(value)?;
                if let Some(blob) = &blob
                    && Mode::decode(&blob.data).is_none()
                {
                    return Err(VfsError::InvalidArgument);
                }
                config.crtcs[pipe].mode = blob;
            }
            _ => return Err(VfsError::InvalidArgument),
        },
        _ => {
            let plane = &mut config.planes[2 * pipe + (kind == KIND_CURSOR) as usize];
            match prop {
                Prop::FbId => {
                    plane.fb = if value == 0 {
                        None
                    } else {
                        Some(state.fbs.get(&(value as u32)).cloned().ok_or(VfsError::InvalidArgument)?)
                    }
                }
                Prop::PlaneCrtc => plane.crtc = crtc_of(value)?,
                Prop::SrcX => plane.src[0] = range(value, 0, u32::MAX as i64)? as u32,
                Prop::SrcY => plane.src[1] = range(value, 0, u32::MAX as i64)? as u32,
                Prop::SrcW => plane.src[2] = range(value, 0, u32::MAX as i64)? as u32,
                Prop::SrcH => plane.src[3] = range(value, 0, u32::MAX as i64)? as u32,
                Prop::CrtcX => plane.crtc_pos[0] = range(value, i32::MIN as i64, i32::MAX as i64)? as i32,
                Prop::CrtcY => plane.crtc_pos[1] = range(value, i32::MIN as i64, i32::MAX as i64)? as i32,
                Prop::CrtcW => plane.crtc_size[0] = range(value, 0, i32::MAX as i64)? as u32,
                Prop::CrtcH => plane.crtc_size[1] = range(value, 0, i32::MAX as i64)? as u32,
                Prop::Damage => plane.damage = blob_of(value)?,
                // Explicit fences need sync_file support; -1 means none.
                Prop::InFence if value as i64 == -1 => {}
                _ => return Err(VfsError::InvalidArgument),
            }
        }
    }
    Ok(())
}

fn plane_rects(plane: &PlaneState) -> (Rect, Rect) {
    (
        Rect::new(
            (plane.src[0] >> 16) as i32,
            (plane.src[1] >> 16) as i32,
            plane.src[2] >> 16,
            plane.src[3] >> 16,
        ),
        Rect::new(
            plane.crtc_pos[0],
            plane.crtc_pos[1],
            plane.crtc_size[0],
            plane.crtc_size[1],
        ),
    )
}

/// Generic validation (Linux drm_atomic_helper_check + simple-display rules).
fn check(device: &Device, config: &Config) -> Result<(), VfsError> {
    for (pipe, info) in device.config.pipes.iter().enumerate() {
        let crtc = &config.crtcs[pipe];
        let connector = &config.connectors[pipe];
        if connector.crtc.is_some_and(|c| c != pipe) {
            return Err(VfsError::InvalidArgument);
        }
        let mode = crtc.mode.as_ref().and_then(|b| Mode::decode(&b.data));
        if crtc.mode.is_some() && mode.is_none() {
            return Err(VfsError::InvalidArgument);
        }
        if let Some(mode) = &mode
            && !info.modes.iter().any(|m| m.same_timing(mode))
        {
            return Err(VfsError::InvalidArgument);
        }
        if crtc.active && (mode.is_none() || connector.crtc != Some(pipe)) {
            return Err(VfsError::InvalidArgument);
        }
        let primary = &config.planes[2 * pipe];
        if primary.fb.is_some() != primary.crtc.is_some() {
            return Err(VfsError::InvalidArgument);
        }
        if let Some(fb) = &primary.fb {
            if primary.crtc != Some(pipe) || mode.is_none() {
                return Err(VfsError::InvalidArgument);
            }
            let mode = mode.unwrap();
            if !info.formats.contains(&fb.format)
                || primary.src.iter().any(|v| v & 0xffff != 0)
            {
                return Err(VfsError::InvalidArgument);
            }
            let (source, destination) = plane_rects(primary);
            // The primary plane covers the whole CRTC without scaling.
            if destination != Rect::new(0, 0, mode.hdisplay as u32, mode.vdisplay as u32)
                || source.x2 - source.x1 != destination.x2 - destination.x1
                || source.y2 - source.y1 != destination.y2 - destination.y1
                || source.x2 as u32 > fb.width
                || source.y2 as u32 > fb.height
            {
                return Err(VfsError::InvalidArgument);
            }
        }
        let cursor = &config.planes[2 * pipe + 1];
        if let Some(fb) = &cursor.fb {
            let (width, height) = info.cursor.ok_or(VfsError::InvalidArgument)?;
            if cursor.crtc != Some(pipe)
                || fb.format != super::fourcc::ARGB8888
                || fb.width > width
                || fb.height > height
            {
                return Err(VfsError::InvalidArgument);
            }
        }
    }
    Ok(())
}

fn update_for<'a>(plane: &'a PlaneState, damage: &'a [Rect]) -> PlaneUpdate<'a> {
    let (source, destination) = plane_rects(plane);
    PlaneUpdate {
        framebuffer: plane.fb.as_deref(),
        source,
        destination,
        damage,
    }
}

/// Damage for a plane: the whole source when the image or the mode changed,
/// FB_DAMAGE_CLIPS when given, nothing otherwise.
fn damage_for(old: &PlaneState, new: &PlaneState, full: bool) -> Vec<Rect> {
    let (source, _) = plane_rects(new);
    let same_fb = match (&old.fb, &new.fb) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    };
    if full || !same_fb || old.src != new.src {
        return vec![source];
    }
    let Some(blob) = &new.damage else {
        return Vec::new();
    };
    let clips = blob
        .data
        .chunks_exact(16)
        .map(|c| Rect {
            x1: i32::from_le_bytes(c[0..4].try_into().unwrap()),
            y1: i32::from_le_bytes(c[4..8].try_into().unwrap()),
            x2: i32::from_le_bytes(c[8..12].try_into().unwrap()),
            y2: i32::from_le_bytes(c[12..16].try_into().unwrap()),
        })
        .collect();
    coalesce(clips, &source)
}

/// Validate and apply `changes`; `flags` are DRM_MODE_ATOMIC_* bits.
pub(crate) fn commit(
    device: &Arc<Device>,
    file: u64,
    changes: &[Change],
    flags: u32,
    user_data: u64,
) -> Result<(), VfsError> {
    device.modeset(|| commit_locked(device, file, changes, flags, user_data))
}

fn commit_locked(
    device: &Arc<Device>,
    file: u64,
    changes: &[Change],
    flags: u32,
    user_data: u64,
) -> Result<(), VfsError> {
    let (old, new) = {
        let state = device.kms.lock();
        let old = state.config.clone();
        let mut new = old.clone();
        for plane in &mut new.planes {
            plane.damage = None; // damage applies to one commit only
        }
        for change in changes {
            apply(device, &state, &mut new, change)?;
        }
        (old, new)
    };
    check(device, &new)?;
    let pipes = device.config.pipes.len();
    let mut modeset = vec![false; pipes];
    let mut touched = vec![false; pipes];
    for pipe in 0..pipes {
        let mode_changed = old.crtcs[pipe].active != new.crtcs[pipe].active
            || old.crtcs[pipe].mode.as_ref().map(|b| &b.data) != new.crtcs[pipe].mode.as_ref().map(|b| &b.data)
            || old.connectors[pipe].crtc != new.connectors[pipe].crtc;
        modeset[pipe] = mode_changed;
        touched[pipe] = mode_changed
            || changes.iter().any(|c| decode_object(device, c.object).is_some_and(|(p, _)| p == pipe));
    }
    if modeset.iter().any(|&m| m) && flags & ATOMIC_ALLOW_MODESET == 0 {
        return Err(VfsError::InvalidArgument);
    }
    for pipe in 0..pipes {
        let mode = new.crtcs[pipe].mode.as_ref().and_then(|b| Mode::decode(&b.data));
        let primary = &new.planes[2 * pipe];
        let damage = damage_for(&old.planes[2 * pipe], primary, modeset[pipe]);
        device
            .driver
            .check(pipe, mode.as_ref(), &update_for(primary, &damage))?;
    }
    if flags & ATOMIC_PAGE_FLIP_EVENT != 0 {
        for pipe in (0..pipes).filter(|&p| touched[p]) {
            if !new.crtcs[pipe].active {
                return Err(VfsError::InvalidArgument);
            }
            if device.vblank[pipe].flip_pending() {
                return Err(VfsError::Busy);
            }
        }
    }
    if flags & ATOMIC_TEST_ONLY != 0 {
        return Ok(());
    }

    // Program the hardware. Turn off first, then modes, then planes.
    for pipe in 0..pipes {
        if modeset[pipe] && !new.crtcs[pipe].active {
            device.driver.set_mode(pipe, None)?;
            super::vblank::crtc_off(device, pipe);
        }
    }
    for pipe in 0..pipes {
        if !new.crtcs[pipe].active {
            continue;
        }
        let mode = new.crtcs[pipe].mode.as_ref().and_then(|b| Mode::decode(&b.data));
        if modeset[pipe] {
            device.driver.set_mode(pipe, mode.as_ref())?;
            super::vblank::crtc_on(device, pipe, mode.as_ref().unwrap());
        }
        let primary = &new.planes[2 * pipe];
        let damage = damage_for(&old.planes[2 * pipe], primary, modeset[pipe]);
        if !damage.is_empty() || modeset[pipe] || primary.fb.is_none() != old.planes[2 * pipe].fb.is_none() {
            device.driver.update_plane(pipe, &update_for(primary, &damage))?;
        }
        let cursor = &new.planes[2 * pipe + 1];
        let old_cursor = &old.planes[2 * pipe + 1];
        let cursor_changed = cursor.crtc_pos != old_cursor.crtc_pos
            || match (&cursor.fb, &old_cursor.fb) {
                (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                (None, None) => false,
                _ => true,
            };
        if device.config.pipes[pipe].cursor.is_some() && (cursor_changed || modeset[pipe]) {
            let hot = device.kms.lock().cursor_hot[pipe];
            let _ = device.driver.update_cursor(
                pipe,
                &CursorUpdate {
                    framebuffer: cursor.fb.as_deref(),
                    x: cursor.crtc_pos[0],
                    y: cursor.crtc_pos[1],
                    hot_x: hot.0,
                    hot_y: hot.1,
                },
            );
        }
    }
    let any_active = new.crtcs.iter().any(|c| c.active);
    device.kms.lock().config = new;
    if device.config.takes_over_console {
        if any_active {
            crate::tty::vt::claim_scanout();
        } else {
            crate::tty::vt::release_scanout();
        }
    }
    let event = flags & ATOMIC_PAGE_FLIP_EVENT != 0;
    let mut waits = Vec::new();
    for pipe in (0..pipes).filter(|&p| touched[p]) {
        if device.kms.lock().active(pipe) {
            let target = super::vblank::queue_flip(device, pipe, event.then_some((file, user_data)));
            waits.push((pipe, target));
        }
    }
    if flags & ATOMIC_NONBLOCK == 0 {
        // Blocking commits return once the update is on screen (flip_done).
        for (pipe, target) in waits {
            super::vblank::wait_sequence(device, pipe, target);
        }
    }
    Ok(())
}

// ---- ioctl handlers ----------------------------------------------------------

pub(crate) struct Caller<'a> {
    pub device: &'a Arc<Device>,
    pub file: u64,
    pub atomic: bool,
    pub universal_planes: bool,
    pub handles: &'a crate::sync::SpinLock<super::gem::Handles>,
}

pub(crate) fn get_resources(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let pipes = c.device.config.pipes.len();
    let fbs: Vec<u32> = c
        .device
        .kms
        .lock()
        .fbs
        .values()
        .filter(|fb| fb.owner == c.file)
        .map(|fb| fb.id)
        .collect();
    let crtcs: Vec<u32> = (0..pipes).map(|p| object_id(p, KIND_CRTC)).collect();
    let connectors: Vec<u32> = (0..pipes).map(|p| object_id(p, KIND_CONNECTOR)).collect();
    let encoders: Vec<u32> = (0..pipes).map(|p| object_id(p, KIND_ENCODER)).collect();
    for (pointer, count, values) in [(0, 32, &fbs), (8, 36, &crtcs), (16, 40, &connectors), (24, 44, &encoders)] {
        ioctl::copy_out_u32(arg.u64(pointer), arg.u32(count), values)?;
        arg.set_u32(count, values.len() as u32);
    }
    let (min, max) = (c.device.config.min_size, c.device.config.max_size);
    arg.set_u32(48, min.0);
    arg.set_u32(52, max.0);
    arg.set_u32(56, min.1);
    arg.set_u32(60, max.1);
    Ok(())
}

pub(crate) fn get_crtc(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let (pipe, kind) = decode_object(c.device, arg.u32(12)).ok_or(VfsError::NotFound)?;
    if kind != KIND_CRTC {
        return Err(VfsError::NotFound);
    }
    let state = c.device.kms.lock();
    let primary = &state.config.planes[2 * pipe];
    arg.set_u32(16, primary.fb.as_ref().map_or(0, |fb| fb.id));
    arg.set_u32(20, primary.src[0] >> 16);
    arg.set_u32(24, primary.src[1] >> 16);
    arg.set_u32(28, 0); // gamma_size
    match state.mode(pipe) {
        Some(mode) if state.active(pipe) || state.config.crtcs[pipe].mode.is_some() => {
            arg.set_u32(32, 1);
            arg.data[36..104].copy_from_slice(&mode.encode());
        }
        _ => {
            arg.set_u32(32, 0);
            arg.data[36..104].fill(0);
        }
    }
    Ok(())
}

pub(crate) fn get_encoder(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let (pipe, kind) = decode_object(c.device, arg.u32(0)).ok_or(VfsError::NotFound)?;
    if kind != KIND_ENCODER {
        return Err(VfsError::NotFound);
    }
    let routed = c.device.kms.lock().config.connectors[pipe].crtc;
    arg.set_u32(4, 0); // DRM_MODE_ENCODER_NONE, as simpledrm
    arg.set_u32(8, routed.map_or(0, |p| object_id(p, KIND_CRTC)));
    arg.set_u32(12, 1 << pipe);
    arg.set_u32(16, 0);
    Ok(())
}

pub(crate) fn get_connector(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let (pipe, kind) = decode_object(c.device, arg.u32(48)).ok_or(VfsError::NotFound)?;
    if kind != KIND_CONNECTOR {
        return Err(VfsError::NotFound);
    }
    let info = &c.device.config.pipes[pipe];
    let encoders = [object_id(pipe, KIND_ENCODER)];
    ioctl::copy_out_u32(arg.u64(0), arg.u32(40), &encoders)?;
    arg.set_u32(40, 1);
    if arg.u64(8) != 0 && arg.u32(32) as usize >= info.modes.len() {
        let bytes: Vec<u8> = info.modes.iter().flat_map(|m| m.encode()).collect();
        crate::user::uaccess::write(arg.u64(8), &bytes)?;
    }
    arg.set_u32(32, info.modes.len() as u32);
    let props = object_props(c.device, pipe, KIND_CONNECTOR, c.atomic);
    let (ids, values) = {
        let state = c.device.kms.lock();
        let ids: Vec<u32> = props.iter().map(|p| *p as u32).collect();
        let values: Vec<u64> = props.iter().map(|p| prop_value(&state, pipe, KIND_CONNECTOR, *p)).collect();
        (ids, values)
    };
    ioctl::copy_out_u32(arg.u64(16), arg.u32(36), &ids)?;
    ioctl::copy_out_u64(arg.u64(24), arg.u32(36), &values)?;
    arg.set_u32(36, ids.len() as u32);
    let routed = c.device.kms.lock().config.connectors[pipe].crtc;
    arg.set_u32(44, if routed.is_some() { encoders[0] } else { 0 });
    arg.set_u32(52, info.connector_type as u32);
    arg.set_u32(56, 1);
    arg.set_u32(60, 1); // connected
    arg.set_u32(64, info.width_mm);
    arg.set_u32(68, info.height_mm);
    arg.set_u32(72, 1); // subpixel unknown
    Ok(())
}

pub(crate) fn get_plane_resources(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let mut planes = Vec::new();
    if c.universal_planes {
        for (pipe, info) in c.device.config.pipes.iter().enumerate() {
            planes.push(object_id(pipe, KIND_PRIMARY));
            if info.cursor.is_some() {
                planes.push(object_id(pipe, KIND_CURSOR));
            }
        }
    }
    ioctl::copy_out_u32(arg.u64(0), arg.u32(8), &planes)?;
    arg.set_u32(8, planes.len() as u32);
    Ok(())
}

pub(crate) fn get_plane(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let (pipe, kind) = decode_object(c.device, arg.u32(0)).ok_or(VfsError::NotFound)?;
    if kind != KIND_PRIMARY && kind != KIND_CURSOR {
        return Err(VfsError::NotFound);
    }
    let (crtc, fb) = {
        let state = c.device.kms.lock();
        let plane = &state.config.planes[2 * pipe + (kind == KIND_CURSOR) as usize];
        (
            plane.crtc.map_or(0, |p| object_id(p, KIND_CRTC)),
            plane.fb.as_ref().map_or(0, |fb| fb.id),
        )
    };
    arg.set_u32(4, crtc);
    arg.set_u32(8, fb);
    arg.set_u32(12, 1 << pipe);
    arg.set_u32(16, 0);
    let formats = if kind == KIND_PRIMARY {
        c.device.config.pipes[pipe].formats.clone()
    } else {
        vec![super::fourcc::ARGB8888]
    };
    ioctl::copy_out_u32(arg.u64(24), arg.u32(20), &formats)?;
    arg.set_u32(20, formats.len() as u32);
    Ok(())
}

pub(crate) fn get_property(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let prop = Prop::from_id(arg.u32(16)).ok_or(VfsError::NotFound)?;
    if prop.flags() & PROP_ATOMIC != 0 && !c.atomic {
        return Err(VfsError::NotFound);
    }
    let mut name = [0u8; 32];
    name[..prop.name().len()].copy_from_slice(prop.name().as_bytes());
    arg.data[24..56].copy_from_slice(&name);
    arg.set_u32(20, prop.flags());
    let values = prop.values();
    let enums = prop.enums();
    if prop.flags() & PROP_ENUM != 0 {
        ioctl::copy_out_u64(arg.u64(0), arg.u32(56), &values)?;
        if arg.u64(8) != 0 && arg.u32(60) as usize >= enums.len() {
            let mut bytes = Vec::new();
            for (value, text) in enums {
                bytes.extend_from_slice(&value.to_le_bytes());
                let mut field = [0u8; 32];
                field[..text.len()].copy_from_slice(text.as_bytes());
                bytes.extend_from_slice(&field);
            }
            crate::user::uaccess::write(arg.u64(8), &bytes)?;
        }
        arg.set_u32(56, values.len() as u32);
        arg.set_u32(60, enums.len() as u32);
    } else {
        ioctl::copy_out_u64(arg.u64(0), arg.u32(56), &values)?;
        arg.set_u32(56, values.len() as u32);
        arg.set_u32(60, 0);
    }
    Ok(())
}

pub(crate) fn obj_get_properties(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let (pipe, kind) = decode_object(c.device, arg.u32(20)).ok_or(VfsError::NotFound)?;
    let expected = match kind {
        KIND_CONNECTOR => OBJ_CONNECTOR,
        KIND_ENCODER => OBJ_ENCODER,
        KIND_CRTC => OBJ_CRTC,
        _ => OBJ_PLANE,
    };
    if arg.u32(24) != 0 && arg.u32(24) != expected {
        return Err(VfsError::NotFound);
    }
    let props = object_props(c.device, pipe, kind, c.atomic);
    let (ids, values): (Vec<u32>, Vec<u64>) = {
        let state = c.device.kms.lock();
        props.iter().map(|p| (*p as u32, prop_value(&state, pipe, kind, *p))).unzip()
    };
    ioctl::copy_out_u32(arg.u64(0), arg.u32(16), &ids)?;
    ioctl::copy_out_u64(arg.u64(8), arg.u32(16), &values)?;
    arg.set_u32(16, ids.len() as u32);
    Ok(())
}

pub(crate) fn get_blob(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let blob = c.device.kms.lock().blobs.get(&arg.u32(0)).cloned().ok_or(VfsError::NotFound)?;
    if arg.u64(8) != 0 && arg.u32(4) as usize >= blob.data.len() && !blob.data.is_empty() {
        crate::user::uaccess::write(arg.u64(8), &blob.data)?;
    }
    arg.set_u32(4, blob.data.len() as u32);
    Ok(())
}

pub(crate) fn create_blob(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let length = arg.u32(8) as usize;
    if length == 0 || length > 1 << 20 {
        return Err(VfsError::InvalidArgument);
    }
    let data = crate::user::uaccess::read_vec(arg.u64(0), length, length)?;
    let id = c.device.kms.lock().add_blob(data, c.file);
    arg.set_u32(12, id);
    Ok(())
}

pub(crate) fn destroy_blob(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let mut state = c.device.kms.lock();
    match state.blobs.get(&arg.u32(0)) {
        Some(blob) if blob.owner == c.file => {
            state.blobs.remove(&arg.u32(0));
            Ok(())
        }
        Some(_) => Err(VfsError::PermissionDenied),
        None => Err(VfsError::NotFound),
    }
}

fn add_fb(c: &Caller, width: u32, height: u32, format: u32, handle: u32, pitch: u32, offset: u32) -> Result<u32, VfsError> {
    let (min, max) = (c.device.config.min_size, c.device.config.max_size);
    let cpp = super::fourcc::cpp(format).ok_or(VfsError::InvalidArgument)?;
    if width < min.0 || width > max.0 || height < min.1 || height > max.1 {
        return Err(VfsError::InvalidArgument);
    }
    let object = c.handles.lock().get(handle)?;
    let row = width as u64 * cpp as u64;
    let end = offset as u64 + pitch as u64 * (height as u64 - 1) + row;
    if (pitch as u64) < row || end > object.size {
        return Err(VfsError::InvalidArgument);
    }
    let mut state = c.device.kms.lock();
    let id = state.next_fb;
    state.next_fb += 1;
    state.fbs.insert(
        id,
        Arc::new(Framebuffer {
            id,
            owner: c.file,
            object,
            width,
            height,
            format,
            pitch,
            offset,
        }),
    );
    Ok(id)
}

pub(crate) fn addfb(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let format = super::fourcc::from_legacy(arg.u32(16), arg.u32(20)).ok_or(VfsError::InvalidArgument)?;
    let id = add_fb(c, arg.u32(4), arg.u32(8), format, arg.u32(24), arg.u32(12), 0)?;
    arg.set_u32(0, id);
    Ok(())
}

pub(crate) fn addfb2(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    const FB_MODIFIERS: u32 = 2;
    let flags = arg.u32(16);
    if flags & !FB_MODIFIERS != 0 {
        return Err(VfsError::InvalidArgument);
    }
    // Single-plane linear formats only.
    if (1..4).any(|i| arg.u32(20 + 4 * i) != 0) || (flags & FB_MODIFIERS != 0 && arg.u64(72) != 0) {
        return Err(VfsError::InvalidArgument);
    }
    let id = add_fb(c, arg.u32(4), arg.u32(8), arg.u32(12), arg.u32(20), arg.u32(36), arg.u32(52))?;
    arg.set_u32(0, id);
    Ok(())
}

/// Remove framebuffers; a pipe still scanning one out is switched off, as
/// Linux's drm_framebuffer_remove does.
pub(crate) fn remove_fbs(device: &Arc<Device>, ids: &[u32]) {
    let removed: Vec<Arc<Framebuffer>> = {
        let mut state = device.kms.lock();
        ids.iter().filter_map(|id| state.fbs.remove(id)).collect()
    };
    if removed.is_empty() {
        return;
    }
    device.modeset(|| {
        let mut state = device.kms.lock();
        let mut off = Vec::new();
        for pipe in 0..device.config.pipes.len() {
            for index in [2 * pipe, 2 * pipe + 1] {
                let uses = state.config.planes[index]
                    .fb
                    .as_ref()
                    .is_some_and(|fb| removed.iter().any(|r| Arc::ptr_eq(r, fb)));
                if uses {
                    state.config.planes[index].fb = None;
                    state.config.planes[index].crtc = None;
                    if index == 2 * pipe {
                        state.config.crtcs[pipe] = CrtcState::default();
                        off.push(pipe);
                    }
                }
            }
        }
        let any_active = state.config.crtcs.iter().any(|c| c.active);
        drop(state);
        for pipe in off {
            let _ = device.driver.set_mode(pipe, None);
            super::vblank::crtc_off(device, pipe);
        }
        if device.config.takes_over_console && !any_active {
            crate::tty::vt::release_scanout();
        }
    });
}

pub(crate) fn rmfb(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let id = arg.u32(0);
    match c.device.kms.lock().fbs.get(&id) {
        Some(fb) if fb.owner == c.file => {}
        Some(_) => return Err(VfsError::PermissionDenied),
        None => return Err(VfsError::NotFound),
    }
    remove_fbs(c.device, &[id]);
    Ok(())
}

pub(crate) fn get_fb(c: &Caller, arg: &mut Ioctl, v2: bool) -> Result<(), VfsError> {
    let fb = c.device.kms.lock().fbs.get(&arg.u32(0)).cloned().ok_or(VfsError::NotFound)?;
    let handle = c.handles.lock().insert(fb.object.clone());
    arg.set_u32(4, fb.width);
    arg.set_u32(8, fb.height);
    if v2 {
        arg.set_u32(12, fb.format);
        arg.set_u32(16, 0);
        arg.data[20..104].fill(0);
        arg.set_u32(20, handle);
        arg.set_u32(36, fb.pitch);
        arg.set_u32(52, fb.offset);
    } else {
        let cpp = super::fourcc::cpp(fb.format).unwrap_or(4);
        let depth = match fb.format {
            super::fourcc::ARGB8888 => 32,
            super::fourcc::XRGB2101010 => 30,
            super::fourcc::RGB565 => 16,
            _ => 24,
        };
        arg.set_u32(12, fb.pitch);
        arg.set_u32(16, cpp * 8);
        arg.set_u32(20, depth);
        arg.set_u32(24, handle);
    }
    Ok(())
}

/// Legacy SETCRTC as an atomic commit with ALLOW_MODESET.
pub(crate) fn set_crtc(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let crtc_id = arg.u32(12);
    let (pipe, kind) = decode_object(c.device, crtc_id).ok_or(VfsError::NotFound)?;
    if kind != KIND_CRTC {
        return Err(VfsError::NotFound);
    }
    let fb_id = arg.u32(16);
    let mode_valid = arg.u32(32) != 0;
    let connector = object_id(pipe, KIND_CONNECTOR);
    let crtc = object_id(pipe, KIND_CRTC);
    let primary = object_id(pipe, KIND_PRIMARY);
    let change = |object, prop: Prop, value| Change { object, prop: prop as u32, value };
    if !mode_valid {
        let changes = [
            change(primary, Prop::FbId, 0),
            change(primary, Prop::PlaneCrtc, 0),
            change(crtc, Prop::Active, 0),
            change(crtc, Prop::ModeId, 0),
            change(connector, Prop::ConnectorCrtc, 0),
        ];
        return commit(c.device, c.file, &changes, ATOMIC_ALLOW_MODESET, 0);
    }
    let mode = Mode::decode(&arg.data[36..104]).ok_or(VfsError::InvalidArgument)?;
    let connectors = ioctl::read_u32s(arg.u64(0), arg.u32(8) as usize)?;
    if connectors.iter().any(|&id| id != connector) {
        return Err(VfsError::InvalidArgument);
    }
    let fb = if fb_id == u32::MAX {
        let state = c.device.kms.lock();
        state.config.planes[2 * pipe].fb.as_ref().map_or(0, |fb| fb.id)
    } else {
        fb_id
    };
    let blob = c.device.kms.lock().add_blob(mode.encode().to_vec(), c.file);
    let (x, y) = (arg.u32(20), arg.u32(24));
    let changes = [
        change(crtc, Prop::ModeId, blob as u64),
        change(crtc, Prop::Active, 1),
        change(connector, Prop::ConnectorCrtc, crtc as u64),
        change(primary, Prop::FbId, fb as u64),
        change(primary, Prop::PlaneCrtc, if fb != 0 { crtc as u64 } else { 0 }),
        change(primary, Prop::SrcX, (x as u64) << 16),
        change(primary, Prop::SrcY, (y as u64) << 16),
        change(primary, Prop::SrcW, (mode.hdisplay as u64) << 16),
        change(primary, Prop::SrcH, (mode.vdisplay as u64) << 16),
        change(primary, Prop::CrtcX, 0),
        change(primary, Prop::CrtcY, 0),
        change(primary, Prop::CrtcW, mode.hdisplay as u64),
        change(primary, Prop::CrtcH, mode.vdisplay as u64),
    ];
    // The blob stays registered (owned by this file, freed on close) so
    // MODE_ID keeps reporting its id.
    commit(c.device, c.file, &changes, ATOMIC_ALLOW_MODESET, 0)
}

/// Legacy PAGE_FLIP: swap the primary plane's framebuffer at next vblank.
pub(crate) fn page_flip(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    const FLIP_EVENT: u32 = 1;
    const FLIP_ASYNC: u32 = 2;
    let flags = arg.u32(8);
    if flags & !(FLIP_EVENT | FLIP_ASYNC | 4 | 8) != 0 || flags & FLIP_ASYNC != 0 {
        return Err(VfsError::InvalidArgument);
    }
    let (pipe, kind) = decode_object(c.device, arg.u32(0)).ok_or(VfsError::NotFound)?;
    if kind != KIND_CRTC {
        return Err(VfsError::NotFound);
    }
    {
        let state = c.device.kms.lock();
        let current = state.config.planes[2 * pipe].fb.clone().ok_or(VfsError::Busy)?;
        let next = state.fbs.get(&arg.u32(4)).ok_or(VfsError::NotFound)?;
        if next.format != current.format || next.width != current.width || next.height != current.height {
            return Err(VfsError::InvalidArgument);
        }
    }
    let primary = object_id(pipe, KIND_PRIMARY);
    let changes = [Change { object: primary, prop: Prop::FbId as u32, value: arg.u32(4) as u64 }];
    let event = if flags & FLIP_EVENT != 0 { ATOMIC_PAGE_FLIP_EVENT } else { 0 };
    if c.device.vblank[pipe].flip_pending() {
        return Err(VfsError::Busy);
    }
    commit(c.device, c.file, &changes, event | ATOMIC_NONBLOCK, arg.u64(16))
}

/// DIRTYFB: present damaged regions of `fb` wherever it is scanned out.
/// The hot path of shadow-buffered clients (Xorg), so it goes straight to
/// the driver with coalesced clips instead of building an atomic state.
pub(crate) fn dirty_fb(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let fb_id = arg.u32(0);
    let count = arg.u32(12) as usize;
    if count > 256 * 1024 {
        return Err(VfsError::InvalidArgument);
    }
    let raw = if count == 0 {
        Vec::new()
    } else {
        crate::user::uaccess::read_vec(arg.u64(16), count * 8, count * 8)?
    };
    let clips: Vec<Rect> = raw
        .chunks_exact(8)
        .map(|c| {
            let v = |o: usize| u16::from_le_bytes([c[o], c[o + 1]]) as i32;
            Rect { x1: v(0), y1: v(2), x2: v(4), y2: v(6) }
        })
        .collect();
    c.device.modeset(|| {
        let state = c.device.kms.lock();
        state.fbs.get(&fb_id).ok_or(VfsError::NotFound)?;
        let targets: Vec<(usize, PlaneState)> = (0..c.device.config.pipes.len())
            .filter(|&p| state.config.crtcs[p].active)
            .filter_map(|p| {
                let plane = &state.config.planes[2 * p];
                plane
                    .fb
                    .as_ref()
                    .is_some_and(|fb| fb.id == fb_id)
                    .then(|| (p, plane.clone()))
            })
            .collect();
        drop(state);
        for (pipe, plane) in targets {
            let (source, _) = plane_rects(&plane);
            let damage = if clips.is_empty() { vec![source] } else { coalesce(clips.clone(), &source) };
            if !damage.is_empty() {
                c.device.driver.update_plane(pipe, &update_for(&plane, &damage))?;
            }
        }
        Ok(())
    })
}

/// Legacy CURSOR/CURSOR2 for drivers with a cursor plane; ENXIO makes Xorg
/// use its software cursor otherwise.
pub(crate) fn cursor(c: &Caller, arg: &mut Ioctl, v2: bool) -> Result<(), VfsError> {
    const CURSOR_BO: u32 = 1;
    const CURSOR_MOVE: u32 = 2;
    let (pipe, kind) = decode_object(c.device, arg.u32(4)).ok_or(VfsError::NotFound)?;
    if kind != KIND_CRTC {
        return Err(VfsError::NotFound);
    }
    let Some((max_w, max_h)) = c.device.config.pipes[pipe].cursor else {
        return Err(VfsError::NoSuchDeviceOrAddress);
    };
    let flags = arg.u32(0);
    let plane = object_id(pipe, KIND_CURSOR);
    let crtc = object_id(pipe, KIND_CRTC);
    let mut changes = Vec::new();
    if flags & CURSOR_BO != 0 {
        let handle = arg.u32(24);
        let fb = if handle == 0 {
            0
        } else {
            let (w, h) = (arg.u32(16), arg.u32(20));
            if w == 0 || h == 0 || w > max_w || h > max_h {
                return Err(VfsError::InvalidArgument);
            }
            add_fb(c, w, h, super::fourcc::ARGB8888, handle, w * 4, 0)?
        };
        if v2 {
            c.device.kms.lock().cursor_hot[pipe] = (arg.i32(28), arg.i32(32));
        }
        let (w, h) = if fb == 0 { (0, 0) } else { (arg.u32(16), arg.u32(20)) };
        changes.extend([
            Change { object: plane, prop: Prop::FbId as u32, value: fb as u64 },
            Change { object: plane, prop: Prop::PlaneCrtc as u32, value: if fb != 0 { crtc as u64 } else { 0 } },
            Change { object: plane, prop: Prop::SrcW as u32, value: (w as u64) << 16 },
            Change { object: plane, prop: Prop::SrcH as u32, value: (h as u64) << 16 },
            Change { object: plane, prop: Prop::CrtcW as u32, value: w as u64 },
            Change { object: plane, prop: Prop::CrtcH as u32, value: h as u64 },
        ]);
    }
    if flags & CURSOR_MOVE != 0 {
        changes.push(Change { object: plane, prop: Prop::CrtcX as u32, value: arg.i32(8) as i64 as u64 });
        changes.push(Change { object: plane, prop: Prop::CrtcY as u32, value: arg.i32(12) as i64 as u64 });
    }
    commit(c.device, c.file, &changes, ATOMIC_NONBLOCK, 0)
}

/// OBJ_SETPROPERTY / legacy connector SETPROPERTY (DPMS maps to ACTIVE).
pub(crate) fn set_property(c: &Caller, object: u32, prop: u32, value: u64) -> Result<(), VfsError> {
    let (pipe, kind) = decode_object(c.device, object).ok_or(VfsError::NotFound)?;
    if kind == KIND_CONNECTOR && prop == Prop::Dpms as u32 {
        if value > 3 {
            return Err(VfsError::InvalidArgument);
        }
        let has_mode = c.device.kms.lock().config.crtcs[pipe].mode.is_some();
        let mut changes = vec![Change { object, prop, value }];
        if has_mode {
            changes.push(Change {
                object: object_id(pipe, KIND_CRTC),
                prop: Prop::Active as u32,
                value: (value == 0) as u64,
            });
        }
        return commit(c.device, c.file, &changes, ATOMIC_ALLOW_MODESET, 0);
    }
    commit(c.device, c.file, &[Change { object, prop, value }], ATOMIC_ALLOW_MODESET, 0)
}

/// DRM_IOCTL_MODE_ATOMIC.
pub(crate) fn atomic(c: &Caller, arg: &mut Ioctl) -> Result<(), VfsError> {
    let flags = arg.u32(0);
    let known = ATOMIC_PAGE_FLIP_EVENT | ATOMIC_TEST_ONLY | ATOMIC_NONBLOCK | ATOMIC_ALLOW_MODESET;
    if !c.atomic || flags & !known != 0 || arg.u64(40) != 0 {
        return Err(VfsError::InvalidArgument);
    }
    if flags & ATOMIC_TEST_ONLY != 0 && flags & ATOMIC_PAGE_FLIP_EVENT != 0 {
        return Err(VfsError::InvalidArgument);
    }
    let objects = ioctl::read_u32s(arg.u64(8), arg.u32(4) as usize)?;
    let counts = ioctl::read_u32s(arg.u64(16), objects.len())?;
    let total: usize = counts.iter().map(|&n| n as usize).sum();
    let props = ioctl::read_u32s(arg.u64(24), total)?;
    let values = ioctl::read_u64s(arg.u64(32), total)?;
    let mut changes = Vec::with_capacity(total);
    let mut index = 0;
    for (object, count) in objects.iter().zip(counts) {
        for _ in 0..count {
            changes.push(Change { object: *object, prop: props[index], value: values[index] });
            index += 1;
        }
    }
    commit(c.device, c.file, &changes, flags, arg.u64(48))
}

/// Drop everything a closing file owns.
pub(crate) fn close_file(device: &Arc<Device>, file: u64) {
    let fbs: Vec<u32> = device
        .kms
        .lock()
        .fbs
        .values()
        .filter(|fb| fb.owner == file)
        .map(|fb| fb.id)
        .collect();
    remove_fbs(device, &fbs);
    device.kms.lock().blobs.retain(|_, blob| blob.owner != file);
}
