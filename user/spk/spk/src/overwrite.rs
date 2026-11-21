//
//       user/spk/spk/src/overwrite.rs
//       Absolute path glob matching for --overwrite
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use crate::error::{Result, SpkError};

#[derive(Clone, Debug, Default)]
pub struct Overwrite {
    patterns: Vec<String>,
}

impl Overwrite {
    pub fn parse(patterns: &[String]) -> Result<Self> {
        for pattern in patterns {
            validate(pattern)?;
        }
        Ok(Self {
            patterns: patterns.to_vec(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn matches(&self, path: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| glob_match(pattern.as_bytes(), path.as_bytes()))
    }
}

fn validate(pattern: &str) -> Result<()> {
    if !pattern.starts_with('/') {
        return Err(SpkError::Usage(format!(
            "invalid --overwrite glob '{pattern}': must be an absolute path"
        )));
    }
    if pattern.contains('\0') || pattern.chars().any(char::is_control) {
        return Err(SpkError::Usage(format!(
            "invalid --overwrite glob '{pattern}': control character"
        )));
    }
    for component in pattern.split('/') {
        if component == ".." || component == "." {
            return Err(SpkError::Usage(format!(
                "invalid --overwrite glob '{pattern}': '..' paths are not allowed"
            )));
        }
    }
    let bytes = pattern.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                if index + 1 >= bytes.len() {
                    return Err(SpkError::Usage(format!(
                        "invalid --overwrite glob '{pattern}': trailing escape"
                    )));
                }
                index += 2;
                continue;
            }
            b'[' => {
                index += 1;
                if index < bytes.len() && (bytes[index] == b'!' || bytes[index] == b'^') {
                    index += 1;
                }
                if index < bytes.len() && bytes[index] == b']' {
                    index += 1;
                }
                let mut closed = false;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                        continue;
                    }
                    if bytes[index] == b']' {
                        closed = true;
                        index += 1;
                        break;
                    }
                    index += 1;
                }
                if !closed {
                    return Err(SpkError::Usage(format!(
                        "invalid --overwrite glob '{pattern}': unclosed '['"
                    )));
                }
                continue;
            }
            _ => index += 1,
        }
    }
    Ok(())
}

fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let mut memo = vec![vec![None; text.len() + 1]; pattern.len() + 1];
    matches(pattern, text, 0, 0, &mut memo)
}

fn matches(
    pattern: &[u8],
    text: &[u8],
    mut p: usize,
    mut t: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(value) = memo[p][t] {
        return value;
    }
    let result = loop {
        if p == pattern.len() {
            break t == text.len();
        }
        match pattern[p] {
            b'*' => {
                while p + 1 < pattern.len() && pattern[p + 1] == b'*' {
                    p += 1;
                }
                if p + 1 == pattern.len() {
                    break true;
                }
                let mut found = false;
                for next in t..=text.len() {
                    if matches(pattern, text, p + 1, next, memo) {
                        found = true;
                        break;
                    }
                }
                break found;
            }
            b'?' => {
                if t == text.len() {
                    break false;
                }
                p += 1;
                t += 1;
            }
            b'\\' if p + 1 < pattern.len() => {
                if t == text.len() || pattern[p + 1] != text[t] {
                    break false;
                }
                p += 2;
                t += 1;
            }
            b'[' => {
                if t == text.len() {
                    break false;
                }
                let (ok, next) = class_match(pattern, p + 1, text[t]);
                if !ok {
                    break false;
                }
                p = next;
                t += 1;
            }
            byte => {
                if t == text.len() || byte != text[t] {
                    break false;
                }
                p += 1;
                t += 1;
            }
        }
    };
    memo[p][t] = Some(result);
    result
}

fn class_match(pattern: &[u8], mut p: usize, byte: u8) -> (bool, usize) {
    let mut negate = false;
    if p < pattern.len() && (pattern[p] == b'!' || pattern[p] == b'^') {
        negate = true;
        p += 1;
    }
    let mut matched = false;
    let mut first = true;
    while p < pattern.len() {
        if pattern[p] == b']' && !first {
            return (matched ^ negate, p + 1);
        }
        first = false;
        let mut start = pattern[p];
        if start == b'\\' && p + 1 < pattern.len() {
            p += 1;
            start = pattern[p];
        }
        p += 1;
        let mut end = start;
        if p + 1 < pattern.len() && pattern[p] == b'-' && pattern[p + 1] != b']' {
            p += 1;
            end = pattern[p];
            if end == b'\\' && p + 1 < pattern.len() {
                p += 1;
                end = pattern[p];
            }
            p += 1;
        }
        if (start..=end).contains(&byte) {
            matched = true;
        }
    }
    (false, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_globs() {
        let set = Overwrite::parse(&[
            "/usr/bin/hello".into(),
            "/usr/share/*".into(),
            "/opt/[ab]?x".into(),
        ])
        .unwrap();
        assert!(set.matches("/usr/bin/hello"));
        assert!(set.matches("/usr/share/docs/readme"));
        assert!(set.matches("/opt/a9x"));
        assert!(!set.matches("/opt/c9x"));
        assert!(!set.matches("usr/bin/hello"));
    }

    #[test]
    fn invalid_globs_rejected() {
        for pattern in ["usr/bin/hello", "/usr/../etc/passwd", "/opt/[abc"] {
            assert!(Overwrite::parse(&[pattern.into()]).is_err(), "{pattern}");
        }
    }
}
