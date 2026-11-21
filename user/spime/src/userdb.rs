/*
 *
 *       user/spime/src/userdb.rs
 *       Persistent user lexicon, dynamic frequency tuning, and hidden-word
 *       suppression saved atomically to `~/.local/share/spime/user.db`.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserEntry {
    pub word: String,
    pub count: u32,
    pub seq: u64,
}

/// Persistent user learning & candidate management database.
#[derive(Debug)]
pub struct UserDb {
    path: Option<PathBuf>,
    next_seq: u64,
    entries: HashMap<String, Vec<UserEntry>>,
    hidden: HashSet<(String, String)>,
    dirty: bool,
}

impl Default for UserDb {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl UserDb {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            next_seq: 1,
            entries: HashMap::new(),
            hidden: HashSet::new(),
            dirty: false,
        }
    }

    pub fn default_path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("SPIME_USER_DB").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(p));
        }
        if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(data).join("spime/user.db"));
        }
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(|h| PathBuf::from(h).join(".local/share/spime/user.db"))
    }

    pub fn open_default() -> Self {
        match Self::default_path() {
            Some(path) => Self::open_path(path),
            None => Self::in_memory(),
        }
    }

    pub fn open_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut db = Self {
            path: Some(path.clone()),
            next_seq: 1,
            entries: HashMap::new(),
            hidden: HashSet::new(),
            dirty: false,
        };
        if let Ok(text) = fs::read_to_string(&path) {
            db.load_str(&text);
        }
        db
    }

    fn load_str(&mut self, text: &str) {
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let Some(kind) = parts.next() else {
                continue;
            };
            match kind {
                "U" => {
                    let (Some(key), Some(word), Some(cnt_s), Some(seq_s)) =
                        (parts.next(), parts.next(), parts.next(), parts.next())
                    else {
                        continue;
                    };
                    let Ok(count) = cnt_s.parse::<u32>() else {
                        continue;
                    };
                    let Ok(seq) = seq_s.parse::<u64>() else {
                        continue;
                    };
                    self.next_seq = self.next_seq.max(seq + 1);
                    self.entries
                        .entry(key.to_string())
                        .or_default()
                        .push(UserEntry {
                            word: word.to_string(),
                            count: count.max(1),
                            seq,
                        });
                }
                "D" => {
                    if let (Some(key), Some(word)) = (parts.next(), parts.next()) {
                        self.hidden.insert((key.to_string(), word.to_string()));
                    }
                }
                _ => {}
            }
        }
    }

    pub fn record_commit(&mut self, syl_key: &str, word: &str) {
        let key = syl_key.trim();
        let w = word.trim();
        if key.is_empty() || w.is_empty() || key.len() > 96 || w.len() > 64 {
            return;
        }
        self.hidden.remove(&(key.to_string(), w.to_string()));
        let seq = self.next_seq;
        self.next_seq += 1;
        let list = self.entries.entry(key.to_string()).or_default();
        if let Some(existing) = list.iter_mut().find(|e| e.word == w) {
            existing.count = existing.count.saturating_add(1);
            existing.seq = seq;
        } else {
            list.push(UserEntry {
                word: w.to_string(),
                count: 1,
                seq,
            });
            if list.len() > 32 {
                list.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| b.seq.cmp(&a.seq)));
                list.truncate(32);
            }
        }
        self.dirty = true;
    }

    pub fn delete_candidate(&mut self, syl_key: &str, word: &str) {
        let key = syl_key.trim();
        let w = word.trim();
        if key.is_empty() || w.is_empty() {
            return;
        }
        if let Some(list) = self.entries.get_mut(key) {
            list.retain(|e| e.word != w);
            if list.is_empty() {
                self.entries.remove(key);
            }
        }
        self.hidden.insert((key.to_string(), w.to_string()));
        self.dirty = true;
    }

    pub fn is_hidden(&self, syl_key: &str, word: &str) -> bool {
        self.hidden
            .contains(&(syl_key.to_string(), word.to_string()))
    }

    /// Return learned user entries matching exact syllable sequence key, highest-boost first.
    pub fn lookup(&self, syl_key: &str) -> Vec<&UserEntry> {
        let Some(list) = self.entries.get(syl_key) else {
            return Vec::new();
        };
        let mut refs: Vec<&UserEntry> = list.iter().collect();
        refs.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| b.seq.cmp(&a.seq)));
        refs
    }

    /// Compute frequency boost in quantized `log10 * 100` units above base dictionary weight.
    pub fn boost_qlogp(&self, syl_key: &str, word: &str) -> i32 {
        let Some(list) = self.entries.get(syl_key) else {
            return 0;
        };
        let Some(entry) = list.iter().find(|e| e.word == word) else {
            return 0;
        };
        let base = 120 + (entry.count.min(200) as f32).ln_1p() as i32 * 45;
        let recency = if entry.seq + 20 >= self.next_seq {
            60
        } else {
            0
        };
        base + recency
    }

    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(ref path) = self.path else {
            self.dirty = false;
            return;
        };
        if let Err(err) = self.save_to_path(path) {
            eprintln!("spime: save userdb `{}` failed: {err}", path.display());
            return;
        }
        self.dirty = false;
    }

    fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = String::from("# spime user lexicon v1\n");
        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        for key in keys {
            if let Some(list) = self.entries.get(key) {
                for e in list {
                    out.push_str(&format!("U\t{}\t{}\t{}\t{}\n", key, e.word, e.count, e.seq));
                }
            }
        }
        let mut hidden: Vec<&(String, String)> = self.hidden.iter().collect();
        hidden.sort();
        for (k, w) in hidden {
            out.push_str(&format!("D\t{}\t{}\n", k, w));
        }
        let tmp = path.with_extension("db.tmp");
        fs::write(&tmp, out.as_bytes())?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userdb_learn_promote_delete_and_roundtrip() {
        let tmp = std::env::temp_dir().join("spime-userdb-test.db");
        let _ = fs::remove_file(&tmp);
        {
            let mut db = UserDb::open_path(&tmp);
            db.record_commit("ni'hao", "拟好");
            db.record_commit("ni'hao", "拟好");
            assert!(db.boost_qlogp("ni'hao", "拟好") > 120);
            db.delete_candidate("ni'hao", "泥好");
            assert!(db.is_hidden("ni'hao", "泥好"));
            db.flush();
        }
        {
            let db2 = UserDb::open_path(&tmp);
            let res = db2.lookup("ni'hao");
            assert_eq!(res.len(), 1);
            assert_eq!(res[0].word, "拟好");
            assert_eq!(res[0].count, 2);
            assert!(db2.is_hidden("ni'hao", "泥好"));
        }
        let _ = fs::remove_file(&tmp);
    }
}
