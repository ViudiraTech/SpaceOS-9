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

#[inline]
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let alpha = (t.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    blend_u8(a, b, alpha)
}

#[inline(always)]
fn f32_to_u8(alpha: f32) -> u8 {
    (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Premultiplied `0xAARRGGBB` sprite used for icons, logos and thumbnails.
#[derive(Clone)]
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

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub buffer: PixelBuffer,
    clip: Rect,
    dirty: Rect,
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
    fn clip_rect(&self, rect: Rect) -> Rect {
        rect.intersect(self.clip)
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
        self.pixels_mut().fill(color & 0x00ff_ffff);
        self.mark_all_dirty();
    }

    pub fn copy_background(&mut self, bg: &[u32]) {
        let dst = self.pixels_mut();
        if bg.len() == dst.len() {
            dst.copy_from_slice(bg);
            self.mark_all_dirty();
        }
    }

    #[inline(always)]
    pub fn blend_pixel_u8(&mut self, x: i32, y: i32, color: u32, alpha: u8) {
        if alpha == 0 || !self.clip.contains(x, y) {
            return;
        }
        let idx = y as usize * self.width + x as usize;
        let pixels = self.buffer.as_mut_slice();
        pixels[idx] = blend_u8(pixels[idx], color, alpha);
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
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        let color = color & 0x00ff_ffff;
        if alpha == 255 {
            for y in clip.y..clip.bottom() {
                let start = y as usize * width + clip.x as usize;
                pixels[start..start + clip.w as usize].fill(color);
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
        let span = rect.h.max(1) as u32;
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        for y in clip.y..clip.bottom() {
            let t = (((y - rect.y).max(0) as u32 * 255) / span).min(255) as u8;
            let color = blend_u8(top, bottom, t);
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
        let span = (rect.w - 1).max(1) as u32;
        let width = self.width;
        let pixels = self.buffer.as_mut_slice();
        let first_start = clip.y as usize * width + clip.x as usize;
        for dx in 0..clip.w {
            let x = clip.x + dx;
            let t = (((x - rect.x).max(0) as u32 * 255) / span).min(255) as u8;
            pixels[first_start + dx as usize] = blend_u8(left, right, t);
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
        let color = color & 0x00ff_ffff;
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
        let c0 = even_color & 0x00ff_ffff;
        let c1 = odd_color & 0x00ff_ffff;
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
        let rf = r as f32;
        for y in clip.y..clip.bottom() {
            let t = (((y - rect.y).max(0) as u32 * 255) / span).min(255) as u8;
            let row_color = blend_u8(top, bottom, t);
            let in_top = y < rect.y + r;
            let in_bot = y >= rect.bottom() - r;
            if !in_top && !in_bot {
                self.fill_u8(Rect::new(clip.x, y, clip.w, 1), row_color, a_u8);
                continue;
            }
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
    }

    pub fn rounded_border(&mut self, rect: Rect, radius: i32, color: u32, alpha: f32, width: i32) {
        let a_u8 = f32_to_u8(alpha);
        let bw = width.max(1);
        if a_u8 == 0 || rect.is_empty() {
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
        let target = Rect::new(x, y, w as i32, h as i32);
        let clip = self.clip_rect(target);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let stride = self.width;
        let pixels = self.buffer.as_mut_slice();
        let color = color & 0x00ff_ffff;
        for py in clip.y..clip.bottom() {
            let gy = (py - y) as usize;
            let cov_row = &coverage[gy * w..gy * w + w];
            let dst_row = &mut pixels[py as usize * stride..py as usize * stride + stride];
            let col_start = (clip.x - x) as usize;
            let col_len = clip.w as usize;
            let dst_x = clip.x as usize;
            for i in 0..col_len {
                let alpha = cov_row[col_start + i];
                if alpha != 0 {
                    let slot = &mut dst_row[dst_x + i];
                    *slot = blend_u8(*slot, color, alpha);
                }
            }
        }
    }

    /// Blit a premultiplied `0xAARRGGBB` sprite at `(x, y)` respecting `clip`.
    pub fn blit_sprite(&mut self, x: i32, y: i32, sprite: &Sprite) {
        let target = Rect::new(x, y, sprite.width as i32, sprite.height as i32);
        let clip = self.clip_rect(target);
        if clip.is_empty() {
            return;
        }
        self.mark_dirty(clip);
        let sw = sprite.width as usize;
        let dw = self.width;
        let pixels = self.buffer.as_mut_slice();
        for py in clip.y..clip.bottom() {
            let sy = (py - y) as usize;
            let src_row = &sprite.pixels[sy * sw..sy * sw + sw];
            let dst_row = &mut pixels[py as usize * dw..py as usize * dw + dw];
            let sx_start = (clip.x - x) as usize;
            let dx_start = clip.x as usize;
            for i in 0..clip.w as usize {
                let spx = src_row[sx_start + i];
                if (spx >> 24) != 0 {
                    let slot = &mut dst_row[dx_start + i];
                    *slot = blend_premul(*slot, spx);
                }
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
                *slot = blend_u8(*slot, row_color, alpha);
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
                        *slot = blend_u8(*slot, fill, alpha);
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
}
