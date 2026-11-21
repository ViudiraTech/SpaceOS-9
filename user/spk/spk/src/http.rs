//
//       user/spk/spk/src/http.rs
//       Minimal Linux-friendly HTTP/1.1 client with rustls transport
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{Result, SpkError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;

type OpenResponse = (u16, BTreeMap<String, String>, Box<dyn Read>, String);
const MAX_HEADER_BYTES: usize = 64 * 1024;
const DEFAULT_CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    Http,
    Https,
}

impl Scheme {
    fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

#[derive(Clone, Debug)]
struct Url {
    scheme: Scheme,
    host: String,
    port: u16,
    path: String,
}

impl Url {
    fn parse(text: &str) -> Result<Self> {
        let (scheme, rest) = if let Some(rest) = text.strip_prefix("https://") {
            (Scheme::Https, rest)
        } else if let Some(rest) = text.strip_prefix("http://") {
            (Scheme::Http, rest)
        } else {
            return Err(SpkError::Network(format!("unsupported URL scheme: {text}")));
        };
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        if authority.is_empty() || authority.contains('@') {
            return Err(SpkError::Network(format!("invalid URL authority: {text}")));
        }
        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            let Some((host, rest)) = rest.split_once(']') else {
                return Err(SpkError::Network(format!("invalid IPv6 URL: {text}")));
            };
            let port = if let Some(port) = rest.strip_prefix(':') {
                port.parse()
                    .map_err(|_| SpkError::Network(format!("invalid URL port: {text}")))?
            } else {
                scheme.default_port()
            };
            (host.to_string(), port)
        } else if let Some((host, port)) = authority.rsplit_once(':') {
            if port.bytes().all(|b| b.is_ascii_digit()) && !port.is_empty() {
                (
                    host.to_string(),
                    port.parse()
                        .map_err(|_| SpkError::Network(format!("invalid URL port: {text}")))?,
                )
            } else {
                (authority.to_string(), scheme.default_port())
            }
        } else {
            (authority.to_string(), scheme.default_port())
        };
        if host.is_empty() {
            return Err(SpkError::Network(format!("URL has no host: {text}")));
        }
        let mut path = path.to_string();
        if let Some(fragment) = path.find('#') {
            path.truncate(fragment);
        }
        if path.is_empty() {
            path.push('/');
        }
        Ok(Self {
            scheme,
            host,
            port,
            path,
        })
    }

    fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == self.scheme.default_port() {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    fn origin(&self) -> String {
        let scheme = match self.scheme {
            Scheme::Http => "http",
            Scheme::Https => "https",
        };
        format!("{scheme}://{}", self.host_header())
    }

    fn resolve(&self, location: &str) -> Result<Self> {
        if location.starts_with("http://") || location.starts_with("https://") {
            return Self::parse(location);
        }
        if location.starts_with('/') {
            return Ok(Self {
                scheme: self.scheme,
                host: self.host.clone(),
                port: self.port,
                path: location.to_string(),
            });
        }
        let base = self
            .path
            .rsplit_once('/')
            .map(|(base, _)| base)
            .unwrap_or("");
        let path = format!("{base}/{location}");
        Ok(Self {
            scheme: self.scheme,
            host: self.host.clone(),
            port: self.port,
            path,
        })
    }
}

#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub final_url: String,
}

#[derive(Clone, Debug)]
pub struct DownloadResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub bytes: u64,
    pub final_url: String,
}

#[derive(Clone, Debug)]
pub struct HttpClient {
    ca_bundle: PathBuf,
    connect_timeout: Duration,
    io_timeout: Duration,
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        let ca_bundle = std::env::var_os("SPK_CA_BUNDLE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CA_BUNDLE));
        let connect_timeout = std::env::var("SPK_HTTP_CONNECT_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(CONNECT_TIMEOUT);
        let io_timeout = std::env::var("SPK_HTTP_IO_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(IO_TIMEOUT);
        Ok(Self {
            ca_bundle,
            connect_timeout,
            io_timeout,
        })
    }

    pub fn with_timeouts(connect: Duration, io: Duration) -> Result<Self> {
        Ok(Self {
            ca_bundle: std::env::var_os("SPK_CA_BUNDLE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_CA_BUNDLE)),
            connect_timeout: connect,
            io_timeout: io,
        })
    }

    pub fn get(
        &self,
        url: &str,
        headers: &[(&str, String)],
        max_body: u64,
    ) -> Result<HttpResponse> {
        let mut response = Vec::new();
        let result = self.request(url, headers, max_body, &mut response)?;
        Ok(HttpResponse {
            status: result.status,
            headers: result.headers,
            body: response,
            final_url: result.final_url,
        })
    }

    pub fn download(
        &self,
        url: &str,
        headers: &[(&str, String)],
        sink: &mut dyn Write,
        max_body: u64,
    ) -> Result<DownloadResponse> {
        self.request(url, headers, max_body, sink)
    }

    /// Download a response body into `path`. With `resume_from > 0`, a Range
    /// request is sent; a 206 response is appended only when its
    /// Content-Range starts at the existing length. A 200 response truncates
    /// the file and starts over.
    pub fn download_file(
        &self,
        url: &str,
        headers: &[(&str, String)],
        path: &std::path::Path,
        resume_from: u64,
        max_body: u64,
    ) -> Result<DownloadResponse> {
        let mut current = Url::parse(url)?;
        let mut request_headers: Vec<(&str, String)> = headers.to_vec();
        if resume_from > 0
            && !request_headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("range"))
        {
            request_headers.push(("Range", format!("bytes={resume_from}-")));
        }
        for redirects in 0..=MAX_REDIRECTS {
            let (status, response_headers, mut reader, final_url) =
                self.open(&current, &request_headers)?;
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let location = response_headers.get("location").ok_or_else(|| {
                    SpkError::Network(format!("HTTP {status} without Location from {final_url}"))
                })?;
                let next = current.resolve(location)?;
                if current.scheme == Scheme::Https && next.scheme == Scheme::Http {
                    return Err(SpkError::Network(format!(
                        "refusing HTTPS-to-HTTP redirect {} -> {}",
                        final_url, location
                    )));
                }
                if redirects == MAX_REDIRECTS {
                    return Err(SpkError::Network(format!(
                        "too many redirects while fetching {url}"
                    )));
                }
                current = next;
                continue;
            }
            if status == 416 {
                return Err(SpkError::Network(format!(
                    "HTTP 416 range not satisfiable from {final_url}"
                )));
            }
            let append = status == 206 && resume_from > 0;
            if append {
                let content_range = response_headers.get("content-range").ok_or_else(|| {
                    SpkError::Network("206 response without Content-Range".into())
                })?;
                let start = content_range
                    .strip_prefix("bytes ")
                    .and_then(|value| value.split_once('-'))
                    .and_then(|(start, _)| start.parse::<u64>().ok())
                    .ok_or_else(|| {
                        SpkError::Network(format!("invalid Content-Range: {content_range}"))
                    })?;
                if start != resume_from {
                    return Err(SpkError::Network(format!(
                        "server resumed at byte {start}, expected {resume_from}"
                    )));
                }
            }
            let file = if append {
                std::fs::OpenOptions::new().append(true).open(path)
            } else {
                std::fs::File::create(path)
            }
            .map_err(|error| SpkError::Network(format!("open {}: {error}", path.display())))?;
            let mut file = std::io::BufWriter::new(file);
            let bytes = copy_body(&mut reader, &response_headers, status, &mut file, max_body)?;
            file.flush()
                .map_err(|error| SpkError::Network(format!("flush {}: {error}", path.display())))?;
            let file = file.into_inner().map_err(|error| {
                SpkError::Network(format!("finish {}: {error}", path.display()))
            })?;
            file.sync_all()
                .map_err(|error| SpkError::Network(format!("sync {}: {error}", path.display())))?;
            return Ok(DownloadResponse {
                status,
                headers: response_headers,
                bytes,
                final_url,
            });
        }
        Err(SpkError::Network(format!(
            "too many redirects while fetching {url}"
        )))
    }

    fn request(
        &self,
        url: &str,
        headers: &[(&str, String)],
        max_body: u64,
        sink: &mut dyn Write,
    ) -> Result<DownloadResponse> {
        let mut current = Url::parse(url)?;
        for redirects in 0..=MAX_REDIRECTS {
            let (status, response_headers, mut reader, final_url) = self.open(&current, headers)?;
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let location = response_headers.get("location").ok_or_else(|| {
                    SpkError::Network(format!("HTTP {status} without Location from {final_url}"))
                })?;
                let next = current.resolve(location)?;
                if current.scheme == Scheme::Https && next.scheme == Scheme::Http {
                    return Err(SpkError::Network(format!(
                        "refusing HTTPS-to-HTTP redirect {} -> {}",
                        final_url, location
                    )));
                }
                if redirects == MAX_REDIRECTS {
                    return Err(SpkError::Network(format!(
                        "too many redirects while fetching {url}"
                    )));
                }
                current = next;
                continue;
            }
            let bytes = copy_body(&mut reader, &response_headers, status, sink, max_body)?;
            return Ok(DownloadResponse {
                status,
                headers: response_headers,
                bytes,
                final_url,
            });
        }
        Err(SpkError::Network(format!(
            "too many redirects while fetching {url}"
        )))
    }

    fn open(&self, url: &Url, headers: &[(&str, String)]) -> Result<OpenResponse> {
        let address = if url.host.contains(':') {
            format!("[{}]:{}", url.host, url.port)
        } else {
            format!("{}:{}", url.host, url.port)
        };
        let addrs: Vec<_> = address
            .to_socket_addrs()
            .map_err(|error| SpkError::Network(format!("resolve {address}: {error}")))?
            .collect();
        if addrs.is_empty() {
            return Err(SpkError::Network(format!("no address for {address}")));
        }
        let mut last_error = None;
        let mut stream = None;
        for addr in addrs {
            // SpaceKernel's TCP stack completes connect synchronously for a
            // blocking socket; std's connect_timeout uses a nonblocking
            // connect+poll path that can report writable before the socket is
            // connected. Use a blocking connect and keep per-read/write I/O
            // deadlines. (SPK_HTTP_CONNECT_TIMEOUT_MS remains accepted by the
            // public constructor for callers that need it.)
            let _ = &self.connect_timeout;
            match TcpStream::connect(addr) {
                Ok(value) => {
                    stream = Some(value);
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let stream = stream.ok_or_else(|| {
            SpkError::Network(format!(
                "connect {address}: {}",
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "no usable address".into())
            ))
        })?;
        stream
            .set_read_timeout(Some(self.io_timeout))
            .map_err(|error| SpkError::Network(format!("set read timeout: {error}")))?;
        stream
            .set_write_timeout(Some(self.io_timeout))
            .map_err(|error| SpkError::Network(format!("set write timeout: {error}")))?;
        let mut transport: Box<dyn ReadWrite> = match url.scheme {
            Scheme::Http => Box::new(stream),
            Scheme::Https => Box::new(tls_stream(&self.ca_bundle, &url.host, stream)?),
        };
        let mut request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: spk/0.1\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
            url.path,
            url.host_header()
        );
        for (name, value) in headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        transport
            .write_all(request.as_bytes())
            .map_err(|error| SpkError::Network(format!("send request: {error}")))?;
        transport
            .flush()
            .map_err(|error| SpkError::Network(format!("flush request: {error}")))?;
        let reader: Box<dyn Read> = Box::new(transport);
        let mut reader = BufReader::new(reader);
        let (status, response_headers) = read_headers(&mut reader, &url.origin())?;
        let final_url = format!("{}{}", url.origin(), url.path);
        Ok((status, response_headers, Box::new(reader), final_url))
    }
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

fn tls_stream(
    ca_bundle: &PathBuf,
    host: &str,
    stream: TcpStream,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>> {
    let roots = load_roots(ca_bundle).map_err(SpkError::Network)?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|error| SpkError::Network(format!("invalid TLS server name {host}: {error}")))?;
    let connection = rustls::ClientConnection::new(Arc::new(config), name)
        .map_err(|error| SpkError::Network(format!("TLS setup for {host}: {error}")))?;
    Ok(rustls::StreamOwned::new(connection, stream))
}

fn load_roots(path: &PathBuf) -> std::result::Result<rustls::RootCertStore, String> {
    let data = fs::read(path)
        .map_err(|error| format!("cannot read CA bundle {}: {error}", path.display()))?;
    let mut reader = BufReader::new(std::io::Cursor::new(data));
    let mut roots = rustls::RootCertStore::empty();
    let mut added = 0usize;
    for cert in rustls_pemfile::certs(&mut reader) {
        let cert = cert.map_err(|error| format!("parse CA bundle {}: {error}", path.display()))?;
        roots
            .add(cert)
            .map_err(|error| format!("invalid CA certificate: {error}"))?;
        added += 1;
    }
    if added == 0 {
        return Err(format!(
            "CA bundle {} contains no certificates",
            path.display()
        ));
    }
    Ok(roots)
}

fn read_headers(
    reader: &mut BufReader<Box<dyn Read>>,
    origin: &str,
) -> Result<(u16, BTreeMap<String, String>)> {
    let mut total = 0usize;
    let mut line = String::new();
    let mut status_line = String::new();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| SpkError::Network(format!("read response headers: {error}")))?;
        if read == 0 {
            return Err(SpkError::Network(format!(
                "connection closed before HTTP response from {origin}"
            )));
        }
        total += read;
        if total > MAX_HEADER_BYTES {
            return Err(SpkError::Network("HTTP response headers too large".into()));
        }
        if status_line.is_empty() {
            status_line = line.trim_end_matches(['\r', '\n']).to_string();
            continue;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(SpkError::Network("malformed HTTP response header".into()));
        };
        let key = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        headers
            .entry(key)
            .and_modify(|existing: &mut String| {
                existing.push_str(", ");
                existing.push_str(&value);
            })
            .or_insert(value);
    }
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(SpkError::Network(format!(
            "malformed HTTP status line: {status_line}"
        )));
    }
    let status = parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| SpkError::Network(format!("malformed HTTP status line: {status_line}")))?;
    Ok((status, headers))
}

fn copy_body(
    reader: &mut dyn Read,
    headers: &BTreeMap<String, String>,
    status: u16,
    sink: &mut dyn Write,
    max_body: u64,
) -> Result<u64> {
    if status == 204 || status == 304 || (100..200).contains(&status) {
        return Ok(0);
    }
    let chunked = headers.get("transfer-encoding").is_some_and(|value| {
        value
            .to_ascii_lowercase()
            .split(',')
            .any(|item| item.trim() == "chunked")
    });
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<u64>().ok());
    if let Some(length) = length {
        if length > max_body {
            return Err(SpkError::Network(format!(
                "HTTP body {length} exceeds limit {max_body}"
            )));
        }
    }
    let mut written = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    if chunked {
        loop {
            let mut line = String::new();
            let read = read_line_limited(reader, &mut line, 1024)?;
            if read == 0 {
                return Err(SpkError::Network("truncated chunked response".into()));
            }
            let size_text = line.trim().split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(size_text, 16)
                .map_err(|_| SpkError::Network("invalid chunk size".into()))?;
            if size == 0 {
                break;
            }
            let mut remaining = size;
            while remaining > 0 {
                let want = remaining.min(buffer.len() as u64) as usize;
                let read = reader
                    .read(&mut buffer[..want])
                    .map_err(|error| SpkError::Network(format!("read response body: {error}")))?;
                if read == 0 {
                    return Err(SpkError::Network("truncated chunk".into()));
                }
                written += read as u64;
                if written > max_body {
                    return Err(SpkError::Network("HTTP body exceeds limit".into()));
                }
                sink.write_all(&buffer[..read])
                    .map_err(|error| SpkError::Network(format!("write response body: {error}")))?;
                remaining -= read as u64;
            }
            let mut crlf = [0u8; 2];
            reader
                .read_exact(&mut crlf)
                .map_err(|error| SpkError::Network(format!("read chunk terminator: {error}")))?;
        }
        return Ok(written);
    }
    if let Some(length) = length {
        let mut remaining = length;
        while remaining > 0 {
            let want = remaining.min(buffer.len() as u64) as usize;
            let read = reader
                .read(&mut buffer[..want])
                .map_err(|error| SpkError::Network(format!("read response body: {error}")))?;
            if read == 0 {
                return Err(SpkError::Network(format!(
                    "HTTP body truncated: got {written} of {length} bytes"
                )));
            }
            written += read as u64;
            sink.write_all(&buffer[..read])
                .map_err(|error| SpkError::Network(format!("write response body: {error}")))?;
            remaining -= read as u64;
        }
        return Ok(written);
    }
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| SpkError::Network(format!("read response body: {error}")))?;
        if read == 0 {
            break;
        }
        written += read as u64;
        if written > max_body {
            return Err(SpkError::Network(format!(
                "HTTP body exceeds limit {max_body}"
            )));
        }
        sink.write_all(&buffer[..read])
            .map_err(|error| SpkError::Network(format!("write response body: {error}")))?;
    }
    Ok(written)
}

fn read_line_limited(reader: &mut dyn Read, line: &mut String, limit: usize) -> Result<usize> {
    let mut byte = [0u8; 1];
    line.clear();
    let mut bytes = Vec::new();
    loop {
        let read = reader
            .read(&mut byte)
            .map_err(|error| SpkError::Network(format!("read line: {error}")))?;
        if read == 0 {
            break;
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() > limit {
            return Err(SpkError::Network("HTTP header/line too long".into()));
        }
    }
    line.push_str(&String::from_utf8_lossy(&bytes));
    Ok(bytes.len())
}

/// Resolve a package/index URL against a configured base.
pub fn join_url(base: &str, name: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        name.trim_start_matches('/')
    )
}

/// Header value for HTTP date, if present.
pub fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers.get(&name.to_ascii_lowercase()).map(String::as_str)
}
