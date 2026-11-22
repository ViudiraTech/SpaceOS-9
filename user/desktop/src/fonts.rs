//
//       user/desktop/src/fonts.rs
//       Discover installed font families for the configurable font roles
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//
// SpaceOS has no fontconfig: the renderer loads what it needs directly. This
// module walks /usr/share/fonts, reads each face's family/subfamily names and
// answers "which file provides family X in regular/bold weight?", which is the
// part of fontconfig's job the desktop actually uses.
//

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One face inside a file (TTC files hold several).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceRef {
    pub path: PathBuf,
    pub index: u32,
}

#[derive(Clone, Debug)]
pub struct Family {
    pub name: String,
    pub regular: FaceRef,
    pub bold: Option<FaceRef>,
}

fn be16(data: &[u8], at: usize) -> u16 {
    data.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .unwrap_or(0)
}

fn be32(data: &[u8], at: usize) -> u32 {
    data.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(0)
}

/// Family (name ID 1) and subfamily (name ID 2) of one face.
fn face_names(data: &[u8], face: usize) -> (String, String) {
    let tables = be16(data, face + 4) as usize;
    let mut family = String::new();
    let mut subfamily = String::new();
    for table in 0..tables.min(64) {
        let record = face + 12 + table * 16;
        if data.get(record..record + 4) != Some(b"name") {
            continue;
        }
        let name_table = be32(data, record + 8) as usize;
        let count = be16(data, name_table + 2) as usize;
        let string_offset = be16(data, name_table + 4) as usize;
        for index in 0..count.min(256) {
            let entry = name_table + 6 + index * 12;
            let platform = be16(data, entry);
            let encoding = be16(data, entry + 2);
            let name_id = be16(data, entry + 6);
            if name_id != 1 && name_id != 2 {
                continue;
            }
            let length = be16(data, entry + 8) as usize;
            let offset = be16(data, entry + 10) as usize;
            let start = name_table + string_offset + offset;
            let Some(bytes) = data.get(start..start.saturating_add(length)) else {
                continue;
            };
            let value = if platform == 3 && (encoding == 1 || encoding == 10) {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            } else if platform == 1 && encoding == 0 {
                bytes.iter().map(|&b| b as char).collect()
            } else {
                continue;
            };
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }
            match name_id {
                1 if family.is_empty() => family = value,
                2 if subfamily.is_empty() => subfamily = value,
                _ => {}
            }
        }
        break;
    }
    (family, subfamily)
}

fn face_count(data: &[u8]) -> u32 {
    if data.get(0..4) == Some(b"ttcf") {
        be32(data, 8).clamp(1, 64)
    } else {
        1
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out, budget);
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(ext.as_deref(), Some("ttf") | Some("ttc") | Some("otf")) {
            out.push(path);
            *budget = budget.saturating_sub(1);
            if *budget == 0 {
                return;
            }
        }
    }
}

/// Every family installed under `<sysroot>/usr/share/fonts`, sorted by name.
pub fn scan(sysroot: &Path) -> Vec<Family> {
    let mut files = Vec::new();
    let mut budget = 256usize;
    collect_files(&sysroot.join("usr/share/fonts"), &mut files, &mut budget);
    files.sort();

    let mut families: BTreeMap<String, Family> = BTreeMap::new();
    for path in files {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let faces = face_count(&bytes);
        for index in 0..faces {
            let offset = if bytes.get(0..4) == Some(b"ttcf") {
                be32(&bytes, 12 + index as usize * 4) as usize
            } else {
                0
            };
            let (family, subfamily) = face_names(&bytes, offset);
            if family.is_empty() {
                continue;
            }
            let reference = FaceRef {
                path: path.clone(),
                index,
            };
            let bold = subfamily.to_ascii_lowercase().contains("bold")
                || path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.contains("Bold"));
            let entry = families.entry(family.clone()).or_insert(Family {
                name: family,
                regular: reference.clone(),
                bold: None,
            });
            if bold {
                if entry.bold.is_none() {
                    entry.bold = Some(reference);
                }
            } else if entry.regular.index != reference.index
                || entry.regular.path != reference.path
            {
                // keep the first regular face discovered
            }
        }
    }
    let mut list: Vec<Family> = families.into_values().collect();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    list
}

/// Resolve a configured family name against the installed families.
pub fn resolve(families: &[Family], wanted: &str) -> Option<Family> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return None;
    }
    let lowered = wanted.to_lowercase();
    families
        .iter()
        .find(|f| f.name.to_lowercase() == lowered)
        .or_else(|| {
            families
                .iter()
                .find(|f| f.name.to_lowercase().contains(&lowered))
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_exact_then_substring() {
        let families = vec![
            Family {
                name: "Noto Sans CJK SC".into(),
                regular: FaceRef {
                    path: "/fonts/a.ttc".into(),
                    index: 0,
                },
                bold: None,
            },
            Family {
                name: "DejaVu Sans".into(),
                regular: FaceRef {
                    path: "/fonts/b.ttf".into(),
                    index: 0,
                },
                bold: None,
            },
        ];
        assert_eq!(resolve(&families, "dejavu sans").unwrap().name, "DejaVu Sans");
        assert_eq!(resolve(&families, "CJK SC").unwrap().name, "Noto Sans CJK SC");
        assert!(resolve(&families, "  ").is_none());
        assert!(resolve(&families, "Comic").is_none());
    }
}
