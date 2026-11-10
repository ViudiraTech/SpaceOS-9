/*
 *
 *       src/sound/card.rs
 *       Sound card registry, /dev/snd publication and /proc/asound
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A card groups PCM, control and timer devices.  `Card::register` allocates
//! the device minors and publishes `controlC<n>`, `pcmC<n>D<m>p`,
//! `pcmC<n>D<m>c` through the device registry, which in turn creates the
//! `/dev/snd` nodes, the `/sys/class/sound` entries and the uevents udev
//! expects.  Registration is explicit, mirroring `snd_card_register`.

use alloc::{collections::VecDeque, format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::{
    device,
    sched::WaitQueue,
    sync::Mutex,
    vfs::{
        Inode, InodeKind, Stat, VfsError, abi,
        tree::{Attribute, ino},
    },
};

use super::{
    SUBSYSTEM, control, hwdep, pcm, rawmidi,
    uapi::{self, SNDRV_MAJOR},
};

/// One control notification in the card-wide event log.
#[derive(Clone, Copy)]
pub struct ControlEvent {
    pub sequence: u64,
    pub event: uapi::snd_ctl_event,
}

pub struct Card {
    pub number: u32,
    pub id: String,
    pub driver: String,
    pub shortname: String,
    pub longname: String,
    pub mixername: String,
    pub components: Mutex<String>,
    pub pcms: Mutex<Vec<Arc<pcm::Pcm>>>,
    pub hwdeps: Mutex<Vec<Arc<hwdep::Hwdep>>>,
    pub rawmidis: Mutex<Vec<Arc<rawmidi::RawMidi>>>,
    pub controls: Mutex<Vec<Arc<control::ControlElem>>>,
    pub next_numid: AtomicU32,
    pub events: Mutex<VecDeque<ControlEvent>>,
    pub event_sequence: AtomicU64,
    pub event_wait: WaitQueue,
    pub registered: AtomicBool,
    nodes: Mutex<Vec<u64>>,
    minors: Mutex<Vec<u32>>,
}

static CARDS: Mutex<Vec<Arc<Card>>> = Mutex::new(Vec::new());
static CARD_NUMBERS: Mutex<[bool; uapi::SNDRV_CARDS as usize]> =
    Mutex::new([false; uapi::SNDRV_CARDS as usize]);

struct Minors {
    used: [bool; 256],
}

impl Minors {
    const fn new() -> Self {
        let mut used = [false; 256];
        used[uapi::SNDRV_MINOR_SEQUENCER as usize] = true;
        used[uapi::SNDRV_MINOR_TIMER as usize] = true;
        Self { used }
    }
    fn alloc(&mut self) -> Option<u32> {
        for (index, used) in self.used.iter_mut().enumerate() {
            if !*used {
                *used = true;
                return Some(index as u32);
            }
        }
        None
    }
    fn release(&mut self, minor: u32) {
        if minor as usize >= 256
            || minor == uapi::SNDRV_MINOR_SEQUENCER as u32
            || minor == uapi::SNDRV_MINOR_TIMER as u32
        {
            return;
        }
        self.used[minor as usize] = false;
    }
}

static MINORS: Mutex<Minors> = Mutex::new(Minors::new());

pub fn minor_device(minor: u32, session: Arc<dyn Inode>) -> Arc<dyn Inode> {
    Arc::new(MinorNode { minor, session })
}

/// The inode backend handed to `device::register`.  A fresh session is created
/// for every `open`, which is what makes per-file PCM and control state
/// independent.
struct MinorNode {
    minor: u32,
    session: Arc<dyn Inode>,
}

impl Inode for MinorNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0);
        stat.rdev = abi::makedev(SNDRV_MAJOR, self.minor as u64);
        Ok(stat)
    }
    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        match self.session.open(flags)? {
            Some(session) => Ok(Some(session)),
            None => Ok(Some(self.session.clone())),
        }
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        self.session.read_at(offset, buffer)
    }
    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        self.session.write_at(offset, buffer)
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        self.session.ioctl(cmd, arg)
    }
    fn poll(&self) -> u32 {
        self.session.poll()
    }
    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        self.session.poll_sources(sources);
    }
    fn mmap(
        &self,
        space: &mut crate::user::UserAddressSpace,
        address: u64,
        length: u64,
        offset: u64,
        prot: u32,
    ) -> Result<(), VfsError> {
        self.session.mmap(space, address, length, offset, prot)
    }
    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.session.set_nonblock(nonblock)
    }
    #[cfg(feature = "sound")]
    fn sound_substream(&self) -> Option<Arc<pcm::Substream>> {
        self.session.sound_substream()
    }
}

impl Card {
    /// Create an unregistered card.  `index` is the requested card number or a
    /// negative value to allocate the lowest free one.
    pub fn new(
        index: i32,
        id: &str,
        driver: &str,
        shortname: &str,
        longname: &str,
    ) -> Result<Arc<Self>, VfsError> {
        let mut numbers = CARD_NUMBERS.lock();
        let number = if index < 0 {
            numbers
                .iter()
                .position(|used| !*used)
                .ok_or(VfsError::NoSpace)? as u32
        } else {
            let number = index as u32;
            if number >= uapi::SNDRV_CARDS {
                return Err(VfsError::InvalidArgument);
            }
            if numbers[number as usize] {
                return Err(VfsError::Busy);
            }
            number
        };
        numbers[number as usize] = true;
        drop(numbers);
        Ok(Arc::new(Self {
            number,
            id: String::from(id),
            driver: String::from(driver),
            shortname: String::from(shortname),
            longname: String::from(longname),
            mixername: String::from(shortname),
            components: Mutex::new(String::new()),
            pcms: Mutex::new(Vec::new()),
            hwdeps: Mutex::new(Vec::new()),
            rawmidis: Mutex::new(Vec::new()),
            controls: Mutex::new(Vec::new()),
            next_numid: AtomicU32::new(1),
            events: Mutex::new(VecDeque::new()),
            event_sequence: AtomicU64::new(0),
            event_wait: WaitQueue::new(),
            registered: AtomicBool::new(false),
            nodes: Mutex::new(Vec::new()),
            minors: Mutex::new(Vec::new()),
        }))
    }

    pub fn add_component(&self, component: &str) {
        let mut components = self.components.lock();
        if !components.is_empty() {
            components.push(' ');
        }
        components.push_str(component);
    }

    pub fn add_control(&self, element: Arc<control::ControlElem>) -> Result<(), VfsError> {
        let mut elements = self.controls.lock();
        if elements
            .iter()
            .any(|existing| existing.id().same_identity(&element.id()))
        {
            return Err(VfsError::AlreadyExists);
        }
        let mut id = element.id();
        if id.numid == 0 {
            id.numid = self.next_numid.fetch_add(1, Ordering::AcqRel);
            element.set_numid(id.numid);
        }
        elements.push(element);
        Ok(())
    }

    pub fn remove_control(&self, id: &uapi::snd_ctl_elem_id) -> Result<(), VfsError> {
        let mut elements = self.controls.lock();
        let index = elements
            .iter()
            .position(|element| element.id().matches(id))
            .ok_or(VfsError::NotFound)?;
        let element = elements.remove(index);
        drop(elements);
        self.notify(uapi::SNDRV_CTL_EVENT_MASK_REMOVE, &element.id());
        Ok(())
    }

    pub fn controls(&self) -> Vec<Arc<control::ControlElem>> {
        self.controls.lock().clone()
    }

    pub fn find_control(&self, id: &uapi::snd_ctl_elem_id) -> Option<Arc<control::ControlElem>> {
        self.controls
            .lock()
            .iter()
            .find(|element| element.id().matches(id))
            .cloned()
    }

    pub fn add_pcm(&self, device: Arc<pcm::Pcm>) -> Result<(), VfsError> {
        let mut pcms = self.pcms.lock();
        if pcms.iter().any(|existing| existing.device == device.device) {
            return Err(VfsError::AlreadyExists);
        }
        pcms.push(device);
        Ok(())
    }

    pub fn minors(&self) -> Vec<u32> {
        self.minors.lock().clone()
    }

    pub fn add_hwdep(&self, device: Arc<hwdep::Hwdep>) -> Result<(), VfsError> {
        let mut hwdeps = self.hwdeps.lock();
        if hwdeps
            .iter()
            .any(|existing| existing.device == device.device)
        {
            return Err(VfsError::AlreadyExists);
        }
        hwdeps.push(device);
        Ok(())
    }

    pub fn hwdeps(&self) -> Vec<Arc<hwdep::Hwdep>> {
        self.hwdeps.lock().clone()
    }

    pub fn add_rawmidi(&self, device: Arc<rawmidi::RawMidi>) -> Result<(), VfsError> {
        let mut mids = self.rawmidis.lock();
        if mids.iter().any(|existing| existing.device == device.device) {
            return Err(VfsError::AlreadyExists);
        }
        mids.push(device);
        Ok(())
    }

    pub fn rawmidis(&self) -> Vec<Arc<rawmidi::RawMidi>> {
        self.rawmidis.lock().clone()
    }

    pub fn pcms(&self) -> Vec<Arc<pcm::Pcm>> {
        self.pcms.lock().clone()
    }

    pub fn find_pcm(&self, device: u32) -> Option<Arc<pcm::Pcm>> {
        self.pcms
            .lock()
            .iter()
            .find(|pcm| pcm.device == device)
            .cloned()
    }

    pub fn notify(&self, mask: u32, id: &uapi::snd_ctl_elem_id) {
        let mut event = uapi::snd_ctl_event::zeroed();
        event.kind = uapi::SNDRV_CTL_EVENT_ELEM;
        // SAFETY: the union is plain data; `elem` is the active member.
        event.data.elem = uapi::snd_ctl_event_elem { mask, id: *id };
        let sequence = self.event_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let mut events = self.events.lock();
        if events.len() >= 256 {
            events.pop_front();
        }
        events.push_back(ControlEvent { sequence, event });
        drop(events);
        crate::ipc::pollwait::wake_object(self as *const Card);
        self.event_wait.wake_all();
    }

    pub fn register(self: &Arc<Self>) -> Result<(), VfsError> {
        if self.registered.swap(true, Ordering::AcqRel) {
            return Err(VfsError::AlreadyExists);
        }
        if let Err(error) = self.register_inner() {
            self.rollback();
            self.registered.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    fn rollback(&self) {
        for id in self.nodes.lock().drain(..) {
            let _ = device::unregister(id);
        }
        for minor in self.minors.lock().drain(..) {
            MINORS.lock().release(minor);
        }
        CARDS.lock().retain(|card| card.number != self.number);
        super::procfs::card_removed(self.number);
    }

    fn register_inner(self: &Arc<Self>) -> Result<(), VfsError> {
        CARDS.lock().push(self.clone());
        // Control first, matching Linux card registration order.
        let control_minor = MINORS.lock().alloc().ok_or(VfsError::NoSpace)?;
        self.minors.lock().push(control_minor);
        let control_session: Arc<dyn Inode> = Arc::new(control::ControlSession::new(self.clone()));
        let backend = minor_device(control_minor, control_session);
        let name = format!("controlC{}", self.number);
        match device::register(
            &name,
            SUBSYSTEM,
            Some(&format!("snd/{name}")),
            Some(backend),
            Vec::new(),
        ) {
            Ok(device) => self.nodes.lock().push(device.id),
            Err(error) => {
                MINORS.lock().release(control_minor);
                return Err(error);
            }
        }

        for pcm in self.pcms() {
            for direction in [pcm::Direction::Playback, pcm::Direction::Capture] {
                let substreams = pcm.substreams(direction);
                if substreams.is_empty() {
                    continue;
                }
                let minor = MINORS.lock().alloc().ok_or(VfsError::NoSpace)?;
                self.minors.lock().push(minor);
                let name = format!("pcmC{}D{}{}", self.number, pcm.device, direction.suffix());
                let node: Arc<dyn Inode> = pcm::PcmDeviceNode::new(substreams);
                let backend = minor_device(minor, node);
                match device::register(
                    &name,
                    SUBSYSTEM,
                    Some(&format!("snd/{name}")),
                    Some(backend),
                    vec![("pcm_class", Attribute::text("generic\n"))],
                ) {
                    Ok(device) => self.nodes.lock().push(device.id),
                    Err(error) => {
                        MINORS.lock().release(minor);
                        return Err(error);
                    }
                }
            }
        }
        for hwdep in self.hwdeps() {
            let minor = MINORS.lock().alloc().ok_or(VfsError::NoSpace)?;
            self.minors.lock().push(minor);
            let name = format!("hwC{}D{}", self.number, hwdep.device);
            let node: Arc<dyn Inode> = hwdep::HwdepDeviceNode::new(hwdep);
            let backend = minor_device(minor, node);
            match device::register(
                &name,
                SUBSYSTEM,
                Some(&format!("snd/{name}")),
                Some(backend),
                Vec::new(),
            ) {
                Ok(device) => self.nodes.lock().push(device.id),
                Err(error) => {
                    MINORS.lock().release(minor);
                    return Err(error);
                }
            }
        }
        for midi in self.rawmidis() {
            let minor = MINORS.lock().alloc().ok_or(VfsError::NoSpace)?;
            self.minors.lock().push(minor);
            let name = midi.node_name();
            let node: Arc<dyn Inode> = rawmidi::RawMidiDeviceNode::new(midi);
            let backend = minor_device(minor, node);
            match device::register(
                &name,
                SUBSYSTEM,
                Some(&format!("snd/{name}")),
                Some(backend),
                Vec::new(),
            ) {
                Ok(device) => self.nodes.lock().push(device.id),
                Err(error) => {
                    MINORS.lock().release(minor);
                    return Err(error);
                }
            }
        }
        super::procfs::card_added(self);
        Ok(())
    }

    pub fn unregister(&self) {
        if !self.registered.swap(false, Ordering::AcqRel) {
            return;
        }
        for id in self.nodes.lock().drain(..) {
            let _ = device::unregister(id);
        }
        for minor in self.minors.lock().drain(..) {
            MINORS.lock().release(minor);
        }
        CARDS.lock().retain(|card| card.number != self.number);
        CARD_NUMBERS.lock()[self.number as usize] = false;
        super::procfs::card_removed(self.number);
    }
}

impl Drop for Card {
    fn drop(&mut self) {
        // A card released without an explicit unregister must not leak its
        // number or character-device minors.
        if self.registered.swap(false, Ordering::AcqRel) {
            for id in self.nodes.lock().drain(..) {
                let _ = device::unregister(id);
            }
            for minor in self.minors.lock().drain(..) {
                MINORS.lock().release(minor);
            }
            super::procfs::card_removed(self.number);
        }
        CARD_NUMBERS.lock()[self.number as usize] = false;
    }
}

pub fn cards() -> Vec<Arc<Card>> {
    CARDS.lock().clone()
}

pub fn find(number: u32) -> Option<Arc<Card>> {
    CARDS
        .lock()
        .iter()
        .find(|card| card.number == number)
        .cloned()
}

pub fn registered_count() -> usize {
    CARDS.lock().len()
}
