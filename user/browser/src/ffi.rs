use crate::net::{
    decode_image_sprite, parse_headers_json, prefetch_subresources, resolve_url_ref,
    serialize_headers_json, sync_fetch, sync_fetch_with_kind, AsyncFetchRequest, FetchKind,
    FetchPool, FetchResponse,
};
use crate::storage::{origin_for_url, BrowserStorage};
use spui_ui::draw::{blend_u8, Canvas, Rect, Sprite};
use spui_ui::text::{Face, TextRenderer};
use spui_ui::x11::CursorKind;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const SPACE_BORDER_NONE: c_int = 0;
pub const SPACE_BORDER_SOLID: c_int = 1;
pub const SPACE_BORDER_DASHED: c_int = 2;
pub const SPACE_BORDER_DOTTED: c_int = 3;
pub const SPACE_BORDER_DOUBLE: c_int = 4;

const TEXT_DECORATION_UNDERLINE: c_int = 0x01;
const TEXT_DECORATION_OVERLINE: c_int = 0x02;
const TEXT_DECORATION_LINE_THROUGH: c_int = 0x04;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SpaceBorderSide {
    pub width: c_int,
    pub color: u32,
    pub style: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SpaceBorders {
    pub x: c_int,
    pub y: c_int,
    pub w: c_int,
    pub h: c_int,
    pub left: SpaceBorderSide,
    pub top: SpaceBorderSide,
    pub right: SpaceBorderSide,
    pub bottom: SpaceBorderSide,
    pub radius_tl_x: c_int,
    pub radius_tl_y: c_int,
    pub radius_tr_x: c_int,
    pub radius_tr_y: c_int,
    pub radius_br_x: c_int,
    pub radius_br_y: c_int,
    pub radius_bl_x: c_int,
    pub radius_bl_y: c_int,
}

#[repr(C)]
pub struct SpacePage {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct SpaceHostCallbacks {
    pub user_data: *mut c_void,

    pub create_font: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            family: *const c_char,
            size_px: c_int,
            weight: c_int,
            italic: c_int,
            decoration: c_int,
            out_height: *mut c_int,
            out_ascent: *mut c_int,
            out_descent: *mut c_int,
            out_x_height: *mut c_int,
        ) -> u64,
    >,
    pub delete_font: Option<unsafe extern "C" fn(ud: *mut c_void, font_handle: u64)>,
    pub text_width:
        Option<unsafe extern "C" fn(ud: *mut c_void, text: *const c_char, font_handle: u64) -> i32>,
    pub draw_text: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            text: *const c_char,
            font_handle: u64,
            argb: u32,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            decoration: c_int,
        ),
    >,

    pub fill_rect: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            argb: u32,
            radii8: *const c_int,
        ),
    >,
    pub draw_linear_gradient: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            x0: f32,
            y0: f32,
            x1: f32,
            y1: f32,
            colors: *const u32,
            stops: *const f32,
            stop_count: c_int,
            radii8: *const c_int,
        ),
    >,
    pub draw_radial_gradient: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            cx: f32,
            cy: f32,
            rx: f32,
            ry: f32,
            colors: *const u32,
            stops: *const f32,
            stop_count: c_int,
            radii8: *const c_int,
        ),
    >,
    pub draw_borders: Option<unsafe extern "C" fn(ud: *mut c_void, borders: *const SpaceBorders)>,
    pub draw_list_marker: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            marker_type: c_int,
            argb: u32,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            index: c_int,
        ),
    >,
    pub set_clip: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            radii8: *const c_int,
        ),
    >,
    pub del_clip: Option<unsafe extern "C" fn(ud: *mut c_void)>,
    pub draw_rgba_blit: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            pixels_argb: *const u32,
            src_w: c_int,
            src_h: c_int,
            dst_x: c_int,
            dst_y: c_int,
            dst_w: c_int,
            dst_h: c_int,
        ),
    >,

    pub load_image: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            src: *const c_char,
            baseurl: *const c_char,
            redraw_on_ready: c_int,
        ),
    >,
    pub get_image_size: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            src: *const c_char,
            baseurl: *const c_char,
            out_w: *mut c_int,
            out_h: *mut c_int,
        ),
    >,
    pub draw_image: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            src: *const c_char,
            baseurl: *const c_char,
            clip_x: c_int,
            clip_y: c_int,
            clip_w: c_int,
            clip_h: c_int,
            orig_x: c_int,
            orig_y: c_int,
            orig_w: c_int,
            orig_h: c_int,
            pos_x: c_int,
            pos_y: c_int,
            pos_w: c_int,
            pos_h: c_int,
            repeat_mode: c_int,
        ),
    >,

    pub fetch_sync: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            method: *const c_char,
            url: *const c_char,
            headers_json: *const c_char,
            body: *const c_char,
            body_len: c_int,
            out_status: *mut c_int,
            out_body: *mut *mut c_char,
            out_len: *mut c_int,
            out_resolved_url: *mut *mut c_char,
            out_headers_json: *mut *mut c_char,
        ),
    >,
    pub fetch_async: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            method: *const c_char,
            url: *const c_char,
            headers_json: *const c_char,
            body: *const c_char,
            body_len: c_int,
        ) -> u64,
    >,
    pub free_host_buf: Option<unsafe extern "C" fn(ud: *mut c_void, ptr: *mut c_char)>,

    pub set_title: Option<unsafe extern "C" fn(ud: *mut c_void, title: *const c_char)>,
    pub set_base_url: Option<unsafe extern "C" fn(ud: *mut c_void, base_url: *const c_char)>,
    pub set_cursor: Option<unsafe extern "C" fn(ud: *mut c_void, cursor: *const c_char)>,
    pub on_navigate:
        Option<unsafe extern "C" fn(ud: *mut c_void, url: *const c_char, replace: c_int)>,
    pub on_history_op: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            op: c_int,
            delta: c_int,
            url: *const c_char,
            title: *const c_char,
        ),
    >,
    pub on_console:
        Option<unsafe extern "C" fn(ud: *mut c_void, level: c_int, msg: *const c_char)>,
    pub on_alert: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            kind: c_int,
            msg: *const c_char,
            def: *const c_char,
            out_reply: *mut *mut c_char,
        ),
    >,
    pub on_select_popup: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            node_id: u64,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            items_tsv: *const c_char,
            selected_idx: c_int,
        ),
    >,
    pub on_dirty: Option<unsafe extern "C" fn(ud: *mut c_void, relayout: c_int, repaint: c_int)>,

    pub storage_get:
        Option<unsafe extern "C" fn(ud: *mut c_void, is_session: c_int, key: *const c_char) -> *mut c_char>,
    pub storage_set: Option<
        unsafe extern "C" fn(
            ud: *mut c_void,
            is_session: c_int,
            key: *const c_char,
            val: *const c_char,
        ),
    >,
    pub storage_remove:
        Option<unsafe extern "C" fn(ud: *mut c_void, is_session: c_int, key: *const c_char)>,
    pub storage_clear: Option<unsafe extern "C" fn(ud: *mut c_void, is_session: c_int)>,
    pub storage_len: Option<unsafe extern "C" fn(ud: *mut c_void, is_session: c_int) -> c_int>,
    pub storage_key:
        Option<unsafe extern "C" fn(ud: *mut c_void, is_session: c_int, idx: c_int) -> *mut c_char>,

    pub cookies_get: Option<unsafe extern "C" fn(ud: *mut c_void) -> *mut c_char>,
    pub cookies_set: Option<unsafe extern "C" fn(ud: *mut c_void, set_cookie_kv: *const c_char)>,
    pub now_ms: Option<unsafe extern "C" fn(ud: *mut c_void) -> f64>,
    pub prefetch_urls: Option<unsafe extern "C" fn(ud: *mut c_void, urls_nl: *const c_char)>,
}

extern "C" {
    pub fn space_page_create(
        cb: *const SpaceHostCallbacks,
        viewport_w: c_int,
        viewport_h: c_int,
        dark_mode: c_int,
        is_win98: c_int,
    ) -> *mut SpacePage;
    pub fn space_page_destroy(page: *mut SpacePage);
    pub fn space_page_set_theme(page: *mut SpacePage, dark_mode: c_int, is_win98: c_int);
    pub fn space_page_load_html(
        page: *mut SpacePage,
        html: *const c_char,
        html_len: c_int,
        url: *const c_char,
    );
    pub fn space_page_resize(page: *mut SpacePage, viewport_w: c_int, viewport_h: c_int);
    pub fn space_page_relayout(page: *mut SpacePage, force: c_int) -> c_int;
    pub fn space_page_doc_width(page: *mut SpacePage) -> c_int;
    pub fn space_page_doc_height(page: *mut SpacePage) -> c_int;
    pub fn space_page_draw(
        page: *mut SpacePage,
        scroll_x: c_int,
        scroll_y: c_int,
        clip_w: c_int,
        clip_h: c_int,
    );
    pub fn space_page_mouse_move(
        page: *mut SpacePage,
        x: c_int,
        y: c_int,
        scroll_x: c_int,
        scroll_y: c_int,
    );
    pub fn space_page_mouse_down(
        page: *mut SpacePage,
        x: c_int,
        y: c_int,
        scroll_x: c_int,
        scroll_y: c_int,
        button: c_int,
    );
    pub fn space_page_mouse_up(
        page: *mut SpacePage,
        x: c_int,
        y: c_int,
        scroll_x: c_int,
        scroll_y: c_int,
        button: c_int,
    );
    pub fn space_page_mouse_leave(page: *mut SpacePage);
    pub fn space_page_key_down(
        page: *mut SpacePage,
        key_code: c_int,
        utf8_text: *const c_char,
        mods: c_int,
    ) -> c_int;
    pub fn space_page_select_option(page: *mut SpacePage, node_id: u64, selected_index: c_int);
    pub fn space_page_resolve_fetch(
        page: *mut SpacePage,
        req_id: u64,
        status: c_int,
        resolved_url: *const c_char,
        headers_json: *const c_char,
        body: *const c_char,
        body_len: c_int,
        err: *const c_char,
    );
    pub fn space_page_tick(page: *mut SpacePage, now_ms: f64) -> c_int;
    pub fn space_page_eval(
        page: *mut SpacePage,
        code: *const c_char,
        code_len: c_int,
        out_buf: *mut c_char,
        out_cap: c_int,
    ) -> c_int;
    pub fn space_page_dump_dom(
        page: *mut SpacePage,
        out_buf: *mut c_char,
        out_cap: c_int,
    ) -> c_int;
    pub fn space_page_find_text(
        page: *mut SpacePage,
        query: *const c_char,
        occurrence_idx: c_int,
        out_y: *mut c_int,
    ) -> c_int;
    pub fn space_page_set_focused(page: *mut SpacePage, focused: c_int);
}

#[derive(Clone, Copy, Debug)]
struct FontEntry {
    face: Face,
    size_px: i32,
    ascent: i32,
    height: i32,
}

#[derive(Clone)]
enum ImageState {
    Loading,
    Ready(Sprite),
    Failed,
}

#[derive(Clone, Debug)]
pub struct ConsoleEntry {
    pub level: i32,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct AlertRequest {
    pub kind: i32, // 0=alert, 1=confirm, 2=prompt
    pub message: String,
    pub default_value: String,
}

#[derive(Clone, Debug)]
pub struct SelectPopupRequest {
    pub node_id: u64,
    pub viewport_rect: Rect,
    pub items: Vec<String>,
    pub selected_idx: usize,
}

#[derive(Clone, Debug)]
pub enum HistoryCommand {
    Push { url: String, title: String },
    Replace { url: String, title: String },
    Go(i32),
}

struct PaintBinding {
    canvas: *mut Canvas,
    text: *mut TextRenderer,
    origin_x: i32,
    origin_y: i32,
    clip_stack: Vec<Rect>,
}

pub struct HostState {
    pub tab_id: u64,
    pub nav_gen: u64,
    pub storage: Arc<Mutex<BrowserStorage>>,
    pub fetch_pool: Option<Arc<FetchPool>>,
    pub dark_mode: bool,
    pub is_win98: bool,
    pub page_url: String,
    pub base_url: String,
    pub title: String,
    pub cursor: CursorKind,

    start_instant: Instant,
    next_font_id: u64,
    fonts: HashMap<u64, FontEntry>,
    images: HashMap<String, ImageState>,
    scaled_images: HashMap<(String, u32, u32), Sprite>,
    next_req_id: u64,
    active_image_reqs: HashMap<u64, String>,

    paint: Option<PaintBinding>,
    measure_text_ptr: *mut TextRenderer,

    pub pending_navigations: Vec<(String, bool)>,
    pub pending_history_ops: Vec<HistoryCommand>,
    pub pending_select_popup: Option<SelectPopupRequest>,
    pub pending_alerts: Vec<AlertRequest>,
    pub console_logs: Vec<ConsoleEntry>,
    pub dirty_relayout: bool,
    pub dirty_repaint: bool,
    pub next_dialog_reply: Option<Option<String>>,
}

impl HostState {
    pub fn new(
        tab_id: u64,
        storage: Arc<Mutex<BrowserStorage>>,
        fetch_pool: Option<Arc<FetchPool>>,
        dark_mode: bool,
        is_win98: bool,
    ) -> Self {
        Self {
            tab_id,
            nav_gen: 0,
            storage,
            fetch_pool,
            dark_mode,
            is_win98,
            page_url: String::from("about:blank"),
            base_url: String::from("about:blank"),
            title: String::new(),
            cursor: CursorKind::Default,
            start_instant: Instant::now(),
            next_font_id: 1,
            fonts: HashMap::new(),
            images: HashMap::new(),
            scaled_images: HashMap::new(),
            next_req_id: 1,
            active_image_reqs: HashMap::new(),
            paint: None,
            measure_text_ptr: core::ptr::null_mut(),
            pending_navigations: Vec::new(),
            pending_history_ops: Vec::new(),
            pending_select_popup: None,
            pending_alerts: Vec::new(),
            console_logs: Vec::new(),
            dirty_relayout: false,
            dirty_repaint: false,
            next_dialog_reply: None,
        }
    }

    fn alloc_c_bytes(bytes: &[u8]) -> *mut c_char {
        unsafe {
            let ptr = libc::malloc(bytes.len() + 1) as *mut u8;
            if ptr.is_null() {
                return core::ptr::null_mut();
            }
            if !bytes.is_empty() {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            }
            *ptr.add(bytes.len()) = 0;
            ptr as *mut c_char
        }
    }

    fn alloc_c_str(s: &str) -> *mut c_char {
        Self::alloc_c_bytes(s.as_bytes())
    }

    fn effective_origin(&self) -> String {
        let origin = origin_for_url(&self.page_url);
        if origin.is_empty() {
            String::from("local://page")
        } else {
            origin
        }
    }

    fn with_text<R>(&mut self, text: Option<&mut TextRenderer>, f: impl FnOnce() -> R) -> R {
        let prev = self.measure_text_ptr;
        self.measure_text_ptr = match text {
            Some(t) => t as *mut TextRenderer,
            None => core::ptr::null_mut(),
        };
        let out = f();
        self.measure_text_ptr = prev;
        out
    }

    fn cached_scaled_sprite(&mut self, key: &str, w: u32, h: u32) -> Option<&Sprite> {
        let src = match self.images.get(key) {
            Some(ImageState::Ready(s)) => s,
            _ => return None,
        };
        if w == 0 || h == 0 || (src.width == w && src.height == h) {
            return Some(src);
        }
        let cache_key = (key.to_string(), w, h);
        if !self.scaled_images.contains_key(&cache_key) {
            if self.scaled_images.len() >= 64 {
                self.scaled_images.clear();
            }
            let scaled = src.scale_bilinear(w, h);
            self.scaled_images.insert(cache_key.clone(), scaled);
        }
        self.scaled_images.get(&cache_key)
    }

    fn measure_string(&mut self, text: &str, font: FontEntry) -> i32 {
        if !self.measure_text_ptr.is_null() {
            let tr = unsafe { &mut *self.measure_text_ptr };
            tr.measure(text, font.size_px as f32, font.face).ceil() as i32
        } else {
            let mut acc = 0.0f32;
            for ch in text.chars() {
                let w = if ch.is_ascii() {
                    if font.face == Face::Mono {
                        0.60
                    } else if ch == 'i' || ch == 'l' || ch == '!' || ch == '.' {
                        0.32
                    } else {
                        0.55
                    }
                } else {
                    1.0
                };
                acc += font.size_px as f32 * w;
            }
            acc.ceil() as i32
        }
    }
}

unsafe fn host_mut<'a>(ud: *mut c_void) -> &'a mut HostState {
    &mut *(ud as *mut HostState)
}

unsafe fn read_c_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr).to_string_lossy().into_owned()
}

fn read_radii8(ptr: *const c_int) -> [i32; 8] {
    if ptr.is_null() {
        return [0; 8];
    }
    let slice = unsafe { core::slice::from_raw_parts(ptr, 8) };
    let mut out = [0i32; 8];
    out.copy_from_slice(slice);
    out
}

fn max_radius(radii: &[i32; 8]) -> i32 {
    radii.iter().copied().max().unwrap_or(0).max(0)
}

fn point_inside_radii(px: f32, py: f32, x0: f32, y0: f32, w: f32, h: f32, r: &[i32; 8]) -> bool {
    let lx = px - x0;
    let ly = py - y0;
    if lx < 0.0 || ly < 0.0 || lx >= w || ly >= h {
        return false;
    }
    let check_corner = |dx: f32, dy: f32, rx: i32, ry: i32| -> bool {
        if rx <= 0 || ry <= 0 {
            return true;
        }
        let fx = dx / (rx as f32);
        let fy = dy / (ry as f32);
        fx * fx + fy * fy <= 1.0
    };
    if (lx as i32) < r[0] && (ly as i32) < r[1] {
        return check_corner(r[0] as f32 - lx, r[1] as f32 - ly, r[0], r[1]);
    }
    if lx > w - r[2] as f32 && (ly as i32) < r[3] {
        return check_corner(lx - (w - r[2] as f32), r[3] as f32 - ly, r[2], r[3]);
    }
    if lx > w - r[4] as f32 && ly > h - r[5] as f32 {
        return check_corner(lx - (w - r[4] as f32), ly - (h - r[5] as f32), r[4], r[5]);
    }
    if (lx as i32) < r[6] && ly > h - r[7] as f32 {
        return check_corner(r[6] as f32 - lx, ly - (h - r[7] as f32), r[6], r[7]);
    }
    true
}

fn interpolate_stops(colors: &[u32], stops: &[f32], t_raw: f32) -> (u32, u8) {
    if colors.is_empty() {
        return (0, 0);
    }
    if colors.len() == 1 || stops.len() != colors.len() {
        let c = colors[0];
        return (c & 0x00FF_FFFF, ((c >> 24) & 0xFF) as u8);
    }
    let t = t_raw.clamp(0.0, 1.0);
    if t <= stops[0] {
        let c = colors[0];
        return (c & 0x00FF_FFFF, ((c >> 24) & 0xFF) as u8);
    }
    for i in 0..colors.len() - 1 {
        let s0 = stops[i];
        let s1 = stops[i + 1];
        if t <= s1 || i == colors.len() - 2 {
            let span = (s1 - s0).max(1e-6);
            let k = ((t - s0) / span).clamp(0.0, 1.0);
            let c0 = colors[i];
            let c1 = colors[i + 1];
            let lerp = |shift: u32| -> u32 {
                let a = ((c0 >> shift) & 0xFF) as f32;
                let b = ((c1 >> shift) & 0xFF) as f32;
                (a + (b - a) * k).round().clamp(0.0, 255.0) as u32
            };
            let a = lerp(24) as u8;
            let rgb = (lerp(16) << 16) | (lerp(8) << 8) | lerp(0);
            return (rgb, a);
        }
    }
    let last = *colors.last().unwrap();
    (last & 0x00FF_FFFF, ((last >> 24) & 0xFF) as u8)
}

unsafe extern "C" fn cb_create_font(
    ud: *mut c_void,
    family: *const c_char,
    size_px: c_int,
    weight: c_int,
    _italic: c_int,
    _decoration: c_int,
    out_height: *mut c_int,
    out_ascent: *mut c_int,
    out_descent: *mut c_int,
    out_x_height: *mut c_int,
) -> u64 {
    let host = host_mut(ud);
    let fam = read_c_str(family).to_ascii_lowercase();
    let sz = size_px.max(6);
    let face = if fam.contains("mono")
        || fam.contains("courier")
        || fam.contains("consolas")
        || fam.contains("menlo")
        || fam.contains("code")
    {
        Face::Mono
    } else if weight >= 600 {
        Face::Bold
    } else {
        Face::Ui
    };
    let ascent = ((sz as f32) * 0.80).round().max(4.0) as i32;
    let descent = ((sz as f32) * 0.24).round().max(2.0) as i32;
    let height = ascent + descent;
    let x_height = ((sz as f32) * 0.54).round().max(3.0) as i32;

    if !out_height.is_null() {
        *out_height = height;
    }
    if !out_ascent.is_null() {
        *out_ascent = ascent;
    }
    if !out_descent.is_null() {
        *out_descent = descent;
    }
    if !out_x_height.is_null() {
        *out_x_height = x_height;
    }

    let id = host.next_font_id;
    host.next_font_id += 1;
    host.fonts.insert(
        id,
        FontEntry {
            face,
            size_px: sz,
            ascent,
            height,
        },
    );
    id
}

unsafe extern "C" fn cb_delete_font(ud: *mut c_void, font_handle: u64) {
    let host = host_mut(ud);
    host.fonts.remove(&font_handle);
}

unsafe extern "C" fn cb_text_width(ud: *mut c_void, text: *const c_char, font_handle: u64) -> i32 {
    let host = host_mut(ud);
    let s = read_c_str(text);
    if s.is_empty() {
        return 0;
    }
    let entry = host.fonts.get(&font_handle).copied().unwrap_or(FontEntry {
        face: Face::Ui,
        size_px: 16,
        ascent: 13,
        height: 17,
    });
    host.measure_string(&s, entry)
}

unsafe extern "C" fn cb_draw_text(
    ud: *mut c_void,
    text: *const c_char,
    font_handle: u64,
    argb: u32,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    decoration: c_int,
) {
    let host = host_mut(ud);
    let alpha = ((argb >> 24) & 0xFF) as u8;
    if alpha == 0 {
        return;
    }
    let entry = host.fonts.get(&font_handle).copied().unwrap_or(FontEntry {
        face: Face::Ui,
        size_px: 16,
        ascent: 13,
        height: 17,
    });
    let s = read_c_str(text);
    if s.is_empty() {
        return;
    }
    let Some(paint) = host.paint.as_ref() else {
        return;
    };
    if paint.canvas.is_null() || paint.text.is_null() {
        return;
    }
    let dx = paint.origin_x + x;
    let dy = paint.origin_y + y;
    let baseline = dy as f32 + entry.ascent as f32;
    let rgb = argb & 0x00FF_FFFF;
    let clip = *paint.clip_stack.last().unwrap();
    let canvas = &mut *paint.canvas;
    let tr = &mut *paint.text;

    canvas.with_clip(clip, |c| {
        let drawn_w = tr.draw(c, &s, dx as f32, baseline, entry.size_px as f32, rgb, entry.face);
        let line_w = (w.max(drawn_w.ceil() as i32)).max(1);
        if (decoration & TEXT_DECORATION_UNDERLINE) != 0 {
            let uy = (dy + entry.ascent + 2).min(dy + h.max(entry.height) - 1);
            c.fill_u8(Rect::new(dx, uy, line_w, 1), rgb, alpha);
        }
        if (decoration & TEXT_DECORATION_LINE_THROUGH) != 0 {
            let ly = dy + (entry.height.max(h) / 2);
            c.fill_u8(Rect::new(dx, ly, line_w, 1), rgb, alpha);
        }
        if (decoration & TEXT_DECORATION_OVERLINE) != 0 {
            c.fill_u8(Rect::new(dx, dy + 1, line_w, 1), rgb, alpha);
        }
    });
}

unsafe extern "C" fn cb_fill_rect(
    ud: *mut c_void,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    argb: u32,
    radii8: *const c_int,
) {
    let host = host_mut(ud);
    let alpha = ((argb >> 24) & 0xFF) as u8;
    if alpha == 0 || w <= 0 || h <= 0 {
        return;
    }
    let Some(paint) = host.paint.as_ref() else {
        return;
    };
    if paint.canvas.is_null() {
        return;
    }
    let r = Rect::new(paint.origin_x + x, paint.origin_y + y, w, h);
    let clip = *paint.clip_stack.last().unwrap();
    if r.intersect(clip).is_empty() {
        return;
    }
    let radii = read_radii8(radii8);
    let max_r = max_radius(&radii);
    let rgb = argb & 0x00FF_FFFF;
    let canvas = &mut *paint.canvas;
    canvas.with_clip(clip, |c| {
        if max_r > 0 {
            c.rounded_rect(r, max_r, rgb, alpha as f32 / 255.0);
        } else if alpha == 255 {
            c.fill(r, rgb);
        } else {
            c.fill_u8(r, rgb, alpha);
        }
    });
}

unsafe extern "C" fn cb_draw_linear_gradient(
    ud: *mut c_void,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    colors: *const u32,
    stops: *const f32,
    stop_count: c_int,
    radii8: *const c_int,
) {
    let host = host_mut(ud);
    if w <= 0 || h <= 0 || colors.is_null() || stops.is_null() || stop_count <= 0 {
        return;
    }
    let Some(paint) = host.paint.as_ref() else {
        return;
    };
    if paint.canvas.is_null() {
        return;
    }
    let colors = core::slice::from_raw_parts(colors, stop_count as usize);
    let stops = core::slice::from_raw_parts(stops, stop_count as usize);
    let radii = read_radii8(radii8);
    let has_radii = max_radius(&radii) > 0;
    let ox = paint.origin_x;
    let oy = paint.origin_y;
    let rect = Rect::new(ox + x, oy + y, w, h);
    let clip = *paint.clip_stack.last().unwrap();
    let vis = rect.intersect(clip);
    if vis.is_empty() {
        return;
    }
    let dx = x1 - x0;
    let dy = y1 - y0;
    let denom = dx * dx + dy * dy;
    let canvas = &mut *paint.canvas;
    canvas.with_clip(clip, |c| {
        for py in vis.y..vis.bottom() {
            let ly = (py - oy) as f32;
            for px in vis.x..vis.right() {
                let lx = (px - ox) as f32;
                if has_radii && !point_inside_radii(lx, ly, x as f32, y as f32, w as f32, h as f32, &radii) {
                    continue;
                }
                let t = if denom <= 1e-6 {
                    0.0
                } else {
                    ((lx - x0) * dx + (ly - y0) * dy) / denom
                };
                let (rgb, a) = interpolate_stops(colors, stops, t);
                if a > 0 {
                    c.blend_pixel_u8(px, py, rgb, a);
                }
            }
        }
    });
}

unsafe extern "C" fn cb_draw_radial_gradient(
    ud: *mut c_void,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    colors: *const u32,
    stops: *const f32,
    stop_count: c_int,
    radii8: *const c_int,
) {
    let host = host_mut(ud);
    if w <= 0 || h <= 0 || colors.is_null() || stops.is_null() || stop_count <= 0 {
        return;
    }
    let Some(paint) = host.paint.as_ref() else {
        return;
    };
    if paint.canvas.is_null() {
        return;
    }
    let colors = core::slice::from_raw_parts(colors, stop_count as usize);
    let stops = core::slice::from_raw_parts(stops, stop_count as usize);
    let radii = read_radii8(radii8);
    let has_radii = max_radius(&radii) > 0;
    let ox = paint.origin_x;
    let oy = paint.origin_y;
    let rect = Rect::new(ox + x, oy + y, w, h);
    let clip = *paint.clip_stack.last().unwrap();
    let vis = rect.intersect(clip);
    if vis.is_empty() {
        return;
    }
    let rx = rx.max(1.0);
    let ry = ry.max(1.0);
    let canvas = &mut *paint.canvas;
    canvas.with_clip(clip, |c| {
        for py in vis.y..vis.bottom() {
            let ly = (py - oy) as f32;
            let ny = (ly - cy) / ry;
            for px in vis.x..vis.right() {
                let lx = (px - ox) as f32;
                if has_radii && !point_inside_radii(lx, ly, x as f32, y as f32, w as f32, h as f32, &radii) {
                    continue;
                }
                let nx = (lx - cx) / rx;
                let t = (nx * nx + ny * ny).sqrt();
                let (rgb, a) = interpolate_stops(colors, stops, t);
                if a > 0 {
                    c.blend_pixel_u8(px, py, rgb, a);
                }
            }
        }
    });
}

fn draw_border_line(
    c: &mut Canvas,
    rect: Rect,
    side: SpaceBorderSide,
    horizontal: bool,
) {
    if side.style == SPACE_BORDER_NONE || side.width <= 0 {
        return;
    }
    let alpha = ((side.color >> 24) & 0xFF) as u8;
    if alpha == 0 {
        return;
    }
    let rgb = side.color & 0x00FF_FFFF;
    match side.style {
        SPACE_BORDER_DASHED => {
            let step = (side.width * 4).max(6);
            let dash = (side.width * 2).max(3);
            if horizontal {
                let mut x = rect.x;
                while x < rect.right() {
                    let dw = dash.min(rect.right() - x);
                    c.fill_u8(Rect::new(x, rect.y, dw, rect.h), rgb, alpha);
                    x += step;
                }
            } else {
                let mut y = rect.y;
                while y < rect.bottom() {
                    let dh = dash.min(rect.bottom() - y);
                    c.fill_u8(Rect::new(rect.x, y, rect.w, dh), rgb, alpha);
                    y += step;
                }
            }
        }
        SPACE_BORDER_DOTTED => {
            let dot = side.width.max(1);
            let step = dot * 2;
            if horizontal {
                let mut x = rect.x;
                while x < rect.right() {
                    let dw = dot.min(rect.right() - x);
                    c.fill_u8(Rect::new(x, rect.y, dw, rect.h), rgb, alpha);
                    x += step;
                }
            } else {
                let mut y = rect.y;
                while y < rect.bottom() {
                    let dh = dot.min(rect.bottom() - y);
                    c.fill_u8(Rect::new(rect.x, y, rect.w, dh), rgb, alpha);
                    y += step;
                }
            }
        }
        SPACE_BORDER_DOUBLE if side.width >= 3 => {
            let bar = (side.width / 3).max(1);
            if horizontal {
                c.fill_u8(Rect::new(rect.x, rect.y, rect.w, bar), rgb, alpha);
                c.fill_u8(Rect::new(rect.x, rect.bottom() - bar, rect.w, bar), rgb, alpha);
            } else {
                c.fill_u8(Rect::new(rect.x, rect.y, bar, rect.h), rgb, alpha);
                c.fill_u8(Rect::new(rect.right() - bar, rect.y, bar, rect.h), rgb, alpha);
            }
        }
        _ => {
            c.fill_u8(rect, rgb, alpha);
        }
    }
}

unsafe extern "C" fn cb_draw_borders(ud: *mut c_void, borders_ptr: *const SpaceBorders) {
    let host = host_mut(ud);
    if borders_ptr.is_null() {
        return;
    }
    let b = *borders_ptr;
    if b.w <= 0 || b.h <= 0 {
        return;
    }
    let Some(paint) = host.paint.as_ref() else {
        return;
    };
    if paint.canvas.is_null() {
        return;
    }
    let rect = Rect::new(paint.origin_x + b.x, paint.origin_y + b.y, b.w, b.h);
    let clip = *paint.clip_stack.last().unwrap();
    if rect.intersect(clip).is_empty() {
        return;
    }
    let max_r = [
        b.radius_tl_x,
        b.radius_tl_y,
        b.radius_tr_x,
        b.radius_tr_y,
        b.radius_br_x,
        b.radius_br_y,
        b.radius_bl_x,
        b.radius_bl_y,
    ]
    .into_iter()
    .max()
    .unwrap_or(0)
    .max(0);
    let canvas = &mut *paint.canvas;
    canvas.with_clip(clip, |c| {
        let same_solid = max_r > 0
            && b.top.style == SPACE_BORDER_SOLID
            && b.top.width > 0
            && b.top.width == b.right.width
            && b.top.width == b.bottom.width
            && b.top.width == b.left.width
            && b.top.color == b.right.color
            && b.top.color == b.bottom.color
            && b.top.color == b.left.color;
        if same_solid {
            let alpha = ((b.top.color >> 24) & 0xFF) as f32 / 255.0;
            if alpha > 0.0 {
                c.rounded_border(rect, max_r, b.top.color & 0x00FF_FFFF, alpha, b.top.width);
            }
            return;
        }
        if b.top.width > 0 {
            draw_border_line(
                c,
                Rect::new(rect.x, rect.y, rect.w, b.top.width.min(rect.h)),
                b.top,
                true,
            );
        }
        if b.bottom.width > 0 {
            let bh = b.bottom.width.min(rect.h);
            draw_border_line(
                c,
                Rect::new(rect.x, rect.bottom() - bh, rect.w, bh),
                b.bottom,
                true,
            );
        }
        if b.left.width > 0 {
            draw_border_line(
                c,
                Rect::new(rect.x, rect.y, b.left.width.min(rect.w), rect.h),
                b.left,
                false,
            );
        }
        if b.right.width > 0 {
            let rw = b.right.width.min(rect.w);
            draw_border_line(
                c,
                Rect::new(rect.right() - rw, rect.y, rw, rect.h),
                b.right,
                false,
            );
        }
    });
}

unsafe extern "C" fn cb_draw_list_marker(
    ud: *mut c_void,
    marker_type: c_int,
    argb: u32,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    index: c_int,
) {
    let host = host_mut(ud);
    let alpha = ((argb >> 24) & 0xFF) as u8;
    if alpha == 0 {
        return;
    }
    let Some(paint) = host.paint.as_ref() else {
        return;
    };
    if paint.canvas.is_null() {
        return;
    }
    let dx = paint.origin_x + x;
    let dy = paint.origin_y + y;
    let rgb = argb & 0x00FF_FFFF;
    let af = alpha as f32 / 255.0;
    let clip = *paint.clip_stack.last().unwrap();
    let canvas = &mut *paint.canvas;
    let tr = if paint.text.is_null() {
        None
    } else {
        Some(&mut *paint.text)
    };
    canvas.with_clip(clip, |c| {
        let cx = dx as f32 + w.max(6) as f32 * 0.5;
        let cy = dy as f32 + h.max(6) as f32 * 0.5;
        let r = (w.min(h).max(4) as f32 * 0.42).clamp(2.0, 5.0);
        match marker_type {
            0 => {}
            1 => c.ring(cx, cy, r, 1.2, rgb, af),
            2 => c.circle(cx, cy, r, rgb, af),
            3 => {
                let sz = (r * 2.0).round().max(3.0) as i32;
                c.fill_u8(
                    Rect::new((cx - sz as f32 * 0.5) as i32, (cy - sz as f32 * 0.5) as i32, sz, sz),
                    rgb,
                    alpha,
                );
            }
            _ => {
                if let Some(tr) = tr {
                    let label = format!("{index}.");
                    tr.draw_right(c, &label, (dx + w) as f32, (dy + h - 3) as f32, 13.0, rgb, Face::Ui);
                }
            }
        }
    });
}

unsafe extern "C" fn cb_set_clip(
    ud: *mut c_void,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    _radii8: *const c_int,
) {
    let host = host_mut(ud);
    if let Some(paint) = host.paint.as_mut() {
        let top = *paint.clip_stack.last().unwrap();
        let r = Rect::new(paint.origin_x + x, paint.origin_y + y, w.max(0), h.max(0));
        paint.clip_stack.push(top.intersect(r));
    }
}

unsafe extern "C" fn cb_del_clip(ud: *mut c_void) {
    let host = host_mut(ud);
    if let Some(paint) = host.paint.as_mut() {
        if paint.clip_stack.len() > 1 {
            paint.clip_stack.pop();
        }
    }
}

unsafe extern "C" fn cb_draw_rgba_blit(
    ud: *mut c_void,
    pixels_argb: *const u32,
    src_w: c_int,
    src_h: c_int,
    dst_x: c_int,
    dst_y: c_int,
    dst_w: c_int,
    dst_h: c_int,
) {
    let host = host_mut(ud);
    if pixels_argb.is_null() || src_w <= 0 || src_h <= 0 || dst_w <= 0 || dst_h <= 0 {
        return;
    }
    let Some(paint) = host.paint.as_ref() else {
        return;
    };
    if paint.canvas.is_null() {
        return;
    }
    let dx = paint.origin_x + dst_x;
    let dy = paint.origin_y + dst_y;
    let clip = *paint.clip_stack.last().unwrap();
    let dst_rect = Rect::new(dx, dy, dst_w, dst_h);
    let vis = dst_rect.intersect(clip);
    if vis.is_empty() {
        return;
    }
    let buf = core::slice::from_raw_parts(pixels_argb, (src_w as usize) * (src_h as usize));
    let canvas = &mut *paint.canvas;
    canvas.with_clip(clip, |c| {
        if src_w == dst_w && src_h == dst_h {
            let sw = src_w as usize;
            let cw = c.width as i32;
            let ch = c.height as i32;
            let x0 = vis.x.max(0);
            let y0 = vis.y.max(0);
            let x1 = vis.right().min(cw);
            let y1 = vis.bottom().min(ch);
            let fb = c.pixels_mut();
            for py in y0..y1 {
                let sy = (py - dy) as usize;
                let row_off = (py as usize) * (cw as usize);
                for px in x0..x1 {
                    let sx = (px - dx) as usize;
                    let argb = buf[sy * sw + sx];
                    let a = ((argb >> 24) & 0xFF) as u8;
                    if a == 255 {
                        fb[row_off + (px as usize)] = argb & 0x00FF_FFFF;
                    } else if a > 0 {
                        let dst = fb[row_off + (px as usize)];
                        fb[row_off + (px as usize)] = blend_u8(dst, argb & 0x00FF_FFFF, a);
                    }
                }
            }
            c.mark_dirty(vis);
        } else {
            let mut rgba = Vec::with_capacity(buf.len() * 4);
            for &px in buf {
                rgba.push(((px >> 16) & 0xFF) as u8);
                rgba.push(((px >> 8) & 0xFF) as u8);
                rgba.push((px & 0xFF) as u8);
                rgba.push(((px >> 24) & 0xFF) as u8);
            }
            let sprite = Sprite::from_rgba_straight(src_w as u32, src_h as u32, &rgba)
                .scale_bilinear(dst_w as u32, dst_h as u32);
            c.blit_sprite(dx, dy, &sprite);
        }
    });
}

unsafe extern "C" fn cb_load_image(
    ud: *mut c_void,
    src: *const c_char,
    baseurl: *const c_char,
    redraw_on_ready: c_int,
) {
    let host = host_mut(ud);
    let src_s = read_c_str(src);
    if src_s.is_empty() {
        return;
    }
    let base_s = read_c_str(baseurl);
    let eff_base = if base_s.is_empty() {
        host.base_url.as_str()
    } else {
        base_s.as_str()
    };
    let resolved = resolve_url_ref(eff_base, &src_s);
    if host.images.contains_key(&resolved) {
        return;
    }

    if resolved.starts_with("data:")
        || resolved.starts_with("file://")
        || resolved.starts_with("space://")
    {
        let resp = sync_fetch(
            "GET",
            &resolved,
            &Default::default(),
            &[],
            Some(&host.storage),
            host.dark_mode,
            host.is_win98,
        );
        if resp.status >= 200 && resp.status < 400 {
            if let Some(sprite) = decode_image_sprite(&resp.body, &resolved) {
                host.images.insert(resolved, ImageState::Ready(sprite));
                if redraw_on_ready != 0 {
                    host.dirty_relayout = true;
                    host.dirty_repaint = true;
                }
                return;
            }
        }
        host.images.insert(resolved, ImageState::Failed);
        return;
    }

    if let Some(pool) = host.fetch_pool.clone() {
        host.images.insert(resolved.clone(), ImageState::Loading);
        let req_id = host.next_req_id;
        host.next_req_id += 1;
        host.active_image_reqs.insert(req_id, resolved.clone());
        pool.enqueue(AsyncFetchRequest {
            tab_id: host.tab_id,
            nav_gen: host.nav_gen,
            req_id,
            is_navigation: false,
            is_image: true,
            method: String::from("GET"),
            url: resolved,
            headers: Default::default(),
            body: Vec::new(),
        });
    } else {
        host.images.insert(resolved, ImageState::Failed);
    }
}

unsafe extern "C" fn cb_get_image_size(
    ud: *mut c_void,
    src: *const c_char,
    baseurl: *const c_char,
    out_w: *mut c_int,
    out_h: *mut c_int,
) {
    let host = host_mut(ud);
    let src_s = read_c_str(src);
    let base_s = read_c_str(baseurl);
    let eff_base = if base_s.is_empty() {
        host.base_url.as_str()
    } else {
        base_s.as_str()
    };
    let resolved = resolve_url_ref(eff_base, &src_s);
    if !host.images.contains_key(&resolved) && !src_s.is_empty() {
        cb_load_image(ud, src, baseurl, 0);
    }
    let (w, h) = match host.images.get(&resolved) {
        Some(ImageState::Ready(s)) => (s.width as c_int, s.height as c_int),
        _ => (0, 0),
    };
    if !out_w.is_null() {
        *out_w = w;
    }
    if !out_h.is_null() {
        *out_h = h;
    }
}

unsafe extern "C" fn cb_draw_image(
    ud: *mut c_void,
    src: *const c_char,
    baseurl: *const c_char,
    clip_x: c_int,
    clip_y: c_int,
    clip_w: c_int,
    clip_h: c_int,
    orig_x: c_int,
    orig_y: c_int,
    orig_w: c_int,
    orig_h: c_int,
    _pos_x: c_int,
    _pos_y: c_int,
    _pos_w: c_int,
    _pos_h: c_int,
    repeat_mode: c_int,
) {
    let host = host_mut(ud);
    let src_s = read_c_str(src);
    if src_s.is_empty() || orig_w <= 0 || orig_h <= 0 || clip_w <= 0 || clip_h <= 0 {
        return;
    }
    let base_s = read_c_str(baseurl);
    let eff_base = if base_s.is_empty() {
        host.base_url.as_str()
    } else {
        base_s.as_str()
    };
    let resolved = resolve_url_ref(eff_base, &src_s);
    let Some(paint) = host.paint.as_ref() else {
        return;
    };
    if paint.canvas.is_null() {
        return;
    }
    let canvas_ptr = paint.canvas;
    let ox = paint.origin_x;
    let oy = paint.origin_y;
    let top_clip = *paint.clip_stack.last().unwrap();
    let img_clip = top_clip.intersect(Rect::new(ox + clip_x, oy + clip_y, clip_w, clip_h));
    if img_clip.is_empty() {
        return;
    }

    let Some(sprite) = host.cached_scaled_sprite(&resolved, orig_w as u32, orig_h as u32) else {
        return;
    };
    let tile_w = sprite.width.max(1) as i32;
    let tile_h = sprite.height.max(1) as i32;
    let canvas = &mut *canvas_ptr;
    canvas.with_clip(img_clip, |c| {
        let dx0 = ox + orig_x;
        let dy0 = oy + orig_y;
        match repeat_mode {
            0 => {
                // repeat both axes
                let mut start_x = dx0;
                while start_x > img_clip.x {
                    start_x -= tile_w;
                }
                let mut start_y = dy0;
                while start_y > img_clip.y {
                    start_y -= tile_h;
                }
                let mut y = start_y;
                while y < img_clip.bottom() {
                    let mut x = start_x;
                    while x < img_clip.right() {
                        c.blit_sprite(x, y, sprite);
                        x += tile_w;
                    }
                    y += tile_h;
                }
            }
            1 => {
                // repeat-x
                let mut start_x = dx0;
                while start_x > img_clip.x {
                    start_x -= tile_w;
                }
                let mut x = start_x;
                while x < img_clip.right() {
                    c.blit_sprite(x, dy0, sprite);
                    x += tile_w;
                }
            }
            2 => {
                // repeat-y
                let mut start_y = dy0;
                while start_y > img_clip.y {
                    start_y -= tile_h;
                }
                let mut y = start_y;
                while y < img_clip.bottom() {
                    c.blit_sprite(dx0, y, sprite);
                    y += tile_h;
                }
            }
            _ => c.blit_sprite(dx0, dy0, sprite),
        }
    });
}

unsafe extern "C" fn cb_fetch_sync(
    ud: *mut c_void,
    method: *const c_char,
    url: *const c_char,
    headers_json: *const c_char,
    body: *const c_char,
    body_len: c_int,
    out_status: *mut c_int,
    out_body: *mut *mut c_char,
    out_len: *mut c_int,
    out_resolved_url: *mut *mut c_char,
    out_headers_json: *mut *mut c_char,
) {
    let host = host_mut(ud);
    let m = read_c_str(method);
    let u = read_c_str(url);
    let hj = read_c_str(headers_json);
    let hdrs = parse_headers_json(&hj);
    let req_body = if !body.is_null() && body_len > 0 {
        core::slice::from_raw_parts(body as *const u8, body_len as usize)
    } else {
        &[]
    };
    let resp = sync_fetch_with_kind(
        if m.is_empty() { "GET" } else { &m },
        &u,
        &hdrs,
        req_body,
        Some(&host.storage),
        host.dark_mode,
        host.is_win98,
        FetchKind::Subresource,
    );
    if !out_status.is_null() {
        *out_status = resp.status as c_int;
    }
    if !out_body.is_null() {
        *out_body = HostState::alloc_c_bytes(&resp.body);
    }
    if !out_len.is_null() {
        *out_len = resp.body.len() as c_int;
    }
    if !out_resolved_url.is_null() {
        *out_resolved_url = HostState::alloc_c_str(&resp.final_url);
    }
    if !out_headers_json.is_null() {
        *out_headers_json = HostState::alloc_c_str(&serialize_headers_json(&resp.headers));
    }
}

unsafe extern "C" fn cb_fetch_async(
    ud: *mut c_void,
    method: *const c_char,
    url: *const c_char,
    headers_json: *const c_char,
    body: *const c_char,
    body_len: c_int,
) -> u64 {
    let host = host_mut(ud);
    let m = read_c_str(method);
    let u = read_c_str(url);
    let hj = read_c_str(headers_json);
    let hdrs = parse_headers_json(&hj);
    let req_body = if !body.is_null() && body_len > 0 {
        core::slice::from_raw_parts(body as *const u8, body_len as usize).to_vec()
    } else {
        Vec::new()
    };
    let req_id = host.next_req_id;
    host.next_req_id += 1;
    if let Some(pool) = host.fetch_pool.as_ref() {
        pool.enqueue(AsyncFetchRequest {
            tab_id: host.tab_id,
            nav_gen: host.nav_gen,
            req_id,
            is_navigation: false,
            is_image: false,
            method: if m.is_empty() { String::from("GET") } else { m },
            url: resolve_url_ref(&host.base_url, &u),
            headers: hdrs,
            body: req_body,
        });
    }
    req_id
}

unsafe extern "C" fn cb_prefetch_urls(ud: *mut c_void, urls_nl: *const c_char) {
    let host = host_mut(ud);
    let raw = read_c_str(urls_nl);
    if raw.is_empty() {
        return;
    }
    let urls: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    prefetch_subresources(&urls, Some(&host.storage), host.dark_mode, host.is_win98);
}

unsafe extern "C" fn cb_free_host_buf(_ud: *mut c_void, ptr: *mut c_char) {
    if !ptr.is_null() {
        libc::free(ptr as *mut c_void);
    }
}

unsafe extern "C" fn cb_set_title(ud: *mut c_void, title: *const c_char) {
    let host = host_mut(ud);
    host.title = read_c_str(title);
}

unsafe extern "C" fn cb_set_base_url(ud: *mut c_void, base_url: *const c_char) {
    let host = host_mut(ud);
    let b = read_c_str(base_url);
    if !b.is_empty() {
        host.base_url = b;
    }
}

unsafe extern "C" fn cb_set_cursor(ud: *mut c_void, cursor: *const c_char) {
    let host = host_mut(ud);
    let c = read_c_str(cursor);
    host.cursor = match c.as_str() {
        "pointer" => CursorKind::Pointer,
        "text" => CursorKind::Text,
        _ => CursorKind::Default,
    };
}

unsafe extern "C" fn cb_on_navigate(ud: *mut c_void, url: *const c_char, replace: c_int) {
    let host = host_mut(ud);
    let u = read_c_str(url);
    if !u.is_empty() {
        host.pending_navigations.push((u, replace != 0));
    }
}

unsafe extern "C" fn cb_on_history_op(
    ud: *mut c_void,
    op: c_int,
    delta: c_int,
    url: *const c_char,
    title: *const c_char,
) {
    let host = host_mut(ud);
    let u = read_c_str(url);
    let t = read_c_str(title);
    match op {
        1 => host.pending_history_ops.push(HistoryCommand::Push { url: u, title: t }),
        2 => host.pending_history_ops.push(HistoryCommand::Replace { url: u, title: t }),
        3 => host.pending_history_ops.push(HistoryCommand::Go(delta)),
        _ => {}
    }
}

unsafe extern "C" fn cb_on_console(ud: *mut c_void, level: c_int, msg: *const c_char) {
    let host = host_mut(ud);
    let message = read_c_str(msg);
    if host.console_logs.len() >= 256 {
        host.console_logs.remove(0);
    }
    host.console_logs.push(ConsoleEntry { level, message });
}

unsafe extern "C" fn cb_on_alert(
    ud: *mut c_void,
    kind: c_int,
    msg: *const c_char,
    def: *const c_char,
    out_reply: *mut *mut c_char,
) {
    let host = host_mut(ud);
    let message = read_c_str(msg);
    let default_value = read_c_str(def);
    if let Some(reply) = host.next_dialog_reply.take() {
        if !out_reply.is_null() {
            *out_reply = reply.map(|s| HostState::alloc_c_str(&s)).unwrap_or(core::ptr::null_mut());
        }
        host.pending_alerts.push(AlertRequest {
            kind,
            message,
            default_value,
        });
        return;
    }
    host.pending_alerts.push(AlertRequest {
        kind,
        message,
        default_value: default_value.clone(),
    });
    if !out_reply.is_null() {
        *out_reply = match kind {
            1 => HostState::alloc_c_str("true"),
            2 => HostState::alloc_c_str(&default_value),
            _ => core::ptr::null_mut(),
        };
    }
}

unsafe extern "C" fn cb_on_select_popup(
    ud: *mut c_void,
    node_id: u64,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    items_tsv: *const c_char,
    selected_idx: c_int,
) {
    let host = host_mut(ud);
    let raw = read_c_str(items_tsv);
    let items: Vec<String> = raw.split('\n').map(|s| s.to_string()).collect();
    if items.is_empty() {
        return;
    }
    let idx = (selected_idx.max(0) as usize).min(items.len() - 1);
    host.pending_select_popup = Some(SelectPopupRequest {
        node_id,
        viewport_rect: Rect::new(x, y, w.max(60), h.max(20)),
        items,
        selected_idx: idx,
    });
}

unsafe extern "C" fn cb_on_dirty(ud: *mut c_void, relayout: c_int, repaint: c_int) {
    let host = host_mut(ud);
    if relayout != 0 {
        host.dirty_relayout = true;
    }
    if repaint != 0 {
        host.dirty_repaint = true;
    }
}

unsafe extern "C" fn cb_storage_get(
    ud: *mut c_void,
    is_session: c_int,
    key: *const c_char,
) -> *mut c_char {
    let host = host_mut(ud);
    let k = read_c_str(key);
    let origin = host.effective_origin();
    let val = host
        .storage
        .lock()
        .ok()
        .and_then(|s| s.kv_get(is_session != 0, &origin, &k));
    match val {
        Some(v) => HostState::alloc_c_str(&v),
        None => core::ptr::null_mut(),
    }
}

unsafe extern "C" fn cb_storage_set(
    ud: *mut c_void,
    is_session: c_int,
    key: *const c_char,
    val: *const c_char,
) {
    let host = host_mut(ud);
    let k = read_c_str(key);
    let v = read_c_str(val);
    let origin = host.effective_origin();
    if let Ok(mut s) = host.storage.lock() {
        s.kv_set(is_session != 0, &origin, &k, &v);
    }
}

unsafe extern "C" fn cb_storage_remove(ud: *mut c_void, is_session: c_int, key: *const c_char) {
    let host = host_mut(ud);
    let k = read_c_str(key);
    let origin = host.effective_origin();
    if let Ok(mut s) = host.storage.lock() {
        s.kv_remove(is_session != 0, &origin, &k);
    }
}

unsafe extern "C" fn cb_storage_clear(ud: *mut c_void, is_session: c_int) {
    let host = host_mut(ud);
    let origin = host.effective_origin();
    if let Ok(mut s) = host.storage.lock() {
        s.kv_clear(is_session != 0, &origin);
    }
}

unsafe extern "C" fn cb_storage_len(ud: *mut c_void, is_session: c_int) -> c_int {
    let host = host_mut(ud);
    let origin = host.effective_origin();
    host.storage
        .lock()
        .map(|s| s.kv_len(is_session != 0, &origin) as c_int)
        .unwrap_or(0)
}

unsafe extern "C" fn cb_storage_key(
    ud: *mut c_void,
    is_session: c_int,
    idx: c_int,
) -> *mut c_char {
    let host = host_mut(ud);
    if idx < 0 {
        return core::ptr::null_mut();
    }
    let origin = host.effective_origin();
    let val = host
        .storage
        .lock()
        .ok()
        .and_then(|s| s.kv_key(is_session != 0, &origin, idx as usize));
    match val {
        Some(k) => HostState::alloc_c_str(&k),
        None => core::ptr::null_mut(),
    }
}

unsafe extern "C" fn cb_cookies_get(ud: *mut c_void) -> *mut c_char {
    let host = host_mut(ud);
    let val = host
        .storage
        .lock()
        .map(|s| s.cookies_for_url(&host.page_url))
        .unwrap_or_default();
    HostState::alloc_c_str(&val)
}

unsafe extern "C" fn cb_cookies_set(ud: *mut c_void, set_cookie_kv: *const c_char) {
    let host = host_mut(ud);
    let kv = read_c_str(set_cookie_kv);
    if let Ok(mut s) = host.storage.lock() {
        s.set_cookie_from_str(&host.page_url, &kv);
    }
}

unsafe extern "C" fn cb_now_ms(ud: *mut c_void) -> f64 {
    let host = host_mut(ud);
    host.start_instant.elapsed().as_secs_f64() * 1000.0
}

pub struct SpacePageHandle {
    page: *mut SpacePage,
    host: Box<HostState>,
}

impl Drop for SpacePageHandle {
    fn drop(&mut self) {
        if !self.page.is_null() {
            unsafe { space_page_destroy(self.page) };
            self.page = core::ptr::null_mut();
        }
    }
}

impl SpacePageHandle {
    pub fn new(
        tab_id: u64,
        storage: Arc<Mutex<BrowserStorage>>,
        fetch_pool: Option<Arc<FetchPool>>,
        viewport_w: i32,
        viewport_h: i32,
        dark_mode: bool,
        is_win98: bool,
    ) -> Self {
        let mut host = Box::new(HostState::new(tab_id, storage, fetch_pool, dark_mode, is_win98));
        let ud = (&mut *host as *mut HostState) as *mut c_void;
        let cb = SpaceHostCallbacks {
            user_data: ud,
            create_font: Some(cb_create_font),
            delete_font: Some(cb_delete_font),
            text_width: Some(cb_text_width),
            draw_text: Some(cb_draw_text),
            fill_rect: Some(cb_fill_rect),
            draw_linear_gradient: Some(cb_draw_linear_gradient),
            draw_radial_gradient: Some(cb_draw_radial_gradient),
            draw_borders: Some(cb_draw_borders),
            draw_list_marker: Some(cb_draw_list_marker),
            set_clip: Some(cb_set_clip),
            del_clip: Some(cb_del_clip),
            draw_rgba_blit: Some(cb_draw_rgba_blit),
            load_image: Some(cb_load_image),
            get_image_size: Some(cb_get_image_size),
            draw_image: Some(cb_draw_image),
            fetch_sync: Some(cb_fetch_sync),
            fetch_async: Some(cb_fetch_async),
            free_host_buf: Some(cb_free_host_buf),
            set_title: Some(cb_set_title),
            set_base_url: Some(cb_set_base_url),
            set_cursor: Some(cb_set_cursor),
            on_navigate: Some(cb_on_navigate),
            on_history_op: Some(cb_on_history_op),
            on_console: Some(cb_on_console),
            on_alert: Some(cb_on_alert),
            on_select_popup: Some(cb_on_select_popup),
            on_dirty: Some(cb_on_dirty),
            storage_get: Some(cb_storage_get),
            storage_set: Some(cb_storage_set),
            storage_remove: Some(cb_storage_remove),
            storage_clear: Some(cb_storage_clear),
            storage_len: Some(cb_storage_len),
            storage_key: Some(cb_storage_key),
            cookies_get: Some(cb_cookies_get),
            cookies_set: Some(cb_cookies_set),
            now_ms: Some(cb_now_ms),
            prefetch_urls: Some(cb_prefetch_urls),
        };
        let page = unsafe {
            space_page_create(
                &cb,
                viewport_w.max(64),
                viewport_h.max(64),
                i32::from(dark_mode),
                i32::from(is_win98),
            )
        };
        Self { page, host }
    }

    pub fn host(&self) -> &HostState {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut HostState {
        &mut self.host
    }

    pub fn set_theme(&mut self, dark_mode: bool, is_win98: bool, text: Option<&mut TextRenderer>) {
        self.host.dark_mode = dark_mode;
        self.host.is_win98 = is_win98;
        let page = self.page;
        self.host.with_text(text, || unsafe {
            space_page_set_theme(page, i32::from(dark_mode), i32::from(is_win98));
        });
    }

    pub fn load_html(&mut self, html: &str, url: &str, text: Option<&mut TextRenderer>) {
        self.host.page_url = url.to_string();
        self.host.base_url = url.to_string();
        self.host.title.clear();
        self.host.cursor = CursorKind::Default;
        self.host.images.clear();
        self.host.scaled_images.clear();
        self.host.active_image_reqs.clear();
        self.host.pending_navigations.clear();
        self.host.pending_history_ops.clear();
        self.host.pending_select_popup = None;
        self.host.pending_alerts.clear();
        self.host.dirty_relayout = false;
        self.host.dirty_repaint = false;

        let url_c = CString::new(url.as_bytes()).unwrap_or_else(|_| CString::new("about:blank").unwrap());
        let page = self.page;
        let bytes = html.as_bytes();
        self.host.with_text(text, || unsafe {
            space_page_load_html(
                page,
                bytes.as_ptr() as *const c_char,
                bytes.len() as c_int,
                url_c.as_ptr(),
            );
        });
    }

    pub fn resize(&mut self, w: i32, h: i32, text: Option<&mut TextRenderer>) {
        let page = self.page;
        self.host.with_text(text, || unsafe {
            space_page_resize(page, w.max(64), h.max(64));
        });
    }

    pub fn relayout(&mut self, force: bool, text: Option<&mut TextRenderer>) -> bool {
        let page = self.page;
        self.host.dirty_relayout = false;
        let r = self
            .host
            .with_text(text, || unsafe { space_page_relayout(page, i32::from(force)) });
        r != 0
    }

    pub fn doc_width(&self) -> i32 {
        unsafe { space_page_doc_width(self.page) }
    }

    pub fn doc_height(&self) -> i32 {
        unsafe { space_page_doc_height(self.page) }
    }

    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        text: Option<&mut TextRenderer>,
        viewport: Rect,
        scroll_x: i32,
        scroll_y: i32,
    ) {
        if viewport.w <= 0 || viewport.h <= 0 {
            return;
        }
        self.host.dirty_repaint = false;
        let text_ptr = text.map_or(core::ptr::null_mut(), |t| t as *mut TextRenderer);
        let prev_measure = self.host.measure_text_ptr;
        self.host.measure_text_ptr = text_ptr;
        self.host.paint = Some(PaintBinding {
            canvas: canvas as *mut Canvas,
            text: text_ptr,
            origin_x: viewport.x,
            origin_y: viewport.y,
            clip_stack: vec![viewport],
        });
        unsafe {
            space_page_draw(self.page, scroll_x, scroll_y, viewport.w, viewport.h);
        }
        self.host.paint = None;
        self.host.measure_text_ptr = prev_measure;
    }

    pub fn mouse_move(
        &mut self,
        x: i32,
        y: i32,
        scroll_x: i32,
        scroll_y: i32,
        text: Option<&mut TextRenderer>,
    ) {
        let page = self.page;
        self.host.with_text(text, || unsafe {
            space_page_mouse_move(page, x, y, scroll_x, scroll_y);
        });
    }

    pub fn mouse_down(
        &mut self,
        x: i32,
        y: i32,
        scroll_x: i32,
        scroll_y: i32,
        button: i32,
        text: Option<&mut TextRenderer>,
    ) {
        let page = self.page;
        self.host.with_text(text, || unsafe {
            space_page_mouse_down(page, x, y, scroll_x, scroll_y, button);
        });
    }

    pub fn mouse_up(
        &mut self,
        x: i32,
        y: i32,
        scroll_x: i32,
        scroll_y: i32,
        button: i32,
        text: Option<&mut TextRenderer>,
    ) {
        let page = self.page;
        self.host.with_text(text, || unsafe {
            space_page_mouse_up(page, x, y, scroll_x, scroll_y, button);
        });
    }

    pub fn mouse_leave(&mut self, text: Option<&mut TextRenderer>) {
        let page = self.page;
        self.host.with_text(text, || unsafe {
            space_page_mouse_leave(page);
        });
    }

    pub fn key_down(
        &mut self,
        key_code: i32,
        utf8_text: &str,
        mods: i32,
        text: Option<&mut TextRenderer>,
    ) -> bool {
        let c_txt = CString::new(utf8_text.as_bytes()).unwrap_or_default();
        let page = self.page;
        let r = self.host.with_text(text, || unsafe {
            space_page_key_down(page, key_code, c_txt.as_ptr(), mods)
        });
        r != 0
    }

    pub fn select_option(
        &mut self,
        node_id: u64,
        selected_idx: usize,
        text: Option<&mut TextRenderer>,
    ) {
        let page = self.page;
        self.host.with_text(text, || unsafe {
            space_page_select_option(page, node_id, selected_idx as c_int);
        });
    }

    pub fn resolve_fetch_or_image(
        &mut self,
        req_id: u64,
        is_image: bool,
        resp: &FetchResponse,
        text: Option<&mut TextRenderer>,
    ) -> bool {
        if is_image {
            if let Some(url_key) = self.host.active_image_reqs.remove(&req_id) {
                if resp.status >= 200 && resp.status < 400 {
                    if let Some(sprite) = decode_image_sprite(&resp.body, &resp.final_url) {
                        self.host.images.insert(url_key, ImageState::Ready(sprite));
                        self.host.dirty_relayout = true;
                        self.host.dirty_repaint = true;
                        return true;
                    }
                }
                self.host.images.insert(url_key, ImageState::Failed);
            }
            return false;
        }
        let url_c = CString::new(resp.final_url.as_bytes()).unwrap_or_default();
        let hdr_c = CString::new(serialize_headers_json(&resp.headers).as_bytes()).unwrap_or_default();
        let err_c = resp
            .error
            .as_ref()
            .and_then(|e| CString::new(e.as_bytes()).ok());
        let page = self.page;
        self.host.with_text(text, || unsafe {
            space_page_resolve_fetch(
                page,
                req_id,
                resp.status as c_int,
                url_c.as_ptr(),
                hdr_c.as_ptr(),
                resp.body.as_ptr() as *const c_char,
                resp.body.len() as c_int,
                err_c.as_ref().map_or(core::ptr::null(), |c| c.as_ptr()),
            );
        });
        true
    }

    pub fn tick(&mut self, text: Option<&mut TextRenderer>) -> bool {
        let now = self.host.start_instant.elapsed().as_secs_f64() * 1000.0;
        let force_relayout = self.host.dirty_relayout;
        self.host.dirty_relayout = false;
        let page = self.page;
        self.host.with_text(text, || unsafe {
            if force_relayout {
                space_page_relayout(page, 1);
            }
            space_page_tick(page, now) != 0
        })
    }

    pub fn eval(&mut self, code: &str, text: Option<&mut TextRenderer>) -> String {
        let mut buf = vec![0u8; 32 * 1024];
        let page = self.page;
        let bytes = code.as_bytes();
        let written = self.host.with_text(text, || unsafe {
            space_page_eval(
                page,
                bytes.as_ptr() as *const c_char,
                bytes.len() as c_int,
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
            )
        });
        if written <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..(written as usize).min(buf.len())]).into_owned()
    }

    pub fn dump_dom(&mut self) -> String {
        let mut buf = vec![0u8; 64 * 1024];
        let written = unsafe {
            space_page_dump_dom(self.page, buf.as_mut_ptr() as *mut c_char, buf.len() as c_int)
        };
        if written <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..(written as usize).min(buf.len())]).into_owned()
    }

    pub fn find_text(&mut self, query: &str, occurrence_idx: i32) -> (i32, Option<i32>) {
        let q = match CString::new(query.as_bytes()) {
            Ok(c) => c,
            Err(_) => return (0, None),
        };
        let mut y = -1i32;
        let total = unsafe { space_page_find_text(self.page, q.as_ptr(), occurrence_idx, &mut y) };
        if total > 0 && y >= 0 {
            (total, Some(y))
        } else {
            (total, None)
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        unsafe { space_page_set_focused(self.page, i32::from(focused)) };
    }
}
