/*
 *
 *       src/sched/pelt.rs
 *       Per-Entity Load Tracking (PELT) with 32ms fixed-point geometric decay
 *
 *       2026/11/13 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Fixed-point integer Per-Entity Load Tracking modeled after Linux `kernel/sched/pelt.c`.
//! Half-life is 32 periods of 1024 us (`~32.768 ms`, decay factor `y = 0.5^(1/32)`).
//! All arithmetic uses pure integer operations suitable for `no_std` IRQ paths.

pub const PELT_PERIOD_US: u64 = 1024;
/// Steady-state limit of `sum_{n=0..inf} 1024 * y^n` (`1024 / (1 - 0.5^(1/32))`).
pub const LOAD_AVG_MAX: u64 = 47_788;
pub const LOAD_AVG_MAX_SCALED: u64 = LOAD_AVG_MAX * 1024;

/// Fixed-point multipliers `round(y^n * 2^32)` for `n = 0..32`, where `y = 0.5^(1/32)`.
const Y_POW32: [u32; 32] = [
    4_294_967_295,
    4_202_935_003,
    4_112_874_773,
    4_024_744_348,
    3_938_502_376,
    3_854_108_391,
    3_771_522_796,
    3_690_706_840,
    3_611_622_603,
    3_534_232_978,
    3_458_501_653,
    3_384_393_094,
    3_311_872_529,
    3_240_905_930,
    3_171_459_999,
    3_103_502_151,
    3_037_000_500,
    2_971_923_842,
    2_908_241_642,
    2_845_924_021,
    2_784_941_738,
    2_725_266_179,
    2_666_869_345,
    2_609_723_834,
    2_553_802_834,
    2_499_080_105,
    2_445_529_972,
    2_393_127_307,
    2_341_847_524,
    2_291_666_561,
    2_242_560_872,
    2_194_507_417,
];

/// Decay `value` by `n` PELT periods (`value * y^n`), purely in integer arithmetic.
#[inline]
pub fn decay_load(value: u64, periods: u64) -> u64 {
    if periods == 0 || value == 0 {
        return value;
    }
    let halvings = periods / 32;
    if halvings >= 64 {
        return 0;
    }
    let shifted = value >> halvings;
    if shifted == 0 {
        return 0;
    }
    let rem = (periods % 32) as usize;
    ((u128::from(shifted) * u128::from(Y_POW32[rem])) >> 32) as u64
}

/// Exact geometric prefix sum `sum_{i=1..=periods} 1024 * y^i` for closed-form
/// accumulation across multi-period intervals without looping.
#[inline]
fn decay_series_sum(periods: u64) -> u64 {
    if periods == 0 {
        return 0;
    }
    // sum_{i=1..=p} (1024*1024)*y^i = LOAD_AVG_MAX_SCALED * y * (1 - y^p),
    // which equals decay_load(LOAD_AVG_MAX_SCALED, 1) - decay_load(LOAD_AVG_MAX_SCALED, p + 1).
    let head = decay_load(LOAD_AVG_MAX_SCALED, 1);
    let tail = decay_load(LOAD_AVG_MAX_SCALED, periods + 1);
    head.saturating_sub(tail)
}

/// Signed per-entity delta returned by [`LoadAvg::update`].
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct LoadDelta {
    pub load_avg: i64,
    pub runnable_avg: i64,
    pub util_avg: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadAvg {
    pub last_update_us: u64,
    pub period_contrib: u32,
    pub runnable_sum: u64,
    pub util_sum: u64,
    pub load_avg: u64,
    pub runnable_avg: u32,
    pub util_avg: u32,
}

#[inline]
const fn get_pelt_divider(period_contrib: u32) -> u64 {
    LOAD_AVG_MAX - 1024 + period_contrib as u64
}

impl LoadAvg {
    pub const fn new() -> Self {
        Self {
            last_update_us: 0,
            period_contrib: 0,
            runnable_sum: 0,
            util_sum: 0,
            load_avg: 0,
            runnable_avg: 0,
            util_avg: 0,
        }
    }

    /// Initialize a newly created task with realistic post-fork estimates so initial
    /// placement spreads work instead of treating newly spawned tasks as zero-load.
    pub fn seed_new(&mut self, weight: u32, now_us: u64) {
        let div = get_pelt_divider(0);
        self.last_update_us = now_us;
        self.period_contrib = 0;
        self.runnable_avg = 1024;
        self.util_avg = 512;
        self.runnable_sum = 1024 * div;
        self.util_sum = 512 * div;
        self.load_avg = u64::from(weight);
    }

    #[inline]
    fn recompute_averages(&mut self, weight: u32) -> LoadDelta {
        let old_load = self.load_avg as i64;
        let old_runnable = i64::from(self.runnable_avg);
        let old_util = i64::from(self.util_avg);

        let div = get_pelt_divider(self.period_contrib);
        let r_avg = (self.runnable_sum / div).min(1024) as u32;
        let u_avg = (self.util_sum / div).min(1024) as u32;
        self.runnable_avg = r_avg;
        self.util_avg = u_avg;
        self.load_avg = (u64::from(weight) * u64::from(r_avg)) >> 10;

        LoadDelta {
            load_avg: self.load_avg as i64 - old_load,
            runnable_avg: i64::from(self.runnable_avg) - old_runnable,
            util_avg: i64::from(self.util_avg) - old_util,
        }
    }

    /// Advance PELT sums to `now_us`. `runnable` should be true if the entity was
    /// ready or running during the elapsed span; `running` true if executing on CPU.
    pub fn update(&mut self, now_us: u64, weight: u32, runnable: bool, running: bool) -> LoadDelta {
        if self.last_update_us == 0 {
            self.last_update_us = now_us;
            return self.recompute_averages(weight);
        }
        let mut delta = now_us.saturating_sub(self.last_update_us);
        if delta == 0 {
            return self.recompute_averages(weight);
        }
        self.last_update_us = now_us;

        let mut d1 = PELT_PERIOD_US.saturating_sub(u64::from(self.period_contrib));
        if delta < d1 {
            self.period_contrib += delta as u32;
            let cap = get_pelt_divider(self.period_contrib) << 10;
            let contrib = delta << 10;
            if runnable {
                self.runnable_sum = (self.runnable_sum + contrib).min(cap);
            }
            if running {
                self.util_sum = (self.util_sum + contrib).min(cap);
            }
            return self.recompute_averages(weight);
        }

        // Complete the partially accumulated initial period first, then full periods,
        // leaving `d3` trailing microseconds in the new active window.
        delta -= d1;
        let periods = delta / PELT_PERIOD_US;
        let d3 = delta % PELT_PERIOD_US;
        let decayed_periods = periods + 1;

        self.runnable_sum = decay_load(self.runnable_sum, decayed_periods);
        self.util_sum = decay_load(self.util_sum, decayed_periods);

        // `d1` from previous window decays by `decayed_periods`; full `periods`
        // contribute `y^1..=y^periods`; `d3` is in period 0 (un-decayed).
        d1 <<= 10;
        let middle = decay_series_sum(periods);
        let tail = d3 << 10;
        let total_contrib = decay_load(d1, decayed_periods) + middle + tail;

        self.period_contrib = d3 as u32;
        let cap = get_pelt_divider(self.period_contrib) << 10;
        if runnable {
            self.runnable_sum = (self.runnable_sum + total_contrib).min(cap);
        }
        if running {
            self.util_sum = (self.util_sum + total_contrib).min(cap);
        }

        self.recompute_averages(weight)
    }
}
