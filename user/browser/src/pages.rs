use crate::storage::BrowserStorage;

pub struct BuiltinPage {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn serve_builtin(url: &str, storage: Option<&BrowserStorage>, dark_mode: bool, is_win98: bool) -> BuiltinPage {
    if url.starts_with("about:blank") || url.is_empty() {
        return BuiltinPage {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: String::from("<!doctype html><html><head><title>空白页</title></head><body></body></html>"),
        };
    }

    let route = url
        .split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url);

    match route {
        "space://js-lab" => BuiltinPage {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: js_lab_html(dark_mode, is_win98),
        },
        "space://about" => BuiltinPage {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: about_html(dark_mode, is_win98),
        },
        _ => {
            // If space://search?q=... arrived from home form, render quick offline/search router page
            if route == "space://search" {
                let q = url.split("q=").nth(1).unwrap_or("").split('&').next().unwrap_or("");
                let decoded = q.replace('+', " ");
                return BuiltinPage {
                    status: 200,
                    content_type: "text/html; charset=utf-8",
                    body: search_router_html(&decoded, dark_mode, is_win98),
                };
            }
            BuiltinPage {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: home_html(storage, dark_mode, is_win98),
            }
        }
    }
}

fn base_css(dark_mode: bool, is_win98: bool) -> &'static str {
    if is_win98 {
        return r#"
        body { font-family: sans-serif; margin: 20px; background: #c0c0c0; color: #000; }
        .card { background: #fff; border: 2px solid #808080; padding: 14px; margin-bottom: 14px; }
        .btn { background: #c0c0c0; border: 2px solid #000; padding: 6px 12px; color: #000; text-decoration: none; display: inline-block; }
        .pill { background: #dfdfdf; border: 1px solid #808080; padding: 4px 10px; display: inline-block; margin-right: 6px; }
        "#;
    }
    if dark_mode {
        return r#"
        body { font-family: sans-serif; margin: 0; padding: 28px 36px; background: #14161d; color: #f1f5f9; }
        .card { background: #1c202b; border: 1px solid #2d3343; border-radius: 10px; padding: 18px; margin-bottom: 16px; }
        .muted { color: #94a3b8; }
        .pill { background: #252b3b; color: #93c5fd; border-radius: 999px; padding: 4px 11px; font-size: 12px; display: inline-block; margin-right: 6px; }
        .link-card { display: block; background: #232836; border: 1px solid #333b4f; border-radius: 8px; padding: 12px 14px; color: #e2e8f0; text-decoration: none; margin-bottom: 8px; }
        "#;
    }
    r#"
    body { font-family: sans-serif; margin: 0; padding: 28px 36px; background: #f4f6fb; color: #0f172a; }
    .card { background: #ffffff; border: 1px solid #e2e8f0; border-radius: 10px; padding: 18px; margin-bottom: 16px; }
    .muted { color: #64748b; }
    .pill { background: #eff6ff; color: #1d4ed8; border-radius: 999px; padding: 4px 11px; font-size: 12px; display: inline-block; margin-right: 6px; }
    .link-card { display: block; background: #f8fafc; border: 1px solid #e2e8f0; border-radius: 8px; padding: 12px 14px; color: #0f172a; text-decoration: none; margin-bottom: 8px; }
    "#
}

fn home_html(storage: Option<&BrowserStorage>, dark_mode: bool, is_win98: bool) -> String {
    let mut bmarks_html = String::new();
    if let Some(st) = storage {
        for b in st.bookmarks.iter().take(12) {
            bmarks_html.push_str(&format!(
                "<a class=\"link-card\" href=\"{url}\"><strong>{title}</strong><div class=\"muted\" style=\"font-size:12px;margin-top:3px;\">{url}</div></a>",
                url = escape_html(&b.url),
                title = escape_html(&b.title),
            ));
        }
    }
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>Space 浏览器 - 主页</title>
<style>
{css}
.hero {{ display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: 12px; }}
.grid {{ display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }}
</style>
</head>
<body>
  <div class="card hero">
    <div>
      <h1 style="margin:0 0 6px 0;font-size:24px;">Space 浏览器</h1>
      <div class="muted" style="font-size:13px;">LiteHTML 渲染引擎 · QuickJS-NG 脚本引擎 · SPUI 4.0 原生桌面</div>
      <div style="margin-top:10px;">
        <span class="pill">HTML5 + CSS Grid/Flexbox</span>
        <span class="pill">ES2023 + DOM + Fetch</span>
        <span class="pill">Canvas 2D + rustls HTTPS</span>
      </div>
    </div>
    <div style="text-align:right;">
      <div id="live-clock" style="font-size:20px;font-weight:bold;">--:--:--</div>
      <div class="muted" style="font-size:12px;" id="engine-status">正在初始化 QuickJS-NG...</div>
    </div>
  </div>

  <div class="card">
    <form action="space://search" style="display:flex;gap:8px;align-items:center;">
      <input type="text" name="q" size="46" placeholder="输入关键词或访问 space://js-lab / https://...">
      <input type="submit" value="搜索 / 跳转">
      <a href="space://js-lab" style="margin-left:8px;text-decoration:none;"><button type="button">打开 JS 实验室</button></a>
    </form>
  </div>

  <div class="grid">
    <div class="card">
      <h2 style="margin:0 0 12px 0;font-size:16px;">快捷书签</h2>
      {bookmarks}
    </div>
    <div class="card">
      <h2 style="margin:0 0 12px 0;font-size:16px;">键盘快捷键与特性</h2>
      <ul style="margin:0;padding-left:18px;line-height:1.8;font-size:13px;">
        <li><strong>Ctrl+T / Ctrl+W</strong>：新建 / 关闭标签页</li>
        <li><strong>Ctrl+L</strong>：聚焦地址栏并全选</li>
        <li><strong>Ctrl+F</strong>：页内查找文本并滚动定位</li>
        <li><strong>F12</strong>：展开/收起 DevTools（Console REPL / DOM / Storage）</li>
        <li><strong>F5 / Alt+← / Alt+→</strong>：刷新 / 后退 / 前进</li>
      </ul>
      <div style="margin-top:12px;">
        <a href="space://about">关于内核与协议支持 →</a>
      </div>
    </div>
  </div>

<script>
function updateClock() {{
  var d = new Date();
  var h = String(d.getHours()).padStart(2, '0');
  var m = String(d.getMinutes()).padStart(2, '0');
  var s = String(d.getSeconds()).padStart(2, '0');
  var el = document.getElementById('live-clock');
  if (el) el.textContent = h + ':' + m + ':' + s;
}}
updateClock();
setInterval(updateClock, 1000);
var st = document.getElementById('engine-status');
if (st) st.textContent = 'QuickJS-NG 就绪 · UA: SpaceBrowser/1.0';
</script>
</body>
</html>"#,
        css = base_css(dark_mode, is_win98),
        bookmarks = bmarks_html
    )
}

fn search_router_html(query: &str, dark_mode: bool, is_win98: bool) -> String {
    let trimmed = query.trim();
    if trimmed.contains("://") || trimmed.starts_with('/') {
        let escaped = escape_html(trimmed);
        return format!(
            "<!doctype html><html><head><title>跳转中...</title></head><body><script>location.replace('{escaped}');</script><a href=\"{escaped}\">{escaped}</a></body></html>"
        );
    }
    let q_esc = escape_html(trimmed);
    let url_q = url_encode_simple(trimmed);
    let css = base_css(dark_mode, is_win98);
    format!(
        r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>搜索: {q_esc}</title>
<style>{css}</style>
</head>
<body>
  <h2>搜索路由器: "{q_esc}"</h2>
  <div class="card">
    <p>选择联网引擎或返回内置实验页：</p>
    <ul>
      <li><a href="https://lite.duckduckgo.com/lite/?q={url_q}">使用 DuckDuckGo Lite 搜索 "{q_esc}"</a></li>
      <li><a href="https://html.duckduckgo.com/html/?q={url_q}">使用 DuckDuckGo HTML 搜索 "{q_esc}"</a></li>
      <li><a href="https://cn.bing.com/search?q={url_q}">使用 Bing 搜索 "{q_esc}"</a></li>
      <li><a href="space://js-lab">打开内置 JS + DOM 实验室</a></li>
    </ul>
  </div>
</body>
</html>"#
    )
}

fn url_encode_simple(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn js_lab_html(dark_mode: bool, is_win98: bool) -> String {
    let base = base_css(dark_mode, is_win98);
    let extra = if is_win98 {
        r#"
  .box { background: #fff; border: 2px solid #808080; padding: 14px; margin-bottom: 14px; }
  .badge { background: #dfdfdf; color: #000; border: 1px solid #808080; padding: 3px 9px; font-size: 12px; }
  .highlighted { background: #ffff00; border-color: #000; font-weight: bold; }
  canvas { border: 2px solid #808080; background: #fff; }
  #form-out { margin-top: 8px; font-size: 13px; color: #000080; }
"#
    } else if dark_mode {
        r#"
  .box { background: #1c202b; border: 1px solid #2d3343; border-radius: 8px; padding: 14px; margin-bottom: 14px; color: #f1f5f9; }
  .badge { background: #1e3a8a; color: #bfdbfe; padding: 3px 9px; border-radius: 4px; font-size: 12px; border: 1px solid #3b82f6; }
  .highlighted { background: #854d0e; color: #fef08a; border-color: #facc15; font-weight: bold; }
  canvas { border: 1px solid #334155; border-radius: 6px; background: #0f172a; }
  #form-out { margin-top: 8px; font-size: 13px; color: #34d399; }
"#
    } else {
        r#"
  .box { background: #fff; border: 1px solid #cbd5e1; border-radius: 8px; padding: 14px; margin-bottom: 14px; color: #0f172a; }
  .badge { background: #e0f2fe; color: #0369a1; padding: 3px 9px; border-radius: 4px; font-size: 12px; }
  .highlighted { background: #fef08a; color: #854d0e; border-color: #eab308; font-weight: bold; }
  canvas { border: 1px solid #94a3b8; border-radius: 6px; background: #fff; }
  #form-out { margin-top: 8px; font-size: 13px; color: #0f766e; }
"#
    };
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>JS + DOM 实验室 - Space Browser</title>
<style>
{base}
{extra}
  .row {{ display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }}
</style>
</head>
<body>
  <h1 style="margin-top:0;">LiteHTML + QuickJS-NG 实验室</h1>

  <div class="box">
    <h3 style="margin-top:0;">1. DOM 动态修改、classList 与 CSS 样式重排</h3>
    <div class="row" style="margin-bottom:8px;">
      <button id="btn-add">添加节点</button>
      <button id="btn-style">切换高亮样式</button>
      <button id="btn-clear">清空列表</button>
      <span id="counter" class="badge">计数: 0</span>
    </div>
    <ul id="dyn-list"></ul>
  </div>

  <div class="box">
    <h3 style="margin-top:0;">2. Canvas 2D 实时动画与图形光栅</h3>
    <div class="row">
      <canvas id="cv" width="360" height="160"></canvas>
      <div>
        <button id="btn-anim">暂停/恢复动画</button>
        <p id="canvas-status" style="font-size:13px;">动画运行中...</p>
      </div>
    </div>
  </div>

  <div class="box">
    <h3 style="margin-top:0;">3. 表单控件、事件冒泡与 localStorage</h3>
    <form id="demo-form" style="line-height:2.1;">
      <div>文本输入: <input id="inp-name" type="text" name="user" value="SpaceUser" placeholder="输入用户名"></div>
      <div>下拉选择: <select id="sel-theme" name="mode"><option value="spui">SPUI 4.0</option><option value="win98">Windows 98</option><option value="retro">Space Retro</option></select></div>
      <div>选项与滑块: <label><input id="chk-agree" type="checkbox" checked> 启用持久化</label> · 滑块: <input id="rng" type="range" min="0" max="100" value="64"></div>
      <div>多行备注:<br><textarea id="ta" rows="2" cols="36">LiteHTML + QuickJS-NG 表单测试</textarea></div>
      <div class="row" style="margin-top:6px;">
        <button type="submit">拦截并显示表单数据</button>
        <button type="button" id="btn-store">保存/读取 localStorage</button>
      </div>
    </form>
    <div id="form-out"></div>
  </div>

<script>
var count = 0;
var listEl = document.getElementById('dyn-list');
var counterEl = document.getElementById('counter');

document.getElementById('btn-add').addEventListener('click', function(){{
  count++;
  counterEl.textContent = '计数: ' + count;
  var li = document.createElement('li');
  li.textContent = '动态条目 #' + count + ' @ ' + Math.round(performance.now()) + 'ms';
  listEl.appendChild(li);
}});

document.getElementById('btn-style').addEventListener('click', function(){{
  counterEl.classList.toggle('highlighted');
  counterEl.style.borderRadius = counterEl.classList.contains('highlighted') ? '12px' : '4px';
}});

document.getElementById('btn-clear').addEventListener('click', function(){{
  listEl.innerHTML = '';
  count = 0;
  counterEl.textContent = '计数: 0';
}});

var cv = document.getElementById('cv');
var ctx = cv.getContext('2d');
var running = true;
var angle = 0;

function drawFrame(){{
  ctx.clearRect(0, 0, 360, 160);
  ctx.fillStyle = '#0f172a';
  ctx.fillRect(0, 0, 360, 160);

  ctx.strokeStyle = '#334155';
  ctx.lineWidth = 1;
  for (var x = 20; x < 360; x += 20) {{
    ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, 160); ctx.stroke();
  }}

  ctx.save();
  ctx.translate(180, 80);
  ctx.rotate(angle);
  ctx.fillStyle = '#3b82f6';
  ctx.fillRect(-34, -34, 68, 68);
  ctx.strokeStyle = '#93c5fd';
  ctx.lineWidth = 2;
  ctx.strokeRect(-34, -34, 68, 68);
  ctx.restore();

  ctx.beginPath();
  ctx.arc(180 + Math.cos(angle * 2) * 110, 80 + Math.sin(angle * 2) * 45, 14, 0, Math.PI * 2);
  ctx.fillStyle = '#10b981';
  ctx.fill();

  ctx.fillStyle = '#f8fafc';
  ctx.font = '13px sans-serif';
  ctx.fillText('Canvas 2D · QuickJS-NG', 12, 22);

  if (running) {{
    angle += 0.04;
    requestAnimationFrame(drawFrame);
  }}
}}
requestAnimationFrame(drawFrame);

document.getElementById('btn-anim').addEventListener('click', function(){{
  running = !running;
  document.getElementById('canvas-status').textContent = running ? '动画运行中...' : '动画已暂停';
  if (running) requestAnimationFrame(drawFrame);
}});

document.getElementById('demo-form').addEventListener('submit', function(e){{
  e.preventDefault();
  var info = {{
    user: document.getElementById('inp-name').value,
    mode: document.getElementById('sel-theme').value,
    persist: document.getElementById('chk-agree').checked,
    range: document.getElementById('rng').value,
    note: document.getElementById('ta').value
  }};
  document.getElementById('form-out').textContent = '提交拦截结果: ' + JSON.stringify(info);
}});

document.getElementById('btn-store').addEventListener('click', function(){{
  var val = document.getElementById('inp-name').value;
  localStorage.setItem('lab_user', val);
  document.cookie = 'lab_mode=' + document.getElementById('sel-theme').value + '; path=/';
  document.getElementById('form-out').textContent =
    '已存储 localStorage["lab_user"]=' + localStorage.getItem('lab_user') + ' · cookie=' + document.cookie;
}});
</script>
</body>
</html>"#
    )
}

fn about_html(dark_mode: bool, is_win98: bool) -> String {
    let css = base_css(dark_mode, is_win98);
    let box_extra = if is_win98 {
        ".box { background: #fff; border: 2px solid #808080; padding: 16px; }"
    } else if dark_mode {
        ".box { background: #1c202b; border: 1px solid #2d3343; border-radius: 8px; padding: 16px; color: #f1f5f9; }"
    } else {
        ".box { background: #fff; border: 1px solid #cbd5e1; border-radius: 8px; padding: 16px; color: #0f172a; }"
    };
    format!(
        r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>关于 Space 浏览器</title>
<style>
{css}
  body {{ line-height: 1.7; max-width: 780px; }}
  {box_extra}
</style>
</head>
<body>
  <h1 style="margin-top:0;">Space Browser 1.0</h1>
  <div class="box">
    <p><strong>Space Browser</strong> 是专为 SpaceOS-9 设计的轻量级独立图形网页浏览器：</p>
    <ul>
      <li><strong>排版引擎</strong>：LiteHTML (C++17) + Gumbo HTML5 解析器，支持 Flexbox、CSS Grid、媒体查询、渐变与圆角边框。</li>
      <li><strong>脚本引擎</strong>：QuickJS-NG (C11)，运行于 64 MiB 内存与看门狗超时沙箱中，提供 DOM、事件冒泡、Timers/rAF、Promise、fetch/XHR、Storage 与 Canvas 2D。</li>
      <li><strong>网络协议栈</strong>：纯 Rust HTTP/1.1 + rustls TLS 1.2/1.3 (ring + webpki-roots) + gzip/deflate/brotli 解压。</li>
      <li><strong>界面适配</strong>：复用 SPUI 4.0 原生 X11 MIT-SHM 管线，完整支持 SPUI 4.0 与 Windows 98 主题实时切换。</li>
    </ul>
    <p><a href="space://home">← 返回主页</a></p>
  </div>
</body>
</html>"#
    )
}
