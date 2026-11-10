/*
 *
 *       src/sound/control.rs
 *       ALSA control (mixer) elements, TLV and notification interface
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! `/dev/snd/controlC<n>` implements the `'U'` ioctl group: card information,
//! the element list, element info/read/write, user elements, TLV and the
//! subscription queue that backs `snd_ctl_read` and poll.  Driver controls and
//! userspace-created controls share one representation; driver-specific
//! behaviour is expressed through [`ControlOps`].

use alloc::{string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::{
    ipc::pollwait,
    sched,
    sync::{Mutex, SpinLock},
    user::uaccess,
    vfs::{Inode, InodeKind, Stat, VfsError, abi, tree::ino},
};

use super::{card::Card, set_string, uapi};

pub const SNDRV_CTL_TLVT_CONTAINER: u32 = 0;
pub const SNDRV_CTL_TLVT_DB_SCALE: u32 = 1;
pub const SNDRV_CTL_TLVT_DB_MINMAX: u32 = 2;
pub const SNDRV_CTL_TLVT_DB_MINMAX_MUTE: u32 = 3;

/// Driver-provided element behaviour.  `info` fills the type-specific union,
/// `get`/`put` work on the ABI value structure directly, as Linux callbacks do.
pub trait ControlOps: Send + Sync {
    fn info(&self, info: &mut uapi::snd_ctl_elem_info) -> Result<(), VfsError>;
    fn get(&self, value: &mut uapi::snd_ctl_elem_value) -> Result<(), VfsError>;
    /// Returns whether the value changed, so an event can be queued.
    fn put(&self, value: &uapi::snd_ctl_elem_value) -> Result<bool, VfsError>;
    fn tlv_read(&self, _output: &mut Vec<u32>) -> Result<(), VfsError> {
        Err(VfsError::NotSupported)
    }
    fn tlv_write(&self, _input: &[u32]) -> Result<(), VfsError> {
        Err(VfsError::NotSupported)
    }
}

pub struct ControlElem {
    id: SpinLock<uapi::snd_ctl_elem_id>,
    kind: u32,
    access: u32,
    count: u32,
    min: i64,
    max: i64,
    step: i64,
    items: Vec<String>,
    values: Mutex<Vec<i64>>,
    bytes: Mutex<Vec<u8>>,
    tlv: Mutex<Vec<u32>>,
    locked_by: Mutex<i32>,
    ops: Option<Arc<dyn ControlOps>>,
}

impl ControlElem {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        iface: u32,
        device: u32,
        subdevice: u32,
        name: &str,
        index: u32,
        kind: u32,
        count: u32,
        min: i64,
        max: i64,
        step: i64,
    ) -> Arc<Self> {
        let mut id = uapi::snd_ctl_elem_id::zeroed();
        id.iface = iface;
        id.device = device;
        id.subdevice = subdevice;
        id.index = index;
        set_string(&mut id.name, name);
        Arc::new(Self {
            id: SpinLock::new(id),
            kind,
            access: uapi::SNDRV_CTL_ELEM_ACCESS_READWRITE,
            count,
            min,
            max,
            step,
            items: Vec::new(),
            values: Mutex::new(vec![0; count as usize]),
            bytes: Mutex::new(vec![0; count as usize * 4]),
            tlv: Mutex::new(Vec::new()),
            locked_by: Mutex::new(0),
            ops: None,
        })
    }

    pub fn with_items(mut self: Arc<Self>, items: Vec<&str>) -> Arc<Self> {
        if let Some(elem) = Arc::get_mut(&mut self) {
            elem.items = items.into_iter().map(String::from).collect();
        }
        self
    }

    pub fn with_tlv(mut self: Arc<Self>, tlv: Vec<u32>) -> Arc<Self> {
        if let Some(elem) = Arc::get_mut(&mut self) {
            elem.tlv = Mutex::new(tlv);
        }
        self
    }

    pub fn with_access(mut self: Arc<Self>, access: u32) -> Arc<Self> {
        if let Some(elem) = Arc::get_mut(&mut self) {
            elem.access = access;
        }
        self
    }

    pub fn with_ops(
        iface: u32,
        device: u32,
        subdevice: u32,
        name: &str,
        index: u32,
        access: u32,
        ops: Arc<dyn ControlOps>,
    ) -> Arc<Self> {
        let mut id = uapi::snd_ctl_elem_id::zeroed();
        id.iface = iface;
        id.device = device;
        id.subdevice = subdevice;
        id.index = index;
        set_string(&mut id.name, name);
        Arc::new(Self {
            id: SpinLock::new(id),
            kind: uapi::SNDRV_CTL_ELEM_TYPE_NONE,
            access,
            count: 1,
            min: 0,
            max: 0,
            step: 0,
            items: Vec::new(),
            values: Mutex::new(vec![0; 128]),
            bytes: Mutex::new(vec![0; 512]),
            tlv: Mutex::new(Vec::new()),
            locked_by: Mutex::new(0),
            ops: Some(ops),
        })
    }

    /// Create a userspace element (`SNDRV_CTL_ELEM_ACCESS_USER`).
    pub fn user(
        id: &uapi::snd_ctl_elem_id,
        kind: u32,
        access: u32,
        count: u32,
        min: i64,
        max: i64,
        step: i64,
        items: Vec<String>,
    ) -> Arc<Self> {
        let count = count.max(1).min(128);
        Arc::new(Self {
            id: SpinLock::new(*id),
            kind,
            access: access | uapi::SNDRV_CTL_ELEM_ACCESS_USER,
            count,
            min,
            max,
            step,
            items,
            values: Mutex::new(vec![0; count as usize]),
            bytes: Mutex::new(vec![0; count as usize * 4]),
            tlv: Mutex::new(Vec::new()),
            locked_by: Mutex::new(0),
            ops: None,
        })
    }

    pub fn id(&self) -> uapi::snd_ctl_elem_id {
        *self.id.lock()
    }

    pub fn set_numid(&self, numid: u32) {
        self.id.lock().numid = numid;
    }

    pub fn is_user(&self) -> bool {
        self.access & uapi::SNDRV_CTL_ELEM_ACCESS_USER != 0
    }

    pub fn info(&self) -> uapi::snd_ctl_elem_info {
        let mut info = uapi::snd_ctl_elem_info::zeroed();
        info.id = self.id();
        info.kind = self.kind;
        info.access = self.access;
        info.count = self.count;
        let owner = *self.locked_by.lock();
        if owner != 0 {
            info.access |= uapi::SNDRV_CTL_ELEM_ACCESS_OWNER;
            info.owner = owner;
        }
        if let Some(ops) = &self.ops {
            if ops.info(&mut info).is_err() {
                info.kind = uapi::SNDRV_CTL_ELEM_TYPE_NONE;
            }
            return info;
        }
        match self.kind {
            uapi::SNDRV_CTL_ELEM_TYPE_BOOLEAN
            | uapi::SNDRV_CTL_ELEM_TYPE_INTEGER
            | uapi::SNDRV_CTL_ELEM_TYPE_INTEGER64 => {
                // The union is plain data; writing a member needs no unsafe.
                info.value.integer.min = self.min;
                info.value.integer.max = self.max;
                info.value.integer.step = self.step;
            }
            uapi::SNDRV_CTL_ELEM_TYPE_ENUMERATED => {
                // SAFETY: the union is plain data; `enumerated` is the active member.
                unsafe {
                    info.value.enumerated.items = self.items.len() as u32;
                    let index = (info.value.enumerated.item as usize).min(self.items.len());
                    if let Some(name) = self.items.get(index) {
                        set_string(&mut info.value.enumerated.name, name);
                    }
                }
            }
            _ => {}
        }
        info
    }

    pub fn read(&self, value: &mut uapi::snd_ctl_elem_value) {
        value.id = self.id();
        if let Some(ops) = &self.ops {
            let _ = ops.get(value);
            return;
        }
        match self.kind {
            uapi::SNDRV_CTL_ELEM_TYPE_BOOLEAN
            | uapi::SNDRV_CTL_ELEM_TYPE_INTEGER
            | uapi::SNDRV_CTL_ELEM_TYPE_INTEGER64 => {
                let values = self.values.lock();
                value.set_integer(&values);
            }
            uapi::SNDRV_CTL_ELEM_TYPE_ENUMERATED => {
                let values = self.values.lock();
                let items: Vec<u32> = values.iter().map(|value| *value as u32).collect();
                value.set_enum_items(&items);
            }
            uapi::SNDRV_CTL_ELEM_TYPE_BYTES | uapi::SNDRV_CTL_ELEM_TYPE_IEC958 => {
                let bytes = self.bytes.lock();
                // SAFETY: the union is plain data and `bytes` is the widest
                // member; the source is bounded by its 512-byte capacity.
                unsafe {
                    value.value.bytes = [0; 512];
                    let length = bytes.len().min(512);
                    value.value.bytes[..length].copy_from_slice(&bytes[..length]);
                }
            }
            _ => {}
        }
    }

    /// Returns whether the write changed the stored value.
    pub fn write(&self, value: &uapi::snd_ctl_elem_value) -> Result<bool, VfsError> {
        if let Some(ops) = &self.ops {
            return ops.put(value);
        }
        match self.kind {
            uapi::SNDRV_CTL_ELEM_TYPE_BOOLEAN
            | uapi::SNDRV_CTL_ELEM_TYPE_INTEGER
            | uapi::SNDRV_CTL_ELEM_TYPE_INTEGER64 => {
                let incoming = value.integer();
                let mut values = self.values.lock();
                let mut changed = false;
                for (slot, new) in values.iter_mut().zip(incoming.iter()) {
                    let bounded = (*new).clamp(self.min, self.max);
                    if self.step > 0 {
                        let bounded = self.min + ((bounded - self.min) / self.step) * self.step;
                        if *slot != bounded {
                            *slot = bounded;
                            changed = true;
                        }
                    } else if *slot != bounded {
                        *slot = bounded;
                        changed = true;
                    }
                }
                Ok(changed)
            }
            uapi::SNDRV_CTL_ELEM_TYPE_ENUMERATED => {
                let incoming = value.enum_items();
                let mut values = self.values.lock();
                let mut changed = false;
                for (slot, new) in values.iter_mut().zip(incoming.iter()) {
                    let bounded = (*new as i64).clamp(0, self.items.len().saturating_sub(1) as i64);
                    if *slot != bounded {
                        *slot = bounded;
                        changed = true;
                    }
                }
                Ok(changed)
            }
            uapi::SNDRV_CTL_ELEM_TYPE_BYTES | uapi::SNDRV_CTL_ELEM_TYPE_IEC958 => {
                // SAFETY: the union is plain data; `bytes` is the active member.
                let incoming = unsafe { value.value.bytes };
                let mut bytes = self.bytes.lock();
                let changed = bytes.as_slice() != &incoming[..bytes.len()];
                let length = bytes.len();
                bytes.copy_from_slice(&incoming[..length]);
                Ok(changed)
            }
            _ => Err(VfsError::NotSupported),
        }
    }

    pub fn tlv_read(&self) -> Result<Vec<u32>, VfsError> {
        if let Some(ops) = &self.ops {
            let mut output = Vec::new();
            ops.tlv_read(&mut output)?;
            return Ok(output);
        }
        Ok(self.tlv.lock().clone())
    }

    /// Update one stored value without going through the userspace ABI.  Used
    /// by jack reporting and other in-kernel producers.
    pub fn set_value(&self, index: usize, value: i64) {
        let mut values = self.values.lock();
        if let Some(slot) = values.get_mut(index) {
            *slot = value;
        }
    }

    pub fn tlv_write(&self, input: &[u32]) -> Result<(), VfsError> {
        if let Some(ops) = &self.ops {
            return ops.tlv_write(input);
        }
        *self.tlv.lock() = input.to_vec();
        Ok(())
    }
}

fn task_pid() -> i32 {
    sched::current()
        .map(|task| task.stats().tgid as i32)
        .unwrap_or(0)
}

pub struct ControlSession {
    card: Arc<Card>,
    cursor: AtomicU64,
    subscribed: AtomicBool,
    nonblock: AtomicBool,
    template: AtomicBool,
    self_reference: SpinLock<alloc::sync::Weak<ControlSession>>,
}

impl ControlSession {
    pub fn new(card: Arc<Card>) -> Self {
        Self {
            card,
            cursor: AtomicU64::new(0),
            subscribed: AtomicBool::new(false),
            nonblock: AtomicBool::new(false),
            template: AtomicBool::new(true),
            self_reference: SpinLock::new(alloc::sync::Weak::new()),
        }
    }

    fn session(card: Arc<Card>, nonblock: bool) -> Arc<Self> {
        let cursor = card.event_sequence.load(Ordering::Acquire);
        Arc::new_cyclic(|weak| Self {
            card,
            cursor: AtomicU64::new(cursor),
            subscribed: AtomicBool::new(false),
            nonblock: AtomicBool::new(nonblock),
            template: AtomicBool::new(false),
            self_reference: SpinLock::new(weak.clone()),
        })
    }

    fn next_event(&self) -> Option<uapi::snd_ctl_event> {
        let cursor = self.cursor.load(Ordering::Acquire);
        let events = self.card.events.lock();
        let event = events
            .iter()
            .find(|event| event.sequence > cursor)
            .copied()?;
        self.cursor.store(event.sequence, Ordering::Release);
        Some(event.event)
    }
}

impl Inode for ControlSession {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        if !self.template.load(Ordering::Acquire) {
            return Ok(None);
        }
        let nonblock = flags & abi::O_NONBLOCK != 0;
        Ok(Some(Self::session(self.card.clone(), nonblock)))
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        control_ioctl(&self.card, self, cmd as u32, arg)
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        let size = core::mem::size_of::<uapi::snd_ctl_event>();
        if buffer.len() < size {
            return Err(VfsError::InvalidArgument);
        }
        loop {
            if let Some(event) = self.next_event() {
                let bytes =
                    unsafe { core::slice::from_raw_parts(&event as *const _ as *const u8, size) };
                buffer[..size].copy_from_slice(bytes);
                return Ok(size);
            }
            if self.nonblock.load(Ordering::Acquire) {
                return Err(VfsError::WouldBlock);
            }
            let sequence = self.cursor.load(Ordering::Acquire);
            self.card
                .event_wait
                .wait_until(|| self.card.event_sequence.load(Ordering::Acquire) > sequence);
        }
    }

    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }

    fn poll(&self) -> u32 {
        let cursor = self.cursor.load(Ordering::Acquire);
        if self.card.event_sequence.load(Ordering::Acquire) > cursor {
            super::pcm::POLLIN
        } else {
            0
        }
    }

    fn poll_sources(&self, sources: &mut pollwait::PollSources) {
        sources.object(self.card.as_ref());
        sources.tick();
    }
}

fn fill_card_info(card: &Card) -> uapi::snd_ctl_card_info {
    let mut info = uapi::snd_ctl_card_info::zeroed();
    info.card = card.number as i32;
    set_string(&mut info.id, &card.id);
    set_string(&mut info.driver, &card.driver);
    set_string(&mut info.name, &card.shortname);
    set_string(&mut info.longname, &card.longname);
    set_string(&mut info.mixername, &card.mixername);
    let components = card.components.lock().clone();
    set_string(&mut info.components, &components);
    info
}

fn find_substream(
    card: &Card,
    device: u32,
    subdevice: u32,
    stream: i32,
) -> Option<Arc<super::pcm::Substream>> {
    let pcm = card.find_pcm(device)?;
    let direction = match stream {
        uapi::SNDRV_PCM_STREAM_PLAYBACK => super::pcm::Direction::Playback,
        uapi::SNDRV_PCM_STREAM_CAPTURE => super::pcm::Direction::Capture,
        _ => return None,
    };
    pcm.substream(direction, subdevice)
        .or_else(|| pcm.first_substream(direction))
}

fn control_ioctl(
    card: &Arc<Card>,
    session: &ControlSession,
    cmd: u32,
    arg: u64,
) -> Result<u64, VfsError> {
    match cmd {
        uapi::SNDRV_CTL_IOCTL_PVERSION => {
            uaccess::write_value(arg, &(uapi::SNDRV_CTL_VERSION as i32))?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_CARD_INFO => {
            let info = fill_card_info(card);
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_CARD_BYTES => {
            let mut request = uaccess::read_value::<uapi::snd_ctl_card_bytes>(arg)?;
            if request.kind != uapi::SND_CTL_CARD_BTYPE_COMPONENTS {
                return Err(VfsError::InvalidArgument);
            }
            let components = card.components.lock().clone();
            let capacity = request.data_allocated as usize;
            let mut data = components.into_bytes();
            data.push(0);
            let copied = data.len().min(capacity);
            if capacity > 0 {
                uaccess::write(request.data, &data[..copied])?;
            }
            request.data_len = data.len() as u32;
            request.reserved = 0;
            uaccess::write_value(arg, &request)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_LIST => {
            let mut request = uaccess::read_value::<uapi::snd_ctl_elem_list>(arg)?;
            let elements = card.controls();
            request.count = elements.len() as u32;
            let offset = request.offset as usize;
            let used = (request.space as usize).min(elements.len().saturating_sub(offset));
            request.used = used as u32;
            if used != 0 {
                if request.pids == 0 {
                    return Err(VfsError::InvalidArgument);
                }
                let mut bytes = vec![0u8; used * core::mem::size_of::<uapi::snd_ctl_elem_id>()];
                for (index, element) in elements[offset..offset + used].iter().enumerate() {
                    let id = element.id();
                    let raw = unsafe {
                        core::slice::from_raw_parts(
                            &id as *const _ as *const u8,
                            core::mem::size_of::<uapi::snd_ctl_elem_id>(),
                        )
                    };
                    let start = index * core::mem::size_of::<uapi::snd_ctl_elem_id>();
                    bytes[start..start + raw.len()].copy_from_slice(raw);
                }
                uaccess::write(request.pids, &bytes)?;
            }
            uaccess::write_value(arg, &request)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_INFO => {
            let request = uaccess::read_value::<uapi::snd_ctl_elem_info>(arg)?;
            let element = card.find_control(&request.id).ok_or(VfsError::NotFound)?;
            let info = element.info();
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_READ => {
            let mut request = uaccess::read_value::<uapi::snd_ctl_elem_value>(arg)?;
            let element = card.find_control(&request.id).ok_or(VfsError::NotFound)?;
            if element.info().access & uapi::SNDRV_CTL_ELEM_ACCESS_READ == 0 {
                return Err(VfsError::PermissionDenied);
            }
            element.read(&mut request);
            uaccess::write_value(arg, &request)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_WRITE => {
            let request = uaccess::read_value::<uapi::snd_ctl_elem_value>(arg)?;
            let element = card.find_control(&request.id).ok_or(VfsError::NotFound)?;
            if element.info().access & uapi::SNDRV_CTL_ELEM_ACCESS_WRITE == 0 {
                return Err(VfsError::PermissionDenied);
            }
            let owner = *element.locked_by.lock();
            if owner != 0 && owner != task_pid() {
                return Err(VfsError::Busy);
            }
            if element.write(&request)? {
                let id = element.id();
                card.notify(uapi::SNDRV_CTL_EVENT_MASK_VALUE, &id);
            }
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_ADD => {
            let info = uaccess::read_value::<uapi::snd_ctl_elem_info>(arg)?;
            if info.access & uapi::SNDRV_CTL_ELEM_ACCESS_USER == 0 {
                return Err(VfsError::NotPermitted);
            }
            let mut id = info.id;
            if id.iface > uapi::SNDRV_CTL_ELEM_IFACE_SEQUENCER {
                return Err(VfsError::InvalidArgument);
            }
            let (min, max, step) = unsafe {
                (
                    info.value.integer.min,
                    info.value.integer.max,
                    info.value.integer.step,
                )
            };
            let mut items = Vec::new();
            if info.kind == uapi::SNDRV_CTL_ELEM_TYPE_ENUMERATED {
                // SAFETY: the union is plain data; `enumerated` is active for
                // an enumerated element.
                let enumerated = unsafe { info.value.enumerated };
                let count = enumerated.items as usize;
                let length = (enumerated.names_length as usize).min(1 << 16);
                if enumerated.names_ptr != 0 && length != 0 {
                    let mut names = vec![0u8; length];
                    uaccess::read(enumerated.names_ptr, &mut names)?;
                    for name in names.split(|byte| *byte == 0) {
                        if items.len() >= count {
                            break;
                        }
                        if !name.is_empty() {
                            items.push(String::from_utf8_lossy(name).into_owned());
                        }
                    }
                }
            }
            let element = ControlElem::user(
                &id,
                info.kind,
                info.access,
                info.count.max(1),
                min,
                max,
                step,
                items,
            );
            let numid = card.next_numid.fetch_add(1, Ordering::AcqRel);
            id.numid = numid;
            element.set_numid(numid);
            card.add_control(element.clone())?;
            card.notify(uapi::SNDRV_CTL_EVENT_MASK_ADD, &id);
            let mut response = element.info();
            response.id = id;
            uaccess::write_value(arg, &response)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_REPLACE => {
            let info = uaccess::read_value::<uapi::snd_ctl_elem_info>(arg)?;
            let element = card.find_control(&info.id).ok_or(VfsError::NotFound)?;
            if !element.is_user() {
                return Err(VfsError::NotPermitted);
            }
            card.remove_control(&info.id)?;
            let mut id = info.id;
            let numid = if id.numid != 0 {
                id.numid
            } else {
                card.next_numid.fetch_add(1, Ordering::AcqRel)
            };
            id.numid = numid;
            let (min, max, step) = unsafe {
                (
                    info.value.integer.min,
                    info.value.integer.max,
                    info.value.integer.step,
                )
            };
            let replacement = ControlElem::user(
                &id,
                info.kind,
                info.access,
                info.count.max(1),
                min,
                max,
                step,
                Vec::new(),
            );
            card.add_control(replacement.clone())?;
            card.notify(uapi::SNDRV_CTL_EVENT_MASK_ADD, &id);
            let response = replacement.info();
            uaccess::write_value(arg, &response)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_REMOVE => {
            let id = uaccess::read_value::<uapi::snd_ctl_elem_id>(arg)?;
            let element = card.find_control(&id).ok_or(VfsError::NotFound)?;
            if !element.is_user() {
                return Err(VfsError::NotPermitted);
            }
            card.remove_control(&id)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_LOCK => {
            let id = uaccess::read_value::<uapi::snd_ctl_elem_id>(arg)?;
            let element = card.find_control(&id).ok_or(VfsError::NotFound)?;
            let mut owner = element.locked_by.lock();
            let pid = task_pid();
            if *owner != 0 && *owner != pid {
                return Err(VfsError::Busy);
            }
            *owner = pid;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_ELEM_UNLOCK => {
            let id = uaccess::read_value::<uapi::snd_ctl_elem_id>(arg)?;
            let element = card.find_control(&id).ok_or(VfsError::NotFound)?;
            let mut owner = element.locked_by.lock();
            if *owner != 0 && *owner != task_pid() {
                return Err(VfsError::NotPermitted);
            }
            *owner = 0;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_SUBSCRIBE_EVENTS => {
            let mask = uaccess::read_value::<i32>(arg)?;
            session.subscribed.store(mask != 0, Ordering::Release);
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_TLV_READ => {
            let header = uaccess::read_value::<uapi::snd_ctl_tlv>(arg)?;
            if header.length as usize > 4096 || header.length % 4 != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let elements = card.controls();
            let element = elements
                .iter()
                .find(|element| element.id().numid == header.numid)
                .ok_or(VfsError::NotFound)?;
            let tlv = element.tlv_read()?;
            let bytes_length = (tlv.len() * 4).min(header.length as usize);
            let mut response = vec![0u8; 8 + bytes_length];
            response[0..4].copy_from_slice(&header.numid.to_ne_bytes());
            response[4..8].copy_from_slice(&(bytes_length as u32).to_ne_bytes());
            for (index, value) in tlv.iter().take(bytes_length / 4).enumerate() {
                let start = 8 + index * 4;
                response[start..start + 4].copy_from_slice(&value.to_ne_bytes());
            }
            uaccess::write(arg, &response)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_TLV_WRITE | uapi::SNDRV_CTL_IOCTL_TLV_COMMAND => {
            let header = uaccess::read_value::<uapi::snd_ctl_tlv>(arg)?;
            if header.length as usize > 4096 || header.length % 4 != 0 {
                return Err(VfsError::InvalidArgument);
            }
            let elements = card.controls();
            let element = elements
                .iter()
                .find(|element| element.id().numid == header.numid)
                .ok_or(VfsError::NotFound)?;
            let data = arg.checked_add(8).ok_or(VfsError::Fault)?;
            let bytes = uaccess::read_vec(data, header.length as usize, header.length as usize)?;
            let mut values = Vec::with_capacity(header.length as usize / 4);
            for chunk in bytes.chunks_exact(4) {
                values.push(u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            element.tlv_write(&values)?;
            let id = element.id();
            card.notify(uapi::SNDRV_CTL_EVENT_MASK_TLV, &id);
            if cmd == uapi::SNDRV_CTL_IOCTL_TLV_WRITE {
                uaccess::write_value(
                    arg,
                    &uapi::snd_ctl_tlv {
                        numid: header.numid,
                        length: header.length,
                    },
                )?;
            }
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE => {
            let current = uaccess::read_value::<i32>(arg)?;
            let mut next = -1;
            for pcm in card.pcms() {
                if current < 0 || pcm.device as i32 > current {
                    next = pcm.device as i32;
                    break;
                }
            }
            uaccess::write_value(arg, &next)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_PCM_INFO => {
            let mut request = uaccess::read_value::<uapi::snd_pcm_info>(arg)?;
            let substream = find_substream(card, request.device, request.subdevice, request.stream)
                .ok_or(VfsError::NoDevice)?;
            let info = substream.info();
            request = info;
            uaccess::write_value(arg, &request)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_PCM_PREFER_SUBDEVICE => {
            let _ = uaccess::read_value::<i32>(arg)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_POWER => {
            let value = uaccess::read_value::<i32>(arg)?;
            let d0 = uapi::SNDRV_CTL_POWER_D0 as i32;
            uaccess::write_value(arg, &if value == 0 { d0 } else { value })?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_POWER_STATE => {
            uaccess::write_value(arg, &(uapi::SNDRV_CTL_POWER_D0 as i32))?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_HWDEP_NEXT_DEVICE => {
            let current = uaccess::read_value::<i32>(arg)?;
            let mut next = -1;
            for hwdep in card.hwdeps() {
                if current < 0 || hwdep.device as i32 > current {
                    next = hwdep.device as i32;
                    break;
                }
            }
            uaccess::write_value(arg, &next)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_HWDEP_INFO => {
            let mut info = uaccess::read_value::<uapi::snd_hwdep_info>(arg)?;
            let hwdep = card
                .hwdeps()
                .into_iter()
                .find(|hwdep| hwdep.device == info.device)
                .ok_or(VfsError::NoDevice)?;
            info = hwdep.info();
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_RAWMIDI_NEXT_DEVICE => {
            let current = uaccess::read_value::<i32>(arg)?;
            let mut next = -1;
            for midi in card.rawmidis() {
                if current < 0 || midi.device as i32 > current {
                    next = midi.device as i32;
                    break;
                }
            }
            uaccess::write_value(arg, &next)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_RAWMIDI_INFO => {
            let mut info = uaccess::read_value::<uapi::snd_rawmidi_info>(arg)?;
            let midi = card
                .rawmidis()
                .into_iter()
                .find(|midi| midi.device == info.device)
                .ok_or(VfsError::NoDevice)?;
            let stream = match info.stream {
                uapi::SNDRV_RAWMIDI_STREAM_OUTPUT => midi.output.clone(),
                uapi::SNDRV_RAWMIDI_STREAM_INPUT => midi.input.clone(),
                _ => midi.input.clone().or_else(|| midi.output.clone()),
            }
            .ok_or(VfsError::NoDevice)?;
            info = stream.info(
                midi.card,
                midi.device,
                &midi.id,
                &midi.name,
                midi.info_flags,
            );
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_CTL_IOCTL_RAWMIDI_PREFER_SUBDEVICE => {
            let _ = uaccess::read_value::<i32>(arg)?;
            Ok(0)
        }
        _ => Err(VfsError::NotATty),
    }
}

/// Helper used by the built-in dummy card and sample modules to build a
/// standard 0..max integer volume with a dB TLV tree.
pub fn volume_element(name: &str, count: u32, max: i64) -> Arc<ControlElem> {
    let element = ControlElem::new(
        uapi::SNDRV_CTL_ELEM_IFACE_MIXER,
        0,
        0,
        name,
        0,
        uapi::SNDRV_CTL_ELEM_TYPE_INTEGER,
        count,
        0,
        max,
        1,
    );
    // dB_MINMAX with 1/100 dB units, as alsa-lib expects: -50 dB .. 0 dB.
    let tlv = vec![SNDRV_CTL_TLVT_DB_MINMAX, 2 * 4, (-5000i32) as u32, 0];
    element
        .with_access(
            uapi::SNDRV_CTL_ELEM_ACCESS_READWRITE
                | uapi::SNDRV_CTL_ELEM_ACCESS_TLV_READ
                | uapi::SNDRV_CTL_ELEM_ACCESS_TLV_WRITE,
        )
        .with_tlv(tlv)
}

pub fn switch_element(name: &str, count: u32) -> Arc<ControlElem> {
    ControlElem::new(
        uapi::SNDRV_CTL_ELEM_IFACE_MIXER,
        0,
        0,
        name,
        0,
        uapi::SNDRV_CTL_ELEM_TYPE_BOOLEAN,
        count,
        0,
        1,
        0,
    )
}

pub fn enumerated_element(name: &str, items: Vec<&str>) -> Arc<ControlElem> {
    let count = items.len() as u32;
    let element = ControlElem::new(
        uapi::SNDRV_CTL_ELEM_IFACE_MIXER,
        0,
        0,
        name,
        0,
        uapi::SNDRV_CTL_ELEM_TYPE_ENUMERATED,
        1,
        0,
        count.saturating_sub(1) as i64,
        0,
    );
    element.with_items(items)
}

/// A jack element exposed as a read-only, volatile boolean control, matching
/// the kcontrol the Linux jack core creates for non-root userspace.
pub struct Jack {
    pub card: Arc<Card>,
    pub element: Arc<ControlElem>,
    pub status: AtomicU32,
}

pub fn jack_new(card: &Arc<Card>, id: &str, _jack_type: u32) -> Result<Arc<Jack>, VfsError> {
    let element = ControlElem::new(
        uapi::SNDRV_CTL_ELEM_IFACE_CARD,
        0,
        0,
        &alloc::format!("{id} Jack"),
        0,
        uapi::SNDRV_CTL_ELEM_TYPE_BOOLEAN,
        1,
        0,
        1,
        0,
    )
    .with_access(uapi::SNDRV_CTL_ELEM_ACCESS_READ | uapi::SNDRV_CTL_ELEM_ACCESS_VOLATILE);
    card.add_control(element.clone())?;
    Ok(Arc::new(Jack {
        card: card.clone(),
        element,
        status: AtomicU32::new(0),
    }))
}

pub fn jack_report(jack: &Jack, status: u32) {
    let previous = jack.status.swap(status, Ordering::AcqRel);
    if previous == status {
        return;
    }
    jack.element.set_value(0, i64::from(status != 0));
    let id = jack.element.id();
    jack.card.notify(uapi::SNDRV_CTL_EVENT_MASK_VALUE, &id);
}
