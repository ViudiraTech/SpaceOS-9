use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub title: String,
    pub url: String,
    pub created: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub timestamp: u64,
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookieEntry {
    pub domain: String,
    pub path: String,
    pub name: String,
    pub value: String,
    pub expires: u64,
}

pub struct BrowserStorage {
    base_dir: PathBuf,
    pub bookmarks: Vec<Bookmark>,
    pub history: Vec<HistoryEntry>,
    pub cookies: Vec<CookieEntry>,
    /// `localStorage`: origin -> ordered (key, value)
    pub local_kv: BTreeMap<String, Vec<(String, String)>>,
    /// `sessionStorage`: origin -> ordered (key, value)
    pub session_kv: BTreeMap<String, Vec<(String, String)>>,
}

pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn origin_for_url(raw: &str) -> String {
    if let Ok(u) = Url::parse(raw) {
        match u.scheme() {
            "space" => return format!("space://{}", u.host_str().unwrap_or("local")),
            "file" => return String::from("file://local"),
            "about" | "data" => return String::from("null"),
            scheme => {
                if let Some(host) = u.host_str() {
                    return match u.port() {
                        Some(p) => format!("{scheme}://{host}:{p}"),
                        None => format!("{scheme}://{host}"),
                    };
                }
            }
        }
    }
    String::from("null")
}

fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn resolve_data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            let p = PathBuf::from(home).join(".local/share/space-browser");
            if fs::create_dir_all(&p).is_ok() {
                return p;
            }
        }
    }
    let fallback = PathBuf::from("/tmp/space-browser");
    let _ = fs::create_dir_all(&fallback);
    fallback
}

impl BrowserStorage {
    pub fn load() -> Self {
        Self::load_from_dir(resolve_data_dir())
    }

    pub fn load_from_dir(base_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&base_dir);
        let mut s = Self {
            base_dir,
            bookmarks: Vec::new(),
            history: Vec::new(),
            cookies: Vec::new(),
            local_kv: BTreeMap::new(),
            session_kv: BTreeMap::new(),
        };
        s.load_bookmarks();
        s.load_history();
        s.load_cookies();
        s.load_local_kv();
        s
    }

    fn load_bookmarks(&mut self) {
        let path = self.base_dir.join("bookmarks.tsv");
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 2 {
                    self.bookmarks.push(Bookmark {
                        title: unescape_field(parts[0]),
                        url: unescape_field(parts[1]),
                        created: parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(0),
                    });
                }
            }
        }
        if self.bookmarks.is_empty() {
            let now = now_unix_secs();
            self.bookmarks = vec![
                Bookmark {
                    title: String::from("Space 主页"),
                    url: String::from("space://home"),
                    created: now,
                },
                Bookmark {
                    title: String::from("JS + DOM 实验室"),
                    url: String::from("space://js-lab"),
                    created: now,
                },
                Bookmark {
                    title: String::from("关于 Space 浏览器"),
                    url: String::from("space://about"),
                    created: now,
                },
            ];
            self.save_bookmarks();
        }
    }

    pub fn save_bookmarks(&self) {
        let mut buf = String::new();
        for b in &self.bookmarks {
            buf.push_str(&format!(
                "{}\t{}\t{}\n",
                escape_field(&b.title),
                escape_field(&b.url),
                b.created
            ));
        }
        let _ = fs::write(self.base_dir.join("bookmarks.tsv"), buf);
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.bookmarks.iter().any(|b| b.url == url)
    }

    pub fn toggle_bookmark(&mut self, title: &str, url: &str) -> bool {
        if let Some(idx) = self.bookmarks.iter().position(|b| b.url == url) {
            self.bookmarks.remove(idx);
            self.save_bookmarks();
            false
        } else {
            let clean_title = if title.trim().is_empty() { url } else { title.trim() };
            self.bookmarks.push(Bookmark {
                title: clean_title.to_string(),
                url: url.to_string(),
                created: now_unix_secs(),
            });
            self.save_bookmarks();
            true
        }
    }

    fn load_history(&mut self) {
        let path = self.base_dir.join("history.tsv");
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 3 {
                    self.history.push(HistoryEntry {
                        timestamp: parts[0].parse().unwrap_or(0),
                        title: unescape_field(parts[1]),
                        url: unescape_field(parts[2]),
                    });
                }
            }
        }
    }

    pub fn save_history(&self) {
        let mut buf = String::new();
        for h in &self.history {
            buf.push_str(&format!(
                "{}\t{}\t{}\n",
                h.timestamp,
                escape_field(&h.title),
                escape_field(&h.url)
            ));
        }
        let _ = fs::write(self.base_dir.join("history.tsv"), buf);
    }

    pub fn record_history(&mut self, url: &str, title: &str) {
        if url.is_empty() || url == "about:blank" || url.starts_with("data:") {
            return;
        }
        if let Some(pos) = self.history.iter().position(|h| h.url == url) {
            self.history.remove(pos);
        }
        let clean_title = if title.trim().is_empty() { url } else { title.trim() };
        self.history.insert(
            0,
            HistoryEntry {
                timestamp: now_unix_secs(),
                title: clean_title.to_string(),
                url: url.to_string(),
            },
        );
        self.history.truncate(250);
        self.save_history();
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.save_history();
    }

    pub fn suggest_urls(&self, query: &str, limit: usize) -> Vec<(String, String, bool)> {
        let q = query.trim().to_lowercase();
        let mut out = Vec::new();
        for b in &self.bookmarks {
            if q.is_empty() || b.url.to_lowercase().contains(&q) || b.title.to_lowercase().contains(&q) {
                out.push((b.title.clone(), b.url.clone(), true));
                if out.len() >= limit {
                    return out;
                }
            }
        }
        for h in &self.history {
            if out.iter().any(|(_, u, _)| u == &h.url) {
                continue;
            }
            if q.is_empty() || h.url.to_lowercase().contains(&q) || h.title.to_lowercase().contains(&q) {
                out.push((h.title.clone(), h.url.clone(), false));
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    fn load_local_kv(&mut self) {
        let path = self.base_dir.join("localstorage.tsv");
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 3 {
                    let origin = unescape_field(parts[0]);
                    let k = unescape_field(parts[1]);
                    let v = unescape_field(parts[2]);
                    let list = self.local_kv.entry(origin).or_default();
                    if let Some(existing) = list.iter_mut().find(|(ek, _)| ek == &k) {
                        existing.1 = v;
                    } else {
                        list.push((k, v));
                    }
                }
            }
        }
    }

    fn save_local_kv(&self) {
        let mut buf = String::new();
        for (origin, list) in &self.local_kv {
            for (k, v) in list {
                buf.push_str(&format!(
                    "{}\t{}\t{}\n",
                    escape_field(origin),
                    escape_field(k),
                    escape_field(v)
                ));
            }
        }
        let _ = fs::write(self.base_dir.join("localstorage.tsv"), buf);
    }

    pub fn kv_get(&self, is_session: bool, origin: &str, key: &str) -> Option<String> {
        let map = if is_session { &self.session_kv } else { &self.local_kv };
        map.get(origin)?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    pub fn kv_set(&mut self, is_session: bool, origin: &str, key: &str, val: &str) {
        let map = if is_session { &mut self.session_kv } else { &mut self.local_kv };
        let list = map.entry(origin.to_string()).or_default();
        if let Some(existing) = list.iter_mut().find(|(k, _)| k == key) {
            existing.1 = val.to_string();
        } else {
            list.push((key.to_string(), val.to_string()));
        }
        if !is_session {
            self.save_local_kv();
        }
    }

    pub fn kv_remove(&mut self, is_session: bool, origin: &str, key: &str) {
        let map = if is_session { &mut self.session_kv } else { &mut self.local_kv };
        if let Some(list) = map.get_mut(origin) {
            list.retain(|(k, _)| k != key);
        }
        if !is_session {
            self.save_local_kv();
        }
    }

    pub fn kv_clear(&mut self, is_session: bool, origin: &str) {
        let map = if is_session { &mut self.session_kv } else { &mut self.local_kv };
        map.remove(origin);
        if !is_session {
            self.save_local_kv();
        }
    }

    pub fn kv_len(&self, is_session: bool, origin: &str) -> usize {
        let map = if is_session { &self.session_kv } else { &self.local_kv };
        map.get(origin).map_or(0, Vec::len)
    }

    pub fn kv_key(&self, is_session: bool, origin: &str, idx: usize) -> Option<String> {
        let map = if is_session { &self.session_kv } else { &self.local_kv };
        map.get(origin).and_then(|v| v.get(idx)).map(|(k, _)| k.clone())
    }

    fn load_cookies(&mut self) {
        let path = self.base_dir.join("cookies.tsv");
        let now = now_unix_secs();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 5 {
                    let expires = parts[4].parse().unwrap_or(0);
                    if expires != 0 && expires < now {
                        continue;
                    }
                    self.cookies.push(CookieEntry {
                        domain: unescape_field(parts[0]),
                        path: unescape_field(parts[1]),
                        name: unescape_field(parts[2]),
                        value: unescape_field(parts[3]),
                        expires,
                    });
                }
            }
        }
    }

    fn save_cookies(&self) {
        let mut buf = String::new();
        for c in &self.cookies {
            buf.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                escape_field(&c.domain),
                escape_field(&c.path),
                escape_field(&c.name),
                escape_field(&c.value),
                c.expires
            ));
        }
        let _ = fs::write(self.base_dir.join("cookies.tsv"), buf);
    }

    pub fn set_cookie_from_str(&mut self, url_str: &str, header_val: &str) {
        let Ok(u) = Url::parse(url_str) else { return };
        let default_domain = u.host_str().unwrap_or("local").to_lowercase();
        let mut domain = default_domain.clone();
        let mut path = String::from("/");
        let mut expires = 0u64;
        let mut parts = header_val.split(';');
        let Some(first) = parts.next() else { return };
        let Some((raw_k, raw_v)) = first.split_once('=') else { return };
        let name = raw_k.trim().to_string();
        if name.is_empty() {
            return;
        }
        let value = raw_v.trim().to_string();
        for attr in parts {
            let trimmed = attr.trim();
            if let Some((ak, av)) = trimmed.split_once('=') {
                match ak.trim().to_ascii_lowercase().as_str() {
                    "domain" => {
                        let d = av.trim().trim_start_matches('.').to_lowercase();
                        if !d.is_empty() && (default_domain == d || default_domain.ends_with(&format!(".{d}"))) {
                            domain = d;
                        }
                    }
                    "path" => {
                        let p = av.trim();
                        if !p.is_empty() {
                            path = p.to_string();
                        }
                    }
                    "max-age" => {
                        if let Ok(sec) = av.trim().parse::<i64>() {
                            expires = if sec <= 0 { 1 } else { now_unix_secs().saturating_add(sec as u64) };
                        }
                    }
                    _ => {}
                }
            }
        }

        self.cookies
            .retain(|c| !(c.domain == domain && c.path == path && c.name == name));
        if expires != 1 {
            self.cookies.push(CookieEntry {
                domain,
                path,
                name,
                value,
                expires,
            });
        }
        self.save_cookies();
    }

    pub fn cookies_for_url(&self, url_str: &str) -> String {
        let Ok(u) = Url::parse(url_str) else { return String::new() };
        let host = u.host_str().unwrap_or("local").to_lowercase();
        let path = u.path();
        let now = now_unix_secs();
        let mut pairs = Vec::new();
        for c in &self.cookies {
            if c.expires != 0 && c.expires < now {
                continue;
            }
            let domain_ok = host == c.domain || host.ends_with(&format!(".{}", c.domain));
            let path_ok = path.starts_with(&c.path);
            if domain_ok && path_ok {
                pairs.push(format!("{}={}", c.name, c.value));
            }
        }
        pairs.join("; ")
    }
}
