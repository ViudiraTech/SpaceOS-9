/*
 *
 *       src/signal/mod.rs
 *       Linux POSIX signal state, queues, delivery and system calls
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Process-directed and thread-directed signal state.  Dispositions live in a
//! `SignalState` shared by CLONE_SIGHAND threads; the pending queue, blocked mask
//! and alternate stack are per thread.  Standard signals coalesce, real-time
//! signals queue in order.
//!
//! Delivery happens at the two user-return boundaries that have a complete
//! register frame: syscall exit and the timer interrupt stub.  The handler is
//! entered by rewriting that frame, and `rt_sigreturn` restores it.

use crate::{
    sched,
    sync::SpinLock,
    user::uaccess,
    vfs::{VfsError, abi},
};
use alloc::vec::Vec;

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGILL: u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGABRT: u32 = 6;
pub const SIGBUS: u32 = 7;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGTTIN: u32 = 21;
pub const SIGTTOU: u32 = 22;
pub const SIGWINCH: u32 = 28;
pub const SIGSYS: u32 = 31;
pub const SIGRTMIN: u32 = 32;
pub const SIGRTMAX: u32 = 64;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

pub const SA_NOCLDSTOP: u64 = 1;
pub const SA_NOCLDWAIT: u64 = 2;
pub const SA_SIGINFO: u64 = 4;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_KNOWN: u64 = SA_NOCLDSTOP
    | SA_NOCLDWAIT
    | SA_SIGINFO
    | SA_ONSTACK
    | SA_RESTART
    | SA_NODEFER
    | SA_RESETHAND
    | SA_RESTORER;

pub const SS_ONSTACK: u32 = 1;
pub const SS_DISABLE: u32 = 2;
pub const MINSIGSTKSZ: u64 = 2048;

pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 0x80;
pub const SI_TKILL: i32 = -6;

const SIG_BLOCK: u64 = 0;
const SIG_UNBLOCK: u64 = 1;
const SIG_SETMASK: u64 = 2;

const SIGSET_SIZE: u64 = 8;
pub const SIGKILL_MASK: u64 = 1 << (SIGKILL - 1);
pub const SIGSTOP_MASK: u64 = 1 << (SIGSTOP - 1);

#[derive(Clone, Copy, Debug)]
pub struct Signal {
    pub number: u32,
    pub code: i32,
    pub pid: u32,
    pub uid: u32,
    pub value: u64,
}

impl Signal {
    pub fn user(number: u32, sender: u32) -> Self {
        Self {
            number,
            code: SI_USER,
            pid: sender,
            uid: 0,
            value: 0,
        }
    }

    pub fn kernel(number: u32) -> Self {
        Self {
            number,
            code: SI_KERNEL,
            pid: 0,
            uid: 0,
            value: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

impl SigAction {
    pub const fn default() -> Self {
        Self {
            handler: SIG_DFL,
            flags: 0,
            restorer: 0,
            mask: 0,
        }
    }
}

/// `struct sighand_struct`: dispositions and the process-wide pending queue.
pub struct SignalState {
    actions: SpinLock<[SigAction; 65]>,
    shared: SpinLock<Vec<Signal>>,
    // Linux's signal->group_exec_task: while an exec is de-threading, a fatal
    // signal makes the receiving sibling exit itself instead of zapping the
    // whole group.
    exec_leader: SpinLock<Option<u64>>,
}

impl SignalState {
    pub fn new() -> Self {
        Self {
            actions: SpinLock::new([SigAction::default(); 65]),
            shared: SpinLock::new(Vec::new()),
            exec_leader: SpinLock::new(None),
        }
    }

    pub fn fork_copy(&self) -> Self {
        let actions = *self.actions.lock();
        Self {
            actions: SpinLock::new(actions),
            shared: SpinLock::new(Vec::new()),
            exec_leader: SpinLock::new(None),
        }
    }

    pub fn exec_leader(&self) -> Option<u64> {
        *self.exec_leader.lock()
    }

    pub fn set_exec_leader(&self, leader: Option<u64>) {
        *self.exec_leader.lock() = leader;
    }

    pub fn shared_pending(&self) -> crate::sync::Guard<'_, Vec<Signal>> {
        self.shared.lock()
    }

    pub fn action(&self, number: u32) -> SigAction {
        self.actions.lock()[number as usize]
    }

    pub fn set_action(&self, number: u32, action: SigAction) {
        self.actions.lock()[number as usize] = action;
    }
}

impl Default for SignalState {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-thread signal state (`struct signal_struct` parts that are not shared).
#[derive(Clone)]
pub struct ThreadSignals {
    pub pending: Vec<Signal>,
    pub blocked: u64,
    pub altstack_sp: u64,
    pub altstack_size: u64,
    pub altstack_flags: u32,
    pub on_altstack: bool,
    pub stopped: bool,
    pub stop_signal: u32,
    pub sigsuspend_restore: Option<u64>,
}

impl ThreadSignals {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            blocked: 0,
            altstack_sp: 0,
            altstack_size: 0,
            altstack_flags: SS_DISABLE,
            on_altstack: false,
            stopped: false,
            stop_signal: 0,
            sigsuspend_restore: None,
        }
    }

    pub fn pending_mask(&self) -> u64 {
        self.pending
            .iter()
            .fold(0u64, |mask, signal| mask | signal_bit(signal.number))
    }
}

impl Default for ThreadSignals {
    fn default() -> Self {
        Self::new()
    }
}

pub fn signal_bit(number: u32) -> u64 {
    if number == 0 || number > SIGRTMAX {
        return 0;
    }
    1u64 << (number - 1)
}

fn enqueue(queue: &mut Vec<Signal>, signal: Signal) {
    if signal.number < SIGRTMIN && queue.iter().any(|queued| queued.number == signal.number) {
        return;
    }
    queue.push(signal);
    crate::ipc::poll_kick();
}

/// Lowest-numbered standard signal first; real-time signals stay FIFO.
fn take_narrowest(queue: &mut Vec<Signal>, blocked: u64) -> Option<Signal> {
    let mut best: Option<usize> = None;
    for (index, signal) in queue.iter().enumerate() {
        if signal_bit(signal.number) & blocked != 0 {
            continue;
        }
        if signal.number >= SIGRTMIN {
            if best.is_none() {
                best = Some(index);
            }
            continue;
        }
        match best {
            Some(current) if queue[current].number <= signal.number => {}
            _ => best = Some(index),
        }
    }
    best.map(|index| queue.remove(index))
}

pub fn send_to_thread(task: &sched::TaskRef, signal: Signal) {
    if signal.number == 0 || signal.number as u64 > SIGRTMAX as u64 {
        return;
    }
    let thread = task.thread_signals();
    let mut state = thread.lock();
    enqueue(&mut state.pending, signal);
    drop(state);
    sched::set_need_resched();
}

pub fn send_to_process(pid: u64, signal: Signal) -> bool {
    let members = sched::tasks_in_group(pid);
    if members.is_empty() {
        return false;
    }
    if pid == sched::init_pid() && pid != 0 {
        // Linux SIGNAL_UNKILLABLE: init only receives signals it handles.
        let leader = members
            .iter()
            .find(|member| {
                let stats = member.stats();
                stats.ltid == stats.tgid
            })
            .unwrap_or(&members[0]);
        let action = leader.signals().action(signal.number);
        if action.handler == SIG_DFL
            || signal.number == SIGKILL
            || signal.number == SIGSTOP
            || signal.number == SIGCONT
        {
            return true;
        }
    }
    if signal.number == SIGCONT {
        // SIGCONT is not blockable and resumes every thread even when it is
        // parked in the default stop action.
        for member in &members {
            let thread = member.thread_signals();
            {
                let mut state = thread.lock();
                state.stopped = false;
                state.stop_signal = 0;
            }
            send_to_thread(member, signal);
        }
        let leader = members
            .iter()
            .find(|member| {
                let stats = member.stats();
                stats.ltid == stats.tgid
            })
            .cloned()
            .unwrap_or_else(|| members[0].clone());
        let stats = leader.stats();
        {
            let mut task = leader.lock();
            task.stop_signal = 0;
            task.stop_reported = false;
            task.continue_pending = true;
        }
        notify_parent_event(stats.ppid, stats.tgid, CLD_CONTINUED, SIGCONT as i32);
        return true;
    }
    if is_stop_signal(signal.number) {
        // Process-directed stop applies to every thread in the group.
        for member in &members {
            send_to_thread(member, signal);
        }
        return true;
    }
    let bit = signal_bit(signal.number);
    for member in &members {
        let state = member.thread_signals();
        if state.lock().blocked & bit == 0 {
            send_to_thread(member, signal);
            return true;
        }
    }
    // Every thread blocks it: leave it in the shared queue for a future mask
    // change to observe.
    let shared = members[0].signals();
    let mut queue = shared.shared_pending();
    enqueue(&mut queue, signal);
    true
}

/// Raise SIGPIPE for a failed write on a pipe/socket in the current process.
pub fn raise_sigpipe() {
    if let Some(task) = sched::current() {
        let stats = task.stats();
        if stats.user {
            let _ = send_to_process(stats.tgid, Signal::kernel(SIGPIPE));
        }
    }
}

/// Deliver a kernel-generated signal to a process group (TTY control).
pub fn send_to_pgrp(pgid: u64, number: u32) -> bool {
    if number == 0 || number > SIGRTMAX {
        return false;
    }
    send_to_group(pgid, Signal::kernel(number))
}

/// True when a masked signal is pending for the current thread or its group.
pub fn pending_masked(mask: u64) -> bool {
    let Some(task) = sched::current() else {
        return false;
    };
    let thread = task.thread_signals();
    if thread
        .lock()
        .pending
        .iter()
        .any(|signal| signal_bit(signal.number) & mask != 0)
    {
        return true;
    }
    task.signals()
        .shared_pending()
        .iter()
        .any(|signal| signal_bit(signal.number) & mask != 0)
}

/// Dequeue the lowest matching signal from the thread queue, then the shared
/// queue.  Used by signalfd.
pub fn take_pending_masked(mask: u64) -> Option<Signal> {
    let task = sched::current()?;
    let thread = task.thread_signals();
    let signal = {
        let mut state = thread.lock();
        take_narrowest(&mut state.pending, !mask)
    };
    if signal.is_some() {
        return signal;
    }
    let shared = task.signals();
    let mut queue = shared.shared_pending();
    take_narrowest(&mut queue, !mask)
}

/// `rt_sigtimedwait(2)`: synchronously dequeue one of `set`.
pub fn rt_sigtimedwait(set: u64, info: u64, timeout: u64, sigsetsize: u64) -> i64 {
    if sigsetsize != SIGSET_SIZE {
        return -abi::EINVAL;
    }
    let mask = match uaccess::read_value::<u64>(set) {
        Ok(mask) => mask & !(SIGKILL_MASK | SIGSTOP_MASK),
        Err(error) => return error.to_errno(),
    };
    let deadline = if timeout == 0 {
        None
    } else {
        let value: [i64; 2] = match uaccess::read_value(timeout) {
            Ok(value) => value,
            Err(error) => return error.to_errno(),
        };
        if value[0] < 0 || !(0..1_000_000_000).contains(&value[1]) {
            return -abi::EINVAL;
        }
        Some(
            crate::time::uptime_micros()
                .saturating_add((value[0] as u64).saturating_mul(1_000_000))
                .saturating_add((value[1] as u64).div_ceil(1000)),
        )
    };
    loop {
        if let Some(signal) = take_pending_masked(mask) {
            if info != 0 {
                let mut raw = [0u8; 128];
                raw[0..4].copy_from_slice(&signal.number.to_le_bytes());
                if let Err(error) = uaccess::write(info, &raw) {
                    return error.to_errno();
                }
            }
            return i64::from(signal.number);
        }
        if deadline.is_some_and(|deadline| crate::time::uptime_micros() >= deadline) {
            return -abi::EAGAIN;
        }
        if pending_for_current() {
            return -abi::EINTR;
        }
        crate::sched::sleep_micros(1000);
    }
}

/// True when the signal is blocked or its disposition is SIG_IGN for `task`.
pub fn signal_ignored(task: &sched::TaskRef, number: u32) -> bool {
    if task.thread_signals().lock().blocked & signal_bit(number) != 0 {
        return true;
    }
    task.signals().action(number).handler == SIG_IGN
}

/// POSIX orphaned process group: no member has a parent in a different
/// process group of the same session.
pub fn pgrp_orphaned(pgid: u64, sid: u64) -> bool {
    let tasks = sched::tasks();
    let members: Vec<_> = tasks
        .iter()
        .filter(|task| {
            let stats = task.stats();
            stats.user && stats.pgid == pgid && stats.sid == sid
        })
        .collect();
    if members.is_empty() {
        return true;
    }
    for member in members {
        let ppid = member.stats().ppid;
        if let Some(parent) = tasks.iter().find(|task| {
            let stats = task.stats();
            stats.user && stats.tgid == ppid
        }) {
            let parent_stats = parent.stats();
            if parent_stats.sid == sid && parent_stats.pgid != pgid {
                return false;
            }
        }
    }
    true
}

fn is_stop_signal(number: u32) -> bool {
    matches!(number, SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU)
}

fn notify_parent_event(ppid: u64, pid: u64, code: i32, status: i32) {
    if ppid == 0 {
        return;
    }
    let signal = Signal {
        number: SIGCHLD,
        code,
        pid: pid as u32,
        uid: 0,
        value: status as u32 as u64,
    };
    let _ = send_to_process(ppid, signal);
}

pub fn current_pid() -> u32 {
    sched::current()
        .map(|task| task.stats().tgid as u32)
        .unwrap_or(0)
}

/// Permission model: this kernel has a single root user, so existence and range
/// checks remain, while uid/gid checks cannot fail.
fn send_by_pid(pid: i64, signal: Signal) -> Result<(), i64> {
    let current_pid = current_pid() as i64;
    if signal.number == 0 || signal.number as u64 > SIGRTMAX as u64 {
        return Err(abi::EINVAL);
    }
    let mut delivered = false;
    if pid > 0 {
        delivered = send_to_process(pid as u64, signal);
    } else if pid == 0 {
        let pgid = sched::current().map(|task| task.stats().pgid).unwrap_or(0);
        delivered = send_to_group(pgid, signal);
    } else if pid == -1 {
        for task in sched::tasks() {
            let stats = task.stats();
            if stats.ltid == 0 || stats.ltid != stats.tgid || stats.tgid == current_pid as u64 {
                continue;
            }
            delivered |= send_to_process(stats.tgid, signal);
        }
    } else {
        delivered = send_to_group((-pid) as u64, signal);
    }
    if delivered { Ok(()) } else { Err(abi::ESRCH) }
}

fn send_to_group(pgid: u64, signal: Signal) -> bool {
    let mut delivered = false;
    for task in sched::tasks() {
        let stats = task.stats();
        if stats.pgid == pgid && stats.ltid != 0 && stats.ltid == stats.tgid {
            delivered |= send_to_process(stats.tgid, signal);
        }
    }
    delivered
}

pub fn kill(pid: i64, sig: u64) -> i64 {
    if sig > SIGRTMAX as u64 {
        return -abi::EINVAL;
    }
    if sig == 0 {
        // Signal 0 only performs existence/permission checking; it must never
        // enter the pending queue (Linux kill(2)).
        let current = current_pid() as i64;
        let tasks = sched::tasks();
        let exists = if pid > 0 {
            tasks.iter().any(|task| task.stats().tgid == pid as u64)
        } else if pid == 0 {
            let pgid = sched::current()
                .map(|task| task.stats().pgid)
                .unwrap_or(u64::MAX);
            tasks.iter().any(|task| task.stats().pgid == pgid)
        } else if pid == -1 {
            tasks.iter().any(|task| task.stats().tgid != current as u64)
                || sched::current().is_some()
        } else {
            let pgid = (-pid) as u64;
            tasks.iter().any(|task| task.stats().pgid == pgid)
        };
        return if exists { 0 } else { -abi::ESRCH };
    }
    let signal = Signal::user(sig as u32, current_pid());
    match send_by_pid(pid, signal) {
        Ok(()) => 0,
        Err(error) => -error,
    }
}

pub fn tkill(tid: u64, sig: u64) -> i64 {
    if sig > SIGRTMAX as u64 {
        return -abi::EINVAL;
    }
    let Some(task) = sched::find_by_ltid(tid) else {
        return -abi::ESRCH;
    };
    if sig == 0 {
        return 0;
    }
    send_to_thread(&task, Signal::user(sig as u32, current_pid()));
    0
}

pub fn tgkill(tgid: u64, tid: u64, sig: u64) -> i64 {
    if sig > SIGRTMAX as u64 {
        return -abi::EINVAL;
    }
    let Some(task) = sched::find_by_ltid(tid) else {
        return -abi::ESRCH;
    };
    if task.stats().tgid != tgid {
        return -abi::EINVAL;
    }
    if sig == 0 {
        return 0;
    }
    send_to_thread(&task, Signal::user(sig as u32, current_pid()));
    0
}

// ---- signal frame layout (x86_64) -----------------------------------------

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use super::*;
    use crate::arch::{InterruptFrame, SyscallFrame};

    pub const FRAME_PRETCODE: usize = 0;
    pub const FRAME_UCONTEXT: usize = 8;
    pub const UCONTEXT_FLAGS: usize = 0;
    pub const UCONTEXT_LINK: usize = 8;
    pub const UCONTEXT_STACK: usize = 16;
    pub const UCONTEXT_MCONTEXT: usize = 40;
    pub const UCONTEXT_SIGMASK: usize = 296;
    pub const UCONTEXT_SIZE: usize = 304;
    pub const FRAME_SIGINFO: usize = FRAME_UCONTEXT + UCONTEXT_SIZE;
    /// With the frame placed at 64n - 8, this offset keeps the FPU image on a
    /// 64-byte boundary as XSAVE requires.
    pub const FRAME_FPSTATE: usize = 456;
    pub const FRAME_BASE: usize = FRAME_FPSTATE + 512;
    /// System V x86-64: leaf functions may use 128 bytes below RSP without
    /// moving it, and that memory must survive an asynchronous signal.
    pub const RED_ZONE: u64 = 128;
    pub const SIGCONTEXT_SIZE: usize = 256;

    const SC_R8: usize = 0;
    const SC_R9: usize = 8;
    const SC_R10: usize = 16;
    const SC_R11: usize = 24;
    const SC_R12: usize = 32;
    const SC_R13: usize = 40;
    const SC_R14: usize = 48;
    const SC_R15: usize = 56;
    const SC_RDI: usize = 64;
    const SC_RSI: usize = 72;
    const SC_RBP: usize = 80;
    const SC_RBX: usize = 88;
    const SC_RDX: usize = 96;
    const SC_RAX: usize = 104;
    const SC_RCX: usize = 112;
    const SC_RSP: usize = 120;
    const SC_RIP: usize = 128;
    const SC_EFLAGS: usize = 136;
    const SC_CS: usize = 144;
    const SC_GS: usize = 146;
    const SC_FS: usize = 148;
    const SC_SS: usize = 150;
    const SC_ERR: usize = 152;
    const SC_TRAPNO: usize = 160;
    const SC_OLDMASK: usize = 168;
    const SC_CR2: usize = 176;
    const SC_FPSTATE: usize = 184;

    #[derive(Clone, Copy, Default)]
    pub struct UserRegs {
        pub r15: u64,
        pub r14: u64,
        pub r13: u64,
        pub r12: u64,
        pub rbp: u64,
        pub rbx: u64,
        pub r11: u64,
        pub rcx: u64,
        pub r9: u64,
        pub r8: u64,
        pub r10: u64,
        pub rdx: u64,
        pub rsi: u64,
        pub rdi: u64,
        pub rax: u64,
        pub rsp: u64,
        pub rip: u64,
        pub flags: u64,
    }

    impl UserRegs {
        pub fn from_syscall(frame: &SyscallFrame) -> Self {
            Self {
                r15: frame.gpr.r15,
                r14: frame.gpr.r14,
                r13: frame.gpr.r13,
                r12: frame.gpr.r12,
                rbp: frame.gpr.rbp,
                rbx: frame.gpr.rbx,
                r11: frame.gpr.r11,
                rcx: frame.gpr.rcx,
                r9: frame.gpr.r9,
                r8: frame.gpr.r8,
                r10: frame.gpr.r10,
                rdx: frame.gpr.rdx,
                rsi: frame.gpr.rsi,
                rdi: frame.gpr.rdi,
                rax: frame.gpr.rax,
                rsp: frame.rsp,
                rip: frame.rip,
                flags: frame.flags,
            }
        }

        pub fn to_syscall(self, frame: &mut SyscallFrame) {
            frame.gpr.r15 = self.r15;
            frame.gpr.r14 = self.r14;
            frame.gpr.r13 = self.r13;
            frame.gpr.r12 = self.r12;
            frame.gpr.rbp = self.rbp;
            frame.gpr.rbx = self.rbx;
            frame.gpr.r11 = self.r11;
            frame.gpr.rcx = self.rcx;
            frame.gpr.r9 = self.r9;
            frame.gpr.r8 = self.r8;
            frame.gpr.r10 = self.r10;
            frame.gpr.rdx = self.rdx;
            frame.gpr.rsi = self.rsi;
            frame.gpr.rdi = self.rdi;
            frame.gpr.rax = self.rax;
            frame.rsp = self.rsp;
            frame.rip = self.rip;
            frame.flags = self.flags;
        }

        pub fn from_interrupt(frame: &InterruptFrame) -> Self {
            Self {
                r15: frame.gpr.r15,
                r14: frame.gpr.r14,
                r13: frame.gpr.r13,
                r12: frame.gpr.r12,
                rbp: frame.gpr.rbp,
                rbx: frame.gpr.rbx,
                r11: frame.gpr.r11,
                rcx: frame.gpr.rcx,
                r9: frame.gpr.r9,
                r8: frame.gpr.r8,
                r10: frame.gpr.r10,
                rdx: frame.gpr.rdx,
                rsi: frame.gpr.rsi,
                rdi: frame.gpr.rdi,
                rax: frame.gpr.rax,
                rsp: frame.rsp,
                rip: frame.rip,
                flags: frame.flags,
            }
        }

        pub fn to_interrupt(self, frame: &mut InterruptFrame) {
            frame.gpr.r15 = self.r15;
            frame.gpr.r14 = self.r14;
            frame.gpr.r13 = self.r13;
            frame.gpr.r12 = self.r12;
            frame.gpr.rbp = self.rbp;
            frame.gpr.rbx = self.rbx;
            frame.gpr.r11 = self.r11;
            frame.gpr.rcx = self.rcx;
            frame.gpr.r9 = self.r9;
            frame.gpr.r8 = self.r8;
            frame.gpr.r10 = self.r10;
            frame.gpr.rdx = self.rdx;
            frame.gpr.rsi = self.rsi;
            frame.gpr.rdi = self.rdi;
            frame.gpr.rax = self.rax;
            frame.rip = self.rip;
            frame.flags = self.flags;
            frame.rsp = self.rsp;
        }
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn get_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn get_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    fn write_siginfo(bytes: &mut [u8], signal: &Signal) {
        put_u32(bytes, 0, signal.number);
        put_u32(bytes, 4, 0);
        put_u32(bytes, 8, signal.code as u32);
        put_u32(bytes, 16, signal.pid);
        put_u32(bytes, 20, signal.uid);
        if signal.number == SIGCHLD {
            // _sigchld.si_status occupies the first four bytes of the union
            // after pid/uid.
            put_u32(bytes, 24, signal.value as u32);
        } else {
            put_u64(bytes, 24, signal.value);
        }
    }

    pub fn setup_frame(
        task: &sched::TaskRef,
        regs: &mut UserRegs,
        signal: Signal,
        action: SigAction,
        blocked: u64,
    ) -> Result<(), VfsError> {
        let thread = task.thread_signals();
        let (stack_pointer, stack_size, stack_flags, on_stack) = {
            let state = thread.lock();
            (
                state.altstack_sp,
                state.altstack_size,
                state.altstack_flags,
                state.on_altstack,
            )
        };
        let use_altstack = action.flags & SA_ONSTACK != 0
            && stack_flags & SS_DISABLE == 0
            && stack_pointer != 0
            && stack_size >= MINSIGSTKSZ
            && !on_stack;
        let base_sp = if use_altstack {
            stack_pointer.saturating_add(stack_size)
        } else {
            regs.rsp.saturating_sub(RED_ZONE)
        };

        let mut fpu = [0u8; 4096];
        let fpu_len = task.fpu_image(&mut fpu).min(4096);
        let frame_size = (FRAME_FPSTATE + fpu_len.max(512) + 63) & !63;
        if base_sp <= (frame_size as u64 + 4096) {
            return Err(VfsError::Fault);
        }
        // The handler is entered as if called: RSP points at the return address
        // (`pretcode`), so RSP + 8 must be 16-byte aligned.
        let frame_address = ((base_sp - frame_size as u64) & !63) - 8;
        let mut frame = alloc::vec![0u8; frame_size];

        put_u64(&mut frame, FRAME_PRETCODE, action.restorer);
        put_u64(&mut frame, FRAME_UCONTEXT + UCONTEXT_FLAGS, 0);
        put_u64(&mut frame, FRAME_UCONTEXT + UCONTEXT_LINK, 0);
        put_u64(&mut frame, FRAME_UCONTEXT + UCONTEXT_STACK, stack_pointer);
        put_u32(
            &mut frame,
            FRAME_UCONTEXT + UCONTEXT_STACK + 8,
            if on_stack { SS_ONSTACK } else { stack_flags },
        );
        put_u64(&mut frame, FRAME_UCONTEXT + UCONTEXT_STACK + 16, stack_size);

        let context = FRAME_UCONTEXT + UCONTEXT_MCONTEXT;
        put_u64(&mut frame, context + SC_R8, regs.r8);
        put_u64(&mut frame, context + SC_R9, regs.r9);
        put_u64(&mut frame, context + SC_R10, regs.r10);
        put_u64(&mut frame, context + SC_R11, regs.r11);
        put_u64(&mut frame, context + SC_R12, regs.r12);
        put_u64(&mut frame, context + SC_R13, regs.r13);
        put_u64(&mut frame, context + SC_R14, regs.r14);
        put_u64(&mut frame, context + SC_R15, regs.r15);
        put_u64(&mut frame, context + SC_RDI, regs.rdi);
        put_u64(&mut frame, context + SC_RSI, regs.rsi);
        put_u64(&mut frame, context + SC_RBP, regs.rbp);
        put_u64(&mut frame, context + SC_RBX, regs.rbx);
        put_u64(&mut frame, context + SC_RDX, regs.rdx);
        put_u64(&mut frame, context + SC_RAX, regs.rax);
        put_u64(&mut frame, context + SC_RCX, regs.rcx);
        put_u64(&mut frame, context + SC_RSP, regs.rsp);
        put_u64(&mut frame, context + SC_RIP, regs.rip);
        put_u64(&mut frame, context + SC_EFLAGS, regs.flags);
        frame[context + SC_CS..context + SC_CS + 2].copy_from_slice(&0x33u16.to_le_bytes());
        frame[context + SC_GS..context + SC_GS + 2].copy_from_slice(&0u16.to_le_bytes());
        frame[context + SC_FS..context + SC_FS + 2].copy_from_slice(&0u16.to_le_bytes());
        frame[context + SC_SS..context + SC_SS + 2].copy_from_slice(&0x2bu16.to_le_bytes());
        put_u64(&mut frame, context + SC_ERR, 0);
        put_u64(&mut frame, context + SC_TRAPNO, 0);
        put_u64(&mut frame, context + SC_OLDMASK, blocked);
        put_u64(&mut frame, context + SC_CR2, 0);
        let fpstate_address = if fpu_len != 0 {
            frame[FRAME_FPSTATE..FRAME_FPSTATE + fpu_len].copy_from_slice(&fpu[..fpu_len]);
            frame_address + FRAME_FPSTATE as u64
        } else {
            0
        };
        put_u64(&mut frame, context + SC_FPSTATE, fpstate_address);
        put_u64(&mut frame, FRAME_UCONTEXT + UCONTEXT_SIGMASK, blocked);
        write_siginfo(&mut frame[FRAME_SIGINFO..FRAME_SIGINFO + 128], &signal);

        uaccess::write(frame_address, &frame)?;

        regs.rip = action.handler;
        regs.rsp = frame_address;
        regs.flags = (regs.flags & !0x400) | 0x200;
        regs.rdi = signal.number as u64;
        regs.rsi = frame_address + FRAME_SIGINFO as u64;
        regs.rdx = frame_address + FRAME_UCONTEXT as u64;
        regs.rax = 0;

        let mut state = thread.lock();
        state.on_altstack = use_altstack;
        Ok(())
    }

    pub fn restore_frame(task: &sched::TaskRef, regs: &mut UserRegs) -> Result<(), VfsError> {
        let frame_address = regs.rsp.checked_sub(8).ok_or(VfsError::Fault)?;
        let mut header = [0u8; 8];
        uaccess::read(frame_address, &mut header)?;
        let size = FRAME_FPSTATE + 512;
        let frame = uaccess::read_vec(frame_address, size, size)?;
        let context = FRAME_UCONTEXT + UCONTEXT_MCONTEXT;
        let old_blocked =
            get_u64(&frame, FRAME_UCONTEXT + UCONTEXT_SIGMASK) & !(SIGKILL_MASK | SIGSTOP_MASK);
        regs.r8 = get_u64(&frame, context + SC_R8);
        regs.r9 = get_u64(&frame, context + SC_R9);
        regs.r10 = get_u64(&frame, context + SC_R10);
        regs.r11 = get_u64(&frame, context + SC_R11);
        regs.r12 = get_u64(&frame, context + SC_R12);
        regs.r13 = get_u64(&frame, context + SC_R13);
        regs.r14 = get_u64(&frame, context + SC_R14);
        regs.r15 = get_u64(&frame, context + SC_R15);
        regs.rdi = get_u64(&frame, context + SC_RDI);
        regs.rsi = get_u64(&frame, context + SC_RSI);
        regs.rbp = get_u64(&frame, context + SC_RBP);
        regs.rbx = get_u64(&frame, context + SC_RBX);
        regs.rdx = get_u64(&frame, context + SC_RDX);
        regs.rax = get_u64(&frame, context + SC_RAX);
        regs.rcx = get_u64(&frame, context + SC_RCX);
        regs.rsp = get_u64(&frame, context + SC_RSP);
        regs.rip = get_u64(&frame, context + SC_RIP);
        regs.flags = get_u64(&frame, context + SC_EFLAGS) | 0x200;
        let fpstate = get_u64(&frame, context + SC_FPSTATE);
        if fpstate != 0 {
            let mut image = [0u8; 4096];
            let length = task.fpu_size().min(4096);
            if let Ok(()) = uaccess::read(fpstate, &mut image[..length]) {
                let _ = task.restore_fpu_image(&image[..length]);
            }
        }
        let thread = task.thread_signals();
        let (stack_sp, stack_size, stack_flags) = {
            let mut state = thread.lock();
            state.blocked = old_blocked;
            state.on_altstack = state.altstack_flags & SS_DISABLE == 0
                && state.altstack_sp != 0
                && regs.rsp >= state.altstack_sp
                && regs.rsp < state.altstack_sp.saturating_add(state.altstack_size);
            if let Some(mask) = state.sigsuspend_restore.take() {
                state.blocked = mask & !(SIGKILL_MASK | SIGSTOP_MASK);
            }
            (state.altstack_sp, state.altstack_size, state.altstack_flags)
        };
        let _ = (stack_sp, stack_size, stack_flags);
        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
pub use x86_64::{UserRegs, restore_frame, setup_frame};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DefaultAction {
    Ignore,
    Stop,
    Terminate(bool),
}

fn default_action(number: u32) -> DefaultAction {
    match number {
        SIGCHLD | SIGCONT | 23 | SIGWINCH => DefaultAction::Ignore,
        SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => DefaultAction::Stop,
        SIGQUIT | SIGILL | SIGTRAP | SIGABRT | SIGBUS | SIGFPE | SIGSEGV | 24 | 25 | SIGSYS => {
            DefaultAction::Terminate(true)
        }
        _ => DefaultAction::Terminate(false),
    }
}

fn take_pending(task: &sched::TaskRef) -> Option<(Signal, SigAction)> {
    let thread = task.thread_signals();
    let mut state = thread.lock();
    let blocked = state.blocked;
    let signal = take_narrowest(&mut state.pending, blocked);
    drop(state);
    let signal = match signal {
        Some(signal) => signal,
        None => {
            let shared = task.signals();
            let mut queue = shared.shared_pending();
            take_narrowest(&mut queue, blocked)?
        }
    };
    Some((signal, task.signals().action(signal.number)))
}

fn update_blocked(task: &sched::TaskRef, signal: u32, action: SigAction) {
    let thread = task.thread_signals();
    let mut state = thread.lock();
    let mut blocked = state.blocked | (action.mask & !(SIGKILL_MASK | SIGSTOP_MASK));
    if action.flags & SA_NODEFER == 0 {
        blocked |= signal_bit(signal);
    }
    state.blocked = blocked & !(SIGKILL_MASK | SIGSTOP_MASK);
}

fn terminate_group(task: &sched::TaskRef, signal: u32, core: bool) -> ! {
    let stats = task.stats();
    if let Some(exec_leader) = task.signals().exec_leader() {
        // de_thread is in progress: only this thread exits (Linux
        // do_group_exit sees group_exec_task and skips zap_other_threads).
        let status = if stats.ltid == exec_leader {
            0
        } else {
            signal as i32 | if core { 0x80 } else { 0 }
        };
        crate::sched::exit(status);
    }
    if stats.tgid != 0 {
        crate::tty::vt::release_owner(stats.tgid);
        for member in sched::tasks_in_group(stats.tgid) {
            if member.stats().ltid != stats.ltid {
                send_to_thread(&member, Signal::kernel(SIGKILL));
            }
        }
    }
    let status = signal as i32 | if core { 0x80 } else { 0 };
    crate::sched::exit(status)
}

#[cfg(target_arch = "x86_64")]
fn stop_current(task: &sched::TaskRef, signal: u32) {
    let stats = task.stats();
    let thread = task.thread_signals();
    {
        let mut state = thread.lock();
        state.stopped = true;
        state.stop_signal = signal;
    }
    if stats.user && stats.ltid == stats.tgid {
        let mut task = task.lock();
        task.stop_signal = signal;
        task.stop_reported = false;
    }
    notify_parent_event(stats.ppid, stats.tgid, CLD_STOPPED, signal as i32);
    // SIGCONT clears `stopped` from the sender side; a fatal pending signal
    // also ends the park so the delivery loop can terminate the task.
    while thread.lock().stopped && !pending_for_current() {
        crate::sched::sleep_micros(1000);
    }
}

#[cfg(target_arch = "x86_64")]
fn deliver(task: &sched::TaskRef, mut regs: UserRegs) -> Option<UserRegs> {
    loop {
        let Some((signal, action)) = take_pending(task) else {
            return None;
        };
        match action.handler {
            SIG_IGN => continue,
            SIG_DFL => match default_action(signal.number) {
                DefaultAction::Ignore => {
                    if signal.number == SIGCONT {
                        let thread = task.thread_signals();
                        let mut state = thread.lock();
                        state.stopped = false;
                        state.stop_signal = 0;
                    }
                    continue;
                }
                DefaultAction::Stop => {
                    let stats = task.stats();
                    // Job-control stop signals do not stop an orphaned group;
                    // SIGSTOP always does.
                    if signal.number != SIGSTOP && pgrp_orphaned(stats.pgid, stats.sid) {
                        continue;
                    }
                    stop_current(task, signal.number);
                    continue;
                }
                DefaultAction::Terminate(core) => terminate_group(task, signal.number, core),
            },
            handler => {
                let action = SigAction { handler, ..action };
                let thread = task.thread_signals();
                let blocked = thread.lock().blocked;
                if setup_frame(task, &mut regs, signal, action, blocked).is_err() {
                    terminate_group(task, SIGSEGV, true);
                }
                update_blocked(task, signal.number, action);
                if action.flags & SA_RESETHAND != 0 {
                    task.signals()
                        .set_action(signal.number, SigAction::default());
                }
                return Some(regs);
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
pub fn deliver_on_syscall_return(frame: &mut crate::arch::SyscallFrame) {
    let Some(task) = sched::current() else {
        return;
    };
    if let Some(regs) = deliver(&task, UserRegs::from_syscall(frame)) {
        regs.to_syscall(frame);
    } else {
        // A temporary wait mask outlives the syscall only while a handler that
        // it admitted is running; rt_sigreturn restores it in that case.
        let thread = task.thread_signals();
        let mut state = thread.lock();
        if let Some(mask) = state.sigsuspend_restore.take() {
            state.blocked = mask & !(SIGKILL_MASK | SIGSTOP_MASK);
        }
    }
}

#[cfg(target_arch = "x86_64")]
pub fn deliver_on_interrupt(frame: &mut crate::arch::InterruptFrame) {
    let Some(task) = sched::current() else {
        return;
    };
    if let Some(regs) = deliver(&task, UserRegs::from_interrupt(frame)) {
        regs.to_interrupt(frame);
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn deliver_on_syscall_return(_frame: &mut ()) {}

#[cfg(not(target_arch = "x86_64"))]
pub fn deliver_on_interrupt(_frame: &mut ()) {}

/// Restore the frame saved by `setup_frame`; called with IRQs disabled.
#[cfg(target_arch = "x86_64")]
pub fn rt_sigreturn(frame: &mut crate::arch::SyscallFrame) {
    let Some(task) = sched::current() else {
        return;
    };
    let mut regs = UserRegs::from_syscall(frame);
    if restore_frame(&task, &mut regs).is_err() {
        terminate_group(&task, SIGSEGV, true);
    }
    regs.to_syscall(frame);
}

// ---- system calls ----------------------------------------------------------

fn read_action(address: u64) -> Result<SigAction, VfsError> {
    let bytes = uaccess::read_vec(address, 32, 32)?;
    #[cfg(target_arch = "x86_64")]
    {
        Ok(SigAction {
            handler: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            flags: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            restorer: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            mask: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        })
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Ok(SigAction {
            handler: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            mask: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            flags: u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as u64,
            restorer: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        })
    }
}

fn write_action(address: u64, action: &SigAction) -> Result<(), VfsError> {
    let mut bytes = [0u8; 32];
    #[cfg(target_arch = "x86_64")]
    {
        bytes[0..8].copy_from_slice(&action.handler.to_le_bytes());
        bytes[8..16].copy_from_slice(&action.flags.to_le_bytes());
        bytes[16..24].copy_from_slice(&action.restorer.to_le_bytes());
        bytes[24..32].copy_from_slice(&action.mask.to_le_bytes());
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        bytes[0..8].copy_from_slice(&action.handler.to_le_bytes());
        bytes[8..16].copy_from_slice(&action.mask.to_le_bytes());
        bytes[16..20].copy_from_slice(&(action.flags as u32).to_le_bytes());
        bytes[24..32].copy_from_slice(&action.restorer.to_le_bytes());
    }
    uaccess::write(address, &bytes)
}

pub fn rt_sigaction(signo: u64, act: u64, oldact: u64, sigsetsize: u64) -> i64 {
    if sigsetsize != SIGSET_SIZE || signo == 0 || signo > SIGRTMAX as u64 {
        return -abi::EINVAL;
    }
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    let signals = task.signals();
    if oldact != 0 {
        let mut old = signals.action(signo as u32);
        old.flags &= SA_KNOWN;
        if let Err(error) = write_action(oldact, &old) {
            return error.to_errno();
        }
    }
    if act != 0 {
        if signo == SIGKILL as u64 || signo == SIGSTOP as u64 {
            return -abi::EINVAL;
        }
        let mut action = match read_action(act) {
            Ok(action) => action,
            Err(error) => return error.to_errno(),
        };
        if action.handler > 1
            && (action.handler >= crate::user::UserAddressSpace::user_limit()
                || action.handler == 0)
        {
            return -abi::EINVAL;
        }
        action.flags &= SA_KNOWN;
        action.mask &= !(SIGKILL_MASK | SIGSTOP_MASK);
        let discard_pending = action.handler == SIG_IGN
            || (action.handler == SIG_DFL
                && matches!(default_action(signo as u32), DefaultAction::Ignore));
        signals.set_action(signo as u32, action);
        if discard_pending {
            let stats = task.stats();
            let mut shared = signals.shared_pending();
            shared.retain(|pending| pending.number != signo as u32);
            drop(shared);
            for member in sched::tasks_in_group(stats.tgid) {
                member
                    .thread_signals()
                    .lock()
                    .pending
                    .retain(|pending| pending.number != signo as u32);
            }
        }
    }
    0
}

pub fn rt_sigprocmask(how: u64, set: u64, oldset: u64, sigsetsize: u64) -> i64 {
    if sigsetsize != SIGSET_SIZE || how > SIG_SETMASK {
        return -abi::EINVAL;
    }
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    let new_mask = if set != 0 {
        match uaccess::read_value::<u64>(set) {
            Ok(mask) => mask & !(SIGKILL_MASK | SIGSTOP_MASK),
            Err(error) => return error.to_errno(),
        }
    } else {
        0
    };
    let thread = task.thread_signals();
    let mut state = thread.lock();
    let old = state.blocked;
    if set != 0 {
        state.blocked = match how {
            SIG_BLOCK => old | new_mask,
            SIG_UNBLOCK => old & !new_mask,
            _ => new_mask,
        } & !(SIGKILL_MASK | SIGSTOP_MASK);
    }
    drop(state);
    if oldset != 0 {
        if let Err(error) = uaccess::write_value(oldset, &old) {
            return error.to_errno();
        }
    }
    0
}

pub fn rt_sigpending(set: u64, sigsetsize: u64) -> i64 {
    if sigsetsize != SIGSET_SIZE {
        return -abi::EINVAL;
    }
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    let mask = task.thread_signals().lock().pending_mask()
        | task
            .signals()
            .shared_pending()
            .iter()
            .fold(0, |mask, signal| mask | signal_bit(signal.number));
    match uaccess::write_value(set, &mask) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn sigaltstack(ss: u64, old_ss: u64) -> i64 {
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    let thread = task.thread_signals();
    if old_ss != 0 {
        let state = thread.lock();
        let stack = [
            state.altstack_sp,
            if state.on_altstack {
                SS_ONSTACK as u64
            } else {
                state.altstack_flags as u64
            },
            state.altstack_size,
        ];
        drop(state);
        if let Err(error) = uaccess::write_value(old_ss, &stack) {
            return error.to_errno();
        }
    }
    if ss != 0 {
        let stack = match uaccess::read_value::<[u64; 3]>(ss) {
            Ok(stack) => stack,
            Err(error) => return error.to_errno(),
        };
        let [sp, flags, size] = stack;
        let flags = flags as u32;
        if flags & !(SS_ONSTACK | SS_DISABLE) != 0 || flags & SS_ONSTACK != 0 {
            return -abi::EINVAL;
        }
        if flags & SS_DISABLE == 0 && size < MINSIGSTKSZ {
            return -abi::ENOMEM;
        }
        let mut state = thread.lock();
        if state.on_altstack {
            return -abi::EPERM;
        }
        if flags & SS_DISABLE != 0 {
            state.altstack_sp = 0;
            state.altstack_size = 0;
            state.altstack_flags = SS_DISABLE;
        } else {
            if size > crate::user::UserAddressSpace::user_limit() {
                return -abi::ENOMEM;
            }
            state.altstack_sp = sp;
            state.altstack_size = size;
            state.altstack_flags = 0;
        }
    }
    0
}

pub fn pause() -> i64 {
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    loop {
        if has_deliverable(&task) {
            return -abi::EINTR;
        }
        crate::sched::sleep_micros(1000);
    }
}

pub fn rt_sigsuspend(mask: u64, sigsetsize: u64) -> i64 {
    if sigsetsize != SIGSET_SIZE {
        return -abi::EINVAL;
    }
    let Some(task) = sched::current() else {
        return -abi::ESRCH;
    };
    let mask = match uaccess::read_value::<u64>(mask) {
        Ok(mask) => mask & !(SIGKILL_MASK | SIGSTOP_MASK),
        Err(error) => return error.to_errno(),
    };
    install_wait_mask(mask);
    loop {
        if has_deliverable(&task) {
            break;
        }
        crate::sched::sleep_micros(1000);
    }
    -abi::EINTR
}

/// Replace the blocked mask for the rest of this syscall (`ppoll`, `pselect`,
/// `sigsuspend`). The previous mask returns when the syscall exits or, if a
/// signal handler is delivered, when that handler returns.
pub fn install_wait_mask(mask: u64) {
    let Some(task) = sched::current() else {
        return;
    };
    let thread = task.thread_signals();
    let mut state = thread.lock();
    let old = state.blocked;
    state.blocked = mask & !(SIGKILL_MASK | SIGSTOP_MASK);
    state.sigsuspend_restore = Some(old);
}

pub fn pending_for_current() -> bool {
    sched::current()
        .map(|task| has_deliverable(&task))
        .unwrap_or(false)
}

/// True when delivering this signal would actually run a handler, stop, or
/// terminate the task.  Ignored and default-ignore signals (notably SIGCHLD)
/// are excluded so they do not turn interruptible waits into EINTR.
fn interrupts_wait(task: &sched::TaskRef, number: u32, blocked: u64) -> bool {
    if signal_bit(number) & blocked != 0 {
        return false;
    }
    let action = task.signals().action(number);
    if action.handler == SIG_IGN {
        return false;
    }
    if action.handler == SIG_DFL && matches!(default_action(number), DefaultAction::Ignore) {
        return false;
    }
    true
}

fn has_deliverable(task: &sched::TaskRef) -> bool {
    let thread = task.thread_signals();
    let blocked = thread.lock().blocked;
    let local = thread
        .lock()
        .pending
        .iter()
        .any(|signal| interrupts_wait(task, signal.number, blocked));
    if local {
        return true;
    }
    task.signals()
        .shared_pending()
        .iter()
        .any(|signal| interrupts_wait(task, signal.number, blocked))
}

/// Prepare a thread group for `execve`: other threads are killed and waited
/// for, handled dispositions reset (ignored survive with cleared fields),
/// pending signals and the blocked mask are preserved, and the alternate stack
/// is disabled.  A non-leader thread adopts the group leader's PID, matching
/// Linux's de_thread identity exchange.
pub fn prepare_exec(task: &sched::TaskRef) -> Result<(), VfsError> {
    let stats = task.stats();
    let signals = task.signals();
    if stats.user {
        signals.set_exec_leader(Some(stats.ltid));
        for member in sched::tasks_in_group(stats.tgid) {
            if member.stats().ltid != stats.ltid {
                send_to_thread(&member, Signal::kernel(SIGKILL));
            }
        }
        loop {
            let live_other = sched::tasks_in_group(stats.tgid)
                .into_iter()
                .any(|member| member.stats().ltid != stats.ltid);
            if !live_other {
                break;
            }
            if pending_for_current() {
                signals.set_exec_leader(None);
                return Err(VfsError::Interrupted);
            }
            sched::sleep_micros(1000);
        }
        signals.set_exec_leader(None);
        if stats.ltid != stats.tgid {
            // Kernel waits for the old leader to become inactive, discards
            // its zombie record and takes over its PID.
            let _ = sched::take_zombie(stats.tgid);
            task.lock().ltid = stats.tgid;
        }
    }
    {
        let mut actions = signals.actions.lock();
        for action in actions.iter_mut() {
            if action.handler != SIG_IGN {
                action.handler = SIG_DFL;
            }
            action.flags = 0;
            action.restorer = 0;
            action.mask = 0;
        }
    }
    let thread = task.thread_signals();
    {
        let mut state = thread.lock();
        state.altstack_sp = 0;
        state.altstack_size = 0;
        state.altstack_flags = SS_DISABLE;
        state.on_altstack = false;
        state.stopped = false;
        state.stop_signal = 0;
    }
    Ok(())
}

/// Kernel-internal delivery of a fatal signal (e.g. a non-COW user fault).
pub fn force_fatal(signal: u32, core: bool) -> ! {
    let Some(task) = sched::current() else {
        crate::sched::exit(signal as i32 | if core { 0x80 } else { 0 })
    };
    terminate_group(&task, signal, core)
}

/// Send SIGCHLD for a terminating child.  `code` is the `siginfo.si_code`
/// (CLD_EXITED/CLD_KILLED/CLD_DUMPED).
pub fn notify_child_exit(ppid: u64, pid: u64, code: i32, status: i32) {
    if ppid == 0 {
        return;
    }
    let signal = Signal {
        number: SIGCHLD,
        code,
        pid: pid as u32,
        uid: 0,
        value: status as u32 as u64,
    };
    let _ = send_to_process(ppid, signal);
}

pub const CLD_EXITED: i32 = 1;
pub const CLD_KILLED: i32 = 2;
pub const CLD_DUMPED: i32 = 3;
pub const CLD_STOPPED: i32 = 5;
pub const CLD_CONTINUED: i32 = 6;
