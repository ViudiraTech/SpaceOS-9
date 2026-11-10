/*
 *
 *       src/sound/timer.rs
 *       ALSA timer interface: /dev/snd/timer and the system timer source
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! A single kernel timer source backs both the global (`SNDRV_TIMER_CLASS_GLOBAL`,
//! device `SNDRV_TIMER_GLOBAL_SYSTEM`) and the per-card application timers.
//! Ticks are generated on the kernel software-timer thread at the requested
//! resolution, so a timer client never depends on the scheduler tick alone.
//! The ioctl surface is the `'T'` group; `read` returns `snd_timer_read`
//! records, or `snd_timer_tread` records after `SNDRV_TIMER_IOCTL_TREAD`.

use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{
    ipc::pollwait,
    sched::WaitQueue,
    sync::Mutex,
    time,
    user::uaccess,
    vfs::{Inode, InodeKind, Stat, VfsError, abi, tree::ino},
};

use super::{card, set_string, uapi};

const DEFAULT_RESOLUTION_NS: u64 = 1_000_000; // 1 ms
const MIN_PERIOD_US: u64 = 1_000;

static RESOLUTION_NS: AtomicU64 = AtomicU64::new(DEFAULT_RESOLUTION_NS);
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static SESSIONS: Mutex<Vec<Weak<TimerSession>>> = Mutex::new(Vec::new());
static UTIMERS: Mutex<Vec<Weak<UTimer>>> = Mutex::new(Vec::new());

#[derive(Clone, Copy)]
struct TimerRecord {
    event: i32,
    tstamp: uapi::timespec,
    val: u32,
    ticks: u32,
    resolution: u32,
}

struct Params {
    flags: u32,
    ticks: u32,
    queue_size: u32,
    filter: u32,
    running: bool,
    paused: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TimerRef {
    class: i32,
    card: i32,
    device: i32,
    subdevice: i32,
}

struct UTimer {
    id: u32,
    resolution_ns: AtomicU64,
}

impl UTimer {
    fn trigger(&self) {
        let id = self.id as i32;
        for weak in SESSIONS.lock().iter() {
            let Some(session) = weak.upgrade() else {
                continue;
            };
            let bound = *session.bound.lock();
            if bound.is_some_and(|timer| {
                timer.class == uapi::SNDRV_TIMER_CLASS_GLOBAL
                    && timer.device == uapi::SNDRV_TIMER_GLOBAL_UDRIVEN
                    && timer.subdevice == id
            }) {
                session.push_tick();
            }
        }
    }
}

impl Drop for UTimer {
    fn drop(&mut self) {
        let id = self.id;
        UTIMERS
            .lock()
            .retain(|weak| weak.upgrade().is_some_and(|timer| timer.id != id));
    }
}

struct UTimerInode {
    timer: Arc<UTimer>,
}

impl Inode for UTimerInode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        match cmd as u32 {
            uapi::SNDRV_TIMER_IOCTL_PVERSION => {
                uaccess::write_value(arg, &(uapi::SNDRV_TIMER_VERSION as i32))?;
                Ok(0)
            }
            uapi::SNDRV_TIMER_IOCTL_TRIGGER => {
                self.timer.trigger();
                Ok(0)
            }
            _ => Err(VfsError::NotATty),
        }
    }
    fn read_at(&self, _offset: u64, _buffer: &mut [u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }
}

pub struct TimerSession {
    template: AtomicBool,
    nonblock: AtomicBool,
    tread: AtomicBool,
    bound: Mutex<Option<TimerRef>>,
    params: Mutex<Params>,
    queue: Mutex<VecDeque<TimerRecord>>,
    generation: AtomicU64,
    arm_generation: AtomicU64,
    wait: WaitQueue,
    overrun: AtomicU64,
    self_reference: crate::sync::SpinLock<Weak<TimerSession>>,
}

impl TimerSession {
    fn template() -> Self {
        Self {
            template: AtomicBool::new(true),
            nonblock: AtomicBool::new(false),
            tread: AtomicBool::new(false),
            bound: Mutex::new(None),
            params: Mutex::new(Params {
                flags: 0,
                ticks: 0,
                queue_size: 32,
                filter: 0,
                running: false,
                paused: false,
            }),
            queue: Mutex::new(VecDeque::new()),
            generation: AtomicU64::new(0),
            arm_generation: AtomicU64::new(0),
            wait: WaitQueue::new(),
            overrun: AtomicU64::new(0),
            self_reference: crate::sync::SpinLock::new(Weak::new()),
        }
    }

    fn session(nonblock: bool) -> Arc<Self> {
        let session = Arc::new_cyclic(|weak| Self {
            template: AtomicBool::new(false),
            nonblock: AtomicBool::new(nonblock),
            tread: AtomicBool::new(false),
            bound: Mutex::new(None),
            params: Mutex::new(Params {
                flags: 0,
                ticks: 0,
                queue_size: 32,
                filter: 0,
                running: false,
                paused: false,
            }),
            queue: Mutex::new(VecDeque::new()),
            generation: AtomicU64::new(1),
            arm_generation: AtomicU64::new(1),
            wait: WaitQueue::new(),
            overrun: AtomicU64::new(0),
            self_reference: crate::sync::SpinLock::new(weak.clone()),
        });
        SESSIONS.lock().push(Arc::downgrade(&session));
        session
    }

    fn period_micros(&self) -> u64 {
        let params = self.params.lock();
        let resolution = RESOLUTION_NS.load(Ordering::Acquire);
        (u64::from(params.ticks.max(1)) * resolution / 1_000).max(MIN_PERIOD_US)
    }

    fn push(&self, record: TimerRecord) {
        // Without TREAD, `read` returns only tick records; state changes still
        // wake a blocked reader through the generation counter.
        if record.event != uapi::SNDRV_TIMER_EVENT_TICK && !self.tread.load(Ordering::Acquire) {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.wait.wake_all();
            return;
        }
        let queue_size = self.params.lock().queue_size.max(2) as usize;
        let mut queue = self.queue.lock();
        if queue.len() >= queue_size {
            queue.pop_front();
            self.overrun.fetch_add(1, Ordering::AcqRel);
        }
        queue.push_back(record);
        drop(queue);
        self.generation.fetch_add(1, Ordering::AcqRel);
        pollwait::wake_object(self as *const TimerSession);
        self.wait.wake_all();
    }

    fn push_tick(&self) {
        // Snapshot under the lock, then drop it: `push` takes the same lock to
        // read the queue size and the mutex is not reentrant.
        let (running, paused, filter, ticks) = {
            let params = self.params.lock();
            (params.running, params.paused, params.filter, params.ticks)
        };
        if !running || paused {
            return;
        }
        if filter == 0 || filter & (1 << uapi::SNDRV_TIMER_EVENT_TICK) != 0 {
            let resolution = RESOLUTION_NS.load(Ordering::Acquire) as u32;
            self.push(TimerRecord {
                event: uapi::SNDRV_TIMER_EVENT_TICK,
                tstamp: uapi::timespec::from_micros(time::uptime_micros()),
                val: ticks,
                ticks,
                resolution,
            });
        }
    }
}

impl Drop for TimerSession {
    fn drop(&mut self) {
        self.arm_generation.fetch_add(1, Ordering::AcqRel);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.params.lock().running = false;
    }
}

fn arm(session: &Arc<TimerSession>, generation: u64) {
    if session.arm_generation.load(Ordering::Acquire) != generation {
        return;
    }
    {
        let params = session.params.lock();
        if !params.running || params.paused {
            return;
        }
    }
    let period = session.period_micros();
    let clone = session.clone();
    crate::net::timer::schedule_in(
        period,
        alloc::boxed::Box::new(move || {
            if clone.arm_generation.load(Ordering::Acquire) != generation {
                return;
            }
            clone.push_tick();
            arm(&clone, generation);
        }),
    );
}

fn restart(session: &TimerSession) {
    let generation = session.arm_generation.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(arc) = session.self_reference.lock().upgrade() {
        arm(&arc, generation);
    }
}

fn all_timers() -> Vec<TimerRef> {
    let mut timers = Vec::new();
    timers.push(TimerRef {
        class: uapi::SNDRV_TIMER_CLASS_GLOBAL,
        card: -1,
        device: uapi::SNDRV_TIMER_GLOBAL_SYSTEM,
        subdevice: 0,
    });
    for card in card::cards() {
        timers.push(TimerRef {
            class: uapi::SNDRV_TIMER_CLASS_CARD,
            card: card.number as i32,
            device: 0,
            subdevice: 0,
        });
    }
    for weak in UTIMERS.lock().iter() {
        if let Some(timer) = weak.upgrade() {
            timers.push(TimerRef {
                class: uapi::SNDRV_TIMER_CLASS_GLOBAL,
                card: -1,
                device: uapi::SNDRV_TIMER_GLOBAL_UDRIVEN,
                subdevice: timer.id as i32,
            });
        }
    }
    timers
}

fn timer_matches(reference: TimerRef, id: &uapi::snd_timer_id) -> bool {
    if id.dev_class == uapi::SNDRV_TIMER_CLASS_GLOBAL {
        if id.device == uapi::SNDRV_TIMER_GLOBAL_UDRIVEN {
            return reference.class == uapi::SNDRV_TIMER_CLASS_GLOBAL
                && reference.device == uapi::SNDRV_TIMER_GLOBAL_UDRIVEN
                && reference.subdevice == id.subdevice;
        }
        return reference.class == uapi::SNDRV_TIMER_CLASS_GLOBAL
            && reference.device == uapi::SNDRV_TIMER_GLOBAL_SYSTEM
            && (id.device == uapi::SNDRV_TIMER_GLOBAL_SYSTEM || id.device < 0);
    }
    if id.dev_class == uapi::SNDRV_TIMER_CLASS_CARD {
        return reference.class == uapi::SNDRV_TIMER_CLASS_CARD
            && reference.card == id.card
            && reference.device == 0
            && (id.device == 0 || id.device < 0);
    }
    false
}

fn utimer_resolution(id: i32) -> Option<u64> {
    UTIMERS.lock().iter().find_map(|weak| {
        weak.upgrade()
            .filter(|timer| timer.id as i32 == id)
            .map(|timer| timer.resolution_ns.load(Ordering::Acquire))
    })
}

fn timer_resolution(timer: TimerRef) -> u64 {
    if timer.device == uapi::SNDRV_TIMER_GLOBAL_UDRIVEN {
        utimer_resolution(timer.subdevice).unwrap_or(DEFAULT_RESOLUTION_NS)
    } else {
        RESOLUTION_NS.load(Ordering::Acquire)
    }
}

fn is_utimer(timer: TimerRef) -> bool {
    timer.class == uapi::SNDRV_TIMER_CLASS_GLOBAL
        && timer.device == uapi::SNDRV_TIMER_GLOBAL_UDRIVEN
}

fn resolve(timers: &[TimerRef], id: &uapi::snd_timer_id) -> Option<TimerRef> {
    timers
        .iter()
        .copied()
        .find(|timer| timer_matches(*timer, id))
}

fn active_clients() -> u32 {
    let mut sessions = SESSIONS.lock();
    sessions.retain(|session| session.strong_count() != 0);
    sessions
        .iter()
        .filter_map(Weak::upgrade)
        .filter(|session| session.params.lock().running)
        .count() as u32
}

fn fill_info(timer: TimerRef, info: &mut uapi::snd_timer_info) {
    info.flags = 0;
    info.card = timer.card;
    match (timer.class, timer.device) {
        (uapi::SNDRV_TIMER_CLASS_GLOBAL, uapi::SNDRV_TIMER_GLOBAL_UDRIVEN) => {
            set_string(&mut info.id, &alloc::format!("uTimer{}", timer.subdevice));
            set_string(&mut info.name, "Userspace timer");
        }
        (uapi::SNDRV_TIMER_CLASS_GLOBAL, _) => {
            set_string(&mut info.id, "System");
            set_string(&mut info.name, "System timer");
        }
        _ => {
            set_string(&mut info.id, &alloc::format!("card{}", timer.card));
            set_string(&mut info.name, "Application timer");
        }
    }
    info.reserved0 = 0;
    info.resolution = timer_resolution(timer);
}

fn fill_ginfo(timer: TimerRef, info: &mut uapi::snd_timer_ginfo) {
    info.tid = uapi::snd_timer_id::zeroed();
    info.tid.dev_class = timer.class;
    info.tid.dev_sclass = if timer.class == uapi::SNDRV_TIMER_CLASS_CARD {
        uapi::SNDRV_TIMER_SCLASS_APPLICATION
    } else {
        uapi::SNDRV_TIMER_SCLASS_NONE
    };
    info.tid.card = timer.card;
    info.tid.device = timer.device;
    info.tid.subdevice = timer.subdevice;
    info.flags = 0;
    info.card = timer.card;
    match (timer.class, timer.device) {
        (uapi::SNDRV_TIMER_CLASS_GLOBAL, uapi::SNDRV_TIMER_GLOBAL_UDRIVEN) => {
            set_string(&mut info.id, &alloc::format!("uTimer{}", timer.subdevice));
            set_string(&mut info.name, "Userspace timer");
        }
        (uapi::SNDRV_TIMER_CLASS_GLOBAL, _) => {
            set_string(&mut info.id, "System");
            set_string(&mut info.name, "System timer");
        }
        _ => {
            set_string(&mut info.id, &alloc::format!("card{}", timer.card));
            set_string(&mut info.name, "Application timer");
        }
    }
    info.reserved0 = 0;
    let resolution = timer_resolution(timer);
    info.resolution = resolution;
    info.resolution_min = resolution;
    info.resolution_max = resolution;
    info.clients = active_clients();
}

fn allocate_utimer_id() -> Option<u32> {
    let timers = UTIMERS.lock();
    let used: Vec<u32> = timers
        .iter()
        .filter_map(Weak::upgrade)
        .map(|timer| timer.id)
        .collect();
    (0..128).find(|id| !used.contains(id))
}

fn create_utimer(resolution: u64) -> Result<(u32, i32), VfsError> {
    let id = allocate_utimer_id().ok_or(VfsError::NoSpace)?;
    let timer = Arc::new(UTimer {
        id,
        resolution_ns: AtomicU64::new(resolution.max(1)),
    });
    UTIMERS.lock().push(Arc::downgrade(&timer));
    let inode: Arc<dyn Inode> = Arc::new(UTimerInode { timer });
    let file = Arc::new(crate::vfs::OpenFile::new(
        inode,
        alloc::string::String::from("anon_inode:[alsatimer]"),
        abi::O_RDWR | abi::O_NONBLOCK,
    ));
    let fd = crate::vfs::current_fd_table().lock().alloc_fd(
        0,
        crate::vfs::FdEntry {
            file,
            cloexec: false,
        },
    )?;
    Ok((id, fd as i32))
}

fn timer_ioctl(session: &TimerSession, cmd: u32, arg: u64) -> Result<u64, VfsError> {
    match cmd {
        uapi::SNDRV_TIMER_IOCTL_PVERSION => {
            uaccess::write_value(arg, &(uapi::SNDRV_TIMER_VERSION as i32))?;
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_NEXT_DEVICE => {
            let id = uaccess::read_value::<uapi::snd_timer_id>(arg)?;
            let timers = all_timers();
            let next = if id.dev_class == uapi::SNDRV_TIMER_CLASS_NONE {
                timers.first().copied()
            } else {
                timers
                    .iter()
                    .position(|timer| timer_matches(*timer, &id))
                    .and_then(|index| timers.get(index + 1).copied())
            };
            let mut response = id;
            match next {
                Some(timer) => {
                    response.dev_class = timer.class;
                    response.dev_sclass = if timer.class == uapi::SNDRV_TIMER_CLASS_CARD {
                        uapi::SNDRV_TIMER_SCLASS_APPLICATION
                    } else {
                        uapi::SNDRV_TIMER_SCLASS_NONE
                    };
                    response.card = timer.card;
                    response.device = timer.device;
                    response.subdevice = timer.subdevice;
                }
                None => return Err(VfsError::NoDevice),
            }
            uaccess::write_value(arg, &response)?;
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_GINFO => {
            let mut info = uaccess::read_value::<uapi::snd_timer_ginfo>(arg)?;
            let timers = all_timers();
            let timer = resolve(&timers, &info.tid).ok_or(VfsError::NoDevice)?;
            fill_ginfo(timer, &mut info);
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_GPARAMS => {
            let params = uaccess::read_value::<uapi::snd_timer_gparams>(arg)?;
            if params.period_den == 0 || params.period_num == 0 {
                return Err(VfsError::InvalidArgument);
            }
            let resolution = (params.period_num as u128).saturating_mul(1_000_000_000)
                / params.period_den as u128;
            RESOLUTION_NS.store((resolution as u64).max(1), Ordering::Release);
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_GSTATUS => {
            let mut status = uaccess::read_value::<uapi::snd_timer_gstatus>(arg)?;
            let timers = all_timers();
            let timer = resolve(&timers, &status.tid).ok_or(VfsError::NoDevice)?;
            let resolution = RESOLUTION_NS.load(Ordering::Acquire);
            status.tid.dev_class = timer.class;
            status.tid.card = timer.card;
            status.tid.device = 0;
            status.resolution = resolution;
            status.resolution_num = resolution;
            status.resolution_den = 1_000_000_000;
            uaccess::write_value(arg, &status)?;
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_SELECT => {
            let select = uaccess::read_value::<uapi::snd_timer_select>(arg)?;
            let timers = all_timers();
            let timer = resolve(&timers, &select.id).ok_or(VfsError::NoDevice)?;
            *session.bound.lock() = Some(timer);
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_INFO => {
            let mut info = uaccess::read_value::<uapi::snd_timer_info>(arg)?;
            let timer = (*session.bound.lock()).unwrap_or(TimerRef {
                class: uapi::SNDRV_TIMER_CLASS_GLOBAL,
                card: -1,
                device: uapi::SNDRV_TIMER_GLOBAL_SYSTEM,
                subdevice: 0,
            });
            fill_info(timer, &mut info);
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_PARAMS => {
            let params = uaccess::read_value::<uapi::snd_timer_params>(arg)?;
            if params.ticks == 0 {
                return Err(VfsError::InvalidArgument);
            }
            let queue_size = params.queue_size.clamp(32, 1024);
            let mut current = session.params.lock();
            current.flags = params.flags;
            current.ticks = params.ticks;
            current.queue_size = queue_size;
            current.filter = params.filter;
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_STATUS => {
            let mut status = uapi::snd_timer_status::zeroed();
            status.tstamp = uapi::timespec::from_micros(time::uptime_micros());
            let timer = (*session.bound.lock()).unwrap_or(TimerRef {
                class: uapi::SNDRV_TIMER_CLASS_GLOBAL,
                card: -1,
                device: uapi::SNDRV_TIMER_GLOBAL_SYSTEM,
                subdevice: 0,
            });
            status.resolution = timer_resolution(timer).min(u32::MAX as u64) as u32;
            status.lost = 0;
            status.overrun = session.overrun.load(Ordering::Acquire).min(u32::MAX as u64) as u32;
            status.queue = session.queue.lock().len() as u32;
            uaccess::write_value(arg, &status)?;
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_START => {
            {
                let mut params = session.params.lock();
                if params.ticks == 0 {
                    return Err(VfsError::InvalidArgument);
                }
                params.running = true;
                params.paused = false;
            }
            let timer = *session.bound.lock();
            session.push(TimerRecord {
                event: uapi::SNDRV_TIMER_EVENT_START,
                tstamp: uapi::timespec::from_micros(time::uptime_micros()),
                val: timer
                    .map_or_else(|| RESOLUTION_NS.load(Ordering::Acquire), timer_resolution)
                    .min(u32::MAX as u64) as u32,
                ticks: 0,
                resolution: 0,
            });
            if !timer.is_some_and(is_utimer) {
                restart(session);
            }
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_STOP => {
            {
                let mut params = session.params.lock();
                params.running = false;
                params.paused = false;
            }
            session.arm_generation.fetch_add(1, Ordering::AcqRel);
            session.generation.fetch_add(1, Ordering::AcqRel);
            session.push(TimerRecord {
                event: uapi::SNDRV_TIMER_EVENT_STOP,
                tstamp: uapi::timespec::from_micros(time::uptime_micros()),
                val: 0,
                ticks: 0,
                resolution: 0,
            });
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_CONTINUE => {
            {
                let mut params = session.params.lock();
                params.running = true;
                params.paused = false;
            }
            let timer = *session.bound.lock();
            session.push(TimerRecord {
                event: uapi::SNDRV_TIMER_EVENT_CONTINUE,
                tstamp: uapi::timespec::from_micros(time::uptime_micros()),
                val: timer
                    .map_or_else(|| RESOLUTION_NS.load(Ordering::Acquire), timer_resolution)
                    .min(u32::MAX as u64) as u32,
                ticks: 0,
                resolution: 0,
            });
            if !timer.is_some_and(is_utimer) {
                restart(session);
            }
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_PAUSE => {
            session.params.lock().paused = true;
            session.arm_generation.fetch_add(1, Ordering::AcqRel);
            session.generation.fetch_add(1, Ordering::AcqRel);
            session.push(TimerRecord {
                event: uapi::SNDRV_TIMER_EVENT_PAUSE,
                tstamp: uapi::timespec::from_micros(time::uptime_micros()),
                val: 0,
                ticks: 0,
                resolution: 0,
            });
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_TREAD | uapi::SNDRV_TIMER_IOCTL_TREAD64 => {
            let enable = uaccess::read_value::<i32>(arg)?;
            session.tread.store(enable != 0, Ordering::Release);
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_CREATE => {
            let mut info = uaccess::read_value::<uapi::snd_timer_uinfo>(arg)?;
            let (id, fd) = create_utimer(info.resolution)?;
            info.id = id;
            info.fd = fd;
            uaccess::write_value(arg, &info)?;
            Ok(0)
        }
        uapi::SNDRV_TIMER_IOCTL_TRIGGER => Err(VfsError::NotATty),
        _ => Err(VfsError::NotATty),
    }
}

impl Inode for TimerSession {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        if !self.template.load(Ordering::Acquire) {
            return Ok(None);
        }
        let nonblock = flags & abi::O_NONBLOCK != 0;
        Ok(Some(Self::session(nonblock)))
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        timer_ioctl(self, cmd as u32, arg)
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        loop {
            {
                let mut queue = self.queue.lock();
                if let Some(record) = queue.pop_front() {
                    drop(queue);
                    if self.tread.load(Ordering::Acquire) {
                        let size = core::mem::size_of::<uapi::snd_timer_tread>();
                        if buffer.len() < size {
                            return Err(VfsError::InvalidArgument);
                        }
                        let tread = uapi::snd_timer_tread {
                            event: record.event,
                            pad1: [0; 4],
                            tstamp: record.tstamp,
                            val: record.val,
                            pad2: [0; 4],
                        };
                        let bytes = unsafe {
                            core::slice::from_raw_parts(&tread as *const _ as *const u8, size)
                        };
                        buffer[..size].copy_from_slice(bytes);
                        return Ok(size);
                    }
                    let size = core::mem::size_of::<uapi::snd_timer_read>();
                    if buffer.len() < size {
                        return Err(VfsError::InvalidArgument);
                    }
                    let read = uapi::snd_timer_read {
                        resolution: record.resolution,
                        ticks: record.ticks,
                    };
                    let bytes = unsafe {
                        core::slice::from_raw_parts(&read as *const _ as *const u8, size)
                    };
                    buffer[..size].copy_from_slice(bytes);
                    return Ok(size);
                }
            }
            if self.nonblock.load(Ordering::Acquire) {
                return Err(VfsError::WouldBlock);
            }
            let generation = self.generation.load(Ordering::Acquire);
            self.wait
                .wait_until(|| self.generation.load(Ordering::Acquire) != generation);
        }
    }

    fn write_at(&self, _offset: u64, _buffer: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::NotSupported)
    }

    fn poll(&self) -> u32 {
        if self.queue.lock().is_empty() {
            0
        } else {
            super::pcm::POLLIN
        }
    }

    fn poll_sources(&self, sources: &mut pollwait::PollSources) {
        sources.object(self);
        sources.tick();
    }
}

#[cfg(feature = "boot-self-test")]
pub fn test_session() -> Arc<TimerSession> {
    TimerSession::session(false)
}

#[cfg(feature = "boot-self-test")]
fn utimer_self_test() -> bool {
    let Ok((id, fd)) = create_utimer(1_000_000) else {
        return false;
    };
    let client = TimerSession::session(false);
    *client.bound.lock() = Some(TimerRef {
        class: uapi::SNDRV_TIMER_CLASS_GLOBAL,
        card: -1,
        device: uapi::SNDRV_TIMER_GLOBAL_UDRIVEN,
        subdevice: id as i32,
    });
    {
        let mut params = client.params.lock();
        params.ticks = 1;
        params.queue_size = 32;
        params.running = true;
        params.paused = false;
    }
    let timer = UTIMERS
        .lock()
        .iter()
        .find_map(|weak| weak.upgrade().filter(|timer| timer.id == id));
    let Some(timer) = timer else {
        return false;
    };
    timer.trigger();
    let queued = client.queue.lock().len();
    client.params.lock().running = false;
    let _ = crate::vfs::current_fd_table().lock().close(fd as usize);
    queued >= 1
}

#[cfg(feature = "boot-self-test")]
pub fn self_test_late() -> bool {
    let session = TimerSession::session(false);
    {
        let mut params = session.params.lock();
        params.ticks = 1;
        params.queue_size = 32;
        params.running = true;
        params.paused = false;
    }
    restart(&session);
    crate::sched::sleep_micros(50_000);
    let queued = session.queue.lock().len();
    {
        let mut params = session.params.lock();
        params.running = false;
        params.paused = false;
    }
    session.arm_generation.fetch_add(1, Ordering::AcqRel);
    queued >= 2 && utimer_self_test()
}

/// Register `/dev/snd/timer`.  The global timer device has the fixed minor 33,
/// as `include/sound/minors.h` defines.
pub fn init() {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return;
    }
    let session: Arc<dyn Inode> = Arc::new(TimerSession::template());
    let backend = card::minor_device(uapi::SNDRV_MINOR_TIMER as u32, session);
    let _ = crate::device::register(
        "timer",
        super::SUBSYSTEM,
        Some("snd/timer"),
        Some(backend),
        Vec::new(),
    );
}
