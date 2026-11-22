/*
 *
 *       user/desktop/src/draw.rs
 *       High-throughput software canvas, SHM backing, SWAR blending and sprites
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! SPUI paints into an `0x00RRGGBB` pixel buffer backed either by a System V
//! shared-memory segment (`MIT-SHM`) or a heap fallback. All inner loops use
//! fixed-point SWAR integer blending, scissor clipping, dirty-rect tracking and
//! corner-only anti-aliasing so full-desktop redraws stay sub-millisecond.

#![allow(dead_code)]

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn intersect(&self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(x, y, (right - x).max(0), (bottom - y).max(0))
    }

    pub fn union(&self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    pub fn inset(&self, amount: i32) -> Rect {
        Rect::new(
            self.x + amount,
            self.y + amount,
            self.w - amount * 2,
            self.h - amount * 2,
        )
    }

    pub fn expand(&self, amount: i32) -> Rect {
        self.inset(-amount)
    }

    #[inline(always)]
    pub const fn contains_rect(&self, other: Rect) -> bool {
        !other.is_empty()
            && self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }
}

/// Exact 2D non-overlapping rectangle region for compositor occlusion culling.
#[derive(Clone, Debug, Default)]
pub struct RectRegion {
    pub rects: Vec<Rect>,
}

impl RectRegion {
    const MAX_RECTS: usize = 64;

    pub fn new() -> Self {
        Self {
            rects: Vec::with_capacity(16),
        }
    }

    pub fn from_rect(r: Rect) -> Self {
        let mut reg = Self::new();
        if !r.is_empty() {
            reg.rects.push(r);
        }
        reg
    }

    pub fn clear(&mut self) {
        self.rects.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn reset_to_slice(&mut self, src: &[Rect]) {
        self.rects.clear();
        for &r in src {
            if !r.is_empty() {
                self.rects.push(r);
            }
        }
    }

    /// Subtract `cut` from every rectangle in this region in place without introducing overlap.
    pub fn subtract_rect(&mut self, cut: Rect) {
        if self.rects.is_empty() || cut.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.rects.len() {
            let r = self.rects[i];
            let ix = r.intersect(cut);
            if ix.is_empty() {
                i += 1;
                continue;
            }
            self.rects.swap_remove(i);
            // Top strip (full width of r)
            if ix.y > r.y {
                self.rects.push(Rect::new(r.x, r.y, r.w, ix.y - r.y));
            }
            // Bottom strip (full width of r)
            if ix.bottom() < r.bottom() {
                self.rects
                    .push(Rect::new(r.x, ix.bottom(), r.w, r.bottom() - ix.bottom()));
            }
            // Middle-left strip
            if ix.x > r.x {
                self.rects.push(Rect::new(r.x, ix.y, ix.x - r.x, ix.h));
            }
            // Middle-right strip
            if ix.right() < r.right() {
                self.rects
                    .push(Rect::new(ix.right(), ix.y, r.right() - ix.right(), ix.h));
            }
        }
        self.coalesce_if_needed();
    }

    /// Add `add` to this region, keeping existing parts covered and bounding region complexity.
    pub fn add_rect(&mut self, add: Rect) {
        if add.is_empty() {
            return;
        }
        // Fast reject if already fully covered by one existing rectangle.
        if self.rects.iter().any(|r| r.contains_rect(add)) {
            return;
        }
        // Remove existing rectangles completely contained in `add`.
        self.rects.retain(|r| !add.contains_rect(*r));
        // Subtract any overlap from `add`? Instead, subtract existing rects or simply subtract `add`
        // from existing first so the region stays disjoint.
        self.subtract_rect_no_coalesce(add);
        self.rects.push(add);
        self.coalesce_if_needed();
    }

    fn subtract_rect_no_coalesce(&mut self, cut: Rect) {
        let mut i = 0;
        while i < self.rects.len() {
            let r = self.rects[i];
            let ix = r.intersect(cut);
            if ix.is_empty() {
                i += 1;
                continue;
            }
            self.rects.swap_remove(i);
            if ix.y > r.y {
                self.rects.push(Rect::new(r.x, r.y, r.w, ix.y - r.y));
            }
            if ix.bottom() < r.bottom() {
                self.rects
                    .push(Rect::new(r.x, ix.bottom(), r.w, r.bottom() - ix.bottom()));
            }
            if ix.x > r.x {
                self.rects.push(Rect::new(r.x, ix.y, ix.x - r.x, ix.h));
            }
            if ix.right() < r.right() {
                self.rects
                    .push(Rect::new(ix.right(), ix.y, r.right() - ix.right(), ix.h));
            }
        }
    }

    fn coalesce_if_needed(&mut self) {
        if self.rects.len() <= Self::MAX_RECTS {
            return;
        }
        // First merge horizontally adjacent bands sharing (y, h).
        self.rects.sort_unstable_by_key(|r| (r.y, r.h, r.x));
        let mut i = 0;
        while i + 1 < self.rects.len() {
            let a = self.rects[i];
            let b = self.rects[i + 1];
            if a.y == b.y && a.h == b.h && a.right() >= b.x {
                self.rects[i] = Rect::new(a.x, a.y, a.right().max(b.right()) - a.x, a.h);
                self.rects.remove(i + 1);
            } else {
                i += 1;
            }
        }
        // If still over budget, iteratively union minimal-waste pairs and preserve strict disjointness.
        let mut guard = 0usize;
        while self.rects.len() > Self::MAX_RECTS {
            guard += 1;
            if guard > Self::MAX_RECTS * 2 {
                let bb = self.bounding_box();
                self.rects.clear();
                if !bb.is_empty() {
                    self.rects.push(bb);
                }
                break;
            }
            let last = self.rects.pop().unwrap();
            let mut best_idx = 0usize;
            let mut best_waste = i64::MAX;
            for (idx, &ex) in self.rects.iter().enumerate() {
                let u = ex.union(last);
                let waste = i64::from(u.w) * i64::from(u.h)
                    - i64::from(ex.w) * i64::from(ex.h)
                    - i64::from(last.w) * i64::from(last.h);
                if waste < best_waste {
                    best_waste = waste;
                    best_idx = idx;
                }
            }
            let merged = self.rects.swap_remove(best_idx).union(last);
            self.subtract_rect_no_coalesce(merged);
            self.rects.push(merged);
        }
    }

    pub fn intersects_rect(&self, target: Rect) -> bool {
        !target.is_empty() && self.rects.iter().any(|r| !r.intersect(target).is_empty())
    }

    pub fn intersect_rect_into(&self, target: Rect, out: &mut Vec<Rect>) {
        out.clear();
        if target.is_empty() {
            return;
        }
        for &r in &self.rects {
            let ix = r.intersect(target);
            if !ix.is_empty() {
                out.push(ix);
            }
        }
    }

    pub fn bounding_box(&self) -> Rect {
        let mut bb = Rect::default();
        for &r in &self.rects {
            bb = bb.union(r);
        }
        bb
    }
}

#[inline(always)]
pub const fn rgb(color: u32) -> (u8, u8, u8) {
    (
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
    )
}

/// Fast SWAR integer blend of `src` (`0xRRGGBB`) over `dst` (`0xRRGGBB`).
#[inline(always)]
pub const fn blend_u8(dst: u32, src: u32, alpha: u8) -> u32 {
    if alpha == 0 {
        return dst;
    }
    if alpha == 255 {
        return src & 0x00ff_ffff;
    }
    let a = (alpha as u32) + ((alpha as u32) >> 7);
    let inv = 256 - a;
    let rb = (((src & 0x00ff_00ff) * a + (dst & 0x00ff_00ff) * inv) >> 8) & 0x00ff_00ff;
    let g = (((src & 0x0000_ff00) * a + (dst & 0x0000_ff00) * inv) >> 8) & 0x0000_ff00;
    rb | g
}

/// Blend premultiplied `0xAARRGGBB` over opaque `0x00RRGGBB`.
#[inline(always)]
pub const fn blend_premul(dst: u32, premul_argb: u32) -> u32 {
    let alpha = (premul_argb >> 24) as u8;
    if alpha == 0 {
        return dst;
    }
    if alpha == 255 {
        return premul_argb & 0x00ff_ffff;
    }
    let a = (alpha as u32) + ((alpha as u32) >> 7);
    let inv = 256 - a;
    let drb = (((dst & 0x00ff_00ff) * inv) >> 8) & 0x00ff_00ff;
    let dg = (((dst & 0x0000_ff00) * inv) >> 8) & 0x0000_ff00;
    let r_raw = ((drb >> 16) & 0xff) + ((premul_argb >> 16) & 0xff);
    let g_raw = ((dg >> 8) & 0xff) + ((premul_argb >> 8) & 0xff);
    let b_raw = (drb & 0xff) + (premul_argb & 0xff);
    let r = if r_raw > 255 { 255 } else { r_raw };
    let g = if g_raw > 255 { 255 } else { g_raw };
    let b = if b_raw > 255 { 255 } else { b_raw };
    (r << 16) | (g << 8) | b
}

/// Premultiply straight `0xRRGGBB` + `alpha` into `0xAARRGGBB`.
#[inline(always)]
pub const fn premultiply_u8(rgb_color: u32, alpha: u8) -> u32 {
    if alpha == 0 {
        return 0;
    }
    if alpha == 255 {
        return 0xff00_0000 | (rgb_color & 0x00ff_ffff);
    }
    let a = (alpha as u32) + ((alpha as u32) >> 7);
    let rb = (((rgb_color & 0x00ff_00ff) * a) >> 8) & 0x00ff_00ff;
    let g = (((rgb_color & 0x0000_ff00) * a) >> 8) & 0x0000_ff00;
    ((alpha as u32) << 24) | rb | g
}

/// Porter-Duff `SRC_OVER` blending straight `src_rgb` (`alpha`) over premultiplied `dst_argb`.
#[inline(always)]
pub const fn src_over_u8(dst_argb: u32, src_rgb: u32, alpha: u8) -> u32 {
    if alpha == 0 {
        return dst_argb;
    }
    if alpha == 255 {
        return 0xff00_0000 | (src_rgb & 0x00ff_ffff);
    }
    let da = (dst_argb >> 24) & 0xff;
    if da == 255 {
        return 0xff00_0000 | blend_u8(dst_argb, src_rgb, alpha);
    }
    let sa = alpha as u32;
    let sf = sa + (sa >> 7);
    let inv = 256 - sf;
    let srb = (((src_rgb & 0x00ff_00ff) * sf) >> 8) & 0x00ff_00ff;
    let sg = (((src_rgb & 0x0000_ff00) * sf) >> 8) & 0x0000_ff00;
    if da == 0 {
        return (sa << 24) | srb | sg;
    }
    let drb = (((dst_argb & 0x00ff_00ff) * inv) >> 8) & 0x00ff_00ff;
    let dg = (((dst_argb & 0x0000_ff00) * inv) >> 8) & 0x0000_ff00;
    let out_a_raw = sa + ((da * inv) >> 8);
    let out_a = if out_a_raw > 255 { 255 } else { out_a_raw };
    let r_raw = ((srb >> 16) & 0xff) + ((drb >> 16) & 0xff);
    let g_raw = ((sg >> 8) & 0xff) + ((dg >> 8) & 0xff);
    let b_raw = (srb & 0xff) + (drb & 0xff);
    let r = if r_raw > 255 { 255 } else { r_raw };
    let g = if g_raw > 255 { 255 } else { g_raw };
    let b = if b_raw > 255 { 255 } else { b_raw };
    (out_a << 24) | (r << 16) | (g << 8) | b
}

/// Porter-Duff `SRC_OVER` blending premultiplied `premul_src` over premultiplied `dst_argb`.
#[inline(always)]
pub const fn src_over_premul(dst_argb: u32, premul_src: u32) -> u32 {
    let sa = (premul_src >> 24) & 0xff;
    if sa == 0 {
        return dst_argb;
    }
    if sa == 255 {
        return premul_src;
    }
    let da = (dst_argb >> 24) & 0xff;
    if da == 255 {
        return 0xff00_0000 | blend_premul(dst_argb, premul_src);
    }
    if da == 0 {
        return premul_src;
    }
    let sf = sa + (sa >> 7);
    let inv = 256 - sf;
    let out_a_raw = sa + ((da * inv) >> 8);
    let out_a = if out_a_raw > 255 { 255 } else { out_a_raw };
    let drb = (((dst_argb & 0x00ff_00ff) * inv) >> 8) & 0x00ff_00ff;
    let dg = (((dst_argb & 0x0000_ff00) * inv) >> 8) & 0x0000_ff00;
    let r_raw = ((drb >> 16) & 0xff) + ((premul_src >> 16) & 0xff);
    let g_raw = ((dg >> 8) & 0xff) + ((premul_src >> 8) & 0xff);
    let b_raw = (drb & 0xff) + (premul_src & 0xff);
    let r = if r_raw > 255 { 255 } else { r_raw };
    let g = if g_raw > 255 { 255 } else { g_raw };
    let b = if b_raw > 255 { 255 } else { b_raw };
    (out_a << 24) | (r << 16) | (g << 8) | b
}

/// Scale a premultiplied `0xAARRGGBB` pixel by opacity `scale_u8` (`0..=255`).
#[inline(always)]
pub const fn scale_premul(premul_argb: u32, scale_u8: u8) -> u32 {
    if scale_u8 == 255 {
        return premul_argb;
    }
    if scale_u8 == 0 || (premul_argb >> 24) == 0 {
        return 0;
    }
    let f = (scale_u8 as u32) + ((scale_u8 as u32) >> 7);
    let ag = (((premul_argb >> 8) & 0x00ff_00ff) * f) & 0xff00_ff00;
    let rb = (((premul_argb & 0x00ff_00ff) * f) >> 8) & 0x00ff_00ff;
    ag | rb
}

#[inline]
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let alpha = (t.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    blend_u8(a, b, alpha)
}

#[inline(always)]
fn f32_to_u8(alpha: f32) -> u8 {
    (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Pre-bake a 256-entry `0x00RRGGBB` gradient lookup table from sorted CSS stops.
pub fn build_gradient_lut(stops: &[(f32, u32)]) -> [u32; 256] {
    let mut lut = [0u32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        *slot = Canvas::sample_gradient_stops(stops, i as f32 / 255.0) & 0x00ff_ffff;
    }
    lut
}

/// Premultiplied `0xAARRGGBB` sprite used for icons, logos and thumbnails.
#[derive(Clone, Debug)]
pub struct Sprite {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl Sprite {
    /// Construct from `tiny-skia` premultiplied `[R, G, B, A]` bytes.
    pub fn from_rgba_premul(width: u32, height: u32, rgba: &[u8]) -> Self {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for px in rgba.chunks_exact(4) {
            let (r, g, b, a) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
            pixels.push((a << 24) | (r << 16) | (g << 8) | b);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Construct from straight (unpremultiplied) `[R, G, B, A]` bytes (e.g. PNG).
    pub fn from_rgba_straight(width: u32, height: u32, rgba: &[u8]) -> Self {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for px in rgba.chunks_exact(4) {
            let a = px[3] as u32;
            let r = (px[0] as u32 * a + 127) / 255;
            let g = (px[1] as u32 * a + 127) / 255;
            let b = (px[2] as u32 * a + 127) / 255;
            pixels.push((a << 24) | (r << 16) | (g << 8) | b);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Construct opaque sprite from `[R, G, B]` bytes (e.g. JPEG).
    pub fn from_rgb(width: u32, height: u32, rgb_bytes: &[u8]) -> Self {
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for px in rgb_bytes.chunks_exact(3) {
            pixels
                .push(0xff00_0000 | ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Bilinear cover-crop scaling into an opaque `0x00RRGGBB` buffer of `dst_w × dst_h`.
    pub fn scale_cover_opaque(&self, dst_w: usize, dst_h: usize) -> Vec<u32> {
        if dst_w == 0 || dst_h == 0 || self.width == 0 || self.height == 0 {
            return vec![0; dst_w * dst_h];
        }
        let sw = self.width as usize;
        let sh = self.height as usize;
        // Choose cover crop box inside (sw, sh) preserving aspect ratio.
        let (crop_x, crop_y, crop_w, crop_h) = if sw * dst_h > sh * dst_w {
            let cw = (sh * dst_w / dst_h).max(1).min(sw);
            ((sw - cw) / 2, 0, cw, sh)
        } else {
            let ch = (sw * dst_h / dst_w).max(1).min(sh);
            (0, (sh - ch) / 2, sw, ch)
        };

        let mut out = vec![0u32; dst_w * dst_h];
        for dy in 0..dst_h {
            let sy_fp = ((crop_y * 256) + (dy * crop_h * 256) / dst_h).min((sh - 1) * 256);
            let sy0 = sy_fp >> 8;
            let sy1 = (sy0 + 1).min(sh - 1);
            let fy = (sy_fp & 0xff) as u32;
            let row0 = &self.pixels[sy0 * sw..(sy0 + 1) * sw];
            let row1 = &self.pixels[sy1 * sw..(sy1 + 1) * sw];
            let out_row = &mut out[dy * dst_w..(dy + 1) * dst_w];

            for (dx, slot) in out_row.iter_mut().enumerate() {
                let sx_fp = ((crop_x * 256) + (dx * crop_w * 256) / dst_w).min((sw - 1) * 256);
                let sx0 = sx_fp >> 8;
                let sx1 = (sx0 + 1).min(sw - 1);
                let fx = (sx_fp & 0xff) as u32;

                let p00 = row0[sx0] & 0x00ff_ffff;
                let p10 = row0[sx1] & 0x00ff_ffff;
                let p01 = row1[sx0] & 0x00ff_ffff;
                let p11 = row1[sx1] & 0x00ff_ffff;

                let top_rb = (((p00 & 0x00ff_00ff) * (256 - fx) + (p10 & 0x00ff_00ff) * fx) >> 8)
                    & 0x00ff_00ff;
                let bot_rb = (((p01 & 0x00ff_00ff) * (256 - fx) + (p11 & 0x00ff_00ff) * fx) >> 8)
                    & 0x00ff_00ff;
                let rb = ((top_rb * (256 - fy) + bot_rb * fy) >> 8) & 0x00ff_00ff;

                let top_g = (((p00 & 0x0000_ff00) * (256 - fx) + (p10 & 0x0000_ff00) * fx) >> 8)
                    & 0x0000_ff00;
                let bot_g = (((p01 & 0x0000_ff00) * (256 - fx) + (p11 & 0x0000_ff00) * fx) >> 8)
                    & 0x0000_ff00;
                let g = ((top_g * (256 - fy) + bot_g * fy) >> 8) & 0x0000_ff00;

                *slot = rb | g;
            }
        }
        out
    }

    /// Bilinear stretch scaling of the entire sprite into an opaque `0x00RRGGBB` buffer.
    pub fn scale_stretch_opaque(&self, dst_w: usize, dst_h: usize) -> Vec<u32> {
        if dst_w == 0 || dst_h == 0 || self.width == 0 || self.height == 0 {
            return vec![0; dst_w * dst_h];
        }
        let sw = self.width as usize;
        let sh = self.height as usize;
        let mut out = vec![0u32; dst_w * dst_h];
        for dy in 0..dst_h {
            let sy_fp = ((dy * sh * 256) / dst_h).min((sh - 1) * 256);
            let sy0 = sy_fp >> 8;
            let sy1 = (sy0 + 1).min(sh - 1);
            let fy = (sy_fp & 0xff) as u32;
            let row0 = &self.pixels[sy0 * sw..(sy0 + 1) * sw];
            let row1 = &self.pixels[sy1 * sw..(sy1 + 1) * sw];
            let out_row = &mut out[dy * dst_w..(dy + 1) * dst_w];

            for (dx, slot) in out_row.iter_mut().enumerate() {
                let sx_fp = ((dx * sw * 256) / dst_w).min((sw - 1) * 256);
                let sx0 = sx_fp >> 8;
                let sx1 = (sx0 + 1).min(sw - 1);
                let fx = (sx_fp & 0xff) as u32;

                let p00 = row0[sx0] & 0x00ff_ffff;
                let p10 = row0[sx1] & 0x00ff_ffff;
                let p01 = row1[sx0] & 0x00ff_ffff;
                let p11 = row1[sx1] & 0x00ff_ffff;

                let top_rb = (((p00 & 0x00ff_00ff) * (256 - fx) + (p10 & 0x00ff_00ff) * fx) >> 8)
                    & 0x00ff_00ff;
                let bot_rb = (((p01 & 0x00ff_00ff) * (256 - fx) + (p11 & 0x00ff_00ff) * fx) >> 8)
                    & 0x00ff_00ff;
                let rb = ((top_rb * (256 - fy) + bot_rb * fy) >> 8) & 0x00ff_00ff;

                let top_g = (((p00 & 0x0000_ff00) * (256 - fx) + (p10 & 0x0000_ff00) * fx) >> 8)
                    & 0x0000_ff00;
                let bot_g = (((p01 & 0x0000_ff00) * (256 - fx) + (p11 & 0x0000_ff00) * fx) >> 8)
                    & 0x0000_ff00;
                let g = ((top_g * (256 - fy) + bot_g * fy) >> 8) & 0x0000_ff00;

                *slot = rb | g;
            }
        }
        out
    }

    /// Aspect-preserving letterboxed scaling into an opaque `0x00RRGGBB` buffer filled with `bg`.
    pub fn scale_contain_opaque(&self, dst_w: usize, dst_h: usize, bg: u32) -> Vec<u32> {
        let fill = bg & 0x00ff_ffff;
        let mut out = vec![fill; dst_w * dst_h];
        if dst_w == 0 || dst_h == 0 || self.width == 0 || self.height == 0 {
            return out;
        }
        let sw = self.width as usize;
        let sh = self.height as usize;
        let (fit_w, fit_h) = if sw * dst_h > sh * dst_w {
            (dst_w, (dst_w * sh / sw).max(1).min(dst_h))
        } else {
            ((dst_h * sw / sh).max(1).min(dst_w), dst_h)
        };
        let off_x = (dst_w - fit_w) / 2;
        let off_y = (dst_h - fit_h) / 2;
        let scaled = self.scale_stretch_opaque(fit_w, fit_h);
        for ry in 0..fit_h {
            let src_row = &scaled[ry * fit_w..(ry + 1) * fit_w];
            let dst_start = (off_y + ry) * dst_w + off_x;
            out[dst_start..dst_start + fit_w].copy_from_slice(src_row);
        }
        out
    }

    /// Bilinear scale into a `dst_w × dst_h` premultiplied `Sprite` (e.g. PNG logo).
    pub fn scale_bilinear(&self, dst_w: u32, dst_h: u32) -> Self {
        let (dw, dh) = (dst_w.max(1) as usize, dst_h.max(1) as usize);
        let (sw, sh) = (self.width as usize, self.height as usize);
        if sw == 0 || sh == 0 {
            return Self {
                width: dst_w,
                height: dst_h,
                pixels: vec![0; dw * dh],
            };
        }
        if sw == dw && sh == dh {
            return self.clone();
        }
        let mut pixels = vec![0u32; dw * dh];
        for dy in 0..dh {
            let sy_fp = ((dy * sh * 256 + 128) / dh)
                .saturating_sub(128)
                .min((sh - 1) * 256);
            let sy0 = sy_fp >> 8;
            let sy1 = (sy0 + 1).min(sh - 1);
            let fy = (sy_fp & 0xff) as u32;
            for dx in 0..dw {
                let sx_fp = ((dx * sw * 256 + 128) / dw)
                    .saturating_sub(128)
                    .min((sw - 1) * 256);
                let sx0 = sx_fp >> 8;
                let sx1 = (sx0 + 1).min(sw - 1);
                let fx = (sx_fp & 0xff) as u32;

                let c00 = self.pixels[sy0 * sw + sx0];
                let c10 = self.pixels[sy0 * sw + sx1];
                let c01 = self.pixels[sy1 * sw + sx0];
                let c11 = self.pixels[sy1 * sw + sx1];

                let mut out_px = 0u32;
                for shift in [0, 8, 16, 24] {
                    let v00 = (c00 >> shift) & 0xff;
                    let v10 = (c10 >> shift) & 0xff;
                    let v01 = (c01 >> shift) & 0xff;
                    let v11 = (c11 >> shift) & 0xff;
                    let top = (v00 * (256 - fx) + v10 * fx) >> 8;
                    let bot = (v01 * (256 - fx) + v11 * fx) >> 8;
                    let val = ((top * (256 - fy) + bot * fy) >> 8) & 0xff;
                    out_px |= val << shift;
                }
                pixels[dy * dw + dx] = out_px;
            }
        }
        Self {
            width: dst_w,
            height: dst_h,
            pixels,
        }
    }
}

pub enum PixelBuffer {
    Heap(Vec<u32>),
    Shm { ptr: *mut u32, len: usize, seg: u32 },
}

// SAFETY: `PixelBuffer::Shm` points to an exclusive SysV SHM mapping owned by this Canvas.
unsafe impl Send for PixelBuffer {}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        if let Self::Shm { ptr, .. } = *self {
            if !ptr.is_null() {
                unsafe {
                    libc::shmdt(ptr as *const libc::c_void);
                }
            }
        }
    }
}

impl PixelBuffer {
    #[inline(always)]
    pub fn as_slice(&self) -> &[u32] {
        match self {
            Self::Heap(vec) => vec.as_slice(),
            Self::Shm { ptr, len, .. } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
        }
    }

    #[inline(always)]
    pub fn as_mut_slice(&mut self) -> &mut [u32] {
        match self {
            Self::Heap(vec) => vec.as_mut_slice(),
            Self::Shm { ptr, len, .. } => unsafe { std::slice::from_raw_parts_mut(*ptr, *len) },
        }
    }

    pub fn shm_seg(&self) -> Option<u32> {
        match self {
            Self::Heap(_) => None,
            Self::Shm { seg, .. } => Some(*seg),
        }
    }
}

struct RoundedRowCorner {
    x_start: i32,
    x_end: i32,
    cx: f32,
    dy: f32,
    rf: f32,
}

enum RoundedRowSpans {
    Circle {
        cx: f32,
        dy: f32,
        rf: f32,
    },
    Rect {
        mid_l: i32,
        mid_r: i32,
        left: Option<RoundedRowCorner>,
        right: Option<RoundedRowCorner>,
    },
}

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub buffer: PixelBuffer,
    clip: Rect,
    dirty: Rect,
    pub argb: bool,
}

impl Canvas {
    pub fn new(width: usize, height: usize, background: u32) -> Self {
        let full = Rect::new(0, 0, width as i32, height as i32);
        Self {
            width,
            height,
            buffer: PixelBuffer::Heap(vec![background; width * height]),
            clip: full,
            dirty: full,
            argb: false,
        }
    }

    pub fn from_shm(width: usize, height: usize, ptr: *mut u32, seg: u32, background: u32) -> Self {
        let len = width * height;
        let mut buffer = PixelBuffer::Shm { ptr, len, seg };
        buffer.as_mut_slice().fill(background);
        let full = Rect::new(0, 0, width as i32, height as i32);
        Self {
            width,
            height,
            buffer,
            clip: full,
            dirty: full,
            argb: false,
        }
    }

    pub fn set_argb(&mut self, enabled: bool) {
        self.argb = enabled;
    }

    pub const fn is_argb(&self) -> bool {
        self.argb
    }

    /// Consume this offscreen canvas into a premultiplied `Sprite` without copying heap buffers.
    pub fn into_sprite(mut self) -> Sprite {
        let pixels = match &mut self.buffer {
            PixelBuffer::Heap(vec) => std::mem::take(vec),
            PixelBuffer::Shm { .. } => self.buffer.as_slice().to_vec(),
        };
        Sprite {
            width: self.width as u32,
            height: self.height as u32,
            pixels,
        }
    }

    #[inline(always)]
    const fn opaque_color(&self, color: u32) -> u32 {
        if self.argb {
            0xff00_0000 | (color & 0x00ff_ffff)
        } else {
            color & 0x00ff_ffff
        }
    }

    #[inline(always)]
    const fn mix_pixel(&self, dst: u32, color: u32, alpha: u8) -> u32 {
        if self.argb {
            src_over_u8(dst, color, alpha)
        } else {
            blend_u8(dst, color, alpha)
        }
    }

    #[inline(always)]
    const fn mix_premul(&self, dst: u32, premul_src: u32) -> u32 {
        if self.argb {
            src_over_premul(dst, premul_src)
        } else {
            blend_premul(dst, premul_src)
        }
    }

    #[inline(always)]
    pub fn pixels(&self) -> &[u32] {
        self.buffer.as_slice()
    }

    #[inline(always)]
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        self.buffer.as_mut_slice()
    }

    pub fn shm_seg(&self) -> Option<u32> {
        self.buffer.shm_seg()
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    pub fn with_clip(&mut self, rect: Rect, f: impl FnOnce(&mut Self)) {
        let prev = self.clip;
        self.clip = prev.intersect(rect);
        f(self);
        self.clip = prev;
    }

    #[inline(always)]
    pub const fn clip(&self) -> Rect {
        self.clip
    }

    #[inline(always)]
    pub fn clip_rect(&self, rect: Rect) -> Rect {
        rect.intersect(self.clip)
    }

    /// Fast row-wise copy from an opaque `0xRRGGBB` source slice over `rect` clipped to `self.clip`.
    pub fn copy_rect_from(&mut self, src: &[u32], src_w: usize, rect: Rect) {
        let clip = self.clip_rect(rect);
        if clip.is_empty() || src_w == 0 {
            return;
        }
        let max_src_rows = src.len() / src_w;
        let max_y = clip.bottom().min(max_src_rows as i32);
        let max_x = clip.right().min(src_w as i32);
        if max_y <= clip.y || max_x <= clip.x {
            return;
        }
        let valid = Rect::new(clip.x, clip.y, max_x - clip.x, max_y - clip.y);
        self.mark_dirty(valid);
        let dw = self.width;
        let dst = self.buffer.as_mut_slice();
        let cnt = valid.w as usize;
        let sx = valid.x as usize;
        for py in valid.y..valid.bottom() {
            let y = py as usize;
            let s_off = y * src_w + sx;
            let d_off = y * dw + sx;
            dst[d_off..d_off + cnt].copy_from_slice(&src[s_off..s_off + cnt]);
        }
    }

    #[inline(always)]
    pub fn mark_dirty(&mut self, rect: Rect) {
        let clipped = rect.intersect(self.bounds());
        if !clipped.is_empty() {
            self.dirty = self.dirty.union(clipped);
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty = self.bounds();
    }

    pub fn take_dirty(&mut self) -> Rect {
        let out = self.dirty;
        self.dirty = Rect::default();
        out
    }

    pub fn clear(&mut self, color: u32) {
        let px = self.opaque_color(color);
        self.pixels_mut().fill(px);
        self.mark_all_dirty();
    }

    /// Clear `rect` to full transparent `0x0000_0000` (for ARGB glass regions)
    /// when `self.argb` is enabled, otherwise fills with `fallback`.
    pub fn clear_transparent(&mut self, rect: Rect, fallback: u32) {
        if !self.argb {
            self.fill(rect, fallback);
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        for y in clip.y..clip.bottom() {
            let start = y as usize * width + clip.x as usize;
            pixels[start..start + clip.w as usize].fill(0);
        }
    }

    /// Ensure every pixel inside `rect` carries `alpha = 0xFF` on 32-bit ARGB canvases.
    pub fn enforce_opaque_rect(&mut self, rect: Rect) {
        let clip = rect.intersect(self.bounds());
        if clip.is_empty() {
            return;
        }
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        for y in clip.y..clip.bottom() {
            let start = y as usize * width + clip.x as usize;
            for px in &mut pixels[start..start + clip.w as usize] {
                *px |= 0xff00_0000;
            }
        }
    }

    /// Anti-alias outer `radius x radius` window corners in the ARGB alpha channel.
    pub fn mask_rounded_corners(&mut self, radius: i32) {
        self.mask_rounded_corners_rect(radius, self.bounds());
    }

    /// Anti-alias outer `radius x radius` window corners inside `area` so partial
    /// damage updates do not re-darken untouched corner pixels.
    pub fn mask_rounded_corners_rect(&mut self, radius: i32, area: Rect) {
        self.mask_top_bottom_corners_rect(radius, radius, area);
    }

    /// Anti-alias top (`top_radius`) and bottom (`bottom_radius`) window corners independently.
    pub fn mask_top_bottom_corners_rect(&mut self, top_radius: i32, bottom_radius: i32, area: Rect) {
        if (top_radius <= 0 && bottom_radius <= 0) || self.width < 2 || self.height < 2 {
            return;
        }
        let area = area.intersect(self.bounds());
        if area.is_empty() {
            return;
        }
        let w = self.width as i32;
        let h = self.height as i32;
        let tr = top_radius.max(0).min(w / 2).min(h / 2);
        let br = bottom_radius.max(0).min(w / 2).min(h / 2);
        let stride = self.width;
        let pixels = self.buffer.as_mut_slice();
        let mut corners: [(i32, i32, i32, f32, f32); 4] = [(0, 0, 0, 0.0, 0.0); 4];
        let mut count = 0;
        if tr > 0 {
            let rf = tr as f32;
            corners[count] = (0, 0, tr, rf, rf);
            count += 1;
            corners[count] = (w - tr, 0, tr, (w - tr) as f32, rf);
            count += 1;
        }
        if br > 0 {
            let rf = br as f32;
            corners[count] = (0, h - br, br, rf, (h - br) as f32);
            count += 1;
            corners[count] = (w - br, h - br, br, (w - br) as f32, (h - br) as f32);
            count += 1;
        }
        for &(bx, by, r, cx, cy) in &corners[..count] {
            let corner_clip = Rect::new(bx, by, r, r).intersect(area);
            if corner_clip.is_empty() {
                continue;
            }
            let rf = r as f32;
            for y in corner_clip.y..corner_clip.bottom() {
                let dy = y as f32 + 0.5 - cy;
                let row = y as usize * stride;
                for x in corner_clip.x..corner_clip.right() {
                    let dx = x as f32 + 0.5 - cx;
                    let cov = Self::corner_coverage(dx, dy, rf);
                    if cov < 1.0 {
                        let slot = &mut pixels[row + x as usize];
                        if cov <= 0.0 {
                            *slot = 0;
                        } else {
                            *slot = scale_premul(*slot, f32_to_u8(cov));
                        }
                    }
                }
            }
        }
    }

    pub fn copy_background(&mut self, bg: &[u32]) {
        let argb = self.argb;
        let dst = self.pixels_mut();
        if bg.len() == dst.len() {
            if argb {
                for (d, &s) in dst.iter_mut().zip(bg.iter()) {
                    *d = 0xff00_0000 | (s & 0x00ff_ffff);
                }
            } else {
                dst.copy_from_slice(bg);
            }
            self.mark_all_dirty();
        }
    }

    #[inline(always)]
    pub fn blend_pixel_u8(&mut self, x: i32, y: i32, color: u32, alpha: u8) {
        if alpha == 0 || !self.clip.contains(x, y) {
            return;
        }
        let idx = y as usize * self.width + x as usize;
        let argb = self.argb;
        let pixels = self.buffer.as_mut_slice();
        pixels[idx] = if argb {
            src_over_u8(pixels[idx], color, alpha)
        } else {
            blend_u8(pixels[idx], color, alpha)
        };
    }

    #[inline]
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: u32, alpha: f32) {
        self.blend_pixel_u8(x, y, color, f32_to_u8(alpha));
    }

    pub fn fill(&mut self, rect: Rect, color: u32) {
        self.fill_u8(rect, color, 255);
    }

    pub fn fill_alpha(&mut self, rect: Rect, color: u32, alpha: f32) {
        self.fill_u8(rect, color, f32_to_u8(alpha));
    }

    pub fn fill_u8(&mut self, rect: Rect, color: u32, alpha: u8) {
        let clip = self.clip_rect(rect);
        if clip.is_empty() || alpha == 0 {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        let opaque = if argb {
            0xff00_0000 | (color & 0x00ff_ffff)
        } else {
            color & 0x00ff_ffff
        };
        if alpha == 255 {
            for y in clip.y..clip.bottom() {
                let start = y as usize * width + clip.x as usize;
                pixels[start..start + clip.w as usize].fill(opaque);
            }
        } else if argb {
            for y in clip.y..clip.bottom() {
                let start = y as usize * width + clip.x as usize;
                for slot in &mut pixels[start..start + clip.w as usize] {
                    *slot = src_over_u8(*slot, color, alpha);
                }
            }
        } else {
            for y in clip.y..clip.bottom() {
                let start = y as usize * width + clip.x as usize;
                for slot in &mut pixels[start..start + clip.w as usize] {
                    *slot = blend_u8(*slot, color, alpha);
                }
            }
        }
    }

    pub fn border_1px(&mut self, rect: Rect, color: u32) {
        if rect.is_empty() {
            return;
        }
        self.fill(Rect::new(rect.x, rect.y, rect.w, 1), color);
        self.fill(Rect::new(rect.x, rect.bottom() - 1, rect.w, 1), color);
        if rect.h > 2 {
            self.fill(Rect::new(rect.x, rect.y + 1, 1, rect.h - 2), color);
            self.fill(
                Rect::new(rect.right() - 1, rect.y + 1, 1, rect.h - 2),
                color,
            );
        }
    }

    pub fn vertical_gradient(&mut self, rect: Rect, top: u32, bottom: u32) {
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let span = rect.h.max(1) as u32;
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        for y in clip.y..clip.bottom() {
            let t = (((y - rect.y).max(0) as u32 * 255) / span).min(255) as u8;
            let raw = blend_u8(top, bottom, t);
            let color = if argb { 0xff00_0000 | raw } else { raw };
            let start = y as usize * width + clip.x as usize;
            pixels[start..start + clip.w as usize].fill(color);
        }
    }

    pub fn horizontal_gradient(&mut self, rect: Rect, left: u32, right: u32) {
        if left == right {
            self.fill(rect, left);
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let span = (rect.w - 1).max(1) as u32;
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        let first_start = clip.y as usize * width + clip.x as usize;
        for dx in 0..clip.w {
            let x = clip.x + dx;
            let t = (((x - rect.x).max(0) as u32 * 255) / span).min(255) as u8;
            let raw = blend_u8(left, right, t);
            pixels[first_start + dx as usize] = if argb { 0xff00_0000 | raw } else { raw };
        }
        let row_len = clip.w as usize;
        for y in (clip.y + 1)..clip.bottom() {
            let dst_start = y as usize * width + clip.x as usize;
            pixels.copy_within(first_start..(first_start + row_len), dst_start);
        }
    }

    /// Paint a single-pixel 3D bevel frame strictly inside `rect`.
    pub fn bevel_1px(&mut self, rect: Rect, top_left: u32, bottom_right: u32) {
        if rect.w < 2 || rect.h < 2 {
            return;
        }
        self.fill(Rect::new(rect.x, rect.y, rect.w - 1, 1), top_left);
        self.fill(Rect::new(rect.x, rect.y + 1, 1, rect.h - 2), top_left);
        self.fill(
            Rect::new(rect.x, rect.bottom() - 1, rect.w, 1),
            bottom_right,
        );
        self.fill(
            Rect::new(rect.right() - 1, rect.y, 1, rect.h - 1),
            bottom_right,
        );
    }

    /// Paint a classic 2px-per-side Win98 4-color 3D bevel strictly inside `rect`.
    pub fn bevel(
        &mut self,
        rect: Rect,
        top_outer: u32,
        top_inner: u32,
        bottom_outer: u32,
        bottom_inner: u32,
    ) {
        self.bevel_1px(rect, top_outer, bottom_outer);
        let inner = rect.inset(1);
        if inner.w >= 2 && inner.h >= 2 {
            self.bevel_1px(inner, top_inner, bottom_inner);
        }
    }

    /// Paint a classic Windows 98 1px alternating-pixel dotted focus outline strictly inside `rect`.
    pub fn dotted_border_1px(&mut self, rect: Rect, color: u32) {
        if rect.w < 2 || rect.h < 2 {
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let color = self.opaque_color(color);
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        let top_y = rect.y;
        let bot_y = rect.bottom() - 1;
        let left_x = rect.x;
        let right_x = rect.right() - 1;

        for x in clip.x..clip.right() {
            if top_y >= clip.y && top_y < clip.bottom() && ((x - rect.x) & 1 == 0) {
                pixels[top_y as usize * width + x as usize] = color;
            }
            if bot_y >= clip.y
                && bot_y < clip.bottom()
                && ((x - rect.x + (bot_y - rect.y)) & 1 == 0)
            {
                pixels[bot_y as usize * width + x as usize] = color;
            }
        }
        for y in clip.y..clip.bottom() {
            if left_x >= clip.x && left_x < clip.right() && ((y - rect.y) & 1 == 0) {
                pixels[y as usize * width + left_x as usize] = color;
            }
            if right_x >= clip.x
                && right_x < clip.right()
                && (((right_x - rect.x) + (y - rect.y)) & 1 == 0)
            {
                pixels[y as usize * width + right_x as usize] = color;
            }
        }
    }

    /// Fill `rect` with a classic Windows 98 1-bit checkerboard dither pattern (`(x + y) & 1`).
    pub fn dither_checkerboard(&mut self, rect: Rect, even_color: u32, odd_color: u32) {
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let c0 = self.opaque_color(even_color);
        let c1 = self.opaque_color(odd_color);
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        for y in clip.y..clip.bottom() {
            let row = y as usize * width;
            for x in clip.x..clip.right() {
                pixels[row + x as usize] = if (x + y) & 1 == 0 { c0 } else { c1 };
            }
        }
    }

    /// Corner distance for a point `(px, py)` relative to one of the 4 corner centers.
    #[inline(always)]
    fn corner_coverage(dx: f32, dy: f32, r: f32) -> f32 {
        let dist = (dx * dx + dy * dy).sqrt();
        (r + 0.5 - dist).clamp(0.0, 1.0)
    }

    pub fn rounded_rect(&mut self, rect: Rect, radius: i32, color: u32, alpha: f32) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        if rect.w == rect.h && radius * 2 >= rect.w && rect.w >= 4 {
            let clip = self.clip_rect(rect);
            if clip.is_empty() {
                return;
            }
            self.mark_dirty(clip);
            let cx = rect.x as f32 + rect.w as f32 * 0.5;
            let cy = rect.y as f32 + rect.h as f32 * 0.5;
            let rf = rect.w as f32 * 0.5;
            for y in clip.y..clip.bottom() {
                let dy = y as f32 + 0.5 - cy;
                for x in clip.x..clip.right() {
                    let dx = x as f32 + 0.5 - cx;
                    let cov = Self::corner_coverage(dx, dy, rf);
                    if cov > 0.0 {
                        self.blend_pixel_u8(x, y, color, ((a_u8 as f32) * cov + 0.5) as u8);
                    }
                }
            }
            return;
        }
        let r = radius.max(0).min(rect.w / 2).min(rect.h / 2);
        if r == 0 {
            self.fill_u8(rect, color, a_u8);
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        // 1) Center horizontal band [rect.y + r .. rect.bottom() - r]: pure span fill!
        if rect.h > r * 2 {
            self.fill_u8(
                Rect::new(rect.x, rect.y + r, rect.w, rect.h - r * 2),
                color,
                a_u8,
            );
        }
        // 2) Top & bottom center spans [rect.x + r .. rect.right() - r]: pure span fill!
        if rect.w > r * 2 {
            self.fill_u8(
                Rect::new(rect.x + r, rect.y, rect.w - r * 2, r),
                color,
                a_u8,
            );
            self.fill_u8(
                Rect::new(rect.x + r, rect.bottom() - r, rect.w - r * 2, r),
                color,
                a_u8,
            );
        }
        // 3) Only the 4 r x r corner squares compute Euclidean AA!
        let rf = r as f32;
        let corners = [
            (rect.x, rect.y, rect.x as f32 + rf, rect.y as f32 + rf),
            (
                rect.right() - r,
                rect.y,
                (rect.right() - r) as f32,
                rect.y as f32 + rf,
            ),
            (
                rect.x,
                rect.bottom() - r,
                rect.x as f32 + rf,
                (rect.bottom() - r) as f32,
            ),
            (
                rect.right() - r,
                rect.bottom() - r,
                (rect.right() - r) as f32,
                (rect.bottom() - r) as f32,
            ),
        ];
        for (bx, by, cx, cy) in corners {
            let cbox = self.clip_rect(Rect::new(bx, by, r, r));
            for y in cbox.y..cbox.bottom() {
                let dy = y as f32 + 0.5 - cy;
                for x in cbox.x..cbox.right() {
                    let dx = x as f32 + 0.5 - cx;
                    let cov = Self::corner_coverage(dx, dy, rf);
                    if cov > 0.0 {
                        let px_a = ((a_u8 as f32) * cov + 0.5) as u8;
                        self.blend_pixel_u8(x, y, color, px_a);
                    }
                }
            }
        }
    }

    fn fill_rounded_row(
        &mut self,
        rect: Rect,
        clip: Rect,
        r: i32,
        y: i32,
        row_color: u32,
        a_u8: u8,
    ) {
        let in_top = y < rect.y + r;
        let in_bot = y >= rect.bottom() - r;
        if !in_top && !in_bot {
            self.fill_u8(Rect::new(clip.x, y, clip.w, 1), row_color, a_u8);
            return;
        }
        let rf = r as f32;
        let mid_l = (rect.x + r).max(clip.x);
        let mid_r = (rect.right() - r).min(clip.right());
        if mid_r > mid_l {
            self.fill_u8(Rect::new(mid_l, y, mid_r - mid_l, 1), row_color, a_u8);
        }
        let cy = if in_top {
            rect.y as f32 + rf
        } else {
            (rect.bottom() - r) as f32
        };
        let dy = y as f32 + 0.5 - cy;
        for x in clip.x..mid_l.min(clip.right()) {
            let dx = x as f32 + 0.5 - (rect.x as f32 + rf);
            let cov = Self::corner_coverage(dx, dy, rf);
            if cov > 0.0 {
                self.blend_pixel_u8(x, y, row_color, ((a_u8 as f32) * cov + 0.5) as u8);
            }
        }
        for x in mid_r.max(clip.x)..clip.right() {
            let dx = x as f32 + 0.5 - (rect.right() - r) as f32;
            let cov = Self::corner_coverage(dx, dy, rf);
            if cov > 0.0 {
                self.blend_pixel_u8(x, y, row_color, ((a_u8 as f32) * cov + 0.5) as u8);
            }
        }
    }

    pub fn rounded_gradient(&mut self, rect: Rect, radius: i32, top: u32, bottom: u32, alpha: f32) {
        if top == bottom {
            self.rounded_rect(rect, radius, top, alpha);
            return;
        }
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let r = radius.max(0).min(rect.w / 2).min(rect.h / 2);
        if r == 0 && a_u8 == 255 {
            self.vertical_gradient(rect, top, bottom);
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let span = rect.h.max(1) as u32;
        for y in clip.y..clip.bottom() {
            let t = (((y - rect.y).max(0) as u32 * 255) / span).min(255) as u8;
            let row_color = blend_u8(top, bottom, t);
            self.fill_rounded_row(rect, clip, r, y, row_color, a_u8);
        }
    }

    /// Fill an anti-aliased rounded rectangle with Windows 7 4-stop vertical gloss (`top_hi->top_lo` | `bot_hi->bot_lo`).
    #[allow(clippy::too_many_arguments)]
    pub fn rounded_glossy_4stop(
        &mut self,
        rect: Rect,
        radius: i32,
        top_hi: u32,
        top_lo: u32,
        bot_hi: u32,
        bot_lo: u32,
        alpha: f32,
    ) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let r = radius.max(0).min(rect.w / 2).min(rect.h / 2);
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let top_h = (rect.h / 2).max(1);
        let bot_h = (rect.h - top_h).max(1);
        for y in clip.y..clip.bottom() {
            let rel = y - rect.y;
            let row_color = if rel < top_h {
                let t = if top_h <= 1 {
                    0
                } else {
                    (((rel.max(0) as u32) * 255) / (top_h - 1) as u32).min(255) as u8
                };
                blend_u8(top_hi, top_lo, t)
            } else {
                let brel = rel - top_h;
                let t = if bot_h <= 1 {
                    0
                } else {
                    (((brel.max(0) as u32) * 255) / (bot_h - 1) as u32).min(255) as u8
                };
                blend_u8(bot_hi, bot_lo, t)
            };
            self.fill_rounded_row(rect, clip, r, y, row_color, a_u8);
        }
    }

    pub fn rounded_border(&mut self, rect: Rect, radius: i32, color: u32, alpha: f32, width: i32) {
        let a_u8 = f32_to_u8(alpha);
        let bw = width.max(1);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        if rect.w == rect.h && radius * 2 >= rect.w && rect.w >= 4 {
            let clip = self.clip_rect(rect);
            if clip.is_empty() {
                return;
            }
            self.mark_dirty(clip);
            let cx = rect.x as f32 + rect.w as f32 * 0.5;
            let cy = rect.y as f32 + rect.h as f32 * 0.5;
            let outer_r = rect.w as f32 * 0.5;
            let inner_r = (outer_r - bw as f32).max(0.0);
            for y in clip.y..clip.bottom() {
                let dy = y as f32 + 0.5 - cy;
                for x in clip.x..clip.right() {
                    let dx = x as f32 + 0.5 - cx;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let outer_cov = (outer_r + 0.5 - dist).clamp(0.0, 1.0);
                    let inner_cov = (dist - inner_r + 0.5).clamp(0.0, 1.0);
                    let cov = outer_cov * inner_cov;
                    if cov > 0.0 {
                        self.blend_pixel_u8(x, y, color, ((a_u8 as f32) * cov + 0.5) as u8);
                    }
                }
            }
            return;
        }
        let r = radius.max(0).min(rect.w / 2).min(rect.h / 2);
        if r == 0 && bw == 1 && a_u8 == 255 {
            self.border_1px(rect, color);
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        // Straight top, bottom, left, right border segments: zero sqrt!
        if rect.w > r * 2 {
            self.fill_u8(
                Rect::new(rect.x + r, rect.y, rect.w - r * 2, bw),
                color,
                a_u8,
            );
            self.fill_u8(
                Rect::new(rect.x + r, rect.bottom() - bw, rect.w - r * 2, bw),
                color,
                a_u8,
            );
        }
        if rect.h > r * 2 {
            self.fill_u8(
                Rect::new(rect.x, rect.y + r, bw, rect.h - r * 2),
                color,
                a_u8,
            );
            self.fill_u8(
                Rect::new(rect.right() - bw, rect.y + r, bw, rect.h - r * 2),
                color,
                a_u8,
            );
        }
        // Only the 4 r x r corner boxes compute ring AA!
        if r > 0 {
            let rf = r as f32;
            let inner_r = (rf - bw as f32).max(0.0);
            let corners = [
                (rect.x, rect.y, rect.x as f32 + rf, rect.y as f32 + rf),
                (
                    rect.right() - r,
                    rect.y,
                    (rect.right() - r) as f32,
                    rect.y as f32 + rf,
                ),
                (
                    rect.x,
                    rect.bottom() - r,
                    rect.x as f32 + rf,
                    (rect.bottom() - r) as f32,
                ),
                (
                    rect.right() - r,
                    rect.bottom() - r,
                    (rect.right() - r) as f32,
                    (rect.bottom() - r) as f32,
                ),
            ];
            for (bx, by, cx, cy) in corners {
                let cbox = self.clip_rect(Rect::new(bx, by, r, r));
                for y in cbox.y..cbox.bottom() {
                    let dy = y as f32 + 0.5 - cy;
                    for x in cbox.x..cbox.right() {
                        let dx = x as f32 + 0.5 - cx;
                        let dist = (dx * dx + dy * dy).sqrt();
                        let outer_cov = (rf + 0.5 - dist).clamp(0.0, 1.0);
                        let inner_cov = (dist - inner_r + 0.5).clamp(0.0, 1.0);
                        let cov = outer_cov * inner_cov;
                        if cov > 0.0 {
                            self.blend_pixel_u8(x, y, color, ((a_u8 as f32) * cov + 0.5) as u8);
                        }
                    }
                }
            }
        }
    }

    /// Evaluate a sorted CSS multi-stop color ramp `[(pos_0_to_1, 0xRRGGBB)]` at `t` in `0.0..=1.0`.
    #[inline]
    pub fn sample_gradient_stops(stops: &[(f32, u32)], t: f32) -> u32 {
        if stops.is_empty() {
            return 0;
        }
        if stops.len() == 1 || t <= stops[0].0 {
            return stops[0].1;
        }
        let last = stops[stops.len() - 1];
        if t >= last.0 {
            return last.1;
        }
        for pair in stops.windows(2) {
            let (p0, c0) = pair[0];
            let (p1, c1) = pair[1];
            if t >= p0 && t <= p1 {
                let span = (p1 - p0).max(1e-5);
                let frac = ((t - p0) / span).clamp(0.0, 1.0);
                return blend_u8(c0, c1, (frac * 255.0 + 0.5) as u8);
            }
        }
        last.1
    }

    /// Compute anti-aliased coverage (`0.0..=1.0`) of `(x, y)` inside `rect` with 4-corner radii `[tl, tr, br, bl]`.
    #[inline]
    pub fn corner_point_coverage(rect: Rect, radii: [i32; 4], x: i32, y: i32) -> f32 {
        if !rect.contains(x, y) {
            return 0.0;
        }
        if rect.w == rect.h && radii.iter().all(|&r| r * 2 >= rect.w) && rect.w >= 4 {
            let cx = rect.x as f32 + rect.w as f32 * 0.5;
            let cy = rect.y as f32 + rect.h as f32 * 0.5;
            let rf = rect.w as f32 * 0.5;
            return Self::corner_coverage(x as f32 + 0.5 - cx, y as f32 + 0.5 - cy, rf);
        }
        let max_r = (rect.w / 2).min(rect.h / 2).max(0);
        let tl = radii[0].clamp(0, max_r);
        let tr = radii[1].clamp(0, max_r);
        let br = radii[2].clamp(0, max_r);
        let bl = radii[3].clamp(0, max_r);
        if tl > 0 && x < rect.x + tl && y < rect.y + tl {
            let rf = tl as f32;
            return Self::corner_coverage(
                x as f32 + 0.5 - (rect.x as f32 + rf),
                y as f32 + 0.5 - (rect.y as f32 + rf),
                rf,
            );
        }
        if tr > 0 && x >= rect.right() - tr && y < rect.y + tr {
            let rf = tr as f32;
            return Self::corner_coverage(
                x as f32 + 0.5 - (rect.right() - tr) as f32,
                y as f32 + 0.5 - (rect.y as f32 + rf),
                rf,
            );
        }
        if br > 0 && x >= rect.right() - br && y >= rect.bottom() - br {
            let rf = br as f32;
            return Self::corner_coverage(
                x as f32 + 0.5 - (rect.right() - br) as f32,
                y as f32 + 0.5 - (rect.bottom() - br) as f32,
                rf,
            );
        }
        if bl > 0 && x < rect.x + bl && y >= rect.bottom() - bl {
            let rf = bl as f32;
            return Self::corner_coverage(
                x as f32 + 0.5 - (rect.x as f32 + rf),
                y as f32 + 0.5 - (rect.bottom() - bl) as f32,
                rf,
            );
        }
        1.0
    }

    fn rounded_row_spans(
        rect: Rect,
        clip: Rect,
        radii: [i32; 4],
        y: i32,
    ) -> RoundedRowSpans {
        if rect.w == rect.h && radii.iter().all(|&r| r * 2 >= rect.w) && rect.w >= 4 {
            let cx = rect.x as f32 + rect.w as f32 * 0.5;
            let cy = rect.y as f32 + rect.h as f32 * 0.5;
            let rf = rect.w as f32 * 0.5;
            return RoundedRowSpans::Circle {
                cx,
                dy: y as f32 + 0.5 - cy,
                rf,
            };
        }
        let max_r = (rect.w / 2).min(rect.h / 2).max(0);
        let tl = radii[0].clamp(0, max_r);
        let tr = radii[1].clamp(0, max_r);
        let br = radii[2].clamp(0, max_r);
        let bl = radii[3].clamp(0, max_r);

        let left_r = if y < rect.y + tl {
            Some((tl, rect.y as f32 + tl as f32))
        } else if y >= rect.bottom() - bl && bl > 0 {
            Some((bl, (rect.bottom() - bl) as f32))
        } else {
            None
        };
        let right_r = if y < rect.y + tr {
            Some((tr, rect.y as f32 + tr as f32))
        } else if y >= rect.bottom() - br && br > 0 {
            Some((br, (rect.bottom() - br) as f32))
        } else {
            None
        };

        let l_inset = left_r.map_or(0, |(r, _)| r);
        let r_inset = right_r.map_or(0, |(r, _)| r);
        let mid_l = (rect.x + l_inset).max(clip.x);
        let mid_r = (rect.right() - r_inset).min(clip.right());
        let left = left_r.and_then(|(r, cy)| {
            let end = mid_l.min(clip.right());
            if clip.x < end {
                let rf = r as f32;
                Some(RoundedRowCorner {
                    x_start: clip.x,
                    x_end: end,
                    cx: rect.x as f32 + rf,
                    dy: y as f32 + 0.5 - cy,
                    rf,
                })
            } else {
                None
            }
        });
        let right = right_r.and_then(|(r, cy)| {
            let start = mid_r.max(clip.x);
            if start < clip.right() {
                Some(RoundedRowCorner {
                    x_start: start,
                    x_end: clip.right(),
                    cx: (rect.right() - r) as f32,
                    dy: y as f32 + 0.5 - cy,
                    rf: r as f32,
                })
            } else {
                None
            }
        });
        RoundedRowSpans::Rect {
            mid_l,
            mid_r,
            left,
            right,
        }
    }

    fn fill_rounded_row_corners(
        &mut self,
        rect: Rect,
        clip: Rect,
        radii: [i32; 4],
        y: i32,
        row_color: u32,
        a_u8: u8,
    ) {
        match Self::rounded_row_spans(rect, clip, radii, y) {
            RoundedRowSpans::Circle { cx, dy, rf } => {
                for x in clip.x..clip.right() {
                    let cov = Self::corner_coverage(x as f32 + 0.5 - cx, dy, rf);
                    if cov > 0.0 {
                        self.blend_pixel_u8(x, y, row_color, ((a_u8 as f32) * cov + 0.5) as u8);
                    }
                }
            }
            RoundedRowSpans::Rect {
                mid_l,
                mid_r,
                left,
                right,
            } => {
                if mid_r > mid_l {
                    self.fill_u8(Rect::new(mid_l, y, mid_r - mid_l, 1), row_color, a_u8);
                }
                for corner in [left, right].into_iter().flatten() {
                    for x in corner.x_start..corner.x_end {
                        let cov = Self::corner_coverage(
                            x as f32 + 0.5 - corner.cx,
                            corner.dy,
                            corner.rf,
                        );
                        if cov > 0.0 {
                            self.blend_pixel_u8(
                                x,
                                y,
                                row_color,
                                ((a_u8 as f32) * cov + 0.5) as u8,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Fill `rect` with per-corner border radii `[tl, tr, br, bl]`.
    pub fn rounded_rect_corners(
        &mut self,
        rect: Rect,
        radii: [i32; 4],
        color: u32,
        alpha: f32,
    ) {
        if radii[0] == radii[1] && radii[1] == radii[2] && radii[2] == radii[3] {
            self.rounded_rect(rect, radii[0], color, alpha);
            return;
        }
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        for y in clip.y..clip.bottom() {
            self.fill_rounded_row_corners(rect, clip, radii, y, color, a_u8);
        }
    }

    /// Fill `rect` with a multi-stop vertical CSS `linear-gradient(to bottom, ...)` using a pre-baked 256-entry LUT.
    pub fn multi_stop_gradient_v_lut(
        &mut self,
        rect: Rect,
        lut: &[u32; 256],
        radii: [i32; 4],
        alpha: f32,
    ) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let span = (rect.h - 1).max(1);
        for y in clip.y..clip.bottom() {
            let idx = (((y - rect.y) * 255 + span / 2) / span).clamp(0, 255) as usize;
            self.fill_rounded_row_corners(rect, clip, radii, y, lut[idx], a_u8);
        }
    }

    /// Fill `rect` with a multi-stop vertical CSS `linear-gradient(to bottom, ...)` and 4-corner radii.
    pub fn multi_stop_gradient_v(
        &mut self,
        rect: Rect,
        stops: &[(f32, u32)],
        radii: [i32; 4],
        alpha: f32,
    ) {
        if stops.is_empty() {
            return;
        }
        let lut = build_gradient_lut(stops);
        self.multi_stop_gradient_v_lut(rect, &lut, radii, alpha);
    }

    /// Fill `rect` with a multi-stop horizontal CSS `linear-gradient(to right, ...)` using a pre-baked 256-entry LUT.
    pub fn multi_stop_gradient_h_lut(
        &mut self,
        rect: Rect,
        lut: &[u32; 256],
        radii: [i32; 4],
        alpha: f32,
    ) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);

        let w = clip.w as usize;
        let mut stack_buf = [0u32; 1024];
        let mut heap_buf = Vec::new();
        let row_cols: &mut [u32] = if w <= stack_buf.len() {
            &mut stack_buf[..w]
        } else {
            heap_buf.resize(w, 0);
            heap_buf.as_mut_slice()
        };
        let span = (rect.w - 1).max(1);
        let argb = self.argb;
        let fill_mask = if a_u8 == 255 && argb { 0xff00_0000 } else { 0 };
        for (i, slot) in row_cols.iter_mut().enumerate() {
            let dx = (clip.x - rect.x) + i as i32;
            let idx = ((dx * 255 + span / 2) / span).clamp(0, 255) as usize;
            *slot = (lut[idx] & 0x00ff_ffff) | fill_mask;
        }

        let dw = self.width;
        let pixels = self.buffer.as_mut_slice();
        for y in clip.y..clip.bottom() {
            let row_off = y as usize * dw;
            match Self::rounded_row_spans(rect, clip, radii, y) {
                RoundedRowSpans::Circle { cx, dy, rf } => {
                    for (i, x) in (clip.x..clip.right()).enumerate() {
                        let cov = Self::corner_coverage(x as f32 + 0.5 - cx, dy, rf);
                        let eff_a = ((a_u8 as f32) * cov + 0.5) as u8;
                        if eff_a != 0 {
                            let idx = row_off + x as usize;
                            let src = row_cols[i];
                            pixels[idx] = if argb {
                                src_over_u8(pixels[idx], src, eff_a)
                            } else {
                                blend_u8(pixels[idx], src, eff_a)
                            };
                        }
                    }
                }
                RoundedRowSpans::Rect {
                    mid_l,
                    mid_r,
                    left,
                    right,
                } => {
                    if mid_r > mid_l {
                        let off = (mid_l - clip.x) as usize;
                        let cnt = (mid_r - mid_l) as usize;
                        let dst = &mut pixels[row_off + mid_l as usize..row_off + mid_r as usize];
                        let src_slice = &row_cols[off..off + cnt];
                        if a_u8 == 255 {
                            dst.copy_from_slice(src_slice);
                        } else if argb {
                            for (d, &s) in dst.iter_mut().zip(src_slice.iter()) {
                                *d = src_over_u8(*d, s, a_u8);
                            }
                        } else {
                            for (d, &s) in dst.iter_mut().zip(src_slice.iter()) {
                                *d = blend_u8(*d, s, a_u8);
                            }
                        }
                    }
                    for corner in [left, right].into_iter().flatten() {
                        for x in corner.x_start..corner.x_end {
                            let cov = Self::corner_coverage(
                                x as f32 + 0.5 - corner.cx,
                                corner.dy,
                                corner.rf,
                            );
                            let eff_a = ((a_u8 as f32) * cov + 0.5) as u8;
                            if eff_a != 0 {
                                let idx = row_off + x as usize;
                                let src = row_cols[(x - clip.x) as usize];
                                pixels[idx] = if argb {
                                    src_over_u8(pixels[idx], src, eff_a)
                                } else {
                                    blend_u8(pixels[idx], src, eff_a)
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fill `rect` with a multi-stop horizontal CSS `linear-gradient(to right, ...)` and 4-corner radii.
    pub fn multi_stop_gradient_h(
        &mut self,
        rect: Rect,
        stops: &[(f32, u32)],
        radii: [i32; 4],
        alpha: f32,
    ) {
        if stops.is_empty() {
            return;
        }
        let lut = build_gradient_lut(stops);
        self.multi_stop_gradient_h_lut(rect, &lut, radii, alpha);
    }

    /// Fill `rect` with a multi-stop diagonal (`135deg`) gradient using a pre-baked 256-entry LUT.
    pub fn multi_stop_gradient_diag_lut(
        &mut self,
        rect: Rect,
        lut: &[u32; 256],
        radii: [i32; 4],
        alpha: f32,
    ) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let span = (rect.w - 1).max(1) + (rect.h - 1).max(1);
        let dw = self.width;
        let argb = self.argb;
        let opaque_mask = if argb { 0xff00_0000 } else { 0 };
        let pixels = self.buffer.as_mut_slice();
        for y in clip.y..clip.bottom() {
            let row_off = y as usize * dw;
            let dy = y - rect.y;
            match Self::rounded_row_spans(rect, clip, radii, y) {
                RoundedRowSpans::Circle { cx, dy: cdy, rf } => {
                    for x in clip.x..clip.right() {
                        let cov = Self::corner_coverage(x as f32 + 0.5 - cx, cdy, rf);
                        let eff_a = ((a_u8 as f32) * cov + 0.5) as u8;
                        if eff_a != 0 {
                            let idx = (((x - rect.x + dy) * 255 + span / 2) / span)
                                .clamp(0, 255) as usize;
                            let p = row_off + x as usize;
                            pixels[p] = if argb {
                                src_over_u8(pixels[p], lut[idx], eff_a)
                            } else {
                                blend_u8(pixels[p], lut[idx], eff_a)
                            };
                        }
                    }
                }
                RoundedRowSpans::Rect {
                    mid_l,
                    mid_r,
                    left,
                    right,
                } => {
                    if mid_r > mid_l {
                        let base_num = (mid_l - rect.x) + dy;
                        let dst = &mut pixels[row_off + mid_l as usize..row_off + mid_r as usize];
                        if a_u8 == 255 {
                            for (i, slot) in dst.iter_mut().enumerate() {
                                let idx = (((base_num + i as i32) * 255 + span / 2) / span)
                                    .clamp(0, 255) as usize;
                                *slot = lut[idx] | opaque_mask;
                            }
                        } else if argb {
                            for (i, slot) in dst.iter_mut().enumerate() {
                                let idx = (((base_num + i as i32) * 255 + span / 2) / span)
                                    .clamp(0, 255) as usize;
                                *slot = src_over_u8(*slot, lut[idx], a_u8);
                            }
                        } else {
                            for (i, slot) in dst.iter_mut().enumerate() {
                                let idx = (((base_num + i as i32) * 255 + span / 2) / span)
                                    .clamp(0, 255) as usize;
                                *slot = blend_u8(*slot, lut[idx], a_u8);
                            }
                        }
                    }
                    for corner in [left, right].into_iter().flatten() {
                        for x in corner.x_start..corner.x_end {
                            let cov = Self::corner_coverage(
                                x as f32 + 0.5 - corner.cx,
                                corner.dy,
                                corner.rf,
                            );
                            let eff_a = ((a_u8 as f32) * cov + 0.5) as u8;
                            if eff_a != 0 {
                                let idx = (((x - rect.x + dy) * 255 + span / 2) / span)
                                    .clamp(0, 255) as usize;
                                let p = row_off + x as usize;
                                pixels[p] = if argb {
                                    src_over_u8(pixels[p], lut[idx], eff_a)
                                } else {
                                    blend_u8(pixels[p], lut[idx], eff_a)
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fill `rect` with a multi-stop diagonal (`135deg`) CSS `linear-gradient(135deg, ...)` and 4-corner radii.
    pub fn multi_stop_gradient_diag(
        &mut self,
        rect: Rect,
        stops: &[(f32, u32)],
        radii: [i32; 4],
        alpha: f32,
    ) {
        if stops.is_empty() {
            return;
        }
        let lut = build_gradient_lut(stops);
        self.multi_stop_gradient_diag_lut(rect, &lut, radii, alpha);
    }

    /// Fill `rect` with a CSS `radial-gradient(circle at cx% cy%, ...)` using a pre-baked 256-entry LUT.
    pub fn multi_stop_radial_gradient_lut(
        &mut self,
        rect: Rect,
        cx_pct: f32,
        cy_pct: f32,
        lut: &[u32; 256],
        radii: [i32; 4],
        alpha: f32,
    ) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let cx = rect.x as f32 + rect.w as f32 * cx_pct;
        let cy = rect.y as f32 + rect.h as f32 * cy_pct;
        let max_dx = (cx - rect.x as f32).abs().max((rect.right() as f32 - cx).abs());
        let max_dy = (cy - rect.y as f32).abs().max((rect.bottom() as f32 - cy).abs());
        let max_dist = (max_dx * max_dx + max_dy * max_dy).sqrt().max(1.0);
        let inv_scale = 255.0 / max_dist;
        let dw = self.width;
        let argb = self.argb;
        let opaque_mask = if argb { 0xff00_0000 } else { 0 };
        let pixels = self.buffer.as_mut_slice();
        for y in clip.y..clip.bottom() {
            let row_off = y as usize * dw;
            let dy = y as f32 + 0.5 - cy;
            let dy_sq = dy * dy;
            match Self::rounded_row_spans(rect, clip, radii, y) {
                RoundedRowSpans::Circle { cx: ccx, dy: cdy, rf } => {
                    for x in clip.x..clip.right() {
                        let cov = Self::corner_coverage(x as f32 + 0.5 - ccx, cdy, rf);
                        let eff_a = ((a_u8 as f32) * cov + 0.5) as u8;
                        if eff_a != 0 {
                            let dx = x as f32 + 0.5 - cx;
                            let idx = (((dx * dx + dy_sq).sqrt() * inv_scale) + 0.5)
                                .clamp(0.0, 255.0) as usize;
                            let p = row_off + x as usize;
                            pixels[p] = if argb {
                                src_over_u8(pixels[p], lut[idx], eff_a)
                            } else {
                                blend_u8(pixels[p], lut[idx], eff_a)
                            };
                        }
                    }
                }
                RoundedRowSpans::Rect {
                    mid_l,
                    mid_r,
                    left,
                    right,
                } => {
                    if mid_r > mid_l {
                        let base_dx = mid_l as f32 + 0.5 - cx;
                        let dst = &mut pixels[row_off + mid_l as usize..row_off + mid_r as usize];
                        if a_u8 == 255 {
                            for (i, slot) in dst.iter_mut().enumerate() {
                                let dx = base_dx + i as f32;
                                let idx = (((dx * dx + dy_sq).sqrt() * inv_scale) + 0.5)
                                    .clamp(0.0, 255.0) as usize;
                                *slot = lut[idx] | opaque_mask;
                            }
                        } else if argb {
                            for (i, slot) in dst.iter_mut().enumerate() {
                                let dx = base_dx + i as f32;
                                let idx = (((dx * dx + dy_sq).sqrt() * inv_scale) + 0.5)
                                    .clamp(0.0, 255.0) as usize;
                                *slot = src_over_u8(*slot, lut[idx], a_u8);
                            }
                        } else {
                            for (i, slot) in dst.iter_mut().enumerate() {
                                let dx = base_dx + i as f32;
                                let idx = (((dx * dx + dy_sq).sqrt() * inv_scale) + 0.5)
                                    .clamp(0.0, 255.0) as usize;
                                *slot = blend_u8(*slot, lut[idx], a_u8);
                            }
                        }
                    }
                    for corner in [left, right].into_iter().flatten() {
                        for x in corner.x_start..corner.x_end {
                            let cov = Self::corner_coverage(
                                x as f32 + 0.5 - corner.cx,
                                corner.dy,
                                corner.rf,
                            );
                            let eff_a = ((a_u8 as f32) * cov + 0.5) as u8;
                            if eff_a != 0 {
                                let dx = x as f32 + 0.5 - cx;
                                let idx = (((dx * dx + dy_sq).sqrt() * inv_scale) + 0.5)
                                    .clamp(0.0, 255.0) as usize;
                                let p = row_off + x as usize;
                                pixels[p] = if argb {
                                    src_over_u8(pixels[p], lut[idx], eff_a)
                                } else {
                                    blend_u8(pixels[p], lut[idx], eff_a)
                                };
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fill `rect` with a CSS `radial-gradient(circle at cx% cy%, ...)` and 4-corner radii.
    pub fn multi_stop_radial_gradient(
        &mut self,
        rect: Rect,
        cx_pct: f32,
        cy_pct: f32,
        stops: &[(f32, u32)],
        radii: [i32; 4],
        alpha: f32,
    ) {
        if stops.is_empty() {
            return;
        }
        let lut = build_gradient_lut(stops);
        self.multi_stop_radial_gradient_lut(rect, cx_pct, cy_pct, &lut, radii, alpha);
    }

    /// Draw a border with per-corner radii `[tl, tr, br, bl]`.
    pub fn rounded_border_corners(
        &mut self,
        rect: Rect,
        radii: [i32; 4],
        color: u32,
        alpha: f32,
        width: i32,
    ) {
        if radii[0] == radii[1] && radii[1] == radii[2] && radii[2] == radii[3] {
            self.rounded_border(rect, radii[0], color, alpha, width);
            return;
        }
        let a_u8 = f32_to_u8(alpha);
        let bw = width.max(1);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let max_r = (rect.w / 2).min(rect.h / 2).max(0);
        let tl = radii[0].clamp(0, max_r);
        let tr = radii[1].clamp(0, max_r);
        let br = radii[2].clamp(0, max_r);
        let bl = radii[3].clamp(0, max_r);

        if rect.w > tl + tr {
            self.fill_u8(
                Rect::new(rect.x + tl, rect.y, rect.w - tl - tr, bw),
                color,
                a_u8,
            );
        }
        if rect.w > bl + br {
            self.fill_u8(
                Rect::new(rect.x + bl, rect.bottom() - bw, rect.w - bl - br, bw),
                color,
                a_u8,
            );
        }
        if rect.h > tl + bl {
            self.fill_u8(
                Rect::new(rect.x, rect.y + tl, bw, rect.h - tl - bl),
                color,
                a_u8,
            );
        }
        if rect.h > tr + br {
            self.fill_u8(
                Rect::new(rect.right() - bw, rect.y + tr, bw, rect.h - tr - br),
                color,
                a_u8,
            );
        }
        let corners = [
            (rect.x, rect.y, tl, rect.x as f32 + tl as f32, rect.y as f32 + tl as f32),
            (
                rect.right() - tr,
                rect.y,
                tr,
                (rect.right() - tr) as f32,
                rect.y as f32 + tr as f32,
            ),
            (
                rect.right() - br,
                rect.bottom() - br,
                br,
                (rect.right() - br) as f32,
                (rect.bottom() - br) as f32,
            ),
            (
                rect.x,
                rect.bottom() - bl,
                bl,
                rect.x as f32 + bl as f32,
                (rect.bottom() - bl) as f32,
            ),
        ];
        for (bx, by, r, cx, cy) in corners {
            if r <= 0 {
                continue;
            }
            let rf = r as f32;
            let inner_r = (rf - bw as f32).max(0.0);
            let cbox = self.clip_rect(Rect::new(bx, by, r, r));
            for y in cbox.y..cbox.bottom() {
                let dy = y as f32 + 0.5 - cy;
                for x in cbox.x..cbox.right() {
                    let dx = x as f32 + 0.5 - cx;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let outer_cov = (rf + 0.5 - dist).clamp(0.0, 1.0);
                    let inner_cov = (dist - inner_r + 0.5).clamp(0.0, 1.0);
                    let cov = outer_cov * inner_cov;
                    if cov > 0.0 {
                        self.blend_pixel_u8(x, y, color, ((a_u8 as f32) * cov + 0.5) as u8);
                    }
                }
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, radius: f32, color: u32, alpha: f32) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || radius <= 0.0 {
            return;
        }
        let bounds = Rect::new(
            (cx - radius - 1.0).floor() as i32,
            (cy - radius - 1.0).floor() as i32,
            (radius * 2.0 + 3.0).ceil() as i32,
            (radius * 2.0 + 3.0).ceil() as i32,
        );
        let clip = self.clip_rect(bounds);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        for y in clip.y..clip.bottom() {
            let dy = y as f32 + 0.5 - cy;
            for x in clip.x..clip.right() {
                let dx = x as f32 + 0.5 - cx;
                let d = (dx * dx + dy * dy).sqrt() - radius;
                let coverage = (0.5 - d).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.blend_pixel_u8(x, y, color, ((a_u8 as f32) * coverage + 0.5) as u8);
                }
            }
        }
    }

    pub fn ring(&mut self, cx: f32, cy: f32, radius: f32, thickness: f32, color: u32, alpha: f32) {
        let a_u8 = f32_to_u8(alpha);
        let outer = radius + thickness * 0.5;
        let inner = radius - thickness * 0.5;
        let bounds = Rect::new(
            (cx - outer - 1.0).floor() as i32,
            (cy - outer - 1.0).floor() as i32,
            (outer * 2.0 + 3.0).ceil() as i32,
            (outer * 2.0 + 3.0).ceil() as i32,
        );
        let clip = self.clip_rect(bounds);
        if clip.is_empty() || a_u8 == 0 {
            return;
        }
        self.mark_dirty(clip);
        for y in clip.y..clip.bottom() {
            let dy = y as f32 + 0.5 - cy;
            for x in clip.x..clip.right() {
                let dx = x as f32 + 0.5 - cx;
                let d = (dx * dx + dy * dy).sqrt();
                let coverage =
                    (outer + 0.5 - d).clamp(0.0, 1.0) * (d - inner + 0.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.blend_pixel_u8(x, y, color, ((a_u8 as f32) * coverage + 0.5) as u8);
                }
            }
        }
    }

    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32, alpha: f32) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 {
            return;
        }
        let min_x = x0.min(x1);
        let min_y = y0.min(y1);
        self.mark_dirty(Rect::new(
            min_x,
            min_y,
            (x0 - x1).abs() + 1,
            (y0 - y1).abs() + 1,
        ));
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let (mut x, mut y) = (x0, y0);
        let mut error = dx + dy;
        loop {
            self.blend_pixel_u8(x, y, color, a_u8);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * error;
            if e2 >= dy {
                error += dy;
                x += sx;
            }
            if e2 <= dx {
                error += dx;
                y += sy;
            }
        }
    }

    /// Blend an 8-bit coverage bitmap (glyph mask) using clipped SWAR integer blending.
    pub fn blend_coverage(
        &mut self,
        x: i32,
        y: i32,
        w: usize,
        h: usize,
        color: u32,
        coverage: &[u8],
    ) {
        self.blend_coverage_alpha(x, y, w, h, color, coverage, 255);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn blend_coverage_alpha(
        &mut self,
        x: i32,
        y: i32,
        w: usize,
        h: usize,
        color: u32,
        coverage: &[u8],
        opacity: u8,
    ) {
        if opacity == 0 {
            return;
        }
        let target = Rect::new(x, y, w as i32, h as i32);
        let clip = self.clip_rect(target);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let stride = self.width;
        let pixels = self.buffer.as_mut_slice();
        let color = color & 0x00ff_ffff;
        let op = opacity as u16;
        for py in clip.y..clip.bottom() {
            let gy = (py - y) as usize;
            let cov_row = &coverage[gy * w..gy * w + w];
            let dst_row = &mut pixels[py as usize * stride..py as usize * stride + stride];
            let col_start = (clip.x - x) as usize;
            let col_len = clip.w as usize;
            let dst_x = clip.x as usize;
            for i in 0..col_len {
                let raw = cov_row[col_start + i];
                if raw != 0 {
                    let alpha = if op == 255 {
                        raw
                    } else {
                        ((raw as u16 * op + 127) / 255) as u8
                    };
                    if alpha != 0 {
                        let slot = &mut dst_row[dst_x + i];
                        *slot = if argb {
                            src_over_u8(*slot, color, alpha)
                        } else {
                            blend_u8(*slot, color, alpha)
                        };
                    }
                }
            }
        }
    }

    /// Blit a premultiplied `0xAARRGGBB` sprite at `(x, y)` respecting `clip`.
    pub fn blit_sprite(&mut self, x: i32, y: i32, sprite: &Sprite) {
        self.blit_sprite_opacity(x, y, sprite, 255);
    }

    /// Blit a premultiplied `0xAARRGGBB` sprite with global opacity `opacity` (`0..=255`).
    pub fn blit_sprite_opacity(&mut self, x: i32, y: i32, sprite: &Sprite, opacity: u8) {
        self.blit_sprite_subrect(
            x,
            y,
            sprite,
            Rect::new(0, 0, sprite.width as i32, sprite.height as i32),
            opacity,
        );
    }

    /// Blit a 1:1 sub-rectangle `src_rect` of a premultiplied sprite at `(dx, dy)` respecting `clip`.
    pub fn blit_sprite_subrect(
        &mut self,
        dx: i32,
        dy: i32,
        sprite: &Sprite,
        src_rect: Rect,
        opacity: u8,
    ) {
        if opacity == 0 || sprite.width == 0 || sprite.height == 0 {
            return;
        }
        let sb = Rect::new(0, 0, sprite.width as i32, sprite.height as i32);
        let srect = src_rect.intersect(sb);
        if srect.is_empty() {
            return;
        }
        let target = Rect::new(dx, dy, srect.w, srect.h);
        let clip = self.clip_rect(target);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let sw = sprite.width as usize;
        let dw = self.width;
        let pixels = self.buffer.as_mut_slice();
        let ox = srect.x + (clip.x - dx);
        let oy = srect.y - dy;
        let cnt = clip.w as usize;
        let dx_start = clip.x as usize;
        let sx_start = ox as usize;
        for py in clip.y..clip.bottom() {
            let sy = (oy + py) as usize;
            let src_row = &sprite.pixels[sy * sw + sx_start..sy * sw + sx_start + cnt];
            let dst_row = &mut pixels[py as usize * dw + dx_start..py as usize * dw + dx_start + cnt];
            if opacity == 255 {
                let opaque_mask = if argb { 0xff00_0000 } else { 0 };
                for (slot, &spx) in dst_row.iter_mut().zip(src_row.iter()) {
                    let sa = spx >> 24;
                    if sa == 255 {
                        *slot = (spx & 0x00ff_ffff) | opaque_mask;
                    } else if sa != 0 {
                        *slot = if argb {
                            src_over_premul(*slot, spx)
                        } else {
                            blend_premul(*slot, spx)
                        };
                    }
                }
            } else {
                for (slot, &raw) in dst_row.iter_mut().zip(src_row.iter()) {
                    if (raw >> 24) != 0 {
                        let spx = scale_premul(raw, opacity);
                        *slot = if argb {
                            src_over_premul(*slot, spx)
                        } else {
                            blend_premul(*slot, spx)
                        };
                    }
                }
            }
        }
    }

    /// Cross-fade two same-size premultiplied sprites `s0 -> s1` by `t` (`0.0..=1.0`).
    pub fn blit_sprite_crossfade(&mut self, x: i32, y: i32, s0: &Sprite, s1: &Sprite, t: f32) {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.001 {
            self.blit_sprite(x, y, s0);
            return;
        }
        if t >= 0.999 {
            self.blit_sprite(x, y, s1);
            return;
        }
        if s0.width != s1.width || s0.height != s1.height {
            self.blit_sprite_opacity(x, y, s0, f32_to_u8(1.0 - t));
            self.blit_sprite_opacity(x, y, s1, f32_to_u8(t));
            return;
        }
        let target = Rect::new(x, y, s0.width as i32, s0.height as i32);
        let clip = self.clip_rect(target);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let w2 = (t * 256.0 + 0.5) as u32;
        let w1 = 256 - w2;
        let sw = s0.width as usize;
        let dw = self.width;
        let pixels = self.buffer.as_mut_slice();
        for py in clip.y..clip.bottom() {
            let sy = (py - y) as usize;
            let row0 = &s0.pixels[sy * sw..sy * sw + sw];
            let row1 = &s1.pixels[sy * sw..sy * sw + sw];
            let dst_row = &mut pixels[py as usize * dw..py as usize * dw + dw];
            let sx_start = (clip.x - x) as usize;
            let dx_start = clip.x as usize;
            for i in 0..clip.w as usize {
                let p0 = row0[sx_start + i];
                let p1 = row1[sx_start + i];
                if ((p0 | p1) >> 24) == 0 {
                    continue;
                }
                let ag = ((((p0 >> 8) & 0x00ff_00ff) * w1 + ((p1 >> 8) & 0x00ff_00ff) * w2))
                    & 0xff00_ff00;
                let rb = (((p0 & 0x00ff_00ff) * w1 + (p1 & 0x00ff_00ff) * w2) >> 8) & 0x00ff_00ff;
                let spx = ag | rb;
                if (spx >> 24) != 0 {
                    let slot = &mut dst_row[dx_start + i];
                    *slot = if argb {
                        src_over_premul(*slot, spx)
                    } else {
                        blend_premul(*slot, spx)
                    };
                }
            }
        }
    }

    /// Stretch-blit a premultiplied sprite into `dst_rect` with nearest/linear Y and optional accent tint.
    pub fn blit_sprite_stretched_tint(
        &mut self,
        dst_rect: Rect,
        sprite: &Sprite,
        tint: Option<(u32, f32)>,
        opacity: u8,
    ) {
        if opacity == 0 || sprite.width == 0 || sprite.height == 0 || dst_rect.is_empty() {
            return;
        }
        let clip = self.clip_rect(dst_rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let sw = sprite.width as i32;
        let sh = sprite.height as i32;
        let dw = self.width;
        let pixels = self.buffer.as_mut_slice();
        let (tr, tg, tb, tf) = match tint {
            Some((c, f)) => (
                ((c >> 16) & 0xff) as i32,
                ((c >> 8) & 0xff) as i32,
                (c & 0xff) as i32,
                (f.clamp(0.0, 1.0) * 256.0 + 0.5) as i32,
            ),
            None => (0, 0, 0, 0),
        };
        for py in clip.y..clip.bottom() {
            let sy = (((py - dst_rect.y) * sh) / dst_rect.h.max(1)).clamp(0, sh - 1) as usize;
            let src_row = &sprite.pixels[sy * sw as usize..(sy + 1) * sw as usize];
            let dst_row = &mut pixels[py as usize * dw..py as usize * dw + dw];
            for px in clip.x..clip.right() {
                let sx = (((px - dst_rect.x) * sw) / dst_rect.w.max(1)).clamp(0, sw - 1) as usize;
                let mut spx = src_row[sx];
                let sa = (spx >> 24) & 0xff;
                if sa == 0 {
                    continue;
                }
                if tf > 0 {
                    let sr = ((spx >> 16) & 0xff) as i32;
                    let sg = ((spx >> 8) & 0xff) as i32;
                    let sb = (spx & 0xff) as i32;
                    let tint_r = (tr * sa as i32) / 255;
                    let tint_g = (tg * sa as i32) / 255;
                    let tint_b = (tb * sa as i32) / 255;
                    let nr = (sr + (((tint_r - sr) * tf) >> 8)).clamp(0, sa as i32) as u32;
                    let ng = (sg + (((tint_g - sg) * tf) >> 8)).clamp(0, sa as i32) as u32;
                    let nb = (sb + (((tint_b - sb) * tf) >> 8)).clamp(0, sa as i32) as u32;
                    spx = (sa << 24) | (nr << 16) | (ng << 8) | nb;
                }
                if opacity < 255 {
                    spx = scale_premul(spx, opacity);
                }
                let slot = &mut dst_row[px as usize];
                *slot = if argb {
                    src_over_premul(*slot, spx)
                } else {
                    blend_premul(*slot, spx)
                };
            }
        }
    }

    /// Fill a rectangle with a vertical linear color/alpha gradient.
    pub fn fill_gradient_v(
        &mut self,
        rect: Rect,
        top_color: u32,
        bottom_color: u32,
        top_alpha: f32,
        bottom_alpha: f32,
    ) {
        let clip = self.clip_rect(rect);
        if clip.is_empty() || rect.h <= 0 {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let stride = self.width;
        let pixels = self.buffer.as_mut_slice();
        let inv_h = 1.0 / (rect.h as f32).max(1.0);
        for py in clip.y..clip.bottom() {
            let t = ((py - rect.y) as f32 * inv_h).clamp(0.0, 1.0);
            let row_color = mix(top_color, bottom_color, t);
            let alpha = (lerp_f32(top_alpha, bottom_alpha, t).clamp(0.0, 1.0) * 255.0) as u8;
            if alpha == 0 {
                continue;
            }
            let row = &mut pixels[py as usize * stride..py as usize * stride + stride];
            for px in clip.x..clip.right() {
                let slot = &mut row[px as usize];
                *slot = if argb {
                    src_over_u8(*slot, row_color, alpha)
                } else {
                    blend_u8(*slot, row_color, alpha)
                };
            }
        }
    }

    /// Draw a smooth sparkline chart with optional vertical gradient area fill.
    pub fn draw_sparkline(
        &mut self,
        rect: Rect,
        values: &[f32],
        stroke_color: u32,
        fill_color: Option<u32>,
    ) {
        if values.len() < 2 || rect.w <= 4 || rect.h <= 4 {
            return;
        }
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        let n = values.len();
        let step_x = (rect.w as f32 - 1.0) / (n - 1) as f32;
        let base_y = rect.bottom() as f32 - 1.0;
        let scale_y = (rect.h as f32 - 2.0).max(1.0);

        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n);
        for (i, &v) in values.iter().enumerate() {
            let clamped = v.clamp(0.0, 1.0);
            let px = rect.x as f32 + i as f32 * step_x;
            let py = base_y - clamped * scale_y;
            pts.push((px, py));
        }

        // Draw vertical area fill under the curve if requested
        if let Some(fill) = fill_color {
            let argb = self.argb;
            let stride = self.width;
            let pixels = self.buffer.as_mut_slice();
            for i in 0..n - 1 {
                let x0 = pts[i].0.round() as i32;
                let x1 = pts[i + 1].0.round() as i32;
                let seg_clip_x0 = x0.max(clip.x);
                let seg_clip_x1 = x1.min(clip.right());
                if seg_clip_x0 >= seg_clip_x1 {
                    continue;
                }
                let dx = (pts[i + 1].0 - pts[i].0).max(0.001);
                for x in seg_clip_x0..seg_clip_x1 {
                    let t = ((x as f32 - pts[i].0) / dx).clamp(0.0, 1.0);
                    let top_y = (pts[i].1 + t * (pts[i + 1].1 - pts[i].1)).round() as i32;
                    let y_start = top_y.max(clip.y);
                    let y_end = (rect.bottom() - 1).min(clip.bottom());
                    for y in y_start..=y_end {
                        let rel_y = (y - top_y) as f32 / (rect.h as f32).max(1.0);
                        let alpha = ((1.0 - rel_y * 0.75).clamp(0.05, 0.40) * 255.0) as u8;
                        let slot = &mut pixels[y as usize * stride + x as usize];
                        *slot = if argb {
                            src_over_u8(*slot, fill, alpha)
                        } else {
                            blend_u8(*slot, fill, alpha)
                        };
                    }
                }
            }
            self.mark_dirty(clip);
        }

        // Draw stroke line segments
        for i in 0..n - 1 {
            self.line(
                pts[i].0.round() as i32,
                pts[i].1.round() as i32,
                pts[i + 1].0.round() as i32,
                pts[i + 1].1.round() as i32,
                stroke_color,
                1.0,
            );
        }
    }

    /// Windows 7 Aero split-gloss vertical gradient (upper highlight half + sharp
    /// reflection transition + lower saturated half) clipped cleanly to `radius`.
    #[allow(clippy::too_many_arguments)]
    pub fn split_gloss_vgradient(
        &mut self,
        rect: Rect,
        radius: i32,
        top_start: u32,
        top_end: u32,
        bot_start: u32,
        bot_end: u32,
        split_ratio: f32,
        alpha: f32,
    ) {
        let a_u8 = f32_to_u8(alpha);
        if a_u8 == 0 || rect.is_empty() {
            return;
        }
        let r = radius.max(0).min(rect.w / 2).min(rect.h / 2);
        let clip = self.clip_rect(rect);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let split_y = rect.y + ((rect.h as f32) * split_ratio.clamp(0.1, 0.9)).round() as i32;
        let top_h = (split_y - rect.y).max(1) as f32;
        let bot_h = (rect.bottom() - split_y).max(1) as f32;
        let rf = r as f32;
        for py in clip.y..clip.bottom() {
            let row_color = if py < split_y {
                let t = ((py - rect.y) as f32 / top_h).clamp(0.0, 1.0);
                mix(top_start, top_end, t)
            } else {
                let t = ((py - split_y) as f32 / bot_h).clamp(0.0, 1.0);
                mix(bot_start, bot_end, t)
            };
            if r == 0 || (py >= rect.y + r && py < rect.bottom() - r) {
                self.fill_u8(Rect::new(clip.x, py, clip.w, 1), row_color, a_u8);
                continue;
            }
            let mid_l = (rect.x + r).max(clip.x);
            let mid_r = (rect.right() - r).min(clip.right());
            if mid_r > mid_l {
                self.fill_u8(Rect::new(mid_l, py, mid_r - mid_l, 1), row_color, a_u8);
            }
            let cy = if py < rect.y + r {
                rect.y as f32 + rf
            } else {
                (rect.bottom() - r) as f32
            };
            let dy = py as f32 + 0.5 - cy;
            for x in clip.x..mid_l.min(clip.right()) {
                let dx = x as f32 + 0.5 - (rect.x as f32 + rf);
                let cov = Self::corner_coverage(dx, dy, rf);
                if cov > 0.0 {
                    self.blend_pixel_u8(x, py, row_color, ((a_u8 as f32) * cov + 0.5) as u8);
                }
            }
            for x in mid_r.max(clip.x)..clip.right() {
                let dx = x as f32 + 0.5 - (rect.right() - r) as f32;
                let cov = Self::corner_coverage(dx, dy, rf);
                if cov > 0.0 {
                    self.blend_pixel_u8(x, py, row_color, ((a_u8 as f32) * cov + 0.5) as u8);
                }
            }
        }
    }

    /// Paint a smooth circular radial glow centered at `(cx, cy)` inside current `clip`.
    pub fn radial_glow(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        color: u32,
        max_alpha: f32,
    ) {
        if radius <= 1.0 || max_alpha <= 0.002 {
            return;
        }
        let bounds = Rect::new(
            (cx - radius).floor() as i32,
            (cy - radius).floor() as i32,
            (radius * 2.0 + 2.0).ceil() as i32,
            (radius * 2.0 + 2.0).ceil() as i32,
        );
        let clip = self.clip_rect(bounds);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let color = color & 0x00ff_ffff;
        let inv_r = 1.0 / radius;
        let stride = self.width;
        let pixels = self.buffer.as_mut_slice();
        for py in clip.y..clip.bottom() {
            let dy = (py as f32 + 0.5 - cy) * inv_r;
            let dy2 = dy * dy;
            if dy2 >= 1.0 {
                continue;
            }
            let row = &mut pixels[py as usize * stride..py as usize * stride + stride];
            for px in clip.x..clip.right() {
                let dx = (px as f32 + 0.5 - cx) * inv_r;
                let d2 = dx * dx + dy2;
                if d2 < 1.0 {
                    let falloff = 1.0 - d2;
                    let a = (falloff * falloff * max_alpha * 255.0 + 0.5) as u8;
                    if a > 0 {
                        let slot = &mut row[px as usize];
                        *slot = if argb {
                            src_over_u8(*slot, color, a)
                        } else {
                            blend_u8(*slot, color, a)
                        };
                    }
                }
            }
        }
    }

    /// Diagonal glossy specular shimmer band sweeping across `rect` at normalized `phase` (`0.0..=1.0`).
    pub fn diagonal_shimmer(
        &mut self,
        rect: Rect,
        phase: f32,
        band_w: f32,
        color: u32,
        max_alpha: f32,
    ) {
        let clip = self.clip_rect(rect);
        if clip.is_empty() || band_w <= 1.0 || max_alpha <= 0.002 {
            return;
        }
        self.mark_dirty(clip);
        let argb = self.argb;
        let skew = (rect.h as f32) * 0.45;
        let sweep_span = rect.w as f32 + band_w * 2.0 + skew;
        let center_base = rect.x as f32 - band_w + phase.clamp(0.0, 1.0) * sweep_span;
        let inv_half = 2.0 / band_w;
        let color = color & 0x00ff_ffff;
        let stride = self.width;
        let pixels = self.buffer.as_mut_slice();
        for py in clip.y..clip.bottom() {
            let row_center = center_base - ((py - rect.y) as f32) * 0.45;
            let x_lo = ((row_center - band_w * 0.5).floor() as i32).max(clip.x);
            let x_hi = ((row_center + band_w * 0.5).ceil() as i32).min(clip.right());
            if x_lo >= x_hi {
                continue;
            }
            let row = &mut pixels[py as usize * stride..py as usize * stride + stride];
            for px in x_lo..x_hi {
                let d = ((px as f32 + 0.5 - row_center) * inv_half).abs();
                if d < 1.0 {
                    let s = 1.0 - d;
                    let intensity = s * s * (3.0 - 2.0 * s);
                    let a = (intensity * max_alpha * 255.0 + 0.5) as u8;
                    if a > 0 {
                        let slot = &mut row[px as usize];
                        *slot = if argb {
                            src_over_u8(*slot, color, a)
                        } else {
                            blend_u8(*slot, color, a)
                        };
                    }
                }
            }
        }
    }

    /// Paint a soft rounded drop shadow halo around `rect` (used for popups, cards, and windows).
    pub fn rounded_drop_shadow(
        &mut self,
        rect: Rect,
        radius: i32,
        spread: i32,
        y_offset: i32,
        max_alpha: f32,
    ) {
        if spread <= 0 || max_alpha <= 0.002 || rect.is_empty() {
            return;
        }
        let s = spread.min(24);
        let step = if s > 8 { 2 } else { 1 };
        let mut ring = s;
        while ring > 0 {
            let t = ring as f32 / s as f32;
            let ring_alpha = max_alpha * (1.0 - t) * (1.0 - t) * 0.45;
            let expand = Rect::new(
                rect.x - ring,
                rect.y - ring + y_offset,
                rect.w + ring * 2,
                rect.h + ring * 2,
            );
            self.rounded_border(expand, radius + ring, 0x0000_0000, ring_alpha, step);
            ring -= step;
        }
    }

    /// Blur `rect` on this `Canvas` in place using `blur_rect_fast`.
    pub fn blur_rect(&mut self, rect: Rect, radius: i32, scratch: &mut BlurScratch) {
        let clip = self.clip_rect(rect);
        if clip.is_empty() || radius <= 0 {
            return;
        }
        self.mark_dirty(clip);
        let w = self.width;
        let h = self.height;
        let argb = self.argb;
        blur_rect_fast(self.pixels_mut(), w, h, clip, radius, scratch);
        if argb {
            self.enforce_opaque_rect(clip);
        }
    }
}

/// Reusable scratch allocations for zero-alloc per-frame backdrop blur.
#[derive(Default)]
pub struct BlurScratch {
    down_a: Vec<u32>,
    down_b: Vec<u32>,
    down_c: Vec<u32>,
    col_r: Vec<u32>,
    col_g: Vec<u32>,
    col_b: Vec<u32>,
    x_lut: Vec<(usize, usize, u32)>,
}

/// One-dimensional $O(1)$ per-pixel sliding box pass over a packed `0xRRGGBB` buffer
/// (separable H+V passes approximate Gaussian blur at fixed-point arithmetic speed).
fn box_blur_h(src: &[u32], dst: &mut [u32], w: usize, h: usize, radius: usize) {
    if w == 0 || h == 0 || radius == 0 {
        dst[..w * h].copy_from_slice(&src[..w * h]);
        return;
    }
    let r = radius.min(32);
    let win = (r * 2 + 1) as u32;
    let inv = ((1u32 << 16) + win / 2) / win;
    let init_mul = r as u32 + 1;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let out = &mut dst[y * w..(y + 1) * w];
        let first = row[0];
        let mut sr = ((first >> 16) & 0xff) * init_mul;
        let mut sg = ((first >> 8) & 0xff) * init_mul;
        let mut sb = (first & 0xff) * init_mul;
        for i in 1..=r {
            let p = row[i.min(w - 1)];
            sr += (p >> 16) & 0xff;
            sg += (p >> 8) & 0xff;
            sb += p & 0xff;
        }
        for x in 0..w {
            let cr = ((sr * inv) >> 16).min(255);
            let cg = ((sg * inv) >> 16).min(255);
            let cb = ((sb * inv) >> 16).min(255);
            out[x] = (cr << 16) | (cg << 8) | cb;
            let p_in = row[(x + r + 1).min(w - 1)];
            let p_out = row[x.saturating_sub(r)];
            sr = sr + ((p_in >> 16) & 0xff) - ((p_out >> 16) & 0xff);
            sg = sg + ((p_in >> 8) & 0xff) - ((p_out >> 8) & 0xff);
            sb = sb + (p_in & 0xff) - (p_out & 0xff);
        }
    }
}

/// Cache-friendly row-major vertical sliding box pass over `0xRRGGBB` using streaming column sums.
fn box_blur_v(
    src: &[u32],
    dst: &mut [u32],
    w: usize,
    h: usize,
    radius: usize,
    col_r: &mut Vec<u32>,
    col_g: &mut Vec<u32>,
    col_b: &mut Vec<u32>,
) {
    if w == 0 || h == 0 || radius == 0 {
        dst[..w * h].copy_from_slice(&src[..w * h]);
        return;
    }
    let r = radius.min(32);
    let win = (r * 2 + 1) as u32;
    let inv = ((1u32 << 16) + win / 2) / win;

    col_r.resize(w, 0);
    col_g.resize(w, 0);
    col_b.resize(w, 0);
    let col_r = &mut col_r[..w];
    let col_g = &mut col_g[..w];
    let col_b = &mut col_b[..w];

    let row0 = &src[..w];
    let top_mul = (r + 1) as u32;
    for x in 0..w {
        let p = row0[x];
        col_r[x] = ((p >> 16) & 0xff) * top_mul;
        col_g[x] = ((p >> 8) & 0xff) * top_mul;
        col_b[x] = (p & 0xff) * top_mul;
    }
    for i in 1..=r {
        let row = &src[i.min(h - 1) * w..][..w];
        for x in 0..w {
            let p = row[x];
            col_r[x] += (p >> 16) & 0xff;
            col_g[x] += (p >> 8) & 0xff;
            col_b[x] += p & 0xff;
        }
    }

    for y in 0..h {
        let out_row = &mut dst[y * w..][..w];
        let y_in = (y + r + 1).min(h - 1);
        let y_out = y.saturating_sub(r);
        let in_row = &src[y_in * w..][..w];
        let sub_row = &src[y_out * w..][..w];
        for x in 0..w {
            let sr = col_r[x];
            let sg = col_g[x];
            let sb = col_b[x];
            let cr = ((sr * inv) >> 16).min(255);
            let cg = ((sg * inv) >> 16).min(255);
            let cb = ((sb * inv) >> 16).min(255);
            out_row[x] = (cr << 16) | (cg << 8) | cb;
            let p_in = in_row[x];
            let p_out = sub_row[x];
            col_r[x] = sr + ((p_in >> 16) & 0xff) - ((p_out >> 16) & 0xff);
            col_g[x] = sg + ((p_in >> 8) & 0xff) - ((p_out >> 8) & 0xff);
            col_b[x] = sb + (p_in & 0xff) - (p_out & 0xff);
        }
    }
}

#[inline(always)]
fn avg4_rgb(p00: u32, p10: u32, p01: u32, p11: u32) -> u32 {
    let rb = (((p00 & 0x00ff_00ff)
        + (p10 & 0x00ff_00ff)
        + (p01 & 0x00ff_00ff)
        + (p11 & 0x00ff_00ff)
        + 0x0002_0002)
        >> 2)
        & 0x00ff_00ff;
    let g = (((p00 & 0x0000_ff00)
        + (p10 & 0x0000_ff00)
        + (p01 & 0x0000_ff00)
        + (p11 & 0x0000_ff00)
        + 0x0000_0200)
        >> 2)
        & 0x0000_ff00;
    rb | g
}

fn downsample_2x_region(
    src: &[u32],
    stride: usize,
    rx: usize,
    ry: usize,
    rw: usize,
    rh: usize,
    dst: &mut Vec<u32>,
) -> (usize, usize) {
    let dw = (rw + 1) / 2;
    let dh = (rh + 1) / 2;
    dst.resize(dw * dh, 0);
    let pairs_w = rw / 2;
    let has_tail = (rw & 1) != 0;
    for dy in 0..dh {
        let sy0 = ry + (dy * 2).min(rh - 1);
        let sy1 = ry + (dy * 2 + 1).min(rh - 1);
        let r0 = &src[sy0 * stride + rx..][..rw];
        let r1 = &src[sy1 * stride + rx..][..rw];
        let out_row = &mut dst[dy * dw..(dy + 1) * dw];
        for dx in 0..pairs_w {
            let sx = dx * 2;
            out_row[dx] = avg4_rgb(r0[sx], r0[sx + 1], r1[sx], r1[sx + 1]);
        }
        if has_tail {
            let sx = rw - 1;
            out_row[pairs_w] = avg4_rgb(r0[sx], r0[sx], r1[sx], r1[sx]);
        }
    }
    (dw, dh)
}

fn upsample_bilinear_to_region(
    src: &[u32],
    sw: usize,
    sh: usize,
    dst: &mut [u32],
    stride: usize,
    rx: usize,
    ry: usize,
    rw: usize,
    rh: usize,
    x_lut: &mut Vec<(usize, usize, u32)>,
) {
    if sw == 0 || sh == 0 || rw == 0 || rh == 0 {
        return;
    }
    x_lut.clear();
    x_lut.reserve(rw);
    for x in 0..rw {
        let fx_fp = ((x * sw * 256 + 128) / rw)
            .saturating_sub(128)
            .min((sw - 1) * 256);
        let sx0 = fx_fp >> 8;
        let sx1 = (sx0 + 1).min(sw - 1);
        let fx = (fx_fp & 0xff) as u32;
        x_lut.push((sx0, sx1, fx));
    }
    let x_lut = &x_lut[..rw];

    for y in 0..rh {
        let fy_fp = ((y * sh * 256 + 128) / rh)
            .saturating_sub(128)
            .min((sh - 1) * 256);
        let sy0 = fy_fp >> 8;
        let sy1 = (sy0 + 1).min(sh - 1);
        let fy = (fy_fp & 0xff) as u32;
        let inv_fy = 256 - fy;
        let row0 = &src[sy0 * sw..];
        let row1 = &src[sy1 * sw..];
        let dst_row = &mut dst[(ry + y) * stride + rx..][..rw];
        for (dst_px, &(sx0, sx1, fx)) in dst_row.iter_mut().zip(x_lut.iter()) {
            let inv_fx = 256 - fx;

            let p00 = row0[sx0];
            let p10 = row0[sx1];
            let p01 = row1[sx0];
            let p11 = row1[sx1];

            let top_rb = (((p00 & 0x00ff_00ff) * inv_fx + (p10 & 0x00ff_00ff) * fx) >> 8)
                & 0x00ff_00ff;
            let bot_rb = (((p01 & 0x00ff_00ff) * inv_fx + (p11 & 0x00ff_00ff) * fx) >> 8)
                & 0x00ff_00ff;
            let out_rb = ((top_rb * inv_fy + bot_rb * fy) >> 8) & 0x00ff_00ff;

            let top_g = (((p00 & 0x0000_ff00) * inv_fx + (p10 & 0x0000_ff00) * fx) >> 8)
                & 0x0000_ff00;
            let bot_g = (((p01 & 0x0000_ff00) * inv_fx + (p11 & 0x0000_ff00) * fx) >> 8)
                & 0x0000_ff00;
            let out_g = ((top_g * inv_fy + bot_g * fy) >> 8) & 0x0000_ff00;

            *dst_px = out_rb | out_g;
        }
    }
}

/// Fast CPU backdrop frosted-glass blur over `rect` in `buf`.
///
/// Uses 2x/4x box downsampling + dual-pass separable $O(1)$ sliding box blur +
/// bilinear upsample for radii >= 4, delivering smooth Dual-Kawase quality at
/// high frame rates without per-pixel integer division.
pub fn blur_rect_fast(
    buf: &mut [u32],
    width: usize,
    height: usize,
    rect: Rect,
    radius: i32,
    scratch: &mut BlurScratch,
) {
    let clipped = rect.intersect(Rect::new(0, 0, width as i32, height as i32));
    if clipped.is_empty() || radius <= 0 {
        return;
    }
    let rx = clipped.x as usize;
    let ry = clipped.y as usize;
    let rw = clipped.w as usize;
    let rh = clipped.h as usize;

    if radius >= 4 && rw >= 8 && rh >= 8 {
        let (w1, h1) = downsample_2x_region(buf, width, rx, ry, rw, rh, &mut scratch.down_a);
        if radius >= 8 && rw >= 16 && rh >= 16 && w1 >= 8 && h1 >= 8 {
            let (w2, h2) = downsample_2x_region(&scratch.down_a, w1, 0, 0, w1, h1, &mut scratch.down_b);
            let r_scaled = ((radius + 2) / 4).max(1) as usize;
            scratch.down_c.resize(w2 * h2, 0);
            box_blur_h(&scratch.down_b, &mut scratch.down_c, w2, h2, r_scaled);
            box_blur_v(
                &scratch.down_c,
                &mut scratch.down_b,
                w2,
                h2,
                r_scaled,
                &mut scratch.col_r,
                &mut scratch.col_g,
                &mut scratch.col_b,
            );
            box_blur_h(&scratch.down_b, &mut scratch.down_c, w2, h2, r_scaled.max(1));
            box_blur_v(
                &scratch.down_c,
                &mut scratch.down_b,
                w2,
                h2,
                r_scaled.max(1),
                &mut scratch.col_r,
                &mut scratch.col_g,
                &mut scratch.col_b,
            );
            upsample_bilinear_to_region(
                &scratch.down_b,
                w2,
                h2,
                buf,
                width,
                rx,
                ry,
                rw,
                rh,
                &mut scratch.x_lut,
            );
        } else {
            let r_scaled = ((radius + 1) / 2).max(1) as usize;
            scratch.down_b.resize(w1 * h1, 0);
            box_blur_h(&scratch.down_a, &mut scratch.down_b, w1, h1, r_scaled);
            box_blur_v(
                &scratch.down_b,
                &mut scratch.down_a,
                w1,
                h1,
                r_scaled,
                &mut scratch.col_r,
                &mut scratch.col_g,
                &mut scratch.col_b,
            );
            upsample_bilinear_to_region(
                &scratch.down_a,
                w1,
                h1,
                buf,
                width,
                rx,
                ry,
                rw,
                rh,
                &mut scratch.x_lut,
            );
        }
        return;
    }

    let r = (radius as usize).min(16);
    scratch.down_a.resize(rw * rh, 0);
    scratch.down_b.resize(rw * rh, 0);
    for y in 0..rh {
        let src_start = (ry + y) * width + rx;
        scratch.down_a[y * rw..(y + 1) * rw].copy_from_slice(&buf[src_start..src_start + rw]);
    }
    box_blur_h(&scratch.down_a, &mut scratch.down_b, rw, rh, r);
    box_blur_v(
        &scratch.down_b,
        &mut scratch.down_a,
        rw,
        rh,
        r,
        &mut scratch.col_r,
        &mut scratch.col_g,
        &mut scratch.col_b,
    );
    for y in 0..rh {
        let dst_start = (ry + y) * width + rx;
        buf[dst_start..dst_start + rw].copy_from_slice(&scratch.down_a[y * rw..(y + 1) * rw]);
    }
}

/// Linear interpolation between `a` and `b` by factor `t` (0.0 ..= 1.0).
#[inline(always)]
pub fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Standard cubic ease-out easing function.
#[inline(always)]
pub fn ease_out_cubic(t: f32) -> f32 {
    let p = (1.0 - t).clamp(0.0, 1.0);
    1.0 - p * p * p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lerp_and_easing() {
        assert_eq!(lerp_f32(10.0, 20.0, 0.0), 10.0);
        assert_eq!(lerp_f32(10.0, 20.0, 0.5), 15.0);
        assert_eq!(lerp_f32(10.0, 20.0, 1.0), 20.0);

        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5); // Ease-out curve rises faster early on
    }

    #[test]
    fn test_sparkline_draw() {
        let mut canvas = Canvas::new(100, 50, 0x000000);
        let vals = [0.1, 0.5, 0.8, 0.2, 0.9];
        canvas.draw_sparkline(Rect::new(10, 10, 80, 30), &vals, 0x3b82f6, Some(0x3b82f6));
        // Verify buffer is modified without crashing or out-of-bounds
        assert!(canvas.buffer.as_slice().iter().any(|&p| p != 0x000000));
    }

    #[test]
    fn bevel_and_horizontal_gradient_stay_within_rect() {
        let mut canvas = Canvas::new(40, 20, 0x111111);
        canvas.bevel(
            Rect::new(4, 2, 20, 10),
            0xffffff,
            0xdfdfdf,
            0x0a0a0a,
            0x808080,
        );
        let px = canvas.pixels();
        assert_eq!(px[2 * 40 + 4], 0xffffff);
        assert_eq!(px[3 * 40 + 5], 0xdfdfdf);
        assert_eq!(px[11 * 40 + 23], 0x0a0a0a);
        assert_eq!(px[10 * 40 + 22], 0x808080);
        assert_eq!(px[1 * 40 + 4], 0x111111);

        canvas.horizontal_gradient(Rect::new(2, 14, 16, 4), 0x000080, 0x1084d0);
        let px = canvas.pixels();
        assert_eq!(px[14 * 40 + 2], 0x000080);
        assert_eq!(px[14 * 40 + 17], 0x1084d0);
        assert_eq!(px[17 * 40 + 2], 0x000080);
    }

    #[test]
    fn argb_glass_clear_and_enforce_opaque_work() {
        let mut c = Canvas::new(20, 20, 0xff22_3344);
        c.set_argb(true);
        c.clear(0x112233);
        assert_eq!(c.pixels()[0], 0xff11_2233);
        c.clear_transparent(Rect::new(0, 0, 20, 6), 0);
        assert_eq!(c.pixels()[0], 0);
        assert_eq!(c.pixels()[6 * 20], 0xff11_2233);

        // Semi-transparent white fog over transparent region accumulates alpha cleanly.
        c.fill_alpha(Rect::new(2, 1, 4, 2), 0xffffff, 0.5);
        assert!((c.pixels()[20 + 2] >> 24) > 100);

        // Enforce opaque over content body strips zeroed alphas from third-party blits.
        c.pixels_mut()[10 * 20 + 5] = 0x00aa_bbcc;
        c.enforce_opaque_rect(Rect::new(0, 6, 20, 14));
        assert_eq!(c.pixels()[10 * 20 + 5], 0xffaa_bbcc);
    }

    #[test]
    fn downsampled_blur_smooths_edges() {
        let mut canvas = Canvas::new(65, 63, 0x000000);
        canvas.fill(Rect::new(16, 16, 33, 31), 0xffffff);
        let mut scratch = BlurScratch::default();
        canvas.blur_rect(Rect::new(0, 0, 65, 63), 10, &mut scratch);
        let edge = canvas.pixels()[16 * 65 + 14];
        let edge_r = (edge >> 16) & 0xff;
        assert!(edge_r > 0 && edge_r < 255, "expected blurred edge, got {edge_r}");
    }

    #[test]
    fn rect_region_subtraction_and_union_remain_disjoint() {
        let mut reg = RectRegion::from_rect(Rect::new(0, 0, 100, 100));
        reg.subtract_rect(Rect::new(20, 20, 40, 50));
        let total_area: i64 = reg.rects.iter().map(|r| i64::from(r.w) * i64::from(r.h)).sum();
        assert_eq!(total_area, 100 * 100 - 40 * 50);

        for i in 0..reg.rects.len() {
            for j in (i + 1)..reg.rects.len() {
                assert!(reg.rects[i].intersect(reg.rects[j]).is_empty());
            }
        }

        reg.add_rect(Rect::new(10, 10, 30, 30));
        let new_area: i64 = reg.rects.iter().map(|r| i64::from(r.w) * i64::from(r.h)).sum();
        assert_eq!(new_area, 8000 + 20 * 20); // overlaps (20,20)-(40,40) which was missing, i.e. 400 px restored
        for i in 0..reg.rects.len() {
            for j in (i + 1)..reg.rects.len() {
                assert!(reg.rects[i].intersect(reg.rects[j]).is_empty());
            }
        }
    }
}
