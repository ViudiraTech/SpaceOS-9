/*
 *
 *       src/time.rs
 *       Counter frequency discovery and kernel uptime
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{arch, boot};

static FREQUENCY: AtomicU64 = AtomicU64::new(0);
static ORIGIN: AtomicU64 = AtomicU64::new(0);
static REALTIME_BASE: AtomicU64 = AtomicU64::new(0);
// Linux clocksource-style scaling: units = (ticks * mult) >> shift, with the
// largest shift whose multiplier still fits in 64 bits. The 64x64->128
// multiply replaces a 128-bit software division on every clock read, which
// the scheduler performs several times per context switch.
static MICROS_MULT: AtomicU64 = AtomicU64::new(0);
static MICROS_SHIFT: AtomicU64 = AtomicU64::new(0);
static NANOS_MULT: AtomicU64 = AtomicU64::new(0);
static NANOS_SHIFT: AtomicU64 = AtomicU64::new(0);

/// `(mult, shift)` with `ticks * unit / frequency == (ticks * mult) >> shift`
/// to a relative error below 2^-63.
fn scale_factor(unit: u64, frequency: u64) -> (u64, u32) {
    let mut shift = 0;
    while shift < 127 {
        let next = shift + 1;
        if (unit as u128).leading_zeros() <= next {
            break;
        }
        if ((unit as u128) << next) / frequency as u128 > u64::MAX as u128 {
            break;
        }
        shift = next;
    }
    (
        (((unit as u128) << shift) / frequency as u128) as u64,
        shift,
    )
}

#[inline]
fn scale(ticks: u64, mult: &AtomicU64, shift: &AtomicU64) -> u64 {
    let product = ticks as u128 * mult.load(Ordering::Relaxed) as u128;
    (product >> shift.load(Ordering::Relaxed)).min(u64::MAX as u128) as u64
}

pub fn init() {
    let frequency = boot::counter_frequency()
        .or_else(|| crate::cpuid::info().and_then(|cpu| cpu.counter_hz))
        .filter(|value| *value != 0)
        .unwrap_or(0);
    if frequency != 0 {
        let (mult, shift) = scale_factor(1_000_000, frequency);
        MICROS_MULT.store(mult, Ordering::Relaxed);
        MICROS_SHIFT.store(u64::from(shift), Ordering::Relaxed);
        let (mult, shift) = scale_factor(1_000_000_000, frequency);
        NANOS_MULT.store(mult, Ordering::Relaxed);
        NANOS_SHIFT.store(u64::from(shift), Ordering::Relaxed);
    }
    ORIGIN.store(arch::counter(), Ordering::Release);
    // Publishes the scale factors above together with the frequency.
    FREQUENCY.store(frequency, Ordering::Release);
    if let Some(seconds) = read_rtc_seconds() {
        REALTIME_BASE.store(seconds * 1_000_000, Ordering::Release);
    }
}

pub fn uptime_micros() -> u64 {
    let frequency = FREQUENCY.load(Ordering::Acquire);
    if frequency == 0 {
        return 0;
    }
    let ticks = arch::counter().wrapping_sub(ORIGIN.load(Ordering::Acquire));
    scale(ticks, &MICROS_MULT, &MICROS_SHIFT)
}

/// Monotonic nanoseconds since boot, at the full counter resolution.
pub fn uptime_nanos() -> u64 {
    if FREQUENCY.load(Ordering::Acquire) == 0 {
        return 0;
    }
    let ticks = arch::counter().wrapping_sub(ORIGIN.load(Ordering::Acquire));
    scale(ticks, &NANOS_MULT, &NANOS_SHIFT)
}

/// Wall-clock microseconds since the Unix epoch.
///
/// The base comes from the battery-backed RTC at boot and advances with the
/// monotonic counter. `set_realtime_micros` adjusts it, as `settimeofday` does.
pub fn realtime_micros() -> u64 {
    REALTIME_BASE
        .load(Ordering::Acquire)
        .saturating_add(uptime_micros())
}

pub fn set_realtime_micros(value: u64) {
    REALTIME_BASE.store(value.saturating_sub(uptime_micros()), Ordering::Release);
}

#[cfg(target_arch = "x86_64")]
fn read_rtc_seconds() -> Option<u64> {
    use x86_64::instructions::port::Port;
    let read = |register: u8| -> u8 {
        let mut index = Port::<u8>::new(0x70);
        let mut data = Port::<u8>::new(0x71);
        // SAFETY: CMOS index/data ports have no memory-safety side effects and
        // this kernel is their sole user.
        unsafe {
            index.write(register);
            data.read()
        }
    };
    let snapshot = || {
        for _ in 0..1_000_000 {
            if read(0x0A) & 0x80 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        [
            read(0x00),
            read(0x02),
            read(0x04),
            read(0x07),
            read(0x08),
            read(0x09),
            read(0x32),
        ]
    };
    let mut values = snapshot();
    for _ in 0..8 {
        let again = snapshot();
        if again == values {
            break;
        }
        values = again;
    }
    let status = read(0x0B);
    let decode = |value: u8| {
        if status & 0x04 != 0 {
            u64::from(value)
        } else {
            u64::from(value & 0x0F) + u64::from(value >> 4) * 10
        }
    };
    let second = decode(values[0]);
    let minute = decode(values[1]);
    let pm = values[2] & 0x80 != 0;
    let mut hour = decode(values[2] & 0x7F);
    if status & 0x02 == 0 {
        hour %= 12;
        if pm {
            hour += 12;
        }
    }
    let day = decode(values[3]);
    let month = decode(values[4]);
    let mut year = decode(values[5]);
    let century = decode(values[6]);
    year += if (19..=21).contains(&century) {
        century * 100
    } else {
        2000
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }
    // Days-from-civil algorithm for the proleptic Gregorian calendar.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * shifted + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

#[cfg(not(target_arch = "x86_64"))]
fn read_rtc_seconds() -> Option<u64> {
    None
}

pub fn counter_frequency() -> Option<u64> {
    let value = FREQUENCY.load(Ordering::Acquire);
    (value != 0).then_some(value)
}
