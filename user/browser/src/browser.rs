use crate::ffi::{HistoryCommand, SelectPopupRequest, SpacePageHandle};
use crate::net::{
    normalize_omnibox_input, resolve_url_ref, sync_fetch, AsyncFetchRequest, FetchPool,
    FetchResponse,
};
use crate::storage::BrowserStorage;
use spui_ui::app::{App, UiEvent};
use spui_ui::draw::{mix, Rect};
use spui_ui::icons::{self, Icon};
use spui_ui::text::{Face, TextRenderer};
use spui_ui::theme::Theme;
use spui_ui::ui::{self, SmoothScroll};
use spui_ui::x11::{CursorKind, Key};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const TAB_BAR_H: i32 = 32;
const TOOLBAR_H: i32 = 38;
const FIND_BAR_H: i32 = 32;
const STATUS_BAR_H: i32 = 22;
const DEVTOOLS_H: i32 = 196;
const SCROLLBAR_W: i32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusTarget {
    Page,
    Omnibox,
    FindBar,
    DevRepl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DrawerMode {
    None,
    Bookmarks,
    History,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DevTab {
    Console,
    Elements,
    Storage,
}

#[derive(Clone, Debug, Default)]
struct EditableLine {
    text: String,
    select_all: bool,
}

impl EditableLine {
    fn set(&mut self, value: &str, select_all: bool) {
        self.text.clear();
        self.text.push_str(value);
        self.select_all = select_all;
    }

    fn handle_key(&mut self, key: &Key) -> (bool, bool) {
        // Returns (text_changed, enter_pressed)
        match key.sym {
            0xff0d | 0xff8d => {
                self.select_all = false;
                return (false, true);
            }
            0xff08 => {
                if self.select_all {
                    self.text.clear();
                    self.select_all = false;
                    return (true, false);
                }
                if self.text.pop().is_some() {
                    return (true, false);
                }
                return (false, false);
            }
            _ => {}
        }
        if key.control() {
            match key.sym {
                0x0061 | 0x0041 => {
                    self.select_all = !self.text.is_empty();
                    return (false, false);
                }
                0x0075 | 0x0055 => {
                    if !self.text.is_empty() {
                        self.text.clear();
                        self.select_all = false;
                        return (true, false);
                    }
                    return (false, false);
                }
                _ => return (false, false),
            }
        }
        if let Some(ch) = decode_key_char(key) {
            if self.select_all {
                self.text.clear();
                self.select_all = false;
            }
            self.text.push(ch);
            return (true, false);
        }
        (false, false)
    }
}

fn decode_key_char(key: &Key) -> Option<char> {
    if key.control() || key.alt() {
        return None;
    }
    if let Some(ch) = key.text() {
        if !ch.is_control() {
            return Some(ch);
        }
        return None;
    }
    if (0x0100_0000..=0x0110_ffff).contains(&key.sym) {
        if let Some(ch) = char::from_u32(key.sym - 0x0100_0000) {
            if !ch.is_control() {
                return Some(ch);
            }
        }
    }
    None
}

fn key_to_utf8(key: &Key) -> String {
    if let Some(ch) = decode_key_char(key) {
        let mut s = String::with_capacity(4);
        s.push(ch);
        s
    } else {
        String::new()
    }
}

struct TabState {
    id: u64,
    nav_gen: u64,
    page: SpacePageHandle,
    url: String,
    title: String,
    history: Vec<String>,
    hist_idx: usize,
    vscroll: SmoothScroll,
    scroll_x: i32,
    loading: bool,
    pending_nav_req: Option<u64>,
    alert_banner: Option<(i32, String)>,
}

impl TabState {
    fn new(
        id: u64,
        storage: Arc<Mutex<BrowserStorage>>,
        fetch_pool: Arc<FetchPool>,
        vw: i32,
        vh: i32,
        dark_mode: bool,
        is_win98: bool,
    ) -> Self {
        Self {
            id,
            nav_gen: 0,
            page: SpacePageHandle::new(id, storage, Some(fetch_pool), vw, vh, dark_mode, is_win98),
            url: String::from("space://home"),
            title: String::from("Space 浏览器"),
            history: Vec::new(),
            hist_idx: 0,
            vscroll: SmoothScroll::new(format!("browser_tab_scroll_{id}")),
            scroll_x: 0,
            loading: false,
            pending_nav_req: None,
            alert_banner: None,
        }
    }

    fn can_back(&self) -> bool {
        self.hist_idx > 0
    }

    fn can_forward(&self) -> bool {
        self.hist_idx + 1 < self.history.len()
    }

    fn display_title(&self) -> &str {
        if !self.title.trim().is_empty() {
            &self.title
        } else if !self.url.is_empty() {
            &self.url
        } else {
            "新标签页"
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TabHit {
    tab_rect: Rect,
    close_rect: Rect,
}

#[derive(Clone, Debug)]
struct LayoutRects {
    tab_bar: Rect,
    tabs: Vec<TabHit>,
    new_tab_btn: Rect,
    toolbar: Rect,
    back_btn: Rect,
    fwd_btn: Rect,
    reload_btn: Rect,
    home_btn: Rect,
    omnibox: Rect,
    star_btn: Rect,
    bookmarks_btn: Rect,
    history_btn: Rect,
    devtools_btn: Rect,
    find_bar: Option<Rect>,
    find_input: Rect,
    find_prev_btn: Rect,
    find_next_btn: Rect,
    find_close_btn: Rect,
    viewport_frame: Rect,
    viewport: Rect,
    scrollbar_track: Rect,
    devtools_rect: Option<Rect>,
    dev_tabs: [Rect; 3],
    dev_close_btn: Rect,
    dev_repl_rect: Rect,
    status_bar: Rect,
    banner_rect: Option<Rect>,
    banner_close_btn: Rect,
    autocomplete_rect: Option<Rect>,
    drawer_rect: Option<Rect>,
}

fn compute_layout(
    content: Rect,
    tab_count: usize,
    find_open: bool,
    devtools_open: bool,
    has_banner: bool,
    suggest_count: usize,
    drawer_mode: DrawerMode,
) -> LayoutRects {
    let tab_bar = Rect::new(content.x, content.y, content.w, TAB_BAR_H);
    let mut tabs = Vec::with_capacity(tab_count);
    let count = tab_count.max(1) as i32;
    let avail_tab_w = (content.w - 42).max(96);
    let tab_w = (avail_tab_w / count).clamp(88, 210);
    let mut tx = content.x + 6;
    for _ in 0..tab_count {
        let tr = Rect::new(tx, tab_bar.y + 4, tab_w - 4, TAB_BAR_H - 4);
        let cr = Rect::new(tr.right() - 20, tr.y + (tr.h - 16) / 2, 16, 16);
        tabs.push(TabHit {
            tab_rect: tr,
            close_rect: cr,
        });
        tx += tab_w;
    }
    let new_tab_btn = Rect::new(tx.min(content.right() - 32), tab_bar.y + 5, 26, 23);

    let toolbar = Rect::new(content.x, tab_bar.bottom(), content.w, TOOLBAR_H);
    let mut bx = toolbar.x + 8;
    let by = toolbar.y + (TOOLBAR_H - 28) / 2;
    let back_btn = Rect::new(bx, by, 28, 28);
    bx += 32;
    let fwd_btn = Rect::new(bx, by, 28, 28);
    bx += 32;
    let reload_btn = Rect::new(bx, by, 28, 28);
    bx += 32;
    let home_btn = Rect::new(bx, by, 28, 28);
    bx += 36;

    let mut rx = toolbar.right() - 8;
    rx -= 30;
    let devtools_btn = Rect::new(rx, by, 30, 28);
    rx -= 48;
    let history_btn = Rect::new(rx, by, 44, 28);
    rx -= 48;
    let bookmarks_btn = Rect::new(rx, by, 44, 28);
    rx -= 32;
    let star_btn = Rect::new(rx, by, 28, 28);
    rx -= 8;

    let omni_w = (rx - bx).max(120);
    let omnibox = Rect::new(bx, by, omni_w, 28);

    let mut y_cur = toolbar.bottom();
    let (find_bar, find_input, find_prev_btn, find_next_btn, find_close_btn) = if find_open {
        let fb = Rect::new(content.x, y_cur, content.w, FIND_BAR_H);
        y_cur += FIND_BAR_H;
        let fy = fb.y + 4;
        let inp = Rect::new(fb.x + 8, fy, (fb.w - 220).max(120), 24);
        let prev = Rect::new(fb.right() - 102, fy, 28, 24);
        let next = Rect::new(fb.right() - 70, fy, 28, 24);
        let close = Rect::new(fb.right() - 36, fy, 28, 24);
        (Some(fb), inp, prev, next, close)
    } else {
        (None, Rect::default(), Rect::default(), Rect::default(), Rect::default())
    };

    let status_bar = Rect::new(content.x, content.bottom() - STATUS_BAR_H, content.w, STATUS_BAR_H);
    let dev_h = if devtools_open {
        DEVTOOLS_H.min((status_bar.y - y_cur - 100).max(0))
    } else {
        0
    };
    let (devtools_rect, dev_tabs, dev_close_btn, dev_repl_rect) = if dev_h >= 80 {
        let dr = Rect::new(content.x, status_bar.y - dev_h, content.w, dev_h);
        let t0 = Rect::new(dr.x + 8, dr.y + 4, 86, 24);
        let t1 = Rect::new(t0.right() + 4, dr.y + 4, 86, 24);
        let t2 = Rect::new(t1.right() + 4, dr.y + 4, 92, 24);
        let dc = Rect::new(dr.right() - 30, dr.y + 4, 24, 22);
        let repl = Rect::new(dr.x + 8, dr.bottom() - 30, (dr.w - 16).max(80), 24);
        (Some(dr), [t0, t1, t2], dc, repl)
    } else {
        (
            None,
            [Rect::default(), Rect::default(), Rect::default()],
            Rect::default(),
            Rect::default(),
        )
    };

    let vp_top = y_cur;
    let vp_bottom = devtools_rect.map_or(status_bar.y, |r| r.y);
    let viewport_frame = Rect::new(content.x, vp_top, content.w, (vp_bottom - vp_top).max(40));
    let inner = viewport_frame.inset(2);
    let scrollbar_track = Rect::new(
        (inner.right() - SCROLLBAR_W).max(inner.x),
        inner.y,
        SCROLLBAR_W.min(inner.w),
        inner.h,
    );
    let viewport = Rect::new(inner.x, inner.y, (inner.w - SCROLLBAR_W).max(32), inner.h);

    let (banner_rect, banner_close_btn) = if has_banner {
        let br = Rect::new(viewport.x + 12, viewport.y + 10, (viewport.w - 24).max(100), 32);
        let bc = Rect::new(br.right() - 56, br.y + 4, 48, 24);
        (Some(br), bc)
    } else {
        (None, Rect::default())
    };

    let autocomplete_rect = if suggest_count > 0 {
        let h = (suggest_count as i32 * 28 + 8).min(240);
        Some(Rect::new(omnibox.x, omnibox.bottom() + 2, omnibox.w, h))
    } else {
        None
    };

    let drawer_rect = if drawer_mode != DrawerMode::None {
        let dw = (content.w - 24).min(390);
        let dh = (viewport_frame.h - 16).clamp(120, 420);
        Some(Rect::new(content.right() - dw - 8, toolbar.bottom() + 4, dw, dh))
    } else {
        None
    };

    LayoutRects {
        tab_bar,
        tabs,
        new_tab_btn,
        toolbar,
        back_btn,
        fwd_btn,
        reload_btn,
        home_btn,
        omnibox,
        star_btn,
        bookmarks_btn,
        history_btn,
        devtools_btn,
        find_bar,
        find_input,
        find_prev_btn,
        find_next_btn,
        find_close_btn,
        viewport_frame,
        viewport,
        scrollbar_track,
        devtools_rect,
        dev_tabs,
        dev_close_btn,
        dev_repl_rect,
        status_bar,
        banner_rect,
        banner_close_btn,
        autocomplete_rect,
        drawer_rect,
    }
}

struct BrowserState {
    storage: Arc<Mutex<BrowserStorage>>,
    fetch_pool: Arc<FetchPool>,
    tabs: Vec<TabState>,
    active_tab: usize,
    next_tab_id: u64,
    next_nav_req_id: u64,
    focus: FocusTarget,
    omnibox: EditableLine,
    suggestions: Vec<(String, String, bool)>,
    suggest_sel: Option<usize>,
    drawer: DrawerMode,
    drawer_items: Vec<(String, String, usize)>,
    find_open: bool,
    find_query: EditableLine,
    find_idx: i32,
    find_total: i32,
    devtools_open: bool,
    dev_tab: DevTab,
    dev_repl: EditableLine,
    dev_dom_cache: String,
    pointer: (i32, i32),
    hover_select_idx: Option<usize>,
    dark_mode: bool,
    is_win98: bool,
}

impl BrowserState {
    fn new(
        initial_url: &str,
        storage: Arc<Mutex<BrowserStorage>>,
        fetch_pool: Arc<FetchPool>,
        vw: i32,
        vh: i32,
        theme: &Theme,
        text: Option<&mut TextRenderer>,
    ) -> Self {
        let is_win98 = theme.is_win98();
        let dark_mode = !is_win98 && theme.is_dark();
        fetch_pool.set_theme_flags(dark_mode, is_win98);
        let mut st = Self {
            storage,
            fetch_pool,
            tabs: Vec::new(),
            active_tab: 0,
            next_tab_id: 1,
            next_nav_req_id: 1000,
            focus: FocusTarget::Page,
            omnibox: EditableLine::default(),
            suggestions: Vec::new(),
            suggest_sel: None,
            drawer: DrawerMode::None,
            drawer_items: Vec::new(),
            find_open: false,
            find_query: EditableLine::default(),
            find_idx: 0,
            find_total: 0,
            devtools_open: false,
            dev_tab: DevTab::Console,
            dev_repl: EditableLine::default(),
            dev_dom_cache: String::new(),
            pointer: (-1, -1),
            hover_select_idx: None,
            dark_mode,
            is_win98,
        };
        st.open_new_tab(initial_url, vw, vh, text);
        st
    }

    fn current_tab(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }

    fn current_tab_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    fn open_new_tab(
        &mut self,
        url: &str,
        vw: i32,
        vh: i32,
        text: Option<&mut TextRenderer>,
    ) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let tab = TabState::new(
            id,
            Arc::clone(&self.storage),
            Arc::clone(&self.fetch_pool),
            vw,
            vh,
            self.dark_mode,
            self.is_win98,
        );
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.navigate_tab(self.active_tab, url, true, text);
    }

    fn close_tab(&mut self, idx: usize, vw: i32, vh: i32, text: Option<&mut TextRenderer>) {
        if self.tabs.len() <= 1 {
            self.navigate_tab(0, "space://home", true, text);
            return;
        }
        if idx >= self.tabs.len() {
            return;
        }
        let removed = self.tabs.remove(idx);
        self.fetch_pool.cancel_tab(removed.id);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.sync_omnibox_from_tab();
        self.tabs[self.active_tab].page.resize(vw, vh, text);
    }

    fn sync_omnibox_from_tab(&mut self) {
        let url = self.tabs[self.active_tab].url.clone();
        self.omnibox.set(&url, false);
        self.suggestions.clear();
        self.suggest_sel = None;
    }

    fn navigate_tab(
        &mut self,
        tab_idx: usize,
        raw_url: &str,
        push_history: bool,
        mut text: Option<&mut TextRenderer>,
    ) {
        if tab_idx >= self.tabs.len() {
            return;
        }
        let norm = normalize_omnibox_input(raw_url);
        let target_url = if norm.is_empty() {
            String::from("about:blank")
        } else {
            norm
        };

        let tab_id = self.tabs[tab_idx].id;
        let nav_gen = self.fetch_pool.begin_navigation(tab_id);
        self.tabs[tab_idx].nav_gen = nav_gen;

        let is_local = target_url.starts_with("about:")
            || target_url.starts_with("space://")
            || target_url.starts_with("file://")
            || target_url.starts_with("data:");

        if is_local {
            let resp = sync_fetch(
                "GET",
                &target_url,
                &BTreeMap::new(),
                &[],
                Some(&self.storage),
                self.dark_mode,
                self.is_win98,
            );
            self.apply_loaded_document(tab_idx, &target_url, &resp, push_history, text.as_deref_mut());
            return;
        }

        let req_id = self.next_nav_req_id;
        self.next_nav_req_id += 1;
        let tab = &mut self.tabs[tab_idx];
        tab.url = target_url.clone();
        tab.loading = true;
        tab.pending_nav_req = Some(req_id);
        tab.alert_banner = None;
        if tab.title.is_empty() {
            tab.title = target_url.clone();
        }
        if tab_idx == self.active_tab {
            self.omnibox.set(&target_url, false);
        }
        self.fetch_pool.enqueue(AsyncFetchRequest {
            tab_id: tab.id,
            nav_gen,
            req_id,
            is_navigation: true,
            is_image: false,
            method: String::from("GET"),
            url: target_url,
            headers: BTreeMap::new(),
            body: Vec::new(),
        });
    }

    fn apply_loaded_document(
        &mut self,
        tab_idx: usize,
        requested_url: &str,
        resp: &FetchResponse,
        push_history: bool,
        text: Option<&mut TextRenderer>,
    ) {
        let final_url = if resp.final_url.is_empty() {
            requested_url.to_string()
        } else {
            resp.final_url.clone()
        };
        let html = if let Some(err) = resp.error.as_ref() {
            render_error_page(&final_url, err, self.dark_mode, self.is_win98)
        } else {
            let ct = resp
                .headers
                .get("content-type")
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if ct.contains("image/") {
                format!(
                    "<!DOCTYPE html><html><head><title>{url}</title></head><body style='margin:0;padding:24px;text-align:center'><img src='{url}' style='max-width:100%'></body></html>",
                    url = escape_html(&final_url)
                )
            } else if ct.starts_with("text/plain") || ct.contains("application/json") {
                let body_str = String::from_utf8_lossy(&resp.body);
                format!(
                    "<!DOCTYPE html><html><head><title>{url}</title></head><body><pre style='white-space:pre-wrap;font-family:monospace;padding:16px'>{body}</pre></body></html>",
                    url = escape_html(&final_url),
                    body = escape_html(&body_str)
                )
            } else {
                String::from_utf8_lossy(&resp.body).into_owned()
            }
        };

        let tab = &mut self.tabs[tab_idx];
        tab.loading = false;
        tab.pending_nav_req = None;
        tab.url = final_url.clone();
        tab.vscroll.snap_to(0.0);
        tab.scroll_x = 0;
        tab.page.host_mut().nav_gen = tab.nav_gen;
        tab.page.load_html(&html, &final_url, text);

        let host_title = tab.page.host().title.clone();
        tab.title = if host_title.trim().is_empty() {
            final_url.clone()
        } else {
            host_title
        };

        if push_history {
            if tab.hist_idx + 1 < tab.history.len() {
                tab.history.truncate(tab.hist_idx + 1);
            }
            if tab.history.last().map(|s| s.as_str()) != Some(&final_url) {
                tab.history.push(final_url.clone());
            }
            tab.hist_idx = tab.history.len().saturating_sub(1);
        }

        if resp.error.is_none() {
            if let Ok(mut st) = self.storage.lock() {
                st.record_history(&final_url, &tab.title);
            }
        }
        if tab_idx == self.active_tab {
            self.omnibox.set(&final_url, false);
            self.suggestions.clear();
            self.suggest_sel = None;
            if self.devtools_open && self.dev_tab == DevTab::Elements {
                self.dev_dom_cache = self.tabs[self.active_tab].page.dump_dom();
            }
        }
    }

    fn go_back(&mut self, text: Option<&mut TextRenderer>) {
        let url_opt = {
            let tab = &mut self.tabs[self.active_tab];
            if tab.hist_idx > 0 {
                tab.hist_idx -= 1;
                Some(tab.history[tab.hist_idx].clone())
            } else {
                None
            }
        };
        if let Some(url) = url_opt {
            self.navigate_tab(self.active_tab, &url, false, text);
        }
    }

    fn go_forward(&mut self, text: Option<&mut TextRenderer>) {
        let url_opt = {
            let tab = &mut self.tabs[self.active_tab];
            if tab.hist_idx + 1 < tab.history.len() {
                tab.hist_idx += 1;
                Some(tab.history[tab.hist_idx].clone())
            } else {
                None
            }
        };
        if let Some(url) = url_opt {
            self.navigate_tab(self.active_tab, &url, false, text);
        }
    }

    fn reload_or_stop(&mut self, text: Option<&mut TextRenderer>) {
        if self.tabs[self.active_tab].loading {
            let tab = &mut self.tabs[self.active_tab];
            let tab_id = tab.id;
            tab.loading = false;
            tab.pending_nav_req = None;
            self.fetch_pool.cancel_tab(tab_id);
            return;
        }
        let url = self.tabs[self.active_tab].url.clone();
        self.navigate_tab(self.active_tab, &url, false, text);
    }

    fn refresh_suggestions(&mut self) {
        let q = self.omnibox.text.trim();
        if self.focus != FocusTarget::Omnibox || q.is_empty() {
            self.suggestions.clear();
            self.suggest_sel = None;
            return;
        }
        if let Ok(st) = self.storage.lock() {
            self.suggestions = st.suggest_urls(q, 7);
        }
        self.suggest_sel = None;
    }

    fn refresh_drawer(&mut self) {
        self.drawer_items.clear();
        let Ok(st) = self.storage.lock() else {
            return;
        };
        match self.drawer {
            DrawerMode::Bookmarks => {
                for (i, b) in st.bookmarks.iter().enumerate() {
                    self.drawer_items
                        .push((b.title.clone(), b.url.clone(), i));
                }
            }
            DrawerMode::History => {
                for (i, h) in st.history.iter().rev().take(40).enumerate() {
                    self.drawer_items
                        .push((h.title.clone(), h.url.clone(), i));
                }
            }
            DrawerMode::None => {}
        }
    }

    fn run_find(&mut self, step: i32) {
        let q = self.find_query.text.clone();
        if q.is_empty() {
            self.find_total = 0;
            self.find_idx = 0;
            return;
        }
        let next_idx = if self.find_total <= 0 {
            0
        } else {
            (self.find_idx + step).rem_euclid(self.find_total)
        };
        let (total, y_opt) = self.tabs[self.active_tab].page.find_text(&q, next_idx);
        self.find_total = total;
        self.find_idx = if total > 0 { next_idx.min(total - 1) } else { 0 };
        if let Some(y) = y_opt {
            self.tabs[self.active_tab].vscroll.animate_to(y as f32, 0.16);
        }
    }

    fn sync_theme(&mut self, app: &mut App, vw: i32, vh: i32) {
        let win98 = app.theme.is_win98();
        let dark = !win98 && app.theme.is_dark();
        if win98 != self.is_win98 || dark != self.dark_mode {
            self.is_win98 = win98;
            self.dark_mode = dark;
            self.fetch_pool.set_theme_flags(dark, win98);
            for (idx, tab) in self.tabs.iter_mut().enumerate() {
                tab.page.set_theme(dark, win98, Some(&mut app.text));
                tab.page.resize(vw, vh, Some(&mut app.text));
                if tab.url.starts_with("space://") {
                    let u = tab.url.clone();
                    let resp = sync_fetch(
                        "GET",
                        &u,
                        &BTreeMap::new(),
                        &[],
                        Some(&self.storage),
                        dark,
                        win98,
                    );
                    let html = String::from_utf8_lossy(&resp.body);
                    tab.page.load_html(&html, &u, Some(&mut app.text));
                }
                let _ = idx;
            }
        }
    }

    fn flush_host_events(&mut self, app: &mut App, vp: Rect) -> bool {
        let mut changed = false;
        let navs: Vec<(String, bool)>;
        let hist_ops: Vec<HistoryCommand>;
        let alerts: Vec<(i32, String)>;
        let new_title: String;
        let cur_url: String;
        let cursor: CursorKind;

        {
            let tab = &mut self.tabs[self.active_tab];
            let host = tab.page.host_mut();
            navs = core::mem::take(&mut host.pending_navigations);
            hist_ops = core::mem::take(&mut host.pending_history_ops);
            alerts = host
                .pending_alerts
                .drain(..)
                .map(|a| (a.kind, a.message))
                .collect();
            new_title = host.title.clone();
            cur_url = host.page_url.clone();
            cursor = host.cursor;
        }

        if !new_title.is_empty() && new_title != self.tabs[self.active_tab].title {
            self.tabs[self.active_tab].title = new_title;
            changed = true;
        }

        for cmd in hist_ops {
            match cmd {
                HistoryCommand::Push { url, title } => {
                    let full = resolve_url_ref(&cur_url, &url);
                    let tab = &mut self.tabs[self.active_tab];
                    if tab.hist_idx + 1 < tab.history.len() {
                        tab.history.truncate(tab.hist_idx + 1);
                    }
                    tab.history.push(full.clone());
                    tab.hist_idx = tab.history.len().saturating_sub(1);
                    tab.url = full.clone();
                    if !title.is_empty() {
                        tab.title = title;
                    }
                    self.omnibox.set(&full, false);
                    changed = true;
                }
                HistoryCommand::Replace { url, title } => {
                    let full = resolve_url_ref(&cur_url, &url);
                    let tab = &mut self.tabs[self.active_tab];
                    if let Some(slot) = tab.history.get_mut(tab.hist_idx) {
                        *slot = full.clone();
                    }
                    tab.url = full.clone();
                    if !title.is_empty() {
                        tab.title = title;
                    }
                    self.omnibox.set(&full, false);
                    changed = true;
                }
                HistoryCommand::Go(delta) => {
                    if delta < 0 {
                        self.go_back(Some(&mut app.text));
                    } else if delta > 0 {
                        self.go_forward(Some(&mut app.text));
                    }
                    changed = true;
                }
            }
        }

        if let Some((kind, msg)) = alerts.into_iter().last() {
            self.tabs[self.active_tab].alert_banner = Some((kind, msg));
            changed = true;
        }

        for (url, replace) in navs {
            self.navigate_tab(self.active_tab, &url, !replace, Some(&mut app.text));
            changed = true;
        }

        if self.focus == FocusTarget::Page && vp.contains(self.pointer.0, self.pointer.1) {
            app.set_content_cursor(cursor);
        } else if self.focus == FocusTarget::Omnibox || self.focus == FocusTarget::FindBar {
            app.set_content_cursor(CursorKind::Default);
        }

        let desired_title = format!("{} — Space 浏览器", self.current_tab().display_title());
        if app.title != desired_title {
            app.title = desired_title;
            let _ = app.x.set_title(app.win, &app.title);
        }
        changed
    }

    fn process_async_fetches(&mut self, text: &mut TextRenderer) -> bool {
        let completed = self.fetch_pool.drain_completed();
        if completed.is_empty() {
            return false;
        }
        let mut redraw = false;
        for item in completed {
            let Some(tab_idx) = self.tabs.iter().position(|t| t.id == item.tab_id) else {
                continue;
            };
            if self.tabs[tab_idx].pending_nav_req == Some(item.req_id) {
                let req_url = self.tabs[tab_idx].url.clone();
                self.apply_loaded_document(tab_idx, &req_url, &item.response, true, Some(text));
                redraw = true;
            } else if (item.nav_gen == 0 || item.nav_gen == self.tabs[tab_idx].nav_gen)
                && self.tabs[tab_idx].page.resolve_fetch_or_image(
                    item.req_id,
                    item.is_image,
                    &item.response,
                    Some(text),
                )
            {
                redraw = true;
            }
        }
        redraw
    }

    fn resize_all_tabs(&mut self, vw: i32, vh: i32, text: &mut TextRenderer) {
        for tab in &mut self.tabs {
            tab.page.resize(vw, vh, Some(text));
        }
    }
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_error_page(url: &str, err: &str, dark: bool, win98: bool) -> String {
    let bg = if win98 {
        "#ffffff"
    } else if dark {
        "#0f172a"
    } else {
        "#f8fafc"
    };
    let card = if win98 {
        "#dfdfdf"
    } else if dark {
        "#1e293b"
    } else {
        "#ffffff"
    };
    let fg = if dark && !win98 { "#e2e8f0" } else { "#0f172a" };
    let mut s = String::new();
    s.push_str("<!DOCTYPE html><html><head><title>无法访问页面</title></head><body style='margin:0;padding:40px;font-family:sans-serif;background:");
    s.push_str(bg);
    s.push_str(";color:");
    s.push_str(fg);
    s.push_str("'><div style='max-width:620px;margin:0 auto;padding:24px;border:1px solid #64748b;border-radius:8px;background:");
    s.push_str(card);
    s.push_str("'><h2 style='margin-top:0'>无法连接到目标页面</h2><p>地址: <code>");
    s.push_str(&escape_html(url));
    s.push_str("</code></p><p style='color:#ef4444'>");
    s.push_str(&escape_html(err));
    s.push_str("</p><p><a href='space://home'>返回主页</a> · <a href='space://js-lab'>打开 JS 实验室</a></p></div></body></html>");
    s
}

fn select_popup_rect(vp: Rect, popup: &SelectPopupRequest) -> Rect {
    let item_h = 26;
    let total_h = (popup.items.len() as i32 * item_h + 6).min(220);
    let w = popup.viewport_rect.w.clamp(120, 280);
    let x = (vp.x + popup.viewport_rect.x).clamp(vp.x + 2, (vp.right() - w - 2).max(vp.x + 2));
    let mut y = vp.y + popup.viewport_rect.bottom();
    if y + total_h > vp.bottom() {
        y = (vp.y + popup.viewport_rect.y - total_h).max(vp.y + 2);
    }
    Rect::new(x, y, w, total_h)
}

fn render_browser(app: &mut App, st: &mut BrowserState, layout: &LayoutRects) {
    let theme = &app.theme;
    let canvas = &mut app.canvas;
    let text = &mut app.text;
    let win98 = theme.is_win98();

    // 1. Tab Strip
    canvas.fill(layout.tab_bar, theme.panel);
    canvas.fill(
        Rect::new(layout.tab_bar.x, layout.tab_bar.bottom() - 1, layout.tab_bar.w, 1),
        theme.window_border,
    );
    for (i, th) in layout.tabs.iter().enumerate() {
        let active = i == st.active_tab;
        let hov = th.tab_rect.contains(st.pointer.0, st.pointer.1);
        let tab = &st.tabs[i];
        if win98 {
            let bg = if active { theme.window } else { theme.panel };
            canvas.fill(th.tab_rect, bg);
            ui::raised_bevel(canvas, theme, th.tab_rect);
            if active {
                canvas.fill(
                    Rect::new(th.tab_rect.x + 1, th.tab_rect.bottom() - 2, th.tab_rect.w - 2, 2),
                    theme.window,
                );
            }
        } else {
            let bg = if active {
                theme.window_high
            } else if hov {
                theme.panel_hover
            } else {
                mix(theme.panel, theme.window, 0.35)
            };
            canvas.rounded_rect(th.tab_rect, 6, bg, 1.0);
            if active {
                canvas.rounded_border(th.tab_rect, 6, theme.window_border, 1.0, 1);
                canvas.fill(
                    Rect::new(th.tab_rect.x + 10, th.tab_rect.y + 2, th.tab_rect.w - 20, 2),
                    theme.accent,
                );
            }
        }
        let prefix = if tab.loading { "● " } else { "" };
        let title_str = format!("{prefix}{}", tab.display_title());
        let label = text.truncate(&title_str, (th.tab_rect.w - 34) as f32, 12.5, Face::Ui);
        let fg = if active { theme.text } else { theme.text_muted };
        text.draw(
            canvas,
            &label,
            (th.tab_rect.x + 10) as f32,
            (th.tab_rect.y + 19) as f32,
            12.5,
            fg,
            if active { Face::Bold } else { Face::Ui },
        );
        let close_hov = th.close_rect.contains(st.pointer.0, st.pointer.1);
        if close_hov {
            canvas.rounded_rect(th.close_rect, 4, theme.panel_hover, 0.9);
        }
        text.draw_centered(
            canvas,
            "x",
            (th.close_rect.x + th.close_rect.w / 2) as f32,
            (th.close_rect.y + 12) as f32,
            11.5,
            theme.text_muted,
            Face::Mono,
        );
    }

    ui::button(
        canvas,
        text,
        theme,
        layout.new_tab_btn,
        "+",
        None,
        false,
        layout.new_tab_btn.contains(st.pointer.0, st.pointer.1),
    );

    // 2. Navigation Toolbar
    canvas.fill(layout.toolbar, theme.window_high);
    canvas.fill(
        Rect::new(layout.toolbar.x, layout.toolbar.bottom() - 1, layout.toolbar.w, 1),
        theme.window_border,
    );

    let can_back = st.current_tab().can_back();
    let can_fwd = st.current_tab().can_forward();
    ui::icon_button(
        canvas,
        theme,
        layout.back_btn,
        Icon::GoBack,
        can_back,
        layout.back_btn.contains(st.pointer.0, st.pointer.1),
    );
    ui::button(
        canvas,
        text,
        theme,
        layout.fwd_btn,
        ">",
        None,
        false,
        can_fwd && layout.fwd_btn.contains(st.pointer.0, st.pointer.1),
    );
    if st.current_tab().loading {
        ui::button(
            canvas,
            text,
            theme,
            layout.reload_btn,
            "x",
            None,
            false,
            layout.reload_btn.contains(st.pointer.0, st.pointer.1),
        );
    } else {
        ui::icon_button(
            canvas,
            theme,
            layout.reload_btn,
            Icon::Refresh,
            true,
            layout.reload_btn.contains(st.pointer.0, st.pointer.1),
        );
    }
    ui::icon_button(
        canvas,
        theme,
        layout.home_btn,
        Icon::GoHome,
        true,
        layout.home_btn.contains(st.pointer.0, st.pointer.1),
    );

    ui::text_input(
        canvas,
        text,
        theme,
        layout.omnibox,
        &st.omnibox.text,
        "输入网址或搜索词...",
        st.focus == FocusTarget::Omnibox,
        Some(Icon::Search),
    );

    let is_bookmarked = st
        .storage
        .lock()
        .map(|s| s.is_bookmarked(&st.current_tab().url))
        .unwrap_or(false);
    ui::button(
        canvas,
        text,
        theme,
        layout.star_btn,
        if is_bookmarked { "★" } else { "☆" },
        None,
        is_bookmarked,
        layout.star_btn.contains(st.pointer.0, st.pointer.1),
    );
    ui::button(
        canvas,
        text,
        theme,
        layout.bookmarks_btn,
        "书签",
        None,
        st.drawer == DrawerMode::Bookmarks,
        layout.bookmarks_btn.contains(st.pointer.0, st.pointer.1),
    );
    ui::button(
        canvas,
        text,
        theme,
        layout.history_btn,
        "历史",
        None,
        st.drawer == DrawerMode::History,
        layout.history_btn.contains(st.pointer.0, st.pointer.1),
    );
    ui::button(
        canvas,
        text,
        theme,
        layout.devtools_btn,
        "{}",
        None,
        st.devtools_open,
        layout.devtools_btn.contains(st.pointer.0, st.pointer.1),
    );

    // 3. Optional Find Bar
    if let Some(fb) = layout.find_bar {
        canvas.fill(fb, theme.panel);
        canvas.fill(
            Rect::new(fb.x, fb.bottom() - 1, fb.w, 1),
            theme.window_border,
        );
        ui::text_input(
            canvas,
            text,
            theme,
            layout.find_input,
            &st.find_query.text,
            "在页面中查找...",
            st.focus == FocusTarget::FindBar,
            Some(Icon::Search),
        );
        let cnt_txt = if st.find_query.text.is_empty() {
            String::new()
        } else if st.find_total > 0 {
            format!("{}/{}", st.find_idx + 1, st.find_total)
        } else {
            String::from("0/0")
        };
        text.draw_right(
            canvas,
            &cnt_txt,
            (layout.find_prev_btn.x - 10) as f32,
            (fb.y + 21) as f32,
            12.0,
            theme.text_muted,
            Face::Mono,
        );
        ui::button(
            canvas,
            text,
            theme,
            layout.find_prev_btn,
            "▲",
            None,
            false,
            layout.find_prev_btn.contains(st.pointer.0, st.pointer.1),
        );
        ui::button(
            canvas,
            text,
            theme,
            layout.find_next_btn,
            "▼",
            None,
            false,
            layout.find_next_btn.contains(st.pointer.0, st.pointer.1),
        );
        ui::button(
            canvas,
            text,
            theme,
            layout.find_close_btn,
            "x",
            None,
            false,
            layout.find_close_btn.contains(st.pointer.0, st.pointer.1),
        );
    }

    // 4. Web Page Viewport + Scrollbar
    let page_bg = if st.dark_mode { 0x111622 } else { 0xffffff };
    canvas.fill(layout.viewport_frame, theme.window);
    if win98 {
        ui::sunken_bevel(canvas, theme, layout.viewport_frame);
    } else {
        canvas.border_1px(layout.viewport_frame, theme.window_border);
    }
    canvas.fill(layout.viewport, page_bg);

    let scroll_x = st.current_tab().scroll_x;
    let scroll_y = st.current_tab().vscroll.offset_i32();
    st.tabs[st.active_tab]
        .page
        .draw(canvas, Some(text), layout.viewport, scroll_x, scroll_y);
    let doc_h = st.tabs[st.active_tab].page.doc_height().max(layout.viewport.h) as f32;
    let vp_h = layout.viewport.h.max(1) as f32;
    st.tabs[st.active_tab].vscroll.draw_v(
        canvas,
        theme,
        layout.scrollbar_track,
        vp_h,
        doc_h,
    );

    // 5. DevTools Panel (Console / Elements / Storage)
    if let Some(dr) = layout.devtools_rect {
        canvas.fill(dr, theme.panel);
        canvas.fill(Rect::new(dr.x, dr.y, dr.w, 1), theme.window_border);
        let tab_names = ["Console", "Elements", "Storage"];
        let dev_kinds = [DevTab::Console, DevTab::Elements, DevTab::Storage];
        for i in 0..3 {
            let active = st.dev_tab == dev_kinds[i];
            ui::button(
                canvas,
                text,
                theme,
                layout.dev_tabs[i],
                tab_names[i],
                None,
                active,
                layout.dev_tabs[i].contains(st.pointer.0, st.pointer.1),
            );
        }
        ui::button(
            canvas,
            text,
            theme,
            layout.dev_close_btn,
            "x",
            None,
            false,
            layout.dev_close_btn.contains(st.pointer.0, st.pointer.1),
        );

        let body = Rect::new(
            dr.x + 8,
            dr.y + 32,
            (dr.w - 16).max(60),
            (dr.h - 68).max(32),
        );
        canvas.fill(body, theme.window);
        canvas.border_1px(body, theme.window_border);

        canvas.with_clip(body.inset(4), |c| {
            let mut y = body.y + 18;
            match st.dev_tab {
                DevTab::Console => {
                    let logs = &st.tabs[st.active_tab].page.host().console_logs;
                    let mut lines: Vec<(i32, &str)> = Vec::new();
                    for entry in logs {
                        for line in entry.message.lines() {
                            lines.push((entry.level, line));
                        }
                    }
                    let max_lines = (body.h / 18).max(1) as usize;
                    let start = lines.len().saturating_sub(max_lines);
                    for &(level, line) in &lines[start..] {
                        let col = if level >= 2 {
                            0xef4444
                        } else if level == 1 {
                            0xf59e0b
                        } else {
                            theme.text
                        };
                        let s = text.truncate(line, (body.w - 14) as f32, 12.0, Face::Mono);
                        text.draw(c, &s, (body.x + 6) as f32, y as f32, 12.0, col, Face::Mono);
                        y += 18;
                    }
                }
                DevTab::Elements => {
                    for line in st.dev_dom_cache.lines().take((body.h / 17).max(1) as usize) {
                        let s = text.truncate(line, (body.w - 14) as f32, 12.0, Face::Mono);
                        text.draw(
                            c,
                            &s,
                            (body.x + 6) as f32,
                            y as f32,
                            12.0,
                            theme.text,
                            Face::Mono,
                        );
                        y += 17;
                    }
                }
                DevTab::Storage => {
                    let url = st.current_tab().url.clone();
                    let origin = crate::storage::origin_for_url(&url);
                    if let Ok(guard) = st.storage.lock() {
                        let ck = guard.cookies_for_url(&url);
                        let c_line = format!("document.cookie: {}", if ck.is_empty() { "(empty)" } else { &ck });
                        let trunc = text.truncate(&c_line, (body.w - 14) as f32, 12.0, Face::Mono);
                        text.draw(c, &trunc, (body.x + 6) as f32, y as f32, 12.0, theme.accent, Face::Mono);
                        y += 20;
                        if let Some(pairs) = guard.local_kv.get(&origin) {
                            for (k, v) in pairs.iter().take(6) {
                                let row = format!("local[{k}] = {v}");
                                let s = text.truncate(&row, (body.w - 14) as f32, 12.0, Face::Mono);
                                text.draw(c, &s, (body.x + 6) as f32, y as f32, 12.0, theme.text, Face::Mono);
                                y += 17;
                            }
                        }
                    }
                }
            }
        });

        ui::text_input(
            canvas,
            text,
            theme,
            layout.dev_repl_rect,
            &st.dev_repl.text,
            "> 输入 JavaScript 表达式并按 Enter 求值...",
            st.focus == FocusTarget::DevRepl,
            None,
        );
    }

    // 6. Status Bar
    canvas.fill(layout.status_bar, theme.panel);
    canvas.fill(
        Rect::new(layout.status_bar.x, layout.status_bar.y, layout.status_bar.w, 1),
        theme.window_border,
    );
    let err_cnt = st
        .tabs[st.active_tab]
        .page
        .host()
        .console_logs
        .iter()
        .filter(|e| e.level >= 2)
        .count();
    let left_status = if st.current_tab().loading {
        format!("正在加载 {} ...", st.current_tab().url)
    } else {
        format!("{}   ({}x{}px)", st.current_tab().url, layout.viewport.w, doc_h as i32)
    };
    let left_s = text.truncate(&left_status, (layout.status_bar.w - 180) as f32, 11.5, Face::Ui);
    text.draw(
        canvas,
        &left_s,
        (layout.status_bar.x + 8) as f32,
        (layout.status_bar.y + 15) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );
    let right_s = format!(
        "JS err: {err_cnt}  |  {}",
        if win98 { "Win98" } else { "SPUI 4.0" }
    );
    text.draw_right(
        canvas,
        &right_s,
        (layout.status_bar.right() - 8) as f32,
        (layout.status_bar.y + 15) as f32,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    // 7. Overlays: Alert Banner, Autocomplete, Bookmarks/History Drawer, <select> Popup
    if let (Some(br), Some((_kind, msg))) = (layout.banner_rect, &st.current_tab().alert_banner) {
        ui::popup_surface(canvas, theme, br, 6);
        let trunc = text.truncate(msg, (br.w - 84) as f32, 12.5, Face::Ui);
        text.draw(
            canvas,
            &trunc,
            (br.x + 12) as f32,
            (br.y + 21) as f32,
            12.5,
            theme.text,
            Face::Ui,
        );
        ui::button(
            canvas,
            text,
            theme,
            layout.banner_close_btn,
            "关闭",
            None,
            true,
            layout.banner_close_btn.contains(st.pointer.0, st.pointer.1),
        );
    }

    if let Some(ar) = layout.autocomplete_rect {
        ui::popup_surface(canvas, theme, ar, 6);
        for (i, (title, url, is_bm)) in st.suggestions.iter().enumerate() {
            let row = Rect::new(ar.x + 4, ar.y + 4 + i as i32 * 28, ar.w - 8, 26);
            let sel = st.suggest_sel == Some(i) || row.contains(st.pointer.0, st.pointer.1);
            if sel {
                canvas.rounded_rect(row, 4, theme.accent_soft, 1.0);
            }
            let prefix = if *is_bm { "★ " } else { "· " };
            let line = format!("{prefix}{title} — {url}");
            let trunc = text.truncate(&line, (row.w - 14) as f32, 12.0, Face::Ui);
            text.draw(
                canvas,
                &trunc,
                (row.x + 8) as f32,
                (row.y + 17) as f32,
                12.0,
                theme.text,
                Face::Ui,
            );
        }
    }

    if let Some(dr) = layout.drawer_rect {
        ui::popup_surface(canvas, theme, dr, 8);
        let heading = match st.drawer {
            DrawerMode::Bookmarks => "书签管理",
            DrawerMode::History => "浏览历史",
            DrawerMode::None => "",
        };
        text.draw(
            canvas,
            heading,
            (dr.x + 12) as f32,
            (dr.y + 22) as f32,
            14.0,
            theme.text,
            Face::Bold,
        );
        let clear_r = Rect::new(dr.right() - 78, dr.y + 6, 70, 22);
        if st.drawer == DrawerMode::History {
            ui::button(
                canvas,
                text,
                theme,
                clear_r,
                "清空历史",
                None,
                false,
                clear_r.contains(st.pointer.0, st.pointer.1),
            );
        }
        let max_rows = ((dr.h - 42) / 34).max(1) as usize;
        for (idx, (title, url, _orig_i)) in st.drawer_items.iter().take(max_rows).enumerate() {
            let row = Rect::new(dr.x + 8, dr.y + 34 + idx as i32 * 34, dr.w - 16, 30);
            if row.contains(st.pointer.0, st.pointer.1) {
                canvas.rounded_rect(row, 4, theme.panel_hover, 0.9);
            }
            let t = text.truncate(title, (row.w - 12) as f32, 12.0, Face::Bold);
            let u = text.truncate(url, (row.w - 12) as f32, 11.0, Face::Mono);
            text.draw(canvas, &t, (row.x + 6) as f32, (row.y + 13) as f32, 12.0, theme.text, Face::Bold);
            text.draw(canvas, &u, (row.x + 6) as f32, (row.y + 26) as f32, 11.0, theme.text_muted, Face::Mono);
        }
    }

    if let Some(popup) = st.current_tab().page.host().pending_select_popup.clone() {
        let pr = select_popup_rect(layout.viewport, &popup);
        ui::popup_surface(canvas, theme, pr, 6);
        for (i, label) in popup.items.iter().enumerate() {
            let row = Rect::new(pr.x + 3, pr.y + 3 + i as i32 * 26, pr.w - 6, 24);
            if row.bottom() > pr.bottom() - 2 {
                break;
            }
            let hov = st.hover_select_idx == Some(i) || i == popup.selected_idx;
            if hov {
                canvas.rounded_rect(row, 4, theme.accent_soft, 1.0);
            }
            let s = text.truncate(label, (row.w - 12) as f32, 12.5, Face::Ui);
            text.draw(canvas, &s, (row.x + 8) as f32, (row.y + 16) as f32, 12.5, theme.text, Face::Ui);
        }
    }
    let _ = icons::draw;
}

fn handle_key(app: &mut App, st: &mut BrowserState, layout: &LayoutRects, key: Key) {
    if key.control() {
        match key.sym {
            0x006c | 0x004c => {
                // Ctrl+L: focus omnibox & select all
                st.focus = FocusTarget::Omnibox;
                st.omnibox.select_all = true;
                st.current_tab_mut().page.set_focused(false);
                st.refresh_suggestions();
                app.needs_redraw = true;
                return;
            }
            0x0074 | 0x0054 => {
                // Ctrl+T: new tab
                st.open_new_tab("space://home", layout.viewport.w, layout.viewport.h, Some(&mut app.text));
                st.focus = FocusTarget::Omnibox;
                st.omnibox.select_all = true;
                app.needs_redraw = true;
                return;
            }
            0x0077 | 0x0057 => {
                // Ctrl+W: close tab
                let idx = st.active_tab;
                st.close_tab(idx, layout.viewport.w, layout.viewport.h, Some(&mut app.text));
                app.needs_redraw = true;
                return;
            }
            0x0066 | 0x0046 => {
                // Ctrl+F: find bar
                st.find_open = true;
                st.focus = FocusTarget::FindBar;
                st.find_query.select_all = true;
                app.needs_redraw = true;
                return;
            }
            0x0072 | 0x0052 => {
                // Ctrl+R: reload
                st.reload_or_stop(Some(&mut app.text));
                app.needs_redraw = true;
                return;
            }
            _ => {}
        }
    }

    match key.sym {
        0xffc2 => {
            // F5 reload
            st.reload_or_stop(Some(&mut app.text));
            app.needs_redraw = true;
            return;
        }
        0xffc9 => {
            // F12 toggle DevTools
            st.devtools_open = !st.devtools_open;
            if st.devtools_open && st.dev_tab == DevTab::Elements {
                st.dev_dom_cache = st.tabs[st.active_tab].page.dump_dom();
            }
            app.needs_redraw = true;
            return;
        }
        0xff1b => {
            // Escape closes overlays / find bar / cancels omnibox edit
            if st.tabs[st.active_tab].page.host().pending_select_popup.is_some() {
                st.tabs[st.active_tab].page.host_mut().pending_select_popup = None;
                app.needs_redraw = true;
                return;
            }
            if st.drawer != DrawerMode::None {
                st.drawer = DrawerMode::None;
                app.needs_redraw = true;
                return;
            }
            if st.focus == FocusTarget::FindBar {
                st.find_open = false;
                st.focus = FocusTarget::Page;
                app.needs_redraw = true;
                return;
            }
            if st.focus == FocusTarget::Omnibox {
                st.sync_omnibox_from_tab();
                st.focus = FocusTarget::Page;
                app.needs_redraw = true;
                return;
            }
        }
        0xff51 if key.alt() => {
            st.go_back(Some(&mut app.text));
            app.needs_redraw = true;
            return;
        }
        0xff53 if key.alt() => {
            st.go_forward(Some(&mut app.text));
            app.needs_redraw = true;
            return;
        }
        _ => {}
    }

    match st.focus {
        FocusTarget::Omnibox => {
            if key.sym == 0xff54 && !st.suggestions.is_empty() {
                let next = st.suggest_sel.map_or(0, |i| (i + 1) % st.suggestions.len());
                st.suggest_sel = Some(next);
                let url = st.suggestions[next].1.clone();
                st.omnibox.set(&url, false);
                app.needs_redraw = true;
                return;
            }
            if key.sym == 0xff52 && !st.suggestions.is_empty() {
                let prev = st.suggest_sel.map_or(st.suggestions.len() - 1, |i| {
                    if i == 0 {
                        st.suggestions.len() - 1
                    } else {
                        i - 1
                    }
                });
                st.suggest_sel = Some(prev);
                let url = st.suggestions[prev].1.clone();
                st.omnibox.set(&url, false);
                app.needs_redraw = true;
                return;
            }
            let (changed, enter) = st.omnibox.handle_key(&key);
            if enter {
                let raw = st.omnibox.text.clone();
                st.suggestions.clear();
                st.focus = FocusTarget::Page;
                st.current_tab_mut().page.set_focused(true);
                st.navigate_tab(st.active_tab, &raw, true, Some(&mut app.text));
                app.needs_redraw = true;
            } else if changed {
                st.refresh_suggestions();
                app.needs_redraw = true;
            }
        }
        FocusTarget::FindBar => {
            let (changed, enter) = st.find_query.handle_key(&key);
            if enter {
                st.run_find(if key.shift() { -1 } else { 1 });
                app.needs_redraw = true;
            } else if changed {
                st.find_idx = 0;
                st.find_total = 0;
                st.run_find(0);
                app.needs_redraw = true;
            }
        }
        FocusTarget::DevRepl => {
            let (changed, enter) = st.dev_repl.handle_key(&key);
            if enter {
                let code = st.dev_repl.text.trim().to_string();
                st.dev_repl.text.clear();
                if !code.is_empty() {
                    let res = st.tabs[st.active_tab].page.eval(&code, Some(&mut app.text));
                    let host = st.tabs[st.active_tab].page.host_mut();
                    host.console_logs.push(crate::ffi::ConsoleEntry {
                        level: 0,
                        message: format!("> {code}\n< {res}"),
                    });
                }
                app.needs_redraw = true;
            } else if changed {
                app.needs_redraw = true;
            }
        }
        FocusTarget::Page => {
            let utf8 = key_to_utf8(&key);
            let handled = st.tabs[st.active_tab].page.key_down(
                key.sym as i32,
                &utf8,
                key.state as i32,
                Some(&mut app.text),
            );
            if !handled {
                let step = (layout.viewport.h as f32 * 0.85).max(80.0);
                match key.sym {
                    0xff52 => {
                        let cur = st.current_tab().vscroll.target();
                        st.current_tab_mut().vscroll.animate_to(cur - 56.0, 0.12);
                    }
                    0xff54 => {
                        let cur = st.current_tab().vscroll.target();
                        st.current_tab_mut().vscroll.animate_to(cur + 56.0, 0.12);
                    }
                    0xff55 => {
                        let cur = st.current_tab().vscroll.target();
                        st.current_tab_mut().vscroll.animate_to(cur - step, 0.14);
                    }
                    0xff56 | 0x0020 => {
                        let cur = st.current_tab().vscroll.target();
                        st.current_tab_mut().vscroll.animate_to(cur + step, 0.14);
                    }
                    0xff50 => st.current_tab_mut().vscroll.animate_to(0.0, 0.16),
                    0xff57 => {
                        let max = st.current_tab().vscroll.max_offset();
                        st.current_tab_mut().vscroll.animate_to(max, 0.16);
                    }
                    _ => {}
                }
            }
            app.needs_redraw = true;
        }
    }
}

fn handle_button(
    app: &mut App,
    st: &mut BrowserState,
    layout: &LayoutRects,
    x: i32,
    y: i32,
    button: u8,
    down: bool,
) {
    if !down {
        if st.tabs[st.active_tab].vscroll.handle_button(x, y, button, false) {
            app.needs_redraw = true;
        }
        if layout.viewport.contains(x, y) {
            let lx = x - layout.viewport.x;
            let ly = y - layout.viewport.y;
            let sx = st.current_tab().scroll_x;
            let sy = st.current_tab().vscroll.offset_i32();
            st.tabs[st.active_tab].page.mouse_up(
                lx,
                ly,
                sx,
                sy,
                button as i32,
                Some(&mut app.text),
            );
            app.needs_redraw = true;
        }
        return;
    }

    if button != 1 {
        return;
    }

    // Check active <select> popup first
    if let Some(popup) = st.tabs[st.active_tab].page.host().pending_select_popup.clone() {
        let pr = select_popup_rect(layout.viewport, &popup);
        if pr.contains(x, y) {
            let idx = ((y - (pr.y + 3)) / 26).max(0) as usize;
            if idx < popup.items.len() {
                st.tabs[st.active_tab]
                    .page
                    .select_option(popup.node_id, idx, Some(&mut app.text));
            }
        }
        st.tabs[st.active_tab].page.host_mut().pending_select_popup = None;
        app.needs_redraw = true;
        return;
    }

    // Check autocomplete dropdown
    if let Some(ar) = layout.autocomplete_rect {
        if ar.contains(x, y) {
            let idx = ((y - (ar.y + 4)) / 28).max(0) as usize;
            if let Some((_, url, _)) = st.suggestions.get(idx).cloned() {
                st.suggestions.clear();
                st.focus = FocusTarget::Page;
                st.navigate_tab(st.active_tab, &url, true, Some(&mut app.text));
                app.needs_redraw = true;
                return;
            }
        }
    }

    // Check Bookmarks/History drawer
    if let Some(dr) = layout.drawer_rect {
        if dr.contains(x, y) {
            if st.drawer == DrawerMode::History {
                let clear_r = Rect::new(dr.right() - 78, dr.y + 6, 70, 22);
                if clear_r.contains(x, y) {
                    if let Ok(mut guard) = st.storage.lock() {
                        guard.clear_history();
                    }
                    st.refresh_drawer();
                    app.needs_redraw = true;
                    return;
                }
            }
            if y >= dr.y + 34 {
                let idx = ((y - (dr.y + 34)) / 34).max(0) as usize;
                if let Some((_, url, _)) = st.drawer_items.get(idx).cloned() {
                    st.drawer = DrawerMode::None;
                    st.focus = FocusTarget::Page;
                    st.navigate_tab(st.active_tab, &url, true, Some(&mut app.text));
                    app.needs_redraw = true;
                    return;
                }
            }
            return;
        }
        st.drawer = DrawerMode::None;
    }

    // Dismiss alert banner
    if layout.banner_rect.is_some() && layout.banner_close_btn.contains(x, y) {
        st.tabs[st.active_tab].alert_banner = None;
        app.needs_redraw = true;
        return;
    }

    // Tab strip clicks
    for (i, th) in layout.tabs.iter().enumerate() {
        if th.close_rect.contains(x, y) {
            st.close_tab(i, layout.viewport.w, layout.viewport.h, Some(&mut app.text));
            app.needs_redraw = true;
            return;
        }
        if th.tab_rect.contains(x, y) {
            st.active_tab = i;
            st.sync_omnibox_from_tab();
            st.tabs[i].page.resize(layout.viewport.w, layout.viewport.h, Some(&mut app.text));
            app.needs_redraw = true;
            return;
        }
    }
    if layout.new_tab_btn.contains(x, y) {
        st.open_new_tab("space://home", layout.viewport.w, layout.viewport.h, Some(&mut app.text));
        st.focus = FocusTarget::Omnibox;
        st.omnibox.select_all = true;
        app.needs_redraw = true;
        return;
    }

    // Toolbar clicks
    if layout.back_btn.contains(x, y) {
        st.go_back(Some(&mut app.text));
        app.needs_redraw = true;
        return;
    }
    if layout.fwd_btn.contains(x, y) {
        st.go_forward(Some(&mut app.text));
        app.needs_redraw = true;
        return;
    }
    if layout.reload_btn.contains(x, y) {
        st.reload_or_stop(Some(&mut app.text));
        app.needs_redraw = true;
        return;
    }
    if layout.home_btn.contains(x, y) {
        st.navigate_tab(st.active_tab, "space://home", true, Some(&mut app.text));
        app.needs_redraw = true;
        return;
    }
    if layout.star_btn.contains(x, y) {
        let u = st.current_tab().url.clone();
        let t = st.current_tab().display_title().to_string();
        if let Ok(mut guard) = st.storage.lock() {
            guard.toggle_bookmark(&t, &u);
        }
        app.needs_redraw = true;
        return;
    }
    if layout.bookmarks_btn.contains(x, y) {
        st.drawer = if st.drawer == DrawerMode::Bookmarks {
            DrawerMode::None
        } else {
            DrawerMode::Bookmarks
        };
        st.refresh_drawer();
        app.needs_redraw = true;
        return;
    }
    if layout.history_btn.contains(x, y) {
        st.drawer = if st.drawer == DrawerMode::History {
            DrawerMode::None
        } else {
            DrawerMode::History
        };
        st.refresh_drawer();
        app.needs_redraw = true;
        return;
    }
    if layout.devtools_btn.contains(x, y) {
        st.devtools_open = !st.devtools_open;
        if st.devtools_open && st.dev_tab == DevTab::Elements {
            st.dev_dom_cache = st.tabs[st.active_tab].page.dump_dom();
        }
        app.needs_redraw = true;
        return;
    }
    if layout.omnibox.contains(x, y) {
        st.focus = FocusTarget::Omnibox;
        st.omnibox.select_all = true;
        st.suggestions.clear();
        st.current_tab_mut().page.set_focused(false);
        app.needs_redraw = true;
        return;
    }

    // Find bar clicks
    if layout.find_bar.is_some() {
        if layout.find_input.contains(x, y) {
            st.focus = FocusTarget::FindBar;
            app.needs_redraw = true;
            return;
        }
        if layout.find_prev_btn.contains(x, y) {
            st.run_find(-1);
            app.needs_redraw = true;
            return;
        }
        if layout.find_next_btn.contains(x, y) {
            st.run_find(1);
            app.needs_redraw = true;
            return;
        }
        if layout.find_close_btn.contains(x, y) {
            st.find_open = false;
            st.focus = FocusTarget::Page;
            app.needs_redraw = true;
            return;
        }
    }

    // DevTools clicks
    if let Some(dr) = layout.devtools_rect {
        if dr.contains(x, y) {
            if layout.dev_close_btn.contains(x, y) {
                st.devtools_open = false;
                st.focus = FocusTarget::Page;
                app.needs_redraw = true;
                return;
            }
            let kinds = [DevTab::Console, DevTab::Elements, DevTab::Storage];
            for i in 0..3 {
                if layout.dev_tabs[i].contains(x, y) {
                    st.dev_tab = kinds[i];
                    if st.dev_tab == DevTab::Elements {
                        st.dev_dom_cache = st.tabs[st.active_tab].page.dump_dom();
                    }
                    app.needs_redraw = true;
                    return;
                }
            }
            if layout.dev_repl_rect.contains(x, y) {
                st.focus = FocusTarget::DevRepl;
                app.needs_redraw = true;
                return;
            }
            return;
        }
    }

    // Scrollbar drag/page
    if st.tabs[st.active_tab].vscroll.handle_button(x, y, button, true) {
        app.needs_redraw = true;
        return;
    }

    // Web page viewport click
    if layout.viewport.contains(x, y) {
        st.focus = FocusTarget::Page;
        st.suggestions.clear();
        st.current_tab_mut().page.set_focused(true);
        let lx = x - layout.viewport.x;
        let ly = y - layout.viewport.y;
        let sx = st.current_tab().scroll_x;
        let sy = st.current_tab().vscroll.offset_i32();
        st.tabs[st.active_tab]
            .page
            .mouse_down(lx, ly, sx, sy, button as i32, Some(&mut app.text));
        app.needs_redraw = true;
    }
}

pub fn run_browser(initial_url: Option<String>) -> Result<(), String> {
    let mut app = App::new("Space 浏览器", Icon::Browser, 1080, 720)
        .map_err(|e| format!("failed to initialize SPUI window: {e}"))?;
    let storage = Arc::new(Mutex::new(BrowserStorage::load()));
    let fetch_pool = Arc::new(FetchPool::new(Arc::clone(&storage)));

    let start_url = initial_url
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| String::from("space://home"));
    let content = app.content();
    let init_layout = compute_layout(content, 1, false, false, false, 0, DrawerMode::None);
    let mut st = BrowserState::new(
        &start_url,
        storage,
        fetch_pool,
        init_layout.viewport.w,
        init_layout.viewport.h,
        &app.theme,
        Some(&mut app.text),
    );
    let mut last_vp_size = (init_layout.viewport.w, init_layout.viewport.h);

    loop {
        if app.close {
            break;
        }
        let any_loading = st.tabs.iter().any(|t| t.loading) || st.current_tab().vscroll.is_animating();
        let timeout_ms = if any_loading { 16 } else { 40 };
        let (events, _fetch_ready) = app
            .pump_with_fd(Some(st.fetch_pool.wakeup_fd()), timeout_ms)
            .map_err(|e| format!("event loop error: {e}"))?;

        let content = app.content();
        let mut layout = compute_layout(
            content,
            st.tabs.len(),
            st.find_open,
            st.devtools_open,
            st.current_tab().alert_banner.is_some(),
            st.suggestions.len(),
            st.drawer,
        );
        if (layout.viewport.w, layout.viewport.h) != last_vp_size {
            last_vp_size = (layout.viewport.w, layout.viewport.h);
            st.resize_all_tabs(layout.viewport.w, layout.viewport.h, &mut app.text);
        }

        for ev in events {
            match ev {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose => app.needs_redraw = true,
                UiEvent::ConfigChanged => {
                    st.sync_theme(&mut app, layout.viewport.w, layout.viewport.h);
                    app.needs_redraw = true;
                }
                UiEvent::Configure { .. } => {
                    let c = app.content();
                    layout = compute_layout(
                        c,
                        st.tabs.len(),
                        st.find_open,
                        st.devtools_open,
                        st.current_tab().alert_banner.is_some(),
                        st.suggestions.len(),
                        st.drawer,
                    );
                    if (layout.viewport.w, layout.viewport.h) != last_vp_size {
                        last_vp_size = (layout.viewport.w, layout.viewport.h);
                        st.resize_all_tabs(layout.viewport.w, layout.viewport.h, &mut app.text);
                    }
                    app.needs_redraw = true;
                }
                UiEvent::Motion { x, y, .. } => {
                    st.pointer = (x, y);
                    if st.tabs[st.active_tab].vscroll.handle_motion(x, y) {
                        app.needs_redraw = true;
                    }
                    if let Some(popup) = st.tabs[st.active_tab].page.host().pending_select_popup.clone() {
                        let pr = select_popup_rect(layout.viewport, &popup);
                        let next_hov = if pr.contains(x, y) {
                            let idx = ((y - (pr.y + 3)) / 26).max(0) as usize;
                            (idx < popup.items.len()).then_some(idx)
                        } else {
                            None
                        };
                        if next_hov != st.hover_select_idx {
                            st.hover_select_idx = next_hov;
                            app.needs_redraw = true;
                        }
                    } else if layout.viewport.contains(x, y) {
                        let lx = x - layout.viewport.x;
                        let ly = y - layout.viewport.y;
                        let sx = st.current_tab().scroll_x;
                        let sy = st.current_tab().vscroll.offset_i32();
                        st.tabs[st.active_tab]
                            .page
                            .mouse_move(lx, ly, sx, sy, Some(&mut app.text));
                    } else {
                        st.tabs[st.active_tab].page.mouse_leave(Some(&mut app.text));
                    }
                    app.needs_redraw = true;
                }
                UiEvent::Wheel { x, y, delta } => {
                    if layout.viewport_frame.contains(x, y)
                        && st.tabs[st.active_tab].vscroll.wheel(delta as f32 * 24.0)
                    {
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x,
                    y,
                    button,
                    down,
                    ..
                } => {
                    st.pointer = (x, y);
                    handle_button(&mut app, &mut st, &layout, x, y, button, down);
                }
                UiEvent::Key(key) => {
                    handle_key(&mut app, &mut st, &layout, key);
                }
            }
        }
        if app.close {
            break;
        }

        if st.process_async_fetches(&mut app.text) {
            app.needs_redraw = true;
        }
        if st.tabs[st.active_tab].page.tick(Some(&mut app.text)) {
            app.needs_redraw = true;
        }
        if st.flush_host_events(&mut app, layout.viewport) {
            app.needs_redraw = true;
        }
        if st.current_tab().vscroll.is_animating() || st.tabs[st.active_tab].page.host().dirty_repaint {
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            let final_layout = compute_layout(
                app.content(),
                st.tabs.len(),
                st.find_open,
                st.devtools_open,
                st.current_tab().alert_banner.is_some(),
                st.suggestions.len(),
                st.drawer,
            );
            app.draw_chrome();
            render_browser(&mut app, &mut st, &final_layout);
            app.present().map_err(|e| format!("present error: {e}"))?;
        }
    }
    Ok(())
}
