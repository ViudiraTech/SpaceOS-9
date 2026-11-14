/*
 *
 *       src/sched/lb_model.rs
 *       Pure Linux-style sched_group classification and imbalance heuristics
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Architecture-independent `sched_domain` / `sched_group` classification,
//! `calculate_imbalance` and oscillation-damped task-selection predicates,
//! shared verbatim between the SMP kernel and host model tests.

pub const SD_LOAD_BALANCE: u32 = 1 << 0;
pub const SD_BALANCE_NEWIDLE: u32 = 1 << 1;
pub const SD_BALANCE_EXEC: u32 = 1 << 2;
pub const SD_BALANCE_FORK: u32 = 1 << 3;
pub const SD_BALANCE_WAKE: u32 = 1 << 4;
pub const SD_WAKE_AFFINE: u32 = 1 << 5;
pub const SD_SHARE_CPUCAPACITY: u32 = 1 << 6;
pub const SD_SHARE_LLC: u32 = 1 << 7;

pub const CACHE_HOT_BYPASS_FAILED: u32 = 3;
pub const ACTIVE_BALANCE_FAILED_THRESHOLD: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum DomainLevel {
    Smt = 0,
    Mc = 1,
    Sys = 2,
}

#[derive(Clone, Copy, Debug)]
pub struct DomainParams {
    pub level: DomainLevel,
    pub flags: u32,
    pub min_interval_us: u64,
    pub max_interval_us: u64,
    pub imbalance_pct: u64,
    pub cache_hot_us: u64,
}

impl DomainParams {
    pub const fn for_level(level: DomainLevel, shared_flat_cluster: bool) -> Self {
        match level {
            DomainLevel::Smt => Self {
                level,
                flags: SD_LOAD_BALANCE
                    | SD_BALANCE_NEWIDLE
                    | SD_BALANCE_EXEC
                    | SD_BALANCE_FORK
                    | SD_WAKE_AFFINE
                    | SD_SHARE_CPUCAPACITY
                    | SD_SHARE_LLC,
                min_interval_us: 2_000,
                max_interval_us: 8_000,
                imbalance_pct: 110,
                cache_hot_us: 500,
            },
            DomainLevel::Mc => Self {
                level,
                flags: SD_LOAD_BALANCE
                    | SD_BALANCE_NEWIDLE
                    | SD_BALANCE_EXEC
                    | SD_BALANCE_FORK
                    | SD_WAKE_AFFINE
                    | SD_SHARE_LLC,
                min_interval_us: 4_000,
                max_interval_us: 32_000,
                imbalance_pct: 117,
                cache_hot_us: 2_500,
            },
            DomainLevel::Sys => Self {
                level,
                flags: SD_LOAD_BALANCE
                    | SD_BALANCE_NEWIDLE
                    | SD_BALANCE_EXEC
                    | SD_BALANCE_FORK
                    | if shared_flat_cluster {
                        SD_WAKE_AFFINE | SD_SHARE_LLC
                    } else {
                        0
                    },
                min_interval_us: if shared_flat_cluster { 4_000 } else { 8_000 },
                max_interval_us: if shared_flat_cluster { 32_000 } else { 64_000 },
                imbalance_pct: if shared_flat_cluster { 117 } else { 125 },
                cache_hot_us: if shared_flat_cluster { 2_500 } else { 5_000 },
            },
        }
    }
}

/// Ordered in increasing severity, matching Linux `enum group_type`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum GroupType {
    #[default]
    HasSpare = 0,
    FullyBusy = 1,
    Imbalanced = 2,
    Overloaded = 3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MigrationType {
    MigrateLoad = 0,
    MigrateUtil = 1,
    MigrateTask = 2,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct GroupStats {
    pub group_idx: usize,
    pub sum_nr_running: usize,
    pub idle_cpus: usize,
    pub group_weight: usize,
    pub group_capacity: u64,
    pub sum_load_avg: u64,
    pub sum_util_avg: u64,
    pub max_cpu_nr: usize,
    pub group_type: GroupType,
    pub avg_load: u64,
}

impl GroupStats {
    pub fn classify(&mut self, imbalanced_hint: bool) {
        self.avg_load = (self.sum_load_avg.saturating_mul(1024)) / self.group_capacity.max(1);
        self.group_type = if self.sum_nr_running > self.group_weight {
            GroupType::Overloaded
        } else if imbalanced_hint {
            GroupType::Imbalanced
        } else if self.idle_cpus > 0
            || self.sum_nr_running < self.group_weight
            || (self.sum_util_avg * 100 < self.group_capacity * 80)
        {
            GroupType::HasSpare
        } else {
            GroupType::FullyBusy
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImbalancePlan {
    pub busiest_group: usize,
    pub mig_type: MigrationType,
    pub amount: u64,
}

/// Choose the busiest remote group and calculate Linux-style migration imbalance.
pub fn find_busiest_and_imbalance(
    groups: &[GroupStats],
    local_idx: usize,
    local_is_idle: bool,
    imbalance_pct: u64,
) -> Option<ImbalancePlan> {
    let local = *groups.get(local_idx)?;
    let busiest = groups
        .iter()
        .copied()
        .filter(|g| g.group_idx != local_idx && g.sum_nr_running > 0)
        .max_by_key(|g| (g.group_type, g.avg_load, g.sum_nr_running, g.max_cpu_nr))?;

    if local.group_type > busiest.group_type {
        return None;
    }

    match busiest.group_type {
        GroupType::HasSpare => {
            if busiest.max_cpu_nr <= 1 && (!local_is_idle || local.sum_nr_running >= 1) {
                return None;
            }
            if !local_is_idle && busiest.max_cpu_nr < 2 && local.idle_cpus <= busiest.idle_cpus + 1
            {
                return None;
            }
            if local_is_idle && local.idle_cpus > busiest.idle_cpus && busiest.max_cpu_nr >= 2 {
                let tasks = ((local.idle_cpus - busiest.idle_cpus) / 2)
                    .max(busiest.sum_nr_running.saturating_sub(local.sum_nr_running) / 2)
                    .max(1);
                return Some(ImbalancePlan {
                    busiest_group: busiest.group_idx,
                    mig_type: MigrationType::MigrateTask,
                    amount: tasks as u64,
                });
            }
            if busiest.max_cpu_nr >= 2 && local_is_idle {
                return Some(ImbalancePlan {
                    busiest_group: busiest.group_idx,
                    mig_type: MigrationType::MigrateTask,
                    amount: 1,
                });
            }
            if busiest.sum_util_avg * 100 > local.sum_util_avg * imbalance_pct.max(100) {
                let diff = ((busiest.sum_util_avg - local.sum_util_avg) / 2).max(1);
                return Some(ImbalancePlan {
                    busiest_group: busiest.group_idx,
                    mig_type: MigrationType::MigrateUtil,
                    amount: diff,
                });
            }
            None
        }
        GroupType::Imbalanced => Some(ImbalancePlan {
            busiest_group: busiest.group_idx,
            mig_type: MigrationType::MigrateTask,
            amount: 1,
        }),
        GroupType::FullyBusy => {
            if local.group_type == GroupType::HasSpare {
                if local.idle_cpus > 0 && busiest.max_cpu_nr >= 1 {
                    let tasks = local.idle_cpus.min(busiest.sum_nr_running).max(1);
                    return Some(ImbalancePlan {
                        busiest_group: busiest.group_idx,
                        mig_type: MigrationType::MigrateTask,
                        amount: ((tasks + 1) / 2).max(1) as u64,
                    });
                }
                return Some(ImbalancePlan {
                    busiest_group: busiest.group_idx,
                    mig_type: MigrationType::MigrateUtil,
                    amount: (busiest.sum_util_avg.saturating_sub(local.sum_util_avg) / 2).max(1),
                });
            }
            if !local_is_idle
                && busiest.avg_load.saturating_mul(100)
                    <= local.avg_load.saturating_mul(imbalance_pct)
            {
                return None;
            }
            load_imbalance(groups, &local, &busiest, local_is_idle)
        }
        GroupType::Overloaded => {
            if local.group_type == GroupType::Overloaded
                && busiest.avg_load.saturating_mul(100)
                    <= local.avg_load.saturating_mul(imbalance_pct)
            {
                return None;
            }
            load_imbalance(groups, &local, &busiest, local_is_idle)
        }
    }
}

fn load_imbalance(
    groups: &[GroupStats],
    local: &GroupStats,
    busiest: &GroupStats,
    local_is_idle: bool,
) -> Option<ImbalancePlan> {
    let total_load: u64 = groups.iter().map(|g| g.sum_load_avg).sum();
    let total_cap: u64 = groups.iter().map(|g| g.group_capacity).sum();
    let dom_avg_load = (total_load.saturating_mul(1024)) / total_cap.max(1);

    if busiest.avg_load > dom_avg_load && dom_avg_load > local.avg_load {
        let excess =
            (busiest.avg_load - dom_avg_load).saturating_mul(busiest.group_capacity) / 1024;
        let room = (dom_avg_load - local.avg_load).saturating_mul(local.group_capacity) / 1024;
        let imb = excess.min(room);
        if imb > 0 {
            return Some(ImbalancePlan {
                busiest_group: busiest.group_idx,
                mig_type: MigrationType::MigrateLoad,
                amount: imb,
            });
        }
    }

    if local.idle_cpus > 0 && busiest.sum_nr_running > busiest.group_weight {
        let excess_tasks = (busiest.sum_nr_running - busiest.group_weight)
            .min(local.idle_cpus)
            .max(1);
        return Some(ImbalancePlan {
            busiest_group: busiest.group_idx,
            mig_type: MigrationType::MigrateTask,
            amount: excess_tasks as u64,
        });
    }
    if local_is_idle && busiest.sum_nr_running >= 2 {
        return Some(ImbalancePlan {
            busiest_group: busiest.group_idx,
            mig_type: MigrationType::MigrateTask,
            amount: 1,
        });
    }
    None
}

/// Damped oscillation predicate for detaching weighted tasks cleanly even when
/// mixed `nice` weights exist, relaxing gradually after failed balance passes.
#[inline]
pub fn task_fits_imbalance(
    mig_type: MigrationType,
    remaining_imbalance: u64,
    task_load: u64,
    task_util: u64,
    target_is_idle: bool,
    nr_balance_failed: u32,
) -> bool {
    if remaining_imbalance == 0 {
        return false;
    }
    match mig_type {
        MigrationType::MigrateTask => true,
        MigrationType::MigrateUtil => {
            target_is_idle || (task_util >> nr_balance_failed.min(5)) / 2 <= remaining_imbalance
        }
        MigrationType::MigrateLoad => {
            target_is_idle || (task_load >> nr_balance_failed.min(6)) / 2 <= remaining_imbalance
        }
    }
}

/// Evaluate whether waking `task_util` onto `waker_cpu` preserves cache locality
/// without overloading `waker_cpu` relative to `prev_cpu`.
#[inline]
pub fn wake_affine_check(
    waker_nr: usize,
    waker_util: u64,
    prev_nr: usize,
    prev_util: u64,
    task_util: u64,
    sync_wake: bool,
) -> bool {
    if waker_nr == 0 {
        return true;
    }
    let waker_budget = usize::from(sync_wake);
    if waker_nr <= waker_budget && waker_util + task_util <= 880 {
        return true;
    }
    waker_nr + 1 < prev_nr && waker_util + task_util <= prev_util.saturating_add(128)
}
