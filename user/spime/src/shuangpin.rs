/*
 *
 *       user/spime/src/shuangpin.rs
 *       5 Double-Pinyin (Shuangpin) keyboard mapping schemes:
 *       Xiaohe (小鹤), Ziranma (自然码), Microsoft (微软), Sogou (搜狗), and ABC (智能ABC).
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use crate::pinyin::syllable_id;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShuangpinScheme {
    #[default]
    Full,
    Xiaohe,
    Ziranma,
    Microsoft,
    Sogou,
    Abc,
}

impl ShuangpinScheme {
    pub fn from_config_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "xiaohe" | "xh" => Self::Xiaohe,
            "ziranma" | "zrm" => Self::Ziranma,
            "microsoft" | "ms" => Self::Microsoft,
            "sogou" | "sg" => Self::Sogou,
            "abc" => Self::Abc,
            _ => Self::Full,
        }
    }

    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Xiaohe => "xiaohe",
            Self::Ziranma => "ziranma",
            Self::Microsoft => "microsoft",
            Self::Sogou => "sogou",
            Self::Abc => "abc",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Full => "全拼",
            Self::Xiaohe => "小鹤双拼",
            Self::Ziranma => "自然码双拼",
            Self::Microsoft => "微软双拼",
            Self::Sogou => "搜狗双拼",
            Self::Abc => "智能ABC双拼",
        }
    }
}

fn initial_for(scheme: ShuangpinScheme, c: char) -> Option<&'static str> {
    match scheme {
        ShuangpinScheme::Abc => match c {
            'a' => Some("zh"),
            'e' => Some("ch"),
            'v' => Some("sh"),
            'b' | 'p' | 'm' | 'f' | 'd' | 't' | 'n' | 'l' | 'g' | 'k' | 'h' | 'j' | 'q'
            | 'x' | 'r' | 'z' | 'c' | 's' | 'y' | 'w' => Some(match c {
                'b' => "b",
                'p' => "p",
                'm' => "m",
                'f' => "f",
                'd' => "d",
                't' => "t",
                'n' => "n",
                'l' => "l",
                'g' => "g",
                'k' => "k",
                'h' => "h",
                'j' => "j",
                'q' => "q",
                'x' => "x",
                'r' => "r",
                'z' => "z",
                'c' => "c",
                's' => "s",
                'y' => "y",
                'w' => "w",
                _ => unreachable!(),
            }),
            _ => None,
        },
        _ => match c {
            'u' => Some("sh"),
            'i' => Some("ch"),
            'v' => Some("zh"),
            'b' => Some("b"),
            'p' => Some("p"),
            'm' => Some("m"),
            'f' => Some("f"),
            'd' => Some("d"),
            't' => Some("t"),
            'n' => Some("n"),
            'l' => Some("l"),
            'g' => Some("g"),
            'k' => Some("k"),
            'h' => Some("h"),
            'j' => Some("j"),
            'q' => Some("q"),
            'x' => Some("x"),
            'r' => Some("r"),
            'z' => Some("z"),
            'c' => Some("c"),
            's' => Some("s"),
            'y' => Some("y"),
            'w' => Some("w"),
            _ => None,
        },
    }
}

fn finals_for(scheme: ShuangpinScheme, c: char) -> &'static [&'static str] {
    match scheme {
        ShuangpinScheme::Xiaohe => match c {
            'q' => &["iu"],
            'w' => &["ei"],
            'e' => &["e"],
            'r' => &["uan"],
            't' => &["ve", "ue"],
            'y' => &["un"],
            'u' => &["u"],
            'i' => &["i"],
            'o' => &["o", "uo"],
            'p' => &["ie"],
            'a' => &["a"],
            's' => &["ong", "iong"],
            'd' => &["ai"],
            'f' => &["en"],
            'g' => &["eng"],
            'h' => &["ang"],
            'j' => &["an"],
            'k' => &["ing", "uai"],
            'l' => &["iang", "uang"],
            'z' => &["ou"],
            'x' => &["ia", "ua"],
            'c' => &["ao"],
            'v' => &["ui", "v"],
            'b' => &["in"],
            'n' => &["iao"],
            'm' => &["ian"],
            _ => &[],
        },
        ShuangpinScheme::Ziranma => match c {
            'q' => &["iu"],
            'w' => &["ia", "ua"],
            'e' => &["e"],
            'r' => &["uan", "er"],
            't' => &["ve", "ue"],
            'y' => &["uai", "ing"],
            'u' => &["u"],
            'i' => &["i"],
            'o' => &["o", "uo"],
            'p' => &["un"],
            'a' => &["a"],
            's' => &["ong", "iong"],
            'd' => &["iang", "uang"],
            'f' => &["en"],
            'g' => &["eng"],
            'h' => &["ang"],
            'j' => &["an"],
            'k' => &["ao"],
            'l' => &["ai"],
            'z' => &["ei"],
            'x' => &["ie"],
            'c' => &["iao"],
            'v' => &["ui", "v"],
            'b' => &["ou"],
            'n' => &["in"],
            'm' => &["ian"],
            _ => &[],
        },
        ShuangpinScheme::Microsoft => match c {
            'q' => &["iu"],
            'w' => &["ia", "ua"],
            'e' => &["e"],
            'r' => &["uan", "er"],
            't' => &["ue"],
            'y' => &["uai", "v"],
            'u' => &["u"],
            'i' => &["i"],
            'o' => &["o", "uo"],
            'p' => &["un"],
            'a' => &["a"],
            's' => &["ong", "iong"],
            'd' => &["iang", "uang"],
            'f' => &["en"],
            'g' => &["eng"],
            'h' => &["ang"],
            'j' => &["an"],
            'k' => &["ao"],
            'l' => &["ai"],
            ';' => &["ing"],
            'z' => &["ei"],
            'x' => &["ie"],
            'c' => &["iao"],
            'v' => &["ui", "ve"],
            'b' => &["ou"],
            'n' => &["in"],
            'm' => &["ian"],
            _ => &[],
        },
        ShuangpinScheme::Sogou => match c {
            'q' => &["iu"],
            'w' => &["ia", "ua"],
            'e' => &["e"],
            'r' => &["uan", "er"],
            't' => &["ue", "ve"],
            'y' => &["uai", "v"],
            'u' => &["u"],
            'i' => &["i"],
            'o' => &["o", "uo"],
            'p' => &["un"],
            'a' => &["a"],
            's' => &["ong", "iong"],
            'd' => &["iang", "uang"],
            'f' => &["en"],
            'g' => &["eng"],
            'h' => &["ang"],
            'j' => &["an"],
            'k' => &["ao"],
            'l' => &["ai"],
            ';' => &["ing"],
            'z' => &["ei"],
            'x' => &["ie"],
            'c' => &["iao"],
            'v' => &["ui"],
            'b' => &["ou"],
            'n' => &["in"],
            'm' => &["ian"],
            _ => &[],
        },
        ShuangpinScheme::Abc => match c {
            'q' => &["ei"],
            'w' => &["ian"],
            'e' => &["e"],
            'r' => &["iu", "er"],
            't' => &["iang", "uang"],
            'y' => &["ing"],
            'u' => &["u"],
            'i' => &["i"],
            'o' => &["o", "uo"],
            'p' => &["uan"],
            'a' => &["a"],
            's' => &["ong", "iong"],
            'd' => &["ia", "ua"],
            'f' => &["en"],
            'g' => &["eng"],
            'h' => &["ang"],
            'j' => &["an"],
            'k' => &["ao"],
            'l' => &["ai"],
            'z' => &["iao"],
            'x' => &["ie"],
            'c' => &["in", "uai"],
            'v' => &["v", "ve", "ue"],
            'b' => &["ou"],
            'n' => &["un"],
            'm' => &["ui"],
            _ => &[],
        },
        ShuangpinScheme::Full => &[],
    }
}

fn zero_initial_lookup(scheme: ShuangpinScheme, c1: char, c2: char) -> Option<&'static str> {
    match scheme {
        ShuangpinScheme::Xiaohe | ShuangpinScheme::Ziranma => match (c1, c2) {
            ('a', 'a') => Some("a"),
            ('a', 'i') => Some("ai"),
            ('a', 'n') => Some("an"),
            ('a', 'h') => Some("ang"),
            ('a', 'o') => Some("ao"),
            ('e', 'e') => Some("e"),
            ('e', 'i') => Some("ei"),
            ('e', 'n') => Some("en"),
            ('e', 'g') => Some("eng"),
            ('e', 'r') => Some("er"),
            ('o', 'o') => Some("o"),
            ('o', 'u') => Some("ou"),
            _ => None,
        },
        ShuangpinScheme::Microsoft | ShuangpinScheme::Sogou | ShuangpinScheme::Abc => {
            match (c1, c2) {
                ('o', 'a') | ('a', 'a') => Some("a"),
                ('o', 'l') | ('o', 'i') | ('a', 'i') => Some("ai"),
                ('o', 'j') | ('a', 'n') => Some("an"),
                ('o', 'h') | ('a', 'h') => Some("ang"),
                ('o', 'k') | ('a', 'o') => Some("ao"),
                ('o', 'e') | ('e', 'e') => Some("e"),
                ('o', 'z') | ('e', 'i') => Some("ei"),
                ('o', 'f') | ('e', 'n') => Some("en"),
                ('o', 'g') | ('e', 'g') => Some("eng"),
                ('o', 'r') | ('e', 'r') => Some("er"),
                ('o', 'o') => Some("o"),
                ('o', 'b') | ('o', 'u') => Some("ou"),
                _ => None,
            }
        }
        ShuangpinScheme::Full => None,
    }
}

/// Expand one 2-key Shuangpin stroke pair into valid canonical Mandarin `SylId`s.
pub fn expand_pair(scheme: ShuangpinScheme, c1: char, c2: char) -> Vec<u16> {
    if scheme == ShuangpinScheme::Full {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(2);
    if let Some(zero) = zero_initial_lookup(scheme, c1, c2) {
        if let Some(id) = syllable_id(zero) {
            out.push(id);
        }
    }
    if let Some(init) = initial_for(scheme, c1) {
        for &fin in finals_for(scheme, c2) {
            let norm_fin = match (init, fin) {
                ("j" | "q" | "x" | "y", "v") => "u",
                ("j" | "q" | "x" | "y", "ve") => "ue",
                ("l" | "n", "ue") => "ve",
                _ => fin,
            };
            let cand = format!("{init}{norm_fin}");
            if let Some(id) = syllable_id(&cand) {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// Segment an ASCII Shuangpin buffer (`2` keys per syllable + optional `'` splits) into
/// per-position candidate syllable sets `[Vec<SylId>]`, plus optional trailing unpaired key.
pub fn decode_shuangpin_sequence(
    scheme: ShuangpinScheme,
    raw: &str,
) -> (Vec<Vec<u16>>, Option<char>, Vec<String>) {
    let mut syl_cols = Vec::new();
    let mut disp_parts = Vec::new();
    let mut trailing = None;

    for chunk in raw.split('\'') {
        if chunk.is_empty() {
            continue;
        }
        let chars: Vec<char> = chunk.chars().collect();
        let mut i = 0;
        while i + 1 < chars.len() {
            let c1 = chars[i];
            let c2 = chars[i + 1];
            let ids = expand_pair(scheme, c1, c2);
            if ids.is_empty() {
                disp_parts.push(format!("{c1}{c2}"));
                syl_cols.push(vec![]);
            } else {
                disp_parts.push(crate::pinyin::syllable_str(ids[0]).to_string());
                syl_cols.push(ids);
            }
            i += 2;
        }
        if i < chars.len() {
            trailing = Some(chars[i]);
            disp_parts.push(chars[i].to_string());
        }
    }
    (syl_cols, trailing, disp_parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinyin::syllable_str;

    #[test]
    fn xiaohe_and_ziranma_and_ms_decode_correctly() {
        // Xiaohe: "nihc" -> ni hao
        let (cols, rem, parts) = decode_shuangpin_sequence(ShuangpinScheme::Xiaohe, "nihc");
        assert!(rem.is_none());
        assert_eq!(parts, vec!["ni", "hao"]);
        assert!(cols[0].iter().any(|&id| syllable_str(id) == "ni"));
        assert!(cols[1].iter().any(|&id| syllable_str(id) == "hao"));

        // Xiaohe: "vsgo" -> zhong guo
        let (cols2, _, parts2) = decode_shuangpin_sequence(ShuangpinScheme::Xiaohe, "vsgo");
        assert_eq!(parts2, vec!["zhong", "guo"]);
        assert!(cols2[0].iter().any(|&id| syllable_str(id) == "zhong"));
        assert!(cols2[1].iter().any(|&id| syllable_str(id) == "guo"));

        // Ziranma: "nikc" -> ni kao? wait in ZRM k=ao, c=iao -> "nikl" (ni kai), "nihk" -> ni hao? No in ZRM k=ao -> nikc -> "ni kao"? No, "ni hk" -> hao is `h`+`k` = `hk`!
        let (z_cols, _, _) = decode_shuangpin_sequence(ShuangpinScheme::Ziranma, "nihk");
        assert!(z_cols[0].iter().any(|&id| syllable_str(id) == "ni"));
        assert!(z_cols[1].iter().any(|&id| syllable_str(id) == "hao"));

        // Microsoft: "li;b" -> li + ";" (`ing`) -> bing? ("lib;" -> "li b;", "qq" -> qiu, "ni;hk"?) -> Test Microsoft "; = ing": `j;` -> `jing` (`b;` -> `bing`)!
        let ids_ms = expand_pair(ShuangpinScheme::Microsoft, 'j', ';');
        assert!(ids_ms.iter().any(|&id| syllable_str(id) == "jing"));

        // Sogou: `j;` -> `jing`, `ah` -> `a`+`ang`? (`o`+`h` -> `ang`)
        let ids_sg = expand_pair(ShuangpinScheme::Sogou, 'q', ';');
        assert!(ids_sg.iter().any(|&id| syllable_str(id) == "qing"));

        // ABC: `a` = `zh`, `t` = `iang`/`uang`, `v` = `sh`, `h` = `ang` -> "at" = zhuang, "vh" = shang!
        let (abc_cols, _, _) = decode_shuangpin_sequence(ShuangpinScheme::Abc, "atvh");
        assert!(abc_cols[0].iter().any(|&id| syllable_str(id) == "zhuang"));
        assert!(abc_cols[1].iter().any(|&id| syllable_str(id) == "shang"));
    }
}
