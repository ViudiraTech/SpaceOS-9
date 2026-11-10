/*
 *
 *       src/sound/rawmidi.rs
 *       ALSA RawMIDI interface (/dev/snd/midiC*D*)
 *
 *       2026/11/10 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! RawMIDI uses one device node per card/device.  A file opened read-only sees
//! the input stream, write-only the output stream and read-write both.  The
//! core owns byte queues, parameters, blocking, poll and the `'W'` ioctls;
//! drivers receive bytes with [`RawMidi::receive`] and pull output with the
//! `snd_rawmidi_transmit*` helpers, as in Linux `sound/core/rawmidi.c`.

use alloc::{
    collections::VecDeque,
    string::String,
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

use super::{from_errno, pcm::POLLIN, pcm::POLLOUT, set_string, uapi};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Output,
    Input,
}

impl Stream {
    fn index(self) -> i32 {
        match self {
            Self::Output => uapi::SNDRV_RAWMIDI_STREAM_OUTPUT,
            Self::Input => uapi::SNDRV_RAWMIDI_STREAM_INPUT,
        }
    }
}

pub trait RawMidiOps: Send + Sync {
    fn open(&self, _stream: Stream) -> Result<(), i32> {
        Ok(())
    }
    fn close(&self, _stream: Stream) {}
    fn trigger(&self, _stream: Stream, _up: bool) -> Result<(), i32> {
        Ok(())
    }
    fn drain(&self, _stream: Stream) {}
}

struct Params {
    buffer_size: usize,
    avail_min: usize,
    no_active_sensing: bool,
    mode: u32,
}

pub struct StreamState {
    direction: Stream,
    buffer: Mutex<VecDeque<u8>>,
    params: Mutex<Params>,
    wait: WaitQueue,
    generation: AtomicU64,
    used: AtomicU64,
    limit: AtomicU64,
    xruns: AtomicU64,
    tstamp: Mutex<uapi::timespec>,
    opened: AtomicBool,
    ops: Mutex<Option<Arc<dyn RawMidiOps>>>,
    self_reference: crate::sync::SpinLock<Weak<StreamState>>,
}

impl StreamState {
    fn new(direction: Stream) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            direction,
            buffer: Mutex::new(VecDeque::with_capacity(4096)),
            params: Mutex::new(Params {
                buffer_size: 4096,
                avail_min: 1,
                no_active_sensing: false,
                mode: 0,
            }),
            wait: WaitQueue::new(),
            generation: AtomicU64::new(0),
            used: AtomicU64::new(0),
            limit: AtomicU64::new(4096),
            xruns: AtomicU64::new(0),
            tstamp: Mutex::new(uapi::timespec::zero()),
            opened: AtomicBool::new(false),
            ops: Mutex::new(None),
            self_reference: crate::sync::SpinLock::new(weak.clone()),
        })
    }

    fn publish(&self) {
        let used = self.buffer.lock().len() as u64;
        self.used.store(used, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        *self.tstamp.lock() = uapi::timespec::from_micros(time::uptime_micros());
        pollwait::wake_object(self as *const StreamState);
        self.wait.wake_all();
    }

    fn ops(&self) -> Option<Arc<dyn RawMidiOps>> {
        self.ops.lock().clone()
    }

    pub fn avail(&self) -> usize {
        let used = self.buffer.lock().len();
        let params = self.params.lock();
        match self.direction {
            Stream::Input => used,
            Stream::Output => params.buffer_size.saturating_sub(used),
        }
    }

    fn write_bytes(&self, data: &[u8], nonblock: bool) -> Result<usize, VfsError> {
        let mut written = 0;
        while written < data.len() {
            {
                let mut buffer = self.buffer.lock();
                let limit = self.params.lock().buffer_size;
                if buffer.len() < limit {
                    let take = (limit - buffer.len()).min(data.len() - written);
                    buffer.extend(&data[written..written + take]);
                    written += take;
                    drop(buffer);
                    self.publish();
                    continue;
                }
            }
            if nonblock {
                if written == 0 {
                    return Err(VfsError::WouldBlock);
                }
                break;
            }
            let generation = self.generation.load(Ordering::Acquire);
            self.wait.wait_until(|| {
                self.generation.load(Ordering::Acquire) != generation
                    && self.used.load(Ordering::Acquire) < self.limit.load(Ordering::Acquire)
            });
        }
        if let Some(ops) = self.ops()
            && let Err(error) = ops.trigger(Stream::Output, true)
        {
            return Err(from_errno(error));
        }
        Ok(written)
    }

    fn read_bytes(&self, data: &mut [u8], nonblock: bool) -> Result<usize, VfsError> {
        if data.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut buffer = self.buffer.lock();
                if !buffer.is_empty() {
                    let take = buffer.len().min(data.len());
                    for slot in &mut data[..take] {
                        *slot = buffer.pop_front().unwrap();
                    }
                    drop(buffer);
                    self.publish();
                    return Ok(take);
                }
            }
            if nonblock {
                return Err(VfsError::WouldBlock);
            }
            let generation = self.generation.load(Ordering::Acquire);
            self.wait.wait_until(|| {
                self.generation.load(Ordering::Acquire) != generation
                    && self.used.load(Ordering::Acquire) != 0
            });
        }
    }

    /// Push bytes toward userspace (input stream).  Excess bytes are dropped
    /// and counted as xruns, as Linux does when the input queue is full.
    pub fn receive(&self, data: &[u8]) -> usize {
        let accepted;
        {
            let mut buffer = self.buffer.lock();
            let limit = self.params.lock().buffer_size;
            let space = limit.saturating_sub(buffer.len());
            accepted = space.min(data.len());
            buffer.extend(&data[..accepted]);
        }
        let dropped = data.len() - accepted;
        if dropped != 0 {
            self.xruns.fetch_add(dropped as u64, Ordering::AcqRel);
        }
        if accepted != 0 {
            self.publish();
        }
        accepted
    }

    pub fn transmit_peek(&self, data: &mut [u8]) -> usize {
        let buffer = self.buffer.lock();
        let take = buffer.len().min(data.len());
        for (slot, byte) in data.iter_mut().zip(buffer.iter()) {
            *slot = *byte;
        }
        take
    }

    pub fn transmit_ack(&self, count: usize) -> usize {
        let mut buffer = self.buffer.lock();
        let take = count.min(buffer.len());
        for _ in 0..take {
            buffer.pop_front();
        }
        drop(buffer);
        if take != 0 {
            self.publish();
        }
        take
    }

    pub fn drop_bytes(&self) {
        self.buffer.lock().clear();
        self.publish();
    }

    pub fn drain(&self) -> Result<(), VfsError> {
        loop {
            if self.buffer.lock().is_empty() {
                break;
            }
            let generation = self.generation.load(Ordering::Acquire);
            self.wait.wait_until(|| {
                self.generation.load(Ordering::Acquire) != generation
                    && self.used.load(Ordering::Acquire) == 0
            });
        }
        if let Some(ops) = self.ops() {
            ops.drain(Stream::Output);
        }
        Ok(())
    }

    pub fn info(
        &self,
        card: u32,
        device: u32,
        id: &str,
        name: &str,
        flags: u32,
    ) -> uapi::snd_rawmidi_info {
        let mut info = uapi::snd_rawmidi_info::zeroed();
        info.device = device;
        info.subdevice = 0;
        info.stream = self.direction.index();
        info.card = card as i32;
        info.flags = flags;
        set_string(&mut info.id, id);
        set_string(&mut info.name, name);
        set_string(&mut info.subname, "subdevice #0");
        info.subdevices_count = 1;
        info.subdevices_avail = u32::from(!self.opened.load(Ordering::Acquire));
        info
    }

    pub fn status(&self) -> uapi::snd_rawmidi_status {
        let mut status = uapi::snd_rawmidi_status::zeroed();
        status.stream = self.direction.index();
        status.tstamp = *self.tstamp.lock();
        status.avail = self.avail();
        status.xruns = self.xruns.swap(0, Ordering::AcqRel) as usize;
        status
    }

    pub fn params(&self) -> uapi::snd_rawmidi_params {
        let params = self.params.lock();
        let mut value = uapi::snd_rawmidi_params::zeroed();
        value.stream = self.direction.index();
        value.buffer_size = params.buffer_size;
        value.avail_min = params.avail_min;
        value.no_active_sensing = u32::from(params.no_active_sensing);
        value.mode = params.mode;
        value
    }

    pub fn set_params(&self, value: &uapi::snd_rawmidi_params) -> Result<(), VfsError> {
        let size = value.buffer_size.clamp(32, 64 * 1024);
        let avail = value.avail_min.min(size).max(1);
        {
            let mut params = self.params.lock();
            params.buffer_size = size;
            params.avail_min = avail;
            params.no_active_sensing = value.no_active_sensing & 1 != 0;
            params.mode = value.mode;
        }
        self.limit.store(size as u64, Ordering::Release);
        let mut buffer = self.buffer.lock();
        while buffer.len() > size {
            buffer.pop_front();
        }
        drop(buffer);
        self.publish();
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.used.load(Ordering::Acquire) == 0
    }

    fn poll(&self) -> u32 {
        let avail = self.avail();
        let threshold = self.params.lock().avail_min;
        if avail < threshold {
            return 0;
        }
        match self.direction {
            Stream::Input => POLLIN,
            Stream::Output => POLLOUT,
        }
    }

    fn poll_sources(&self, sources: &mut pollwait::PollSources) {
        sources.object(self);
        sources.tick();
    }
}

pub struct RawMidi {
    pub card: u32,
    pub device: u32,
    pub id: String,
    pub name: String,
    pub info_flags: u32,
    pub output: Option<Arc<StreamState>>,
    pub input: Option<Arc<StreamState>>,
}

impl RawMidi {
    pub fn new(
        card: u32,
        device: u32,
        id: &str,
        name: &str,
        output: bool,
        input: bool,
    ) -> Arc<Self> {
        let mut flags = 0;
        if output {
            flags |= uapi::SNDRV_RAWMIDI_INFO_OUTPUT;
        }
        if input {
            flags |= uapi::SNDRV_RAWMIDI_INFO_INPUT;
        }
        if output && input {
            flags |= uapi::SNDRV_RAWMIDI_INFO_DUPLEX;
        }
        Arc::new(Self {
            card,
            device,
            id: String::from(id),
            name: String::from(name),
            info_flags: flags,
            output: output.then(|| StreamState::new(Stream::Output)),
            input: input.then(|| StreamState::new(Stream::Input)),
        })
    }

    pub fn stream(&self, direction: Stream) -> Option<Arc<StreamState>> {
        match direction {
            Stream::Output => self.output.clone(),
            Stream::Input => self.input.clone(),
        }
    }

    pub fn set_ops(&self, direction: Stream, ops: Arc<dyn RawMidiOps>) {
        if let Some(stream) = self.stream(direction) {
            *stream.ops.lock() = Some(ops);
        }
    }

    pub fn receive(&self, data: &[u8]) -> usize {
        self.input.as_ref().map_or(0, |stream| stream.receive(data))
    }

    pub fn transmit(&self, data: &mut [u8]) -> usize {
        let Some(stream) = &self.output else {
            return 0;
        };
        let count = stream.transmit_peek(data);
        stream.transmit_ack(count);
        count
    }

    pub fn node_name(&self) -> String {
        alloc::format!("midiC{}D{}", self.card, self.device)
    }
}

pub struct RawMidiDeviceNode {
    midi: Arc<RawMidi>,
}

impl RawMidiDeviceNode {
    pub fn new(midi: Arc<RawMidi>) -> Arc<Self> {
        Arc::new(Self { midi })
    }
}

pub struct RawMidiSession {
    midi: Arc<RawMidi>,
    input: Option<Arc<StreamState>>,
    output: Option<Arc<StreamState>>,
    nonblock: AtomicBool,
}

impl RawMidiSession {
    fn session(
        midi: Arc<RawMidi>,
        input: Option<Arc<StreamState>>,
        output: Option<Arc<StreamState>>,
        nonblock: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            midi,
            input,
            output,
            nonblock: AtomicBool::new(nonblock),
        })
    }
}

impl Drop for RawMidiSession {
    fn drop(&mut self) {
        for stream in [self.input.as_ref(), self.output.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(ops) = stream.ops() {
                ops.close(stream.direction);
            }
            stream.opened.store(false, Ordering::Release);
            stream.publish();
        }
    }
}

impl Inode for RawMidiDeviceNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        let access = flags & abi::O_ACCMODE;
        let want_input = access == abi::O_RDONLY || access == abi::O_RDWR;
        let want_output = access == abi::O_WRONLY || access == abi::O_RDWR;
        let input = if want_input {
            Some(self.midi.input.clone().ok_or(VfsError::NoDevice)?)
        } else {
            None
        };
        let output = if want_output {
            Some(self.midi.output.clone().ok_or(VfsError::NoDevice)?)
        } else {
            None
        };
        let mut claimed = Vec::new();
        for stream in [input.as_ref(), output.as_ref()].into_iter().flatten() {
            if stream.opened.swap(true, Ordering::AcqRel) {
                for claimed in claimed.drain(..) {
                    let claimed: &Arc<StreamState> = claimed;
                    claimed.opened.store(false, Ordering::Release);
                }
                return Err(VfsError::WouldBlock);
            }
            claimed.push(stream);
        }
        for stream in [input.as_ref(), output.as_ref()].into_iter().flatten() {
            if let Some(ops) = stream.ops()
                && let Err(error) = ops.open(stream.direction)
            {
                for claimed in [input.as_ref(), output.as_ref()].into_iter().flatten() {
                    claimed.opened.store(false, Ordering::Release);
                }
                return Err(from_errno(error));
            }
        }
        let nonblock = flags & abi::O_NONBLOCK != 0;
        Ok(Some(RawMidiSession::session(
            self.midi.clone(),
            input,
            output,
            nonblock,
        )))
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        rawmidi_ioctl(&self.midi, None, None, cmd as u32, arg)
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        match &self.midi.input {
            Some(stream) => stream.read_bytes(buffer, true),
            None => Err(VfsError::NoDevice),
        }
    }

    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        match &self.midi.output {
            Some(stream) => stream.write_bytes(buffer, false),
            None => Err(VfsError::NoDevice),
        }
    }
}

impl Inode for RawMidiSession {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(ino(), InodeKind::CharDevice, 0o660, 0))
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> Result<u64, VfsError> {
        rawmidi_ioctl(
            &self.midi,
            self.input.as_ref(),
            self.output.as_ref(),
            cmd as u32,
            arg,
        )
    }
    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        let stream = self.input.as_ref().ok_or(VfsError::NoDevice)?;
        stream.read_bytes(buffer, self.nonblock.load(Ordering::Acquire))
    }
    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        let stream = self.output.as_ref().ok_or(VfsError::NoDevice)?;
        stream.write_bytes(buffer, self.nonblock.load(Ordering::Acquire))
    }
    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.nonblock.store(nonblock, Ordering::Release);
        Ok(())
    }
    fn poll(&self) -> u32 {
        let mut events = 0;
        if let Some(stream) = &self.input {
            events |= stream.poll();
        }
        if let Some(stream) = &self.output {
            events |= stream.poll();
        }
        events
    }
    fn poll_sources(&self, sources: &mut pollwait::PollSources) {
        if let Some(stream) = &self.input {
            stream.poll_sources(sources);
        }
        if let Some(stream) = &self.output {
            stream.poll_sources(sources);
        }
    }
}

fn selected_stream(
    midi: &RawMidi,
    input: Option<&Arc<StreamState>>,
    output: Option<&Arc<StreamState>>,
    direction: i32,
) -> Option<Arc<StreamState>> {
    match direction {
        uapi::SNDRV_RAWMIDI_STREAM_INPUT => input.cloned().or_else(|| midi.input.clone()),
        uapi::SNDRV_RAWMIDI_STREAM_OUTPUT => output.cloned().or_else(|| midi.output.clone()),
        _ => None,
    }
}

fn rawmidi_ioctl(
    midi: &Arc<RawMidi>,
    input: Option<&Arc<StreamState>>,
    output: Option<&Arc<StreamState>>,
    cmd: u32,
    arg: u64,
) -> Result<u64, VfsError> {
    match cmd {
        uapi::SNDRV_RAWMIDI_IOCTL_PVERSION => {
            uaccess::write_value(arg, &(uapi::SNDRV_RAWMIDI_VERSION as i32))?;
            Ok(0)
        }
        uapi::SNDRV_RAWMIDI_IOCTL_INFO => {
            let mut info = uaccess::read_value::<uapi::snd_rawmidi_info>(arg)?;
            let stream = selected_stream(midi, input, output, info.stream)
                .or_else(|| input.cloned())
                .or_else(|| output.cloned())
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
        uapi::SNDRV_RAWMIDI_IOCTL_USER_PVERSION => {
            let _ = uaccess::read_value::<i32>(arg)?;
            Ok(0)
        }
        uapi::SNDRV_RAWMIDI_IOCTL_PARAMS => {
            let params = uaccess::read_value::<uapi::snd_rawmidi_params>(arg)?;
            let stream =
                selected_stream(midi, input, output, params.stream).ok_or(VfsError::NoDevice)?;
            stream.set_params(&params)?;
            let mut response = stream.params();
            response.stream = params.stream;
            uaccess::write_value(arg, &response)?;
            Ok(0)
        }
        uapi::SNDRV_RAWMIDI_IOCTL_STATUS => {
            let request = uaccess::read_value::<uapi::snd_rawmidi_status>(arg)?;
            let stream =
                selected_stream(midi, input, output, request.stream).ok_or(VfsError::NoDevice)?;
            let status = stream.status();
            uaccess::write_value(arg, &status)?;
            Ok(0)
        }
        uapi::SNDRV_RAWMIDI_IOCTL_DROP => {
            let direction = uaccess::read_value::<i32>(arg)?;
            let stream =
                selected_stream(midi, input, output, direction).ok_or(VfsError::NoDevice)?;
            stream.drop_bytes();
            Ok(0)
        }
        uapi::SNDRV_RAWMIDI_IOCTL_DRAIN => {
            let direction = uaccess::read_value::<i32>(arg)?;
            let stream =
                selected_stream(midi, input, output, direction).ok_or(VfsError::NoDevice)?;
            if stream.direction == Stream::Output {
                stream.drain()?;
            }
            Ok(0)
        }
        _ => Err(VfsError::NotATty),
    }
}
