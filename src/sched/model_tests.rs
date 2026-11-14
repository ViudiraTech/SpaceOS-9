/*
 *
 *       src/sched/model_tests.rs
 *       Shared host and three-architecture scheduler algorithm verification
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use super::{
    eevdf::{RunQueue, SLICE_NS, nice_to_weight},
    lb_model::{
        GroupStats, GroupType, MigrationType, find_busiest_and_imbalance, task_fits_imbalance,
        wake_affine_check,
    },
    pelt::{LOAD_AVG_MAX_SCALED, LoadAvg, PELT_PERIOD_US, decay_load},
    rbtree::{Node, RBTree},
};
use alloc::{collections::BTreeMap, vec};

pub fn run() {
    randomized_tree();
    weighted_service();
    placement();
    current_accounting();
    pelt_model();
    load_balancer_model();
}

fn random(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}
fn randomized_tree() {
    let mut seed = 0x9e37_79b9_7f4a_7c15;
    let mut tree = RBTree::new();
    let mut model = BTreeMap::new();
    for _ in 0..3000 {
        let tid = random(&mut seed) % 193;
        let key = (i128::from(tid % 17), tid);
        if model.remove(&key).is_some() {
            let node = tree.remove(key).expect("missing tree member");
            assert_eq!(node.value, tid);
            assert!(tree.remove(key).is_none());
        } else {
            let vruntime = i128::from(random(&mut seed) % 1000) - 500;
            let weight = (random(&mut seed) % 88761 + 1) as u32;
            let mut node = Node::new(tid, tid, weight);
            node.key = key;
            node.vruntime = vruntime;
            model.insert(key, (vruntime, weight));
            tree.insert(node);
        }
        tree.validate();
        assert_eq!(tree.len(), model.len());
        let weight: u64 = model.values().map(|(_, w)| u64::from(*w)).sum();
        let sum: i128 = model.values().map(|(v, w)| v * i128::from(*w)).sum();
        assert_eq!((tree.weight(), tree.weighted_sum()), (weight, sum));
        let v = i128::from(random(&mut seed) % 1000) - 500;
        let expected = model
            .iter()
            .find(|(_, (runtime, _))| *runtime <= v)
            .map(|(k, _)| *k);
        assert_eq!(tree.eligible(v).map(|n| n.key), expected);
    }
    while let Some((&key, _)) = model.first_key_value() {
        model.remove(&key);
        assert_eq!(tree.pop_first().unwrap().key, key);
        tree.validate();
    }
    assert_eq!(tree.len(), 0);
    // Monotone insertions and reverse removals exercise both delete directions.
    for tid in 0..512 {
        tree.insert(Node::new(tid, tid, 1024));
        tree.validate();
    }
    for tid in (0..512).rev() {
        assert!(tree.remove((0, tid)).is_some());
        tree.validate();
    }
}
fn weighted_service() {
    assert_eq!(nice_to_weight(-20), 88761);
    assert_eq!(nice_to_weight(0), 1024);
    assert_eq!(nice_to_weight(19), 15);
    assert_eq!(nice_to_weight(i32::MIN), 88761);
    assert_eq!(nice_to_weight(i32::MAX), 15);
    for weights in [
        vec![1024, 1024, 1024],
        vec![1024, 2048, 4096],
        vec![nice_to_weight(-5), nice_to_weight(0), nice_to_weight(5)],
    ] {
        let mut queue = RunQueue::new();
        let mut runtimes = vec![0u64; weights.len()];
        // Simultaneous admission starts every entity at identical virtual time.
        for (tid, &weight) in weights.iter().enumerate() {
            let mut node = Node::new(tid as u64, tid, weight);
            RunQueue::renew(&mut node);
            queue.ready.insert(node);
        }
        for _ in 0..30_000 {
            let virtual_time = queue.virtual_time(None);
            let mut current = queue.pick_next().expect("no eligible task in runnable set");
            assert!(current.vruntime <= virtual_time);
            runtimes[current.value] += 100_000;
            RunQueue::account(&mut current, 100_000);
            queue.ready.insert(current);
        }
        let sum: u64 = weights.iter().map(|&w| u64::from(w)).sum();
        for (&runtime, &weight) in runtimes.iter().zip(&weights) {
            let expected = 3_000_000_000u64 * u64::from(weight) / sum;
            assert!(
                runtime.abs_diff(expected) <= 3 * SLICE_NS,
                "weighted fairness: got {runtime}, expected {expected}"
            );
        }
        queue.ready.validate();
    }
}
fn placement() {
    let mut source = RunQueue::new();
    for (tid, v) in [(1, -1000000), (2, 2000000)] {
        let mut node = Node::new(tid, (), 1024);
        node.vruntime = v;
        RunQueue::renew(&mut node);
        source.ready.insert(node);
    }
    let key = source.ready.first().unwrap().key;
    let mut moved = source.ready.remove(key).unwrap();
    source.save_lag(&mut moved, None);
    let lag = moved.lag;
    let request = moved.deadline - moved.vruntime;
    let mut target = RunQueue::new();
    let mut peer = Node::new(3, (), 2048);
    peer.vruntime = 1_000_000_000;
    RunQueue::renew(&mut peer);
    target.ready.insert(peer);
    target.place(&mut moved, None, false);
    assert_eq!(moved.deadline - moved.vruntime, request);
    let v = moved.vruntime;
    target.ready.insert(moved);
    assert!(
        (target.virtual_time(None) - v - lag).abs() <= 1,
        "migration changed lag"
    );
    target.ready.validate();
    // Integer accounting must retain tiny deltas for high-weight tasks.
    let mut node = Node::new(4, (), 88761);
    RunQueue::renew(&mut node);
    for _ in 0..1000 {
        RunQueue::account(&mut node, 1);
    }
    assert_eq!(node.vruntime, 1024000 / 88761);
}

fn current_accounting() {
    let mut queue = RunQueue::new();
    let mut ready = Node::new(1, (), 1024);
    ready.vruntime = -1024;
    RunQueue::renew(&mut ready);
    queue.ready.insert(ready);
    let mut current = Node::new(2, (), 2048);
    current.vruntime = 1024;
    RunQueue::renew(&mut current);
    assert_eq!(queue.weight(Some(&current)), 3072);
    assert_eq!(queue.virtual_time(Some(&current)), 341);
    queue.update_minimum(Some(&current));
    assert_eq!(queue.minimum, 0); // Monotonic minimum never follows only current.
    let deadline = current.deadline;
    RunQueue::account(&mut current, 1_000_000);
    assert_eq!(
        current.deadline, deadline,
        "preemption renewed an unfinished request"
    );
    let mut budget = 1;
    assert!(
        queue
            .ready
            .find(&mut budget, &mut |n| n.key.1 == 1)
            .is_some()
    );
}

#[test]
fn algorithms() {
    run();
}

fn pelt_model() {
    // 32 periods (32 * 1024 us) equals one half-life.
    let v = 1_000_000u64;
    let half = decay_load(v, 32);
    assert!(
        half.abs_diff(v / 2) <= 4,
        "PELT half-life mismatch: got {half}"
    );
    let quarter = decay_load(v, 64);
    assert!(
        quarter.abs_diff(v / 4) <= 4,
        "PELT quarter-life mismatch: got {quarter}"
    );

    // 100% continuous execution converges to util_avg == 1024, load_avg == weight.
    let mut avg = LoadAvg::new();
    let mut t = 10_000u64;
    avg.update(t, 1024, true, true);
    for _ in 0..400 {
        t += PELT_PERIOD_US;
        avg.update(t, 1024, true, true);
    }
    assert!(
        avg.util_avg >= 1020,
        "util failed to saturate: {}",
        avg.util_avg
    );
    assert!(
        avg.load_avg >= 1020,
        "load failed to saturate: {}",
        avg.load_avg
    );
    assert!(avg.util_sum <= LOAD_AVG_MAX_SCALED);

    // 32ms sleep halves utilization while keeping weight scaling bounded.
    let before = avg.util_avg;
    avg.update(t + 32 * PELT_PERIOD_US, 1024, false, false);
    assert!(
        avg.util_avg.abs_diff(before / 2) <= 8,
        "sleep decay failed: before={before}, after={}",
        avg.util_avg
    );
}

fn load_balancer_model() {
    let mut g_idle = GroupStats {
        group_idx: 0,
        sum_nr_running: 0,
        idle_cpus: 2,
        group_weight: 2,
        group_capacity: 2048,
        sum_load_avg: 0,
        sum_util_avg: 0,
        max_cpu_nr: 0,
        ..GroupStats::default()
    };
    let mut g_over = GroupStats {
        group_idx: 1,
        sum_nr_running: 4,
        idle_cpus: 0,
        group_weight: 2,
        group_capacity: 2048,
        sum_load_avg: 4096,
        sum_util_avg: 2048,
        max_cpu_nr: 2,
        ..GroupStats::default()
    };
    g_idle.classify(false);
    g_over.classify(false);
    assert_eq!(g_idle.group_type, GroupType::HasSpare);
    assert_eq!(g_over.group_type, GroupType::Overloaded);

    let plan =
        find_busiest_and_imbalance(&[g_idle, g_over], 0, true, 125).expect("expected imbalance");
    assert_eq!(plan.busiest_group, 1);
    assert_eq!(plan.mig_type, MigrationType::MigrateLoad);
    assert_eq!(plan.amount, 2048);

    // Near-balanced groups within 17% tolerance must not ping-pong.
    let mut g_a = GroupStats {
        group_idx: 0,
        sum_nr_running: 2,
        idle_cpus: 0,
        group_weight: 2,
        group_capacity: 2048,
        sum_load_avg: 2048,
        sum_util_avg: 1900,
        max_cpu_nr: 1,
        ..GroupStats::default()
    };
    let mut g_b = GroupStats {
        group_idx: 1,
        sum_nr_running: 2,
        idle_cpus: 0,
        group_weight: 2,
        group_capacity: 2048,
        sum_load_avg: 2200,
        sum_util_avg: 1900,
        max_cpu_nr: 1,
        ..GroupStats::default()
    };
    g_a.classify(false);
    g_b.classify(false);
    assert_eq!(find_busiest_and_imbalance(&[g_a, g_b], 0, false, 117), None);

    // Oscillation guard rejects heavy task until failed count relaxes divisor.
    assert!(!task_fits_imbalance(
        MigrationType::MigrateLoad,
        1500,
        4096,
        512,
        false,
        0
    ));
    assert!(task_fits_imbalance(
        MigrationType::MigrateLoad,
        1500,
        4096,
        512,
        false,
        2
    ));

    // Wake-affine prefers idle waker CPU, refuses overloaded waker CPU.
    assert!(wake_affine_check(0, 0, 1, 900, 300, false));
    assert!(!wake_affine_check(2, 950, 1, 400, 300, false));
}
