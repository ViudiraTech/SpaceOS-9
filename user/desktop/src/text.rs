/*
 *
 *       user/desktop/src/text.rs
 *       Demand-paged mmap TrueType font loading, glyph cache and text drawing
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Fonts are real TTF/TTC files from the Alpine font packages: Noto Sans CJK
//! supplies the Chinese interface, Noto Sans Mono CJK the terminal. Instead of
//! eagerly reading ~38 MB of CJK font collections into heap vectors at process
//! start, we `mmap(..., MAP_PRIVATE)` them lazily so font initialization drops
//! from ~2.5 s to under 1 ms.

use crate::draw::Canvas;
use crate::theme;
use ab_glyph_rasterizer::{point, Point, Rasterizer};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use ttf_parser::{Face as ParsedFace, GlyphId, OutlineBuilder};

const ITALIC_SLANT: f32 = 0.21;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Ui,
    Bold,
    Italic,
    BoldItalic,
    Serif,
    SerifBold,
    SerifItalic,
    SerifBoldItalic,
    Mono,
    CyberDisplay,
    CyberMono,
}

impl Face {
    #[inline]
    pub const fn is_italic(self) -> bool {
        matches!(
            self,
            Self::Italic | Self::BoldItalic | Self::SerifItalic | Self::SerifBoldItalic
        )
    }
}

fn resolve_font_path(path: &str) -> String {
    if path.starts_with("/usr/") {
        if let Ok(root) = std::env::var("SPUI_SYSROOT") {
            let trimmed = root.trim();
            if !trimmed.is_empty() {
                return format!("{}{}", trimmed.trim_end_matches('/'), path);
            }
        }
    }
    path.to_string()
}

pub struct Glyph {
    pub width: usize,
    pub height: usize,
    pub xmin: i32,
    pub ymin: i32,
    pub advance: f32,
    pub coverage: Vec<u8>,
}

/// Font file contents. Large CJK collections are mapped read-only so only the
/// pages a process actually touches are faulted in (the kernel maps private
/// file mappings lazily); reading them into the heap cost tens of megabytes
/// and seconds of I/O per window. Falls back to `read` if mapping fails.
enum FontData {
    Mapped { ptr: *const u8, len: usize },
    Heap(Vec<u8>),
}

// SAFETY: the mapping is read-only and lives as long as the process.
unsafe impl Send for FontData {}
unsafe impl Sync for FontData {}

impl Deref for FontData {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &[u8] {
        match self {
            // SAFETY: `ptr`/`len` describe a live PROT_READ mapping.
            Self::Mapped { ptr, len } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
            Self::Heap(data) => data.as_slice(),
        }
    }
}

impl Drop for FontData {
    fn drop(&mut self) {
        if let Self::Mapped { ptr, len } = *self {
            // SAFETY: unmapping exactly the range returned by mmap.
            unsafe { libc::munmap(ptr as *mut libc::c_void, len) };
        }
    }
}

impl FontData {
    fn open(path: &str) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        let opened = std::fs::File::open(path)
            .and_then(|file| file.metadata().map(|meta| (file, meta.len() as usize)));
        if let Ok((file, len)) = opened {
            if len > 0 {
                // SAFETY: read-only private mapping of an open file.
                let ptr = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        len,
                        libc::PROT_READ,
                        libc::MAP_PRIVATE,
                        file.as_raw_fd(),
                        0,
                    )
                };
                if ptr != libc::MAP_FAILED {
                    return Ok(Self::Mapped {
                        ptr: ptr as *const u8,
                        len,
                    });
                }
            }
        }
        std::fs::read(path)
            .map(Self::Heap)
            .map_err(|e| format!("font {path}: {e}"))
    }
}

/// Retain the original collection without preprocessing its glyph outlines.
#[derive(Clone)]
struct Font {
    data: Arc<FontData>,
    index: u32,
}

struct Outline<'a> {
    raster: &'a mut Rasterizer,
    scale: f32,
    slant: f32,
    left: f32,
    top: f32,
    pen: Point,
    start: Point,
}

impl Outline<'_> {
    fn point(&self, x: f32, y: f32) -> Point {
        let sx = x + y * self.slant;
        point(sx * self.scale - self.left, self.top - y * self.scale)
    }
}

impl OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pen = self.point(x, y);
        self.start = self.pen;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let end = self.point(x, y);
        self.raster.draw_line(self.pen, end);
        self.pen = end;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let end = self.point(x, y);
        self.raster.draw_quad(self.pen, self.point(x1, y1), end);
        self.pen = end;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let end = self.point(x, y);
        self.raster
            .draw_cubic(self.pen, self.point(x1, y1), self.point(x2, y2), end);
        self.pen = end;
    }
    fn close(&mut self) {
        self.raster.draw_line(self.pen, self.start);
        self.pen = self.start;
    }
}

impl Font {
    fn has_glyph(&self, ch: char) -> bool {
        ParsedFace::parse(&self.data, self.index)
            .ok()
            .and_then(|f| f.glyph_index(ch))
            .is_some_and(|id| id.0 != 0)
    }

    fn rasterize(&self, ch: char, px: f32, slant: f32) -> Glyph {
        let face = ParsedFace::parse(&self.data, self.index).expect("validated font");
        let id = face.glyph_index(ch).unwrap_or(GlyphId(0));
        let scale = px / f32::from(face.units_per_em());
        let advance = f32::from(face.glyph_hor_advance(id).unwrap_or(0)) * scale;
        let mut glyph = Glyph {
            width: 0,
            height: 0,
            xmin: 0,
            ymin: 0,
            advance,
            coverage: Vec::new(),
        };
        if let Some(bounds) = face.glyph_bounding_box(id) {
            let x0 = f32::from(bounds.x_min);
            let x1 = f32::from(bounds.x_max);
            let y0 = f32::from(bounds.y_min);
            let y1 = f32::from(bounds.y_max);
            let (min_x, max_x) = if slant != 0.0 {
                (
                    (x0 + y0 * slant).min(x0 + y1 * slant),
                    (x1 + y0 * slant).max(x1 + y1 * slant),
                )
            } else {
                (x0, x1)
            };
            glyph.xmin = (min_x * scale).floor() as i32;
            glyph.ymin = (y0 * scale).floor() as i32;
            let right = (max_x * scale).ceil() as i32;
            let top = (y1 * scale).ceil() as i32;
            glyph.width = (right - glyph.xmin).max(0) as usize;
            glyph.height = (top - glyph.ymin).max(0) as usize;
            if glyph.width != 0 && glyph.height != 0 {
                let mut raster = Rasterizer::new(glyph.width, glyph.height);
                face.outline_glyph(
                    id,
                    &mut Outline {
                        raster: &mut raster,
                        scale,
                        slant,
                        left: glyph.xmin as f32,
                        top: top as f32,
                        pen: point(0.0, 0.0),
                        start: point(0.0, 0.0),
                    },
                );
                glyph.coverage.resize(glyph.width * glyph.height, 0);
                raster.for_each_pixel(|i, alpha| {
                    glyph.coverage[i] = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                });
            }
        }
        glyph
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct GlowCacheKey {
    text: String,
    face: Face,
    px_tenths: u16,
    subpx_x: u8,
    sigma_in_tenths: u16,
    sigma_out_tenths: u16,
    gain_tenths: u16,
    intensity_u8: u8,
}

struct CachedGlow {
    dx: i32,
    dy: i32,
    w: usize,
    h: usize,
    adv: f32,
    cov: Vec<u8>,
}

pub struct TextRenderer {
    ui: Font,
    /// Default CJK face, used when a configured family lacks a glyph.
    fallback: Font,
    bold: Font,
    serif: Option<Font>,
    serif_bold: Option<Font>,
    mono: Font,
    cyber_display: Option<Font>,
    cyber_mono: Option<Font>,
    ui_scale: f32,
    cache: HashMap<(Face, char, u16), Arc<Glyph>>,
    glow_buf_a: Vec<f32>,
    glow_buf_b: Vec<f32>,
    glow_buf_c: Vec<f32>,
    glow_cov: Vec<u8>,
    glow_cache: HashMap<GlowCacheKey, CachedGlow>,
}

/// Configuration for Windows Vista/7 DWM-style glyph-mask Gaussian text glow
/// (`DrawThemeTextEx` / `DTT_GLOWSIZE`).
#[derive(Clone, Copy, Debug)]
pub struct TextGlow {
    pub color: u32,
    pub sigma_inner: f32,
    pub sigma_outer: f32,
    pub gain: f32,
    pub intensity: f32,
    pub offset_y: i32,
}

impl TextGlow {
    pub const fn aero_light(active: bool) -> Self {
        Self {
            color: 0xffffff,
            sigma_inner: 2.0,
            sigma_outer: 4.8,
            gain: 4.4,
            intensity: if active { 0.94 } else { 0.62 },
            offset_y: 0,
        }
    }

    pub const fn aero_dark(active: bool) -> Self {
        Self {
            color: 0x040a12,
            sigma_inner: 1.6,
            sigma_outer: 3.4,
            gain: 3.8,
            intensity: if active { 0.70 } else { 0.44 },
            offset_y: 1,
        }
    }

    pub const fn aero_title(dark: bool, active: bool) -> Self {
        if dark {
            Self::aero_dark(active)
        } else {
            Self::aero_light(active)
        }
    }

    pub const fn aero_compact(dark: bool, active: bool) -> Self {
        if dark {
            Self {
                color: 0x040a12,
                sigma_inner: 1.4,
                sigma_outer: 2.8,
                gain: 3.4,
                intensity: if active { 0.62 } else { 0.42 },
                offset_y: 1,
            }
        } else {
            Self {
                color: 0xffffff,
                sigma_inner: 1.7,
                sigma_outer: 3.8,
                gain: 4.0,
                intensity: if active { 0.86 } else { 0.64 },
                offset_y: 0,
            }
        }
    }
}

fn build_gaussian_kernel(sigma: f32) -> (usize, [f32; 41]) {
    let s = sigma.clamp(0.4, 8.0);
    let radius = (s * 2.5).ceil().clamp(1.0, 20.0) as usize;
    let mut kernel = [0.0f32; 41];
    let inv_2s2 = -0.5 / (s * s);
    let mut sum = 0.0f32;
    for (i, slot) in kernel[..=2 * radius].iter_mut().enumerate() {
        let d = i as f32 - radius as f32;
        let w = (inv_2s2 * d * d).exp();
        *slot = w;
        sum += w;
    }
    let inv_sum = 1.0 / sum;
    for slot in &mut kernel[..=2 * radius] {
        *slot *= inv_sum;
    }
    (radius, kernel)
}

fn blur_separable_zero(
    src: &[f32],
    tmp: &mut [f32],
    dst: &mut [f32],
    w: usize,
    h: usize,
    radius: usize,
    kernel: &[f32],
) {
    for y in 0..h {
        let row_off = y * w;
        for x in 0..w {
            let k_start = radius.saturating_sub(x);
            let x_start = x + k_start - radius;
            let x_end = (x + radius + 1).min(w);
            let mut acc = 0.0f32;
            let mut k = k_start;
            for sx in x_start..x_end {
                acc += src[row_off + sx] * kernel[k];
                k += 1;
            }
            tmp[row_off + x] = acc;
        }
    }
    for y in 0..h {
        let dst_row = &mut dst[y * w..(y + 1) * w];
        dst_row.fill(0.0);
        let k_start = radius.saturating_sub(y);
        let y_start = y + k_start - radius;
        let y_end = (y + radius + 1).min(h);
        let mut k = k_start;
        for sy in y_start..y_end {
            let wt = kernel[k];
            let src_row = &tmp[sy * w..(sy + 1) * w];
            for x in 0..w {
                dst_row[x] += src_row[x] * wt;
            }
            k += 1;
        }
    }
}

fn be16(data: &[u8], offset: usize) -> u16 {
    data.get(offset..offset + 2)
        .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
}

fn be32(data: &[u8], offset: usize) -> u32 {
    data.get(offset..offset + 4)
        .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn sfnt_name(data: &[u8], face: usize) -> String {
    let tables = be16(data, face + 4) as usize;
    for table in 0..tables {
        let record = face + 12 + table * 16;
        if data.get(record..record + 4) != Some(b"name") {
            continue;
        }
        let name_table = be32(data, record + 8) as usize;
        let count = be16(data, name_table + 2) as usize;
        let string_offset = be16(data, name_table + 4) as usize;
        let mut family = String::new();
        let mut full = String::new();
        for index in 0..count {
            let entry = name_table + 6 + index * 12;
            let platform = be16(data, entry);
            let encoding = be16(data, entry + 2);
            let name_id = be16(data, entry + 6);
            let length = be16(data, entry + 8) as usize;
            let offset = be16(data, entry + 10) as usize;
            let start = name_table + string_offset + offset;
            let Some(bytes) = data.get(start..start.saturating_add(length)) else {
                continue;
            };
            let value = if platform == 3 && (encoding == 1 || encoding == 10) {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            } else if platform == 1 && encoding == 0 {
                bytes.iter().map(|&b| b as char).collect()
            } else {
                continue;
            };
            match name_id {
                1 if family.is_empty() => family = value,
                4 if full.is_empty() => full = value,
                _ => {}
            }
        }
        return if full.is_empty() { family } else { full };
    }
    String::new()
}

/// Select the TTC face whose family name contains `needle` (case sensitive).
fn collection_index(data: &[u8], needle: &str) -> u32 {
    if data.get(0..4) != Some(b"ttcf") {
        return 0;
    }
    let count = be32(data, 8) as usize;
    for index in 0..count.min(64) {
        let face = be32(data, 12 + index * 4) as usize;
        let name = sfnt_name(data, face);
        if name.contains(needle) {
            return index as u32;
        }
    }
    0
}

fn load(
    path: &str,
    needle: &str,
    files: &mut HashMap<String, Arc<FontData>>,
) -> Result<Font, String> {
    let resolved = resolve_font_path(path);
    let data = if let Some(data) = files.get(&resolved) {
        data.clone()
    } else {
        let data = Arc::new(FontData::open(&resolved)?);
        files.insert(resolved, data.clone());
        data
    };
    let index = collection_index(&data, needle);
    ParsedFace::parse(&data, index).map_err(|e| format!("font {path}: {e:?}"))?;
    Ok(Font { data, index })
}

/// Font families chosen in `uxconf` (`font:/ui-family`, `/document-family`,
/// `/monospace-family`). Empty means "use the bundled defaults".
#[derive(Clone, Debug, Default)]
pub struct FontPrefs {
    pub ui: Option<String>,
    pub document: Option<String>,
    pub monospace: Option<String>,
}

impl FontPrefs {
    pub fn is_default(&self) -> bool {
        [&self.ui, &self.document, &self.monospace]
            .iter()
            .all(|value| value.as_deref().map(str::trim).unwrap_or("").is_empty())
    }

    /// Build preferences from raw `uxconf` values (empty means default).
    pub fn from_values(ui: &str, document: &str, monospace: &str) -> Self {
        let clean = |value: &str| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        Self {
            ui: clean(ui),
            document: clean(document),
            monospace: clean(monospace),
        }
    }
}

fn sysroot_path() -> std::path::PathBuf {
    std::env::var("SPUI_SYSROOT")
        .ok()
        .map(|root| std::path::PathBuf::from(root.trim()))
        .filter(|root| !root.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
}

fn load_at(
    reference: &crate::fonts::FaceRef,
    files: &mut HashMap<String, Arc<FontData>>,
) -> Result<Font, String> {
    let resolved = reference.path.to_string_lossy().into_owned();
    let data = if let Some(data) = files.get(&resolved) {
        data.clone()
    } else {
        let data = Arc::new(FontData::open(&resolved)?);
        files.insert(resolved.clone(), data.clone());
        data
    };
    ParsedFace::parse(&data, reference.index).map_err(|e| format!("font {resolved}: {e:?}"))?;
    Ok(Font {
        data,
        index: reference.index,
    })
}

impl TextRenderer {
    pub fn load() -> Result<Self, String> {
        Self::load_with(&FontPrefs::default())
    }

    /// Load the role faces, honouring the configured families. A family that
    /// cannot be resolved (removed package, unknown name) falls back to the
    /// bundled default per role.
    pub fn load_with(prefs: &FontPrefs) -> Result<Self, String> {
        let mut files = HashMap::new();
        let ui = load(theme::FONT_UI, "CJK SC", &mut files)
            .or_else(|_| load(theme::FONT_FALLBACK, "", &mut files))?;
        let mut bold = load(theme::FONT_UI_BOLD, "CJK SC", &mut files)
            .or_else(|_| load(theme::FONT_UI, "CJK SC", &mut files))?;
        let mut serif = load(theme::FONT_SERIF, "CJK SC", &mut files).ok();
        let mut serif_bold = load(theme::FONT_SERIF_BOLD, "CJK SC", &mut files).ok();
        let mut mono = load(theme::FONT_MONO, "Mono CJK SC", &mut files)
            .or_else(|_| load(theme::FONT_MONO, "CJK SC", &mut files))?;
        let cyber_display = load(theme::FONT_CYBER_DISPLAY, "", &mut files).ok();
        let cyber_mono = load(theme::FONT_CYBER_MONO, "", &mut files).ok();
        let fallback = ui.clone();

        let mut ui = ui;
        if !prefs.is_default() {
            let families = crate::fonts::scan(&sysroot_path());
            if let Some(family) = prefs
                .ui
                .as_deref()
                .and_then(|name| crate::fonts::resolve(&families, name))
            {
                if let Ok(face) = load_at(&family.regular, &mut files) {
                    bold = family
                        .bold
                        .as_ref()
                        .and_then(|index| load_at(index, &mut files).ok())
                        .unwrap_or_else(|| face.clone());
                    ui = face;
                }
            }
            if let Some(family) = prefs
                .document
                .as_deref()
                .and_then(|name| crate::fonts::resolve(&families, name))
            {
                if let Ok(face) = load_at(&family.regular, &mut files) {
                    let bold_face = family
                        .bold
                        .as_ref()
                        .and_then(|index| load_at(index, &mut files).ok());
                    serif_bold = bold_face.clone().or_else(|| Some(face.clone()));
                    serif = Some(face);
                }
            }
            if let Some(family) = prefs
                .monospace
                .as_deref()
                .and_then(|name| crate::fonts::resolve(&families, name))
            {
                if let Ok(face) = load_at(&family.regular, &mut files) {
                    mono = face;
                }
            }
        }
        Ok(Self {
            ui,
            fallback,
            bold,
            serif,
            serif_bold,
            mono,
            cyber_display,
            cyber_mono,
            ui_scale: 1.0,
            cache: HashMap::with_capacity(512),
            glow_buf_a: Vec::new(),
            glow_buf_b: Vec::new(),
            glow_buf_c: Vec::new(),
            glow_cov: Vec::new(),
            glow_cache: HashMap::with_capacity(64),
        })
    }

    pub fn set_ui_scale(&mut self, scale: f32) {
        let clamped = scale.clamp(0.75, 1.5);
        if (clamped - self.ui_scale).abs() > 0.001 {
            self.glow_cache.clear();
        }
        self.ui_scale = clamped;
    }

    fn font_for_char(&self, face: Face, ch: char) -> (&Font, f32) {
        let slant = if face.is_italic() { ITALIC_SLANT } else { 0.0 };
        let font = match face {
            Face::Ui | Face::Italic => &self.ui,
            Face::Bold | Face::BoldItalic => &self.bold,
            Face::Serif | Face::SerifItalic => self.serif.as_ref().unwrap_or(&self.ui),
            Face::SerifBold | Face::SerifBoldItalic => self
                .serif_bold
                .as_ref()
                .or(self.serif.as_ref())
                .unwrap_or(&self.bold),
            Face::Mono => &self.mono,
            Face::CyberDisplay => self
                .cyber_display
                .as_ref()
                .filter(|f| f.has_glyph(ch))
                .unwrap_or(&self.bold),
            Face::CyberMono => self
                .cyber_mono
                .as_ref()
                .filter(|f| f.has_glyph(ch))
                .unwrap_or(&self.mono),
        };
        // A configured Latin family usually has no CJK coverage; borrow the
        // glyph from the bundled CJK face instead of rendering tofu.
        let font = if font.has_glyph(ch) {
            font
        } else if self.fallback.has_glyph(ch) {
            &self.fallback
        } else {
            font
        };
        (font, slant)
    }

    pub fn glyph(&mut self, face: Face, ch: char, px: f32) -> Arc<Glyph> {
        let eff_px = if matches!(
            face,
            Face::Ui
                | Face::Bold
                | Face::Italic
                | Face::BoldItalic
                | Face::Serif
                | Face::SerifBold
                | Face::SerifItalic
                | Face::SerifBoldItalic
        ) {
            px * self.ui_scale
        } else {
            px
        };
        let key = (face, ch, (eff_px * 10.0).round() as u16);
        if let Some(glyph) = self.cache.get(&key) {
            return glyph.clone();
        }
        let (font, slant) = self.font_for_char(face, ch);
        let glyph = Arc::new(font.rasterize(ch, eff_px, slant));
        self.cache.insert(key, glyph.clone());
        glyph
    }

    pub fn mono_advance(&mut self, px: f32) -> f32 {
        self.glyph(Face::Mono, 'M', px).advance.max(6.0)
    }

    pub fn measure(&mut self, text: &str, px: f32, face: Face) -> f32 {
        let mut width = 0.0f32;
        for ch in text.chars() {
            width += self.glyph(face, ch, px).advance;
        }
        width
    }

    /// Truncate `value` with `…` so its rendered width fits inside `max_width`.
    pub fn truncate(&mut self, value: &str, max_width: f32, px: f32, face: Face) -> String {
        if max_width <= 0.0 {
            return String::new();
        }
        if self.measure(value, px, face) <= max_width {
            return value.to_string();
        }
        let ellipsis_w = self.glyph(face, '…', px).advance;
        let budget = (max_width - ellipsis_w).max(0.0);
        let mut acc = 0.0f32;
        let mut out = String::new();
        for ch in value.chars() {
            let adv = self.glyph(face, ch, px).advance;
            if acc + adv > budget {
                break;
            }
            acc += adv;
            out.push(ch);
        }
        out.push('…');
        out
    }

    /// Draw `text` with the baseline at `baseline`; returns the advance width.
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: f32,
        baseline: f32,
        px: f32,
        color: u32,
        face: Face,
    ) -> f32 {
        self.draw_alpha(canvas, text, x, baseline, px, color, 1.0, face)
    }

    /// Draw `text` with explicit opacity `alpha` (`0.0..=1.0`).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_alpha(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: f32,
        baseline: f32,
        px: f32,
        color: u32,
        alpha: f32,
        face: Face,
    ) -> f32 {
        let op = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let mut pen = x;
        for ch in text.chars() {
            let glyph = self.glyph(face, ch, px);
            if glyph.width != 0 && glyph.height != 0 && op > 0 {
                let left = pen.round() as i32 + glyph.xmin;
                let top = baseline.round() as i32 - glyph.ymin - glyph.height as i32;
                canvas.blend_coverage_alpha(
                    left,
                    top,
                    glyph.width,
                    glyph.height,
                    color,
                    &glyph.coverage,
                    op,
                );
            }
            pen += glyph.advance;
        }
        pen - x
    }

    pub fn draw_centered(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        center_x: f32,
        baseline: f32,
        px: f32,
        color: u32,
        face: Face,
    ) {
        let width = self.measure(text, px, face);
        self.draw(
            canvas,
            text,
            center_x - width / 2.0,
            baseline,
            px,
            color,
            face,
        );
    }

    pub fn draw_right(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        right_x: f32,
        baseline: f32,
        px: f32,
        color: u32,
        face: Face,
    ) {
        let width = self.measure(text, px, face);
        self.draw(canvas, text, right_x - width, baseline, px, color, face);
    }

    /// Render a Windows 7 DWM-style glyph-mask dual-lobe Gaussian glow behind `text`.
    /// Call `draw` afterward to paint crisp glyph foreground strokes on top.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_glow(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: f32,
        baseline: f32,
        px: f32,
        face: Face,
        glow: TextGlow,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        if glow.intensity <= 0.001 {
            return self.measure(text, px, face);
        }

        let eff_px = if matches!(face, Face::Ui | Face::Bold) {
            px * self.ui_scale
        } else {
            px
        };
        let key = GlowCacheKey {
            text: text.to_owned(),
            face,
            px_tenths: (eff_px * 10.0).round() as u16,
            subpx_x: ((x - x.floor()) * 8.0).round().clamp(0.0, 7.0) as u8,
            sigma_in_tenths: (glow.sigma_inner.clamp(0.4, 8.0) * 10.0).round() as u16,
            sigma_out_tenths: (glow.sigma_outer.max(glow.sigma_inner).clamp(0.4, 10.0) * 10.0)
                .round() as u16,
            gain_tenths: (glow.gain * 10.0).round() as u16,
            intensity_u8: (glow.intensity.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        };
        let anchor_x = x.round() as i32;
        let base_y = baseline.round() as i32;
        if let Some(cached) = self.glow_cache.get(&key) {
            if cached.w > 0 && cached.h > 0 {
                canvas.blend_coverage_alpha(
                    anchor_x + cached.dx,
                    base_y + cached.dy + glow.offset_y,
                    cached.w,
                    cached.h,
                    glow.color,
                    &cached.cov,
                    255,
                );
            }
            return cached.adv;
        }

        let mut glyphs: Vec<(i32, i32, Arc<Glyph>)> =
            Vec::with_capacity(text.chars().count().min(128));
        let mut pen = x;
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for ch in text.chars() {
            let g = self.glyph(face, ch, px);
            let g_adv = g.advance;
            if g.width != 0 && g.height != 0 {
                let gx0 = pen.round() as i32 + g.xmin;
                let gy0 = base_y - g.ymin - g.height as i32;
                let gx1 = gx0 + g.width as i32;
                let gy1 = gy0 + g.height as i32;
                min_x = min_x.min(gx0);
                min_y = min_y.min(gy0);
                max_x = max_x.max(gx1);
                max_y = max_y.max(gy1);
                glyphs.push((gx0, gy0, g));
            }
            pen += g_adv;
        }
        let adv = pen - x;
        if glyphs.is_empty() || max_x <= min_x || max_y <= min_y {
            if text.len() <= 96 {
                if self.glow_cache.len() >= 64 {
                    self.glow_cache.clear();
                }
                self.glow_cache.insert(
                    key,
                    CachedGlow {
                        dx: 0,
                        dy: 0,
                        w: 0,
                        h: 0,
                        adv,
                        cov: Vec::new(),
                    },
                );
            }
            return adv;
        }

        let sigma1 = glow.sigma_inner.clamp(0.4, 8.0);
        let sigma2 = glow.sigma_outer.max(sigma1).clamp(0.4, 10.0);
        let pad = (sigma2 * 2.5).ceil().clamp(2.0, 20.0) as i32;
        let origin_x = min_x - pad;
        let origin_y = min_y - pad;
        let bw = (max_x - min_x + 2 * pad) as usize;
        let bh = (max_y - min_y + 2 * pad) as usize;
        let len = bw.saturating_mul(bh);
        if len == 0 || len > 512 * 1024 {
            return adv;
        }

        self.glow_buf_a.clear();
        self.glow_buf_a.resize(len, 0.0);
        self.glow_buf_b.clear();
        self.glow_buf_b.resize(len, 0.0);
        self.glow_buf_c.clear();
        self.glow_buf_c.resize(len, 0.0);
        self.glow_cov.clear();
        self.glow_cov.resize(len, 0);

        for (gx0, gy0, g) in &glyphs {
            let lx = (gx0 - origin_x) as usize;
            let ly = (gy0 - origin_y) as usize;
            for row in 0..g.height {
                let src_off = row * g.width;
                let dst_off = (ly + row) * bw + lx;
                for col in 0..g.width {
                    let cov = g.coverage[src_off + col];
                    if cov != 0 {
                        let s = cov as f32 * (1.0 / 255.0);
                        let slot = &mut self.glow_buf_a[dst_off + col];
                        *slot = *slot + s - *slot * s;
                    }
                }
            }
        }

        let (r1, k1) = build_gaussian_kernel(sigma1);
        blur_separable_zero(
            &self.glow_buf_a,
            &mut self.glow_buf_b,
            &mut self.glow_buf_c,
            bw,
            bh,
            r1,
            &k1,
        );
        let sigma_delta = (sigma2 * sigma2 - sigma1 * sigma1).max(0.0).sqrt();
        let intensity = glow.intensity.clamp(0.0, 1.0);
        if sigma_delta >= 0.35 {
            let (r2, k2) = build_gaussian_kernel(sigma_delta);
            blur_separable_zero(
                &self.glow_buf_c,
                &mut self.glow_buf_b,
                &mut self.glow_buf_a,
                bw,
                bh,
                r2,
                &k2,
            );
            for i in 0..len {
                let u = 0.56 * self.glow_buf_c[i] + 0.44 * self.glow_buf_a[i];
                let sat = 1.0 - (-glow.gain * u).exp();
                self.glow_cov[i] = (sat * intensity * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
        } else {
            for i in 0..len {
                let sat = 1.0 - (-glow.gain * self.glow_buf_c[i]).exp();
                self.glow_cov[i] = (sat * intensity * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
        }

        canvas.blend_coverage_alpha(
            origin_x,
            origin_y + glow.offset_y,
            bw,
            bh,
            glow.color,
            &self.glow_cov,
            255,
        );
        if text.len() <= 96 && len <= 64 * 1024 {
            if self.glow_cache.len() >= 64 {
                self.glow_cache.clear();
            }
            self.glow_cache.insert(
                key,
                CachedGlow {
                    dx: origin_x - anchor_x,
                    dy: origin_y - base_y,
                    w: bw,
                    h: bh,
                    adv,
                    cov: self.glow_cov.clone(),
                },
            );
        }
        adv
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_glow(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: f32,
        baseline: f32,
        px: f32,
        color: u32,
        face: Face,
        glow: TextGlow,
    ) -> f32 {
        self.draw_glow(canvas, text, x, baseline, px, face, glow);
        self.draw(canvas, text, x, baseline, px, color, face)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_centered_with_glow(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        center_x: f32,
        baseline: f32,
        px: f32,
        color: u32,
        face: Face,
        glow: TextGlow,
    ) {
        let width = self.measure(text, px, face);
        self.draw_with_glow(
            canvas,
            text,
            center_x - width / 2.0,
            baseline,
            px,
            color,
            face,
            glow,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_right_with_glow(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        right_x: f32,
        baseline: f32,
        px: f32,
        color: u32,
        face: Face,
        glow: TextGlow,
    ) {
        let width = self.measure(text, px, face);
        self.draw_with_glow(
            canvas,
            text,
            right_x - width,
            baseline,
            px,
            color,
            face,
            glow,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Rect;

    #[test]
    fn cjk_faces_share_collection_and_rasterize_on_demand() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/rootfs/stage");
        let path = root.join(theme::FONT_UI.trim_start_matches('/'));
        let path = path.to_str().unwrap();
        let mut files = HashMap::new();
        let ui = load(path, "CJK SC", &mut files).unwrap();
        let mono = load(path, "Mono CJK SC", &mut files).unwrap();
        assert!(Arc::ptr_eq(&ui.data, &mono.data));
        assert_ne!(ui.index, mono.index);
        for ch in ['A', '中', '文', '，', 'g'] {
            let glyph = ui.rasterize(ch, 18.0, 0.0);
            assert!(glyph.advance > 0.0, "{ch}");
            assert_eq!(glyph.coverage.len(), glyph.width * glyph.height);
            assert!(glyph.coverage.iter().any(|&v| v > 0), "{ch}");
            assert!(
                glyph.coverage.iter().any(|&v| v > 0 && v < 255),
                "antialias {ch}"
            );
            let italic = ui.rasterize(ch, 18.0, ITALIC_SLANT);
            assert_eq!(italic.coverage.len(), italic.width * italic.height);
            assert!(italic.width >= glyph.width);
            assert!(italic.coverage.iter().any(|&v| v > 0), "italic {ch}");
        }
        let space = ui.rasterize(' ', 18.0, 0.0);
        assert!(space.advance > 0.0);
        assert!(space.coverage.is_empty());
        assert_eq!(
            mono.rasterize('i', 18.0, 0.0).advance,
            mono.rasterize('W', 18.0, 0.0).advance
        );
        assert!(Face::Italic.is_italic());
        assert!(Face::SerifBoldItalic.is_italic());
        assert!(!Face::Serif.is_italic());
    }

    #[test]
    fn aero_dwm_glow_hugs_glyphs_respects_clip_and_handles_argb() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/rootfs/stage");
        let path = root.join(theme::FONT_UI.trim_start_matches('/'));
        let path = path.to_str().unwrap();
        let mut files = HashMap::new();
        let ui = load(path, "CJK SC", &mut files).unwrap();
        let bold = Font {
            data: ui.data.clone(),
            index: ui.index,
        };
        let mono = load(path, "Mono CJK SC", &mut files).unwrap();
        let fallback = ui.clone();
        let mut renderer = TextRenderer {
            ui,
            fallback,
            bold,
            serif: None,
            serif_bold: None,
            mono,
            cyber_display: None,
            cyber_mono: None,
            ui_scale: 1.0,
            cache: HashMap::with_capacity(64),
            glow_buf_a: Vec::new(),
            glow_buf_b: Vec::new(),
            glow_buf_c: Vec::new(),
            glow_cov: Vec::new(),
            glow_cache: HashMap::with_capacity(64),
        };

        // 1. ARGB transparent glass canvas + clip keeping y=0 dark hairline clean
        let mut glass = Canvas::new(140, 32, 0);
        glass.set_argb(true);
        let glow = TextGlow::aero_title(false, true);
        glass.with_clip(Rect::new(0, 1, 140, 29), |c| {
            renderer.draw_glow(c, "文件管理器", 24.0, 20.0, 12.5, Face::Ui, glow);
        });
        let first_pass = glass.pixels().to_vec();
        let mut glass_cached = Canvas::new(140, 32, 0);
        glass_cached.set_argb(true);
        glass_cached.with_clip(Rect::new(0, 1, 140, 29), |c| {
            renderer.draw_glow(c, "文件管理器", 24.0, 20.0, 12.5, Face::Ui, glow);
        });
        assert_eq!(glass_cached.pixels(), first_pass.as_slice());
        for x in 0..140 {
            assert_eq!(glass.pixels()[x], 0, "top row x={x} should stay clear");
        }
        let non_zero_glow = glass.pixels().iter().filter(|&&p| (p >> 24) > 0).count();
        assert!(non_zero_glow > 150, "expected generous soft glyph halo");
        assert_eq!(glass.pixels()[15 * 140 + 138], 0);

        let sample_near = glass.pixels()[12 * 140 + 28];
        let a = sample_near >> 24;
        let r = (sample_near >> 16) & 0xff;
        let g = (sample_near >> 8) & 0xff;
        let b = sample_near & 0xff;
        assert!(r <= a && g <= a && b <= a);

        // 2. Opaque canvas convenience helper + no-op edge cases
        let mut opaque = Canvas::new(140, 32, 0x204060);
        let adv = renderer.draw_with_glow(
            &mut opaque,
            "设置",
            24.0,
            20.0,
            12.5,
            0x000000,
            Face::Ui,
            TextGlow::aero_compact(false, true),
        );
        assert!(adv > 10.0);
        assert_ne!(opaque.pixels()[15 * 140 + 28], 0x204060);
        assert_eq!(
            renderer.draw_glow(&mut glass, "", 10.0, 20.0, 12.5, Face::Ui, glow),
            0.0
        );
        assert!(renderer.draw_glow(&mut glass, "   ", 10.0, 20.0, 12.5, Face::Ui, glow) > 0.0);
        assert!(
            renderer.draw_glow(
                &mut glass,
                "Test",
                10.0,
                20.0,
                12.5,
                Face::Ui,
                TextGlow {
                    intensity: 0.0,
                    ..glow
                }
            ) > 0.0
        );
    }
}
