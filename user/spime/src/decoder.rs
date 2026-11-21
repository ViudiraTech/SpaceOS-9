/*
 *
 *       user/spime/src/decoder.rs
 *       Whole-sentence Viterbi Beam Search Pinyin/Shuangpin decoder with
 *       partial-prefix selection, dynamic learning, mixed EN/CN, and candidate paging.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use crate::dict::{SpimeDict, SpimeLm};
use crate::pinyin::{
    best_segmentation, build_lattice, expand_fuzzy_variants, format_preedit, syllable_id,
    syllable_str, FuzzyPinyin, SegmentKind, MANDARIN_SYLLABLES,
};
use crate::punct::special_shortcut_candidates;
use crate::shuangpin::{decode_shuangpin_sequence, ShuangpinScheme};
use crate::userdb::UserDb;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

struct CachedFuzzyTable {
    cfg: FuzzyPinyin,
    /// Up to 3 fuzzy alternate syllable IDs per canonical syllable.
    alts: Vec<([u16; 3], u8)>,
}

static FUZZY_CACHE: OnceLock<Mutex<Option<CachedFuzzyTable>>> = OnceLock::new();

fn fuzzy_alts_for(base_id: u16, cfg: &FuzzyPinyin, buf: &mut [u16; 3]) -> usize {
    if !cfg.any() {
        return 0;
    }
    let idx = usize::from(base_id);
    let n_syls = MANDARIN_SYLLABLES.len();
    if idx >= n_syls {
        return 0;
    }
    let lock = FUZZY_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap();
    let rebuild = guard.as_ref().is_none_or(|t| t.cfg != *cfg);
    if rebuild {
        let mut alts = vec![([0u16; 3], 0u8); n_syls];
        for (sid, slot) in alts.iter_mut().enumerate() {
            let vars = expand_fuzzy_variants(sid as u16, cfg);
            let mut cnt = 0usize;
            for &(vid, is_fuzzy) in &vars {
                if is_fuzzy && cnt < 3 {
                    slot.0[cnt] = vid;
                    cnt += 1;
                }
            }
            slot.1 = cnt as u8;
        }
        *guard = Some(CachedFuzzyTable { cfg: *cfg, alts });
    }
    let (arr, len) = guard.as_ref().unwrap().alts[idx];
    *buf = arr;
    usize::from(len)
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub scheme: ShuangpinScheme,
    pub fuzzy: FuzzyPinyin,
    pub mixed_english: bool,
    pub cn_punct: bool,
    pub page_size: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            scheme: ShuangpinScheme::Full,
            fuzzy: FuzzyPinyin::default(),
            mixed_english: true,
            cn_punct: true,
            page_size: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    /// Normalized syllable key (`"ni'hao"`), used for UserDb learning & deletion.
    pub syl_key: String,
    /// Number of raw input units consumed (`len` in bytes for Full, or col index for Shuangpin).
    pub consumed_end: usize,
    pub is_full_sentence: bool,
    pub is_english_or_raw: bool,
    pub score: i32,
}

#[derive(Clone, Debug, Default)]
pub struct ComposingSnapshot {
    pub preedit_display: String,
    pub confirmed_text: String,
    pub candidates: Vec<Candidate>,
}

/// Interactive composition session holding raw input, partial-committed prefix, and paged results.
pub struct ComposeSession {
    pub raw: String,
    pub committed_prefix: String,
    pub committed_syl_keys: Vec<String>,
    pub page: usize,
    pub selected_in_page: usize,
    pub snapshot: ComposingSnapshot,
}

impl Default for ComposeSession {
    fn default() -> Self {
        Self {
            raw: String::new(),
            committed_prefix: String::new(),
            committed_syl_keys: Vec::new(),
            page: 0,
            selected_in_page: 0,
            snapshot: ComposingSnapshot::default(),
        }
    }
}

impl ComposeSession {
    pub fn is_active(&self) -> bool {
        !self.raw.is_empty() || !self.committed_prefix.is_empty()
    }

    pub fn clear(&mut self) {
        self.raw.clear();
        self.committed_prefix.clear();
        self.committed_syl_keys.clear();
        self.page = 0;
        self.selected_in_page = 0;
        self.snapshot = ComposingSnapshot::default();
    }

    pub fn page_count(&self, page_size: usize) -> usize {
        let ps = page_size.max(1);
        self.snapshot.candidates.len().div_ceil(ps).max(1)
    }

    pub fn current_page_slice(&self, page_size: usize) -> &[Candidate] {
        let ps = page_size.max(1);
        let start = self.page * ps;
        if start >= self.snapshot.candidates.len() {
            return &[];
        }
        let end = (start + ps).min(self.snapshot.candidates.len());
        &self.snapshot.candidates[start..end]
    }
}

#[derive(Clone)]
struct WordEdge {
    start: usize,
    end: usize,
    word: String,
    syl_key: String,
    base_qlogp: i32,
    is_partial: bool,
}

#[derive(Clone)]
struct BeamHyp {
    score: i32,
    text: String,
    last_word: String,
    syl_keys: Vec<String>,
    used_partial: bool,
}

pub fn recompute(
    dict: &SpimeDict,
    lm: &SpimeLm,
    userdb: &UserDb,
    cfg: &EngineConfig,
    sess: &mut ComposeSession,
) {
    let raw = sess.raw.clone();
    if raw.is_empty() && sess.committed_prefix.is_empty() {
        sess.snapshot = ComposingSnapshot::default();
        return;
    }
    sess.page = 0;
    sess.selected_in_page = 0;

    if cfg.scheme == ShuangpinScheme::Full {
        decode_full_pinyin(dict, lm, userdb, cfg, sess);
    } else {
        decode_shuangpin_mode(dict, lm, userdb, cfg, sess);
    }
}

fn decode_full_pinyin(
    dict: &SpimeDict,
    lm: &SpimeLm,
    userdb: &UserDb,
    cfg: &EngineConfig,
    sess: &mut ComposeSession,
) {
    let raw = &sess.raw;
    let spans = best_segmentation(raw);
    let formatted_tail = format_preedit(raw, &spans);
    let preedit_display = if sess.committed_prefix.is_empty() {
        formatted_tail
    } else {
        format!("{}{}", sess.committed_prefix, formatted_tail)
    };

    let n = raw.len();
    let lower = raw.to_ascii_lowercase();
    let has_upper = raw.bytes().any(|b| b.is_ascii_uppercase());
    let lattice = build_lattice(&lower);

    // Collect phrase edges across syllable sequences of length 1..=7 at each start byte `s`.
    let mut phrase_edges: Vec<Vec<WordEdge>> = vec![Vec::new(); n + 1];

    // Helper: gather candidate chains up to depth 7 starting at position `s`.
    for s in 0..n {
        if lower.as_bytes()[s] == b'\'' {
            continue;
        }
        let mut stack: Vec<(usize, Vec<u16>, i32)> = Vec::new();
        for edge in &lattice.edges_at[s] {
            if let SegmentKind::Syllable { syl_id, typo } = edge.kind {
                let typo_pen = if typo { -95 } else { 0 };
                stack.push((edge.end, vec![syl_id], typo_pen));
            }
        }

        // Trailing prefix match if `s..n` has no full syllable but is a valid syllable prefix (`"zhon"`).
        if s + 1 <= n && n - s <= 5 && stack.is_empty() && !lower[s..n].contains('\'') {
            let pref = &lower[s..n];
            let mut matched = 0usize;
            for &syl in MANDARIN_SYLLABLES {
                if syl.starts_with(pref) {
                    if let Some(id) = syllable_id(syl) {
                        stack.push((n, vec![id], -140));
                        matched += 1;
                        if matched >= 4 {
                            break;
                        }
                    }
                }
            }
        }

        while let Some((end_pos, syl_seq, acc_pen)) = stack.pop() {
            let klen = syl_seq.len();
            let key_str = join_syls(&syl_seq);
            let word_limit = match klen {
                1 => 24,
                2 => 10,
                3 => 6,
                _ => 4,
            };

            let mut pushed_any = false;
            // User lexicon entries first with strong frequency + recency boost.
            for ue in userdb.lookup(&key_str) {
                let boost = userdb.boost_qlogp(&key_str, &ue.word);
                phrase_edges[s].push(WordEdge {
                    start: s,
                    end: end_pos,
                    word: ue.word.clone(),
                    syl_key: key_str.clone(),
                    base_qlogp: -100 + boost + acc_pen + (ue.word.chars().count() as i32 - 1) * 55,
                    is_partial: false,
                });
                pushed_any = true;
            }

            // Expand fuzzy variants using cached zero-alloc table (`klen <= 2`; Beam merges longer sentences).
            let mut query_seqs: [([u16; 6], i32); 6] = [([0u16; 6], 0); 6];
            query_seqs[0].0[..klen].copy_from_slice(&syl_seq);
            let mut q_cnt = 1usize;
            if cfg.fuzzy.any() && klen <= 2 {
                for pos in 0..klen {
                    let mut alts = [0u16; 3];
                    let acnt = fuzzy_alts_for(syl_seq[pos], &cfg.fuzzy, &mut alts);
                    let cur_len = q_cnt;
                    for &alt_id in &alts[..acnt] {
                        for qi in 0..cur_len {
                            if q_cnt >= query_seqs.len() {
                                break;
                            }
                            let mut alt_seq = query_seqs[qi].0;
                            alt_seq[pos] = alt_id;
                            let pen = query_seqs[qi].1 - 65;
                            query_seqs[q_cnt] = (alt_seq, pen);
                            q_cnt += 1;
                        }
                    }
                }
            }

            for &(qseq, f_pen) in &query_seqs[..q_cnt] {
                let entries = dict.lookup_pinyin(&qseq[..klen], word_limit);
                let qkey = if f_pen == 0 {
                    key_str.clone()
                } else {
                    join_syls(&qseq[..klen])
                };
                for ent in entries {
                    if userdb.is_hidden(&qkey, &ent.word) {
                        continue;
                    }
                    let bonus = ((ent.word.chars().count() as i32) - 1) * 48
                        + userdb.boost_qlogp(&qkey, &ent.word);
                    phrase_edges[s].push(WordEdge {
                        start: s,
                        end: end_pos,
                        word: ent.word,
                        syl_key: qkey.clone(),
                        base_qlogp: i32::from(ent.qlogp) + acc_pen + f_pen + bonus,
                        is_partial: false,
                    });
                    pushed_any = true;
                }
            }

            // Extend multi-syllable chain up to 6 syllables if any prefix/subphrase existed or klen < 4.
            if klen < 6 && (pushed_any || klen < 3) && end_pos < n {
                let next_s = if lower.as_bytes()[end_pos] == b'\'' {
                    end_pos + 1
                } else {
                    end_pos
                };
                if next_s < n {
                    for next_edge in &lattice.edges_at[next_s] {
                        if let SegmentKind::Syllable { syl_id, typo } = next_edge.kind {
                            let mut nseq = syl_seq.clone();
                            nseq.push(syl_id);
                            stack.push((
                                next_edge.end,
                                nseq,
                                acc_pen + if typo { -95 } else { 0 },
                            ));
                        }
                    }
                }
            }
        }

        // Skip apostrophes smoothly in beam graph.
        phrase_edges[s].sort_by(|a, b| b.base_qlogp.cmp(&a.base_qlogp));
        // Deduplicate identical `(end, word)` keeping best score.
        let mut seen_ew: HashSet<(usize, &str)> = HashSet::new();
        let mut deduped = Vec::with_capacity(phrase_edges[s].len().min(36));
        for e in &phrase_edges[s] {
            if seen_ew.insert((e.end, &e.word)) {
                deduped.push(e.clone());
                if deduped.len() >= 36 {
                    break;
                }
            }
        }
        phrase_edges[s] = deduped;
    }

    // Viterbi Beam Search across byte offsets `0..=n`.
    const BEAM: usize = 12;
    let mut beams: Vec<Vec<BeamHyp>> = vec![Vec::new(); n + 1];
    beams[0].push(BeamHyp {
        score: 0,
        text: String::new(),
        last_word: String::new(),
        syl_keys: Vec::new(),
        used_partial: false,
    });

    for pos in 0..n {
        if beams[pos].is_empty() {
            continue;
        }
        if lower.as_bytes()[pos] == b'\'' {
            let next_b = beams[pos].clone();
            beams[pos + 1].extend(next_b);
            continue;
        }
        beams[pos].sort_by(|a, b| b.score.cmp(&a.score));
        beams[pos].truncate(BEAM);
        let cur_hyps = beams[pos].clone();

        for hyp in &cur_hyps {
            for edge in &phrase_edges[pos] {
                let trans = if hyp.last_word.is_empty() {
                    edge.base_qlogp
                } else if let Some(bg) = lm.bigram_qlogp(&hyp.last_word, &edge.word) {
                    i32::from(bg) + edge.base_qlogp / 2 + 160
                } else {
                    edge.base_qlogp + i32::from(lm.backoff_pen_qlogp)
                };
                let mut nkeys = hyp.syl_keys.clone();
                nkeys.push(edge.syl_key.clone());
                let mut ntext = String::with_capacity(hyp.text.len() + edge.word.len());
                ntext.push_str(&hyp.text);
                ntext.push_str(&edge.word);
                beams[edge.end].push(BeamHyp {
                    score: hyp.score + trans,
                    text: ntext,
                    last_word: edge.word.clone(),
                    syl_keys: nkeys,
                    used_partial: hyp.used_partial || edge.is_partial,
                });
            }
        }
    }

    let mut cands: Vec<Candidate> = Vec::new();
    let mut seen_text: HashSet<String> = HashSet::new();

    // 1. Special date/time shortcuts (`rq` / `sj`).
    if !has_upper && sess.committed_prefix.is_empty() {
        for sc in special_shortcut_candidates(&lower) {
            if seen_text.insert(sc.clone()) {
                cands.push(Candidate {
                    text: sc,
                    syl_key: String::new(),
                    consumed_end: n,
                    is_full_sentence: true,
                    is_english_or_raw: false,
                    score: 1000,
                });
            }
        }
    }

    // 2. Full-sentence Beam Search top winners.
    if !beams[n].is_empty() && !has_upper {
        beams[n].sort_by(|a, b| b.score.cmp(&a.score));
        for hyp in beams[n].iter().take(8) {
            let hyp_key = hyp.syl_keys.join("'");
            if userdb.is_hidden(&hyp_key, &hyp.text) {
                continue;
            }
            if seen_text.insert(hyp.text.clone()) {
                cands.push(Candidate {
                    text: hyp.text.clone(),
                    syl_key: hyp_key,
                    consumed_end: n,
                    is_full_sentence: true,
                    is_english_or_raw: false,
                    score: hyp.score,
                });
                if cands.len() >= 4 {
                    break;
                }
            }
        }
    }

    // 3. Longest & highest-frequency prefix phrase candidates at start `0` (for partial commit / disambiguation).
    if !has_upper {
        let mut prefix_sorted = phrase_edges[0].clone();
        prefix_sorted.sort_by(|a, b| {
            let full_a = a.end == n;
            let full_b = b.end == n;
            full_b
                .cmp(&full_a)
                .then_with(|| (b.end / 2).cmp(&(a.end / 2)))
                .then_with(|| b.base_qlogp.cmp(&a.base_qlogp))
        });
        for edge in prefix_sorted.into_iter().take(28) {
            if seen_text.insert(edge.word.clone()) {
                cands.push(Candidate {
                    text: edge.word,
                    syl_key: edge.syl_key,
                    consumed_end: edge.end,
                    is_full_sentence: edge.end == n,
                    is_english_or_raw: false,
                    score: edge.base_qlogp,
                });
            }
        }
    }

    // 4. Jianpin initials lookup (`"zg"` -> `"中国"`, `"bj"` -> `"北京"`, `"nh"` -> `"你好"`).
    if !has_upper && n >= 2 && n <= 5 && lower.bytes().all(|b| b.is_ascii_lowercase()) {
        for ent in dict.lookup_jianpin(&lower, 10) {
            if !userdb.is_hidden(&lower, &ent.word) && seen_text.insert(ent.word.clone()) {
                cands.push(Candidate {
                    text: ent.word,
                    syl_key: String::new(),
                    consumed_end: n,
                    is_full_sentence: true,
                    is_english_or_raw: false,
                    score: i32::from(ent.qlogp) - 60,
                });
            }
        }
    }

    // 5. English & CN-EN mixed dictionary matches (exact & prefix).
    if cfg.mixed_english && n >= 2 && lower.bytes().all(|b| b.is_ascii_alphanumeric()) {
        for ent in dict.lookup_english_prefix(&lower, 4) {
            if seen_text.insert(ent.display.clone()) {
                let exact_bonus = if ent.key == lower { 250 } else { -120 };
                cands.push(Candidate {
                    text: ent.display,
                    syl_key: String::new(),
                    consumed_end: n,
                    is_full_sentence: true,
                    is_english_or_raw: true,
                    score: i32::from(ent.qlogp) + exact_bonus,
                });
            }
        }
    }

    // 6. Fallback raw ASCII candidate when uppercase/mixed or dict missed.
    if has_upper || cands.is_empty() || cfg.mixed_english {
        if seen_text.insert(raw.clone()) {
            cands.push(Candidate {
                text: raw.clone(),
                syl_key: String::new(),
                consumed_end: n,
                is_full_sentence: true,
                is_english_or_raw: true,
                score: -2500,
            });
        }
    }

    sess.snapshot = ComposingSnapshot {
        preedit_display,
        confirmed_text: sess.committed_prefix.clone(),
        candidates: cands,
    };
}

fn decode_shuangpin_mode(
    dict: &SpimeDict,
    lm: &SpimeLm,
    userdb: &UserDb,
    cfg: &EngineConfig,
    sess: &mut ComposeSession,
) {
    let raw = &sess.raw;
    let lower = raw.to_ascii_lowercase();
    let (cols, trailing, disp_parts) = decode_shuangpin_sequence(cfg.scheme, &lower);
    let formatted_tail = disp_parts.join("'");
    let preedit_display = if sess.committed_prefix.is_empty() {
        formatted_tail
    } else {
        format!("{}{}", sess.committed_prefix, formatted_tail)
    };

    let m = cols.len();
    let mut phrase_edges: Vec<Vec<WordEdge>> = vec![Vec::new(); m + 1];
    for s in 0..m {
        let max_len = (m - s).min(6);
        let mut stack: Vec<(usize, Vec<u16>)> = cols[s].iter().map(|&id| (s + 1, vec![id])).collect();
        while let Some((end_c, seq)) = stack.pop() {
            let key_str = join_syls(&seq);
            let lim = if seq.len() == 1 { 20 } else { 8 };
            for ue in userdb.lookup(&key_str) {
                phrase_edges[s].push(WordEdge {
                    start: s,
                    end: end_c,
                    word: ue.word.clone(),
                    syl_key: key_str.clone(),
                    base_qlogp: -100 + userdb.boost_qlogp(&key_str, &ue.word),
                    is_partial: false,
                });
            }
            for ent in dict.lookup_pinyin(&seq, lim) {
                if userdb.is_hidden(&key_str, &ent.word) {
                    continue;
                }
                let bonus = ((ent.word.chars().count() as i32) - 1) * 48
                    + userdb.boost_qlogp(&key_str, &ent.word);
                phrase_edges[s].push(WordEdge {
                    start: s,
                    end: end_c,
                    word: ent.word,
                    syl_key: key_str.clone(),
                    base_qlogp: i32::from(ent.qlogp) + bonus,
                    is_partial: false,
                });
            }
            if end_c - s < max_len {
                for &next_id in &cols[end_c] {
                    let mut nseq = seq.clone();
                    nseq.push(next_id);
                    stack.push((end_c + 1, nseq));
                }
            }
        }
        phrase_edges[s].sort_by(|a, b| b.base_qlogp.cmp(&a.base_qlogp));
    }

    let mut beams: Vec<Vec<BeamHyp>> = vec![Vec::new(); m + 1];
    beams[0].push(BeamHyp {
        score: 0,
        text: String::new(),
        last_word: String::new(),
        syl_keys: Vec::new(),
        used_partial: false,
    });
    for pos in 0..m {
        beams[pos].sort_by(|a, b| b.score.cmp(&a.score));
        beams[pos].truncate(10);
        let cur = beams[pos].clone();
        for hyp in &cur {
            for edge in &phrase_edges[pos] {
                let trans = if hyp.last_word.is_empty() {
                    edge.base_qlogp
                } else if let Some(bg) = lm.bigram_qlogp(&hyp.last_word, &edge.word) {
                    i32::from(bg) + edge.base_qlogp / 2 + 160
                } else {
                    edge.base_qlogp + i32::from(lm.backoff_pen_qlogp)
                };
                let mut nkeys = hyp.syl_keys.clone();
                nkeys.push(edge.syl_key.clone());
                beams[edge.end].push(BeamHyp {
                    score: hyp.score + trans,
                    text: format!("{}{}", hyp.text, edge.word),
                    last_word: edge.word.clone(),
                    syl_keys: nkeys,
                    used_partial: false,
                });
            }
        }
    }

    let mut cands = Vec::new();
    let mut seen = HashSet::new();
    if trailing.is_none() && !beams[m].is_empty() {
        beams[m].sort_by(|a, b| b.score.cmp(&a.score));
        for hyp in beams[m].iter().take(4) {
            if seen.insert(hyp.text.clone()) {
                cands.push(Candidate {
                    text: hyp.text.clone(),
                    syl_key: hyp.syl_keys.join("'"),
                    consumed_end: raw.len(),
                    is_full_sentence: true,
                    is_english_or_raw: false,
                    score: hyp.score,
                });
            }
        }
    }
    if m > 0 {
        for edge in phrase_edges[0].iter().take(24) {
            if seen.insert(edge.word.clone()) {
                let byte_consume = if trailing.is_none() && edge.end == m {
                    raw.len()
                } else {
                    (edge.end * 2).min(raw.len())
                };
                cands.push(Candidate {
                    text: edge.word.clone(),
                    syl_key: edge.syl_key.clone(),
                    consumed_end: byte_consume,
                    is_full_sentence: byte_consume >= raw.len() && trailing.is_none(),
                    is_english_or_raw: false,
                    score: edge.base_qlogp,
                });
            }
        }
    }
    if cfg.mixed_english || cands.is_empty() {
        if seen.insert(raw.clone()) {
            cands.push(Candidate {
                text: raw.clone(),
                syl_key: String::new(),
                consumed_end: raw.len(),
                is_full_sentence: true,
                is_english_or_raw: true,
                score: -2500,
            });
        }
    }
    sess.snapshot = ComposingSnapshot {
        preedit_display,
        confirmed_text: sess.committed_prefix.clone(),
        candidates: cands,
    };
}

/// Apply selection of candidate at global index `cand_idx`.
/// Returns `Some(committed_text)` when the entire buffer is finished, or `None` when partial commit advances.
pub fn select_candidate(
    dict: &SpimeDict,
    lm: &SpimeLm,
    userdb: &mut UserDb,
    cfg: &EngineConfig,
    sess: &mut ComposeSession,
    cand_idx: usize,
) -> Option<String> {
    let cand = sess.snapshot.candidates.get(cand_idx)?.clone();
    if !cand.syl_key.is_empty() && !cand.is_english_or_raw {
        userdb.record_commit(&cand.syl_key, &cand.text);
    }

    let consume = cand.consumed_end.min(sess.raw.len());
    let rest_raw = sess.raw[consume..].trim_start_matches('\'').to_string();
    if rest_raw.is_empty() || cand.is_full_sentence {
        let mut final_word = sess.committed_prefix.clone();
        final_word.push_str(&cand.text);
        if !sess.committed_syl_keys.is_empty() && !cand.syl_key.is_empty() {
            let mut all_keys = sess.committed_syl_keys.clone();
            all_keys.push(cand.syl_key);
            userdb.record_commit(&all_keys.join("'"), &final_word);
        }
        userdb.flush();
        sess.clear();
        Some(final_word)
    } else {
        sess.committed_prefix.push_str(&cand.text);
        if !cand.syl_key.is_empty() {
            sess.committed_syl_keys.push(cand.syl_key);
        }
        sess.raw = rest_raw;
        recompute(dict, lm, userdb, cfg, sess);
        None
    }
}

fn join_syls(ids: &[u16]) -> String {
    let mut s = String::with_capacity(ids.len() * 5);
    for (i, &id) in ids.iter().enumerate() {
        if i > 0 {
            s.push('\'');
        }
        s.push_str(syllable_str(id));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_dict_and_lm() -> (SpimeDict, SpimeLm) {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let root_dict = Path::new(manifest).join("../../build/spime/spime.dict");
        let root_lm = Path::new(manifest).join("../../build/spime/spime.lm");
        if root_dict.exists() && root_lm.exists() {
            (
                SpimeDict::open(&root_dict).unwrap(),
                SpimeLm::open(&root_lm).unwrap(),
            )
        } else {
            (SpimeDict::embedded_fallback(), SpimeLm::empty_fallback())
        }
    }

    #[test]
    fn sentence_decoding_and_partial_commit_learn_into_userdb() {
        let (dict, lm) = test_dict_and_lm();
        let mut db = UserDb::in_memory();
        let cfg = EngineConfig::default();
        let mut sess = ComposeSession::default();

        sess.raw = "nihao".to_string();
        recompute(&dict, &lm, &db, &cfg, &mut sess);
        assert_eq!(sess.snapshot.candidates[0].text, "你好");

        sess.raw = "laji".to_string();
        recompute(&dict, &lm, &db, &cfg, &mut sess);
        assert_eq!(sess.snapshot.candidates[0].text, "垃圾");

        sess.raw = "zhongguo".to_string();
        recompute(&dict, &lm, &db, &cfg, &mut sess);
        assert_eq!(sess.snapshot.candidates[0].text, "中国");

        // Partial commit: pick a prefix word of "nihaoshijie"
        sess.raw = "nihaoshijie".to_string();
        recompute(&dict, &lm, &db, &cfg, &mut sess);
        if let Some(nh_idx) = sess
            .snapshot
            .candidates
            .iter()
            .position(|c| c.text == "你好" && !c.is_full_sentence)
        {
            assert_eq!(select_candidate(&dict, &lm, &mut db, &cfg, &mut sess, nh_idx), None);
            assert_eq!(sess.committed_prefix, "你好");
            assert_eq!(sess.raw, "shijie");
            let committed = select_candidate(&dict, &lm, &mut db, &cfg, &mut sess, 0).unwrap();
            assert_eq!(committed, "你好世界");
            // Verify userdb learned the joined phrase!
            assert!(!db.lookup("ni'hao'shi'jie").is_empty());
        }
    }

    #[test]
    fn xiaohe_shuangpin_decodes_nihc() {
        let (dict, lm) = test_dict_and_lm();
        let db = UserDb::in_memory();
        let cfg = EngineConfig {
            scheme: ShuangpinScheme::Xiaohe,
            ..EngineConfig::default()
        };
        let mut sess = ComposeSession {
            raw: "nihc".to_string(),
            ..ComposeSession::default()
        };
        recompute(&dict, &lm, &db, &cfg, &mut sess);
        assert_eq!(sess.snapshot.candidates[0].text, "你好");
    }

    #[test]
    fn full_sentences_jianpin_fuzzy_english_and_delete_work() {
        let (dict, lm) = test_dict_and_lm();
        let mut db = UserDb::in_memory();
        let cfg = EngineConfig::default();
        let mut sess = ComposeSession::default();

        // Full sentences against real compiled dictionary
        if Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/spime/spime.dict")
            .exists()
        {
            sess.raw = "jintiantianqizhenhao".to_string();
            recompute(&dict, &lm, &db, &cfg, &mut sess);
            assert!(
                sess.snapshot
                    .candidates
                    .iter()
                    .take(2)
                    .any(|c| c.text == "今天天气真好"),
                "cands: {:?}",
                sess.snapshot.candidates.iter().take(5).map(|c| &c.text).collect::<Vec<_>>()
            );

            sess.raw = "zg".to_string();
            recompute(&dict, &lm, &db, &cfg, &mut sess);
            assert!(sess.snapshot.candidates.iter().any(|c| c.text == "中国"));

            sess.raw = "linux".to_string();
            recompute(&dict, &lm, &db, &cfg, &mut sess);
            assert!(sess
                .snapshot
                .candidates
                .iter()
                .any(|c| c.text.eq_ignore_ascii_case("linux")));

            // Fuzzy pinyin zh <-> z: "zongguo" -> "中国"
            let fuzzy_cfg = EngineConfig {
                fuzzy: FuzzyPinyin {
                    zh_z: true,
                    ..FuzzyPinyin::default()
                },
                ..EngineConfig::default()
            };
            sess.raw = "zongguo".to_string();
            recompute(&dict, &lm, &db, &fuzzy_cfg, &mut sess);
            assert!(sess.snapshot.candidates.iter().any(|c| c.text == "中国"));

            // Delete candidate hides it from subsequent recomputes
            sess.raw = "nihao".to_string();
            recompute(&dict, &lm, &db, &cfg, &mut sess);
            let top = sess.snapshot.candidates[0].clone();
            db.delete_candidate(&top.syl_key, &top.text);
            recompute(&dict, &lm, &db, &cfg, &mut sess);
            assert_ne!(sess.snapshot.candidates[0].text, top.text);
        }
    }
}
