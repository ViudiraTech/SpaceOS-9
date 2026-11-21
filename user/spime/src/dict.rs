/*
 *
 *       user/spime/src/dict.rs
 *       Zero-copy `mmap` reader for `spime.dict` (`SPIMEDIC` v1) and
 *       `spime.lm` (`SPIMELM` v1) with safe bounds checking and offline fallback.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use crate::pinyin::{syllable_id, syllable_str};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

pub const DEFAULT_DICT_PATH: &str = "/usr/share/spime/spime.dict";
pub const DEFAULT_LM_PATH: &str = "/usr/share/spime/spime.lm";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictEntry {
    pub word: String,
    pub qlogp: i16,
    pub origin: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnEntry {
    pub key: String,
    pub display: String,
    pub qlogp: i16,
}

enum Backing {
    Mmap {
        ptr: *const u8,
        len: usize,
    },
    Owned(Vec<u8>),
}

// Safety: read-only memory mapping is Sync + Send.
unsafe impl Send for Backing {}
unsafe impl Sync for Backing {}

impl Backing {
    fn open_mmap(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len() as usize;
        if len < 64 {
            return None;
        }
        // SpaceKernel maps regular files lazily via `MAP_PRIVATE` (see `src/syscall/memory.rs`);
        // `MAP_SHARED` fails with ENODEV on ext4 inodes, so use `MAP_PRIVATE` + `fs::read` fallback.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr != libc::MAP_FAILED && !ptr.is_null() {
            return Some(Self::Mmap {
                ptr: ptr as *const u8,
                len,
            });
        }
        std::fs::read(path).ok().map(Self::Owned)
    }

    #[inline]
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Mmap { ptr, len } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
            Self::Owned(v) => v.as_slice(),
        }
    }
}

impl Drop for Backing {
    fn drop(&mut self) {
        if let Self::Mmap { ptr, len } = *self {
            if len > 0 {
                unsafe {
                    libc::munmap(ptr as *mut libc::c_void, len);
                }
            }
        }
    }
}

#[inline]
fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn read_i16(b: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline]
fn read_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

/// Memory-mapped binary Chinese dictionary (`spime.dict`).
pub struct SpimeDict {
    backing: Backing,
    syl_count: usize,
    bucket_off: usize,
    keys_off: usize,
    key_count: usize,
    key_syls_off: usize,
    key_syls_count: usize,
    entries_off: usize,
    entry_count: usize,
    jp_keys_off: usize,
    jp_key_count: usize,
    jp_entries_off: usize,
    jp_entry_count: usize,
    en_keys_off: usize,
    en_key_count: usize,
    str_pool_off: usize,
    str_pool_len: usize,
    /// Map engine canonical `SylId` -> file `SylId` (`u16::MAX` if absent).
    engine_to_file_syl: Vec<u16>,
}

impl SpimeDict {
    pub fn open_default() -> Self {
        let candidates = [
            std::env::var_os("SPIME_DICT_PATH")
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from),
            Some(std::path::PathBuf::from(DEFAULT_DICT_PATH)),
            Some(std::path::PathBuf::from("build/spime/spime.dict")),
        ];
        for cand in candidates.into_iter().flatten() {
            if let Some(d) = Self::open(&cand) {
                return d;
            }
        }
        Self::embedded_fallback()
    }

    pub fn open(path: &Path) -> Option<Self> {
        let backing = Backing::open_mmap(path)?;
        Self::from_backing(backing)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        Self::from_backing(Backing::Owned(bytes))
    }

    fn from_backing(backing: Backing) -> Option<Self> {
        let b = backing.bytes();
        if b.len() < 128 || &b[0..8] != b"SPIMEDIC" {
            return None;
        }
        let ver = read_u32(b, 8);
        if ver != 1 {
            return None;
        }
        let syl_count = read_u32(b, 16) as usize;
        let syl_names_off = read_u32(b, 20) as usize;
        let syl_names_len = read_u32(b, 24) as usize;
        let bucket_off = read_u32(b, 28) as usize;
        let bucket_count = read_u32(b, 32) as usize;
        let keys_off = read_u32(b, 36) as usize;
        let key_count = read_u32(b, 40) as usize;
        let key_syls_off = read_u32(b, 44) as usize;
        let key_syls_count = read_u32(b, 48) as usize;
        let entries_off = read_u32(b, 52) as usize;
        let entry_count = read_u32(b, 56) as usize;
        let jp_keys_off = read_u32(b, 60) as usize;
        let jp_key_count = read_u32(b, 64) as usize;
        let jp_entries_off = read_u32(b, 68) as usize;
        let jp_entry_count = read_u32(b, 72) as usize;
        let en_keys_off = read_u32(b, 76) as usize;
        let en_key_count = read_u32(b, 80) as usize;
        let str_pool_off = read_u32(b, 84) as usize;
        let str_pool_len = read_u32(b, 88) as usize;

        if bucket_count != syl_count
            || syl_names_off.checked_add(syl_names_len)? > b.len()
            || bucket_off.checked_add(bucket_count.checked_mul(8)?)? > b.len()
            || keys_off.checked_add(key_count.checked_mul(12)?)? > b.len()
            || key_syls_off.checked_add(key_syls_count.checked_mul(2)?)? > b.len()
            || entries_off.checked_add(entry_count.checked_mul(8)?)? > b.len()
            || jp_keys_off.checked_add(jp_key_count.checked_mul(12)?)? > b.len()
            || jp_entries_off.checked_add(jp_entry_count.checked_mul(8)?)? > b.len()
            || en_keys_off.checked_add(en_key_count.checked_mul(12)?)? > b.len()
            || str_pool_off.checked_add(str_pool_len)? > b.len()
        {
            return None;
        }

        let mut engine_to_file_syl = vec![u16::MAX; crate::pinyin::MANDARIN_SYLLABLES.len()];
        let syl_slice = &b[syl_names_off..syl_names_off + syl_names_len];
        for (file_idx, chunk) in syl_slice
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .enumerate()
        {
            if file_idx >= syl_count {
                break;
            }
            if let Ok(s) = std::str::from_utf8(chunk) {
                if let Some(eng_id) = syllable_id(s) {
                    engine_to_file_syl[usize::from(eng_id)] = file_idx as u16;
                }
            }
        }

        Some(Self {
            backing,
            syl_count,
            bucket_off,
            keys_off,
            key_count,
            key_syls_off,
            key_syls_count,
            entries_off,
            entry_count,
            jp_keys_off,
            jp_key_count,
            jp_entries_off,
            jp_entry_count,
            en_keys_off,
            en_key_count,
            str_pool_off,
            str_pool_len,
            engine_to_file_syl,
        })
    }

    fn read_str(&self, off: u32, len: u8) -> Option<&str> {
        let start = self.str_pool_off + off as usize;
        let end = start + usize::from(len);
        if end > self.str_pool_off + self.str_pool_len {
            return None;
        }
        std::str::from_utf8(&self.backing.bytes()[start..end]).ok()
    }

    fn read_entries_slice(&self, base_off: usize, total: usize, start: u32, count: u16) -> Vec<DictEntry> {
        let s = start as usize;
        let n = usize::from(count);
        if s + n > total {
            return Vec::new();
        }
        let b = self.backing.bytes();
        let mut out = Vec::with_capacity(n);
        for i in s..s + n {
            let rec = base_off + i * 8;
            let w_off = read_u32(b, rec);
            let w_len = b[rec + 4];
            let origin = b[rec + 5];
            let qlogp = read_i16(b, rec + 6);
            if let Some(word) = self.read_str(w_off, w_len) {
                out.push(DictEntry {
                    word: word.to_string(),
                    qlogp,
                    origin,
                });
            }
        }
        out
    }

    #[inline]
    fn to_file_syl(&self, eng_id: u16) -> Option<u16> {
        let fid = *self.engine_to_file_syl.get(usize::from(eng_id))?;
        (fid != u16::MAX).then_some(fid)
    }

    /// Look up candidates matching an exact sequence of canonical engine `SylId`s.
    pub fn lookup_pinyin(&self, syl_ids: &[u16], limit: usize) -> Vec<DictEntry> {
        if syl_ids.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut file_ids = [0u16; 12];
        if syl_ids.len() > file_ids.len() {
            return Vec::new();
        }
        for (idx, &eng_id) in syl_ids.iter().enumerate() {
            let Some(fid) = self.to_file_syl(eng_id) else {
                return Vec::new();
            };
            file_ids[idx] = fid;
        }
        let target = &file_ids[..syl_ids.len()];
        let first_fid = target[0] as usize;
        if first_fid >= self.syl_count {
            return Vec::new();
        }
        let b = self.backing.bytes();
        let b_rec = self.bucket_off + first_fid * 8;
        let b_start = read_u32(b, b_rec) as usize;
        let b_cnt = read_u32(b, b_rec + 4) as usize;
        if b_cnt == 0 || b_start + b_cnt > self.key_count {
            return Vec::new();
        }

        let mut lo = b_start;
        let mut hi = b_start + b_cnt;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let k_rec = self.keys_off + mid * 12;
            let kl = usize::from(b[k_rec]);
            let ecnt = read_u16(b, k_rec + 2);
            let soff = read_u32(b, k_rec + 4) as usize;
            let estart = read_u32(b, k_rec + 8);
            if soff + kl > self.key_syls_count {
                return Vec::new();
            }
            let cmp = cmp_key_syls(b, self.key_syls_off + soff * 2, kl, target);
            match cmp {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let take = ecnt.min(limit.min(u16::MAX as usize) as u16);
                    return self.read_entries_slice(self.entries_off, self.entry_count, estart, take);
                }
            }
        }
        Vec::new()
    }

    /// Look up Jianpin (initial-letters-only) candidates for e.g. `"zg"`, `"bj"`, `"zrm"`.
    pub fn lookup_jianpin(&self, initials: &str, limit: usize) -> Vec<DictEntry> {
        let key = initials.as_bytes();
        if key.len() < 2 || key.len() > 5 || limit == 0 || self.jp_key_count == 0 {
            return Vec::new();
        }
        let b = self.backing.bytes();
        let mut lo = 0usize;
        let mut hi = self.jp_key_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let rec = self.jp_keys_off + mid * 12;
            let k_off = read_u32(b, rec);
            let k_len = b[rec + 4];
            let Some(mid_str) = self.read_str(k_off, k_len) else {
                return Vec::new();
            };
            match mid_str.as_bytes().cmp(key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let ecnt = read_u16(b, rec + 6);
                    let estart = read_u32(b, rec + 8);
                    let take = ecnt.min(limit.min(u16::MAX as usize) as u16);
                    return self.read_entries_slice(
                        self.jp_entries_off,
                        self.jp_entry_count,
                        estart,
                        take,
                    );
                }
            }
        }
        Vec::new()
    }

    /// Look up exact and prefix English / CN-EN mixed candidates matching lowercase ASCII `prefix`.
    pub fn lookup_english_prefix(&self, prefix: &str, limit: usize) -> Vec<EnEntry> {
        let p = prefix.to_ascii_lowercase();
        let pb = p.as_bytes();
        if pb.len() < 2 || limit == 0 || self.en_key_count == 0 {
            return Vec::new();
        }
        let b = self.backing.bytes();
        // Lower bound binary search on `en_keys`.
        let mut lo = 0usize;
        let mut hi = self.en_key_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let rec = self.en_keys_off + mid * 12;
            let k_off = read_u32(b, rec);
            let k_len = b[rec + 8];
            let Some(ms) = self.read_str(k_off, k_len) else {
                return Vec::new();
            };
            if ms.as_bytes() < pb {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut out = Vec::new();
        for idx in lo..self.en_key_count.min(lo + 64) {
            let rec = self.en_keys_off + idx * 12;
            let k_off = read_u32(b, rec);
            let d_off = read_u32(b, rec + 4);
            let k_len = b[rec + 8];
            let d_len = b[rec + 9];
            let qlogp = read_i16(b, rec + 10);
            let Some(ks) = self.read_str(k_off, k_len) else {
                break;
            };
            if !ks.as_bytes().starts_with(pb) {
                break;
            }
            if let Some(ds) = self.read_str(d_off, d_len) {
                out.push(EnEntry {
                    key: ks.to_string(),
                    display: ds.to_string(),
                    qlogp,
                });
            }
        }
        out.sort_by(|a, b| {
            let a_exact = a.key.len() == pb.len();
            let b_exact = b.key.len() == pb.len();
            b_exact
                .cmp(&a_exact)
                .then_with(|| b.qlogp.cmp(&a.qlogp))
                .then_with(|| a.key.len().cmp(&b.key.len()))
        });
        out.truncate(limit);
        out
    }

    /// Return `(key_count, entry_count, is_fallback)` for startup diagnostics.
    pub fn stats(&self) -> (usize, usize, bool) {
        (self.key_count, self.entry_count, self.key_count <= 64)
    }

    pub fn embedded_fallback() -> Self {
        Self::from_bytes(build_tiny_fallback_dict()).expect("builtin fallback dict valid")
    }
}

fn cmp_key_syls(b: &[u8], base: usize, kl: usize, target: &[u16]) -> std::cmp::Ordering {
    let min_len = kl.min(target.len());
    for i in 0..min_len {
        let lhs = read_u16(b, base + i * 2);
        let rhs = target[i];
        match lhs.cmp(&rhs) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }
    }
    kl.cmp(&target.len())
}

/// Open-addressed memory-mapped binary bigram language model (`spime.lm`).
pub struct SpimeLm {
    backing: Backing,
    mask: usize,
    pub backoff_pen_qlogp: i16,
    pub unigram_default_qlogp: i16,
    keys_off: usize,
    vals_off: usize,
}

impl SpimeLm {
    pub fn open_default() -> Self {
        let candidates = [
            std::env::var_os("SPIME_LM_PATH")
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from),
            Some(std::path::PathBuf::from(DEFAULT_LM_PATH)),
            Some(std::path::PathBuf::from("build/spime/spime.lm")),
        ];
        for cand in candidates.into_iter().flatten() {
            if let Some(lm) = Self::open(&cand) {
                return lm;
            }
        }
        Self::empty_fallback()
    }

    pub fn open(path: &Path) -> Option<Self> {
        let backing = Backing::open_mmap(path)?;
        Self::from_backing(backing)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        Self::from_backing(Backing::Owned(bytes))
    }

    fn from_backing(backing: Backing) -> Option<Self> {
        let b = backing.bytes();
        if b.len() < 64 || &b[0..8] != b"SPIMELM\0" {
            return None;
        }
        if read_u32(b, 8) != 1 {
            return None;
        }
        let total_slots = read_u32(b, 12) as usize;
        if !total_slots.is_power_of_two() || total_slots == 0 {
            return None;
        }
        let backoff_pen_qlogp = read_i16(b, 20);
        let unigram_default_qlogp = read_i16(b, 24);
        let keys_off = read_u32(b, 28) as usize;
        let vals_off = read_u32(b, 32) as usize;
        if keys_off.checked_add(total_slots.checked_mul(8)?)? > b.len()
            || vals_off.checked_add(total_slots.checked_mul(2)?)? > b.len()
        {
            return None;
        }
        Some(Self {
            backing,
            mask: total_slots - 1,
            backoff_pen_qlogp,
            unigram_default_qlogp,
            keys_off,
            vals_off,
        })
    }

    pub fn empty_fallback() -> Self {
        let cap = 64usize;
        let mut buf = vec![0u8; 64 + cap * 10];
        buf[0..8].copy_from_slice(b"SPIMELM\0");
        buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        buf[12..16].copy_from_slice(&(cap as u32).to_le_bytes());
        buf[20..22].copy_from_slice(&(-80i16).to_le_bytes());
        buf[24..26].copy_from_slice(&(-750i16).to_le_bytes());
        buf[28..32].copy_from_slice(&64u32.to_le_bytes());
        buf[32..36].copy_from_slice(&((64 + cap * 8) as u32).to_le_bytes());
        Self::from_bytes(buf).expect("fallback LM valid")
    }

    #[inline]
    pub fn bigram_qlogp(&self, w1: &str, w2: &str) -> Option<i16> {
        let sig = bigram_sig(w1.as_bytes(), w2.as_bytes());
        let b = self.backing.bytes();
        let mut slot = (mix64(sig) as usize) & self.mask;
        for _ in 0..=self.mask {
            let k = read_u64(b, self.keys_off + slot * 8);
            if k == 0 {
                return None;
            }
            if k == sig {
                return Some(read_i16(b, self.vals_off + slot * 2));
            }
            slot = (slot + 1) & self.mask;
        }
        None
    }
}

#[inline]
fn fnv1a32(data: &[u8]) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for &b in data {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[inline]
fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[inline]
fn bigram_sig(w1: &[u8], w2: &[u8]) -> u64 {
    let sig = (u64::from(fnv1a32(w1)) << 32) | u64::from(fnv1a32(w2));
    if sig == 0 { 1 } else { sig }
}

fn build_tiny_fallback_dict() -> Vec<u8> {
    let seed: &[(&[&str], &[(&str, i16)])] = &[
        (&["bei", "jing"], &[("北京", -120)]),
        (&["bu", "shi"], &[("不是", -130)]),
        (&["dian", "ji"], &[("点击", -130)]),
        (&["dian", "nao"], &[("电脑", -125)]),
        (&["hao"], &[("好", -140), ("号", -210)]),
        (&["jin", "tian"], &[("今天", -125)]),
        (&["la", "ji"], &[("垃圾", -125)]),
        (&["ni"], &[("你", -120), ("拟", -280)]),
        (&["ni", "hao"], &[("你好", -105)]),
        (&["shi"], &[("是", -110), ("时", -180), ("试", -240)]),
        (&["shi", "jie"], &[("世界", -140)]),
        (&["shu", "biao"], &[("鼠标", -130)]),
        (&["shu", "ru", "fa"], &[("输入法", -120)]),
        (&["tian", "qi"], &[("天气", -135)]),
        (&["wo"], &[("我", -100)]),
        (&["wo", "men"], &[("我们", -110)]),
        (&["xing", "neng"], &[("性能", -130)]),
        (&["you", "hua"], &[("优化", -130)]),
        (&["zhong"], &[("中", -130), ("种", -220)]),
        (&["zhong", "guo"], &[("中国", -105)]),
        (&["zhong", "wen"], &[("中文", -130)]),
    ];
    let n_syls = crate::pinyin::MANDARIN_SYLLABLES.len();
    let mut syl_names = Vec::new();
    for &s in crate::pinyin::MANDARIN_SYLLABLES {
        syl_names.extend_from_slice(s.as_bytes());
        syl_names.push(0);
    }

    let mut keyed: Vec<(Vec<u16>, Vec<(&str, i16)>)> = seed
        .iter()
        .map(|(pys, words)| {
            let ids: Vec<u16> = pys
                .iter()
                .map(|p| syllable_id(p).unwrap_or(0))
                .collect();
            (ids, words.to_vec())
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));

    let mut buckets = vec![(0u32, 0u32); n_syls];
    let mut str_pool = Vec::new();
    let mut keys_bytes = Vec::new();
    let mut key_syls_bytes = Vec::new();
    let mut entries_bytes = Vec::new();
    let mut entry_idx = 0u32;

    for (k_idx, (ids, words)) in keyed.iter().enumerate() {
        let first = usize::from(ids[0]);
        if buckets[first].1 == 0 {
            buckets[first].0 = k_idx as u32;
        }
        buckets[first].1 += 1;
        let syl_off = (key_syls_bytes.len() / 2) as u32;
        for &id in ids {
            key_syls_bytes.extend_from_slice(&id.to_le_bytes());
        }
        let estart = entry_idx;
        for &(w, ql) in words {
            let w_off = str_pool.len() as u32;
            let wb = w.as_bytes();
            str_pool.extend_from_slice(wb);
            entries_bytes.extend_from_slice(&w_off.to_le_bytes());
            entries_bytes.push(wb.len() as u8);
            entries_bytes.push(1);
            entries_bytes.extend_from_slice(&ql.to_le_bytes());
            entry_idx += 1;
        }
        keys_bytes.push(ids.len() as u8);
        keys_bytes.push(0);
        keys_bytes.extend_from_slice(&(words.len() as u16).to_le_bytes());
        keys_bytes.extend_from_slice(&syl_off.to_le_bytes());
        keys_bytes.extend_from_slice(&estart.to_le_bytes());
    }

    let mut bucket_bytes = Vec::with_capacity(n_syls * 8);
    for (st, cnt) in buckets {
        bucket_bytes.extend_from_slice(&st.to_le_bytes());
        bucket_bytes.extend_from_slice(&cnt.to_le_bytes());
    }

    let align4 = |x: usize| (x + 3) & !3;
    let off_syl = 128usize;
    let off_buck = align4(off_syl + syl_names.len());
    let off_keys = align4(off_buck + bucket_bytes.len());
    let off_ks = align4(off_keys + keys_bytes.len());
    let off_ent = align4(off_ks + key_syls_bytes.len());
    let off_pool = align4(off_ent + entries_bytes.len());
    let total = off_pool + str_pool.len();

    let mut out = vec![0u8; total];
    out[0..8].copy_from_slice(b"SPIMEDIC");
    out[8..12].copy_from_slice(&1u32.to_le_bytes());
    out[16..20].copy_from_slice(&(n_syls as u32).to_le_bytes());
    out[20..24].copy_from_slice(&(off_syl as u32).to_le_bytes());
    out[24..28].copy_from_slice(&(syl_names.len() as u32).to_le_bytes());
    out[28..32].copy_from_slice(&(off_buck as u32).to_le_bytes());
    out[32..36].copy_from_slice(&(n_syls as u32).to_le_bytes());
    out[36..40].copy_from_slice(&(off_keys as u32).to_le_bytes());
    out[40..44].copy_from_slice(&(keyed.len() as u32).to_le_bytes());
    out[44..48].copy_from_slice(&(off_ks as u32).to_le_bytes());
    out[48..52].copy_from_slice(&((key_syls_bytes.len() / 2) as u32).to_le_bytes());
    out[52..56].copy_from_slice(&(off_ent as u32).to_le_bytes());
    out[56..60].copy_from_slice(&entry_idx.to_le_bytes());
    out[60..64].copy_from_slice(&(off_ent as u32).to_le_bytes());
    out[68..72].copy_from_slice(&(off_ent as u32).to_le_bytes());
    out[76..80].copy_from_slice(&(off_ent as u32).to_le_bytes());
    out[84..88].copy_from_slice(&(off_pool as u32).to_le_bytes());
    out[88..92].copy_from_slice(&(str_pool.len() as u32).to_le_bytes());

    out[off_syl..off_syl + syl_names.len()].copy_from_slice(&syl_names);
    out[off_buck..off_buck + bucket_bytes.len()].copy_from_slice(&bucket_bytes);
    out[off_keys..off_keys + keys_bytes.len()].copy_from_slice(&keys_bytes);
    out[off_ks..off_ks + key_syls_bytes.len()].copy_from_slice(&key_syls_bytes);
    out[off_ent..off_ent + entries_bytes.len()].copy_from_slice(&entries_bytes);
    out[off_pool..off_pool + str_pool.len()].copy_from_slice(&str_pool);
    let _ = syllable_str(0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_dict_and_compiled_dict_lookup_cleanly() {
        let fb = SpimeDict::embedded_fallback();
        let nh = fb.lookup_pinyin(
            &[
                syllable_id("ni").unwrap(),
                syllable_id("hao").unwrap(),
            ],
            5,
        );
        assert!(nh.iter().any(|e| e.word == "你好"));

        let lm = SpimeLm::empty_fallback();
        assert_eq!(lm.bigram_qlogp("你好", "世界"), None);
    }
}
