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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Ui,
    Bold,
    Mono,
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
struct Font {
    data: Arc<FontData>,
    index: u32,
}

struct Outline<'a> {
    raster: &'a mut Rasterizer,
    scale: f32,
    left: f32,
    top: f32,
    pen: Point,
    start: Point,
}

impl Outline<'_> {
    fn point(&self, x: f32, y: f32) -> Point {
        point(x * self.scale - self.left, self.top - y * self.scale)
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
    fn rasterize(&self, ch: char, px: f32) -> Glyph {
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
            glyph.xmin = (f32::from(bounds.x_min) * scale).floor() as i32;
            glyph.ymin = (f32::from(bounds.y_min) * scale).floor() as i32;
            let right = (f32::from(bounds.x_max) * scale).ceil() as i32;
            let top = (f32::from(bounds.y_max) * scale).ceil() as i32;
            glyph.width = (right - glyph.xmin).max(0) as usize;
            glyph.height = (top - glyph.ymin).max(0) as usize;
            if glyph.width != 0 && glyph.height != 0 {
                let mut raster = Rasterizer::new(glyph.width, glyph.height);
                face.outline_glyph(
                    id,
                    &mut Outline {
                        raster: &mut raster,
                        scale,
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

pub struct TextRenderer {
    ui: Font,
    bold: Font,
    mono: Font,
    ui_scale: f32,
    cache: HashMap<(Face, char, u16), Arc<Glyph>>,
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
    let data = if let Some(data) = files.get(path) {
        data.clone()
    } else {
        let data = Arc::new(FontData::open(path)?);
        files.insert(path.to_owned(), data.clone());
        data
    };
    let index = collection_index(&data, needle);
    ParsedFace::parse(&data, index).map_err(|e| format!("font {path}: {e:?}"))?;
    Ok(Font { data, index })
}

impl TextRenderer {
    pub fn load() -> Result<Self, String> {
        let mut files = HashMap::new();
        let ui = load(theme::FONT_UI, "CJK SC", &mut files)
            .or_else(|_| load(theme::FONT_FALLBACK, "", &mut files))?;
        let bold = load(theme::FONT_UI_BOLD, "CJK SC", &mut files)
            .or_else(|_| load(theme::FONT_UI, "CJK SC", &mut files))?;
        let mono = load(theme::FONT_MONO, "Mono CJK SC", &mut files)
            .or_else(|_| load(theme::FONT_MONO, "CJK SC", &mut files))?;
        Ok(Self {
            ui,
            bold,
            mono,
            ui_scale: 1.0,
            cache: HashMap::with_capacity(512),
        })
    }

    pub fn set_ui_scale(&mut self, scale: f32) {
        self.ui_scale = scale.clamp(0.75, 1.5);
    }

    fn font(&self, face: Face) -> &Font {
        match face {
            Face::Ui => &self.ui,
            Face::Bold => &self.bold,
            Face::Mono => &self.mono,
        }
    }

    pub fn glyph(&mut self, face: Face, ch: char, px: f32) -> Arc<Glyph> {
        let eff_px = if matches!(face, Face::Ui | Face::Bold) {
            px * self.ui_scale
        } else {
            px
        };
        let key = (face, ch, (eff_px * 10.0).round() as u16);
        if let Some(glyph) = self.cache.get(&key) {
            return glyph.clone();
        }
        let font = self.font(face);
        let glyph = Arc::new(font.rasterize(ch, eff_px));
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
        let mut pen = x;
        for ch in text.chars() {
            let glyph = self.glyph(face, ch, px);
            if glyph.width != 0 && glyph.height != 0 {
                let left = pen.round() as i32 + glyph.xmin;
                let top = baseline.round() as i32 - glyph.ymin - glyph.height as i32;
                canvas.blend_coverage(left, top, glyph.width, glyph.height, color, &glyph.coverage);
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let glyph = ui.rasterize(ch, 18.0);
            assert!(glyph.advance > 0.0, "{ch}");
            assert_eq!(glyph.coverage.len(), glyph.width * glyph.height);
            assert!(glyph.coverage.iter().any(|&v| v > 0), "{ch}");
            assert!(
                glyph.coverage.iter().any(|&v| v > 0 && v < 255),
                "antialias {ch}"
            );
        }
        let space = ui.rasterize(' ', 18.0);
        assert!(space.advance > 0.0);
        assert!(space.coverage.is_empty());
        assert_eq!(
            mono.rasterize('i', 18.0).advance,
            mono.rasterize('W', 18.0).advance
        );
    }
}
