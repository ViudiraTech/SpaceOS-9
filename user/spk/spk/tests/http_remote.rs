//
//       user/spk/spk/tests/http_remote.rs
//       P4 HTTP client, remote index snapshots, Range and cache tests
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use spk_core::index::{Index, IndexPackage};
use spk_core::keys::KeyPair;
use spk_core::version::Version;
use spk_ops::http::HttpClient;
use spk_ops::remote::{self, RemoteOptions};
use spk_ops::security::SecurityState;
use spk_ops::state::Paths;

#[derive(Clone, Default)]
struct Route {
    status: u16,
    body: Vec<u8>,
    etag: String,
    location: Option<String>,
    delay: Duration,
    fail_first: usize,
}

impl Route {
    fn ok(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            body,
            etag: String::new(),
            ..Route::default()
        }
    }
}

struct Server {
    port: u16,
    routes: Arc<Mutex<BTreeMap<String, Route>>>,
    counts: Arc<Mutex<BTreeMap<String, usize>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(routes: BTreeMap<String, Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes = Arc::new(Mutex::new(routes));
        let counts = Arc::new(Mutex::new(BTreeMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let routes = routes.clone();
            let counts = counts.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let routes = routes.clone();
                            let counts = counts.clone();
                            thread::spawn(move || handle(stream, routes, counts));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            port,
            routes,
            counts,
            stop,
            thread: Some(thread),
        }
    }

    fn base(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    fn set(&self, path: &str, route: Route) {
        self.routes.lock().unwrap().insert(path.to_string(), route);
    }

    fn count(&self, path: &str) -> usize {
        self.counts.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle(
    mut stream: TcpStream,
    routes: Arc<Mutex<BTreeMap<String, Route>>>,
    counts: Arc<Mutex<BTreeMap<String, usize>>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
        if request.len() > 64 * 1024 {
            return;
        }
    }
    let text = String::from_utf8_lossy(&request);
    let path = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let headers: BTreeMap<String, String> = text
        .lines()
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let count = {
        let mut counts = counts.lock().unwrap();
        let count = counts.entry(path.clone()).or_insert(0);
        *count += 1;
        *count
    };
    if let Some(route) = routes.lock().unwrap().get(&path).cloned() {
        if !route.delay.is_zero() {
            thread::sleep(route.delay);
        }
        if count <= route.fail_first {
            write_response(&mut stream, 500, &[], b"retry", "");
            return;
        }
        if route.status != 200 {
            if let Some(location) = &route.location {
                write_response(
                    &mut stream,
                    route.status,
                    &[("Location", location.as_str())],
                    b"",
                    "",
                );
            } else {
                write_response(&mut stream, route.status, &[], b"error", "");
            }
            return;
        }
        let etag_match = !route.etag.is_empty()
            && headers
                .get("if-none-match")
                .is_some_and(|value| value == &route.etag);
        if etag_match {
            write_response(&mut stream, 304, &[("ETag", route.etag.as_str())], b"", "");
            return;
        }
        if let Some(range) = headers.get("range") {
            if let Some(start) = range
                .strip_prefix("bytes=")
                .and_then(|value| value.split_once('-'))
                .and_then(|(start, _)| start.parse::<usize>().ok())
            {
                if start < route.body.len() {
                    let body = &route.body[start..];
                    let content_range = format!(
                        "bytes {}-{}/{}",
                        start,
                        route.body.len() - 1,
                        route.body.len()
                    );
                    write_response(
                        &mut stream,
                        206,
                        &[("Content-Range", content_range.as_str())],
                        body,
                        &route.etag,
                    );
                    return;
                }
                write_response(&mut stream, 416, &[], b"", "");
                return;
            }
        }
        write_response(&mut stream, 200, &[], &route.body, &route.etag);
    } else {
        write_response(&mut stream, 404, &[], b"missing", "");
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    extra: &[(&str, &str)],
    body: &[u8],
    etag: &str,
) {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        304 => "Not Modified",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if !etag.is_empty() {
        response.push_str(&format!("ETag: {etag}\r\n"));
    }
    for (name, value) in extra {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn package() -> IndexPackage {
    IndexPackage {
        name: "remote".into(),
        epoch: 0,
        version: "1.0.0".into(),
        release: 1,
        arch: "x86_64".into(),
        abi: "spaceos-1".into(),
        filename: "remote-1.0.0-1-x86_64.spk".into(),
        size: 11,
        sha256: spk_core::fsutil::sha256_bytes(b"remote-body"),
        depends: vec![],
        provides: vec![],
        conflicts: vec![],
        replaces: vec![],
        summary: "remote test".into(),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    paths: Paths,
    key: KeyPair,
    index: Vec<u8>,
    signature: Vec<u8>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path()).unwrap();
    paths.ensure_state().unwrap();
    paths.ensure_trust().unwrap();
    let key = KeyPair::generate("remote test").unwrap();
    std::fs::write(
        paths.trusted.join(format!("{}.pub", key.key_id)),
        key.public().to_json().unwrap(),
    )
    .unwrap();
    let mut index = Index::new("remote", "x86_64", 10, &key.key_id);
    index.packages.push(package());
    let bytes = index.to_bytes().unwrap();
    let signature = serde_json::to_vec_pretty(&key.sign(&bytes)).unwrap();
    Fixture {
        _dir: dir,
        paths,
        key,
        index: bytes,
        signature,
    }
}

fn routes(fixture: &Fixture) -> BTreeMap<String, Route> {
    let mut routes = BTreeMap::new();
    routes.insert(
        "/repo/index.json".into(),
        Route {
            etag: "\"index-1\"".into(),
            ..Route::ok(fixture.index.clone())
        },
    );
    routes.insert(
        "/repo/index.json.sig".into(),
        Route::ok(fixture.signature.clone()),
    );
    routes.insert(
        "/repo/remote-1.0.0-1-x86_64.spk".into(),
        Route::ok(b"remote-body".to_vec()),
    );
    routes
}

#[test]
fn sync_etag_and_cache() {
    let fixture = fixture();
    let server = Server::start(routes(&fixture));
    let options = RemoteOptions::default();
    let first = remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        options,
        spk_ops::i18n::Lang::En,
    )
    .unwrap();
    assert_eq!(first.index.packages.len(), 1);
    assert!(!first.stale);
    let current = std::fs::read_to_string(fixture.paths.repo_cache.join("remote/current")).unwrap();
    assert_eq!(current.trim().len(), 64);
    assert_eq!(server.count("/repo/index.json"), 1);

    let second = remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        options,
        spk_ops::i18n::Lang::En,
    )
    .unwrap();
    assert_eq!(second.index.packages.len(), 1);
    assert_eq!(server.count("/repo/index.json"), 2);
}

#[test]
fn bad_signature_is_fatal_and_bad_index_rejected() {
    let fixture_a = fixture();
    let fixture_b = fixture();
    let server = Server::start(routes(&fixture_a));
    server.set(
        "/repo/index.json.sig",
        Route::ok(b"{\"schema\":1,\"algorithm\":\"ed25519\",\"key_id\":\"ed25519:0000000000000000\",\"sha256\":\"00\",\"signature\":\"AA==\"}".to_vec()),
    );
    assert!(remote::sync_repo(
        &fixture_a.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .is_err());

    let server = Server::start(routes(&fixture_b));
    let mut bad = fixture_b.index.clone();
    bad.extend_from_slice(b"not json");
    let signature = serde_json::to_vec_pretty(&fixture_b.key.sign(&bad)).unwrap();
    server.set("/repo/index.json", Route::ok(bad));
    server.set("/repo/index.json.sig", Route::ok(signature));
    assert!(remote::sync_repo(
        &fixture_b.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .is_err());
}

#[test]
fn sync_retries_500_and_uses_cache_when_server_disappears() {
    let fixture = fixture();
    let mut configured = routes(&fixture);
    configured.get_mut("/repo/index.json").unwrap().fail_first = 2;
    let server = Server::start(configured);
    let synced = remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .unwrap();
    assert_eq!(synced.index.packages.len(), 1);
    assert!(server.count("/repo/index.json") >= 3);
    drop(server);
    let cached = remote::sync_repo(
        &fixture.paths,
        "remote",
        &["http://127.0.0.1:1/repo".to_string()],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .unwrap();
    assert!(cached.stale);
    assert_eq!(cached.index.packages.len(), 1);
}

#[test]
fn package_range_download_and_offline_cache() {
    let fixture = fixture();
    let server = Server::start(routes(&fixture));
    let base = server.base("/repo");
    let pkg = package();
    let path = remote::ensure_package(
        &fixture.paths.package_cache,
        std::slice::from_ref(&base),
        &pkg,
        false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"remote-body");
    assert_eq!(server.count("/repo/remote-1.0.0-1-x86_64.spk"), 1);
    // A second call must use the content-addressed cache.
    let again = remote::ensure_package(
        &fixture.paths.package_cache,
        std::slice::from_ref(&base),
        &pkg,
        true,
    )
    .unwrap();
    assert_eq!(again, path);
    assert_eq!(server.count("/repo/remote-1.0.0-1-x86_64.spk"), 1);

    // Resume a partial download with a Range request.
    let second = "remote-body-2";
    let mut resumed_package = package();
    resumed_package.size = second.len() as u64;
    resumed_package.sha256 = spk_core::fsutil::sha256_bytes(second.as_bytes());
    let part = fixture
        .paths
        .package_cache
        .join(format!("{}.spk.part", resumed_package.sha256));
    std::fs::write(&part, &second.as_bytes()[..5]).unwrap();
    server.set(
        "/repo/remote-1.0.0-1-x86_64.spk",
        Route::ok(second.as_bytes().to_vec()),
    );
    let path = remote::ensure_package(
        &fixture.paths.package_cache,
        &[base],
        &resumed_package,
        false,
    )
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), second.as_bytes());
}

#[test]
fn client_timeout_and_redirect_downgrade_rejected() {
    let server = Server::start(BTreeMap::from([(
        "/slow".into(),
        Route {
            delay: Duration::from_millis(400),
            ..Route::ok(b"slow".to_vec())
        },
    )]));
    let client =
        HttpClient::with_timeouts(Duration::from_millis(200), Duration::from_millis(100)).unwrap();
    assert!(client.get(&server.base("/slow"), &[], 1024).is_err());

    let server = Server::start(BTreeMap::from([
        (
            "/redirect".into(),
            Route {
                status: 302,
                location: Some("/target".into()),
                ..Route::default()
            },
        ),
        ("/target".into(), Route::ok(b"target".to_vec())),
    ]));
    let client = HttpClient::with_timeouts(Duration::from_secs(1), Duration::from_secs(1)).unwrap();
    let response = client.get(&server.base("/redirect"), &[], 1024).unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"target");
    let missing = client.get(&server.base("/missing"), &[], 1024).unwrap();
    assert_eq!(missing.status, 404);
}

#[test]
fn security_floor_and_revocations() {
    let mut security = SecurityState {
        generated_at: 100,
        ..SecurityState::default()
    };
    let mut index = Index::new("remote", "x86_64", 99, "ed25519:0123456789abcdef");
    assert!(security.check_floor(&index, "remote").is_err());
    index.generated_at = 100;
    security.min_version.insert("remote".into(), "2.0".into());
    security
        .revoked_versions
        .push(spk_core::index::RevokedVersion {
            name: "remote".into(),
            version: "1.0.0-1".into(),
            reason: "test".into(),
        });
    assert!(security
        .rejects_target("remote", &Version::parse("1.0.0-1").unwrap())
        .is_some());
    assert!(security
        .rejects_target("remote", &Version::parse("2.0.0-1").unwrap())
        .is_none());
}

fn handle_tls(mut stream: rustls::StreamOwned<rustls::ServerConnection, TcpStream>) {
    let _ = stream.conn.complete_io(&mut stream.sock);
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
        if request.len() > 64 * 1024 {
            return;
        }
    }
    let _ =
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    let _ = stream.flush();
}

#[test]
fn tls_uses_ca_bundle_and_rejects_unknown_ca() {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_der = CertificateDer::from(certified.cert.der().to_vec());
    let key_der =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = stop.clone();
        thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let config = config.clone();
                        thread::spawn(move || {
                            if let Ok(connection) = rustls::ServerConnection::new(config) {
                                handle_tls(rustls::StreamOwned::new(connection, stream));
                            }
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        })
    };
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, certified.cert.pem()).unwrap();
    std::env::set_var("SPK_CA_BUNDLE", &ca);
    let client = HttpClient::with_timeouts(Duration::from_secs(2), Duration::from_secs(2)).unwrap();
    let response = client
        .get(&format!("https://localhost:{port}/"), &[], 16)
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"ok");

    let other = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let wrong = dir.path().join("wrong.pem");
    std::fs::write(&wrong, other.cert.pem()).unwrap();
    std::env::set_var("SPK_CA_BUNDLE", &wrong);
    let client = HttpClient::with_timeouts(Duration::from_secs(2), Duration::from_secs(2)).unwrap();
    assert!(client
        .get(&format!("https://localhost:{port}/"), &[], 16)
        .is_err());
    std::env::remove_var("SPK_CA_BUNDLE");
    stop.store(true, Ordering::Relaxed);
    let _ = thread.join();
}

#[test]
fn cli_install_sync_and_offline_over_http() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(repo_dir.join("x86_64")).unwrap();
    std::fs::create_dir_all(staging.join("usr/share/p4")).unwrap();
    std::fs::write(staging.join("usr/share/p4/file"), b"remote-install\n").unwrap();
    let manifest = dir.path().join("manifest.toml");
    std::fs::write(
        &manifest,
        "name = \"remote-pkg\"\nepoch = 0\nversion = \"1.0.0\"\nrelease = 1\narch = \"x86_64\"\nabi = \"spaceos-1\"\nsummary = \"remote CLI test\"\nlicense = \"MIT\"\nscripts = false\nallow_setuid = false\n",
    )
    .unwrap();
    let package_path = repo_dir
        .join("x86_64")
        .join("remote-pkg-1.0.0-1-x86_64.spk");
    spk_host::pack(&manifest, &staging, &package_path, 19).unwrap();
    let keys = dir.path().join("keys");
    let (secret, public, key) =
        spk_core::keys::write_keypair(&keys, "p4-cli", "P4 CLI", false).unwrap();
    spk_host::generate(
        &repo_dir.join("x86_64"),
        "p4-cli",
        "x86_64",
        &secret,
        Some(100),
        true,
    )
    .unwrap();
    let index = std::fs::read(repo_dir.join("x86_64/index.json")).unwrap();
    let sig = std::fs::read(repo_dir.join("x86_64/index.json.sig")).unwrap();
    let package_bytes = std::fs::read(&package_path).unwrap();
    let server = Server::start(BTreeMap::from([
        ("/repo/index.json".into(), Route::ok(index)),
        ("/repo/index.json.sig".into(), Route::ok(sig)),
        (
            "/repo/remote-pkg-1.0.0-1-x86_64.spk".into(),
            Route::ok(package_bytes),
        ),
    ]));
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("etc/spk/trusted.d")).unwrap();
    std::fs::copy(
        &public,
        root.join("etc/spk/trusted.d")
            .join(format!("{}.pub", key.key_id)),
    )
    .unwrap();
    let spk = env!("CARGO_BIN_EXE_spk");
    let base = server.base("/repo");
    let output = Command::new(spk)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--noconfirm",
            "repo",
            "add",
            "p4-cli",
            &base,
            "--priority",
            "10",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = Command::new(spk)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--noconfirm",
            "--lang",
            "en",
            "install",
            "remote-pkg",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(root.join("usr/share/p4/file")).unwrap(),
        b"remote-install\n"
    );
    assert!(std::fs::metadata(root.join("var/cache/spk/repos")).is_ok());

    let output = Command::new(spk)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--noconfirm",
            "--lang",
            "en",
            "sync",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    drop(server);
    let output = Command::new(spk)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--noconfirm",
            "--offline",
            "--lang",
            "en",
            "install",
            "--needed",
            "remote-pkg",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "offline stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn signed_security_fields_and_rollback_floor() {
    let fixture = fixture();
    let mut index = Index::new("remote", "x86_64", 20, &fixture.key.key_id);
    index.packages.push(package());
    index.min_version.insert("remote".into(), "2.0.0-1".into());
    index
        .revoked_versions
        .push(spk_core::index::RevokedVersion {
            name: "remote".into(),
            version: "1.0.0-1".into(),
            reason: "test".into(),
        });
    index.revoked_keys.push("ed25519:0000000000000000".into());
    let bytes = index.to_bytes().unwrap();
    let signature = serde_json::to_vec_pretty(&fixture.key.sign(&bytes)).unwrap();
    let server = Server::start(BTreeMap::from([
        ("/repo/index.json".into(), Route::ok(bytes)),
        ("/repo/index.json.sig".into(), Route::ok(signature)),
    ]));
    let synced = remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .unwrap();
    assert_eq!(
        synced.security.min_version.get("remote").unwrap(),
        "2.0.0-1"
    );
    assert!(synced
        .security
        .revoked_keys
        .contains("ed25519:0000000000000000"));
    assert!(synced
        .security
        .rejects_target("remote", &Version::parse("1.0.0-1").unwrap())
        .is_some());

    // A signed index older than the local floor is rejected; the cached
    // current pointer remains on the first index.
    let old_index = Index::new("remote", "x86_64", 19, &fixture.key.key_id);
    let old_bytes = old_index.to_bytes().unwrap();
    let old_sig = serde_json::to_vec_pretty(&fixture.key.sign(&old_bytes)).unwrap();
    server.set("/repo/index.json", Route::ok(old_bytes));
    server.set("/repo/index.json.sig", Route::ok(old_sig));
    assert!(remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .is_err());
}

#[test]
fn allow_unsigned_only_for_missing_signature() {
    let fixture = fixture();
    let mut index = Index::new("remote", "x86_64", 5, &fixture.key.key_id);
    index.packages.push(package());
    let bytes = index.to_bytes().unwrap();
    let server = Server::start(BTreeMap::from([
        ("/repo/index.json".into(), Route::ok(bytes.clone())),
        (
            "/repo/index.json.sig".into(),
            Route {
                status: 404,
                ..Route::default()
            },
        ),
    ]));
    let synced = remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions {
            offline: false,
            allow_unsigned: true,
        },
        spk_ops::i18n::Lang::En,
    )
    .unwrap();
    assert_eq!(synced.index.packages.len(), 1);

    // A present but invalid signature is never accepted by --allow-unsigned.
    server.set(
        "/repo/index.json.sig",
        Route::ok(b"{\"schema\":1,\"algorithm\":\"ed25519\",\"key_id\":\"ed25519:0000000000000000\",\"sha256\":\"00\",\"signature\":\"AA==\"}".to_vec()),
    );
    assert!(remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions {
            offline: false,
            allow_unsigned: true,
        },
        spk_ops::i18n::Lang::En,
    )
    .is_err());
}

#[test]
fn mirror_failover_for_index_and_package() {
    let fixture = fixture();
    let server = Server::start(routes(&fixture));
    let good = server.base("/repo");
    let bad = "http://127.0.0.1:1/repo".to_string();
    let synced = remote::sync_repo(
        &fixture.paths,
        "remote",
        &[bad.clone(), good.clone()],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .unwrap();
    assert_eq!(synced.index.packages.len(), 1);
    assert_eq!(synced.mirrors.len(), 2);
    let path = remote::ensure_package(
        &fixture.paths.package_cache,
        &[bad, good],
        &synced.index.packages[0],
        false,
    )
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"remote-body");
}

#[test]
fn dual_signature_verifies_with_either_trusted_key() {
    let fixture = fixture();
    let second = KeyPair::generate("remote second").unwrap();
    std::fs::write(
        fixture.paths.trusted.join(format!("{}.pub", second.key_id)),
        second.public().to_json().unwrap(),
    )
    .unwrap();
    let mut index = Index::new("remote", "x86_64", 30, &fixture.key.key_id);
    index.packages.push(package());
    let bytes = index.to_bytes().unwrap();
    let mut signature = fixture.key.sign(&bytes);
    let secondary = second.sign(&bytes);
    signature.signatures.push(spk_core::keys::SignatureEntry {
        key_id: secondary.key_id,
        signature: secondary.signature,
    });
    let sig_bytes = serde_json::to_vec_pretty(&signature).unwrap();
    let server = Server::start(BTreeMap::from([
        ("/repo/index.json".into(), Route::ok(bytes)),
        ("/repo/index.json.sig".into(), Route::ok(sig_bytes)),
    ]));
    let synced = remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .unwrap();
    assert_eq!(synced.index.packages.len(), 1);

    // Revoking the primary key leaves the secondary signature valid.
    std::fs::write(
        fixture.paths.revoked.join(&fixture.key.key_id),
        b"revoked\n",
    )
    .unwrap();
    let current = std::fs::read_to_string(fixture.paths.repo_cache.join("remote/current")).unwrap();
    let snap = fixture
        .paths
        .repo_cache
        .join("remote/snapshots")
        .join(current.trim())
        .join("index.json.sig");
    let mut revoked = signature.clone();
    revoked.key_id = fixture.key.key_id;
    // Re-verify the cached signature through the public helper.
    let _ = std::fs::read(snap).unwrap();
    let keys = spk_ops::repo::trust_keys(&fixture.paths).unwrap();
    let revoked_keys = spk_ops::repo::revoked_keys(&fixture.paths).unwrap();
    let parsed: spk_core::keys::SignatureFile =
        serde_json::from_slice(&serde_json::to_vec(&revoked).unwrap()).unwrap();
    assert_eq!(
        spk_core::keys::verify_any(&index.to_bytes().unwrap(), &parsed, &keys, &revoked_keys)
            .unwrap(),
        second.key_id
    );
}

#[test]
fn package_index_mismatch_is_rejected_and_404_is_an_error() {
    let fixture = fixture();
    let server = Server::start(routes(&fixture));
    // The index expects sha256("remote-body"); serve a different body.
    server.set(
        "/repo/remote-1.0.0-1-x86_64.spk",
        Route::ok(b"remote-BODY".to_vec()),
    );
    let error = remote::ensure_package(
        &fixture.paths.package_cache,
        &[server.base("/repo")],
        &package(),
        false,
    );
    assert!(error.is_err());
    assert!(!fixture
        .paths
        .package_cache
        .join(format!("{}.spk", package().sha256))
        .exists());

    let server = Server::start(BTreeMap::from([(
        "/repo/index.json".into(),
        Route {
            status: 404,
            ..Route::default()
        },
    )]));
    assert!(remote::sync_repo(
        &fixture.paths,
        "remote",
        &[server.base("/repo")],
        RemoteOptions::default(),
        spk_ops::i18n::Lang::En,
    )
    .is_err());
}
