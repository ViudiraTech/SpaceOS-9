//
//       user/spk/spk-core/src/version.rs
//       Package version parsing and pacman/alpm-compatible comparison
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::cmp::Ordering;
use std::fmt;

use crate::{Error, Result};

/// `[epoch:]pkgver[-pkgrel]`. `pkgrel` is optional because dependency
/// constraints may omit it; the manifest and index always carry one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub epoch: u64,
    pub pkgver: String,
    pub pkgrel: Option<u64>,
}

impl Version {
    pub fn new(epoch: u64, pkgver: impl Into<String>, pkgrel: u64) -> Self {
        Self {
            epoch,
            pkgver: pkgver.into(),
            pkgrel: Some(pkgrel),
        }
    }

    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() {
            return Err(Error::Version("empty version".into()));
        }
        let (epoch, rest) = match input.split_once(':') {
            Some((epoch, rest)) => (parse_number(epoch, "epoch")?, rest),
            None => (0, input),
        };
        let (pkgver, pkgrel) = match rest.rsplit_once('-') {
            Some((ver, rel)) => (ver, Some(parse_number(rel, "release")?)),
            None => (rest, None),
        };
        validate_pkgver(pkgver)?;
        Ok(Self {
            epoch,
            pkgver: pkgver.to_string(),
            pkgrel,
        })
    }

    pub fn cmp_alpm(&self, other: &Self) -> Ordering {
        let epoch = self.epoch.cmp(&other.epoch);
        if epoch != Ordering::Equal {
            return epoch;
        }
        let ver = rpmvercmp(&self.pkgver, &other.pkgver);
        if ver != Ordering::Equal {
            return ver;
        }
        match (self.pkgrel, other.pkgrel) {
            (Some(a), Some(b)) => a.cmp(&b),
            // pacman only compares the release when both sides carry one.
            _ => Ordering::Equal,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.epoch != 0 {
            write!(f, "{}:", self.epoch)?;
        }
        f.write_str(&self.pkgver)?;
        if let Some(rel) = self.pkgrel {
            write!(f, "-{}", rel)?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_alpm(other)
    }
}

pub fn validate_pkgver(version: &str) -> Result<()> {
    if version.is_empty() {
        return Err(Error::Version("empty package version".into()));
    }
    if version.len() > 128 {
        return Err(Error::Version(format!(
            "package version too long: {version}"
        )));
    }
    if !version
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'~'))
    {
        return Err(Error::Version(format!(
            "package version contains invalid characters: {version}"
        )));
    }
    Ok(())
}

fn parse_number(text: &str, field: &str) -> Result<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Version(format!("invalid {field}: '{text}'")));
    }
    text.parse::<u64>()
        .map_err(|_| Error::Version(format!("{field} out of range: '{text}'")))
}

/// Byte-for-byte port of pacman's `rpmvercmp` (lib/libalpm/version.c).
///
/// Highlights that a naive "split on dots" implementation gets wrong:
/// separator lengths are compared, numeric segments outrank alpha segments,
/// leading zeros are ignored, and a trailing alpha segment sorts *older* than
/// the bare version (`1.0a < 1.0`).
pub fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut one, mut two) = (0usize, 0usize);
    let (mut prev1, mut prev2) = (0usize, 0usize);

    while one < a.len() && two < b.len() {
        while one < a.len() && !a[one].is_ascii_alphanumeric() {
            one += 1;
        }
        while two < b.len() && !b[two].is_ascii_alphanumeric() {
            two += 1;
        }
        if one >= a.len() || two >= b.len() {
            break;
        }
        if one - prev1 != two - prev2 {
            return if one - prev1 < two - prev2 {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        prev1 = one;
        prev2 = two;

        let isnum = a[prev1].is_ascii_digit();
        if isnum {
            while prev1 < a.len() && a[prev1].is_ascii_digit() {
                prev1 += 1;
            }
            while prev2 < b.len() && b[prev2].is_ascii_digit() {
                prev2 += 1;
            }
        } else {
            while prev1 < a.len() && a[prev1].is_ascii_alphabetic() {
                prev1 += 1;
            }
            while prev2 < b.len() && b[prev2].is_ascii_alphabetic() {
                prev2 += 1;
            }
        }

        // The other side has no segment of this type; numeric beats alpha.
        if prev2 == two {
            return if isnum {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }

        let seg_a = &a[one..prev1];
        let seg_b = &b[two..prev2];
        let ord = if isnum {
            let (sa, sb) = (strip_zeros(seg_a), strip_zeros(seg_b));
            sa.len().cmp(&sb.len()).then_with(|| sa.cmp(sb))
        } else {
            seg_a.cmp(seg_b)
        };
        if ord != Ordering::Equal {
            return ord;
        }

        one = prev1;
        two = prev2;
    }

    if one >= a.len() && two >= b.len() {
        return Ordering::Equal;
    }
    // A remaining alpha segment is a pre-release and sorts older; any other
    // remaining content (digits, separators) sorts newer.
    let one_alpha = one < a.len() && a[one].is_ascii_alphabetic();
    let two_alpha = two < b.len() && b[two].is_ascii_alphabetic();
    if (one >= a.len() && !two_alpha) || one_alpha {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn strip_zeros(segment: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < segment.len() && segment[start] == b'0' {
        start += 1;
    }
    &segment[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn alpm_ordering_table() {
        let cases = [
            ("1.0", "1.1", Ordering::Less),
            ("1.1", "1.0", Ordering::Greater),
            ("1.0", "1.0", Ordering::Equal),
            ("1.0", "1.0.1", Ordering::Less),
            ("1.0.1", "1.0", Ordering::Greater),
            ("1.0", "1.0a", Ordering::Greater),
            ("1.0a", "1.0", Ordering::Less),
            ("1.0a", "1.0b", Ordering::Less),
            ("1.0rc1", "1.0", Ordering::Less),
            ("1.0alpha", "1.0beta", Ordering::Less),
            ("1.10", "1.9", Ordering::Greater),
            ("1.001", "1.1", Ordering::Equal),
            ("1.01", "1.1", Ordering::Equal),
            ("1..0", "1.0", Ordering::Greater),
            ("1.0.", "1.0", Ordering::Greater),
            ("1.0", "1.0.", Ordering::Less),
            ("1.0+git1", "1.0", Ordering::Greater),
            ("1:0.1", "9.9", Ordering::Greater),
            ("1.0-1", "1.0-2", Ordering::Less),
            ("1.0-10", "1.0-9", Ordering::Greater),
            ("1.0", "1.0-0", Ordering::Equal),
            ("1.0-1", "1.0", Ordering::Equal),
            ("2:1.0-1", "2:1.0-1", Ordering::Equal),
        ];
        for (a, b, expected) in cases {
            assert_eq!(
                v(a).cmp(&v(b)),
                expected,
                "expected {a} vs {b} to be {expected:?}"
            );
        }
    }

    #[test]
    fn display_roundtrip() {
        assert_eq!(v("1.2.3-1").to_string(), "1.2.3-1");
        assert_eq!(v("2:1.2.3-4").to_string(), "2:1.2.3-4");
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
    }

    #[test]
    fn rejects_bad_versions() {
        for bad in ["", "1.0-", "-1", "1.0-abc", "1.0 2", "1.0/2", "1.0-1-2"] {
            assert!(Version::parse(bad).is_err(), "should reject {bad:?}");
        }
    }
}
