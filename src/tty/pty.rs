/*
 *
 *       src/tty/pty.rs
 *       Unix98 pseudo terminals: /dev/ptmx and the devpts filesystem
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Each pair runs a full termios line discipline: POSIX input and output modes,
//! job control and window size. The master side is the terminal emulator; the
//! slave is the application's controlling terminal.

use crate::{
    sync::SpinLock,
    user::uaccess,
    vfs::{DirEntry, FileSystem, Inode, InodeKind, Stat, StatFs, VfsError, abi::makedev},
};
use alloc::{
    collections::{BTreeMap, VecDeque},
    format,
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;
const TCSETSW: u64 = 0x5403;
const TCSETSF: u64 = 0x5404;
const TCSBRK: u64 = 0x5409;
const TCXONC: u64 = 0x540A;
const TCFLSH: u64 = 0x540B;
const TIOCEXCL: u64 = 0x540C;
const TIOCNXCL: u64 = 0x540D;
const TIOCSCTTY: u64 = 0x540E;
const TIOCGPGRP: u64 = 0x540F;
const TIOCSPGRP: u64 = 0x5410;
const TIOCOUTQ: u64 = 0x5411;
const TIOCGWINSZ: u64 = 0x5413;
const TIOCSWINSZ: u64 = 0x5414;
const FIONREAD: u64 = 0x541B;
const TIOCNOTTY: u64 = 0x5422;
const TIOCGSID: u64 = 0x5429;
const TIOCGPTN: u64 = 0x8004_5430;
const TIOCSPTLCK: u64 = 0x4004_5431;
const TIOCGPTLCK: u64 = 0x8004_5439;
const TIOCGPTPEER: u64 = 0x5441;
const TCGETS2: u64 = 0x802C_542A;
const TCSETS2: u64 = 0x402C_542B;
const TCSETSW2: u64 = 0x402C_542C;
const TCSETSF2: u64 = 0x402C_542D;

// c_iflag
const IGNCR: u32 = 0o200;
const ICRNL: u32 = 0o400;
const INLCR: u32 = 0o100;
const IUTF8: u32 = 0o40000;
const IXON: u32 = 0o2000;
// c_oflag
const OPOST: u32 = 0o1;
const ONLCR: u32 = 0o4;
const OCRNL: u32 = 0o10;
// c_lflag
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ECHOE: u32 = 0o20;
const ECHOK: u32 = 0o40;
const ECHONL: u32 = 0o100;
const NOFLSH: u32 = 0o200;
const TOSTOP: u32 = 0o400;
const ECHOCTL: u32 = 0o1000;
const ECHOKE: u32 = 0o4000;
const IEXTEN: u32 = 0o100000;
// c_cc indices
const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VMIN: usize = 6;
const VSUSP: usize = 10;
const VREPRINT: usize = 12;
const VWERASE: usize = 14;
const VLNEXT: usize = 15;

const QUEUE_LIMIT: usize = 64 * 1024;
const LINE_LIMIT: usize = 4095;
const PTS_MAJOR: u64 = 136;
const PTMX_RDEV: u64 = makedev(5, 2);
const MAX_PTYS: usize = 4096;

#[derive(Clone, Copy)]
struct Termios {
    iflag: u32,
    oflag: u32,
    cflag: u32,
    lflag: u32,
    line: u8,
    cc: [u8; 19],
}

impl Termios {
    fn defaults() -> Self {
        let mut cc = [0u8; 19];
        cc[VINTR] = 3;
        cc[VQUIT] = 28;
        cc[VERASE] = 127;
        cc[VKILL] = 21;
        cc[VEOF] = 4;
        cc[VMIN] = 1;
        cc[VSUSP] = 26;
        cc[VREPRINT] = 18;
        cc[VWERASE] = 23;
        cc[VLNEXT] = 22;
        cc[8] = 17; // VSTART
        cc[9] = 19; // VSTOP
        Self {
            iflag: ICRNL | IXON | IUTF8,
            oflag: OPOST | ONLCR,
            cflag: 0o2277, // B38400 | CS8 | CREAD | HUPCL-free default
            lflag: ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
            line: 0,
            cc,
        }
    }

    fn encode(&self) -> [u8; 36] {
        let mut raw = [0u8; 36];
        raw[0..4].copy_from_slice(&self.iflag.to_le_bytes());
        raw[4..8].copy_from_slice(&self.oflag.to_le_bytes());
        raw[8..12].copy_from_slice(&self.cflag.to_le_bytes());
        raw[12..16].copy_from_slice(&self.lflag.to_le_bytes());
        raw[16] = self.line;
        raw[17..36].copy_from_slice(&self.cc);
        raw
    }

    fn decode(raw: &[u8]) -> Self {
        let word = |offset: usize| u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
        let mut cc = [0u8; 19];
        cc.copy_from_slice(&raw[17..36]);
        Self {
            iflag: word(0),
            oflag: word(4),
            cflag: word(8),
            lflag: word(12),
            line: raw[16],
            cc,
        }
    }
}

struct PtyState {
    termios: Termios,
    window: [u16; 4],
    /// Bytes for the master to read (slave output and echo).
    output: VecDeque<u8>,
    /// Bytes available to slave reads.
    input: VecDeque<u8>,
    /// Bytes of the line being edited in canonical mode.
    line: Vec<u8>,
    /// Buffered EOF markers, one per pending end-of-file line.
    eof_pending: usize,
    literal_next: bool,
    master_open: bool,
    slaves: usize,
    ever_opened_slave: bool,
    session: u64,
    foreground: u64,
    locked: bool,
    exclusive: bool,
    nonblock_master: bool,
}

pub struct PtyCore {
    index: u32,
    state: SpinLock<PtyState>,
}

static PTYS: SpinLock<BTreeMap<u32, Weak<PtyCore>>> = SpinLock::new(BTreeMap::new());
static NEXT_INDEX: AtomicU32 = AtomicU32::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);

fn current_ids() -> (u64, u64, u64) {
    crate::sched::current().map_or((0, 0, 0), |task| {
        let stats = task.stats();
        (stats.tgid, stats.pgid, stats.sid)
    })
}

impl PtyCore {
    fn signal_foreground(state: &PtyState, number: u32) {
        if state.foreground != 0 {
            crate::signal::send_to_pgrp(state.foreground, number);
        }
    }

    fn echo_byte(state: &mut PtyState, byte: u8) {
        if state.termios.lflag & ECHOCTL != 0
            && byte < 32
            && !matches!(byte, b'\t' | b'\n' | 0x11 | 0x13)
        {
            state.output.push_back(b'^');
            state.output.push_back(byte + 64);
        } else if byte == b'\n' || byte == b'\r' {
            if byte == b'\n' && state.termios.oflag & ONLCR != 0 {
                state.output.push_back(b'\r');
            }
            state.output.push_back(byte);
        } else {
            state.output.push_back(byte);
        }
    }

    fn erase_one(state: &mut PtyState, echo: bool) {
        // Treat a UTF-8 sequence as one column-erasing unit.
        while let Some(byte) = state.line.pop() {
            if echo && state.termios.lflag & ECHOE != 0 && byte & 0xC0 != 0x80 {
                let extra = if byte < 32 && state.termios.lflag & ECHOCTL != 0 {
                    2
                } else {
                    1
                };
                for _ in 0..extra {
                    state.output.extend(b"\x08 \x08");
                }
            }
            if byte & 0xC0 != 0x80 || state.termios.iflag & IUTF8 == 0 {
                break;
            }
        }
    }

    /// Line-discipline input path: one byte typed at the terminal.
    fn input_byte(state: &mut PtyState, mut byte: u8) {
        let termios = state.termios;
        if state.literal_next {
            state.literal_next = false;
            if termios.lflag & ICANON != 0 {
                state.line.push(byte);
                if termios.lflag & ECHO != 0 {
                    Self::echo_byte(state, byte);
                }
            } else {
                state.input.push_back(byte);
            }
            return;
        }
        if byte == b'\r' {
            if termios.iflag & IGNCR != 0 {
                return;
            }
            if termios.iflag & ICRNL != 0 {
                byte = b'\n';
            }
        } else if byte == b'\n' && termios.iflag & INLCR != 0 {
            byte = b'\r';
        }
        if termios.lflag & ISIG != 0 {
            let signal = if byte == termios.cc[VINTR] {
                Some(crate::signal::SIGINT)
            } else if byte == termios.cc[VQUIT] {
                Some(crate::signal::SIGQUIT)
            } else if byte == termios.cc[VSUSP] {
                Some(crate::signal::SIGTSTP)
            } else {
                None
            };
            if let Some(signal) = signal {
                if termios.lflag & NOFLSH == 0 {
                    state.input.clear();
                    state.line.clear();
                }
                if termios.lflag & ECHO != 0 {
                    Self::echo_byte(state, byte);
                }
                Self::signal_foreground(state, signal);
                return;
            }
        }
        if termios.lflag & IEXTEN != 0 && byte == termios.cc[VLNEXT] && byte != 0 {
            state.literal_next = true;
            return;
        }
        if termios.lflag & ICANON == 0 {
            if state.input.len() < QUEUE_LIMIT {
                state.input.push_back(byte);
            }
            if termios.lflag & ECHO != 0 {
                Self::echo_byte(state, byte);
            }
            return;
        }
        let echo = termios.lflag & ECHO != 0;
        if byte == termios.cc[VERASE] && byte != 0 {
            Self::erase_one(state, echo);
            return;
        }
        if byte == termios.cc[VKILL] && byte != 0 {
            while !state.line.is_empty() {
                Self::erase_one(state, echo && termios.lflag & ECHOK != 0);
            }
            return;
        }
        if termios.lflag & IEXTEN != 0 && byte == termios.cc[VWERASE] && byte != 0 {
            while state.line.last().is_some_and(|c| *c == b' ') {
                Self::erase_one(state, echo);
            }
            while state.line.last().is_some_and(|c| *c != b' ') {
                Self::erase_one(state, echo);
            }
            return;
        }
        if byte == termios.cc[VEOF] && byte != 0 {
            let line = core::mem::take(&mut state.line);
            if line.is_empty() {
                state.eof_pending += 1;
            }
            state.input.extend(line);
            return;
        }
        if byte == b'\n' {
            if echo || termios.lflag & ECHONL != 0 {
                Self::echo_byte(state, byte);
            }
            state.line.push(byte);
            let line = core::mem::take(&mut state.line);
            state.input.extend(line);
            return;
        }
        if state.line.len() < LINE_LIMIT {
            state.line.push(byte);
            if echo {
                Self::echo_byte(state, byte);
            }
        }
    }

    fn output_bytes(state: &mut PtyState, bytes: &[u8]) {
        let flags = state.termios.oflag;
        if flags & OPOST == 0 {
            state.output.extend(bytes);
            return;
        }
        for &byte in bytes {
            if byte == b'\n' && flags & ONLCR != 0 {
                state.output.push_back(b'\r');
                state.output.push_back(b'\n');
            } else if byte == b'\r' && flags & OCRNL != 0 {
                state.output.push_back(b'\n');
            } else {
                state.output.push_back(byte);
            }
        }
    }

    fn set_termios(&self, raw: &[u8], flush_input: bool) {
        let mut state = self.state.lock();
        let was_canonical = state.termios.lflag & ICANON != 0;
        state.termios = Termios::decode(raw);
        if flush_input {
            state.input.clear();
            state.line.clear();
        }
        // Leaving canonical mode makes the partial line readable.
        if was_canonical && state.termios.lflag & ICANON == 0 {
            let line = core::mem::take(&mut state.line);
            state.input.extend(line);
        }
    }

    /// ioctls valid from both ends of the pair.
    fn common_ioctl(
        self: &Arc<Self>,
        command: u64,
        arg: u64,
        slave: bool,
    ) -> Result<u64, VfsError> {
        match command {
            TCGETS => {
                let raw = self.state.lock().termios.encode();
                uaccess::write(arg, &raw)?;
                Ok(0)
            }
            TCGETS2 => {
                let raw = self.state.lock().termios.encode();
                let mut extended = [0u8; 44];
                extended[..36].copy_from_slice(&raw);
                extended[36..40].copy_from_slice(&38400u32.to_le_bytes());
                extended[40..44].copy_from_slice(&38400u32.to_le_bytes());
                uaccess::write(arg, &extended)?;
                Ok(0)
            }
            TCSETS | TCSETSW | TCSETSF => {
                let raw = uaccess::read_vec(arg, 36, 36)?;
                self.set_termios(&raw, command == TCSETSF);
                Ok(0)
            }
            TCSETS2 | TCSETSW2 | TCSETSF2 => {
                let raw = uaccess::read_vec(arg, 44, 44)?;
                self.set_termios(&raw, command == TCSETSF2);
                Ok(0)
            }
            TIOCGWINSZ => {
                let window = self.state.lock().window;
                uaccess::write_value(arg, &window)?;
                Ok(0)
            }
            TIOCSWINSZ => {
                let window: [u16; 4] = uaccess::read_value(arg)?;
                let mut state = self.state.lock();
                if state.window != window {
                    state.window = window;
                    Self::signal_foreground(&state, crate::signal::SIGWINCH);
                }
                Ok(0)
            }
            TCSBRK | TCXONC => Ok(0),
            TCFLSH => {
                let mut state = self.state.lock();
                match arg {
                    0 | 2 => {
                        state.input.clear();
                        state.line.clear();
                    }
                    1 => {}
                    _ => return Err(VfsError::InvalidArgument),
                }
                if matches!(arg, 1 | 2) {
                    state.output.clear();
                }
                Ok(0)
            }
            TIOCEXCL => {
                self.state.lock().exclusive = true;
                Ok(0)
            }
            TIOCNXCL => {
                self.state.lock().exclusive = false;
                Ok(0)
            }
            FIONREAD => {
                let state = self.state.lock();
                let count = if slave {
                    state.input.len()
                } else {
                    state.output.len()
                };
                drop(state);
                uaccess::write_value(arg, &(count as i32))?;
                Ok(0)
            }
            TIOCOUTQ => {
                let count = self.state.lock().output.len();
                uaccess::write_value(arg, &(if slave { count } else { 0 } as i32))?;
                Ok(0)
            }
            TIOCGPGRP => {
                let state = self.state.lock();
                let (_, _, session) = current_ids();
                if slave && state.session != session {
                    return Err(VfsError::NotATty);
                }
                let foreground = state.foreground as i32;
                drop(state);
                uaccess::write_value(arg, &foreground)?;
                Ok(0)
            }
            TIOCSPGRP => {
                let group: i32 = uaccess::read_value(arg)?;
                if group < 0 {
                    return Err(VfsError::InvalidArgument);
                }
                let (_, _, session) = current_ids();
                let mut state = self.state.lock();
                if state.session != session {
                    return Err(VfsError::NotATty);
                }
                state.foreground = group as u64;
                Ok(0)
            }
            TIOCGSID => {
                let session = self.state.lock().session as i32;
                if session == 0 {
                    return Err(VfsError::NotATty);
                }
                uaccess::write_value(arg, &session)?;
                Ok(0)
            }
            TIOCSCTTY => {
                let (pid, group, session) = current_ids();
                if pid != session {
                    return Err(VfsError::NotPermitted);
                }
                let mut state = self.state.lock();
                if state.session == session {
                    return Ok(0);
                }
                let root = crate::sched::current().is_some_and(|task| task.credentials().euid == 0);
                if state.session != 0 && !(arg == 1 && root) {
                    return Err(VfsError::NotPermitted);
                }
                state.session = session;
                state.foreground = group;
                drop(state);
                controlling::attach(session, self);
                Ok(0)
            }
            TIOCNOTTY => {
                let (pid, _, session) = current_ids();
                let mut state = self.state.lock();
                if state.session != session {
                    return Err(VfsError::NotATty);
                }
                if pid == session {
                    let foreground = state.foreground;
                    state.session = 0;
                    state.foreground = 0;
                    drop(state);
                    controlling::detach(session);
                    if foreground != 0 {
                        crate::signal::send_to_pgrp(foreground, crate::signal::SIGHUP);
                        crate::signal::send_to_pgrp(foreground, crate::signal::SIGCONT);
                    }
                }
                Ok(0)
            }
            _ => Err(VfsError::NotATty),
        }
    }
}

/// Session to controlling-terminal lookup behind `/dev/tty`.
pub mod controlling {
    use super::*;

    static SESSIONS: SpinLock<BTreeMap<u64, Weak<PtyCore>>> = SpinLock::new(BTreeMap::new());

    pub(super) fn attach(session: u64, core: &Arc<PtyCore>) {
        SESSIONS.lock().insert(session, Arc::downgrade(core));
    }

    pub(super) fn detach(session: u64) {
        SESSIONS.lock().remove(&session);
    }

    pub(super) fn lookup(session: u64) -> Option<Arc<PtyCore>> {
        SESSIONS.lock().get(&session).and_then(Weak::upgrade)
    }
}

/// Open the calling session's pty when `/dev/tty` refers to one.
pub fn open_controlling() -> Option<Arc<dyn Inode>> {
    let (_, _, session) = current_ids();
    let core = controlling::lookup(session)?;
    core.state.lock().slaves += 1;
    Some(Arc::new(PtySlave { core }))
}

pub struct PtyMaster {
    core: Arc<PtyCore>,
    ino: u64,
}

impl Drop for PtyMaster {
    fn drop(&mut self) {
        let mut state = self.core.state.lock();
        state.master_open = false;
        let foreground = state.foreground;
        let session = state.session;
        drop(state);
        if foreground != 0 {
            crate::signal::send_to_pgrp(foreground, crate::signal::SIGHUP);
            crate::signal::send_to_pgrp(foreground, crate::signal::SIGCONT);
        }
        if session != 0 {
            controlling::detach(session);
        }
        PTYS.lock().remove(&self.core.index);
        LIVE.fetch_sub(1, Ordering::Relaxed);
        crate::ipc::wake_pollers();
    }
}

impl Inode for PtyMaster {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(self.ino, InodeKind::CharDevice, 0o666, 0);
        stat.rdev = PTMX_RDEV;
        Ok(stat)
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.core.state.lock().nonblock_master = nonblock;
        Ok(())
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(Arc::as_ptr(&self.core));
    }

    fn poll(&self) -> u32 {
        let state = self.core.state.lock();
        let mut events = 0;
        if !state.output.is_empty() {
            events |= 1;
        }
        if state.input.len() < QUEUE_LIMIT {
            events |= 4;
        }
        if state.ever_opened_slave && state.slaves == 0 {
            events |= 16;
        }
        events
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut wait = crate::ipc::pollwait::PollWait::new();
        loop {
            wait.arm_object(Arc::as_ptr(&self.core));
            {
                let mut state = self.core.state.lock();
                if !state.output.is_empty() {
                    let count = buffer.len().min(state.output.len());
                    for slot in buffer[..count].iter_mut() {
                        *slot = state.output.pop_front().unwrap();
                    }
                    drop(state);
                    crate::ipc::wake_object(Arc::as_ptr(&self.core));
                    return Ok(count);
                }
                // Linux reports EIO once every slave handle is gone.
                if state.ever_opened_slave && state.slaves == 0 {
                    return Err(VfsError::IoError);
                }
                if state.nonblock_master {
                    return Err(VfsError::WouldBlock);
                }
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    }

    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        let mut state = self.core.state.lock();
        for &byte in buffer {
            PtyCore::input_byte(&mut state, byte);
        }
        drop(state);
        crate::ipc::wake_object(Arc::as_ptr(&self.core));
        Ok(buffer.len())
    }

    fn ioctl(&self, command: u64, arg: u64) -> Result<u64, VfsError> {
        match command {
            TIOCGPTN => {
                uaccess::write_value(arg, &self.core.index)?;
                Ok(0)
            }
            TIOCSPTLCK => {
                let lock: i32 = uaccess::read_value(arg)?;
                self.core.state.lock().locked = lock != 0;
                Ok(0)
            }
            TIOCGPTLCK => {
                let locked = i32::from(self.core.state.lock().locked);
                uaccess::write_value(arg, &locked)?;
                Ok(0)
            }
            TIOCGPTPEER => {
                self.core.state.lock().slaves += 1;
                self.core.state.lock().ever_opened_slave = true;
                let slave: Arc<dyn Inode> = Arc::new(PtySlave {
                    core: self.core.clone(),
                });
                let file = Arc::new(crate::vfs::OpenFile::new(
                    slave,
                    format!("/dev/pts/{}", self.core.index),
                    (arg as u32 & !0o1000000) | 0,
                ));
                let table = crate::vfs::current_fd_table();
                let fd = table.lock().alloc_fd(
                    0,
                    crate::vfs::FdEntry {
                        file,
                        cloexec: arg as u32 & 0o2000000 != 0,
                    },
                )?;
                Ok(fd as u64)
            }
            _ => self.core.common_ioctl(command, arg, false),
        }
    }
}

pub struct PtySlave {
    core: Arc<PtyCore>,
}

impl Drop for PtySlave {
    fn drop(&mut self) {
        let mut state = self.core.state.lock();
        state.slaves = state.slaves.saturating_sub(1);
        drop(state);
        crate::ipc::wake_pollers();
    }
}

impl PtySlave {
    /// Background-group read and write rules from POSIX job control.
    fn job_control_check(&self, reading: bool) -> Result<(), VfsError> {
        let Some(task) = crate::sched::current() else {
            return Ok(());
        };
        let stats = task.stats();
        let state = self.core.state.lock();
        if state.session != stats.sid || state.foreground == 0 || state.foreground == stats.pgid {
            return Ok(());
        }
        if !reading && state.termios.lflag & TOSTOP == 0 {
            return Ok(());
        }
        drop(state);
        let signal = if reading {
            crate::signal::SIGTTIN
        } else {
            crate::signal::SIGTTOU
        };
        if crate::signal::signal_ignored(&task, signal) {
            return if reading {
                Err(VfsError::IoError)
            } else {
                Ok(())
            };
        }
        if crate::signal::pgrp_orphaned(stats.pgid, stats.sid) {
            return Err(VfsError::IoError);
        }
        crate::signal::send_to_pgrp(stats.pgid, signal);
        Err(VfsError::Interrupted)
    }
}

impl Inode for PtySlave {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(
            0x7000_0000 + u64::from(self.core.index),
            InodeKind::CharDevice,
            0o620,
            0,
        );
        stat.rdev = makedev(PTS_MAJOR, u64::from(self.core.index));
        Ok(stat)
    }

    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        sources.object(Arc::as_ptr(&self.core));
    }

    fn poll(&self) -> u32 {
        let state = self.core.state.lock();
        let mut events = 0;
        let canonical = state.termios.lflag & ICANON != 0;
        let _ = canonical;
        if !state.input.is_empty() || state.eof_pending != 0 || !state.master_open {
            events |= 1;
        }
        if state.output.len() < QUEUE_LIMIT {
            events |= 4;
        }
        if !state.master_open {
            events |= 16;
        }
        events
    }

    fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> Result<usize, VfsError> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.job_control_check(true)?;
        let mut wait = crate::ipc::pollwait::PollWait::new();
        loop {
            wait.arm_object(Arc::as_ptr(&self.core));
            {
                let mut state = self.core.state.lock();
                let canonical = state.termios.lflag & ICANON != 0;
                if !state.input.is_empty() {
                    let mut count = 0;
                    while count < buffer.len() {
                        let Some(byte) = state.input.pop_front() else {
                            break;
                        };
                        buffer[count] = byte;
                        count += 1;
                        if canonical && byte == b'\n' {
                            break;
                        }
                    }
                    let minimum = usize::from(state.termios.cc[VMIN]);
                    if canonical || count >= minimum.min(buffer.len()) || minimum == 0 {
                        drop(state);
                        crate::ipc::wake_object(Arc::as_ptr(&self.core));
                        return Ok(count);
                    }
                    for &byte in buffer[..count].iter().rev() {
                        state.input.push_front(byte);
                    }
                } else if state.eof_pending != 0 {
                    state.eof_pending -= 1;
                    return Ok(0);
                } else if !state.master_open {
                    return Ok(0);
                } else if !canonical && state.termios.cc[VMIN] == 0 {
                    return Ok(0);
                }
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            wait.sleep();
        }
    }

    fn write_at(&self, _offset: u64, buffer: &[u8]) -> Result<usize, VfsError> {
        self.job_control_check(false)?;
        let mut state = self.core.state.lock();
        if !state.master_open {
            return Err(VfsError::IoError);
        }
        PtyCore::output_bytes(&mut state, buffer);
        drop(state);
        crate::ipc::wake_object(Arc::as_ptr(&self.core));
        Ok(buffer.len())
    }

    fn ioctl(&self, command: u64, arg: u64) -> Result<u64, VfsError> {
        self.core.common_ioctl(command, arg, true)
    }
}

/// `/dev/ptmx`: every open allocates a new pair.
pub struct PtmxNode;

impl Inode for PtmxNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(0x6FFF_FFFF, InodeKind::CharDevice, 0o666, 0);
        stat.rdev = PTMX_RDEV;
        Ok(stat)
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        if LIVE.fetch_add(1, Ordering::Relaxed) >= MAX_PTYS {
            LIVE.fetch_sub(1, Ordering::Relaxed);
            return Err(VfsError::NoSpace);
        }
        let index = NEXT_INDEX.fetch_add(1, Ordering::Relaxed);
        let core = Arc::new(PtyCore {
            index,
            state: SpinLock::new(PtyState {
                termios: Termios::defaults(),
                window: [24, 80, 0, 0],
                output: VecDeque::new(),
                input: VecDeque::new(),
                line: Vec::new(),
                eof_pending: 0,
                literal_next: false,
                master_open: true,
                slaves: 0,
                ever_opened_slave: false,
                session: 0,
                foreground: 0,
                locked: true,
                exclusive: false,
                nonblock_master: flags & 0o4000 != 0,
            }),
        });
        PTYS.lock().insert(index, Arc::downgrade(&core));
        Ok(Some(Arc::new(PtyMaster {
            core,
            ino: crate::vfs::tree::ino(),
        })))
    }
}

/// Named slave entry in devpts; opening it returns a counted `PtySlave`.
struct PtsNode {
    core: Arc<PtyCore>,
}

impl Inode for PtsNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut stat = Stat::simple(
            0x7000_0000 + u64::from(self.core.index),
            InodeKind::CharDevice,
            0o620,
            0,
        );
        stat.rdev = makedev(PTS_MAJOR, u64::from(self.core.index));
        Ok(stat)
    }

    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        const O_NOCTTY: u32 = 0o400;
        let mut state = self.core.state.lock();
        if state.locked {
            return Err(VfsError::NoSuchDeviceOrAddress);
        }
        if !state.master_open {
            return Err(VfsError::IoError);
        }
        let root = crate::sched::current().is_some_and(|task| task.credentials().euid == 0);
        if state.exclusive && !root {
            return Err(VfsError::Busy);
        }
        state.slaves += 1;
        state.ever_opened_slave = true;
        let (pid, group, session) = current_ids();
        // A session leader without a controlling terminal acquires this one.
        if flags & O_NOCTTY == 0
            && pid != 0
            && pid == session
            && state.session == 0
            && controlling::lookup(session).is_none()
            && crate::tty::controlling_session() != session
        {
            state.session = session;
            state.foreground = group;
            drop(state);
            controlling::attach(session, &self.core);
        }
        Ok(Some(Arc::new(PtySlave {
            core: self.core.clone(),
        })))
    }
}

struct PtsRoot;

impl Inode for PtsRoot {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(0x6FFF_FFFE, InodeKind::Directory, 0o755, 0))
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        if name == "ptmx" {
            return Ok(Arc::new(PtmxNode));
        }
        let index: u32 = name.parse().map_err(|_| VfsError::NotFound)?;
        let core = PTYS
            .lock()
            .get(&index)
            .and_then(Weak::upgrade)
            .ok_or(VfsError::NotFound)?;
        Ok(Arc::new(PtsNode { core }))
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, VfsError> {
        let mut entries = vec![
            DirEntry {
                ino: 0x6FFF_FFFE,
                name: String::from("."),
                kind: InodeKind::Directory,
            },
            DirEntry {
                ino: 1,
                name: String::from(".."),
                kind: InodeKind::Directory,
            },
            DirEntry {
                ino: 0x6FFF_FFFF,
                name: String::from("ptmx"),
                kind: InodeKind::CharDevice,
            },
        ];
        for (&index, weak) in PTYS.lock().iter() {
            if weak.strong_count() != 0 {
                entries.push(DirEntry {
                    ino: 0x7000_0000 + u64::from(index),
                    name: format!("{}", index),
                    kind: InodeKind::CharDevice,
                });
            }
        }
        Ok(entries)
    }
}

pub struct DevPts;

impl FileSystem for DevPts {
    fn fs_type(&self) -> &'static str {
        "devpts"
    }

    fn root_inode(&self) -> Arc<dyn Inode> {
        Arc::new(PtsRoot)
    }

    fn statfs(&self) -> Result<StatFs, VfsError> {
        Ok(StatFs {
            magic: 0x1CD1,
            block_size: 4096,
            total_blocks: 0,
            free_blocks: 0,
            total_files: MAX_PTYS as u64,
            free_files: (MAX_PTYS - LIVE.load(Ordering::Relaxed)) as u64,
            max_name_len: 255,
        })
    }

    fn cache_mode(&self) -> crate::vfs::CacheMode {
        crate::vfs::CacheMode::Revalidate
    }
}
