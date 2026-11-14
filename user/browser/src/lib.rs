pub mod browser;
pub mod ffi;
pub mod net;
pub mod pages;
pub mod storage;

#[cfg(test)]
mod tests {
    use crate::ffi::SpacePageHandle;
    use crate::net::{
        connect_tcp_with_opener, decode_chunked, decode_image_sprite, normalize_omnibox_input,
        parse_http_response, prefetch_subresources, resolve_url_ref, sync_fetch,
        sync_fetch_with_kind, AsyncFetchRequest, FetchKind, FetchPool,
    };
    use crate::storage::BrowserStorage;
    use spui_ui::draw::{Canvas, Rect};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "space_browser_{label}_{}_{}",
            std::process::id(),
            nanos
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn test_storage_bookmarks_history_kv_and_cookies() {
        let dir = unique_temp_dir("storage");
        let mut st = BrowserStorage::load_from_dir(dir.clone());
        st.bookmarks.clear();
        st.history.clear();

        assert!(!st.is_bookmarked("https://example.org/docs"));
        assert!(st.toggle_bookmark("Example Docs", "https://example.org/docs"));
        assert!(st.is_bookmarked("https://example.org/docs"));

        st.record_history("https://example.org/docs/api", "API Guide");
        st.record_history("https://rust-lang.org", "Rust");

        let reloaded = BrowserStorage::load_from_dir(dir.clone());
        assert!(reloaded.is_bookmarked("https://example.org/docs"));
        let suggestions = reloaded.suggest_urls("example.org", 5);
        assert!(!suggestions.is_empty());
        assert!(suggestions[0].2, "bookmark suggestion should prioritize first");

        st.kv_set(false, "https://example.org", "theme", "spui-dark");
        st.kv_set(true, "https://example.org", "session_token", "abc123");
        assert_eq!(
            st.kv_get(false, "https://example.org", "theme").as_deref(),
            Some("spui-dark")
        );
        assert_eq!(st.kv_len(false, "https://example.org"), 1);
        assert_eq!(
            st.kv_key(false, "https://example.org", 0).as_deref(),
            Some("theme")
        );
        st.kv_remove(false, "https://example.org", "theme");
        assert_eq!(st.kv_len(false, "https://example.org"), 0);
        st.kv_clear(true, "https://example.org");
        assert_eq!(st.kv_len(true, "https://example.org"), 0);

        st.set_cookie_from_str(
            "https://app.example.org/sub/page",
            "sid=xyz42; Path=/sub; Domain=example.org",
        );
        assert_eq!(
            st.cookies_for_url("https://api.example.org/sub/data"),
            "sid=xyz42"
        );
        assert_eq!(st.cookies_for_url("https://api.example.org/other"), "");

        st.clear_history();
        assert!(st.history.is_empty());
        assert!(st.toggle_bookmark("Example Docs", "https://example.org/docs") == false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_net_urls_http_parser_and_builtin_pages() {
        assert_eq!(normalize_omnibox_input("example.com"), "https://example.com");
        assert_eq!(normalize_omnibox_input("http://127.0.0.1:8080/a"), "http://127.0.0.1:8080/a");
        assert!(normalize_omnibox_input("space os browser").starts_with("space://search?q="));
        assert_eq!(normalize_omnibox_input("space://js-lab"), "space://js-lab");

        assert_eq!(
            resolve_url_ref("https://example.org/dir/sub/page.html", "../img/a.png"),
            "https://example.org/dir/img/a.png"
        );
        assert_eq!(
            resolve_url_ref("https://example.org/dir/page.html", "/root.css"),
            "https://example.org/root.css"
        );

        let chunked = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(chunked), b"hello world");

        let raw_http = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n";
        let parsed = parse_http_response("https://example.org/test", raw_http, None);
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.headers.get("content-type").map(String::as_str), Some("text/plain"));
        assert_eq!(parsed.body, b"ok");

        let home = sync_fetch("GET", "space://home", &BTreeMap::new(), &[], None, true, false);
        assert_eq!(home.status, 200);
        let home_html = String::from_utf8_lossy(&home.body);
        assert!(home_html.contains("Space"));

        let data_resp = sync_fetch(
            "GET",
            "data:text/html;base64,PGgxPkhlbGxvPC9oMT4=",
            &BTreeMap::new(),
            &[],
            None,
            false,
            false,
        );
        assert_eq!(data_resp.status, 200);
        assert_eq!(data_resp.body, b"<h1>Hello</h1>");

        let svg = b"<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'><rect width='16' height='16' fill='#ef4444'/></svg>";
        let spr = decode_image_sprite(svg, "test.svg").expect("decode SVG sprite");
        assert_eq!((spr.width, spr.height), (16, 16));

        // Real local TCP HTTP fetch verification
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let srv = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 9\r\nConnection: close\r\n\r\nSPACE-NET",
                );
            }
        });
        let http_resp = sync_fetch(
            "GET",
            &format!("http://127.0.0.1:{}/ping", addr.port()),
            &BTreeMap::new(),
            &[],
            None,
            false,
            false,
        );
        let _ = srv.join();
        assert_eq!(http_resp.status, 200);
        assert_eq!(http_resp.body, b"SPACE-NET");
    }

    #[test]
    fn test_engine_dom_events_canvas_forms_and_storage() {
        let dir = unique_temp_dir("engine");
        let storage = Arc::new(Mutex::new(BrowserStorage::load_from_dir(dir.clone())));
        let pool = Arc::new(FetchPool::new(Arc::clone(&storage)));
        let mut page = SpacePageHandle::new(1, Arc::clone(&storage), Some(pool), 800, 600, false, false);

        let html = r#"<!DOCTYPE html>
<html>
<head>
<title>Engine Integration Test</title>
<style>
  body { margin: 0; font-family: sans-serif; }
  .row { display: flex; gap: 10px; }
  .box { width: 120px; height: 40px; background: #2563eb; border-radius: 6px; }
  .tall { height: 220px; background: #10b981; }
</style>
</head>
<body>
  <div id="parent">
    <h1 id="hdr">Initial</h1>
    <div class="row">
      <div class="box">A</div>
      <div class="box">B</div>
    </div>
    <canvas id="cv" width="100" height="60" style="display:block;width:100px;height:60px"></canvas>
    <img id="svgimg" src="data:image/svg+xml;utf8,%3Csvg xmlns='http://www.w3.org/2000/svg' width='20' height='20'%3E%3Crect width='20' height='20' fill='%2322c55e'/%3E%3C/svg%3E" style="display:block;width:20px;height:20px">
    <form id="f">
      <input id="inp" type="text" value="alpha" style="width:140px;height:28px">
      <textarea id="ta" style="width:140px;height:40px">notes</textarea>
      <select id="sel" style="width:120px;height:28px">
        <option value="one">One</option>
        <option value="two">Two</option>
      </select>
      <button id="btn" type="button" style="width:80px;height:28px">Go</button>
    </form>
    <a id="cancel_link" href="https://blocked.invalid/nope">Blocked</a>
    <a id="ok_link" href="sub/next.html">Next</a>
    <div id="mount"></div>
  </div>
  <script>
    window.__bubbled = 0;
    window.__timer_done = false;
    window.__micro_done = false;
    document.getElementById('parent').addEventListener('click', function() {
      window.__bubbled += 1;
    });
    document.getElementById('cancel_link').addEventListener('click', function(e) {
      e.preventDefault();
      window.__prevented = true;
    });

    var hdr = document.getElementById('hdr');
    hdr.textContent = 'Updated Heading';
    hdr.classList.add('active');
    hdr.style.color = '#1e293b';

    var node = document.createElement('div');
    node.className = 'tall';
    node.innerHTML = '<p>Dynamic child content needle_xyz</p>';
    document.getElementById('mount').appendChild(node);

    localStorage.setItem('browser_test_key', 'ok_local');
    sessionStorage.setItem('session_test_key', 'ok_session');
    document.cookie = 'mode=test_cookie; Path=/';

    var ctx = document.getElementById('cv').getContext('2d');
    ctx.fillStyle = '#ef4444';
    ctx.fillRect(0, 0, 100, 60);

    Promise.resolve('Micro').then(function(v) {
      window.__micro_done = (v === 'Micro');
    });
    setTimeout(function() {
      window.__timer_done = true;
    }, 5);
  </script>
</body>
</html>"#;

        page.load_html(html, "https://example.org/index.html", None);
        assert_eq!(page.host().title, "Engine Integration Test");
        let dh = page.doc_height();
        let dom_dbg = page.dump_dom();
        let logs_dbg: Vec<String> = page.host().console_logs.iter().map(|c| c.message.clone()).collect();
        assert!(
            dh > 260,
            "doc_height={dh} logs={logs_dbg:?} dom=\n{dom_dbg}"
        );
        assert_eq!(
            page.eval("document.getElementById('hdr').textContent + '|' + document.querySelectorAll('.box').length", None),
            "Updated Heading|2"
        );
        assert_eq!(
            page.eval("document.getElementById('hdr').classList.contains('active')", None),
            "true"
        );
        assert_eq!(
            page.eval("localStorage.getItem('browser_test_key') + '|' + sessionStorage.getItem('session_test_key')", None),
            "ok_local|ok_session"
        );
        assert!(page.eval("document.cookie", None).contains("mode=test_cookie"));

        // Tick timers + promises
        thread::sleep(Duration::from_millis(12));
        for _ in 0..4 {
            page.tick(None);
        }
        assert_eq!(page.eval("window.__timer_done + '|' + window.__micro_done", None), "true|true");

        // Offscreen Canvas2D + SVG draw test
        let mut offscreen = Canvas::new(800, 600, 0xffffff);
        page.draw(&mut offscreen, None, Rect::new(0, 0, 800, 600), 0, 0);
        assert!(
            offscreen.pixels().iter().any(|&px| px == 0xef4444),
            "Canvas2D red fillRect pixels must draw onto offscreen SPUI canvas"
        );
        assert!(
            offscreen.pixels().iter().any(|&px| px == 0x22c55e),
            "SVG image pixels must draw onto offscreen SPUI canvas"
        );

        // Find-in-Page + DOM Dump
        let (matches, y_opt) = page.find_text("needle_xyz", 0);
        assert_eq!(matches, 1);
        assert!(y_opt.is_some());
        let dom_summary = page.dump_dom();
        assert!(dom_summary.contains("body") && dom_summary.contains("canvas"));

        // Form controls & select option
        assert_eq!(page.eval("document.getElementById('inp').value + '|' + document.getElementById('ta').value", None), "alpha|notes");
        let sel_coords = page.eval("var r = document.getElementById('sel').getBoundingClientRect(); Math.round(r.left + 8) + ',' + Math.round(r.top + 8)", None);
        let parts: Vec<i32> = sel_coords.split(',').filter_map(|s| s.parse().ok()).collect();
        page.mouse_down(parts[0], parts[1], 0, 0, 1, None);
        page.mouse_up(parts[0], parts[1], 0, 0, 1, None);
        let popup = page.host().pending_select_popup.clone().expect("<select> click must open popup");
        assert_eq!(popup.items, vec!["One", "Two"]);
        page.select_option(popup.node_id, 1, None);
        assert_eq!(page.eval("document.getElementById('sel').value", None), "two");

        // Event bubbling & preventDefault on <a> vs permitted <a> navigation
        let blk = page.eval("var r = document.getElementById('cancel_link').getBoundingClientRect(); Math.round(r.left + 5) + ',' + Math.round(r.top + 5)", None);
        let bp: Vec<i32> = blk.split(',').filter_map(|s| s.parse().ok()).collect();
        page.mouse_down(bp[0], bp[1], 0, 0, 1, None);
        page.mouse_up(bp[0], bp[1], 0, 0, 1, None);
        assert_eq!(page.eval("window.__prevented && window.__bubbled > 0", None), "true");
        assert!(page.host().pending_navigations.is_empty(), "preventDefault must block navigation");

        let ok = page.eval("var r = document.getElementById('ok_link').getBoundingClientRect(); Math.round(r.left + 5) + ',' + Math.round(r.top + 5)", None);
        let op: Vec<i32> = ok.split(',').filter_map(|s| s.parse().ok()).collect();
        page.mouse_down(op[0], op[1], 0, 0, 1, None);
        page.mouse_up(op[0], op[1], 0, 0, 1, None);
        assert_eq!(
            page.host().pending_navigations.last().map(|(u, _)| u.as_str()),
            Some("https://example.org/sub/next.html")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_watchdog_aborts_infinite_loop() {
        let dir = unique_temp_dir("watchdog");
        let storage = Arc::new(Mutex::new(BrowserStorage::load_from_dir(dir.clone())));
        let mut page = SpacePageHandle::new(2, storage, None, 640, 480, false, false);
        page.load_html("<!DOCTYPE html><html><body><p>Watchdog</p></body></html>", "about:blank", None);

        let t0 = Instant::now();
        let res = page.eval("while (true) {}", None);
        let elapsed = t0.elapsed();
        assert!(res.starts_with("Error:"), "infinite loop must throw interrupted error, got: {res}");
        assert!(elapsed < Duration::from_secs(5), "watchdog took too long: {elapsed:?}");
        assert_eq!(page.eval("40 + 2", None), "42");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ipv4_priority_never_masked_by_ipv6_eprototype() {
        let addrs = vec![
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)), 443),
        ];
        let mut seen = Vec::new();
        let err = connect_tcp_with_opener(addrs, Duration::from_secs(3), |addr, _| {
            seen.push(*addr);
            if addr.is_ipv6() {
                Err(std::io::Error::from_raw_os_error(libc::EPROTOTYPE))
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "ipv4 connect timed out",
                ))
            }
        })
        .expect_err("must fail with IPv4 timeout");
        assert_eq!(seen.len(), 1);
        assert!(seen[0].is_ipv4());
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);

        let v6_only = vec![SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443)];
        let err_v6 = connect_tcp_with_opener(v6_only, Duration::from_secs(2), |_, _| {
            Err(std::io::Error::from_raw_os_error(libc::EPROTOTYPE))
        })
        .expect_err("v6-only EPROTOTYPE should map to clear message");
        assert_ne!(err_v6.raw_os_error(), Some(libc::EPROTOTYPE));
        assert!(err_v6.to_string().contains("IPv6"));
    }

    #[test]
    fn test_fetch_pool_priority_cancellation_and_subresource_cache() {
        let dir = unique_temp_dir("pool_cache");
        let storage = Arc::new(Mutex::new(BrowserStorage::load_from_dir(dir.clone())));
        let pool = Arc::new(FetchPool::new(Arc::clone(&storage)));

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_srv = Arc::clone(&hits);
        let stop_srv = Arc::clone(&stop);
        let srv = thread::spawn(move || {
            while !stop_srv.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else { break };
                if stop_srv.load(Ordering::Acquire) {
                    break;
                }
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                if first_line.contains("/stale") {
                    hits_srv.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nstale",
                    );
                } else if first_line.contains("/nav") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nNAV",
                    );
                } else if first_line.contains("/asset1.js") || first_line.contains("/asset2.js") {
                    hits_srv.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nvar ok=1;",
                    );
                } else {
                    let _ = stream.write_all(
                        b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        });

        let g1 = pool.begin_navigation(1);
        for i in 0..3 {
            pool.enqueue(AsyncFetchRequest {
                tab_id: 1,
                nav_gen: g1,
                req_id: 100 + i,
                is_navigation: false,
                is_image: true,
                method: String::from("GET"),
                url: format!("http://127.0.0.1:{}/stale?i={i}", addr.port()),
                headers: BTreeMap::new(),
                body: Vec::new(),
            });
        }
        // New navigation cancels queued stale subresources from g1 and runs with nav priority.
        let g2 = pool.begin_navigation(1);
        assert!(g2 > g1);
        pool.enqueue(AsyncFetchRequest {
            tab_id: 1,
            nav_gen: g2,
            req_id: 200,
            is_navigation: true,
            is_image: false,
            method: String::from("GET"),
            url: format!("http://127.0.0.1:{}/nav", addr.port()),
            headers: BTreeMap::new(),
            body: Vec::new(),
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut got_nav = false;
        while Instant::now() < deadline && !got_nav {
            for item in pool.drain_completed() {
                if item.req_id == 200 {
                    assert_eq!(item.response.status, 200);
                    assert_eq!(item.response.body, b"NAV");
                    got_nav = true;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(got_nav, "prioritized navigation request must complete quickly");

        let hits_before_cache = hits.load(Ordering::SeqCst);
        let a1 = format!("http://127.0.0.1:{}/asset1.js", addr.port());
        let a2 = format!("http://127.0.0.1:{}/asset2.js", addr.port());
        prefetch_subresources(&[a1.clone(), a2.clone()], Some(&storage), false, false);
        assert_eq!(hits.load(Ordering::SeqCst), hits_before_cache + 2);

        // Subsequent subresource fetches should hit in-memory cache with 0 new TCP hits.
        let r1 = sync_fetch_with_kind(
            "GET",
            &a1,
            &BTreeMap::new(),
            &[],
            Some(&storage),
            false,
            false,
            FetchKind::Subresource,
        );
        let r2 = sync_fetch_with_kind(
            "GET",
            &a2,
            &BTreeMap::new(),
            &[],
            Some(&storage),
            false,
            false,
            FetchKind::Subresource,
        );
        assert_eq!(r1.body, b"var ok=1;");
        assert_eq!(r2.body, b"var ok=1;");
        assert_eq!(hits.load(Ordering::SeqCst), hits_before_cache + 2);

        // Verify bodyless 204 returns immediately without timing out.
        let r204 = sync_fetch_with_kind(
            "GET",
            &format!("http://127.0.0.1:{}/empty204", addr.port()),
            &BTreeMap::new(),
            &[],
            Some(&storage),
            false,
            false,
            FetchKind::Subresource,
        );
        assert_eq!(r204.status, 204);

        stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(addr);
        let _ = srv.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
