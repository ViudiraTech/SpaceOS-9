/*
 *       src/gpu/kms.rs
 *       KMS objects, atomic validation and asynchronous scanout commits
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
use super::*;
#[derive(Clone)]
pub struct Output {
    pub width: u32,
    pub height: u32,
    pub connected: bool,
    pub edid: Vec<u8>,
}
#[derive(Clone)]
pub struct Framebuffer {
    pub id: u32,
    pub owner: u64,
    pub resource: Arc<Resource>,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub format: u32,
    pub offset: u32,
}

#[derive(Clone, Copy)]
struct Clip {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}
#[derive(Clone)]
struct Config {
    crtc: [u64; 18],
    plane: [u64; 18],
    connector: [u64; 18],
    framebuffer: Option<Framebuffer>,
    mode: Vec<u8>,
}
impl Config {
    fn new() -> Self {
        Self {
            crtc: [0; 18],
            plane: [0; 18],
            connector: [0; 18],
            framebuffer: None,
            mode: Vec::new(),
        }
    }
}
struct Commit {
    configs: Vec<Config>,
    touched: Vec<usize>,
    wait: Vec<Arc<syncobj::Fence>>,
    completion: Arc<syncobj::Fence>,
    events: Arc<SpinLock<VecDeque<Vec<u8>>>>,
    event: bool,
    user_data: u64,
}
pub struct DisplayState {
    configs: Vec<Config>,
    fbs: BTreeMap<u32, Framebuffer>,
    next_fb: u32,
    pending: VecDeque<Commit>,
    sequence: u32,
    pub(super) generation: u64,
    in_progress: bool,
    pub blobs: BTreeMap<u32, Vec<u8>>,
    next_blob: u32,
    blob_owners: BTreeMap<u32, u64>,
}
impl DisplayState {
    pub fn new() -> Self {
        Self {
            configs: Vec::new(),
            fbs: BTreeMap::new(),
            next_fb: 0x10000,
            pending: VecDeque::new(),
            sequence: 0,
            generation: 0,
            in_progress: false,
            blobs: BTreeMap::new(),
            next_blob: 0x100000,
            blob_owners: BTreeMap::new(),
        }
    }
}
const FORMATS: &[u32] = &[0x34325258, 0x34325241];
fn mode(output: &Output) -> Vec<u8> {
    let mut bytes = vec![0; 68];
    let w = output.width as u16;
    let h = output.height as u16;
    put32(
        &mut bytes,
        0,
        (w as u32 + 160) * (h as u32 + 45) * 60 / 1000,
    );
    for (o, v) in [
        (4, w),
        (6, w + 16),
        (8, w + 48),
        (10, w + 160),
        (12, 0),
        (14, h),
        (16, h + 3),
        (18, h + 8),
        (20, h + 45),
        (22, 0),
    ] {
        bytes[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    put32(&mut bytes, 24, 60);
    put32(&mut bytes, 32, 72);
    let name = format!("{}x{}", w, h);
    bytes[36..36 + name.len()].copy_from_slice(name.as_bytes());
    bytes
}
fn id(kind: u32, index: usize) -> u32 {
    kind + index as u32
}
fn object(gpu: &Gpu, value: u32) -> Result<(u32, usize), VfsError> {
    for kind in [0x100, 0x200, 0x300, 0x400] {
        if value >= kind && value < kind + gpu.outputs.lock().len() as u32 {
            return Ok((kind, (value - kind) as usize));
        }
    }
    Err(VfsError::NotFound)
}
fn count_out(
    arg: &mut Ioctl,
    pointer: usize,
    count: usize,
    values: &[u32],
) -> Result<(), VfsError> {
    let capacity = arg.r32(count)? as usize;
    if capacity >= values.len() && !values.is_empty() {
        write32(arg.r64(pointer)?, values)?;
    }
    arg.w32(count, values.len() as u32);
    Ok(())
}
pub fn discover(gpu: &Arc<Gpu>) -> Result<(), VfsError> {
    let count = gpu.transport.config32(8).min(16) as usize;
    if count == 0 {
        return Err(VfsError::NoDevice);
    }
    let reply = gpu.call(0x100, 0, &[], 24 + 16 * 24)?;
    let mut outputs = Vec::new();
    for index in 0..count {
        let o = 24 + index * 24;
        let mut width = u32_at(&reply, o + 8)?;
        let mut height = u32_at(&reply, o + 12)?;
        let connected = u32_at(&reply, o + 16)? != 0;
        if width == 0 || width > 8192 {
            width = 1024;
        }
        if height == 0 || height > 8192 {
            height = 768;
        }
        let edid = if gpu.transport.features & 2 != 0 {
            match gpu.call(0x10a, 0, &[index as u32, 0], 1056) {
                Ok(reply) => {
                    let size = u32_at(&reply, 24)? as usize;
                    if size > 1024 || size % 128 != 0 || 32 + size > reply.len() {
                        return Err(VfsError::IoError);
                    }
                    reply[32..32 + size].to_vec()
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        outputs.push(Output {
            width,
            height,
            connected,
            edid,
        });
    }
    gpu.displays.lock().configs = (0..count).map(|_| Config::new()).collect();
    *gpu.outputs.lock() = outputs;
    Ok(())
}
fn master(session: &Session) -> Result<(), VfsError> {
    if session.gpu.master.load(Ordering::Acquire) == session.id {
        Ok(())
    } else {
        Err(VfsError::PermissionDenied)
    }
}
fn fb(gpu: &Gpu, state: &SessionState, id: u32) -> Result<Framebuffer, VfsError> {
    state
        .framebuffers
        .get(&id)
        .cloned()
        .or_else(|| gpu.displays.lock().fbs.get(&id).cloned())
        .ok_or(VfsError::NotFound)
}
fn addfb(
    session: &Session,
    state: &mut SessionState,
    arg: &mut Ioctl,
    modern: bool,
) -> Result<(), VfsError> {
    let (width, height, pitch, handle, format, offset) = if modern {
        if arg.r32(16)? & !2 != 0
            || arg.data[24..36].iter().any(|&b| b != 0)
            || arg.data[40..52].iter().any(|&b| b != 0)
            || arg.data[56..].iter().any(|&b| b != 0)
        {
            return Err(VfsError::InvalidArgument);
        }
        (
            arg.r32(4)?,
            arg.r32(8)?,
            arg.r32(36)?,
            arg.r32(20)?,
            arg.r32(12)?,
            arg.r32(52)?,
        )
    } else {
        if arg.r32(16)? != 32 || ![24, 32].contains(&arg.r32(20)?) {
            return Err(VfsError::InvalidArgument);
        }
        (
            arg.r32(4)?,
            arg.r32(8)?,
            arg.r32(12)?,
            arg.r32(24)?,
            if arg.r32(20)? == 24 {
                FORMATS[0]
            } else {
                FORMATS[1]
            },
            0,
        )
    };
    let resource = state.resource(handle)?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || !FORMATS.contains(&format)
        || pitch < width.checked_mul(4).ok_or(VfsError::InvalidArgument)?
        || offset as u64 + (height as u64 - 1) * pitch as u64 + width as u64 * 4 > resource.size
    {
        return Err(VfsError::InvalidArgument);
    }
    if resource.blob_mem == 0
        && (resource.width != width || pitch != resource.stride || offset != 0)
    {
        return Err(VfsError::InvalidArgument);
    }
    let mut display = session.gpu.displays.lock();
    if display.fbs.len() >= 4096 {
        return Err(VfsError::NoSpace);
    }
    let id = display.next_fb;
    display.next_fb = display.next_fb.checked_add(1).ok_or(VfsError::NoSpace)?;
    let framebuffer = Framebuffer {
        id,
        owner: session.id,
        resource,
        width,
        height,
        pitch,
        format,
        offset,
    };
    display.fbs.insert(id, framebuffer.clone());
    state.framebuffers.insert(id, framebuffer);
    arg.w32(0, id);
    Ok(())
}
fn props(gpu: &Gpu, state: &SessionState, obj: u32) -> Result<(Vec<u32>, Vec<u64>), VfsError> {
    let (kind, index) = object(gpu, obj)?;
    let d = gpu.displays.lock();
    let c = &d.configs[index];
    let ids: Vec<u32> = match kind {
        0x100 if state.atomic => vec![1, 2, 15],
        0x100 => Vec::new(),
        0x200 if state.atomic => vec![3, 16, 17],
        0x200 => vec![16, 17],
        0x400 if state.atomic => vec![3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
        0x400 => vec![13],
        _ => Vec::new(),
    };
    let values = ids
        .iter()
        .map(|&p| match (kind, p) {
            (_, 13) => 1,
            (_, 14) => u64::MAX,
            (_, 15) => 0,
            (_, 16) => 0x800000 + index as u64,
            _ => match kind {
                0x100 => c.crtc[p as usize],
                0x200 => c.connector[p as usize],
                _ => c.plane[p as usize],
            },
        })
        .collect();
    Ok((ids, values))
}
fn property(arg: &mut Ioctl) -> Result<(), VfsError> {
    let prop = arg.r32(16)?;
    let (name, flags, values, enum_value) = match prop {
        1 => ("ACTIVE", 2 | 0x80000000, vec![0, 1], None),
        2 => ("MODE_ID", 16 | 0x80000000, Vec::new(), None),
        3 => ("CRTC_ID", 64 | 0x80000000, vec![0xcccccccc], None),
        4 => ("FB_ID", 64 | 0x80000000, vec![0xfbfbfbfb], None),
        5 => ("SRC_X", 2 | 0x80000000, vec![0, u32::MAX as u64], None),
        6 => ("SRC_Y", 2 | 0x80000000, vec![0, u32::MAX as u64], None),
        7 => ("SRC_W", 2 | 0x80000000, vec![0, u32::MAX as u64], None),
        8 => ("SRC_H", 2 | 0x80000000, vec![0, u32::MAX as u64], None),
        9 => (
            "CRTC_X",
            128 | 0x80000000,
            vec![i32::MIN as i64 as u64, i32::MAX as u64],
            None,
        ),
        10 => (
            "CRTC_Y",
            128 | 0x80000000,
            vec![i32::MIN as i64 as u64, i32::MAX as u64],
            None,
        ),
        11 => ("CRTC_W", 2 | 0x80000000, vec![0, 8192], None),
        12 => ("CRTC_H", 2 | 0x80000000, vec![0, 8192], None),
        13 => ("type", 8 | 4, Vec::new(), Some((1u64, "Primary"))),
        14 => (
            "IN_FENCE_FD",
            128 | 0x80000000,
            vec![u64::MAX, i32::MAX as u64],
            None,
        ),
        15 => ("OUT_FENCE_PTR", 2 | 0x80000000, vec![0, u64::MAX], None),
        16 => ("EDID", 16 | 4, Vec::new(), None),
        17 => ("DPMS", 8, Vec::new(), Some((0, "On"))),
        _ => return Err(VfsError::NotFound),
    };
    let capacity = arg.r32(56)? as usize;
    if capacity >= values.len() && !values.is_empty() {
        write64(arg.r64(0)?, &values)?;
    }
    if let Some((value, name)) = enum_value {
        if arg.r32(60)? >= 1 {
            let mut bytes = vec![0; 40];
            put64(&mut bytes, 0, value);
            bytes[8..8 + name.len()].copy_from_slice(name.as_bytes());
            uaccess::write(arg.r64(8)?, &bytes)?;
        }
    }
    arg.w32(20, flags);
    arg.data[24..56].fill(0);
    arg.data[24..24 + name.len()].copy_from_slice(name.as_bytes());
    arg.w32(56, values.len() as u32);
    arg.w32(60, u32::from(enum_value.is_some()));
    Ok(())
}
fn validate(
    gpu: &Gpu,
    configs: &mut [Config],
    touched: &[usize],
    state: &SessionState,
) -> Result<(), VfsError> {
    let outputs = gpu.outputs.lock();
    for &index in touched {
        let c = &mut configs[index];
        if c.crtc[1] > 1 {
            return Err(VfsError::InvalidArgument);
        }
        if c.crtc[1] == 0 {
            if c.plane[4] != 0 {
                return Err(VfsError::InvalidArgument);
            }
            c.framebuffer = None;
            continue;
        }
        if !outputs[index].connected
            || c.connector[3] != id(0x100, index) as u64
            || c.plane[3] != id(0x100, index) as u64
        {
            return Err(VfsError::InvalidArgument);
        }
        let fb = fb(gpu, state, c.plane[4] as u32)?;
        let mode = gpu
            .displays
            .lock()
            .blobs
            .get(&(c.crtc[2] as u32))
            .cloned()
            .ok_or(VfsError::InvalidArgument)?;
        if mode.len() != 68 {
            return Err(VfsError::InvalidArgument);
        }
        let width = u16::from_le_bytes(mode[4..6].try_into().unwrap()) as u32;
        let height = u16::from_le_bytes(mode[14..16].try_into().unwrap()) as u32;
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || c.plane[9] != 0
            || c.plane[10] != 0
            || c.plane[11] != width as u64
            || c.plane[12] != height as u64
            || c.plane[7] != ((width as u64) << 16)
            || c.plane[8] != ((height as u64) << 16)
            || c.plane[5] & 65535 != 0
            || c.plane[6] & 65535 != 0
            || (c.plane[5] >> 16) + width as u64 > fb.width as u64
            || (c.plane[6] >> 16) + height as u64 > fb.height as u64
        {
            return Err(VfsError::InvalidArgument);
        }
        c.mode = mode.clone();
        c.framebuffer = Some(fb);
    }
    Ok(())
}
fn commit(
    session: &Session,
    state: &mut SessionState,
    configs: Vec<Config>,
    touched: Vec<usize>,
    wait: Vec<Arc<syncobj::Fence>>,
    flags: u32,
    data: u64,
    reserved: Option<Arc<syncobj::Fence>>,
) -> Result<Arc<syncobj::Fence>, VfsError> {
    let completion = reserved.unwrap_or_else(syncobj::Fence::pending);
    {
        let mut d = session.gpu.displays.lock();
        if !d.pending.is_empty() || d.in_progress {
            return Err(VfsError::Busy);
        }
        if flags & 1 != 0 && session.events.lock().len() + touched.len() > 256 {
            return Err(VfsError::NoSpace);
        }
        d.pending.push_back(Commit {
            configs,
            touched,
            wait,
            completion: completion.clone(),
            events: session.events.clone(),
            event: flags & 1 != 0,
            user_data: data,
        });
    }
    if flags & 0x200 == 0 {
        session.unlocked(state, || completion.wait(&session.gpu))?;
    }
    Ok(completion)
}
pub fn ioctl(
    session: &Session,
    state: &mut SessionState,
    nr: u8,
    arg: &mut Ioctl,
) -> Result<(), VfsError> {
    let gpu = &session.gpu;
    match nr {
        0xa0 => {
            let n = gpu.outputs.lock().len();
            let fbs: Vec<u32> = gpu.displays.lock().fbs.keys().copied().collect();
            count_out(arg, 0, 32, &fbs)?;
            for (pointer, count, kind) in [(8, 36, 0x100), (16, 40, 0x200), (24, 44, 0x300)] {
                count_out(
                    arg,
                    pointer,
                    count,
                    &(0..n).map(|i| id(kind, i)).collect::<Vec<_>>(),
                )?;
            }
            arg.w32(48, 1);
            arg.w32(52, 8192);
            arg.w32(56, 1);
            arg.w32(60, 8192);
            Ok(())
        }
        0xa1 => {
            let (kind, index) = object(gpu, arg.r32(12)?)?;
            if kind != 0x100 {
                return Err(VfsError::InvalidArgument);
            }
            let d = gpu.displays.lock();
            let c = &d.configs[index];
            arg.w32(16, c.plane[4] as u32);
            arg.w32(20, (c.plane[5] >> 16) as u32);
            arg.w32(24, (c.plane[6] >> 16) as u32);
            arg.w32(28, 0);
            arg.w32(32, c.crtc[1] as u32);
            if c.mode.len() == 68 {
                arg.data[36..104].copy_from_slice(&c.mode);
            }
            Ok(())
        }
        0xa6 => {
            let (kind, index) = object(gpu, arg.r32(0)?)?;
            if kind != 0x300 {
                return Err(VfsError::InvalidArgument);
            }
            arg.w32(4, 5);
            arg.w32(
                8,
                if gpu.displays.lock().configs[index].crtc[1] != 0 {
                    id(0x100, index)
                } else {
                    0
                },
            );
            arg.w32(12, 1 << index);
            arg.w32(16, 0);
            Ok(())
        }
        0xa7 => {
            let (kind, index) = object(gpu, arg.r32(48)?)?;
            if kind != 0x200 {
                return Err(VfsError::InvalidArgument);
            }
            let output = gpu.outputs.lock()[index].clone();
            if arg.r32(32)? >= 1 && output.connected {
                uaccess::write(arg.r64(8)?, &mode(&output))?;
            }
            arg.w32(32, u32::from(output.connected));
            let (props, values) = props(gpu, state, id(0x200, index))?;
            if arg.r32(36)? as usize >= props.len() {
                write32(arg.r64(16)?, &props)?;
                write64(arg.r64(24)?, &values)?;
            }
            arg.w32(36, props.len() as u32);
            count_out(arg, 0, 40, &[id(0x300, index)])?;
            arg.w32(44, id(0x300, index));
            arg.w32(52, 15);
            arg.w32(56, index as u32 + 1);
            arg.w32(60, if output.connected { 1 } else { 2 });
            arg.w32(64, 0);
            arg.w32(68, 0);
            arg.w32(72, 0);
            arg.w32(76, 0);
            Ok(())
        }
        0xaa => property(arg),
        0xac => {
            let id = arg.r32(0)?;
            let bytes = if id >= 0x800000 && id < 0x800000 + gpu.outputs.lock().len() as u32 {
                gpu.outputs.lock()[(id - 0x800000) as usize].edid.clone()
            } else {
                gpu.displays
                    .lock()
                    .blobs
                    .get(&id)
                    .cloned()
                    .ok_or(VfsError::NotFound)?
            };
            if arg.r32(4)? as usize >= bytes.len() && !bytes.is_empty() {
                uaccess::write(arg.r64(8)?, &bytes)?;
            }
            arg.w32(4, bytes.len() as u32);
            Ok(())
        }
        0xae | 0xb8 => addfb(session, state, arg, nr == 0xb8),
        0xaf => {
            let id = arg.r32(0)?;
            state.framebuffers.remove(&id).ok_or(VfsError::NotFound)?;
            gpu.displays.lock().fbs.remove(&id);
            Ok(())
        }
        0xad | 0xce => {
            let fb = fb(gpu, state, arg.r32(0)?)?;
            let handle = if state.framebuffers.contains_key(&fb.id)
                || gpu.master.load(Ordering::Acquire) == session.id
            {
                state.add(fb.resource.clone())?
            } else {
                0
            };
            arg.w32(4, fb.width);
            arg.w32(8, fb.height);
            if nr == 0xad {
                arg.w32(12, fb.pitch);
                arg.w32(16, 32);
                arg.w32(20, if fb.format == FORMATS[0] { 24 } else { 32 });
                arg.w32(24, handle);
            } else {
                arg.w32(12, fb.format);
                arg.w32(16, 0);
                arg.w32(20, handle);
                arg.w32(36, fb.pitch);
                arg.w32(52, fb.offset);
            }
            Ok(())
        }
        0xb2 => {
            if arg.r32(8)? != 32 || arg.r32(12)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let resource = gpu.create_2d(arg.r32(4)?, arg.r32(0)?)?;
            let pitch = resource.stride;
            let size = resource.size;
            let handle = state.add(resource)?;
            arg.w32(16, handle);
            arg.w32(20, pitch);
            arg.w64(24, size);
            Ok(())
        }
        0xb3 => {
            let resource = state.resource(arg.r32(0)?)?;
            if arg.r32(4)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            arg.w64(8, (resource.id as u64) << 32);
            Ok(())
        }
        0xb4 => {
            state
                .handles
                .remove(&arg.r32(0)?)
                .ok_or(VfsError::NotFound)?;
            Ok(())
        }
        0xb5 => {
            let count = if state.universal {
                gpu.outputs.lock().len()
            } else {
                0
            };
            count_out(
                arg,
                0,
                8,
                &(0..count).map(|i| id(0x400, i)).collect::<Vec<_>>(),
            )
        }
        0xb6 => {
            let (kind, index) = object(gpu, arg.r32(0)?)?;
            if kind != 0x400 {
                return Err(VfsError::InvalidArgument);
            }
            let d = gpu.displays.lock();
            let c = &d.configs[index];
            arg.w32(4, c.plane[3] as u32);
            arg.w32(8, c.plane[4] as u32);
            arg.w32(12, 1 << index);
            arg.w32(16, 0);
            count_out(arg, 24, 20, FORMATS)
        }
        0xb9 => {
            // struct drm_mode_obj_get_properties: props_ptr, prop_values_ptr,
            // count_props, obj_id, obj_type.
            let capacity = arg.r32(16)? as usize;
            let obj = arg.r32(20)?;
            let object_type = arg.r32(24)?;
            let (kind, _) = object(gpu, obj)?;
            let expected = match kind {
                0x100 => 0xcccccccc,
                0x200 => 0xc0c0c0c0,
                0x400 => 0xeeeeeeee,
                _ => return Err(VfsError::InvalidArgument),
            };
            if object_type != expected {
                return Err(VfsError::InvalidArgument);
            }
            let (props, values) = props(gpu, state, obj)?;
            if capacity >= props.len() {
                write32(arg.r64(0)?, &props)?;
                write64(arg.r64(8)?, &values)?;
            }
            arg.w32(16, props.len() as u32);
            Ok(())
        }
        0xbd => {
            let size = arg.r32(8)? as usize;
            if size == 0 || size > 65536 {
                return Err(VfsError::InvalidArgument);
            }
            let bytes = uaccess::read_vec(arg.r64(0)?, size, 65536)?;
            let mut d = gpu.displays.lock();
            let id = d.next_blob;
            d.next_blob = d.next_blob.checked_add(1).ok_or(VfsError::NoSpace)?;
            if d.blobs.len() >= 4096 {
                return Err(VfsError::NoSpace);
            }
            d.blobs.insert(id, bytes.clone());
            d.blob_owners.insert(id, session.id);
            state.blobs.insert(id, bytes);
            arg.w32(12, id);
            Ok(())
        }
        0xbe => {
            let id = arg.r32(0)?;
            state.blobs.remove(&id).ok_or(VfsError::NotFound)?;
            let mut d = gpu.displays.lock();
            d.blob_owners.remove(&id);
            collect(&mut d);
            Ok(())
        }
        0xbc => {
            master(session)?;
            if !state.atomic {
                return Err(VfsError::InvalidArgument);
            }
            let flags = arg.r32(0)?;
            if flags & !(1 | 0x100 | 0x200 | 0x400) != 0
                || flags & 0x100 != 0 && flags & (1 | 0x200) != 0
                || arg.r64(40)? != 0
            {
                return Err(VfsError::InvalidArgument);
            }
            let count = arg.r32(4)? as usize;
            if count == 0 || count > 64 {
                return Err(VfsError::InvalidArgument);
            }
            let objects = array32(arg.r64(8)?, count)?;
            let counts = array32(arg.r64(16)?, count)?;
            let total = counts
                .iter()
                .try_fold(0usize, |a, &b| a.checked_add(b as usize))
                .ok_or(VfsError::InvalidArgument)?;
            if total > 1024 {
                return Err(VfsError::InvalidArgument);
            }
            let properties = array32(arg.r64(24)?, total)?;
            let values = array64(arg.r64(32)?, total)?;
            let mut configs = gpu.displays.lock().configs.clone();
            let mut touched = Vec::new();
            let mut wait = Vec::new();
            let mut outptr = Vec::new();
            let mut cursor = 0;
            for (obj, count) in objects.into_iter().zip(counts) {
                let (kind, index) = object(gpu, obj)?;
                if kind == 0x300 {
                    return Err(VfsError::InvalidArgument);
                }
                if !touched.contains(&index) {
                    touched.push(index);
                }
                let mut seen = Vec::new();
                for _ in 0..count {
                    let p = properties[cursor] as usize;
                    let value = values[cursor];
                    cursor += 1;
                    if p == 0 || p > 17 || seen.contains(&p) {
                        return Err(VfsError::InvalidArgument);
                    }
                    seen.push(p);
                    let valid = match kind {
                        0x100 => matches!(p, 1 | 2 | 15),
                        0x200 => p == 3,
                        0x400 => matches!(p, 3..=12 | 14),
                        _ => false,
                    };
                    if !valid {
                        return Err(VfsError::InvalidArgument);
                    }
                    if p == 14 {
                        if value != u64::MAX {
                            wait.push(syncobj::fence_fd(value as u32)?);
                        }
                        continue;
                    }
                    if p == 15 {
                        if value != 0 {
                            uaccess::check(value, 4, true)?;
                            outptr.push(value);
                        }
                        continue;
                    }
                    let target = match kind {
                        0x100 => &mut configs[index].crtc,
                        0x200 => &mut configs[index].connector,
                        _ => &mut configs[index].plane,
                    };
                    if flags & 0x400 == 0 && matches!(kind, 0x100 | 0x200) && target[p] != value {
                        return Err(VfsError::InvalidArgument);
                    }
                    target[p] = value;
                }
            }
            validate(gpu, &mut configs, &touched, state)?;
            if flags & 0x100 != 0 {
                return Ok(());
            }
            let completion = syncobj::Fence::pending();
            let mut reserved = Vec::new();
            for ptr in outptr {
                let fd = match install_fd(
                    Arc::new(syncobj::SyncFile {
                        fence: completion.clone(),
                    }),
                    vfs::abi::O_CLOEXEC,
                ) {
                    Ok(fd) => fd,
                    Err(e) => {
                        for (fd, ptr) in reserved {
                            let _ = vfs::current_fd_table().lock().close(fd as usize);
                            let _ = uaccess::write_value(ptr, &-1i32);
                        }
                        return Err(e);
                    }
                };
                if let Err(e) = uaccess::write_value(ptr, &(fd as i32)) {
                    let _ = vfs::current_fd_table().lock().close(fd as usize);
                    for (fd, ptr) in reserved {
                        let _ = vfs::current_fd_table().lock().close(fd as usize);
                        let _ = uaccess::write_value(ptr, &-1i32);
                    }
                    return Err(e);
                }
                reserved.push((fd, ptr));
            }
            if let Err(e) = commit(
                session,
                state,
                configs,
                touched,
                wait,
                flags,
                arg.r64(48)?,
                Some(completion.clone()),
            ) {
                completion.complete(-5);
                for (fd, ptr) in reserved {
                    let _ = vfs::current_fd_table().lock().close(fd as usize);
                    let _ = uaccess::write_value(ptr, &-1i32);
                }
                return Err(e);
            }
            Ok(())
        }
        0xa2 => {
            master(session)?;
            let (kind, index) = object(gpu, arg.r32(12)?)?;
            if kind != 0x100 {
                return Err(VfsError::InvalidArgument);
            }
            let x = arg.r32(20)?;
            let y = arg.r32(24)?;
            let mut configs = gpu.displays.lock().configs.clone();
            if arg.r32(32)? == 0 {
                configs[index] = Config::new();
            } else {
                let connectors = array32(arg.r64(0)?, arg.r32(8)? as usize)?;
                if connectors != [id(0x200, index)] {
                    return Err(VfsError::InvalidArgument);
                }
                let framebuffer = fb(gpu, state, arg.r32(16)?)?;
                let mode_bytes = arg.data[36..104].to_vec();
                let mode_w = u16::from_le_bytes([mode_bytes[4], mode_bytes[5]]) as u64;
                let mode_h = u16::from_le_bytes([mode_bytes[14], mode_bytes[15]]) as u64;
                let blob = {
                    let mut d = gpu.displays.lock();
                    let id = d.next_blob;
                    d.next_blob += 1;
                    d.blobs.insert(id, mode_bytes.clone());
                    d.blob_owners.insert(id, session.id);
                    id
                };
                state.blobs.insert(blob, mode_bytes);
                let c = &mut configs[index];
                c.crtc[1] = 1;
                c.crtc[2] = blob as u64;
                c.connector[3] = id(0x100, index) as u64;
                c.plane[3] = id(0x100, index) as u64;
                c.plane[4] = framebuffer.id as u64;
                c.plane[5] = (x as u64) << 16;
                c.plane[6] = (y as u64) << 16;
                c.plane[7] = mode_w << 16;
                c.plane[8] = mode_h << 16;
                c.plane[9] = 0;
                c.plane[10] = 0;
                c.plane[11] = mode_w;
                c.plane[12] = mode_h;
                validate(gpu, &mut configs, &[index], state)?;
            }
            commit(session, state, configs, vec![index], Vec::new(), 0, 0, None)?;
            Ok(())
        }
        0xb0 => {
            master(session)?;
            let (kind, index) = object(gpu, arg.r32(0)?)?;
            if kind != 0x100 || arg.r32(8)? & !1 != 0 || arg.r32(12)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let mut configs = gpu.displays.lock().configs.clone();
            configs[index].plane[4] = arg.r32(4)? as u64;
            validate(gpu, &mut configs, &[index], state)?;
            commit(
                session,
                state,
                configs,
                vec![index],
                Vec::new(),
                arg.r32(8)? | 0x200,
                arg.r64(16)?,
                None,
            )?;
            Ok(())
        }
        0xb1 => {
            let fb_id = arg.r32(0)?;
            let flags = arg.r32(4)?;
            let count = arg.r32(12)? as usize;
            if DRM_TRACE.load(Ordering::Relaxed) {
                crate::kinfo!(
                    "DIRTYFB fb={:#x} flags={:#x} count={} clips={:#x}",
                    fb_id,
                    flags,
                    count,
                    arg.r64(16).unwrap_or(0)
                );
            }
            let framebuffer = fb(gpu, state, fb_id)?;
            if flags & !3 != 0 || (flags & 1 != 0 && count % 2 != 0) {
                return Err(VfsError::InvalidArgument);
            }
            let full = Clip {
                x: 0,
                y: 0,
                width: framebuffer.width,
                height: framebuffer.height,
            };
            // ANNOTATE_COPY carries source/destination pairs; a full transfer
            // is still correct and avoids depending on that annotation.
            // Xorg can legitimately report thousands of glyph-sized damage
            // rectangles in one frame. Refusing them makes the server disable
            // dirty updates permanently, so fall back to a full transfer.
            let clips = if count == 0 || flags & 1 != 0 || count > 4096 {
                vec![full]
            } else {
                let data = uaccess::read_vec(arg.r64(16)?, count * 8, 4096 * 8)?;
                let mut clips = Vec::with_capacity(count);
                for rect in data.chunks_exact(8) {
                    let x1 = u16::from_ne_bytes([rect[0], rect[1]]) as u32;
                    let y1 = u16::from_ne_bytes([rect[2], rect[3]]) as u32;
                    let x2 = u16::from_ne_bytes([rect[4], rect[5]]) as u32;
                    let y2 = u16::from_ne_bytes([rect[6], rect[7]]) as u32;
                    if x2 > x1 && y2 > y1 && x2 <= framebuffer.width && y2 <= framebuffer.height {
                        clips.push(Clip {
                            x: x1,
                            y: y1,
                            width: x2 - x1,
                            height: y2 - y1,
                        });
                    }
                }
                if clips.is_empty() { vec![full] } else { clips }
            };
            let result = flush(gpu, &framebuffer, &clips);
            if let Err(error) = &result {
                crate::kwarn!("DIRTYFB flush failed: {:?}", error);
            }
            result
        }
        0xa3 | 0xbb => {
            master(session)?;
            let flags = arg.r32(0)?;
            if flags == 0 || flags & !3 != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let (kind, index) = object(gpu, arg.r32(4)?)?;
            if kind != 0x100 {
                return Err(VfsError::InvalidArgument);
            }
            let x = arg.r32(8)?;
            let y = arg.r32(12)?;
            let (hot_x, hot_y) = if nr == 0xbb {
                (arg.r32(28)?, arg.r32(32)?)
            } else {
                (0, 0)
            };
            if flags & 1 != 0 {
                let width = arg.r32(16)?;
                let height = arg.r32(20)?;
                let handle = arg.r32(24)?;
                let res_id = if handle != 0 {
                    let res = state.resource(handle)?;
                    if width != 64 || height != 64 || res.width != 64 || res.height != 64 {
                        return Err(VfsError::InvalidArgument);
                    }
                    gpu.call(0x105, 0, &[0, 0, 64, 64, 0, 0, res.id, 0], 24)?;
                    res.id
                } else {
                    0
                };
                let cmd = command(0x0300, 0, &[index as u32, x, y, 0, res_id, hot_x, hot_y, 0]);
                let req = gpu.transport.submit(true, &cmd, 24, Vec::new())?;
                gpu.transport.wait(&req)?;
            }
            if flags & 2 != 0 && flags & 1 == 0 {
                let cmd = command(0x0301, 0, &[index as u32, x, y, 0, 0, 0, 0, 0]);
                let req = gpu.transport.submit(true, &cmd, 24, Vec::new())?;
                gpu.transport.wait(&req)?;
            }
            Ok(())
        }
        0xb7 => {
            master(session)?;
            let (kind, index) = object(gpu, arg.r32(0)?)?;
            if kind != 0x400 || arg.r32(12)? != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let crtc_id = arg.r32(4)?;
            let fb_id = arg.r32(8)?;
            let mut configs = gpu.displays.lock().configs.clone();
            let c = &mut configs[index];
            if fb_id == 0 || crtc_id == 0 {
                c.plane[3] = 0;
                c.plane[4] = 0;
                c.framebuffer = None;
            } else {
                if crtc_id != id(0x100, index) {
                    return Err(VfsError::InvalidArgument);
                }
                c.plane[3] = crtc_id as u64;
                c.plane[4] = fb_id as u64;
                c.plane[9] = arg.r32(16)? as i32 as i64 as u64;
                c.plane[10] = arg.r32(20)? as i32 as i64 as u64;
                c.plane[11] = arg.r32(24)? as u64;
                c.plane[12] = arg.r32(28)? as u64;
                c.plane[5] = arg.r32(32)? as u64;
                c.plane[6] = arg.r32(36)? as u64;
                c.plane[8] = arg.r32(40)? as u64;
                c.plane[7] = arg.r32(44)? as u64;
            }
            validate(gpu, &mut configs, &[index], state)?;
            commit(session, state, configs, vec![index], Vec::new(), 0, 0, None)?;
            Ok(())
        }
        _ => Err(VfsError::NotSupported),
    }
}
fn flush(gpu: &Gpu, fb: &Framebuffer, clips: &[Clip]) -> Result<(), VfsError> {
    if clips.is_empty() {
        return Ok(());
    }
    let mut min_x = fb.width;
    let mut min_y = fb.height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for clip in clips {
        min_x = min_x.min(clip.x);
        min_y = min_y.min(clip.y);
        max_x = max_x.max(clip.x.saturating_add(clip.width).min(fb.width));
        max_y = max_y.max(clip.y.saturating_add(clip.height).min(fb.height));
    }
    if max_x <= min_x || max_y <= min_y {
        return Ok(());
    }
    let bounds = [Clip {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }];
    let transfers = if clips.len() > 32 { &bounds[..] } else { clips };
    if DRM_TRACE.load(Ordering::Relaxed) {
        let pixels: u64 = transfers
            .iter()
            .map(|c| u64::from(c.width) * u64::from(c.height))
            .sum();
        crate::kinfo!(
            "flush clips={} transfers={} pixels={} bounds={}x{}+{}+{}",
            clips.len(),
            transfers.len(),
            pixels,
            max_x - min_x,
            max_y - min_y,
            min_x,
            min_y
        );
    }
    for clip in transfers {
        let offset = (clip.y as u64)
            .checked_mul(fb.pitch as u64)
            .and_then(|value| value.checked_add((clip.x as u64) * 4))
            .ok_or(VfsError::InvalidArgument)?;
        gpu.call(
            0x105,
            0,
            &[
                clip.x,
                clip.y,
                clip.width,
                clip.height,
                offset as u32,
                (offset >> 32) as u32,
                fb.resource.id,
                0,
            ],
            24,
        )?;
    }
    if fb.resource.blob_mem == 0 {
        // One flush for the union: each RESOURCE_FLUSH makes the host present
        // the display again, so the count matters more than the area.
        gpu.call(
            0x104,
            0,
            &[
                min_x,
                min_y,
                max_x - min_x,
                max_y - min_y,
                fb.resource.id,
                0,
            ],
            24,
        )?;
    }
    Ok(())
}
pub fn service(gpu: &Gpu) {
    let job = {
        let mut d = gpu.displays.lock();
        let Some(job) = d.pending.pop_front() else {
            return;
        };
        d.in_progress = true;
        job
    };
    let result = (|| -> Result<(), VfsError> {
        for fence in &job.wait {
            fence.wait(gpu)?;
        }
        for &index in &job.touched {
            let c = &job.configs[index];
            if let Some(fb) = &c.framebuffer {
                let last = fb.resource.last.lock().clone();
                if let Some(f) = last {
                    f.wait(gpu)?;
                }
                flush(
                    gpu,
                    fb,
                    &[Clip {
                        x: 0,
                        y: 0,
                        width: fb.width,
                        height: fb.height,
                    }],
                )?;
                let x = (c.plane[5] >> 16) as u32;
                let y = (c.plane[6] >> 16) as u32;
                let w = c.plane[11] as u32;
                let h = c.plane[12] as u32;
                if fb.resource.blob_mem != 0 {
                    let mut words = vec![
                        x,
                        y,
                        w,
                        h,
                        index as u32,
                        fb.resource.id,
                        fb.width,
                        fb.height,
                        if fb.format == FORMATS[0] { 2 } else { 1 },
                        0,
                        fb.pitch,
                        0,
                        0,
                        0,
                        fb.offset,
                        0,
                        0,
                        0,
                    ];
                    gpu.call(0x10d, 0, &words, 24)?;
                    words.clear();
                } else {
                    gpu.call(0x103, 0, &[x, y, w, h, index as u32, fb.resource.id], 24)?;
                    gpu.call(0x104, 0, &[x, y, w, h, fb.resource.id, 0], 24)?;
                }
            } else {
                gpu.call(0x103, 0, &[0, 0, 0, 0, index as u32, 0], 24)?;
            }
        }
        Ok(())
    })();
    if result.is_ok() {
        gpu.console_visible.store(false, Ordering::Release);
        let mut display = gpu.displays.lock();
        display.configs = job.configs;
        display.generation += 1;
        collect(&mut display);
        for &index in &job.touched {
            display.sequence = display.sequence.wrapping_add(1);
            if job.event {
                let time = crate::time::uptime_micros();
                let mut event = vec![0; 32];
                put32(&mut event, 0, 2);
                put32(&mut event, 4, 32);
                put64(&mut event, 8, job.user_data);
                put32(&mut event, 16, (time / 1_000_000) as u32);
                put32(&mut event, 20, (time % 1_000_000) as u32);
                put32(&mut event, 24, display.sequence);
                put32(&mut event, 28, id(0x100, index));
                let mut queue = job.events.lock();
                if queue.len() < 256 {
                    queue.push_back(event);
                }
                drop(queue);
                crate::ipc::wake_object(Arc::as_ptr(&job.events));
            }
        }
    }
    gpu.displays.lock().in_progress = false;
    job.completion.complete(if result.is_ok() { 1 } else { -5 });
}

pub fn install_console(gpu: &Gpu, resource: &Arc<Resource>) {
    let output = Output {
        width: resource.width,
        height: resource.height,
        connected: true,
        edid: Vec::new(),
    };
    let bytes = mode(&output);
    let fb = Framebuffer {
        id: 0x8000,
        owner: 0,
        resource: resource.clone(),
        width: resource.width,
        height: resource.height,
        pitch: resource.stride,
        format: FORMATS[0],
        offset: 0,
    };
    let mut d = gpu.displays.lock();
    d.fbs.insert(fb.id, fb.clone());
    d.blobs.insert(0x700000, bytes.clone());
    let c = &mut d.configs[0];
    c.crtc[1] = 1;
    c.crtc[2] = 0x700000;
    c.connector[3] = 0x100;
    c.plane[3] = 0x100;
    c.plane[4] = fb.id as u64;
    c.plane[7] = (resource.width as u64) << 16;
    c.plane[8] = (resource.height as u64) << 16;
    c.plane[11] = resource.width as u64;
    c.plane[12] = resource.height as u64;
    c.framebuffer = Some(fb);
    c.mode = bytes;
    collect(&mut d);
}
pub fn restore_console(gpu: &Gpu, resource: &Arc<Resource>) {
    install_console(gpu, resource);
}
fn collect(d: &mut DisplayState) {
    let used_modes: Vec<u32> = d
        .configs
        .iter()
        .chain(d.pending.iter().flat_map(|j| j.configs.iter()))
        .map(|c| c.crtc[2] as u32)
        .collect();
    d.blobs.retain(|&id, _| {
        id == 0x700000 || d.blob_owners.contains_key(&id) || used_modes.contains(&id)
    });
    let used_fb: Vec<u32> = d
        .configs
        .iter()
        .chain(d.pending.iter().flat_map(|j| j.configs.iter()))
        .filter_map(|c| c.framebuffer.as_ref().map(|f| f.id))
        .collect();
    d.fbs
        .retain(|&id, f| id == 0x8000 || f.owner != 0 || used_fb.contains(&id));
}
pub fn close_session(gpu: &Gpu, id: u64) {
    let mut d = gpu.displays.lock();
    d.blob_owners.retain(|_, owner| *owner != id);
    for f in d.fbs.values_mut() {
        if f.owner == id {
            f.owner = 0;
        }
    }
    collect(&mut d);
}
