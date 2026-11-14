use base64::Engine as _;
use brotli::Decompressor;
use flate2::read::{GzDecoder, ZlibDecoder};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};
use spui_ui::draw::Sprite;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ffi::CString;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, ToSocketAddrs};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

use crate::pages;
use crate::storage::BrowserStorage;

const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 SpaceBrowser/1.0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchKind {
    Navigation,
    Subresource,
}

#[derive(Clone, Debug)]
pub struct FetchResponse {
    pub status: u16,
    pub final_url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub error: Option<String>,
}

pub struct AsyncFetchRequest {
    pub tab_id: u64,
    pub nav_gen: u64,
    pub req_id: u64,
    pub is_navigation: bool,
    pub is_image: bool,
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub struct CompletedAsyncFetch {
    pub tab_id: u64,
    pub nav_gen: u64,
    pub req_id: u64,
    pub is_image: bool,
    pub response: FetchResponse,
}

#[derive(Default)]
struct FetchQueues {
    nav: VecDeque<AsyncFetchRequest>,
    sub: VecDeque<AsyncFetchRequest>,
}

pub struct FetchPool {
    tx_fd: RawFd,
    rx_fd: RawFd,
    completed: Arc<Mutex<Vec<CompletedAsyncFetch>>>,
    queues: Arc<(Mutex<FetchQueues>, Condvar)>,
    tab_gens: Arc<Mutex<HashMap<u64, u64>>>,
    storage: Arc<Mutex<BrowserStorage>>,
    dark_mode: Arc<Mutex<(bool, bool)>>,
}

impl Drop for FetchPool {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.rx_fd);
            libc::close(self.tx_fd);
        }
    }
}

impl FetchPool {
    pub fn new(storage: Arc<Mutex<BrowserStorage>>) -> Self {
        let mut fds = [0i32; 2];
        unsafe {
            if libc::pipe(fds.as_mut_ptr()) == 0 {
                libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK);
                libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK);
            }
        }
        let rx_fd = fds[0];
        let tx_fd = fds[1];
        let completed = Arc::new(Mutex::new(Vec::new()));
        let queues = Arc::new((Mutex::new(FetchQueues::default()), Condvar::new()));
        let tab_gens = Arc::new(Mutex::new(HashMap::<u64, u64>::new()));
        let dark_mode = Arc::new(Mutex::new((false, false)));

        // Dedicated navigation workers (never starved by slow images/XHR) + general workers.
        for worker_idx in 0..6 {
            let nav_only = worker_idx < 2;
            let q = Arc::clone(&queues);
            let gens = Arc::clone(&tab_gens);
            let comp = Arc::clone(&completed);
            let st = Arc::clone(&storage);
            let dm = Arc::clone(&dark_mode);
            thread::spawn(move || loop {
                let job = {
                    let (lock, cv) = &*q;
                    let mut guard = lock.lock().unwrap();
                    loop {
                        let popped = if nav_only {
                            guard.nav.pop_front()
                        } else {
                            guard.nav.pop_front().or_else(|| guard.sub.pop_front())
                        };
                        if let Some(req) = popped {
                            if Self::is_req_stale(&gens, &req) {
                                continue;
                            }
                            break req;
                        }
                        guard = cv.wait(guard).unwrap();
                    }
                };
                let (dark, win98) = *dm.lock().unwrap();
                let kind = if job.is_navigation {
                    FetchKind::Navigation
                } else {
                    FetchKind::Subresource
                };
                let response = sync_fetch_with_kind(
                    &job.method,
                    &job.url,
                    &job.headers,
                    &job.body,
                    Some(&st),
                    dark,
                    win98,
                    kind,
                );
                if !job.is_navigation && Self::is_req_stale(&gens, &job) {
                    continue;
                }
                comp.lock().unwrap().push(CompletedAsyncFetch {
                    tab_id: job.tab_id,
                    nav_gen: job.nav_gen,
                    req_id: job.req_id,
                    is_image: job.is_image,
                    response,
                });
                let byte = [1u8; 1];
                unsafe {
                    libc::write(tx_fd, byte.as_ptr() as *const libc::c_void, 1);
                }
            });
        }

        Self {
            tx_fd,
            rx_fd,
            completed,
            queues,
            tab_gens,
            storage,
            dark_mode,
        }
    }

    fn is_req_stale(tab_gens: &Arc<Mutex<HashMap<u64, u64>>>, req: &AsyncFetchRequest) -> bool {
        if req.nav_gen == 0 {
            return false;
        }
        let guard = tab_gens.lock().unwrap();
        let cur = guard.get(&req.tab_id).copied().unwrap_or(0);
        cur > req.nav_gen
    }

    pub fn begin_navigation(&self, tab_id: u64) -> u64 {
        let next_gen = {
            let mut gens = self.tab_gens.lock().unwrap();
            let entry = gens.entry(tab_id).or_insert(0);
            *entry += 1;
            *entry
        };
        let (lock, _cv) = &*self.queues;
        let mut q = lock.lock().unwrap();
        q.nav.retain(|r| r.tab_id != tab_id || r.nav_gen >= next_gen);
        q.sub.retain(|r| r.tab_id != tab_id || r.nav_gen >= next_gen);
        next_gen
    }

    pub fn cancel_tab(&self, tab_id: u64) {
        let next_gen = {
            let mut gens = self.tab_gens.lock().unwrap();
            let entry = gens.entry(tab_id).or_insert(0);
            *entry += 1;
            *entry
        };
        let (lock, _cv) = &*self.queues;
        let mut q = lock.lock().unwrap();
        q.nav.retain(|r| r.tab_id != tab_id || r.nav_gen >= next_gen);
        q.sub.retain(|r| r.tab_id != tab_id || r.nav_gen >= next_gen);
    }

    pub fn set_theme_flags(&self, dark_mode: bool, is_win98: bool) {
        if let Ok(mut g) = self.dark_mode.lock() {
            *g = (dark_mode, is_win98);
        }
    }

    pub fn wakeup_fd(&self) -> RawFd {
        self.rx_fd
    }

    pub fn enqueue(&self, req: AsyncFetchRequest) {
        if Self::is_req_stale(&self.tab_gens, &req) {
            return;
        }
        let (lock, cv) = &*self.queues;
        let mut guard = lock.lock().unwrap();
        if req.is_navigation {
            guard.nav.push_back(req);
        } else {
            if req.method.eq_ignore_ascii_case("GET")
                && req.body.is_empty()
                && guard.sub.iter().any(|existing| {
                    existing.tab_id == req.tab_id
                        && existing.nav_gen == req.nav_gen
                        && existing.is_image == req.is_image
                        && existing.url == req.url
                })
            {
                return;
            }
            guard.sub.push_back(req);
        }
        cv.notify_all();
    }

    pub fn drain_completed(&self) -> Vec<CompletedAsyncFetch> {
        let mut buf = [0u8; 64];
        unsafe {
            while libc::read(self.rx_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) > 0 {}
        }
        let mut guard = self.completed.lock().unwrap();
        std::mem::take(&mut *guard)
    }

    pub fn storage(&self) -> &Arc<Mutex<BrowserStorage>> {
        &self.storage
    }
}

pub fn parse_headers_json(json: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let s = json.trim();
    if !s.starts_with('{') || !s.ends_with('}') {
        return map;
    }
    let inner = s[1..s.len() - 1].trim();
    if inner.is_empty() {
        return map;
    }
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let Some((k, next_i)) = read_json_str(inner, i) else {
            break;
        };
        i = next_i;
        while i < bytes.len() && bytes[i] != b':' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if let Some((v, after_v)) = read_json_str(inner, i) {
            map.insert(k, v);
            i = after_v;
        } else {
            break;
        }
    }
    map
}

fn read_json_str(s: &str, mut i: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if i >= bytes.len() || bytes[i] != b'"' {
        return None;
    }
    i += 1;
    let mut out = String::new();
    while i < bytes.len() {
        if bytes[i] == b'"' {
            return Some((out, i + 1));
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
            match bytes[i] {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                other => out.push(other as char),
            }
            i += 1;
        } else {
            let ch = s[i..].chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    None
}

pub fn serialize_headers_json(headers: &BTreeMap<String, String>) -> String {
    let mut out = String::from("{");
    for (idx, (k, v)) in headers.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push('"');
        for ch in k.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                _ => out.push(ch),
            }
        }
        out.push_str("\":\"");
        for ch in v.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
        }
        out.push('"');
    }
    out.push('}');
    out
}

pub fn resolve_url_ref(base: &str, rel: &str) -> String {
    let r = rel.trim();
    if r.is_empty() {
        return base.to_string();
    }
    if r.contains("://") || r.starts_with("data:") || r.starts_with("about:") {
        return r.to_string();
    }
    if let Ok(b) = Url::parse(base) {
        if let Ok(joined) = b.join(r) {
            return joined.to_string();
        }
    }
    r.to_string()
}

pub fn normalize_omnibox_input(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::from("space://home");
    }
    if s.starts_with("space://")
        || s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("file://")
        || s.starts_with("about:")
        || s.starts_with("data:")
    {
        return s.to_string();
    }
    if s.starts_with('/') && PathBuf::from(s).exists() {
        return format!("file://{s}");
    }
    let first_seg = s.split('/').next().unwrap_or("");
    if !s.contains(' ') && (first_seg.contains('.') || first_seg.starts_with("localhost")) {
        return format!("https://{s}");
    }
    let mut encoded = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            encoded.push(b as char);
        } else if b == b' ' {
            encoded.push('+');
        } else {
            encoded.push_str(&format!("%{:02X}", b));
        }
    }
    format!("space://search?q={encoded}")
}

pub fn sync_fetch(
    method: &str,
    url_str: &str,
    req_headers: &BTreeMap<String, String>,
    req_body: &[u8],
    storage: Option<&Arc<Mutex<BrowserStorage>>>,
    dark_mode: bool,
    is_win98: bool,
) -> FetchResponse {
    sync_fetch_with_kind(
        method,
        url_str,
        req_headers,
        req_body,
        storage,
        dark_mode,
        is_win98,
        FetchKind::Navigation,
    )
}

pub fn sync_fetch_with_kind(
    method: &str,
    url_str: &str,
    req_headers: &BTreeMap<String, String>,
    req_body: &[u8],
    storage: Option<&Arc<Mutex<BrowserStorage>>>,
    dark_mode: bool,
    is_win98: bool,
    kind: FetchKind,
) -> FetchResponse {
    if url_str.starts_with("about:") || url_str.starts_with("space://") || url_str.is_empty() {
        let guard = storage.and_then(|s| s.lock().ok());
        let page = pages::serve_builtin(url_str, guard.as_deref(), dark_mode, is_win98);
        let mut hdrs = BTreeMap::new();
        hdrs.insert(String::from("content-type"), page.content_type.to_string());
        return FetchResponse {
            status: page.status,
            final_url: if url_str.is_empty() {
                String::from("about:blank")
            } else {
                url_str.to_string()
            },
            headers: hdrs,
            body: page.body.into_bytes(),
            error: None,
        };
    }

    if let Some(rest) = url_str.strip_prefix("data:") {
        return decode_data_url(rest, url_str);
    }

    if url_str.starts_with("file://") {
        return fetch_file_url(url_str);
    }

    if url_str.starts_with("http://") || url_str.starts_with("https://") {
        let cacheable = kind == FetchKind::Subresource
            && method.eq_ignore_ascii_case("GET")
            && req_body.is_empty()
            && req_headers.is_empty();
        if cacheable {
            if let Some(cached) = subresource_cache_get(url_str) {
                return cached;
            }
        }
        let resp = fetch_http_loop(method, url_str, req_headers, req_body, storage, kind);
        if cacheable && resp.error.is_none() && resp.status == 200 {
            subresource_cache_put(url_str, &resp);
        }
        return resp;
    }

    FetchResponse {
        status: 400,
        final_url: url_str.to_string(),
        headers: BTreeMap::new(),
        body: Vec::new(),
        error: Some(format!("不支持的协议: {url_str}")),
    }
}

pub fn prefetch_subresources(
    urls: &[String],
    storage: Option<&Arc<Mutex<BrowserStorage>>>,
    dark_mode: bool,
    is_win98: bool,
) {
    let mut todo = Vec::new();
    let mut seen = HashSet::new();
    for raw in urls {
        let u = raw.trim();
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            continue;
        }
        if !seen.insert(u.to_string()) || subresource_cache_get(u).is_some() {
            continue;
        }
        todo.push(u.to_string());
    }
    if todo.is_empty() {
        return;
    }
    if todo.len() == 1 {
        let _ = sync_fetch_with_kind(
            "GET",
            &todo[0],
            &BTreeMap::new(),
            &[],
            storage,
            dark_mode,
            is_win98,
            FetchKind::Subresource,
        );
        return;
    }
    let idx = AtomicUsize::new(0);
    let workers = todo.len().min(4);
    let empty_hdrs = BTreeMap::new();
    thread::scope(|s| {
        for _ in 0..workers {
            let todo_ref = &todo;
            let idx_ref = &idx;
            let hdrs_ref = &empty_hdrs;
            s.spawn(move || loop {
                let i = idx_ref.fetch_add(1, Ordering::Relaxed);
                if i >= todo_ref.len() {
                    break;
                }
                let _ = sync_fetch_with_kind(
                    "GET",
                    &todo_ref[i],
                    hdrs_ref,
                    &[],
                    storage,
                    dark_mode,
                    is_win98,
                    FetchKind::Subresource,
                );
            });
        }
    });
}

fn decode_data_url(rest: &str, full_url: &str) -> FetchResponse {
    let Some((meta, payload)) = rest.split_once(',') else {
        return FetchResponse {
            status: 400,
            final_url: full_url.to_string(),
            headers: BTreeMap::new(),
            body: Vec::new(),
            error: Some(String::from("无效 data: URL")),
        };
    };
    let is_b64 = meta.split(';').any(|p| p.eq_ignore_ascii_case("base64"));
    let mime = meta
        .split(';')
        .find(|p| !p.is_empty() && !p.eq_ignore_ascii_case("base64"))
        .unwrap_or("text/plain");
    let mut hdrs = BTreeMap::new();
    hdrs.insert(String::from("content-type"), mime.to_string());
    let body = if is_b64 {
        let cleaned: String = payload.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(cleaned.as_bytes())
            .unwrap_or_default()
    } else {
        percent_decode_bytes(payload.as_bytes())
    };
    FetchResponse {
        status: 200,
        final_url: full_url.to_string(),
        headers: hdrs,
        body,
        error: None,
    }
}

fn percent_decode_bytes(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            if let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(&input[i + 1..i + 3]), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

fn fetch_file_url(url_str: &str) -> FetchResponse {
    let path_part = Url::parse(url_str)
        .ok()
        .map(|u| String::from_utf8_lossy(&percent_decode_bytes(u.path().as_bytes())).into_owned())
        .unwrap_or_else(|| url_str.trim_start_matches("file://").to_string());
    match fs::read(&path_part) {
        Ok(bytes) => {
            let ext = PathBuf::from(&path_part)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let mime = match ext.as_str() {
                "html" | "htm" => "text/html; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "js" | "mjs" => "application/javascript; charset=utf-8",
                "json" => "application/json; charset=utf-8",
                "svg" => "image/svg+xml",
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                _ => "text/plain; charset=utf-8",
            };
            let mut hdrs = BTreeMap::new();
            hdrs.insert(String::from("content-type"), mime.to_string());
            FetchResponse {
                status: 200,
                final_url: url_str.to_string(),
                headers: hdrs,
                body: bytes,
                error: None,
            }
        }
        Err(e) => FetchResponse {
            status: 404,
            final_url: url_str.to_string(),
            headers: BTreeMap::new(),
            body: format!(
                "<!doctype html><html><body><h2>无法读取本地文件</h2><p>{}: {}</p></body></html>",
                path_part, e
            )
            .into_bytes(),
            error: Some(e.to_string()),
        },
    }
}

struct CachedSubresource {
    stored_at: Instant,
    response: FetchResponse,
}

#[derive(Default)]
struct SubresourceCache {
    entries: HashMap<String, CachedSubresource>,
    order: VecDeque<String>,
    total_bytes: usize,
}

static SUBRESOURCE_CACHE: OnceLock<Mutex<SubresourceCache>> = OnceLock::new();
static DNS_CACHE: OnceLock<Mutex<HashMap<(String, u16), (Instant, Vec<SocketAddr>)>>> =
    OnceLock::new();
static NEG_HOST_CACHE: OnceLock<Mutex<HashMap<(String, u16), (Instant, String)>>> = OnceLock::new();
static TLS_CLIENT_CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();

const SUBRESOURCE_TTL: Duration = Duration::from_secs(120);
const SUBRESOURCE_MAX_ENTRIES: usize = 64;
const SUBRESOURCE_MAX_BYTES: usize = 8 * 1024 * 1024;
const DNS_CACHE_TTL: Duration = Duration::from_secs(60);
const NEG_HOST_TTL: Duration = Duration::from_secs(15);

fn tls_client_config() -> &'static Arc<rustls::ClientConfig> {
    TLS_CLIENT_CONFIG.get_or_init(|| {
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();
        Arc::new(cfg)
    })
}

fn subresource_cache_get(url: &str) -> Option<FetchResponse> {
    let lock = SUBRESOURCE_CACHE.get_or_init(|| Mutex::new(SubresourceCache::default()));
    let mut cache = lock.lock().ok()?;
    let expired = match cache.entries.get(url) {
        Some(entry) => entry.stored_at.elapsed() > SUBRESOURCE_TTL,
        None => return None,
    };
    if expired {
        if let Some(old) = cache.entries.remove(url) {
            cache.total_bytes = cache.total_bytes.saturating_sub(old.response.body.len());
        }
        cache.order.retain(|k| k != url);
        return None;
    }
    cache.entries.get(url).map(|e| e.response.clone())
}

fn subresource_cache_put(url: &str, resp: &FetchResponse) {
    if resp.body.len() > 1024 * 1024 {
        return;
    }
    if resp
        .headers
        .get("cache-control")
        .is_some_and(|v| v.to_ascii_lowercase().contains("no-store"))
    {
        return;
    }
    let lock = SUBRESOURCE_CACHE.get_or_init(|| Mutex::new(SubresourceCache::default()));
    let Ok(mut cache) = lock.lock() else {
        return;
    };
    if let Some(prev) = cache.entries.remove(url) {
        cache.total_bytes = cache.total_bytes.saturating_sub(prev.response.body.len());
        cache.order.retain(|k| k != url);
    }
    while cache.entries.len() >= SUBRESOURCE_MAX_ENTRIES
        || cache.total_bytes + resp.body.len() > SUBRESOURCE_MAX_BYTES
    {
        let Some(oldest_key) = cache.order.pop_front() else {
            break;
        };
        if let Some(removed) = cache.entries.remove(&oldest_key) {
            cache.total_bytes = cache
                .total_bytes
                .saturating_sub(removed.response.body.len());
        }
    }
    cache.total_bytes += resp.body.len();
    cache.order.push_back(url.to_string());
    cache.entries.insert(
        url.to_string(),
        CachedSubresource {
            stored_at: Instant::now(),
            response: resp.clone(),
        },
    );
}

fn neg_host_get(host: &str, port: u16) -> Option<String> {
    let lock = NEG_HOST_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = lock.lock().ok()?;
    let key = (host.to_ascii_lowercase(), port);
    if let Some((at, msg)) = map.get(&key) {
        if at.elapsed() <= NEG_HOST_TTL {
            return Some(msg.clone());
        }
        map.remove(&key);
    }
    None
}

fn neg_host_put(host: &str, port: u16, msg: &str) {
    let lock = NEG_HOST_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = lock.lock() {
        if map.len() > 128 {
            map.retain(|_, (at, _)| at.elapsed() <= NEG_HOST_TTL);
        }
        map.insert(
            (host.to_ascii_lowercase(), port),
            (Instant::now(), msg.to_string()),
        );
    }
}

fn lookup_ipv4_addrs(host: &str, port: u16) -> Vec<SocketAddr> {
    let Ok(c_host) = CString::new(host) else {
        return Vec::new();
    };
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_INET;
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let rc = unsafe { libc::getaddrinfo(c_host.as_ptr(), std::ptr::null(), &hints, &mut res) };
    if rc != 0 || res.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        let ai = unsafe { &*cur };
        if ai.ai_family == libc::AF_INET
            && !ai.ai_addr.is_null()
            && ai.ai_addrlen as usize >= std::mem::size_of::<libc::sockaddr_in>()
        {
            let sa = unsafe { &*(ai.ai_addr as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(sa.sin_addr.s_addr.to_ne_bytes());
            let sock = SocketAddr::V4(SocketAddrV4::new(ip, port));
            if !out.contains(&sock) {
                out.push(sock);
            }
        }
        cur = ai.ai_next;
    }
    unsafe { libc::freeaddrinfo(res) };
    out
}

fn resolve_host_addrs(host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let key = (host.to_ascii_lowercase(), port);
    let cache_lock = DNS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = cache_lock.lock() {
        if let Some((at, cached)) = map.get(&key) {
            if at.elapsed() <= DNS_CACHE_TTL && !cached.is_empty() {
                return Ok(cached.clone());
            }
            map.remove(&key);
        }
    }

    // Prefer AF_INET first so SpaceOS/musl does not stall sending AAAA queries.
    let mut addrs = lookup_ipv4_addrs(host, port);
    if addrs.is_empty() {
        addrs = (host, port).to_socket_addrs()?.collect();
    }
    if !addrs.is_empty() {
        if let Ok(mut map) = cache_lock.lock() {
            if map.len() > 128 {
                map.retain(|_, (at, _)| at.elapsed() <= DNS_CACHE_TTL);
            }
            map.insert(key, (Instant::now(), addrs.clone()));
        }
    }
    Ok(addrs)
}

fn fetch_http_loop(
    method: &str,
    start_url: &str,
    req_headers: &BTreeMap<String, String>,
    req_body: &[u8],
    storage: Option<&Arc<Mutex<BrowserStorage>>>,
    kind: FetchKind,
) -> FetchResponse {
    let mut current_url = start_url.to_string();
    let mut cur_method = method.to_string();
    let mut cur_body = req_body.to_vec();

    for _hop in 0..6 {
        let resp =
            fetch_http_once(&cur_method, &current_url, req_headers, &cur_body, storage, kind);
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(loc) = resp.headers.get("location") {
                current_url = resolve_url_ref(&current_url, loc);
                if resp.status == 303 || (cur_method == "POST" && matches!(resp.status, 301 | 302)) {
                    cur_method = String::from("GET");
                    cur_body.clear();
                }
                continue;
            }
        }
        return resp;
    }

    FetchResponse {
        status: 310,
        final_url: current_url,
        headers: BTreeMap::new(),
        body: Vec::new(),
        error: Some(String::from("重定向次数过多")),
    }
}

fn fetch_http_once(
    method: &str,
    url_str: &str,
    req_headers: &BTreeMap<String, String>,
    req_body: &[u8],
    storage: Option<&Arc<Mutex<BrowserStorage>>>,
    kind: FetchKind,
) -> FetchResponse {
    let Ok(parsed) = Url::parse(url_str) else {
        return FetchResponse {
            status: 400,
            final_url: url_str.to_string(),
            headers: BTreeMap::new(),
            body: Vec::new(),
            error: Some(format!("无效 URL: {url_str}")),
        };
    };
    let is_tls = parsed.scheme() == "https";
    let Some(host) = parsed.host_str() else {
        return FetchResponse {
            status: 400,
            final_url: url_str.to_string(),
            headers: BTreeMap::new(),
            body: Vec::new(),
            error: Some(String::from("缺少主机名")),
        };
    };
    let port = parsed.port_or_known_default().unwrap_or(if is_tls { 443 } else { 80 });
    let host_header = if parsed.port().is_some() {
        format!("{host}:{port}")
    } else {
        host.to_string()
    };
    let path_q = match parsed.query() {
        Some(q) => format!("{}?{q}", parsed.path()),
        None => parsed.path().to_string(),
    };
    let path_q = if path_q.is_empty() { String::from("/") } else { path_q };

    let mut req_buf = Vec::new();
    let _ = write!(
        req_buf,
        "{method} {path_q} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: {DEFAULT_USER_AGENT}\r\nAccept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8\r\nAccept-Encoding: gzip, deflate, br\r\nConnection: close\r\n"
    );
    if let Some(st) = storage.and_then(|s| s.lock().ok()) {
        let cookie_hdr = st.cookies_for_url(url_str);
        if !cookie_hdr.is_empty() {
            let _ = write!(req_buf, "Cookie: {cookie_hdr}\r\n");
        }
    }
    for (k, v) in req_headers {
        let kl = k.to_ascii_lowercase();
        if !matches!(kl.as_str(), "host" | "connection" | "content-length") {
            let _ = write!(req_buf, "{k}: {v}\r\n");
        }
    }
    if !req_body.is_empty() {
        let _ = write!(req_buf, "Content-Length: {}\r\n", req_body.len());
    }
    req_buf.extend_from_slice(b"\r\n");
    req_buf.extend_from_slice(req_body);

    if kind == FetchKind::Subresource {
        if let Some(cached_err) = neg_host_get(host, port) {
            return FetchResponse {
                status: 502,
                final_url: url_str.to_string(),
                headers: BTreeMap::new(),
                body: Vec::new(),
                error: Some(cached_err),
            };
        }
    }

    let addrs = match resolve_host_addrs(host, port) {
        Ok(it) => it,
        Err(e) => {
            let msg = format!("DNS 解析失败 ({host}): {e}");
            if kind == FetchKind::Subresource {
                neg_host_put(host, port, &msg);
            }
            return FetchResponse {
                status: 502,
                final_url: url_str.to_string(),
                headers: BTreeMap::new(),
                body: Vec::new(),
                error: Some(msg),
            };
        }
    };
    if addrs.is_empty() {
        let msg = format!("无法解析地址: {host}");
        if kind == FetchKind::Subresource {
            neg_host_put(host, port, &msg);
        }
        return FetchResponse {
            status: 502,
            final_url: url_str.to_string(),
            headers: BTreeMap::new(),
            body: Vec::new(),
            error: Some(msg),
        };
    }

    let (connect_timeout, read_timeout, write_timeout) = match kind {
        FetchKind::Navigation => (
            Duration::from_secs(6),
            Duration::from_secs(12),
            Duration::from_secs(6),
        ),
        FetchKind::Subresource => (
            Duration::from_millis(3500),
            Duration::from_secs(5),
            Duration::from_secs(4),
        ),
    };

    let tcp = match connect_tcp_with_fallback(addrs, connect_timeout) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("连接失败 ({host}): {e}");
            if kind == FetchKind::Subresource {
                neg_host_put(host, port, &msg);
            }
            return FetchResponse {
                status: 502,
                final_url: url_str.to_string(),
                headers: BTreeMap::new(),
                body: Vec::new(),
                error: Some(msg),
            };
        }
    };
    let _ = tcp.set_read_timeout(Some(read_timeout));
    let _ = tcp.set_write_timeout(Some(write_timeout));
    let timeout_io = TimeoutTcp {
        inner: tcp,
        read_timeout,
        write_timeout,
    };

    let is_head = method.eq_ignore_ascii_case("HEAD");
    let mut raw_resp = Vec::new();
    let io_res = if is_tls {
        let cfg = Arc::clone(tls_client_config());
        let Ok(sni) = rustls::pki_types::ServerName::try_from(host.to_string()) else {
            return FetchResponse {
                status: 400,
                final_url: url_str.to_string(),
                headers: BTreeMap::new(),
                body: Vec::new(),
                error: Some(format!("无效 TLS 主机名: {host}")),
            };
        };
        match ClientConnection::new(cfg, sni) {
            Ok(conn) => {
                let mut stream = rustls::StreamOwned::new(conn, timeout_io);
                stream
                    .write_all(&req_buf)
                    .and_then(|_| stream.flush())
                    .and_then(|_| read_to_end_allow_eof(&mut stream, &mut raw_resp, is_head))
            }
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())),
        }
    } else {
        let mut stream = timeout_io;
        stream
            .write_all(&req_buf)
            .and_then(|_| stream.flush())
            .and_then(|_| read_to_end_allow_eof(&mut stream, &mut raw_resp, is_head))
    };

    if let Err(e) = io_res {
        if raw_resp.is_empty() || !http_has_headers(&raw_resp) {
            return FetchResponse {
                status: 502,
                final_url: url_str.to_string(),
                headers: BTreeMap::new(),
                body: Vec::new(),
                error: Some(format!("网络读取错误: {e}")),
            };
        }
    }

    parse_http_response(url_str, &raw_resp, storage)
}

use rustls::ClientConnection;

fn poll_fd_ready(fd: RawFd, events: i16, timeout: Duration) -> std::io::Result<i16> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let rem_ms = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, rem_ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                if std::time::Instant::now() < deadline {
                    continue;
                }
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "poll timed out"));
            }
            return Err(err);
        }
        if rc == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "socket timed out"));
        }
        return Ok(pfd.revents);
    }
}

fn connect_single_addr(addr: &SocketAddr, timeout: Duration) -> std::io::Result<TcpStream> {
    let domain = match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    };
    let fd = unsafe { libc::socket(domain, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Wrap immediately so any early error closes the file descriptor cleanly.
    let stream = unsafe { TcpStream::from_raw_fd(fd) };
    stream.set_nonblocking(true)?;

    let rc = match addr {
        SocketAddr::V4(v4) => {
            let sa = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            unsafe {
                libc::connect(
                    stream.as_raw_fd(),
                    &sa as *const libc::sockaddr_in as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
        }
        SocketAddr::V6(v6) => {
            let sa6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                },
                sin6_scope_id: v6.scope_id(),
            };
            unsafe {
                libc::connect(
                    stream.as_raw_fd(),
                    &sa6 as *const libc::sockaddr_in6 as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        }
    };

    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err);
        }
    }

    // SpaceKernel non-blocking connect() returns 0 while still in SynSent,
    // whereas poll(POLLOUT) only fires once the 3-way handshake establishes (or Closed/ERR).
    let revents = poll_fd_ready(stream.as_raw_fd(), libc::POLLOUT, timeout)?;
    let mut so_err: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let gso = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut so_err as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        )
    };
    if gso == 0 && so_err != 0 {
        return Err(std::io::Error::from_raw_os_error(so_err));
    }
    if (revents & libc::POLLOUT) == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "TCP connection closed during handshake",
        ));
    }
    stream.set_nonblocking(false)?;
    Ok(stream)
}

pub fn connect_tcp_with_fallback(
    addrs: Vec<SocketAddr>,
    total_timeout: Duration,
) -> std::io::Result<TcpStream> {
    connect_tcp_with_opener(addrs, total_timeout, connect_single_addr)
}

pub fn connect_tcp_with_opener<F>(
    addrs: Vec<SocketAddr>,
    total_timeout: Duration,
    mut opener: F,
) -> std::io::Result<TcpStream>
where
    F: FnMut(&SocketAddr, Duration) -> std::io::Result<TcpStream>,
{
    let has_v4 = addrs.iter().any(|a| a.is_ipv4());
    // When IPv4 addresses exist, avoid falling through to AF_INET6 (which fails with EPROTOTYPE on SpaceOS
    // and would otherwise overwrite the real IPv4 error).
    let candidates: Vec<SocketAddr> = if has_v4 {
        addrs.into_iter().filter(|a| a.is_ipv4()).take(3).collect()
    } else {
        addrs.into_iter().take(2).collect()
    };
    if candidates.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no resolved socket address",
        ));
    }

    let deadline = Instant::now() + total_timeout;
    let mut last_err: Option<std::io::Error> = None;
    for (idx, addr) in candidates.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let rem_attempts = (candidates.len() - idx) as u32;
        let attempt_timeout = remaining
            .checked_div(rem_attempts)
            .unwrap_or(remaining)
            .max(Duration::from_millis(1200))
            .min(remaining);
        match opener(addr, attempt_timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                let is_unsupported_v6 = addr.is_ipv6()
                    && matches!(
                        e.raw_os_error(),
                        Some(libc::EPROTOTYPE | libc::EAFNOSUPPORT | libc::ENETUNREACH)
                    );
                if is_unsupported_v6 {
                    if last_err.is_none() {
                        last_err = Some(std::io::Error::new(
                            std::io::ErrorKind::AddrNotAvailable,
                            "目标主机仅返回 IPv6 地址，当前网络暂不支持 IPv6 TCP",
                        ));
                    }
                    break;
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "connection timed out")
    }))
}

struct TimeoutTcp {
    inner: TcpStream,
    read_timeout: Duration,
    write_timeout: Duration,
}

impl Read for TimeoutTcp {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let rev = poll_fd_ready(self.inner.as_raw_fd(), libc::POLLIN, self.read_timeout)?;
        if (rev & libc::POLLERR) != 0 && (rev & libc::POLLIN) == 0 {
            let mut so_err: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            unsafe {
                libc::getsockopt(
                    self.inner.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut so_err as *mut libc::c_int as *mut libc::c_void,
                    &mut len,
                );
            }
            if so_err != 0 {
                return Err(std::io::Error::from_raw_os_error(so_err));
            }
        }
        self.inner.read(buf)
    }
}

impl Write for TimeoutTcp {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        poll_fd_ready(self.inner.as_raw_fd(), libc::POLLOUT, self.write_timeout)?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn http_has_headers(raw: &[u8]) -> bool {
    raw.windows(4).any(|w| w == b"\r\n\r\n")
}

fn http_response_complete(raw: &[u8], is_head: bool) -> bool {
    let mut headers_arr = [httparse::EMPTY_HEADER; 128];
    let mut resp = httparse::Response::new(&mut headers_arr);
    let Ok(httparse::Status::Complete(body_off)) = resp.parse(raw) else {
        return false;
    };
    let status = resp.code.unwrap_or(200);
    if is_head || matches!(status, 100..=199 | 204 | 304) {
        return raw.len() >= body_off;
    }
    let mut chunked = false;
    let mut content_len: Option<usize> = None;
    for h in resp.headers.iter() {
        if h.name.eq_ignore_ascii_case("transfer-encoding") {
            let v = String::from_utf8_lossy(h.value);
            if v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        } else if h.name.eq_ignore_ascii_case("content-length") {
            let v = String::from_utf8_lossy(h.value);
            if let Ok(n) = v.trim().parse::<usize>() {
                content_len = Some(n);
            }
        }
    }
    if chunked {
        return chunked_body_complete(&raw[body_off..]);
    }
    if let Some(n) = content_len {
        return raw.len().saturating_sub(body_off) >= n;
    }
    false
}

fn chunked_body_complete(raw: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos < raw.len() {
        let Some(crlf_rel) = raw[pos..].windows(2).position(|w| w == b"\r\n") else {
            return false;
        };
        let line = std::str::from_utf8(&raw[pos..pos + crlf_rel]).unwrap_or("");
        let hex = line.split(';').next().unwrap_or("").trim();
        let Ok(sz) = usize::from_str_radix(hex, 16) else {
            return false;
        };
        pos += crlf_rel + 2;
        if sz == 0 {
            return raw.get(pos..pos + 2) == Some(b"\r\n")
                || raw[pos..].windows(4).any(|w| w == b"\r\n\r\n");
        }
        if pos + sz + 2 > raw.len() {
            return false;
        }
        pos += sz + 2;
    }
    false
}

fn read_to_end_allow_eof<R: Read>(
    reader: &mut R,
    out: &mut Vec<u8>,
    is_head: bool,
) -> std::io::Result<()> {
    let mut chunk = [0u8; 8192];
    loop {
        if http_response_complete(out, is_head) {
            return Ok(());
        }
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() > 16 * 1024 * 1024 || http_response_complete(out, is_head) {
                    return Ok(());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

pub fn parse_http_response(
    url_str: &str,
    raw: &[u8],
    storage: Option<&Arc<Mutex<BrowserStorage>>>,
) -> FetchResponse {
    let mut headers_arr = [httparse::EMPTY_HEADER; 128];
    let mut resp = httparse::Response::new(&mut headers_arr);
    match resp.parse(raw) {
        Ok(httparse::Status::Complete(body_off)) => {
            let status = resp.code.unwrap_or(200);
            let mut hdrs = BTreeMap::new();
            for h in resp.headers.iter() {
                let k = h.name.to_ascii_lowercase();
                let v = String::from_utf8_lossy(h.value).trim().to_string();
                if k == "set-cookie" {
                    if let Some(mut st) = storage.and_then(|s| s.lock().ok()) {
                        st.set_cookie_from_str(url_str, &v);
                    }
                }
                hdrs.insert(k, v);
            }
            let raw_body = &raw[body_off..];
            let unchunked = if hdrs
                .get("transfer-encoding")
                .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
            {
                decode_chunked(raw_body)
            } else {
                raw_body.to_vec()
            };
            let enc = hdrs
                .get("content-encoding")
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            let decoded = decompress_body(&enc, &unchunked);
            FetchResponse {
                status,
                final_url: url_str.to_string(),
                headers: hdrs,
                body: decoded,
                error: None,
            }
        }
        _ => FetchResponse {
            status: 502,
            final_url: url_str.to_string(),
            headers: BTreeMap::new(),
            body: Vec::new(),
            error: Some(String::from("HTTP 响应头解析失败")),
        },
    }
}

pub fn decode_chunked(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < raw.len() {
        let Some(crlf_rel) = raw[pos..].windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_line = String::from_utf8_lossy(&raw[pos..pos + crlf_rel]);
        let hex = size_line.split(';').next().unwrap_or("").trim();
        let Ok(len) = usize::from_str_radix(hex, 16) else {
            break;
        };
        pos += crlf_rel + 2;
        if len == 0 {
            break;
        }
        let take = len.min(raw.len().saturating_sub(pos));
        out.extend_from_slice(&raw[pos..pos + take]);
        pos += take;
        if pos + 2 <= raw.len() && &raw[pos..pos + 2] == b"\r\n" {
            pos += 2;
        }
    }
    out
}

fn decompress_body(encoding: &str, data: &[u8]) -> Vec<u8> {
    if encoding.contains("gzip") {
        let mut dec = GzDecoder::new(data);
        let mut out = Vec::new();
        if dec.read_to_end(&mut out).is_ok() {
            return out;
        }
    } else if encoding.contains("deflate") {
        let mut dec = ZlibDecoder::new(data);
        let mut out = Vec::new();
        if dec.read_to_end(&mut out).is_ok() {
            return out;
        }
    } else if encoding.contains("br") {
        let mut dec = Decompressor::new(data, 4096);
        let mut out = Vec::new();
        if dec.read_to_end(&mut out).is_ok() {
            return out;
        }
    }
    data.to_vec()
}

pub fn decode_image_sprite(bytes: &[u8], hint: &str) -> Option<Sprite> {
    if bytes.is_empty() {
        return None;
    }
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(128)]).to_ascii_lowercase();
    if hint.to_ascii_lowercase().contains(".svg")
        || prefix.contains("<svg")
        || prefix.trim_start().starts_with("<?xml")
    {
        if let Ok(tree) = Tree::from_data(bytes, &Options::default()) {
            let sz = tree.size();
            let w = (sz.width().ceil() as u32).clamp(1, 2048);
            let h = (sz.height().ceil() as u32).clamp(1, 2048);
            if let Some(mut pixmap) = Pixmap::new(w, h) {
                resvg::render(&tree, Transform::identity(), &mut pixmap.as_mut());
                return Some(Sprite::from_rgba_premul(w, h, pixmap.data()));
            }
        }
    }

    // PNG signature check
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        if let Ok(mut reader) = dec.read_info() {
            let mut buf = vec![0u8; reader.output_buffer_size()];
            if let Ok(info) = reader.next_frame(&mut buf) {
                let slice = &buf[..info.buffer_size()];
                match info.color_type {
                    png::ColorType::Rgba => {
                        return Some(Sprite::from_rgba_straight(info.width, info.height, slice));
                    }
                    png::ColorType::Rgb => {
                        return Some(Sprite::from_rgb(info.width, info.height, slice));
                    }
                    png::ColorType::GrayscaleAlpha => {
                        let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
                        for ga in slice.chunks_exact(2) {
                            rgba.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
                        }
                        return Some(Sprite::from_rgba_straight(info.width, info.height, &rgba));
                    }
                    png::ColorType::Grayscale => {
                        let mut rgb = Vec::with_capacity((info.width * info.height * 3) as usize);
                        for &g in slice {
                            rgb.extend_from_slice(&[g, g, g]);
                        }
                        return Some(Sprite::from_rgb(info.width, info.height, &rgb));
                    }
                    _ => {}
                }
            }
        }
    }

    // JPEG fallback
    let mut jdec = zune_jpeg::JpegDecoder::new(bytes);
    if let Ok(decoded) = jdec.decode() {
        if let Some(info) = jdec.info() {
            let (w, h) = (info.width as u32, info.height as u32);
            if decoded.len() == (w * h * 3) as usize {
                return Some(Sprite::from_rgb(w, h, &decoded));
            } else if decoded.len() == (w * h) as usize {
                let mut rgb = Vec::with_capacity((w * h * 3) as usize);
                for &g in &decoded {
                    rgb.extend_from_slice(&[g, g, g]);
                }
                return Some(Sprite::from_rgb(w, h, &rgb));
            }
        }
    }
    None
}
