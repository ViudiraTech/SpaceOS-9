/*
 *
 *       src/random.rs
 *       Entropy pool and ChaCha20 CRNG with Linux getrandom semantics
 *
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Modelled on Linux `drivers/char/random.c` (5.18+):
//!
//! * An input pool hashes everything with BLAKE2s. Inputs are CPU random
//!   instructions (RDSEED/RDRAND, AArch64 RNDR, RISC-V Zkr `seed`), the timing
//!   of every interrupt, and at boot CPU execution-time jitter. Public data
//!   (boot time, command line) is mixed in with zero entropy credit.
//! * Entropy is credited conservatively. Once 256 bits are credited the pool
//!   seeds a ChaCha20 CRNG and the system is "ready"; afterwards the CRNG is
//!   reseeded from the pool at most once a minute.
//! * Output uses fast key erasure: every request first replaces the global key
//!   with fresh keystream, so earlier outputs cannot be recovered from a later
//!   compromise of the state.
//! * `getrandom` (without GRND_INSECURE) and `/dev/random` wait for readiness
//!   on a wait queue; `get_random_bytes` and `/dev/urandom` never block, as on
//!   Linux, and are seeded from whatever has been collected so far.
//!
//! Time is never credited as entropy; only the *variation* of execution and
//! interrupt timing is, at a low rate.

use crate::{arch, sched::WaitQueueHead, smp, sync::SpinLock, vfs::VfsError};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// Bits the pool must hold before the CRNG counts as initialized.
const READY_BITS: u32 = 256;
/// After initialization, reseed from the pool at most this often.
const RESEED_INTERVAL_MICROS: u64 = 60_000_000;
/// Interrupts folded into a per-CPU fast pool before it is credited 1 bit.
const FAST_POOL_INTERRUPTS: u32 = 64;
/// Jitter samples taken at boot before giving up on reaching READY_BITS.
const JITTER_MAX_SAMPLES: u32 = 1 << 16;
/// Non-stuck jitter samples per credited bit.
const JITTER_SAMPLES_PER_BIT: u32 = 64;

// ---- BLAKE2s (RFC 7693) -----------------------------------------------------

const BLAKE2S_IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];
const BLAKE2S_SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

#[derive(Clone)]
struct Blake2s {
    h: [u32; 8],
    t: u64,
    buffer: [u8; 64],
    filled: usize,
}

impl Blake2s {
    const fn new() -> Self {
        let mut h = BLAKE2S_IV;
        // Parameter block: 32-byte digest, no key, fanout 1, depth 1.
        h[0] ^= 0x0101_0020;
        Self {
            h,
            t: 0,
            buffer: [0; 64],
            filled: 0,
        }
    }

    fn compress(&mut self, last: bool) {
        let mut m = [0u32; 16];
        for (word, bytes) in m.iter_mut().zip(self.buffer.chunks_exact(4)) {
            *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        let mut v = [0u32; 16];
        v[..8].copy_from_slice(&self.h);
        v[8..].copy_from_slice(&BLAKE2S_IV);
        v[12] ^= self.t as u32;
        v[13] ^= (self.t >> 32) as u32;
        if last {
            v[14] = !v[14];
        }
        fn g(v: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, x: u32, y: u32) {
            v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
            v[d] = (v[d] ^ v[a]).rotate_right(16);
            v[c] = v[c].wrapping_add(v[d]);
            v[b] = (v[b] ^ v[c]).rotate_right(12);
            v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
            v[d] = (v[d] ^ v[a]).rotate_right(8);
            v[c] = v[c].wrapping_add(v[d]);
            v[b] = (v[b] ^ v[c]).rotate_right(7);
        }
        for s in &BLAKE2S_SIGMA {
            g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
            g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
            g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
            g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
            g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
            g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
            g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
            g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
        }
        for i in 0..8 {
            self.h[i] ^= v[i] ^ v[i + 8];
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            // The final block is compressed by `finalize`, so only compress a
            // full buffer once more input is known to follow.
            if self.filled == 64 {
                self.t = self.t.wrapping_add(64);
                self.compress(false);
                self.filled = 0;
            }
            let take = (64 - self.filled).min(data.len());
            self.buffer[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        self.t = self.t.wrapping_add(self.filled as u64);
        self.buffer[self.filled..].fill(0);
        self.compress(true);
        let mut out = [0u8; 32];
        for (bytes, word) in out.chunks_exact_mut(4).zip(self.h) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

// ---- ChaCha20 (RFC 8439) ----------------------------------------------------

/// One 64-byte block; `tail` is state words 12..16 (counter and nonce).
fn chacha20_block(key: &[u8; 32], tail: [u32; 4]) -> [u8; 64] {
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    for (word, bytes) in state[4..12].iter_mut().zip(key.chunks_exact(4)) {
        *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    state[12..].copy_from_slice(&tail);
    let mut x = state;
    fn qr(x: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
        x[a] = x[a].wrapping_add(x[b]);
        x[d] = (x[d] ^ x[a]).rotate_left(16);
        x[c] = x[c].wrapping_add(x[d]);
        x[b] = (x[b] ^ x[c]).rotate_left(12);
        x[a] = x[a].wrapping_add(x[b]);
        x[d] = (x[d] ^ x[a]).rotate_left(8);
        x[c] = x[c].wrapping_add(x[d]);
        x[b] = (x[b] ^ x[c]).rotate_left(7);
    }
    for _ in 0..10 {
        qr(&mut x, 0, 4, 8, 12);
        qr(&mut x, 1, 5, 9, 13);
        qr(&mut x, 2, 6, 10, 14);
        qr(&mut x, 3, 7, 11, 15);
        qr(&mut x, 0, 5, 10, 15);
        qr(&mut x, 1, 6, 11, 12);
        qr(&mut x, 2, 7, 8, 13);
        qr(&mut x, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for (i, bytes) in out.chunks_exact_mut(4).enumerate() {
        bytes.copy_from_slice(&x[i].wrapping_add(state[i]).to_le_bytes());
    }
    out
}

// ---- pool and CRNG state ----------------------------------------------------

struct InputPool {
    hash: Blake2s,
    /// Credited entropy not yet consumed by a CRNG seeding.
    bits: u32,
}

struct Crng {
    key: [u8; 32],
    /// Distinguishes requests that read the same key (never repeats).
    generation: u64,
    last_reseed_us: u64,
}

static POOL: SpinLock<InputPool> = SpinLock::new(InputPool {
    hash: Blake2s::new(),
    bits: 0,
});
static CRNG: SpinLock<Crng> = SpinLock::new(Crng {
    key: [0; 32],
    generation: 0,
    last_reseed_us: 0,
});
static READY: AtomicBool = AtomicBool::new(false);
static READY_WAIT: WaitQueueHead = WaitQueueHead::new();
/// Total entropy ever credited, for diagnostics.
static CREDITED: AtomicU64 = AtomicU64::new(0);
/// 0: not probed, 1: no CPU source, 2: CPU source present.
static CPU_SOURCE: AtomicU8 = AtomicU8::new(0);
/// Bits credited from boot jitter, for the self-test report.
static JITTER_BITS: AtomicU32 = AtomicU32::new(0);

/// Per-CPU interrupt fast pool (Linux `fast_pool`), touched only by its own
/// CPU with interrupts masked.
struct FastPool {
    state: [u32; 4],
    count: u32,
}
struct FastPools([UnsafeCell<FastPool>; smp::MAX_CPUS]);
// SAFETY: each slot is accessed only by its CPU inside interrupt handling,
// where interrupts are masked, so there is never concurrent access.
unsafe impl Sync for FastPools {}
static FAST_POOLS: FastPools = FastPools(
    [const {
        UnsafeCell::new(FastPool {
            state: [0; 4],
            count: 0,
        })
    }; smp::MAX_CPUS],
);

fn mix(data: &[u8], credit_bits: u32) {
    let mut pool = POOL.lock();
    pool.hash.update(data);
    pool.bits = pool.bits.saturating_add(credit_bits);
    drop(pool);
    if credit_bits != 0 {
        CREDITED.fetch_add(u64::from(credit_bits), Ordering::Relaxed);
        if !READY.load(Ordering::Acquire) {
            try_initialize();
        }
    }
}

/// Hash the pool into a 32-byte seed, keeping the pool's own state chained.
fn extract_seed() -> ([u8; 32], u32) {
    let mut pool = POOL.lock();
    let bits = core::mem::take(&mut pool.bits);
    let seed = pool.hash.clone().finalize();
    // Chain: the next extraction depends on this one without revealing it.
    pool.hash = Blake2s::new();
    pool.hash.update(&seed);
    pool.hash.update(b"spacekernel input pool chain");
    (seed, bits)
}

fn reseed_crng(seed: &[u8; 32]) {
    let mut crng = CRNG.lock();
    let mut hash = Blake2s::new();
    hash.update(&crng.key);
    hash.update(seed);
    crng.key = hash.finalize();
    crng.last_reseed_us = crate::time::uptime_micros();
}

fn try_initialize() {
    if POOL.lock().bits < READY_BITS {
        return;
    }
    let (seed, _) = extract_seed();
    reseed_crng(&seed);
    if !READY.swap(true, Ordering::AcqRel) {
        READY_WAIT.wake_all();
    }
}

// ---- entropy sources --------------------------------------------------------

/// One word from the CPU's random instruction and the entropy it carries.
fn cpu_random_word() -> Option<(u64, u32)> {
    #[cfg(target_arch = "x86_64")]
    {
        use crate::cpuid::{Feature, has};
        if has(Feature::Rdseed) {
            for _ in 0..16 {
                let value: u64;
                let ok: u8;
                // SAFETY: CPUID reports RDSEED; carry reports success.
                unsafe {
                    core::arch::asm!("rdseed {v}", "setc {r}", v = out(reg) value,
                        r = out(reg_byte) ok, options(nostack));
                }
                if ok != 0 {
                    return Some((value, 64));
                }
            }
        }
        if has(Feature::Rdrand) {
            for _ in 0..16 {
                let value: u64;
                let ok: u8;
                // SAFETY: CPUID reports RDRAND; carry reports success.
                unsafe {
                    core::arch::asm!("rdrand {v}", "setc {r}", v = out(reg) value,
                        r = out(reg_byte) ok, options(nostack));
                }
                if ok != 0 {
                    // A DRBG output: trusted like Linux's random.trust_cpu.
                    return Some((value, 64));
                }
            }
        }
        None
    }
    #[cfg(target_arch = "aarch64")]
    {
        let features: u64;
        // SAFETY: EL1 can read the instruction set attribute register.
        unsafe {
            core::arch::asm!("mrs {}, ID_AA64ISAR0_EL1", out(reg) features,
                options(nomem, nostack));
        }
        if features >> 60 & 15 == 0 {
            return None;
        }
        for _ in 0..16 {
            let value: u64;
            let ok: u64;
            // SAFETY: FEAT_RNG is present; RNDR clears Z on success.
            unsafe {
                core::arch::asm!("mrs {v}, S3_3_C2_C4_0", "cset {r}, ne", v = out(reg) value,
                    r = out(reg) ok, options(nostack));
            }
            if ok != 0 {
                return Some((value, 64));
            }
        }
        None
    }
    #[cfg(target_arch = "riscv64")]
    {
        if !crate::cpuid::has(crate::cpuid::Feature::RiscvZkr) {
            return None;
        }
        // Zkr `seed` yields 16 bits per read in the ES16 state; it must be
        // accessed with a read-write instruction.
        let mut value = 0u64;
        let mut collected = 0;
        for _ in 0..256 {
            let seed: u64;
            // SAFETY: the device tree reports Zkr, and the SBI firmware grants
            // S-mode access (mseccfg.SSEED) when it advertises the extension.
            unsafe {
                core::arch::asm!("csrrw {}, 0x015, x0", out(reg) seed, options(nostack));
            }
            match seed >> 30 & 3 {
                0b10 => {
                    value = value << 16 | (seed & 0xffff);
                    collected += 1;
                    if collected == 4 {
                        return Some((value, 64));
                    }
                }
                0b11 => return None, // DEAD: unrecoverable self-test failure.
                _ => core::hint::spin_loop(), // BIST or WAIT
            }
        }
        None
    }
}

/// Whether a CPU random instruction is available (probed once).
pub fn supported() -> bool {
    match CPU_SOURCE.load(Ordering::Acquire) {
        0 => {
            let present = cpu_random_word().is_some();
            CPU_SOURCE.store(if present { 2 } else { 1 }, Ordering::Release);
            present
        }
        state => state == 2,
    }
}

/// Linux `try_to_generate_entropy`, reduced to execution-time jitter: time a
/// data-dependent memory walk and credit only samples whose duration changed
/// (a stuck-test), one bit per `JITTER_SAMPLES_PER_BIT` of them.
fn collect_jitter(target_bits: u32) -> u32 {
    let mut scratch = [0u64; 64];
    let mut hash = Blake2s::new();
    let mut previous = arch::counter();
    let mut previous_delta = 0u64;
    let mut moving = 0u32;
    let mut credited = 0u32;
    let mut index = 0usize;
    for sample in 0..JITTER_MAX_SAMPLES {
        // A short walk whose path depends on the timing just measured.
        for _ in 0..8 {
            index = (index + 1 + (scratch[index] as usize & 7)) % scratch.len();
            scratch[index] = scratch[index].rotate_left(7) ^ previous;
        }
        let now = arch::counter();
        let delta = now.wrapping_sub(previous);
        previous = now;
        hash.update(&delta.to_le_bytes());
        if delta != 0 && delta != previous_delta {
            moving += 1;
        }
        previous_delta = delta;
        if moving == JITTER_SAMPLES_PER_BIT {
            moving = 0;
            credited += 1;
        }
        if credited >= target_bits || (sample + 1) % 4096 == 0 && credited >= target_bits {
            break;
        }
    }
    hash.update(core::hint::black_box(&scratch.map(u64::to_le_bytes)).as_flattened());
    mix(&hash.finalize(), 0);
    credited
}

/// Linux `add_device_randomness`: mix public, boot-unique data, uncredited.
pub fn add_device_randomness(data: &[u8]) {
    mix(data, 0);
}

/// Called for every interrupt on the interrupted CPU. Folds the cycle counter
/// into a per-CPU fast pool; every `FAST_POOL_INTERRUPTS` interrupts the pool
/// is mixed into the input pool with one bit of credit.
pub fn add_interrupt_randomness(irq: u32) {
    let cpu = smp::current_cpu();
    // SAFETY: interrupt context on `cpu` with interrupts masked; see FastPools.
    let pool = unsafe { &mut *FAST_POOLS.0[cpu].get() };
    let counter = arch::counter();
    let s = &mut pool.state;
    // SipHash-style ARX mix, as Linux fast_mix.
    s[0] ^= counter as u32;
    s[1] ^= (counter >> 32) as u32 ^ irq;
    for _ in 0..2 {
        s[0] = s[0].wrapping_add(s[1]);
        s[1] = s[1].rotate_left(5) ^ s[0];
        s[2] = s[2].wrapping_add(s[3]);
        s[3] = s[3].rotate_left(8) ^ s[2];
        s[0] = s[0].rotate_left(16).wrapping_add(s[3]);
        s[3] = s[3].rotate_left(13) ^ s[0];
        s[2] = s[2].wrapping_add(s[1]);
        s[1] = s[1].rotate_left(7) ^ s[2];
        s[2] = s[2].rotate_left(16);
    }
    pool.count += 1;
    if pool.count < FAST_POOL_INTERRUPTS {
        return;
    }
    pool.count = 0;
    let mut bytes = [0u8; 16];
    for (chunk, word) in bytes.chunks_exact_mut(4).zip(pool.state) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    mix(&bytes, 1);
}

/// Seed the pool at boot. Runs after the clock and CPU features are known.
pub fn init() {
    let mut boot = [0u8; 24];
    boot[..8].copy_from_slice(&arch::counter().to_le_bytes());
    boot[8..16].copy_from_slice(&crate::time::realtime_micros().to_le_bytes());
    boot[16..].copy_from_slice(&(smp::current_cpu() as u64).to_le_bytes());
    add_device_randomness(&boot);
    add_device_randomness(crate::boot::cmdline().as_bytes());
    // The CRNG always has a key derived from what has been mixed so far, so
    // the non-blocking interfaces work before full initialization.
    if supported() {
        for _ in 0..(READY_BITS / 64 + 1) {
            if let Some((word, bits)) = cpu_random_word() {
                mix(&word.to_le_bytes(), bits);
            }
        }
    }
    if !ready() {
        let credited = collect_jitter(READY_BITS);
        JITTER_BITS.store(credited, Ordering::Relaxed);
        mix(&[], credited);
    }
    let (seed, bits) = extract_seed();
    reseed_crng(&seed);
    // Bits consumed by this seeding count toward readiness.
    if bits >= READY_BITS && !READY.swap(true, Ordering::AcqRel) {
        READY_WAIT.wake_all();
    }
}

// ---- output -----------------------------------------------------------------

/// Whether the CRNG has been seeded with at least 256 bits of entropy.
pub fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

/// Fill `dst` from the CRNG without blocking (Linux `get_random_bytes`).
pub fn get_random_bytes(dst: &mut [u8]) {
    if dst.is_empty() {
        return;
    }
    if ready() && supported() {
        // Keep mixing fresh CPU randomness in, like Linux's arch_get_random.
        if let Some((word, _)) = cpu_random_word() {
            mix(&word.to_le_bytes(), 0);
        }
    }
    let now = crate::time::uptime_micros();
    let due = ready() && now.saturating_sub(CRNG.lock().last_reseed_us) >= RESEED_INTERVAL_MICROS;
    if due {
        let (seed, _) = extract_seed();
        reseed_crng(&seed);
    }
    // Fast key erasure: derive this request's key and replace the global
    // key under the lock, then generate the stream without it.
    let (key, generation) = {
        let mut crng = CRNG.lock();
        crng.generation = crng.generation.wrapping_add(1);
        let generation = crng.generation;
        let block = chacha20_block(
            &crng.key,
            [0, 0, generation as u32, (generation >> 32) as u32],
        );
        crng.key.copy_from_slice(&block[..32]);
        let mut key = [0u8; 32];
        key.copy_from_slice(&block[32..]);
        (key, generation)
    };
    for (counter, chunk) in dst.chunks_mut(64).enumerate() {
        let block = chacha20_block(
            &key,
            [
                counter as u32,
                (counter as u64 >> 32) as u32,
                generation as u32,
                (generation >> 32) as u32 | 0x8000_0000,
            ],
        );
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
}

/// Wait until the CRNG is initialized (Linux `wait_for_random_bytes`).
/// `nonblock` returns WouldBlock instead of sleeping; a signal interrupts.
pub fn wait_ready(nonblock: bool) -> Result<(), VfsError> {
    if ready() {
        return Ok(());
    }
    if nonblock {
        return Err(VfsError::WouldBlock);
    }
    match READY_WAIT.wait_interruptible(ready) {
        crate::sched::WaitResult::Ready => Ok(()),
        _ => Err(VfsError::Interrupted),
    }
}

/// Compatibility wrapper: non-blocking CRNG output; never fails.
pub fn fill(dst: &mut [u8]) -> Result<(), VfsError> {
    get_random_bytes(dst);
    Ok(())
}

/// Short description of how the pool was seeded, for the boot report.
pub fn status() -> &'static str {
    match (ready(), supported()) {
        (true, true) => "crng(cpu)",
        (true, false) if JITTER_BITS.load(Ordering::Relaxed) >= READY_BITS => "crng(jitter)",
        (true, false) => "crng(jitter+irq)",
        (false, _) => "crng(pending)",
    }
}

/// Bits credited since boot.
pub fn credited_bits() -> u64 {
    CREDITED.load(Ordering::Relaxed)
}

/// Known-answer tests for the primitives (RFC 7693, RFC 8439 2.3.2).
pub fn self_test() -> Result<(), &'static str> {
    let mut hash = Blake2s::new();
    hash.update(b"abc");
    let expected: [u8; 32] = [
        0x50, 0x8c, 0x5e, 0x8c, 0x32, 0x7c, 0x14, 0xe2, 0xe1, 0xa7, 0x2b, 0xa3, 0x4e, 0xeb, 0x45,
        0x2f, 0x37, 0x45, 0x8b, 0x20, 0x9e, 0xd6, 0x3a, 0x29, 0x4d, 0x99, 0x9b, 0x4c, 0x86, 0x67,
        0x59, 0x82,
    ];
    if hash.finalize() != expected {
        return Err("BLAKE2s known answer mismatch");
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let block = chacha20_block(&key, [1, 0x0900_0000, 0x4a00_0000, 0]);
    let expected: [u8; 16] = [
        0x10, 0xf1, 0xe7, 0xe4, 0xd1, 0x3b, 0x59, 0x15, 0x50, 0x0f, 0xdd, 0x1f, 0xa3, 0x20, 0x71,
        0xc4,
    ];
    if block[..16] != expected || block[60..] != [0xa2, 0x50, 0x3c, 0x4e] {
        return Err("ChaCha20 known answer mismatch");
    }
    let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
    get_random_bytes(&mut a);
    get_random_bytes(&mut b);
    if a == b || a == [0; 64] {
        return Err("CRNG produced repeated output");
    }
    Ok(())
}
