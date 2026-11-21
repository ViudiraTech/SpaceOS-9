/*
 *
 *       src/sched/task.rs
 *       Kernel task ownership, CPU affinity and checked stack lifetime
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Only kernel threads exist today. User processes, fork, signals and VFS
//! handles are deliberately absent until their resource ownership is implemented.

use crate::{
    arch, fpu,
    sync::{Mutex, SpinLock},
};
use alloc::{
    alloc::{alloc, dealloc},
    boxed::Box,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::signal::{SignalState, ThreadSignals};
use crate::vfs::FsContext;

static NEXT_PID: AtomicU64 = AtomicU64::new(1);

/// Allocate a Linux PID/TID. Kernel tasks never consume this namespace.
pub fn alloc_pid() -> u64 {
    NEXT_PID.fetch_add(1, Ordering::Relaxed)
}

/// Opaque ownership handle; callers can inspect metadata without borrowing the
/// scheduler-private context or changing task/queue identity.
#[derive(Clone)]
pub struct TaskRef(Arc<SpinLock<Task>>);
static TASKS: SpinLock<Vec<(u64, Weak<SpinLock<Task>>)>> = SpinLock::new(Vec::new());
static ZOMBIES: SpinLock<Vec<TaskRef>> = SpinLock::new(Vec::new());
#[derive(Clone)]
pub struct TaskWeak {
    tid: u64,
    inner: Weak<SpinLock<Task>>,
}
impl TaskWeak {
    pub fn upgrade(&self) -> Option<TaskRef> {
        let task = TaskRef(self.inner.upgrade()?);
        if task.stats().state == TaskState::Dead {
            None
        } else {
            Some(task)
        }
    }
    pub fn tid(&self) -> u64 {
        self.tid
    }
}
pub fn find(tid: u64) -> Option<TaskRef> {
    let weak = TASKS
        .lock()
        .iter()
        .find(|(id, _)| *id == tid)
        .map(|(_, w)| w.clone())?;
    let task = TaskRef(weak.upgrade()?);
    if task.stats().state == TaskState::Dead {
        None
    } else {
        Some(task)
    }
}

/// Linux `/proc/<pid>`: locate a process by thread-group id.  User tasks are
/// preferred so an idle/kernel task with the same numeric id cannot shadow
/// PID 1 or a user process.
pub fn find_tgid(tgid: u64) -> Option<TaskRef> {
    let tasks = all();
    tasks
        .iter()
        .find(|task| {
            let stats = task.stats();
            stats.user && stats.tgid == tgid
        })
        .cloned()
        .or_else(|| tasks.into_iter().find(|task| task.stats().tgid == tgid))
}
pub fn all() -> Vec<TaskRef> {
    let weak: Vec<_> = TASKS.lock().iter().map(|(_, w)| w.clone()).collect();
    weak.into_iter()
        .filter_map(|w| w.upgrade().map(TaskRef))
        .filter(|t| t.stats().state != TaskState::Dead)
        .collect()
}
pub(crate) fn push_zombie(task: TaskRef) {
    ZOMBIES.lock().push(task);
}
pub(crate) fn zombies() -> Vec<TaskRef> {
    ZOMBIES.lock().clone()
}
pub(crate) fn take_zombie(pid: u64) -> Option<TaskRef> {
    let mut list = ZOMBIES.lock();
    let index = list.iter().position(|task| task.stats().tgid == pid)?;
    let reaped = list.remove(index);
    reaped.lock().state = TaskState::Dead;
    Some(reaped)
}
pub(crate) fn has_children(ppid: u64) -> bool {
    if ZOMBIES.lock().iter().any(|task| task.stats().ppid == ppid) {
        return true;
    }
    all()
        .iter()
        .any(|task| task.stats().user && task.stats().ppid == ppid)
}
/// Deliver PR_SET_PDEATHSIG to direct children when their parent dies.
pub(crate) fn deliver_pdeathsig(parent: u64) {
    for child in all() {
        let signal = {
            let task = child.lock();
            if task.user && task.ppid == parent {
                task.pdeathsig
            } else {
                0
            }
        };
        if signal != 0 {
            crate::signal::send_to_thread(&child, crate::signal::Signal::kernel(signal));
        }
    }
}

/// Linux subreaper adoption: the nearest living ancestor marked as a child
/// subreaper inherits orphaned descendants, otherwise PID 1 does.
pub(crate) fn nearest_reaper(exiting: u64) -> u64 {
    let mut parent = find(exiting).map(|task| task.stats().ppid).unwrap_or(0);
    for _ in 0..64 {
        if parent == 0 {
            break;
        }
        match find(parent) {
            Some(task) => {
                if task.child_subreaper() {
                    return parent;
                }
                parent = task.stats().ppid;
            }
            None => break,
        }
    }
    1
}

/// Re-parent live children and already-exited zombies after their parent dies.
pub(crate) fn reparent_children(old: u64, new: u64) {
    for task in all() {
        let mut task = task.lock();
        if task.ppid == old {
            task.ppid = new;
        }
    }
    for task in zombies() {
        let mut task = task.lock();
        if task.ppid == old {
            task.ppid = new;
        }
    }
}
pub fn last_tid() -> u64 {
    NEXT_TID.load(Ordering::Relaxed).saturating_sub(1)
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    pub uid: u32,
    pub gid: u32,
    pub euid: u32,
    pub egid: u32,
    pub suid: u32,
    pub sgid: u32,
    pub fsuid: u32,
    pub fsgid: u32,
}

/// Build a Linux `task_struct.comm` value from an executable path.
pub fn comm_from_path(path: &str) -> [u8; 16] {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut comm = [0u8; 16];
    let length = name.len().min(15);
    comm[..length].copy_from_slice(&name.as_bytes()[..length]);
    comm
}

#[derive(Clone, Copy, Debug)]
pub struct TaskStats {
    pub tid: u64,
    pub tgid: u64,
    pub ltid: u64,
    pub ppid: u64,
    pub pgid: u64,
    pub sid: u64,
    pub user: bool,
    pub exit_signal: u32,
    pub nice: i32,
    pub start_ns: u64,
    pub state: TaskState,
    pub cpu: usize,
    pub affinity: CpuMask,
    pub runtime_ns: u64,
    pub user_ns: u64,
    pub system_ns: u64,
    pub switches: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
    pub migrations: u64,
    pub exit_code: i32,
    pub creds: Credentials,
    /// NUL-padded Linux task command name.
    pub comm: [u8; 16],
    pub pdeathsig: u32,
    pub child_subreaper: bool,
}
impl TaskRef {
    /// Release a vfork parent, once. Must be called without this task's lock.
    pub fn complete_vfork(&self) {
        let done = self.lock().vfork_done.take();
        if let Some(done) = done {
            done.complete();
        }
    }

    /// Kernel thread id without taking the task lock (it never changes).
    pub fn tid(&self) -> u64 {
        // SAFETY: `tid` is written once before the task is shared and is
        // immutable afterwards; no reference to the locked value is formed.
        unsafe { core::ptr::addr_of!((*self.0.as_ptr()).tid).read() }
    }

    /// Raw identity for the per-CPU current-task slot.
    pub(super) fn as_raw(&self) -> *const SpinLock<Task> {
        Arc::as_ptr(&self.0)
    }

    /// Take a new reference from a pointer published by `as_raw`.
    ///
    /// # Safety
    /// Some other reference must keep the task alive for the whole call.
    pub(super) unsafe fn clone_from_raw(raw: *const SpinLock<Task>) -> Self {
        // SAFETY: the caller guarantees a live strong reference exists.
        unsafe {
            Arc::increment_strong_count(raw);
            Self(Arc::from_raw(raw))
        }
    }

    pub(crate) fn lock(&self) -> crate::sync::Guard<'_, Task> {
        self.0.lock()
    }
    pub fn downgrade(&self) -> TaskWeak {
        TaskWeak {
            tid: self.stats().tid,
            inner: Arc::downgrade(&self.0),
        }
    }
    pub fn address_space(&self) -> Option<Arc<Mutex<crate::user::UserAddressSpace>>> {
        self.lock().address_space.clone()
    }
    pub fn files(&self) -> Arc<SpinLock<crate::vfs::FileDescriptorTable>> {
        self.lock().fd_table.clone()
    }
    pub fn set_fd_table(&self, table: Arc<SpinLock<crate::vfs::FileDescriptorTable>>) {
        self.lock().fd_table = table;
    }
    pub fn credentials(&self) -> Credentials {
        self.lock().creds
    }
    pub fn set_credentials(&self, creds: Credentials) {
        self.lock().creds = creds;
    }
    pub fn groups(&self) -> Vec<u32> {
        self.lock().groups.clone()
    }
    pub fn set_groups(&self, groups: Vec<u32>) {
        self.lock().groups = groups;
    }
    pub fn set_affinity(&self, affinity: CpuMask) {
        self.lock().affinity = affinity;
    }
    pub fn comm(&self) -> [u8; 16] {
        self.lock().comm
    }
    pub fn set_comm(&self, name: &[u8]) {
        let mut comm = [0u8; 16];
        let length = name.len().min(15);
        comm[..length].copy_from_slice(&name[..length]);
        self.lock().comm = comm;
    }
    pub fn set_comm_from_path(&self, path: &str) {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.set_comm(name.as_bytes());
    }
    pub fn pdeathsig(&self) -> u32 {
        self.lock().pdeathsig
    }
    pub fn set_pdeathsig(&self, signal: u32) {
        self.lock().pdeathsig = signal;
    }
    pub fn dumpable(&self) -> bool {
        self.lock().dumpable
    }
    pub fn set_dumpable(&self, dumpable: bool) {
        self.lock().dumpable = dumpable;
    }
    pub fn no_new_privs(&self) -> bool {
        self.lock().no_new_privs
    }
    pub fn set_no_new_privs(&self, enabled: bool) {
        self.lock().no_new_privs = enabled;
    }
    pub fn child_subreaper(&self) -> bool {
        self.lock().child_subreaper
    }
    pub fn set_child_subreaper(&self, enabled: bool) {
        self.lock().child_subreaper = enabled;
    }
    pub fn robust_list(&self) -> u64 {
        self.lock().robust_list
    }

    pub fn set_robust_list(&self, head: u64) {
        self.lock().robust_list = head;
    }

    /// Record the POSIX nice value that `getpriority` and procfs report.
    pub fn set_nice(&self, nice: i32) {
        self.lock().nice = nice;
    }

    pub fn clear_child_tid(&self) -> u64 {
        self.lock().clear_child_tid
    }
    pub fn fs(&self) -> Arc<SpinLock<FsContext>> {
        self.lock().fs.clone()
    }
    pub fn set_fs(&self, fs: Arc<SpinLock<FsContext>>) {
        self.lock().fs = fs;
    }
    pub fn mount_namespace(&self) -> Arc<SpinLock<crate::vfs::MountNamespace>> {
        self.lock().mount_ns.clone()
    }
    pub fn set_mount_namespace(&self, ns: Arc<SpinLock<crate::vfs::MountNamespace>>) {
        self.lock().mount_ns = ns;
    }
    pub fn seccomp_filters(&self) -> Vec<Arc<crate::seccomp::Filter>> {
        self.lock().seccomp_filters.clone()
    }
    pub fn add_seccomp_filter(&self, filter: Arc<crate::seccomp::Filter>) {
        self.lock().seccomp_filters.push(filter);
    }
    pub fn signals(&self) -> Arc<SignalState> {
        self.lock().signals.clone()
    }
    pub fn thread_signals(&self) -> Arc<SpinLock<ThreadSignals>> {
        self.lock().thread_signals.clone()
    }
    pub fn exit_signal(&self) -> u32 {
        self.lock().exit_signal
    }
    pub fn files_shared(&self) -> bool {
        self.lock().files_shared
    }
    pub fn is_user(&self) -> bool {
        self.lock().user
    }

    /// Copy the saved extended-register image for a signal frame.
    pub(crate) fn fpu_image(&self, out: &mut [u8]) -> usize {
        let task = self.lock();
        // SAFETY: the task is the current task (or an off-CPU retiring task);
        // its FPU image is only mutated by the owning CPU.
        unsafe {
            match (&*task.fpu_state.get()).as_ref() {
                Some(state) => {
                    let bytes = state.bytes();
                    let count = bytes.len().min(out.len());
                    out[..count].copy_from_slice(&bytes[..count]);
                    count
                }
                None => 0,
            }
        }
    }

    pub(crate) fn fpu_size(&self) -> usize {
        let task = self.lock();
        // SAFETY: see `fpu_image`.
        unsafe {
            (&*task.fpu_state.get())
                .as_ref()
                .map(|state| state.bytes().len())
                .unwrap_or(0)
        }
    }

    /// Import and activate an image from `rt_sigreturn`.  IRQs are masked and
    /// this CPU exclusively owns the task.
    pub(crate) fn restore_fpu_image(&self, bytes: &[u8]) -> bool {
        let flags = arch::irq_save();
        let task = self.lock();
        // SAFETY: the caller runs with IRQs masked on the owning CPU.
        let result = unsafe {
            match (&mut *task.fpu_state.get()).as_mut() {
                Some(state) => state.import(bytes).is_ok() && fpu::restore_current(state).is_ok(),
                None => false,
            }
        };
        drop(task);
        arch::irq_restore(flags);
        result
    }
    pub fn stats(&self) -> TaskStats {
        let task = self.lock();
        TaskStats {
            tid: task.tid,
            tgid: task.tgid,
            ltid: task.ltid,
            ppid: task.ppid,
            pgid: task.pgid,
            sid: task.sid,
            user: task.user,
            exit_signal: task.exit_signal,
            nice: task.nice,
            start_ns: task.start_ns,
            state: task.state,
            cpu: task.on_cpu,
            affinity: task.affinity,
            runtime_ns: task.runtime_ns,
            user_ns: task.user_ns,
            system_ns: task.system_ns,
            switches: task.switches,
            voluntary_switches: task.voluntary_switches,
            involuntary_switches: task.involuntary_switches,
            migrations: task.migrations,
            exit_code: task.exit_code,
            creds: task.creds,
            comm: task.comm,
            pdeathsig: task.pdeathsig,
            child_subreaper: task.child_subreaper,
        }
    }
}
const STACK_SIZE: usize = 64 * 1024;
const STACK_CANARY: u64 = 0x5350_4143_4553_544b;
static NEXT_TID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Blocked,
    Zombie,
    Dead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuMask([u64; 4]);
impl CpuMask {
    pub const fn all() -> Self {
        Self([u64::MAX; 4])
    }
    pub fn one(cpu: usize) -> Self {
        assert!(cpu < crate::smp::MAX_CPUS);
        let mut mask = Self([0; 4]);
        mask.0[cpu / 64] = 1 << (cpu % 64);
        mask
    }
    pub(crate) fn insert(&mut self, cpu: usize) {
        self.0[cpu / 64] |= 1 << (cpu % 64);
    }
    pub const fn empty() -> Self {
        Self([0; 4])
    }
    pub const fn from_words(words: [u64; 4]) -> Self {
        Self(words)
    }
    pub const fn words(self) -> [u64; 4] {
        self.0
    }
    pub fn is_empty(self) -> bool {
        self.0.iter().all(|word| *word == 0)
    }
    pub fn with_cpu(mut self, cpu: usize) -> Self {
        assert!(cpu < crate::smp::MAX_CPUS);
        self.insert(cpu);
        self
    }
    pub(crate) fn cpus(self) -> impl Iterator<Item = usize> {
        self.0
            .into_iter()
            .enumerate()
            .flat_map(|(group, mut bits)| {
                core::iter::from_fn(move || {
                    if bits == 0 {
                        return None;
                    }
                    let bit = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    Some(group * 64 + bit)
                })
            })
    }
    pub fn contains(self, cpu: usize) -> bool {
        cpu < 256 && self.0[cpu / 64] & (1 << (cpu % 64)) != 0
    }
}

/// The lock-wrapped task a `TaskRef` points to.
pub(crate) type TaskLock = SpinLock<Task>;

pub(crate) struct Task {
    pub tid: u64,
    pub tgid: u64,
    pub ltid: u64,
    pub ppid: u64,
    pub pgid: u64,
    pub sid: u64,
    pub user: bool,
    pub exit_signal: u32,
    pub clear_child_tid: u64,
    pub robust_list: u64,
    pub stop_signal: u32,
    pub stop_reported: bool,
    pub continue_pending: bool,
    pub files_shared: bool,
    pub nice: i32,
    pub start_ns: u64,
    pub(crate) state: TaskState,
    pub(crate) on_cpu: usize,
    pub(crate) affinity: CpuMask,
    pub runtime_ns: u64,
    pub user_ns: u64,
    pub system_ns: u64,
    pub switches: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
    pub migrations: u64,
    pub exit_code: i32,
    pub creds: Credentials,
    pub groups: Vec<u32>,
    /// Linux `task_struct.comm`, including the trailing NUL slot.
    pub comm: [u8; 16],
    pub pdeathsig: u32,
    pub dumpable: bool,
    pub no_new_privs: bool,
    pub child_subreaper: bool,
    /// Key of this task's entity in the scheduler's park tree while parked
    /// (`i128::MAX` for an untimed park). Guarded by the park-tree lock.
    pub park_key: Option<i128>,
    /// A wakeup that arrived before the task parked; consumed by `park`.
    pub wake_token: bool,
    /// Linux `vfork_done`: completed when this vfork child execs or exits.
    pub vfork_done: Option<Arc<super::Completion>>,
    pub(crate) context: UnsafeCell<arch::Context>,
    pub(crate) fpu_state: UnsafeCell<Option<fpu::State>>,
    pub(crate) entry: Option<Entry>,
    pub(crate) stack: Option<KernelStack>,
    pub(crate) address_space: Option<Arc<Mutex<crate::user::UserAddressSpace>>>,
    pub(crate) fd_table: Arc<SpinLock<crate::vfs::FileDescriptorTable>>,
    pub(crate) fs: Arc<SpinLock<FsContext>>,
    pub(crate) mount_ns: Arc<SpinLock<crate::vfs::MountNamespace>>,
    pub(crate) seccomp_filters: Vec<Arc<crate::seccomp::Filter>>,
    pub(crate) signals: Arc<SignalState>,
    pub(crate) thread_signals: Arc<SpinLock<ThreadSignals>>,
}

pub(crate) struct KernelStack {
    base: *mut u8,
}
// SAFETY: stacks move only as inactive owned allocations; switch handoff keeps
// the source runqueue locked until the outgoing stack is no longer executing.
unsafe impl Send for KernelStack {}
impl KernelStack {
    fn new() -> Result<Self, &'static str> {
        let layout = Layout::from_size_align(STACK_SIZE, 16).unwrap();
        // SAFETY: valid layout, allocation failure is returned to the caller.
        let base = unsafe { alloc(layout) };
        if base.is_null() {
            return Err("kernel stack allocation failed");
        }
        // SAFETY: the bottom word is reserved exclusively for overflow checking.
        unsafe {
            (base as *mut u64).write(STACK_CANARY);
        }
        Ok(Self { base })
    }
    pub(crate) fn top(&self) -> usize {
        self.base as usize + STACK_SIZE
    }
    pub fn check(&self) {
        // SAFETY: allocation remains owned until the task is retired off-stack.
        assert_eq!(
            unsafe { (self.base as *const u64).read() },
            STACK_CANARY,
            "kernel stack overflow"
        );
    }
}
impl Drop for KernelStack {
    fn drop(&mut self) {
        self.check();
        // SAFETY: retirement runs on another stack; this is the original layout.
        unsafe {
            dealloc(self.base, Layout::from_size_align(STACK_SIZE, 16).unwrap());
        }
    }
}
impl Task {
    pub fn cpu(&self) -> usize {
        self.on_cpu
    }
    pub fn kernel_stack_top(&self) -> Option<usize> {
        self.stack.as_ref().map(|s| s.top())
    }
    pub fn set_address_space(&mut self, space: Arc<Mutex<crate::user::UserAddressSpace>>) {
        self.address_space = Some(space);
    }
    pub fn address_space(&self) -> Option<Arc<Mutex<crate::user::UserAddressSpace>>> {
        self.address_space.clone()
    }
    pub(crate) fn become_zombie(&mut self, code: i32) {
        self.state = TaskState::Zombie;
        self.exit_code = code;
    }
    pub(crate) fn create(
        entry: Option<Entry>,
        cpu: usize,
        affinity: CpuMask,
    ) -> Result<TaskRef, &'static str> {
        Self::create_with_fds(entry, cpu, affinity, None)
    }
    pub(crate) fn create_with_fds(
        entry: Option<Entry>,
        cpu: usize,
        affinity: CpuMask,
        fd_table: Option<Arc<SpinLock<crate::vfs::FileDescriptorTable>>>,
    ) -> Result<TaskRef, &'static str> {
        let idle = entry.is_none();
        let stack = if idle {
            None
        } else {
            Some(KernelStack::new()?)
        };
        let fpu_state = match fpu::State::new() {
            Ok(state) => Some(state),
            Err(fpu::Error::Unsupported) => None,
            Err(_) => return Err("task FPU allocation failed"),
        };
        let mut context = arch::Context::new();
        if let Some(stack) = &stack {
            // SAFETY: the exclusively owned stack is aligned and the no-argument
            // trampoline obtains its closure from this CPU's current task.
            unsafe {
                arch::context_init(&mut context, stack.top(), task_entry as *const () as usize);
            }
        }
        let tid = NEXT_TID.fetch_add(1, Ordering::Relaxed);
        let start_ns = crate::time::uptime_micros().saturating_mul(1000);
        let result = TaskRef(Arc::new(SpinLock::new(Self {
            tid,
            tgid: tid,
            ltid: tid,
            ppid: 0,
            pgid: 0,
            sid: 0,
            user: false,
            exit_signal: 0,
            clear_child_tid: 0,
            robust_list: 0,
            stop_signal: 0,
            stop_reported: false,
            continue_pending: false,
            files_shared: false,
            nice: 0,
            start_ns,
            state: TaskState::Ready,
            on_cpu: cpu,
            affinity,
            runtime_ns: 0,
            user_ns: 0,
            system_ns: 0,
            switches: 0,
            voluntary_switches: 0,
            involuntary_switches: 0,
            migrations: 0,
            exit_code: 0,
            creds: Credentials::default(),
            groups: Vec::new(),
            comm: [0; 16],
            pdeathsig: 0,
            dumpable: true,
            no_new_privs: false,
            child_subreaper: false,
            park_key: None,
            wake_token: false,
            vfork_done: None,
            context: UnsafeCell::new(context),
            fpu_state: UnsafeCell::new(fpu_state),
            entry,
            stack,
            address_space: None,
            fd_table: fd_table.unwrap_or_else(|| {
                Arc::new(SpinLock::new(
                    crate::vfs::FileDescriptorTable::new_with_stdio(),
                ))
            }),
            fs: crate::vfs::kernel_fs(),
            mount_ns: crate::vfs::init_mount_namespace(),
            seccomp_filters: Vec::new(),
            signals: Arc::new(SignalState::new()),
            thread_signals: Arc::new(SpinLock::new(ThreadSignals::new())),
        })));
        let tid = result.stats().tid;
        let mut tasks = TASKS.lock();
        tasks.retain(|(_, w)| w.strong_count() != 0);
        tasks.push((tid, Arc::downgrade(&result.0)));
        Ok(result)
    }

    /// Create a Linux user task with identity, shared resources and address
    /// space installed before the task can run on another CPU.
    pub(crate) fn create_user(
        entry: Entry,
        cpu: usize,
        affinity: CpuMask,
        spec: UserTaskSpec,
    ) -> Result<TaskRef, &'static str> {
        let stack = Some(KernelStack::new()?);
        let fpu_state = match fpu::State::new() {
            Ok(state) => Some(state),
            Err(fpu::Error::Unsupported) => None,
            Err(_) => return Err("task FPU allocation failed"),
        };
        let mut context = arch::Context::new();
        if let Some(stack) = &stack {
            // SAFETY: exclusive stack, entry trampoline obtains the closure
            // from this CPU's current task.
            unsafe {
                arch::context_init(&mut context, stack.top(), task_entry as *const () as usize);
            }
        }
        let tid = NEXT_TID.fetch_add(1, Ordering::Relaxed);
        let start_ns = crate::time::uptime_micros().saturating_mul(1000);
        let result = TaskRef(Arc::new(SpinLock::new(Self {
            tid,
            tgid: spec.tgid,
            ltid: spec.ltid,
            ppid: spec.ppid,
            pgid: spec.pgid,
            sid: spec.sid,
            user: true,
            exit_signal: spec.exit_signal,
            clear_child_tid: spec.clear_child_tid,
            robust_list: 0,
            stop_signal: 0,
            stop_reported: false,
            continue_pending: false,
            files_shared: spec.files_shared,
            nice: 0,
            start_ns,
            state: TaskState::Ready,
            on_cpu: cpu,
            affinity,
            runtime_ns: 0,
            user_ns: 0,
            system_ns: 0,
            switches: 0,
            voluntary_switches: 0,
            involuntary_switches: 0,
            migrations: 0,
            exit_code: 0,
            creds: spec.creds,
            groups: spec.groups,
            comm: spec.comm,
            pdeathsig: 0,
            dumpable: true,
            no_new_privs: spec.no_new_privs,
            child_subreaper: false,
            park_key: None,
            wake_token: false,
            vfork_done: spec.vfork_done,
            context: UnsafeCell::new(context),
            fpu_state: UnsafeCell::new(fpu_state),
            entry: Some(entry),
            stack,
            address_space: Some(spec.address_space),
            fd_table: spec.fd_table,
            fs: spec.fs,
            mount_ns: spec.mount_ns,
            seccomp_filters: spec.seccomp_filters,
            signals: spec.signals,
            thread_signals: spec.thread_signals,
        })));
        let tid = result.stats().tid;
        let mut tasks = TASKS.lock();
        tasks.retain(|(_, w)| w.strong_count() != 0);
        tasks.push((tid, Arc::downgrade(&result.0)));
        Ok(result)
    }
}

pub(crate) struct UserTaskSpec {
    pub tgid: u64,
    pub ltid: u64,
    pub ppid: u64,
    pub pgid: u64,
    pub sid: u64,
    pub exit_signal: u32,
    pub clear_child_tid: u64,
    pub creds: Credentials,
    pub groups: Vec<u32>,
    pub comm: [u8; 16],
    pub no_new_privs: bool,
    pub fd_table: Arc<SpinLock<crate::vfs::FileDescriptorTable>>,
    pub files_shared: bool,
    pub fs: Arc<SpinLock<FsContext>>,
    pub mount_ns: Arc<SpinLock<crate::vfs::MountNamespace>>,
    pub seccomp_filters: Vec<Arc<crate::seccomp::Filter>>,
    pub signals: Arc<SignalState>,
    pub thread_signals: Arc<SpinLock<ThreadSignals>>,
    pub address_space: Arc<Mutex<crate::user::UserAddressSpace>>,
    /// Set for CLONE_VFORK children before they can run.
    pub vfork_done: Option<Arc<super::Completion>>,
}
/// A task's first code: a monomorphized runner and its boxed closure.
///
/// Calling a `Box<dyn FnOnce()>` frees the box only after the closure
/// returns, and user-mode entry closures never return, so every fork leaked
/// its closure allocation. The runner instead moves the closure onto the
/// stack, which frees the box, before calling it.
pub(crate) struct Entry {
    run: unsafe fn(*mut ()),
    closure: *mut (),
}

// SAFETY: built only from `Send` closures and run exactly once by the task.
unsafe impl Send for Entry {}

impl Entry {
    pub(crate) fn new<F: FnOnce() + Send + 'static>(closure: F) -> Self {
        unsafe fn run<F: FnOnce()>(closure: *mut ()) {
            // SAFETY: `closure` came from Box::into_raw::<F> in `new` and is
            // consumed once; the box is freed at the end of this statement.
            let closure: F = *unsafe { Box::from_raw(closure.cast::<F>()) };
            closure();
        }
        Self {
            run: run::<F>,
            closure: Box::into_raw(Box::new(closure)).cast(),
        }
    }

    fn call(self) {
        // SAFETY: `run` matches the type `closure` was created from.
        unsafe { (self.run)(self.closure) }
    }
}

extern "C" fn task_entry() -> ! {
    super::finish_switch();
    let entry = super::current()
        .expect("task entry without current task")
        .lock()
        .entry
        .take()
        .expect("task entered twice");
    arch::enable_interrupts();
    entry.call();
    super::exit(0)
}
