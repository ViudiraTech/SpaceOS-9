use crate::storage::BrowserStorage;
use spui_ui::draw::mix;
use spui_ui::theme::Theme;

pub struct BuiltinPage {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageThemeColors {
    pub bg: u32,
    pub card_bg: u32,
    pub card_border: u32,
    pub text: u32,
    pub text_muted: u32,
    pub accent: u32,
    pub accent_soft: u32,
    pub link: u32,
    pub pill_bg: u32,
    pub pill_text: u32,
    pub pill_border: u32,
    pub input_bg: u32,
    pub input_border: u32,
    pub button_bg: u32,
    pub button_border: u32,
    pub button_text: u32,
    pub code_accent: u32,
    pub radius_card: i32,
    pub radius_control: i32,
    pub dark_mode: bool,
    pub theme_style: i32,
}

impl PageThemeColors {
    pub fn from_theme(theme: &Theme) -> Self {
        if theme.is_win98() {
            return Self::from_legacy(false, 1);
        }
        if theme.is_vortexglass() {
            return Self::from_legacy(theme.is_dark(), 2);
        }
        let dark = theme.is_dark();
        let skin_ctrl = theme.skin().map(|s| &s.controls);
        let card_bg = skin_ctrl
            .and_then(|c| c.card.bg.as_ref().map(|b| b.primary_color()))
            .unwrap_or(theme.window_high);
        let card_border = skin_ctrl
            .and_then(|c| c.card.border.map(|(col, _, _)| col))
            .unwrap_or(theme.panel_border);
        let button_bg = skin_ctrl
            .and_then(|c| c.button_normal.bg.as_ref().map(|b| b.primary_color()))
            .unwrap_or(theme.panel_high);
        let button_border = skin_ctrl
            .and_then(|c| c.button_normal.border.map(|(col, _, _)| col))
            .unwrap_or(theme.window_border);
        let button_text = skin_ctrl.map_or(theme.text, |c| c.button_text);
        let input_bg = skin_ctrl.map_or(theme.input_bg, |c| c.input_bg);
        let input_border = skin_ctrl.map_or(theme.window_border, |c| c.input_border);
        let link = if dark {
            mix(theme.accent, 0xffffff, 0.35)
        } else {
            theme.accent
        };
        Self {
            bg: theme.window,
            card_bg,
            card_border,
            text: theme.text,
            text_muted: theme.text_muted,
            accent: theme.accent,
            accent_soft: theme.accent_soft,
            link,
            pill_bg: theme.accent_soft,
            pill_text: if dark {
                mix(theme.accent, 0xffffff, 0.55)
            } else {
                mix(theme.accent, 0x000000, 0.20)
            },
            pill_border: mix(theme.accent, card_border, 0.45),
            input_bg,
            input_border,
            button_bg,
            button_border,
            button_text,
            code_accent: if dark {
                mix(theme.success, 0xffffff, 0.25)
            } else {
                mix(theme.success, 0x000000, 0.15)
            },
            radius_card: theme.radius_card().clamp(2, 12),
            radius_control: theme.radius_control().clamp(2, 10),
            dark_mode: dark,
            theme_style: 0,
        }
    }

    pub fn from_legacy(dark_mode: bool, theme_style: i32) -> Self {
        match theme_style {
            1 => Self {
                bg: 0xc0c0c0,
                card_bg: 0xffffff,
                card_border: 0x808080,
                text: 0x000000,
                text_muted: 0x404040,
                accent: 0x000080,
                accent_soft: 0xdfdfdf,
                link: 0x0000ee,
                pill_bg: 0xdfdfdf,
                pill_text: 0x000000,
                pill_border: 0x808080,
                input_bg: 0xffffff,
                input_border: 0x808080,
                button_bg: 0xc0c0c0,
                button_border: 0x000000,
                button_text: 0x000000,
                code_accent: 0x000080,
                radius_card: 0,
                radius_control: 0,
                dark_mode: false,
                theme_style: 1,
            },
            2 if dark_mode => Self {
                bg: 0x0d1726,
                card_bg: 0x19263a,
                card_border: 0x486387,
                text: 0xe8f1fc,
                text_muted: 0x9db6d4,
                accent: 0x3b82f6,
                accent_soft: 0x213754,
                link: 0x6cb8ff,
                pill_bg: 0x213754,
                pill_text: 0x8ed0ff,
                pill_border: 0x4873a6,
                input_bg: 0x16202e,
                input_border: 0x465a73,
                button_bg: 0x243142,
                button_border: 0x485b73,
                button_text: 0xf1f6fc,
                code_accent: 0x7dd3fc,
                radius_card: 8,
                radius_control: 4,
                dark_mode: true,
                theme_style: 2,
            },
            2 => Self {
                bg: 0xdfeaf8,
                card_bg: 0xfbfdff,
                card_border: 0x8fb1d9,
                text: 0x0f172a,
                text_muted: 0x4f647d,
                accent: 0x0066cc,
                accent_soft: 0xe6f2ff,
                link: 0x0066cc,
                pill_bg: 0xe6f2ff,
                pill_text: 0x0b57a4,
                pill_border: 0x96c2ed,
                input_bg: 0xffffff,
                input_border: 0xabadb3,
                button_bg: 0xebf1f8,
                button_border: 0x7f97b2,
                button_text: 0x0f172a,
                code_accent: 0x0c5aa6,
                radius_card: 8,
                radius_control: 4,
                dark_mode: false,
                theme_style: 2,
            },
            _ if dark_mode => Self {
                bg: 0x14161d,
                card_bg: 0x1c202b,
                card_border: 0x2d3343,
                text: 0xf1f5f9,
                text_muted: 0x94a3b8,
                accent: 0x3b82f6,
                accent_soft: 0x252b3b,
                link: 0x60a5fa,
                pill_bg: 0x252b3b,
                pill_text: 0x93c5fd,
                pill_border: 0x334155,
                input_bg: 0x1e2430,
                input_border: 0x475569,
                button_bg: 0x1e293b,
                button_border: 0x475569,
                button_text: 0xf1f5f9,
                code_accent: 0x34d399,
                radius_card: 10,
                radius_control: 5,
                dark_mode: true,
                theme_style: 0,
            },
            _ => Self {
                bg: 0xf4f6fb,
                card_bg: 0xffffff,
                card_border: 0xe2e8f0,
                text: 0x0f172a,
                text_muted: 0x64748b,
                accent: 0x2563eb,
                accent_soft: 0xeff6ff,
                link: 0x2563eb,
                pill_bg: 0xeff6ff,
                pill_text: 0x1d4ed8,
                pill_border: 0xcbd5e1,
                input_bg: 0xffffff,
                input_border: 0xcbd5e1,
                button_bg: 0xe2e8f0,
                button_border: 0x94a3b8,
                button_text: 0x0f172a,
                code_accent: 0x0f766e,
                radius_card: 10,
                radius_control: 5,
                dark_mode: false,
                theme_style: 0,
            },
        }
    }
}

fn hex(c: u32) -> String {
    format!("#{:06x}", c & 0x00ff_ffff)
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
    serve_builtin_styled(url, storage, dark_mode, if is_win98 { 1 } else { 0 })
}

pub fn serve_builtin_styled(
    url: &str,
    storage: Option<&BrowserStorage>,
    dark_mode: bool,
    theme_style: i32,
) -> BuiltinPage {
    serve_builtin_themed(url, storage, PageThemeColors::from_legacy(dark_mode, theme_style))
}

pub fn serve_builtin_themed(
    url: &str,
    storage: Option<&BrowserStorage>,
    colors: PageThemeColors,
) -> BuiltinPage {
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
            body: js_lab_html(colors),
        },
        "space://about" => BuiltinPage {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: about_html(colors),
        },
        _ => {
            if route == "space://search" {
                let q = url.split("q=").nth(1).unwrap_or("").split('&').next().unwrap_or("");
                let decoded = q.replace('+', " ");
                return BuiltinPage {
                    status: 200,
                    content_type: "text/html; charset=utf-8",
                    body: search_router_html(&decoded, colors),
                };
            }
            BuiltinPage {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: home_html(storage, colors),
            }
        }
    }
}

fn base_css(c: PageThemeColors) -> String {
    if c.theme_style == 1 {
        return format!(
            "body {{ font-family: sans-serif; margin: 20px; background: {}; color: {}; }}
            a {{ color: {}; }}
            .card {{ background: {}; border: 2px solid {}; padding: 14px; margin-bottom: 14px; }}
            .btn {{ background: {}; border: 2px solid {}; padding: 6px 12px; color: {}; text-decoration: none; display: inline-block; }}
            .pill {{ background: {}; border: 1px solid {}; padding: 4px 10px; display: inline-block; margin-right: 6px; }}
            .link-card {{ display: block; background: {}; border: 1px solid {}; padding: 10px 12px; color: {}; text-decoration: none; margin-bottom: 8px; }}
            .muted {{ color: {}; }}",
            hex(c.bg),
            hex(c.text),
            hex(c.link),
            hex(c.card_bg),
            hex(c.card_border),
            hex(c.button_bg),
            hex(c.button_border),
            hex(c.button_text),
            hex(c.pill_bg),
            hex(c.pill_border),
            hex(c.accent_soft),
            hex(c.card_border),
            hex(c.text),
            hex(c.text_muted),
        );
    }
    let body_bg = if c.theme_style == 2 {
        let top = hex(c.bg);
        let bot = hex(mix(c.bg, c.card_bg, 0.65));
        format!("linear-gradient(180deg, {top} 0%, {bot} 100%)")
    } else {
        hex(c.bg)
    };
    let link_bg = hex(mix(c.card_bg, c.accent_soft, 0.35));
    let pill_rad = if c.theme_style == 2 { 999 } else { 999 };
    let link_rad = (c.radius_card - 2).max(2);
    format!(
        "body {{ font-family: sans-serif; margin: 0; padding: 28px 36px; background: {body_bg}; color: {text}; }}
        a {{ color: {link}; }}
        .card {{ background: {card_bg}; border: 1px solid {card_border}; border-radius: {rc}px; padding: 18px; margin-bottom: 16px; }}
        .muted {{ color: {muted}; }}
        .pill {{ background: {pill_bg}; color: {pill_text}; border: 1px solid {pill_border}; border-radius: {pill_rad}px; padding: 4px 11px; font-size: 12px; display: inline-block; margin-right: 6px; }}
        .link-card {{ display: block; background: {link_bg}; border: 1px solid {card_border}; border-radius: {link_rad}px; padding: 12px 14px; color: {text}; text-decoration: none; margin-bottom: 8px; }}",
        body_bg = body_bg,
        text = hex(c.text),
        link = hex(c.link),
        card_bg = hex(c.card_bg),
        card_border = hex(c.card_border),
        rc = c.radius_card,
        muted = hex(c.text_muted),
        pill_bg = hex(c.pill_bg),
        pill_text = hex(c.pill_text),
        pill_border = hex(c.pill_border),
        pill_rad = pill_rad,
        link_bg = link_bg,
        link_rad = link_rad,
    )
}

fn home_html(storage: Option<&BrowserStorage>, colors: PageThemeColors) -> String {
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
        css = base_css(colors),
        bookmarks = bmarks_html
    )
}

fn search_router_html(query: &str, colors: PageThemeColors) -> String {
    let trimmed = query.trim();
    if trimmed.contains("://") || trimmed.starts_with('/') {
        let escaped = escape_html(trimmed);
        return format!(
            "<!doctype html><html><head><title>跳转中...</title></head><body><script>location.replace('{escaped}');</script><a href=\"{escaped}\">{escaped}</a></body></html>"
        );
    }
    let q_esc = escape_html(trimmed);
    let url_q = url_encode_simple(trimmed);
    let css = base_css(colors);
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

fn js_lab_html(colors: PageThemeColors) -> String {
    let base = base_css(colors);
    let extra = if colors.theme_style == 1 {
        r#"
  .box { background: #fff; border: 2px solid #808080; padding: 14px; margin-bottom: 14px; }
  .badge { background: #dfdfdf; color: #000; border: 1px solid #808080; padding: 3px 9px; font-size: 12px; }
  .highlighted { background: #ffff00; border-color: #000; font-weight: bold; }
  canvas { border: 2px solid #808080; background: #fff; }
  #form-out { margin-top: 8px; font-size: 13px; color: #000080; }
"#
        .to_string()
    } else {
        format!(
            "
  .box {{ background: {card_bg}; border: 1px solid {card_border}; border-radius: {rc}px; padding: 14px; margin-bottom: 14px; color: {text}; }}
  .badge {{ background: {pill_bg}; color: {pill_text}; padding: 3px 9px; border-radius: {rctrl}px; font-size: 12px; border: 1px solid {pill_border}; }}
  .highlighted {{ background: #fef08a; color: #854d0e; border-color: #eab308; font-weight: bold; }}
  canvas {{ border: 1px solid {card_border}; border-radius: {rctrl}px; background: {input_bg}; }}
  #form-out {{ margin-top: 8px; font-size: 13px; color: {code_accent}; }}
",
            card_bg = hex(colors.card_bg),
            card_border = hex(colors.card_border),
            rc = colors.radius_card,
            text = hex(colors.text),
            pill_bg = hex(colors.pill_bg),
            pill_text = hex(colors.pill_text),
            rctrl = colors.radius_control,
            pill_border = hex(colors.pill_border),
            input_bg = hex(colors.input_bg),
            code_accent = hex(colors.code_accent),
        )
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

fn about_html(colors: PageThemeColors) -> String {
    let css = base_css(colors);
    let box_extra = if colors.theme_style == 1 {
        ".box { background: #fff; border: 2px solid #808080; padding: 16px; }".to_string()
    } else {
        format!(
            ".box {{ background: {}; border: 1px solid {}; border-radius: {}px; padding: 16px; color: {}; }}",
            hex(colors.card_bg),
            hex(colors.card_border),
            colors.radius_card,
            hex(colors.text),
        )
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
