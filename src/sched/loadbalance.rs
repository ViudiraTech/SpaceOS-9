/*
 *
 *       src/sched/loadbalance.rs
 *       Linux CFS/EEVDF-modeled SMP load balancer (domains, PELT groups,
 *       wake-affine placement, imbalance damping, newidle & active pull)
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Hierarchical SMP scheduler domains (`SMT` -> `MC` -> `SYS`), partitioned
//! `SchedGroup`s, `group_type` / `calculate_imbalance` heuristics,
//! `select_task_rq_fair` with `wake_affine` and idle-core fast paths,
//! exponential balance interval backoff, cache-hot migration relaxation,
//! `newidle` pull and IRQ-safe deferred `active_balance`.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use super::{
    CpuMask, CpuQueue, Scheduler, TaskState,
    lb_model::{
        ACTIVE_BALANCE_FAILED_THRESHOLD, CACHE_HOT_BYPASS_FAILED, DomainLevel, DomainParams,
        GroupStats, MigrationType, SD_SHARE_LLC, SD_WAKE_AFFINE, find_busiest_and_imbalance,
        task_fits_imbalance, wake_affine_check,
    },
    request_reschedule,
};
use crate::{smp, time};

const SCAN_BUDGET: usize = 32;
const MIGRATION_BUDGET: usize = 4;

#[derive(Clone, Copy)]
struct SchedGroup {
    cpus: CpuMask,
    weight: usize,
    capacity: u64,
}

#[derive(Clone)]
struct SchedDomain {
    params: DomainParams,
    span: CpuMask,
    groups: Vec<SchedGroup>,
    local_group: usize,
    last_balance_us: u64,
    balance_interval_us: u64,
    nr_balance_failed: u32,
}

pub(super) struct Domains {
    levels: Vec<SchedDomain>,
    siblings: CpuMask,
    llc: CpuMask,
}

impl Domains {
    pub fn system(cpu: usize) -> Self {
        let span = CpuMask::one(cpu);
        let params = DomainParams::for_level(DomainLevel::Sys, true);
        let group = SchedGroup {
            cpus: span,
            weight: 1,
            capacity: 1024,
        };
        let mut levels = Vec::with_capacity(1);
        levels.push(SchedDomain {
            params,
            span,
            groups: alloc::vec![group],
            local_group: 0,
            last_balance_us: 0,
            balance_interval_us: params.min_interval_us,
            nr_balance_failed: 0,
        });
        Self {
            levels,
            siblings: span,
            llc: span,
        }
    }
}

fn topology_masks(sched: &Scheduler, cpu: usize) -> (CpuMask, CpuMask, CpuMask, bool) {
    let mut system = CpuMask::empty();
    for other in 0..sched.cpus.len() {
        system.insert(other);
    }
    let Some(topo) = smp::cpu_info(cpu).and_then(|info| info.topology) else {
        return (CpuMask::one(cpu), system, system, false);
    };
    let mut smt = CpuMask::empty();
    let mut mc = CpuMask::empty();
    for other in 0..sched.cpus.len() {
        if let Some(peer) = smp::cpu_info(other).and_then(|info| info.topology)
            && peer.package_id == topo.package_id
        {
            mc.insert(other);
            if peer.core_id == topo.core_id {
                smt.insert(other);
            }
        } else if other == cpu {
            smt.insert(other);
            mc.insert(other);
        }
    }
    let has_smt = smt.cpus().count() > 1;
    (smt, mc, system, has_smt)
}

fn build_groups_for_span(span: CpuMask, child_spans: &[CpuMask], has_smt: bool) -> Vec<SchedGroup> {
    let per_cpu_cap = if has_smt { 768 } else { 1024 };
    let mut seen = CpuMask::empty();
    let mut groups = Vec::new();
    for cpu in span.cpus() {
        if seen.contains(cpu) {
            continue;
        }
        let gmask = child_spans[cpu];
        let mut clipped = CpuMask::empty();
        let mut weight = 0usize;
        for member in gmask.cpus() {
            if span.contains(member) {
                clipped.insert(member);
                seen.insert(member);
                weight += 1;
            }
        }
        if weight != 0 {
            groups.push(SchedGroup {
                cpus: clipped,
                weight,
                capacity: (weight as u64) * per_cpu_cap,
            });
        }
    }
    groups
}

/// Build per-CPU hierarchical scheduler domains (`SMT` -> `MC` -> `SYS`) from
/// discovered CPU topology, partitioning each level into `SchedGroup`s.
pub(super) fn build_domains(sched: &Scheduler) {
    let n = sched.cpus.len();
    let mut smt_spans = Vec::with_capacity(n);
    let mut mc_spans = Vec::with_capacity(n);
    let mut any_smt = false;
    let mut single_spans = Vec::with_capacity(n);
    let mut system = CpuMask::empty();
    for cpu in 0..n {
        system.insert(cpu);
        single_spans.push(CpuMask::one(cpu));
        let (smt, mc, _, has_smt) = topology_masks(sched, cpu);
        smt_spans.push(smt);
        mc_spans.push(mc);
        any_smt |= has_smt;
    }

    let flat_cluster = n <= 8 && mc_spans.iter().all(|&m| m == system);
    let base_us = time::uptime_micros();

    for cpu in 0..n {
        let mut levels = Vec::with_capacity(3);
        let smt = smt_spans[cpu];
        let mc = mc_spans[cpu];

        if smt.cpus().count() > 1 {
            let params = DomainParams::for_level(DomainLevel::Smt, flat_cluster);
            let groups = build_groups_for_span(smt, &single_spans, true);
            let local_group = groups
                .iter()
                .position(|g| g.cpus.contains(cpu))
                .unwrap_or(0);
            levels.push(SchedDomain {
                params,
                span: smt,
                groups,
                local_group,
                last_balance_us: base_us.saturating_add(cpu as u64 * 250),
                balance_interval_us: params.min_interval_us,
                nr_balance_failed: 0,
            });
        }

        if mc.cpus().count() > 1 && mc != smt {
            let params = DomainParams::for_level(DomainLevel::Mc, flat_cluster);
            let child = if any_smt { &smt_spans } else { &single_spans };
            let groups = build_groups_for_span(mc, child, any_smt);
            let local_group = groups
                .iter()
                .position(|g| g.cpus.contains(cpu))
                .unwrap_or(0);
            levels.push(SchedDomain {
                params,
                span: mc,
                groups,
                local_group,
                last_balance_us: base_us.saturating_add(cpu as u64 * 500),
                balance_interval_us: params.min_interval_us,
                nr_balance_failed: 0,
            });
        }

        if levels.is_empty() || levels.last().is_some_and(|d| d.span != system) {
            let params = DomainParams::for_level(DomainLevel::Sys, flat_cluster);
            let child = if levels.is_empty() {
                if any_smt { &smt_spans } else { &single_spans }
            } else {
                &mc_spans
            };
            let groups = build_groups_for_span(system, child, any_smt);
            let local_group = groups
                .iter()
                .position(|g| g.cpus.contains(cpu))
                .unwrap_or(0);
            levels.push(SchedDomain {
                params,
                span: system,
                groups,
                local_group,
                last_balance_us: base_us.saturating_add(cpu as u64 * 750),
                balance_interval_us: params.min_interval_us,
                nr_balance_failed: 0,
            });
        }

        let llc = levels
            .iter()
            .rev()
            .find(|d| d.params.flags & SD_SHARE_LLC != 0)
            .map_or(smt, |d| d.span);

        *sched.cpus[cpu].domains.lock() = Domains {
            levels,
            siblings: smt,
            llc,
        };
        sched.cpus[cpu]
            .last_balance
            .store(base_us.saturating_add(cpu as u64 * 500), Ordering::Relaxed);
    }
}

#[inline]
fn locality(a: usize, b: usize) -> u8 {
    if a == b {
        return 0;
    }
    match (
        smp::cpu_info(a).and_then(|i| i.topology),
        smp::cpu_info(b).and_then(|i| i.topology),
    ) {
        (Some(x), Some(y)) if x.package_id == y.package_id => {
            if x.core_id == y.core_id {
                1
            } else {
                2
            }
        }
        _ => 3,
    }
}

#[inline]
fn cpu_load_metric(sched: &Scheduler, cpu: usize) -> u64 {
    let pelt = sched.cpus[cpu].load_avg.load(Ordering::Relaxed);
    let inst = sched.cpus[cpu].load.load(Ordering::Relaxed);
    pelt.max(inst)
}

#[inline]
fn is_idle_core(sched: &Scheduler, cpu: usize, siblings: CpuMask) -> bool {
    if !smp::online(cpu) || sched.cpus[cpu].runnable.load(Ordering::Acquire) != 0 {
        return false;
    }
    siblings
        .cpus()
        .all(|peer| !smp::online(peer) || sched.cpus[peer].runnable.load(Ordering::Acquire) == 0)
}

/// Linux `select_task_rq_fair`: wake-affine check -> idle physical core / idle LLC
/// sibling fast path -> hierarchical `find_idlest_group` -> `find_idlest_cpu`.
pub(super) fn select_task_rq_fair(
    sched: &Scheduler,
    mask: CpuMask,
    previous: usize,
    waker: Option<(usize, u64)>,
    sync_wake: bool,
) -> Option<usize> {
    let previous = if previous < sched.cpus.len() {
        previous
    } else {
        0
    };

    // Snapshot topology masks without holding any runqueue locks.
    let (prev_siblings, prev_llc, wake_affine_span) = {
        let dom = sched.cpus[previous].domains.lock();
        let aff = dom
            .levels
            .iter()
            .rev()
            .find(|d| d.params.flags & SD_WAKE_AFFINE != 0)
            .map_or(dom.llc, |d| d.span);
        (dom.siblings, dom.llc, aff)
    };

    let target_bias = if let Some((waker_cpu, task_util)) = waker
        && waker_cpu != previous
        && waker_cpu < sched.cpus.len()
        && smp::online(waker_cpu)
        && mask.contains(waker_cpu)
        && wake_affine_span.contains(waker_cpu)
    {
        let waker_nr = sched.cpus[waker_cpu].runnable.load(Ordering::Acquire);
        let waker_util = sched.cpus[waker_cpu].util_avg.load(Ordering::Relaxed);
        let prev_nr = sched.cpus[previous].runnable.load(Ordering::Acquire);
        let prev_util = sched.cpus[previous].util_avg.load(Ordering::Relaxed);
        if wake_affine_check(
            waker_nr, waker_util, prev_nr, prev_util, task_util, sync_wake,
        ) {
            waker_cpu
        } else {
            previous
        }
    } else {
        previous
    };

    let target_llc = if target_bias == previous {
        prev_llc
    } else {
        sched.cpus[target_bias].domains.lock().llc
    };
    let target_sibs = if target_bias == previous {
        prev_siblings
    } else {
        sched.cpus[target_bias].domains.lock().siblings
    };

    // 1. Fast path: `target_bias` itself is an idle physical core.
    if mask.contains(target_bias) && is_idle_core(sched, target_bias, target_sibs) {
        return Some(target_bias);
    }

    // 2. Wakeup path (`waker.is_some()`): check `previous` CPU if it's an idle core or idle CPU
    // before hunting across the LLC when affinity kept `target_bias == previous`.
    if waker.is_some()
        && target_bias == previous
        && mask.contains(previous)
        && smp::online(previous)
        && sched.cpus[previous].runnable.load(Ordering::Acquire) == 0
    {
        return Some(previous);
    }

    // 3. `select_idle_core` inside target LLC.
    if let Some(idle_core_cpu) = target_llc
        .cpus()
        .filter(|&cpu| mask.contains(cpu) && smp::online(cpu))
        .find(|&cpu| {
            let sibs = sched.cpus[cpu].domains.lock().siblings;
            is_idle_core(sched, cpu, sibs)
        })
    {
        return Some(idle_core_cpu);
    }

    // 4. `select_idle_cpu` inside target LLC (or anywhere in mask if LLC is busy).
    if let Some(idle_llc) = target_llc
        .cpus()
        .filter(|&cpu| mask.contains(cpu) && smp::online(cpu))
        .filter(|&cpu| sched.cpus[cpu].runnable.load(Ordering::Acquire) == 0)
        .min_by_key(|&cpu| {
            (
                locality(target_bias, cpu),
                sched.cpus[cpu].util_avg.load(Ordering::Relaxed),
                usize::from(cpu != previous),
            )
        })
    {
        return Some(idle_llc);
    }

    // 5. Hierarchical top-down `find_idlest_group` -> `find_idlest_cpu` on forks / saturated LLC.
    if waker.is_none() {
        let levels = sched.cpus[target_bias].domains.lock().levels.clone();
        if let Some(top) = levels.last() {
            let best_group = top
                .groups
                .iter()
                .filter(|g| g.cpus.cpus().any(|c| mask.contains(c) && smp::online(c)))
                .min_by_key(|g| {
                    let mut load_sum = 0u64;
                    let mut util_sum = 0u64;
                    let mut nr_sum = 0usize;
                    let mut idle_cnt = 0usize;
                    for c in g.cpus.cpus().filter(|&c| smp::online(c)) {
                        let nr = sched.cpus[c].runnable.load(Ordering::Acquire);
                        nr_sum += nr;
                        if nr == 0 {
                            idle_cnt += 1;
                        }
                        load_sum += cpu_load_metric(sched, c);
                        util_sum += sched.cpus[c].util_avg.load(Ordering::Relaxed);
                    }
                    let cap = g.capacity.max(1);
                    let avg_load = (load_sum * 1024) / cap;
                    let avg_util = (util_sum * 1024) / cap;
                    let local_bonus = usize::from(!g.cpus.contains(target_bias));
                    (
                        usize::from(idle_cnt == 0),
                        avg_load,
                        avg_util,
                        nr_sum,
                        local_bonus,
                    )
                });

            if let Some(group) = best_group {
                if let Some(picked) = group
                    .cpus
                    .cpus()
                    .filter(|&c| mask.contains(c) && smp::online(c))
                    .min_by_key(|&cpu| {
                        let nr = sched.cpus[cpu].runnable.load(Ordering::Acquire);
                        let sibs = sched.cpus[cpu].domains.lock().siblings;
                        let smt_busy = sibs
                            .cpus()
                            .filter(|&p| p != cpu && smp::online(p))
                            .filter(|&p| sched.cpus[p].runnable.load(Ordering::Acquire) != 0)
                            .count();
                        (
                            nr,
                            smt_busy,
                            cpu_load_metric(sched, cpu),
                            sched.cpus[cpu].util_avg.load(Ordering::Relaxed),
                            locality(target_bias, cpu),
                        )
                    })
                {
                    return Some(picked);
                }
            }
        }
    }

    // 6. General fallback across all allowed online CPUs.
    (0..sched.cpus.len())
        .filter(|&cpu| mask.contains(cpu) && smp::online(cpu))
        .min_by_key(|&cpu| {
            let nr = sched.cpus[cpu].runnable.load(Ordering::Acquire);
            let sibs = sched.cpus[cpu].domains.lock().siblings;
            let smt_busy = sibs
                .cpus()
                .filter(|&peer| peer != cpu && smp::online(peer))
                .filter(|&peer| sched.cpus[peer].runnable.load(Ordering::Acquire) != 0)
                .count();
            (
                nr,
                smt_busy,
                cpu_load_metric(sched, cpu),
                locality(target_bias, cpu),
                usize::from(cpu != previous),
            )
        })
}

/// Backward-compatible wrapper used by spawns without waker affinity hint.
#[allow(dead_code)]
pub(super) fn select_cpu(sched: &Scheduler, mask: CpuMask, previous: usize) -> Option<usize> {
    select_task_rq_fair(sched, mask, previous, None, false)
}

fn collect_group_stats(sched: &Scheduler, groups: &[SchedGroup]) -> Vec<(GroupStats, bool)> {
    let mut out = Vec::with_capacity(groups.len());
    for (idx, group) in groups.iter().enumerate() {
        let mut stats = GroupStats {
            group_idx: idx,
            group_weight: 0,
            group_capacity: 0,
            ..GroupStats::default()
        };
        let per_cpu_cap = group.capacity / (group.weight.max(1) as u64);
        let mut min_nr = usize::MAX;
        for c in group.cpus.cpus().filter(|&c| smp::online(c)) {
            let nr = sched.cpus[c].runnable.load(Ordering::Acquire);
            stats.sum_nr_running += nr;
            if nr == 0 {
                stats.idle_cpus += 1;
            }
            min_nr = min_nr.min(nr);
            stats.max_cpu_nr = stats.max_cpu_nr.max(nr);
            stats.sum_load_avg += cpu_load_metric(sched, c);
            stats.sum_util_avg += sched.cpus[c].util_avg.load(Ordering::Relaxed);
            stats.group_weight += 1;
            stats.group_capacity += per_cpu_cap.max(1);
        }
        if stats.group_weight == 0 {
            stats.group_weight = 1;
            stats.group_capacity = 1024;
        }
        let imbalanced_hint = stats.group_weight > 1 && min_nr == 0 && stats.max_cpu_nr >= 2;
        stats.classify(imbalanced_hint);
        out.push((stats, imbalanced_hint));
    }
    out
}

fn find_busiest_queue(
    sched: &Scheduler,
    group: &SchedGroup,
    puller_cpu: usize,
    mig_type: MigrationType,
) -> Option<usize> {
    group
        .cpus
        .cpus()
        .filter(|&c| c != puller_cpu && smp::online(c))
        .filter(|&c| sched.cpus[c].runnable.load(Ordering::Acquire) >= 1)
        .max_by_key(|&c| {
            let nr = sched.cpus[c].runnable.load(Ordering::Acquire);
            let load = cpu_load_metric(sched, c);
            let util = sched.cpus[c].util_avg.load(Ordering::Relaxed);
            let pullable = usize::from(nr >= 2);
            match mig_type {
                MigrationType::MigrateLoad => (pullable, load, nr, util),
                MigrationType::MigrateUtil => (pullable, util, nr, load),
                MigrationType::MigrateTask => (pullable, nr as u64, nr, load),
            }
        })
}

/// Pull runnable tasks from a domain `donor` CPU queue into local `cpu` queue
/// following Linux `detach_tasks` damping, cache-hot grace, and active balance fallback.
/// Earliest `last_balance_us + interval` over all levels (Linux `rq->next_balance`).
fn next_balance_deadline(dom: &Domains, idle: bool) -> u64 {
    dom.levels
        .iter()
        .map(|sd| {
            let interval = if idle {
                sd.params.min_interval_us
            } else {
                sd.balance_interval_us
            };
            sd.last_balance_us.saturating_add(interval)
        })
        .min()
        .unwrap_or(u64::MAX)
}

pub(super) fn balance(sched: &Scheduler, cpu: usize, idle: bool) {
    let now = time::uptime_micros();
    // Linux `trigger_load_balance()` only raises SCHED_SOFTIRQ once
    // `time_after_eq(jiffies, rq->next_balance)`. `balance()` is reached from
    // every `yield_now()`, IRQ exit and idle-loop wakeup, so this lock-free
    // check must stay the common path: without it each sched_yield() took the
    // domains lock and cloned every group vector, which made yield-heavy user
    // code ~50x slower and timed out the process/signal self-test.
    // `last_balance` holds the next balance deadline for this CPU.
    if !idle && now < sched.cpus[cpu].last_balance.load(Ordering::Relaxed) {
        return;
    }
    let domain_count = sched.cpus[cpu].domains.lock().levels.len();
    let mut moved_total = 0usize;

    for dom_idx in 0..domain_count {
        let (params, groups, local_group, failed_count) = {
            let mut dom = sched.cpus[cpu].domains.lock();
            let Some(sd) = dom.levels.get_mut(dom_idx) else {
                break;
            };
            // Idle CPUs balance at the domain's base interval; busy CPUs use
            // the backed-off interval (Linux: `busy_factor` scaling).
            let interval = if idle {
                sd.params.min_interval_us
            } else {
                sd.balance_interval_us
            };
            if now.saturating_sub(sd.last_balance_us) < interval || sd.groups.len() <= 1 {
                continue;
            }
            // Stamp before the attempt so lock contention or an empty busiest
            // group does not leave the deadline in the past.
            sd.last_balance_us = now;
            (
                sd.params,
                sd.groups.clone(),
                sd.local_group,
                sd.nr_balance_failed,
            )
        };

        let local_nr = sched.cpus[cpu].runnable.load(Ordering::Acquire);
        let local_is_idle = idle || local_nr == 0;
        let group_stats_raw = collect_group_stats(sched, &groups);
        let group_stats: Vec<GroupStats> = group_stats_raw.iter().map(|(s, _)| *s).collect();

        let plan = find_busiest_and_imbalance(
            &group_stats,
            local_group,
            local_is_idle,
            params.imbalance_pct,
        );
        let Some(plan) = plan else {
            let mut dom = sched.cpus[cpu].domains.lock();
            if let Some(sd) = dom.levels.get_mut(dom_idx) {
                sd.last_balance_us = now;
                sd.nr_balance_failed = 0;
                if !idle {
                    sd.balance_interval_us =
                        (sd.balance_interval_us * 2).min(params.max_interval_us);
                }
            }
            continue;
        };

        let Some(donor) =
            find_busiest_queue(sched, &groups[plan.busiest_group], cpu, plan.mig_type)
        else {
            continue;
        };

        let donor_nr = sched.cpus[donor].runnable.load(Ordering::Acquire);
        let (first, second) = (cpu.min(donor), cpu.max(donor));
        let Some(mut low) = sched.cpus[first].queue.try_lock() else {
            continue;
        };
        let Some(mut high) = sched.cpus[second].queue.try_lock() else {
            continue;
        };
        let (source, target) = if donor == first {
            (&mut *low, &mut *high)
        } else {
            (&mut *high, &mut *low)
        };

        source.account();
        target.account();

        let mut remaining_amount = plan.amount;
        let mut pulled_here = 0usize;
        let force_hot = failed_count >= CACHE_HOT_BYPASS_FAILED;

        while moved_total < MIGRATION_BUDGET && remaining_amount > 0 {
            let source_count =
                source.fair.ready.len() + usize::from(source.fair_current().is_some());
            let target_count =
                target.fair.ready.len() + usize::from(target.fair_current().is_some());
            if source_count <= 1 || (target_count > 0 && source_count <= target_count + 1) {
                break;
            }
            let target_is_idle = target_count == 0;
            let mut budget = SCAN_BUDGET;
            let key = source.fair.ready.find(&mut budget, &mut |node| {
                let task = node.value.lock();
                if task.state != TaskState::Ready || !task.affinity.contains(cpu) {
                    return false;
                }
                drop(task);
                if !idle && !force_hot && now.saturating_sub(node.last_run_us) < params.cache_hot_us
                {
                    return false;
                }
                let tload = node.pelt.load_avg.max(u64::from(node.weight));
                let tutil = u64::from(node.pelt.util_avg).max(64);
                task_fits_imbalance(
                    plan.mig_type,
                    remaining_amount,
                    tload,
                    tutil,
                    target_is_idle,
                    failed_count,
                )
            });
            let Some(key) = key else {
                break;
            };

            let mut node = source.fair.ready.remove(key).unwrap();
            let node_load = node.pelt.load_avg.max(u64::from(node.weight));
            let node_util = u64::from(node.pelt.util_avg).max(64);
            source.detach_pelt(&mut node, now, true);
            source.fair.save_lag(&mut node, source.fair_current());
            node.value.lock().migrations += 1;
            target.admit(node, false);

            moved_total += 1;
            pulled_here += 1;
            remaining_amount = match plan.mig_type {
                MigrationType::MigrateTask => remaining_amount.saturating_sub(1),
                MigrationType::MigrateUtil => remaining_amount.saturating_sub(node_util),
                MigrationType::MigrateLoad => remaining_amount.saturating_sub(node_load),
            };
        }

        source.publish();
        target.publish();
        drop(high);
        drop(low);

        let mut trigger_active = false;
        {
            let mut dom = sched.cpus[cpu].domains.lock();
            if let Some(sd) = dom.levels.get_mut(dom_idx) {
                sd.last_balance_us = now;
                if pulled_here > 0 {
                    sd.nr_balance_failed = 0;
                    sd.balance_interval_us = params.min_interval_us;
                } else if donor_nr >= 2 {
                    sd.nr_balance_failed = sd.nr_balance_failed.saturating_add(1);
                    if !idle {
                        sd.balance_interval_us =
                            (sd.balance_interval_us * 2).min(params.max_interval_us);
                    }
                    if sd.nr_balance_failed >= ACTIVE_BALANCE_FAILED_THRESHOLD {
                        trigger_active = true;
                    }
                }
            }
        }

        if pulled_here != 0 {
            request_reschedule(cpu);
        } else if trigger_active
            && sched.cpus[donor]
                .active_balance
                .compare_exchange(-1, cpu as isize, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            request_reschedule(donor);
        }

        if moved_total >= MIGRATION_BUDGET {
            break;
        }
    }
    publish_next_balance(sched, cpu);
}

/// Publish the busy-CPU deadline consumed by the lock-free fast path above.
fn publish_next_balance(sched: &Scheduler, cpu: usize) {
    let next = next_balance_deadline(&sched.cpus[cpu].domains.lock(), false);
    sched.cpus[cpu].last_balance.store(next, Ordering::Relaxed);
}

/// Newly idle quick-pull executed inside `dispatch()` right before switching to idle.
/// Acquires only remote `try_lock()` so it can never deadlock regardless of lock order.
pub(super) fn newidle_pull(rq: &mut CpuQueue, sched: &Scheduler) -> bool {
    let cpu = rq.cpu;
    let llc = match sched.cpus[cpu].domains.try_lock() {
        Some(dom) => dom.llc,
        None => return false,
    };
    let donor = llc
        .cpus()
        .chain((0..sched.cpus.len()).filter(|&c| !llc.contains(c)))
        .filter(|&other| other != cpu && smp::online(other))
        .filter(|&other| sched.cpus[other].runnable.load(Ordering::Acquire) >= 2)
        .max_by_key(|&other| {
            (
                sched.cpus[other].runnable.load(Ordering::Acquire),
                cpu_load_metric(sched, other),
            )
        });
    let Some(donor) = donor else {
        return false;
    };
    let Some(mut source) = sched.cpus[donor].queue.try_lock() else {
        return false;
    };
    let now = time::uptime_micros();
    source.account();
    let mut budget = SCAN_BUDGET;
    let key = source.fair.ready.find(&mut budget, &mut |node| {
        let task = node.value.lock();
        task.state == TaskState::Ready && task.affinity.contains(cpu)
    });
    let Some(key) = key else {
        return false;
    };
    let mut node = source.fair.ready.remove(key).unwrap();
    source.detach_pelt(&mut node, now, true);
    source.fair.save_lag(&mut node, source.fair_current());
    node.value.lock().migrations += 1;
    source.publish();
    drop(source);

    rq.admit(node, false);
    true
}

/// Complete a deferred active load balance once `finish_switch()` has safely switched
/// off the outgoing task's kernel stack onto another task on `donor_cpu`.
pub(super) fn finish_active_balance(sched: &Scheduler, donor_cpu: usize) {
    if sched.cpus[donor_cpu].active_balance.load(Ordering::Relaxed) < 0 {
        return;
    }
    let target = sched.cpus[donor_cpu]
        .active_balance
        .swap(-1, Ordering::AcqRel);
    if target < 0 {
        return;
    }
    let target = target as usize;
    if target == donor_cpu || target >= sched.cpus.len() || !smp::online(target) {
        return;
    }

    let (first, second) = (donor_cpu.min(target), donor_cpu.max(target));
    let Some(mut low) = sched.cpus[first].queue.try_lock() else {
        return;
    };
    let Some(mut high) = sched.cpus[second].queue.try_lock() else {
        return;
    };
    let (source, dst) = if donor_cpu == first {
        (&mut *low, &mut *high)
    } else {
        (&mut *high, &mut *low)
    };

    let source_total = source.fair.ready.len() + usize::from(source.fair_current().is_some());
    let dst_total = dst.fair.ready.len() + usize::from(dst.fair_current().is_some());
    if source_total < 2 || dst_total != 0 {
        return;
    }

    let now = time::uptime_micros();
    source.account();
    dst.account();
    let mut budget = SCAN_BUDGET;
    let key = source.fair.ready.find(&mut budget, &mut |node| {
        let task = node.value.lock();
        task.state == TaskState::Ready && task.affinity.contains(target)
    });
    let Some(key) = key else {
        return;
    };
    let mut node = source.fair.ready.remove(key).unwrap();
    source.detach_pelt(&mut node, now, true);
    source.fair.save_lag(&mut node, source.fair_current());
    node.value.lock().migrations += 1;
    dst.admit(node, false);
    source.publish();
    dst.publish();
    drop(high);
    drop(low);

    request_reschedule(target);
}
