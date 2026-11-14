/*
 *
 *       src/sched/sched_test.rs
 *       Real SMP, preemption, wait, timer, migration and FPU switch tests
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{CpuMask, WaitQueue, current, sleep_micros, spawn, spawn_on, yield_now};
use crate::{arch, boot, fpu, smp, time};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

macro_rules! sched_log {
    ($($arg:tt)*) => {
        $crate::verbose::log(format_args!($($arg)*))
    };
}

fn until(condition: impl Fn() -> bool) {
    let end = time::uptime_micros() + 10_000_000;
    while !condition() {
        assert!(time::uptime_micros() < end, "scheduler test timed out");
        sleep_micros(1000);
    }
}
pub fn run() {
    super::model_tests::run();
    sched_log!("sched test: augmented RB tree / EEVDF fairness / lag passed");
    cpu_startup();
    preemption();
    weighted_runtime();
    waits();
    sleep();
    fpu_isolation();
    migration();
    migration_with_live_state();
    load_balance_convergence();
    stress();
    sched_log!("scheduler self-test passed");
}
fn cpu_startup() {
    let done = Arc::new(AtomicUsize::new(0));
    for cpu in 0..super::cpu_count() {
        let done = done.clone();
        spawn_on(
            cpu,
            move || {
                assert_eq!(smp::current_cpu(), cpu);
                assert_eq!(arch::page_root(), crate::mm::vmm::root().unwrap());
                assert_eq!(current().unwrap().lock().cpu(), cpu);
                assert!(!arch::fpu::is_enabled());
                let start = super::cpu_ticks(cpu);
                sleep_micros(4000);
                assert_eq!(smp::current_cpu(), cpu, "pinned task migrated");
                assert!(
                    super::cpu_ticks(cpu) > start,
                    "CPU has no real scheduling timer"
                );
                done.fetch_add(1, Ordering::Release);
            },
            0,
        )
        .unwrap();
    }
    until(|| done.load(Ordering::Acquire) == super::cpu_count());
    assert_eq!(smp::online_count(), boot::cpu_count());
    sched_log!(
        "sched test: {} CPU startup / affinity / hardware ticks passed",
        super::cpu_count()
    );
}
fn preemption() {
    let peer = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let flag = peer.clone();
    let finished = done.clone();
    let cpu = smp::current_cpu();
    spawn_on(
        cpu,
        move || {
            let deadline = time::uptime_micros() + 2_000_000;
            // No yield, sleep or scheduler call: only a hardware timer can let the
            // second task on this same CPU release the first task.
            while !flag.load(Ordering::Acquire) {
                assert!(time::uptime_micros() < deadline, "timer preemption failed");
                core::hint::spin_loop();
            }
            assert!(current().unwrap().lock().involuntary_switches != 0);
            finished.store(true, Ordering::Release);
        },
        0,
    )
    .unwrap();
    spawn_on(
        cpu,
        move || {
            peer.store(true, Ordering::Release);
        },
        0,
    )
    .unwrap();
    until(|| done.load(Ordering::Acquire));
    sched_log!("sched test: hardware timer preemption passed");
}
fn weighted_runtime() {
    let started = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let service = Arc::new((0..3).map(|_| AtomicU64::new(0)).collect::<Vec<_>>());
    let cpu = smp::current_cpu();
    let priorities = [-5, 0, 5];
    for (worker, nice) in priorities.into_iter().enumerate() {
        let (started, stop, done, service) =
            (started.clone(), stop.clone(), done.clone(), service.clone());
        spawn_on(
            cpu,
            move || {
                started.fetch_add(1, Ordering::Release);
                while started.load(Ordering::Acquire) != 3 {
                    yield_now();
                }
                let before = current().unwrap().lock().runtime_ns;
                while !stop.load(Ordering::Acquire) {
                    core::hint::spin_loop();
                }
                yield_now();
                service[worker].store(
                    current().unwrap().lock().runtime_ns - before,
                    Ordering::Release,
                );
                done.fetch_add(1, Ordering::Release);
            },
            nice,
        )
        .unwrap();
    }
    until(|| started.load(Ordering::Acquire) == 3);
    sleep_micros(500_000);
    stop.store(true, Ordering::Release);
    until(|| done.load(Ordering::Acquire) == 3);
    let runtimes: Vec<_> = service.iter().map(|n| n.load(Ordering::Acquire)).collect();
    let total: u64 = runtimes.iter().sum();
    let weights: Vec<_> = priorities.into_iter().map(super::nice_to_weight).collect();
    let weight_sum: u64 = weights.iter().map(|&w| u64::from(w)).sum();
    for (&runtime, &weight) in runtimes.iter().zip(&weights) {
        let expected = total * u64::from(weight) / weight_sum;
        assert!(
            runtime.abs_diff(expected) <= expected / 4 + 8_000_000,
            "hardware weighted fairness: {runtimes:?}"
        );
    }
    sched_log!(
        "sched test: real weighted CPU service passed {:?}",
        runtimes
    );
}

fn waits() {
    let queue = Arc::new(WaitQueue::new());
    let permits = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    for rank in 0..4 {
        let (waiter_queue, permits, done) = (queue.clone(), permits.clone(), done.clone());
        spawn_on(
            smp::current_cpu(),
            move || {
                waiter_queue.wait_until(|| permits.load(Ordering::Acquire) > rank);
                assert_eq!(
                    done.fetch_add(1, Ordering::AcqRel),
                    rank,
                    "wait queue is not FIFO"
                );
            },
            0,
        )
        .unwrap();
        until(|| queue.len() == rank + 1);
    }
    for rank in 0..4 {
        permits.store(rank + 1, Ordering::Release);
        assert!(queue.wake_one());
        until(|| done.load(Ordering::Acquire) == rank + 1);
    }
    assert!(queue.is_empty());
    // Wake before enrolment, during enrolment and across the stack handoff.
    for _ in 0..32 {
        let queue = Arc::new(WaitQueue::new());
        let ready = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (q, r, d) = (queue.clone(), ready.clone(), done.clone());
        spawn_on(
            smp::current_cpu(),
            move || {
                q.wait_until(|| r.load(Ordering::Acquire));
                d.store(true, Ordering::Release);
            },
            0,
        )
        .unwrap();
        let remote = (smp::current_cpu() + 1) % super::cpu_count();
        spawn_on(
            remote,
            move || {
                ready.store(true, Ordering::Release);
                queue.wake_all();
            },
            0,
        )
        .unwrap();
        until(|| done.load(Ordering::Acquire));
    }
    sched_log!("sched test: FIFO / cross-CPU wake / wake-before-block races passed");
}
fn sleep() {
    let start = time::uptime_micros();
    sleep_micros(10_000);
    assert!(
        time::uptime_micros() - start >= 10_000,
        "sleep woke before deadline"
    );
    sched_log!("sched test: blocking timer sleep passed");
}
fn fpu_isolation() {
    let Some(config) = fpu::config() else {
        panic!("missing FPU config");
    };
    if config.format == fpu::Format::Unavailable {
        return;
    }
    let done = Arc::new(AtomicUsize::new(0));
    for worker in 0..12 {
        let done = done.clone();
        // Competing tasks on each CPU write distinct real extended-register files.
        spawn_on(
            worker % super::cpu_count(),
            move || {
                let expected = fpu::test_pattern(config, worker as u8 + 1);
                let mut observed = fpu::State::new().unwrap();
                for round in 0..24 {
                    let flags = arch::irq_save();
                    // SAFETY: this task owns this CPU and the patterns are validated.
                    unsafe {
                        fpu::restore_current(&expected).unwrap();
                    }
                    arch::irq_restore(flags);
                    if round % 3 == 0 {
                        let end = time::uptime_micros() + 5000;
                        while time::uptime_micros() < end {
                            core::hint::spin_loop();
                        }
                    } else {
                        yield_now();
                    }
                    let flags = arch::irq_save();
                    // SAFETY: scheduler should have restored this task's live image.
                    unsafe {
                        fpu::save_current(&mut observed).unwrap();
                    }
                    fpu::assert_test_registers(config, &expected, &observed);
                    arch::irq_restore(flags);
                }
                done.fetch_add(1, Ordering::Release);
            },
            0,
        )
        .unwrap();
    }
    until(|| done.load(Ordering::Acquire) == 12);
    sched_log!("sched test: FPU isolation across voluntary / involuntary switches passed");
}
fn migration() {
    if super::cpu_count() == 1 {
        return;
    }
    let done = Arc::new(AtomicUsize::new(0));
    let moved = Arc::new(AtomicUsize::new(0));
    let origin = smp::current_cpu();
    for _ in 0..24 {
        let (done, moved) = (done.clone(), moved.clone());
        super::spawn_placed(
            origin,
            move || {
                for _ in 0..128 {
                    yield_now();
                }
                if current().unwrap().lock().migrations > 0 {
                    moved.fetch_add(1, Ordering::Relaxed);
                }
                done.fetch_add(1, Ordering::Release);
            },
            0,
            CpuMask::all(),
        )
        .unwrap();
    }
    until(|| done.load(Ordering::Acquire) == 24);
    assert!(
        moved.load(Ordering::Acquire) > 0,
        "idle CPUs did not migrate queued work"
    );
    sched_log!(
        "sched test: lag-preserving idle pull migrated {} tasks",
        moved.load(Ordering::Acquire)
    );
}
fn migration_with_live_state() {
    if super::cpu_count() == 1 {
        return;
    }
    let queue = Arc::new(WaitQueue::new());
    let release = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let source = Arc::new(AtomicUsize::new(usize::MAX));
    let (q, r, d, s) = (queue.clone(), release.clone(), done.clone(), source.clone());
    super::spawn_placed(
        smp::current_cpu(),
        move || {
            let config = fpu::config().unwrap();
            let images = if config.format == fpu::Format::Unavailable {
                None
            } else {
                Some((fpu::test_pattern(config, 73), fpu::State::new().unwrap()))
            };
            let flags = arch::irq_save();
            if let Some((expected, _)) = &images {
                // SAFETY: this task owns the CPU with IRQs masked.
                unsafe {
                    fpu::restore_current(expected).unwrap();
                }
            }
            let before = smp::current_cpu();
            s.store(before, Ordering::Release);
            q.wait_until(|| r.load(Ordering::Acquire));
            assert_ne!(
                smp::current_cpu(),
                before,
                "wakeup did not move saved context"
            );
            if let Some((expected, mut observed)) = images {
                // SAFETY: the destination scheduler restored this task's image.
                unsafe {
                    fpu::save_current(&mut observed).unwrap();
                }
                fpu::assert_test_registers(config, &expected, &observed);
            }
            arch::irq_restore(flags);
            d.store(true, Ordering::Release);
        },
        0,
        CpuMask::all(),
    )
    .unwrap();
    until(|| queue.len() == 1);
    let blocked_cpu = source.load(Ordering::Acquire);
    let stop = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let blockers_done = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let (stop, started, finished) = (stop.clone(), started.clone(), blockers_done.clone());
        spawn_on(
            blocked_cpu,
            move || {
                started.fetch_add(1, Ordering::Release);
                while !stop.load(Ordering::Acquire) {
                    core::hint::spin_loop();
                }
                finished.fetch_add(1, Ordering::Release);
            },
            0,
        )
        .unwrap();
    }
    until(|| started.load(Ordering::Acquire) == 2);
    release.store(true, Ordering::Release);
    assert!(queue.wake_one());
    until(|| done.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    until(|| blockers_done.load(Ordering::Acquire) == 2);
    sched_log!("sched test: live context / FPU state migration on wake passed");
}

fn stress() {
    let done = Arc::new(AtomicUsize::new(0));
    let count = Arc::new(AtomicU64::new(0));
    let active = Arc::new((0..64).map(|_| AtomicBool::new(false)).collect::<Vec<_>>());
    let baseline = super::completed_count();
    for worker in 0..64 {
        let (done, count, active) = (done.clone(), count.clone(), active.clone());
        spawn(
            move || {
                for _ in 0..128 {
                    assert!(
                        !active[worker].swap(true, Ordering::AcqRel),
                        "task executing on two CPUs"
                    );
                    count.fetch_add(1, Ordering::Relaxed);
                    yield_now();
                    assert!(active[worker].swap(false, Ordering::AcqRel));
                }
                done.fetch_add(1, Ordering::Release);
            },
            worker as i32 % 11 - 5,
        )
        .unwrap();
    }
    until(|| done.load(Ordering::Acquire) == 64);
    until(|| super::completed_count() >= baseline + 64);
    assert_eq!(count.load(Ordering::Acquire), 64 * 128);
    sched_log!("sched test: 64 tasks / 8192 yields / off-stack retirement passed");
}

fn load_balance_convergence() {
    let ncpus = super::cpu_count();
    if ncpus < 2 {
        return;
    }
    let done = Arc::new(AtomicUsize::new(0));
    let seen_cpus = Arc::new(
        (0..ncpus)
            .map(|_| AtomicBool::new(false))
            .collect::<Vec<_>>(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let n_workers = ncpus * 3;
    for _ in 0..n_workers {
        let (done, seen_cpus, stop) = (done.clone(), seen_cpus.clone(), stop.clone());
        super::spawn_placed(
            0,
            move || {
                while !stop.load(Ordering::Acquire) {
                    let c = smp::current_cpu();
                    if c < seen_cpus.len() {
                        seen_cpus[c].store(true, Ordering::Release);
                    }
                    core::hint::spin_loop();
                }
                done.fetch_add(1, Ordering::Release);
            },
            0,
            CpuMask::all(),
        )
        .unwrap();
    }
    sleep_micros(60_000);
    stop.store(true, Ordering::Release);
    until(|| done.load(Ordering::Acquire) == n_workers);
    let active_cpus = seen_cpus
        .iter()
        .filter(|flag| flag.load(Ordering::Acquire))
        .count();
    assert!(
        active_cpus >= 2,
        "load balancer did not spread saturated CPU 0 burst across CPUs ({active_cpus}/{ncpus})"
    );
    sched_log!(
        "sched test: industrial PELT load balancer converged across {active_cpus}/{ncpus} CPUs"
    );
}
