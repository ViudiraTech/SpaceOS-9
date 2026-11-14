/*
 *
 *       src/sched/mod.rs
 *       CPU-local preemptive scheduling and same-CPU switch lock handoff
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Lock order: ascending CPU runqueues -> wait/timer queue -> task metadata.
//! Wakers detach under the wait/timer lock, release it, then lock the owning
//! runqueue. A switch transfers that runqueue lock to the incoming stack. Thus
//! no CPU can migrate, wake or free the outgoing task before its registers and
//! stack pointer are saved. No ordinary spinlock guard survives a switch.

mod eevdf;
mod lb_model;
mod loadbalance;
#[cfg(feature = "boot-self-test")]
mod model_tests;
mod pelt;
mod rbtree;
#[cfg(feature = "boot-self-test")]
pub mod sched_test;
mod task;
mod wait;

use crate::{
    arch, boot, fpu, smp,
    sync::{Guard, SpinLock},
    time,
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
pub use eevdf::{NICE_0_WEIGHT, nice_to_weight};
use rbtree::{Node, RBTree};
use task::Task;
pub use task::alloc_pid;
pub use task::find as find_task;
pub use task::find_tgid;
pub use task::last_tid;
pub use task::{CpuMask, Credentials, TaskRef, TaskState, TaskStats, TaskWeak, comm_from_path};

/// Linux-visible `cpumask_size()`: one bit per possible CPU, rounded to words.
pub const fn cpu_mask_bytes() -> usize {
    smp::MAX_CPUS / 8
}
pub(crate) use task::{UserTaskSpec, has_children, take_zombie, zombies};
pub use wait::WaitQueue;

type Entity = Box<Node<TaskRef>>;
static SCHEDULER: AtomicPtr<Scheduler> = AtomicPtr::new(core::ptr::null_mut());
static TIMERS: SpinLock<RBTree<TaskRef>> = SpinLock::new(RBTree::new());
static USER_NS: [AtomicU64; smp::MAX_CPUS] = [const { AtomicU64::new(0) }; smp::MAX_CPUS];
static SYSTEM_NS: [AtomicU64; smp::MAX_CPUS] = [const { AtomicU64::new(0) }; smp::MAX_CPUS];
static IDLE_NS: [AtomicU64; smp::MAX_CPUS] = [const { AtomicU64::new(0) }; smp::MAX_CPUS];
static CONTEXT_SWITCHES: AtomicU64 = AtomicU64::new(0);
static LOAD_AVG: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static LAST_LOAD_US: AtomicU64 = AtomicU64::new(0);

pub fn sync_accounting() {
    let Some(sched) = try_scheduler() else {
        return;
    };
    let flags = arch::irq_save();
    let cpu = smp::current_cpu();
    if sched.cpus[cpu].active.load(Ordering::Acquire) {
        sched.cpus[cpu].queue.lock().account();
    }
    arch::irq_restore(flags);
}

pub fn cpu_time(cpu: usize) -> [u64; 3] {
    if let Some(sched) = try_scheduler() {
        if cpu < sched.cpus.len() && sched.cpus[cpu].active.load(Ordering::Acquire) {
            let flags = arch::irq_save();
            sched.cpus[cpu].queue.lock().account();
            arch::irq_restore(flags);
        }
    }
    [
        USER_NS[cpu].load(Ordering::Relaxed),
        SYSTEM_NS[cpu].load(Ordering::Relaxed),
        IDLE_NS[cpu].load(Ordering::Relaxed),
    ]
}
pub fn context_switches() -> u64 {
    CONTEXT_SWITCHES.load(Ordering::Relaxed)
}
pub fn loadavg() -> [(u64, u64); 3] {
    let mut out = [(0u64, 0u64); 3];
    for (i, slot) in out.iter_mut().enumerate() {
        let fixed = LOAD_AVG[i].load(Ordering::Relaxed);
        let int = fixed >> 11;
        let frac = ((fixed & 2047) * 100) >> 11;
        *slot = (int, frac);
    }
    out
}
static COMPLETED: AtomicU64 = AtomicU64::new(0);
static INIT_PID: AtomicU64 = AtomicU64::new(0);

/// PID 1 is only considered established once an init image has been started.
pub fn init_pid() -> u64 {
    INIT_PID.load(Ordering::Acquire)
}

pub fn set_init_pid(pid: u64) {
    INIT_PID.store(pid, Ordering::Release);
}

struct Scheduler {
    cpus: Vec<Cpu>,
}
struct Cpu {
    queue: SpinLock<CpuQueue>,
    reschedule: AtomicBool,
    active: AtomicBool,
    ticks: AtomicU64,
    load: AtomicU64,
    load_avg: AtomicU64,
    util_avg: AtomicU64,
    runnable_avg: AtomicU64,
    runnable: AtomicUsize,
    last_balance: AtomicU64,
    active_balance: AtomicIsize,
    domains: SpinLock<loadbalance::Domains>,
}
struct CpuQueue {
    fair: eevdf::RunQueue<TaskRef>,
    current: Option<Entity>,
    idle: Option<Entity>,
    retired: Option<Entity>,
    clock_us: u64,
    cpu: usize,
    cpu_util: pelt::LoadAvg,
    sum_load_avg: i64,
    sum_runnable_avg: i64,
    sum_util_avg: i64,
}
impl CpuQueue {
    fn fair_current(&self) -> Option<&Node<TaskRef>> {
        self.current.as_deref().filter(|node| node.weight != 0)
    }
    fn apply_pelt_delta(&mut self, delta: pelt::LoadDelta) {
        self.sum_load_avg = (self.sum_load_avg + delta.load_avg).max(0);
        self.sum_runnable_avg = (self.sum_runnable_avg + delta.runnable_avg).max(0);
        self.sum_util_avg = (self.sum_util_avg + delta.util_avg).max(0);
    }
    fn subtract_node_pelt(&mut self, node: &Node<TaskRef>) {
        self.sum_load_avg = (self.sum_load_avg - node.pelt.load_avg as i64).max(0);
        self.sum_runnable_avg = (self.sum_runnable_avg - i64::from(node.pelt.runnable_avg)).max(0);
        self.sum_util_avg = (self.sum_util_avg - i64::from(node.pelt.util_avg)).max(0);
    }
    pub(super) fn detach_pelt(&mut self, node: &mut Entity, now_us: u64, runnable: bool) {
        let delta = node.pelt.update(now_us, node.weight, runnable, false);
        self.apply_pelt_delta(delta);
        self.subtract_node_pelt(node);
    }
    fn sync_empty_ready_pelt(&mut self) {
        if self.fair.ready.len() == 0 {
            if let Some(cur) = self.fair_current() {
                let load_avg = cur.pelt.load_avg as i64;
                let runnable_avg = i64::from(cur.pelt.runnable_avg);
                let util_avg = i64::from(cur.pelt.util_avg);
                self.sum_load_avg = load_avg;
                self.sum_runnable_avg = runnable_avg;
                self.sum_util_avg = util_avg;
            } else {
                self.sum_load_avg = 0;
                self.sum_runnable_avg = 0;
                self.sum_util_avg = 0;
            }
        }
    }
    fn account(&mut self) -> bool {
        let now = time::uptime_micros();
        let elapsed = now.saturating_sub(self.clock_us).saturating_mul(1000);
        self.clock_us = now;
        let busy = self.fair_current().is_some();
        let _ = self.cpu_util.update(now, eevdf::NICE_0_WEIGHT, busy, busy);
        let mut expired = false;
        let mut delta = pelt::LoadDelta::default();
        if let Some(node) = self.current.as_mut() {
            if node.weight != 0 {
                expired = elapsed >= node.remaining_ns;
                eevdf::RunQueue::account(node, elapsed);
                delta = node.pelt.update(now, node.weight, true, true);
                let mut task = node.value.lock();
                task.runtime_ns = task.runtime_ns.saturating_add(elapsed);
                if task.address_space.is_some() {
                    task.user_ns = task.user_ns.saturating_add(elapsed);
                    USER_NS[self.cpu].fetch_add(elapsed, Ordering::Relaxed);
                } else {
                    task.system_ns = task.system_ns.saturating_add(elapsed);
                    SYSTEM_NS[self.cpu].fetch_add(elapsed, Ordering::Relaxed);
                }
            }
            if node.weight == 0 {
                IDLE_NS[self.cpu].fetch_add(elapsed, Ordering::Relaxed);
            }
            node.last_run_us = now;
        }
        self.apply_pelt_delta(delta);
        self.sync_empty_ready_pelt();
        let current = self.current.as_deref().filter(|n| n.weight != 0);
        self.fair.update_minimum(current);
        expired
    }
    fn publish(&self) {
        let cpu = &scheduler().cpus[self.cpu];
        let inst_weight = self.fair.weight(self.fair_current());
        cpu.load.store(inst_weight, Ordering::Relaxed);
        cpu.load_avg
            .store(self.sum_load_avg.max(0) as u64, Ordering::Relaxed);
        cpu.runnable_avg
            .store(self.sum_runnable_avg.max(0) as u64, Ordering::Relaxed);
        let util = (self.sum_util_avg.clamp(0, 1024) as u64).max(u64::from(self.cpu_util.util_avg));
        cpu.util_avg.store(util, Ordering::Relaxed);
        cpu.runnable.store(
            self.fair.ready.len() + usize::from(self.fair_current().is_some()),
            Ordering::Release,
        );
    }
    fn admit(&mut self, mut entity: Entity, fresh: bool) {
        self.account();
        if fresh {
            entity.pelt.seed_new(entity.weight, self.clock_us);
        } else {
            let _ = entity
                .pelt
                .update(self.clock_us, entity.weight, false, false);
        }
        self.sum_load_avg += entity.pelt.load_avg as i64;
        self.sum_runnable_avg += i64::from(entity.pelt.runnable_avg);
        self.sum_util_avg += i64::from(entity.pelt.util_avg);
        self.fair.place(&mut entity, self.fair_current(), fresh);
        {
            let mut task = entity.value.lock();
            task.state = TaskState::Ready;
            task.on_cpu = self.cpu;
        }
        self.fair.ready.insert(entity);
        self.publish();
    }
}
fn try_scheduler() -> Option<&'static Scheduler> {
    let pointer = SCHEDULER.load(Ordering::Acquire);
    // SAFETY: init publishes a complete permanent Box exactly once.
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { &*pointer })
    }
}
fn scheduler() -> &'static Scheduler {
    try_scheduler().expect("scheduler not initialized")
}

pub fn init(num_cpus: usize) {
    assert!((1..=smp::MAX_CPUS).contains(&num_cpus));
    assert!(
        SCHEDULER.load(Ordering::Acquire).is_null(),
        "scheduler initialized twice"
    );
    assert!(
        time::counter_frequency().is_some(),
        "scheduler requires a monotonic clock"
    );
    let mut cpus = Vec::with_capacity(num_cpus);
    for cpu in 0..num_cpus {
        let task = Task::create(None, cpu, CpuMask::one(cpu)).expect("idle task allocation failed");
        let tid = task.lock().tid;
        cpus.push(Cpu {
            queue: SpinLock::new(CpuQueue {
                fair: eevdf::RunQueue::new(),
                current: None,
                idle: Some(Node::new(tid, task, 0)),
                retired: None,
                clock_us: 0,
                cpu,
                cpu_util: pelt::LoadAvg::new(),
                sum_load_avg: 0,
                sum_runnable_avg: 0,
                sum_util_avg: 0,
            }),
            reschedule: AtomicBool::new(false),
            active: AtomicBool::new(false),
            ticks: AtomicU64::new(0),
            load: AtomicU64::new(0),
            load_avg: AtomicU64::new(0),
            util_avg: AtomicU64::new(0),
            runnable_avg: AtomicU64::new(0),
            runnable: AtomicUsize::new(0),
            last_balance: AtomicU64::new(0),
            active_balance: AtomicIsize::new(-1),
            domains: SpinLock::new(loadbalance::Domains::system(cpu)),
        });
    }
    let pointer = Box::into_raw(Box::new(Scheduler { cpus }));
    SCHEDULER.store(pointer, Ordering::Release);
    arch::register_scheduler_irqs().expect("scheduler IRQ registration failed");
}

pub(crate) fn activate_cpu(cpu: usize) {
    let mut rq = scheduler().cpus[cpu].queue.lock();
    assert!(rq.current.is_none());
    let idle = rq.idle.take().unwrap();
    idle.value.lock().state = TaskState::Running;
    rq.current = Some(idle);
    rq.clock_us = time::uptime_micros();
    rq.publish();
}
pub(crate) fn build_domains() {
    loadbalance::build_domains(scheduler());
}
pub(crate) fn start_current_cpu() {
    let cpu = smp::current_cpu();
    scheduler().cpus[cpu].active.store(true, Ordering::Release);
    arch::arm_scheduler_tick().expect("scheduler timer arm failed");
    arch::enable_interrupts();
}

/// Spawn an owned, Send kernel closure; its stack and queue node are allocated
/// before acquiring any scheduler lock. Nice is clamped to [-20, 19].
pub fn spawn(entry: impl FnOnce() + Send + 'static, nice: i32) -> Result<u64, &'static str> {
    spawn_with_affinity(entry, nice, CpuMask::all())
}
pub fn spawn_on(
    cpu: usize,
    entry: impl FnOnce() + Send + 'static,
    nice: i32,
) -> Result<u64, &'static str> {
    if cpu >= boot::cpu_count() {
        return Err("invalid CPU");
    }
    spawn_with_affinity(entry, nice, CpuMask::one(cpu))
}
pub fn spawn_with_affinity(
    entry: impl FnOnce() + Send + 'static,
    nice: i32,
    affinity: CpuMask,
) -> Result<u64, &'static str> {
    spawn_with_fds(entry, nice, affinity, None)
}
/// Spawn a task with an already-prepared descriptor table.  Used by fork so the
/// child table is installed before the task can run on another CPU.
pub fn spawn_with_fds(
    entry: impl FnOnce() + Send + 'static,
    nice: i32,
    affinity: CpuMask,
    fd_table: Option<Arc<SpinLock<crate::vfs::FileDescriptorTable>>>,
) -> Result<u64, &'static str> {
    let sched = try_scheduler().ok_or("scheduler not initialized")?;
    let cpu = loadbalance::select_cpu(sched, affinity, smp::current_cpu())
        .ok_or("no online CPU in affinity")?;
    let task = Task::create_with_fds(Some(Box::new(entry)), cpu, affinity, fd_table)?;
    let tid = {
        let mut guard = task.lock();
        guard.nice = nice.clamp(-20, 19);
        guard.tid
    };
    let entity = Node::new(tid, task, nice_to_weight(nice));
    sched.cpus[cpu].queue.lock().admit(entity, true);
    request_reschedule(cpu);
    Ok(tid)
}

/// Spawn a user task with fully-installed identity and shared resources.
pub(crate) fn spawn_user(
    entry: impl FnOnce() + Send + 'static,
    nice: i32,
    affinity: CpuMask,
    spec: UserTaskSpec,
) -> Result<TaskRef, &'static str> {
    let sched = try_scheduler().ok_or("scheduler not initialized")?;
    let cpu = loadbalance::select_cpu(sched, affinity, smp::current_cpu())
        .ok_or("no online CPU in affinity")?;
    let task = Task::create_user(Box::new(entry), cpu, affinity, spec)?;
    {
        let mut guard = task.lock();
        guard.nice = nice.clamp(-20, 19);
    }
    let tid = task.stats().tid;
    let entity = Node::new(tid, task.clone(), nice_to_weight(nice));
    sched.cpus[cpu].queue.lock().admit(entity, true);
    request_reschedule(cpu);
    Ok(task)
}

/// Snapshot every task that is not retired.
pub fn tasks() -> Vec<TaskRef> {
    task::all()
}

/// Every live task; a snapshot, so targets can exit concurrently.
pub fn all_tasks() -> Vec<TaskRef> {
    task::all()
}

pub fn all_tasks_count() -> usize {
    task::all().len()
}

/// Live threads of a thread group (zombies are not signal targets).
pub fn tasks_in_group(tgid: u64) -> Vec<TaskRef> {
    task::all()
        .into_iter()
        .filter(|task| {
            let stats = task.stats();
            stats.user && stats.tgid == tgid && stats.state != TaskState::Zombie
        })
        .collect()
}

pub fn find_by_ltid(ltid: u64) -> Option<TaskRef> {
    task::all().into_iter().find(|task| {
        let stats = task.stats();
        stats.user && stats.ltid == ltid && stats.state != TaskState::Zombie
    })
}

pub fn group_has_live_threads(tgid: u64) -> bool {
    task::all().into_iter().any(|task| {
        let stats = task.stats();
        stats.user && stats.tgid == tgid && stats.state != TaskState::Zombie
    })
}

pub fn current_fs() -> Option<Arc<SpinLock<crate::vfs::FsContext>>> {
    current().map(|task| task.fs())
}

/// Terminate every other thread and then the current one.
pub fn exit_group(status: i32) -> ! {
    if let Some(current) = current() {
        let stats = current.stats();
        if stats.user {
            for member in tasks_in_group(stats.tgid) {
                if member.stats().ltid != stats.ltid {
                    crate::signal::send_to_thread(
                        &member,
                        crate::signal::Signal::kernel(crate::signal::SIGKILL),
                    );
                }
            }
        }
    }
    exit(status)
}

/// Obtain an ownership handle for this task with a consistent CPU-local lookup.
/// TaskRef::stats returns a snapshot without exposing context/FPU ownership.
pub fn current() -> Option<TaskRef> {
    let sched = try_scheduler()?;
    let flags = arch::irq_save();
    let task = sched.cpus[smp::current_cpu()]
        .queue
        .lock()
        .current
        .as_ref()
        .map(|node| node.value.clone());
    arch::irq_restore(flags);
    task
}

pub fn cpu_count() -> usize {
    scheduler().cpus.len()
}
pub fn cpu_ticks(cpu: usize) -> u64 {
    scheduler().cpus[cpu].ticks.load(Ordering::Relaxed)
}
pub fn completed_count() -> u64 {
    COMPLETED.load(Ordering::Acquire)
}
pub fn set_need_resched() {
    let flags = arch::irq_save();
    request_reschedule(smp::current_cpu());
    arch::irq_restore(flags);
}
pub fn need_resched() -> bool {
    let flags = arch::irq_save();
    let needed = try_scheduler().is_some_and(|sched| {
        sched.cpus[smp::current_cpu()]
            .reschedule
            .load(Ordering::Acquire)
    });
    arch::irq_restore(flags);
    needed
}

fn request_reschedule(cpu: usize) {
    let sched = scheduler();
    sched.cpus[cpu].reschedule.store(true, Ordering::Release);
    if cpu != smp::current_cpu() && smp::online(cpu) {
        arch::send_reschedule(cpu).expect("reschedule IPI failed");
    }
}
pub fn yield_now() {
    let flags = arch::irq_save();
    let cpu = smp::current_cpu();
    loadbalance::balance(scheduler(), cpu, false);
    let rq = scheduler().cpus[cpu].queue.lock();
    dispatch(rq, Disposition::Runnable, true);
    arch::irq_restore(flags);
}
pub fn schedule() {
    yield_now();
}

pub fn sleep_micros(micros: u64) {
    if micros == 0 {
        yield_now();
        return;
    }
    let flags = arch::irq_save();
    let cpu = smp::current_cpu();
    let rq = scheduler().cpus[cpu].queue.lock();
    let wake = time::uptime_micros().saturating_add(micros);
    dispatch(rq, Disposition::Sleep(wake), true);
    arch::irq_restore(flags);
}
pub fn exit(exit_code: i32) -> ! {
    // Enable interrupts so Mutex locks, FD drops (e.g. PTY/GPU teardown), and
    // address-space TLB shootdowns execute in normal preemptible task context.
    arch::enable_interrupts();
    if let Some(current_task) = current() {
        let stats = current_task.stats();
        if stats.user {
            // CLONE_CHILD_CLEARTID must reach the exiting thread's address
            // space before it is torn down.
            let (space, address) = {
                let mut task = current_task.lock();
                let clear = (task.address_space.clone(), task.clear_child_tid);
                task.clear_child_tid = 0;
                clear
            };
            if let (Some(space), address) = (space, address)
                && address != 0
            {
                let root = {
                    let mut space = space.lock();
                    let root = space.root_physical();
                    let _ = space.write_bytes(address, &0u32.to_le_bytes());
                    root
                };
                // Publish the cleared word before the exit becomes observable
                // to another CPU waiting on it.
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                crate::ipc::futex::wake_cleared(root, address, 1);
            }
        }
        // Tear down per-process FDs and private address space before taking the
        // runqueue lock so drop handlers never run inside finish_switch/IRQs.
        let (closed_fds, space) = {
            let mut task = current_task.lock();
            let closed_fds = (!task.files_shared).then(|| task.fd_table.lock().take_all());
            let space = task.address_space.take();
            (closed_fds, space)
        };
        drop(closed_fds);
        if space.is_some() {
            crate::user::UserAddressSpace::deactivate();
        }
        drop(space);
    }
    arch::disable_interrupts();
    let rq = scheduler().cpus[smp::current_cpu()].queue.lock();
    dispatch(rq, Disposition::Exit(exit_code), true);
    panic!("dead task resumed")
}

enum Disposition<'a> {
    Runnable,
    Sleep(u64),
    Wait(Guard<'a, wait::Queue>),
    Exit(i32),
}

fn dispatch(mut rq: Guard<'static, CpuQueue>, disposition: Disposition<'_>, voluntary: bool) {
    let cpu = rq.cpu;
    rq.account();
    let mut previous = rq.current.take().expect("CPU not activated");
    let previous_tid = previous.key.1;
    let previous_context;
    let previous_fpu;
    {
        let mut task = previous.value.lock();
        if let Some(stack) = &task.stack {
            stack.check();
        }
        previous_context = task.context.get();
        previous_fpu = task.fpu_state.get();
        if voluntary {
            task.voluntary_switches += 1;
        } else {
            task.involuntary_switches += 1;
        }
    }
    match disposition {
        Disposition::Runnable if previous.weight == 0 => rq.idle = Some(previous),
        Disposition::Runnable => {
            previous.value.lock().state = TaskState::Ready;
            previous.key.0 = previous.deadline;
            rq.fair.ready.insert(previous);
        }
        Disposition::Sleep(wake) => {
            assert_ne!(previous.weight, 0, "idle cannot sleep");
            rq.subtract_node_pelt(&previous);
            rq.fair.save_lag(&mut previous, None);
            previous.value.lock().state = TaskState::Blocked;
            previous.key.0 = i128::from(wake);
            TIMERS.lock().insert(previous);
        }
        Disposition::Wait(mut queue) => {
            assert_ne!(previous.weight, 0, "idle cannot wait");
            rq.subtract_node_pelt(&previous);
            rq.fair.save_lag(&mut previous, None);
            previous.value.lock().state = TaskState::Blocked;
            previous.key.0 = i128::from(queue.sequence);
            queue.sequence = queue
                .sequence
                .checked_add(1)
                .expect("wait sequence overflow");
            queue.tree.insert(previous);
            drop(queue);
        }
        Disposition::Exit(code) => {
            assert_ne!(previous.weight, 0, "idle cannot exit");
            rq.subtract_node_pelt(&previous);
            let mut task = previous.value.lock();
            if task.user {
                task.become_zombie(code);
            } else {
                task.state = TaskState::Dead;
                task.exit_code = code;
            }
            drop(task);
            assert!(rq.retired.is_none());
            rq.retired = Some(previous);
        }
    }
    let mut picked = rq.fair.pick_next();
    if picked.is_none() && loadbalance::newidle_pull(&mut rq, scheduler()) {
        picked = rq.fair.pick_next();
    }
    let mut next = picked
        .or_else(|| rq.idle.take())
        .expect("missing idle context");
    if next.weight != 0 {
        let delta = next.pelt.update(rq.clock_us, next.weight, true, false);
        rq.apply_pelt_delta(delta);
    }
    let next_tid = next.key.1;
    let next_context;
    let next_fpu;
    {
        let mut task = next.value.lock();
        task.state = TaskState::Running;
        task.on_cpu = cpu;
        if previous_tid != next_tid {
            task.switches += 1;
            CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);
        }
        next_context = task.context.get().cast_const();
        next_fpu = task.fpu_state.get().cast_const();
    }
    rq.current = Some(next);
    rq.publish();
    scheduler().cpus[cpu]
        .reschedule
        .store(false, Ordering::Release);
    if previous_tid == next_tid {
        return;
    }

    // SAFETY: queue/wait/retirement ownership pins both tasks. This runqueue
    // excludes migration and wakeup until the outgoing state is saved. These
    // private fields cannot be accessed through public task metadata handles.
    unsafe {
        if let (Some(previous), Some(next)) = ((&mut *previous_fpu).as_mut(), (&*next_fpu).as_ref())
        {
            fpu::switch(previous, next).expect("task FPU switch failed");
        }
    }
    // SAFETY: this transfers a lock, not an IRQ-restoring Guard. The new or
    // resumed task calls finish_switch on this CPU before it can be preempted.
    unsafe {
        rq.handoff();
        arch::switch(previous_context, next_context);
    }
    finish_switch();
}

pub(crate) fn finish_switch() {
    let cpu = smp::current_cpu();
    let queue = &scheduler().cpus[cpu].queue;
    // SAFETY: every arch::switch continuation and first-entry trampoline owns
    // exactly the handoff on the CPU where it is now executing.
    unsafe {
        queue.unlock_handoff();
    }
    loadbalance::finish_active_balance(scheduler(), cpu);
    if let Some(current_task) = current() {
        let task = current_task.lock();
        if let Some(space) = &task.address_space {
            crate::user::UserAddressSpace::activate_shared(space);
            if let Some(stack_top) = task.kernel_stack_top() {
                arch::set_kernel_stack(cpu, stack_top as u64);
            }
        } else {
            crate::user::UserAddressSpace::deactivate();
        }
    }
    let retired = queue.lock().retired.take();
    if let Some(retired) = retired {
        let stats = retired.value.stats();
        let (space, closed_fds) = {
            let mut task = retired.value.lock();
            let space = task.address_space.take();
            let closed_fds = (!task.files_shared).then(|| task.fd_table.lock().take_all());
            (space, closed_fds)
        };
        // Drop outside `retired.value.lock()` so close/drop paths (e.g. PTY/GPU)
        // never re-enter the task list or lock this task while its lock is held.
        drop(closed_fds);
        drop(space);
        if stats.user && stats.state == TaskState::Zombie {
            if stats.ltid == stats.tgid {
                // The group leader is retained until the whole group exits.
                task::push_zombie(retired.value.clone());
            } else {
                // Non-leader threads do not need reaping by parent; once retired, they become Dead.
                retired.value.lock().state = TaskState::Dead;
            }
            if !group_has_live_threads(stats.tgid) {
                let init = init_pid();
                if init != 0 && stats.tgid == init {
                    // Linux panics rather than leaving a namespace without init.
                    crate::printk::emergency(format_args!(
                        "PID 1 (init) exited with status {:#x}",
                        stats.exit_code
                    ));
                    loop {
                        arch::halt();
                    }
                }
                // Orphans are adopted by the nearest child subreaper, or PID 1.
                task::deliver_pdeathsig(stats.tgid);
                let reaper = task::nearest_reaper(stats.tgid);
                task::reparent_children(stats.tgid, reaper);
                let (code, status) = if stats.exit_code & 0x7f == 0 {
                    (crate::signal::CLD_EXITED, (stats.exit_code >> 8) & 0xff)
                } else if stats.exit_code & 0x80 != 0 {
                    (crate::signal::CLD_DUMPED, stats.exit_code & 0x7f)
                } else {
                    (crate::signal::CLD_KILLED, stats.exit_code & 0x7f)
                };
                crate::signal::notify_child_exit(stats.ppid, stats.tgid, code, status);
            }
        }
        drop(retired); // Never free the stack while it is still executing.
        COMPLETED.fetch_add(1, Ordering::Release);
    }
}

pub(crate) fn wake_entity(entity: Entity) {
    let (source, affinity) = {
        let task = entity.value.lock();
        (task.on_cpu, task.affinity)
    };
    // Synchronize with the outgoing stack save before considering a new CPU.
    // The detached node is exclusively owned here, so after this barrier it
    // cannot run or be migrated until admission. No paired locks are needed.
    {
        let _source = scheduler().cpus[source].queue.lock();
        assert_eq!(entity.value.lock().state, TaskState::Blocked);
    }
    let waker = smp::current_cpu();
    let task_util = u64::from(entity.pelt.util_avg);
    let cpu = loadbalance::select_task_rq_fair(
        scheduler(),
        affinity,
        source,
        Some((waker, task_util)),
        false,
    )
    .expect("blocked task has no online CPU");
    if cpu != source {
        entity.value.lock().migrations += 1;
    }
    scheduler().cpus[cpu].queue.lock().admit(entity, false);
    request_reschedule(cpu);
}

pub fn timer_interrupt(_irq: u32) {
    arch::arm_scheduler_tick().expect("scheduler tick rearm failed");
    tick();
}
pub fn reschedule_interrupt(_irq: u32) {
    scheduler().cpus[smp::current_cpu()]
        .reschedule
        .store(true, Ordering::Release);
}
pub fn tick() {
    let sched = scheduler();
    let cpu = smp::current_cpu();
    if !sched.cpus[cpu].active.load(Ordering::Acquire) {
        return;
    }
    sched.cpus[cpu].ticks.fetch_add(1, Ordering::Relaxed);
    {
        let mut rq = sched.cpus[cpu].queue.lock();
        let expired = rq.account();
        let virtual_time = rq.fair.virtual_time(rq.fair_current());
        if let Some(candidate) = rq.fair.ready.eligible(virtual_time) {
            if expired
                || rq.fair_current().is_none_or(|current| {
                    current.vruntime > virtual_time || candidate.deadline < current.deadline
                })
            {
                sched.cpus[cpu].reschedule.store(true, Ordering::Release);
            }
        }
    }
    let now = time::uptime_micros();
    if cpu == 0 {
        let last = LAST_LOAD_US.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= 50_000
            && LAST_LOAD_US
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let runnable: u64 = sched
                .cpus
                .iter()
                .map(|c| c.runnable.load(Ordering::Relaxed) as u64)
                .sum();
            let sample = runnable << 11;
            // Fixed-point EWMA factors for 1m, 5m, 15m sampled periodically
            let factors = [1884u64, 2014, 2037];
            for (i, &exp) in factors.iter().enumerate() {
                let prev = LOAD_AVG[i].load(Ordering::Relaxed);
                let next = if prev == 0 {
                    sample
                } else {
                    (prev * exp + sample * (2048 - exp)) >> 11
                };
                LOAD_AVG[i].store(next, Ordering::Relaxed);
            }
        }
    }
    for _ in 0..64 {
        let entity = {
            let mut timers = TIMERS.lock();
            if timers
                .first()
                .is_none_or(|node| node.key.0 > i128::from(now))
            {
                None
            } else {
                timers.pop_first()
            }
        };
        let Some(entity) = entity else {
            break;
        };
        wake_entity(entity);
    }
    if cpu == 0 {
        crate::ipc::pollwait::timer_tick(now);
    }
}

/// Called by architecture IRQ exits after EOI/complete and trap-frame capture.
pub(crate) fn irq_exit() {
    let Some(sched) = try_scheduler() else {
        return;
    };
    let cpu = smp::current_cpu();
    if !sched.cpus[cpu].active.load(Ordering::Acquire) {
        return;
    }
    loadbalance::balance(sched, cpu, false);
    if sched.cpus[cpu].active_balance.load(Ordering::Relaxed) >= 0 {
        sched.cpus[cpu].reschedule.store(true, Ordering::Release);
    }
    if sched.cpus[cpu].reschedule.load(Ordering::Acquire) {
        let rq = sched.cpus[cpu].queue.lock();
        dispatch(rq, Disposition::Runnable, false);
    }
}
pub(crate) fn idle_loop() -> ! {
    loop {
        arch::disable_interrupts();
        let cpu = smp::current_cpu();
        loadbalance::balance(scheduler(), cpu, true);
        if need_resched() {
            let rq = scheduler().cpus[cpu].queue.lock();
            dispatch(rq, Disposition::Runnable, false);
        }
        arch::idle_wait();
    }
}

#[cfg(feature = "boot-self-test")]
fn spawn_placed(
    cpu: usize,
    entry: impl FnOnce() + Send + 'static,
    nice: i32,
    affinity: CpuMask,
) -> Result<u64, &'static str> {
    let task = Task::create(Some(Box::new(entry)), cpu, affinity)?;
    let tid = {
        let mut guard = task.lock();
        guard.nice = nice.clamp(-20, 19);
        guard.tid
    };
    scheduler().cpus[cpu]
        .queue
        .lock()
        .admit(Node::new(tid, task, nice_to_weight(nice)), true);
    request_reschedule(cpu);
    Ok(tid)
}

pub fn task_snapshots() -> Vec<TaskStats> {
    let mut list = Vec::new();
    let Some(s) = try_scheduler() else {
        return list;
    };
    for cpu in &s.cpus {
        let q = cpu.queue.lock();
        if let Some(ref node) = q.current {
            list.push(node.value.stats());
        }
        if let Some(ref node) = q.idle {
            list.push(node.value.stats());
        }
        q.fair.ready.for_each(|node| {
            list.push(node.value.stats());
        });
    }
    TIMERS.lock().for_each(|node| {
        list.push(node.value.stats());
    });
    list
}

pub fn task_list() -> Vec<TaskStats> {
    task::all().into_iter().map(|task| task.stats()).collect()
}

pub fn current_fd_table() -> Option<alloc::sync::Arc<SpinLock<crate::vfs::FileDescriptorTable>>> {
    current().map(|task| task.lock().fd_table.clone())
}
