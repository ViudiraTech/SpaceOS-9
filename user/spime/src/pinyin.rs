/*
 *
 *       user/spime/src/pinyin.rs
 *       Canonical Mandarin syllable table, fuzzy pinyin expansion, typo correction,
 *       apostrophe handling, and DAG syllable segmentation.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FuzzyPinyin {
    pub zh_z: bool,
    pub ch_c: bool,
    pub sh_s: bool,
    pub l_n: bool,
    pub f_h: bool,
    pub r_l: bool,
    pub an_ang: bool,
    pub en_eng: bool,
    pub in_ing: bool,
    pub ian_iang: bool,
    pub uan_uang: bool,
}

impl FuzzyPinyin {
    pub fn any(&self) -> bool {
        self.zh_z
            || self.ch_c
            || self.sh_s
            || self.l_n
            || self.f_h
            || self.r_l
            || self.an_ang
            || self.en_eng
            || self.in_ing
            || self.ian_iang
            || self.uan_uang
    }
}

/// Sorted list of all valid canonical Mandarin syllables (identical to `build_spime_dict.py`).
pub const MANDARIN_SYLLABLES: &[&str] = &[
    "a", "ai", "an", "ang", "ao", "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi",
    "bian", "biao", "bie", "bin", "bing", "bo", "bu", "ca", "cai", "can", "cang", "cao", "ce",
    "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong",
    "chou", "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci", "cong", "cou",
    "cu", "cuan", "cui", "cun", "cuo", "da", "dai", "dan", "dang", "dao", "de", "dei", "den",
    "deng", "di", "dia", "dian", "diao", "die", "ding", "diu", "dong", "dou", "du", "duan", "dui",
    "dun", "duo", "e", "ei", "en", "eng", "er", "fa", "fan", "fang", "fei", "fen", "feng", "fiao",
    "fo", "fou", "fu", "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou",
    "gu", "gua", "guai", "guan", "guang", "gui", "gun", "guo", "ha", "hai", "han", "hang", "hao",
    "he", "hei", "hen", "heng", "hong", "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun",
    "huo", "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju",
    "juan", "jue", "jun", "ka", "kai", "kan", "kang", "kao", "ke", "kei", "ken", "keng", "kong",
    "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo", "la", "lai", "lan", "lang",
    "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin", "ling", "liu",
    "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve", "ma", "mai", "man", "mang",
    "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu", "mo",
    "mou", "mu", "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni", "nian",
    "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou", "nu", "nuan", "nuo", "nv", "nve",
    "o", "ou", "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao",
    "pie", "pin", "ping", "po", "pou", "pu", "qi", "qia", "qian", "qiang", "qiao", "qie", "qin",
    "qing", "qiong", "qiu", "qu", "quan", "que", "qun", "ran", "rang", "rao", "re", "ren", "reng",
    "ri", "rong", "rou", "ru", "rua", "ruan", "rui", "run", "ruo", "sa", "sai", "san", "sang",
    "sao", "se", "sen", "seng", "sha", "shai", "shan", "shang", "shao", "she", "shei", "shen",
    "sheng", "shi", "shou", "shu", "shua", "shuai", "shuan", "shuang", "shui", "shun", "shuo",
    "si", "song", "sou", "su", "suan", "sui", "sun", "suo", "ta", "tai", "tan", "tang", "tao",
    "te", "tei", "teng", "ti", "tian", "tiao", "tie", "ting", "tong", "tou", "tu", "tuan", "tui",
    "tun", "tuo", "wa", "wai", "wan", "wang", "wei", "wen", "weng", "wo", "wu", "xi", "xia",
    "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu", "xu", "xuan", "xue", "xun",
    "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong", "you", "yu", "yuan",
    "yue", "yun", "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng", "zha", "zhai",
    "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi", "zhong", "zhou", "zhu",
    "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo", "zi", "zong", "zou", "zu", "zuan",
    "zui", "zun", "zuo",
];

#[inline]
pub fn syllable_id(syl: &str) -> Option<u16> {
    MANDARIN_SYLLABLES.binary_search(&syl).ok().map(|i| i as u16)
}

#[inline]
pub fn syllable_str(id: u16) -> &'static str {
    MANDARIN_SYLLABLES
        .get(usize::from(id))
        .copied()
        .unwrap_or("")
}

/// Normalize user ASCII syllable (`jv`->`ju`, `lue`->`lve`, common typo swap like `ign`->`ing`).
/// Returns `(canonical_id, is_typo_corrected)`.
pub fn normalize_syllable_with_typo(raw: &str) -> Option<(u16, bool)> {
    if let Some(id) = syllable_id(raw) {
        return Some((id, false));
    }
    let aliased = match raw {
        "lue" => Some("lve"),
        "nue" => Some("nve"),
        "jv" => Some("ju"),
        "jve" => Some("jue"),
        "jvan" => Some("juan"),
        "jvn" => Some("jun"),
        "qv" => Some("qu"),
        "qve" => Some("que"),
        "qvan" => Some("quan"),
        "qvn" => Some("qun"),
        "xv" => Some("xu"),
        "xve" => Some("xue"),
        "xvan" => Some("xuan"),
        "xvn" => Some("xun"),
        "yv" => Some("yu"),
        "yve" => Some("yue"),
        "yvan" => Some("yuan"),
        "yvn" => Some("yun"),
        _ => None,
    };
    if let Some(id) = aliased.and_then(syllable_id) {
        return Some((id, false));
    }

    // Fast-typing adjacent-letter transposition corrections (`-gn` <-> `-ng`, `-ina` -> `-ian`).
    if let Some(fixed) = fix_common_typo(raw) {
        if let Some(id) = syllable_id(&fixed) {
            return Some((id, true));
        }
    }
    None
}

fn fix_common_typo(raw: &str) -> Option<String> {
    if raw.len() < 3 || raw.len() > 7 {
        return None;
    }
    let replacements: [(&str, &str); 10] = [
        ("iang", "iagn"),
        ("uang", "uagn"),
        ("iong", "iogn"),
        ("ang", "agn"),
        ("eng", "egn"),
        ("ing", "ign"),
        ("ong", "ogn"),
        ("ian", "ina"),
        ("uan", "una"),
        ("uai", "uia"),
    ];
    for &(good, bad) in &replacements {
        if let Some(stem) = raw.strip_suffix(bad) {
            let mut s = String::with_capacity(stem.len() + good.len());
            s.push_str(stem);
            s.push_str(good);
            return Some(s);
        }
    }
    None
}

/// Expand `base_syl_id` into all allowed syllable IDs under `fuzzy` configuration, exact ID first.
pub fn expand_fuzzy_variants(base_id: u16, fuzzy: &FuzzyPinyin) -> Vec<(u16, bool)> {
    let mut out = vec![(base_id, false)];
    if !fuzzy.any() {
        return out;
    }
    let s = syllable_str(base_id);
    let mut init_alts = Vec::with_capacity(2);
    init_alts.push(s);
    let swap_pair = |prefix_a: &str, prefix_b: &str, alts: &mut Vec<&'static str>, buf: &mut Vec<String>| {
        if let Some(rest) = s.strip_prefix(prefix_a) {
            buf.push(format!("{prefix_b}{rest}"));
        } else if let Some(rest) = s.strip_prefix(prefix_b) {
            buf.push(format!("{prefix_a}{rest}"));
        }
        let _ = alts;
    };
    let mut owned_inits: Vec<String> = Vec::with_capacity(4);
    owned_inits.push(s.to_string());
    if fuzzy.zh_z {
        swap_pair("zh", "z", &mut init_alts, &mut owned_inits);
    }
    if fuzzy.ch_c {
        swap_pair("ch", "c", &mut init_alts, &mut owned_inits);
    }
    if fuzzy.sh_s {
        swap_pair("sh", "s", &mut init_alts, &mut owned_inits);
    }
    if fuzzy.l_n {
        swap_pair("l", "n", &mut init_alts, &mut owned_inits);
    }
    if fuzzy.f_h {
        swap_pair("f", "h", &mut init_alts, &mut owned_inits);
    }
    if fuzzy.r_l {
        swap_pair("r", "l", &mut init_alts, &mut owned_inits);
    }

    let mut all_forms = owned_inits.clone();
    for form in &owned_inits {
        let mut try_end = |a: &str, b: &str| {
            if let Some(stem) = form.strip_suffix(a) {
                all_forms.push(format!("{stem}{b}"));
            } else if let Some(stem) = form.strip_suffix(b) {
                all_forms.push(format!("{stem}{a}"));
            }
        };
        if fuzzy.ian_iang {
            try_end("ian", "iang");
        }
        if fuzzy.uan_uang {
            try_end("uan", "uang");
        }
        if fuzzy.an_ang {
            try_end("an", "ang");
        }
        if fuzzy.en_eng {
            try_end("en", "eng");
        }
        if fuzzy.in_ing {
            try_end("in", "ing");
        }
    }

    for cand in all_forms {
        if let Some(id) = syllable_id(&cand) {
            if !out.iter().any(|&(existing, _)| existing == id) {
                out.push((id, true));
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    Syllable { syl_id: u16, typo: bool },
    Initial(char),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentSpan {
    pub start: usize,
    pub end: usize,
    pub kind: SegmentKind,
}

/// Complete syllable + initial DAG built from raw ASCII pinyin text (`'a'..='z'` + `'\''`).
#[derive(Clone, Debug, Default)]
pub struct SyllableLattice {
    pub len: usize,
    /// Outgoing edges grouped by `start` byte index (`0..=len`).
    pub edges_at: Vec<Vec<SegmentSpan>>,
}

/// Build full syllable + jianpin initial DAG honoring manual `'` boundaries.
pub fn build_lattice(raw: &str) -> SyllableLattice {
    let bytes = raw.as_bytes();
    let n = bytes.len();
    let mut edges_at = vec![Vec::new(); n + 1];

    for i in 0..n {
        if bytes[i] == b'\'' {
            continue;
        }
        let max_end = (i + 7).min(n);
        let mut has_full_from_i = false;
        for j in (i + 1)..=max_end {
            if bytes[j - 1] == b'\'' {
                break;
            }
            let sub = &raw[i..j];
            if let Some((syl_id, typo)) = normalize_syllable_with_typo(sub) {
                edges_at[i].push(SegmentSpan {
                    start: i,
                    end: j,
                    kind: SegmentKind::Syllable { syl_id, typo },
                });
                has_full_from_i = true;
            }
        }
        // Single lowercase consonant or syllable prefix can also participate as a Jianpin initial.
        let ch = bytes[i] as char;
        if ch.is_ascii_lowercase() && (i + 1 == n || bytes[i + 1] != b'\'' || !has_full_from_i) {
            edges_at[i].push(SegmentSpan {
                start: i,
                end: i + 1,
                kind: SegmentKind::Initial(ch),
            });
        }
    }

    SyllableLattice { len: n, edges_at }
}

/// Compute highest-likelihood segmentation of `raw` for preedit visual separation (`"ni'hao"`).
pub fn best_segmentation(raw: &str) -> Vec<SegmentSpan> {
    let bytes = raw.as_bytes();
    let n = bytes.len();
    if n == 0 {
        return Vec::new();
    }
    let lattice = build_lattice(raw);
    let mut dp_cost = vec![i32::MAX / 4; n + 1];
    let mut next_edge: Vec<Option<SegmentSpan>> = vec![None; n];
    dp_cost[n] = 0;

    for i in (0..n).rev() {
        if bytes[i] == b'\'' {
            dp_cost[i] = dp_cost[i + 1];
            continue;
        }
        let mut best_c = i32::MAX / 4;
        let mut best_e = None;
        for edge in &lattice.edges_at[i] {
            let step_cost = match edge.kind {
                SegmentKind::Syllable { typo: false, .. } => {
                    // Prefer fewer, longer canonical syllables.
                    100 - (edge.end - edge.start) as i32 * 4
                }
                SegmentKind::Syllable { typo: true, .. } => 140,
                SegmentKind::Initial(_) => 450,
            };
            let cand = step_cost + dp_cost[edge.end];
            if cand < best_c {
                best_c = cand;
                best_e = Some(edge.clone());
            }
        }
        dp_cost[i] = best_c;
        next_edge[i] = best_e;
    }

    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < n {
        if bytes[pos] == b'\'' {
            pos += 1;
            continue;
        }
        if let Some(edge) = next_edge[pos].clone() {
            pos = edge.end;
            out.push(edge);
        } else {
            out.push(SegmentSpan {
                start: pos,
                end: pos + 1,
                kind: SegmentKind::Initial(bytes[pos] as char),
            });
            pos += 1;
        }
    }
    out
}

/// Format preedit string nicely with apostrophe separators (e.g. `"ni'hao"`, `"xi'an"`).
pub fn format_preedit(raw: &str, spans: &[SegmentSpan]) -> String {
    if spans.is_empty() {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len() + spans.len());
    for (idx, sp) in spans.iter().enumerate() {
        if idx > 0 {
            out.push('\'');
        }
        out.push_str(&raw[sp.start..sp.end]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_sentence_and_apostrophe_cleanly() {
        let s = best_segmentation("woshiyigechengxuyuan");
        let joined: Vec<&str> = s.iter().map(|e| &"woshiyigechengxuyuan"[e.start..e.end]).collect();
        assert_eq!(joined, vec!["wo", "shi", "yi", "ge", "cheng", "xu", "yuan"]);

        let xian = best_segmentation("xi'an");
        let xian_parts: Vec<&str> = xian.iter().map(|e| &"xi'an"[e.start..e.end]).collect();
        assert_eq!(xian_parts, vec!["xi", "an"]);
    }

    #[test]
    fn fuzzy_and_typo_normalization_work() {
        assert_eq!(
            normalize_syllable_with_typo("zhogn").map(|(id, t)| (syllable_str(id), t)),
            Some(("zhong", true))
        );
        assert_eq!(
            normalize_syllable_with_typo("lue").map(|(id, _)| syllable_str(id)),
            Some("lve")
        );
        let fuzzy = FuzzyPinyin {
            zh_z: true,
            in_ing: true,
            ..FuzzyPinyin::default()
        };
        let zhong = syllable_id("zhong").unwrap();
        let vars: Vec<&str> = expand_fuzzy_variants(zhong, &fuzzy)
            .into_iter()
            .map(|(id, _)| syllable_str(id))
            .collect();
        assert!(vars.contains(&"zhong") && vars.contains(&"zong"));

        let xin = syllable_id("xin").unwrap();
        let vars2: Vec<&str> = expand_fuzzy_variants(xin, &fuzzy)
            .into_iter()
            .map(|(id, _)| syllable_str(id))
            .collect();
        assert!(vars2.contains(&"xing"));
    }
}
