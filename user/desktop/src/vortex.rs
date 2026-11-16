/*
 *
 *       user/desktop/src/vortex.rs
 *       VortexGlass 4.0 textures, screen-anchored reflection sheen, fog halo,
 *       procedural SpaceOS Starlight Crystal Start Orb and Win7 Aurora backdrops
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Shared VortexGlass 4.0 visual algorithms ported and upgraded from
//! `Uinxed-Kernel` (`window.c` / `gfx.c`) for SPUI 4.0 and `Shell` compositor.

use crate::draw::{
    blend_premul, blend_u8, mix, scale_premul, src_over_premul, src_over_u8, Canvas, Rect, Sprite,
};
use crate::icons;
use crate::theme::Theme;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[allow(dead_code)]
pub const AERO_FRAME_SIDE: i32 = 8;
#[allow(dead_code)]
pub const AERO_FRAME_TOP_SHADOW: i32 = 4;
#[allow(dead_code)]
pub const AERO_FRAME_BOTTOM: i32 = 12;
#[allow(dead_code)]
pub const AERO_DAMAGE_SIDE: i32 = 12;
#[allow(dead_code)]
pub const AERO_DAMAGE_TOP: i32 = 8;
#[allow(dead_code)]
pub const AERO_DAMAGE_BOTTOM: i32 = 16;
pub const AERO_BLUR_RADIUS: i32 = 8;

const SYSTEM_VORTEX_DIR: &str = "/usr/share/spui/themes/vortexglass";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinBtnKind {
    Minimize,
    Maximize,
    Restore,
    Close,
}

pub struct VortexAssets {
    pub top: Option<Sprite>,
    pub bottom: Option<Sprite>,
    pub left: Option<Sprite>,
    pub right: Option<Sprite>,
    pub reflection: Option<Sprite>,
    pub btn_close: [Option<Sprite>; 4],
    pub btn_max: [Option<Sprite>; 4],
    pub btn_min: [Option<Sprite>; 4],
    pub btn_rst: [Option<Sprite>; 4],
}

static GLOBAL_ASSETS: OnceLock<VortexAssets> = OnceLock::new();

impl VortexAssets {
    /// Lazily load and cache all VortexGlass texture PNGs once per process.
    pub fn global() -> &'static Self {
        GLOBAL_ASSETS.get_or_init(Self::load)
    }

    fn resolve_dir() -> Option<PathBuf> {
        let sys = Path::new(SYSTEM_VORTEX_DIR);
        if sys.is_dir() {
            return Some(sys.to_path_buf());
        }
        let candidates = [
            Path::new("user/desktop/assets/themes/vortexglass"),
            Path::new("/home/jitianyu/Project/SpaceKernel/user/desktop/assets/themes/vortexglass"),
        ];
        for cand in candidates {
            if cand.is_dir() {
                return Some(cand.to_path_buf());
            }
        }
        None
    }

    fn load_one(dir: Option<&Path>, name: &str) -> Option<Sprite> {
        let dir = dir?;
        icons::decode_png(&dir.join(name))
    }

    fn load_states_or_uinxed(
        dir: Option<&Path>,
        uinxed_name: &str,
        uinxed_light: &str,
        fallback_prefix: &str,
    ) -> [Option<Sprite>; 4] {
        let rest = Self::load_one(dir, uinxed_name)
            .or_else(|| Self::load_one(dir, &format!("{fallback_prefix}_0.png")));
        let light = Self::load_one(dir, uinxed_light)
            .or_else(|| Self::load_one(dir, &format!("{fallback_prefix}_1.png")));
        let press = Self::load_one(dir, uinxed_light)
            .or_else(|| Self::load_one(dir, &format!("{fallback_prefix}_2.png")));
        let inact = Self::load_one(dir, uinxed_name)
            .or_else(|| Self::load_one(dir, &format!("{fallback_prefix}_3.png")));
        [rest, light, press, inact]
    }

    pub fn load() -> Self {
        let dir_buf = Self::resolve_dir();
        let dir = dir_buf.as_deref();
        let raw_top = Self::load_one(dir, "top.png");
        let (top, bottom, left, right) = build_slim_aero_borders(raw_top.as_ref());
        let btn_max = Self::load_states_or_uinxed(dir, "max.png", "max_light.png", "btn_max");
        let btn_rst = [
            btn_max[0].clone(),
            btn_max[1].clone(),
            btn_max[2].clone(),
            btn_max[3].clone(),
        ];
        Self {
            top: Some(top),
            bottom: Some(bottom),
            left: Some(left),
            right: Some(right),
            reflection: Self::load_one(dir, "reflection.png"),
            btn_close: Self::load_states_or_uinxed(
                dir,
                "close.png",
                "close_light.png",
                "btn_close",
            ),
            btn_max,
            btn_min: Self::load_states_or_uinxed(dir, "min.png", "min_light.png", "btn_min"),
            btn_rst,
        }
    }

    pub fn caption_sprites(&self, kind: WinBtnKind) -> &[Option<Sprite>; 4] {
        match kind {
            WinBtnKind::Minimize => &self.btn_min,
            WinBtnKind::Maximize => &self.btn_max,
            WinBtnKind::Restore => &self.btn_rst,
            WinBtnKind::Close => &self.btn_close,
        }
    }
}

/// Quintic Perlin smootherstep (`6t^5 - 15t^4 + 10t^3`) ported from `Uinxed-Kernel`.
#[inline(always)]
pub fn smootherstep(t: f32) -> f32 {
    let x = t.clamp(0.0, 1.0);
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

/// Paint Uinxed-Kernel's 8px white luminous fog halo (`draw_vortexglass_fog`)
/// behind titlebar text so dark Aero titles remain crisp over any wallpaper.
pub fn draw_vortexglass_fog(canvas: &mut Canvas, text_rect: Rect, color: u32, max_alpha: f32) {
    if text_rect.w <= 0 || text_rect.h <= 0 || max_alpha <= 0.002 {
        return;
    }
    const PAD: i32 = 9;
    let outer = Rect::new(
        text_rect.x - PAD,
        text_rect.y - 5,
        text_rect.w + PAD * 2,
        text_rect.h + 10,
    );
    let rx = (outer.w as f32 * 0.5).max(1.0);
    let ry = (outer.h as f32 * 0.5).max(1.0);
    let cx = outer.x as f32 + rx;
    let cy = outer.y as f32 + ry;
    let argb = canvas.is_argb();
    let stride = canvas.width;
    let clip = canvas.clip_rect(outer);
    if clip.is_empty() {
        return;
    }
    canvas.mark_dirty(clip);
    let pixels = canvas.pixels_mut();
    let rgb_color = color & 0x00ff_ffff;
    for py in clip.y..clip.bottom() {
        let dy = ((py as f32 + 0.5 - cy) / ry).abs();
        let dy2 = dy * dy;
        if dy2 >= 1.0 {
            continue;
        }
        let row = &mut pixels[py as usize * stride..py as usize * stride + stride];
        for px in clip.x..clip.right() {
            let dx = ((px as f32 + 0.5 - cx) / rx).abs();
            let d = (dx * dx + dy2).sqrt();
            if d < 1.0 {
                let s = smootherstep(1.0 - d);
                let a = (s * max_alpha * 255.0 + 0.5) as u8;
                if a > 0 {
                    let slot = &mut row[px as usize];
                    *slot = if argb {
                        src_over_u8(*slot, rgb_color, a)
                    } else {
                        blend_u8(*slot, rgb_color, a)
                    };
                }
            }
        }
    }
}

/// Paint screen-coordinate-anchored `reflection.png` diagonal Aero glass sheen
/// (`wm_blit_sheen` from Uinxed-Kernel: horizontal X anchored to screen coordinate
/// `screen_origin_x`, vertical Y sampled 1:1 from art top).
pub fn blit_aero_sheen(
    canvas: &mut Canvas,
    dst_rect: Rect,
    screen_origin_x: i32,
    _screen_origin_y: i32,
    title_avoid: Option<Rect>,
    opacity: f32,
) {
    let assets = VortexAssets::global();
    let Some(ref refl) = assets.reflection else {
        // Subtle procedural diagonal sheen fallback when texture file is absent.
        canvas.diagonal_shimmer(dst_rect, 0.38, 96.0, 0xffffff, 0.14 * opacity);
        return;
    };
    let clip = canvas.clip_rect(dst_rect);
    if clip.is_empty() || opacity <= 0.01 {
        return;
    }
    let rw = refl.width as i32;
    let rh = refl.height as i32;
    if rw <= 0 || rh <= 0 {
        return;
    }
    canvas.mark_dirty(clip);
    let op_u8 = (opacity.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let argb = canvas.is_argb();
    let stride = canvas.width;
    let pixels = canvas.pixels_mut();
    for py in clip.y..clip.bottom() {
        let sy = (py - dst_rect.y).rem_euclid(rh) as usize;
        let src_row = &refl.pixels[sy * rw as usize..(sy + 1) * rw as usize];
        let dst_row = &mut pixels[py as usize * stride..py as usize * stride + stride];
        for px in clip.x..clip.right() {
            let sx = (screen_origin_x + (px - dst_rect.x)).rem_euclid(rw) as usize;
            let mut spx = src_row[sx];
            if (spx >> 24) == 0 {
                continue;
            }
            let mut local_op = op_u8;
            if let Some(av) = title_avoid {
                if px >= av.x && px < av.right() && py >= av.y && py < av.bottom() {
                    local_op /= 3;
                }
            }
            if local_op < 255 {
                spx = scale_premul(spx, local_op);
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

/// Faint static top light over Aero glass (`draw_vortexglass_peek` from `Uinxed-Kernel`).
pub fn draw_vortexglass_peek(canvas: &mut Canvas, rect: Rect, peak: u8) {
    let clip = canvas.clip_rect(rect);
    if clip.is_empty() || rect.h <= 0 || peak == 0 {
        return;
    }
    canvas.mark_dirty(clip);
    let argb = canvas.is_argb();
    let stride = canvas.width;
    let pixels = canvas.pixels_mut();
    let h = rect.h;
    for py in clip.y..clip.bottom() {
        let a = ((peak as i32) * (h - (py - rect.y))) / h;
        if a <= 0 {
            continue;
        }
        let a_u8 = a.clamp(0, 255) as u8;
        let row = &mut pixels
            [py as usize * stride + clip.x as usize..py as usize * stride + clip.right() as usize];
        for slot in row.iter_mut() {
            *slot = if argb {
                src_over_u8(*slot, 0xeb_f5_ff, a_u8)
            } else {
                blend_u8(*slot, 0xeb_f5_ff, a_u8)
            };
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_h_3slice_rows(
    canvas: &mut Canvas,
    sprite: &Sprite,
    dst_x: i32,
    dst_y: i32,
    dst_w: i32,
    src_y: i32,
    src_h: i32,
    dst_h: i32,
    repeat_single_row: bool,
    corner: i32,
    opacity: u8,
) {
    if dst_w <= 0
        || dst_h <= 0
        || src_h <= 0
        || opacity == 0
        || sprite.width == 0
        || sprite.height == 0
    {
        return;
    }
    let sw = sprite.width as i32;
    let c = corner.min(sw / 2);
    if dst_w < c * 2 {
        let hw = dst_w / 2;
        if repeat_single_row {
            for ry in 0..dst_h {
                canvas.blit_sprite_subrect(
                    dst_x,
                    dst_y + ry,
                    sprite,
                    Rect::new(0, src_y, hw, 1),
                    opacity,
                );
                canvas.blit_sprite_subrect(
                    dst_x + hw,
                    dst_y + ry,
                    sprite,
                    Rect::new(sw - (dst_w - hw), src_y, dst_w - hw, 1),
                    opacity,
                );
            }
        } else {
            let h = src_h.min(dst_h);
            canvas.blit_sprite_subrect(dst_x, dst_y, sprite, Rect::new(0, src_y, hw, h), opacity);
            canvas.blit_sprite_subrect(
                dst_x + hw,
                dst_y,
                sprite,
                Rect::new(sw - (dst_w - hw), src_y, dst_w - hw, h),
                opacity,
            );
        }
        return;
    }

    if repeat_single_row {
        for ry in 0..dst_h {
            canvas.blit_sprite_subrect(
                dst_x,
                dst_y + ry,
                sprite,
                Rect::new(0, src_y, c, 1),
                opacity,
            );
            canvas.blit_sprite_subrect(
                dst_x + dst_w - c,
                dst_y + ry,
                sprite,
                Rect::new(sw - c, src_y, c, 1),
                opacity,
            );
        }
    } else {
        let h = src_h.min(dst_h);
        canvas.blit_sprite_subrect(dst_x, dst_y, sprite, Rect::new(0, src_y, c, h), opacity);
        canvas.blit_sprite_subrect(
            dst_x + dst_w - c,
            dst_y,
            sprite,
            Rect::new(sw - c, src_y, c, h),
            opacity,
        );
    }

    let mid_rect = Rect::new(
        dst_x + c,
        dst_y,
        dst_w - c * 2,
        if repeat_single_row {
            dst_h
        } else {
            src_h.min(dst_h)
        },
    );
    let clip = canvas.clip_rect(mid_rect);
    if clip.is_empty() {
        return;
    }
    canvas.mark_dirty(clip);
    let argb = canvas.argb;
    let stride = canvas.width;
    let pixels = canvas.pixels_mut();
    let mid_sx = (sw / 2) as usize;
    let sw_u = sprite.width as usize;
    let sh_i = sprite.height as i32;
    for py in clip.y..clip.bottom() {
        let sy = if repeat_single_row {
            src_y.clamp(0, sh_i - 1)
        } else {
            (src_y + (py - dst_y)).clamp(0, sh_i - 1)
        } as usize;
        let mut spx = sprite.pixels[sy * sw_u + mid_sx];
        if (spx >> 24) == 0 {
            continue;
        }
        if opacity < 255 {
            spx = scale_premul(spx, opacity);
            if (spx >> 24) == 0 {
                continue;
            }
        }
        let drow = &mut pixels
            [py as usize * stride + clip.x as usize..py as usize * stride + clip.right() as usize];
        for slot in drow.iter_mut() {
            *slot = if argb {
                src_over_premul(*slot, spx)
            } else {
                blend_premul(*slot, spx)
            };
        }
    }
}

fn draw_v_3slice(
    canvas: &mut Canvas,
    sprite: &Sprite,
    dst_x: i32,
    dst_y: i32,
    dst_h: i32,
    opacity: u8,
) {
    if dst_h <= 0 || opacity == 0 || sprite.width == 0 || sprite.height == 0 {
        return;
    }
    let sw = sprite.width as i32;
    let sh = sprite.height as i32;
    let top_h = 40.min(sh / 2);
    let bot_h = 40.min(sh / 2);
    if dst_h <= top_h + bot_h {
        let hh = dst_h / 2;
        canvas.blit_sprite_subrect(dst_x, dst_y, sprite, Rect::new(0, 0, sw, hh), opacity);
        canvas.blit_sprite_subrect(
            dst_x,
            dst_y + hh,
            sprite,
            Rect::new(0, sh - (dst_h - hh), sw, dst_h - hh),
            opacity,
        );
        return;
    }

    canvas.blit_sprite_subrect(dst_x, dst_y, sprite, Rect::new(0, 0, sw, top_h), opacity);
    canvas.blit_sprite_subrect(
        dst_x,
        dst_y + dst_h - bot_h,
        sprite,
        Rect::new(0, sh - bot_h, sw, bot_h),
        opacity,
    );

    let mid_rect = Rect::new(dst_x, dst_y + top_h, sw, dst_h - top_h - bot_h);
    let clip = canvas.clip_rect(mid_rect);
    if clip.is_empty() {
        return;
    }
    canvas.mark_dirty(clip);
    let mid_sy = 200.min(sh - 1) as usize;
    let sw_u = sprite.width as usize;
    let argb = canvas.argb;
    let stride = canvas.width;
    let pixels = canvas.pixels_mut();
    let srow = &sprite.pixels[mid_sy * sw_u..(mid_sy + 1) * sw_u];
    let sx0 = (clip.x - dst_x) as usize;
    let cnt = clip.w as usize;
    for py in clip.y..clip.bottom() {
        let drow = &mut pixels
            [py as usize * stride + clip.x as usize..py as usize * stride + clip.x as usize + cnt];
        for (slot, &raw) in drow.iter_mut().zip(srow[sx0..sx0 + cnt].iter()) {
            let mut spx = raw;
            if (spx >> 24) == 0 {
                continue;
            }
            if opacity < 255 {
                spx = scale_premul(spx, opacity);
                if (spx >> 24) == 0 {
                    continue;
                }
            }
            *slot = if argb {
                src_over_premul(*slot, spx)
            } else {
                blend_premul(*slot, spx)
            };
        }
    }
}

/// Draw crisp translucent glass surface accents (`glass_tint` + `reflection.png` +
/// top peek light + inner/outer rim) on top of an already-blurred glass `rect`.
pub fn draw_glass_chrome_overlay(
    canvas: &mut Canvas,
    rect: Rect,
    radius: i32,
    theme: &Theme,
    active: bool,
    screen_x: i32,
    screen_y: i32,
    title_avoid: Option<Rect>,
) {
    let (tint, tint_a) = theme.glass_tint(active);
    if radius > 0 {
        canvas.rounded_rect(rect, radius, tint, tint_a as f32 / 255.0);
    } else {
        canvas.fill_u8(rect, tint, tint_a);
    }
    let refl_op = if theme.is_dark() {
        if active {
            0.48
        } else {
            0.30
        }
    } else if active {
        0.72
    } else {
        0.40
    };
    canvas.with_clip(rect, |c| {
        blit_aero_sheen(c, rect, screen_x, screen_y, title_avoid, refl_op);
        draw_vortexglass_peek(c, Rect::new(rect.x, rect.y, rect.w, rect.h.min(28)), 12);
    });
    let inner_hi = if theme.is_dark() { 0.28 } else { 0.58 };
    let outer_c = if active {
        theme.window_border_active
    } else {
        theme.window_border
    };
    if radius > 0 {
        canvas.rounded_border(rect.inset(1), (radius - 1).max(0), 0xffffff, inner_hi, 1);
        canvas.rounded_border(rect, radius, outer_c, if active { 0.85 } else { 0.55 }, 1);
    } else {
        canvas.fill_alpha(Rect::new(rect.x, rect.y, rect.w, 1), outer_c, 0.78);
        canvas.fill_alpha(Rect::new(rect.x, rect.y + 1, rect.w, 1), 0xffffff, inner_hi);
    }
}

#[inline(always)]
fn premul_rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    if a == 0 {
        return 0;
    }
    if a == 255 {
        return 0xff00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
    }
    let af = a as u32 + (a as u32 >> 7);
    let pr = ((r as u32 * af) >> 8).min(a as u32);
    let pg = ((g as u32 * af) >> 8).min(a as u32);
    let pb = ((b as u32 * af) >> 8).min(a as u32);
    ((a as u32) << 24) | (pr << 16) | (pg << 8) | pb
}

#[inline(always)]
fn eval_frame_ring_px(dist_in: f32, is_rim: bool, max_shadow: f32, shadow_base: f32) -> u32 {
    if is_rim {
        return premul_rgba(40, 56, 74, 108);
    }
    if dist_in < 0.0 {
        let out = -dist_in;
        if out > max_shadow {
            return 0;
        }
        let t = (1.0 - out / max_shadow).clamp(0.0, 1.0);
        let a = (shadow_base * t * t).round().clamp(0.0, 255.0) as u8;
        return premul_rgba(0, 0, 0, a);
    }
    if dist_in < 1.0 {
        // 1px outer dark Aero hairline
        let cov = dist_in.clamp(0.0, 1.0);
        return premul_rgba(36, 44, 56, (212.0 * cov).round() as u8);
    }
    if dist_in < 2.0 {
        // 1px crisp white inner bevel
        return premul_rgba(236, 242, 248, 206);
    }
    // Sleek airy frosted glass band
    premul_rgba(255, 255, 255, 58)
}

/// Synthesize sleek `C^1`-continuous Win7 Aero frame textures (`side = 8px`,
/// `top_shadow = 4px`, `bottom = 12px`) anchored to `top.png`'s titlebar crystal curve.
fn build_slim_aero_borders(top_src: Option<&Sprite>) -> (Sprite, Sprite, Sprite, Sprite) {
    const TW: usize = 336;
    const TH: usize = 27;
    let mut top_px = vec![0u32; TW * TH];
    let cx_t = 9.0f32;
    let cy_t = 9.5f32;
    let r_t = 5.5f32;
    for y in 0..TH {
        let top_glass_px = top_src.and_then(|s| {
            if y >= 6 && y <= 24 && y < s.height as usize && s.width > 0 {
                Some(s.pixels[y * s.width as usize + (s.width as usize / 2)])
            } else {
                None
            }
        });
        for x in 0..TW {
            let mx = x.min(TW - 1 - x) as f32;
            let py = y as f32;
            if y >= 8 && mx >= 7.0 {
                top_px[y * TW + x] = match y {
                    25 => premul_rgba(238, 244, 250, 155),
                    26 => premul_rgba(40, 56, 74, 108),
                    _ => top_glass_px
                        .map(|p| scale_premul(p, 215))
                        .unwrap_or_else(|| {
                            let a = (92 - ((y - 6) * 2)) as u8;
                            premul_rgba(255, 255, 255, a)
                        }),
                };
                continue;
            }
            if y >= 8 && mx < 7.0 {
                // Match left/right side frame columns pixel-for-pixel below corner arc
                let dist_in = (mx + 0.5) - 3.5;
                let rim = y == 26 && mx >= 7.0;
                top_px[y * TW + x] = eval_frame_ring_px(dist_in, rim, 3.8, 56.0);
                continue;
            }
            let dist_in = if mx < cx_t && py < cy_t {
                r_t - ((mx + 0.5 - cx_t).hypot(py + 0.5 - cy_t))
            } else {
                (py + 0.5) - 4.0
            };
            top_px[y * TW + x] = if dist_in >= 2.0 && y >= 6 {
                top_glass_px
                    .map(|p| scale_premul(p, 215))
                    .unwrap_or_else(|| premul_rgba(255, 255, 255, 92))
            } else {
                eval_frame_ring_px(dist_in, false, 4.2, 56.0)
            };
        }
    }

    const BW: usize = 181;
    const BH: usize = AERO_FRAME_BOTTOM as usize; // 12
    let mut bot_px = vec![0u32; BW * BH];
    let cx_b = 8.0f32;
    let cy_b = 0.0f32;
    let r_b = 4.5f32;
    for y in 0..BH {
        for x in 0..BW {
            let mx = x.min(BW - 1 - x) as f32;
            let py = y as f32;
            if y == 0 && mx >= 7.0 {
                bot_px[y * BW + x] = premul_rgba(40, 56, 74, 108);
                continue;
            }
            let dist_in = if mx < cx_b {
                r_b - ((mx + 0.5 - cx_b).hypot(py + 0.5 - cy_b))
            } else {
                4.5 - (py + 0.5)
            };
            bot_px[y * BW + x] = eval_frame_ring_px(dist_in, false, 7.2, 66.0);
        }
    }

    const SW: usize = AERO_FRAME_SIDE as usize; // 8
    const SH: usize = 600;
    let mut left_px = vec![0u32; SW * SH];
    let mut right_px = vec![0u32; SW * SH];
    for y in 0..SH {
        // Gentle Win7 Aero vertical sheen fade toward window bottom
        let vfade = (255.0 - (y as f32 / SH as f32) * 55.0).round() as u8;
        for x in 0..SW {
            let dist_in = (x as f32 + 0.5) - 3.5;
            let px = scale_premul(eval_frame_ring_px(dist_in, x == 7, 3.8, 56.0), vfade);
            left_px[y * SW + x] = px;
            right_px[y * SW + (SW - 1 - x)] = px;
        }
    }

    (
        Sprite {
            width: TW as u32,
            height: TH as u32,
            pixels: top_px,
        },
        Sprite {
            width: BW as u32,
            height: BH as u32,
            pixels: bot_px,
        },
        Sprite {
            width: SW as u32,
            height: SH as u32,
            pixels: left_px,
        },
        Sprite {
            width: SW as u32,
            height: SH as u32,
            pixels: right_px,
        },
    )
}

/// Draw Uinxed-Kernel's authentic 3-slice VortexGlass Aero window frame, reflection sheen,
/// top peek light, and subtle accent tinting around `draw_rect` (where `glass_top` is the
/// titlebar + extended glass toolbar height inside `draw_rect`).
pub fn draw_vortex_frame_chrome(
    canvas: &mut Canvas,
    draw_rect: Rect,
    glass_top: i32,
    theme: &Theme,
    active: bool,
    maximized: bool,
) {
    if draw_rect.is_empty() {
        return;
    }
    let assets = VortexAssets::global();
    let edge_op = if active { 255u8 } else { 175u8 };
    let (tint, tint_a) = theme.glass_tint(active);
    let refl_op = if theme.is_dark() {
        if active {
            0.48
        } else {
            0.28
        }
    } else if active {
        0.72
    } else {
        0.38
    };

    let top_glass_h = glass_top.clamp(1, draw_rect.h);
    let client_y = draw_rect.y + top_glass_h;
    let client_h = (draw_rect.h - top_glass_h).max(0);
    let scr_w = canvas.width as i32;
    let tstrip_x = if maximized {
        0
    } else {
        draw_rect.x - AERO_FRAME_SIDE
    };
    let tstrip_w = if maximized {
        scr_w
    } else {
        draw_rect.w + AERO_FRAME_SIDE * 2
    };
    let glass_x = if maximized { 0 } else { draw_rect.x - 4 };
    let glass_w = if maximized { scr_w } else { draw_rect.w + 8 };

    // 1) Top titlebar / extended glass strip (tint -> top.png -> reflection -> top peek).
    let top_glass_rect = Rect::new(glass_x, draw_rect.y + 1, glass_w, (top_glass_h - 1).max(1));
    if !canvas
        .clip_rect(Rect::new(
            tstrip_x,
            draw_rect.y - AERO_FRAME_TOP_SHADOW,
            tstrip_w,
            top_glass_h + AERO_FRAME_TOP_SHADOW,
        ))
        .is_empty()
    {
        canvas.fill_u8(top_glass_rect, tint, tint_a);
        if let Some(ref top) = assets.top {
            if !maximized {
                let head_y = draw_rect.y - AERO_FRAME_TOP_SHADOW;
                draw_h_3slice_rows(
                    canvas, top, tstrip_x, head_y, tstrip_w, 0, 25, 25, false, 24, edge_op,
                );
                let body_start = draw_rect.y + (25 - AERO_FRAME_TOP_SHADOW);
                let rim_y = (client_y - 2).max(body_start);
                if rim_y > body_start {
                    draw_h_3slice_rows(
                        canvas,
                        top,
                        tstrip_x,
                        body_start,
                        tstrip_w,
                        24,
                        1,
                        rim_y - body_start,
                        true,
                        24,
                        edge_op,
                    );
                }
                if client_y - 2 >= draw_rect.y + 1 {
                    draw_h_3slice_rows(
                        canvas,
                        top,
                        tstrip_x,
                        client_y - 2,
                        tstrip_w,
                        25,
                        2,
                        2,
                        false,
                        24,
                        edge_op,
                    );
                }
            } else {
                draw_h_3slice_rows(
                    canvas,
                    top,
                    0,
                    draw_rect.y,
                    scr_w,
                    5,
                    20,
                    20,
                    false,
                    24,
                    edge_op,
                );
                let body_start = draw_rect.y + 20;
                let rim_y = (client_y - 2).max(body_start);
                if rim_y > body_start {
                    draw_h_3slice_rows(
                        canvas,
                        top,
                        0,
                        body_start,
                        scr_w,
                        24,
                        1,
                        rim_y - body_start,
                        true,
                        24,
                        edge_op,
                    );
                }
                if client_y - 2 >= draw_rect.y {
                    draw_h_3slice_rows(
                        canvas,
                        top,
                        0,
                        client_y - 2,
                        scr_w,
                        25,
                        2,
                        2,
                        false,
                        24,
                        edge_op,
                    );
                }
            }
        }
        let sheen_h = (top_glass_h - 2).max(1);
        blit_aero_sheen(
            canvas,
            Rect::new(glass_x, draw_rect.y + 1, glass_w, sheen_h),
            glass_x,
            draw_rect.y + 1,
            None,
            refl_op,
        );
        draw_vortexglass_peek(
            canvas,
            Rect::new(glass_x, draw_rect.y + 1, glass_w, sheen_h.min(28)),
            12,
        );
    }

    if maximized {
        return;
    }

    // 2) Left, Right, and Bottom Aero borders (slim 5px visible glass frame + soft shadow).
    let rim_tint_a = tint_a / 3;
    if client_h > 0 {
        let side_y = client_y;
        let side_h = client_h;
        let left_band = Rect::new(
            draw_rect.x - AERO_FRAME_SIDE,
            side_y,
            AERO_FRAME_SIDE,
            side_h,
        );
        if !canvas.clip_rect(left_band).is_empty() {
            canvas.fill_u8(
                Rect::new(draw_rect.x - 4, side_y, 3, side_h),
                tint,
                rim_tint_a,
            );
            if let Some(ref left) = assets.left {
                draw_v_3slice(canvas, left, left_band.x, side_y, side_h, edge_op);
            }
            blit_aero_sheen(
                canvas,
                Rect::new(draw_rect.x - 4, side_y, 3, side_h),
                draw_rect.x - 4,
                side_y,
                None,
                refl_op * 0.65,
            );
        }

        let right_band = Rect::new(draw_rect.right(), side_y, AERO_FRAME_SIDE, side_h);
        if !canvas.clip_rect(right_band).is_empty() {
            canvas.fill_u8(
                Rect::new(draw_rect.right() + 1, side_y, 3, side_h),
                tint,
                rim_tint_a,
            );
            if let Some(ref right) = assets.right {
                draw_v_3slice(canvas, right, right_band.x, side_y, side_h, edge_op);
            }
            blit_aero_sheen(
                canvas,
                Rect::new(draw_rect.right() + 1, side_y, 3, side_h),
                draw_rect.right() + 1,
                side_y,
                None,
                refl_op * 0.65,
            );
        }
    }

    let bot_band = Rect::new(tstrip_x, draw_rect.bottom(), tstrip_w, AERO_FRAME_BOTTOM);
    if !canvas.clip_rect(bot_band).is_empty() {
        canvas.fill_u8(
            Rect::new(glass_x, draw_rect.bottom(), glass_w, 4),
            tint,
            rim_tint_a,
        );
        if let Some(ref bot) = assets.bottom {
            draw_h_3slice_rows(
                canvas,
                bot,
                bot_band.x,
                bot_band.y,
                bot_band.w,
                0,
                bot.height as i32,
                bot.height as i32,
                false,
                24,
                edge_op,
            );
        }
        blit_aero_sheen(
            canvas,
            Rect::new(glass_x, draw_rect.bottom(), glass_w, 4),
            glass_x,
            draw_rect.bottom(),
            None,
            refl_op * 0.65,
        );
    }
}

/// Draw a Windows 7 4-state crystal caption button sprite with smooth hover fade
/// (`btn_*_0.png` -> `btn_*_1.png`, or pressed `_2.png`, inactive `_3.png`).
pub fn draw_aero_caption_sprite(
    canvas: &mut Canvas,
    dst: Rect,
    kind: WinBtnKind,
    active: bool,
    hover_t: f32,
    pressed: bool,
) -> bool {
    let assets = VortexAssets::global();
    let states = assets.caption_sprites(kind);
    let Some(ref s_rest) = states[0] else {
        return false;
    };
    let s_hover = states[1].as_ref().unwrap_or(s_rest);
    let s_inact = states[3].as_ref().unwrap_or(s_rest);

    let bx = dst.x + (dst.w - s_rest.width as i32) / 2;
    let by = dst.y + (dst.h - s_rest.height as i32) / 2;
    if pressed {
        canvas.blit_sprite(bx, by, s_hover);
        let shade = Rect::new(
            bx + 1,
            by + 1,
            (s_rest.width as i32 - 2).max(1),
            (s_rest.height as i32 - 2).max(1),
        );
        canvas.fill_alpha(shade, 0x06101c, 0.24);
    } else if !active && hover_t <= 0.02 {
        canvas.blit_sprite_opacity(bx, by, s_inact, 148);
    } else if !active {
        canvas.blit_sprite_crossfade(bx, by, s_inact, s_hover, hover_t);
    } else {
        canvas.blit_sprite_crossfade(bx, by, s_rest, s_hover, hover_t);
    }
    true
}

/// Brand-new 60fps SpaceOS Starlight Crystal Start Orb (replaces legacy `orb_0..2.png`).
///
/// Renders a multi-layer anti-aliased crystal sphere with an accent-tinted radial
/// aurora bloom, curved specular dome, orbital ring and crisp 4-point star emblem.
#[allow(clippy::too_many_arguments)]
pub fn draw_space_start_orb(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    radius: f32,
    accent: u32,
    hover_t: f32,
    press_t: f32,
    pulse_phase: f32,
    is_dark: bool,
) {
    if radius <= 4.0 {
        return;
    }
    let pulse = 0.5 + 0.5 * (pulse_phase * std::f32::consts::TAU).sin();
    let glow_t = (hover_t * 0.85 + 0.15 * pulse + press_t * 0.20).clamp(0.0, 1.0);

    // 1) Outer aurora halo around the crystal orb on hover / open.
    let glow_c = mix(accent, 0x7ce2ff, 0.45);
    canvas.radial_glow(
        cx,
        cy,
        radius * (1.52 + 0.25 * glow_t),
        glow_c,
        0.48 * glow_t,
    );

    // 2) Spherical body with layered depth, internal cyan-azure nebula & specular cap.
    let pad = (radius + 2.0).ceil() as i32;
    let bounds = Rect::new(
        cx.floor() as i32 - pad,
        cy.floor() as i32 - pad,
        pad * 2 + 1,
        pad * 2 + 1,
    );
    let clip = bounds.intersect(canvas.bounds());
    if clip.is_empty() {
        return;
    }
    canvas.mark_dirty(clip);

    let rim_col = mix(0xaad8ff, accent, 0.35);
    let core_deep = if is_dark {
        0x071426
    } else {
        mix(0x0b2545, accent, 0.22)
    };
    let core_mid = mix(0x134e8e, accent, 0.50);
    let core_hi = mix(0x4bc8ff, accent, 0.25);
    let press_shift = press_t * 0.08 * radius;
    let argb = canvas.is_argb();
    let stride = canvas.width;
    let pixels = canvas.pixels_mut();
    let inv_r = 1.0 / radius;

    for py in clip.y..clip.bottom() {
        let ny = (py as f32 + 0.5 - (cy + press_shift)) * inv_r;
        let row = &mut pixels[py as usize * stride..py as usize * stride + stride];
        for px in clip.x..clip.right() {
            let nx = (px as f32 + 0.5 - cx) * inv_r;
            let dist_sq = nx * nx + ny * ny;
            let dist = dist_sq.sqrt();
            if dist > 1.0 + inv_r {
                continue;
            }
            let edge_aa = ((1.0 - dist) * radius + 0.5).clamp(0.0, 1.0);
            if edge_aa <= 0.0 {
                continue;
            }

            // Spherical base shading: deep oceanic sapphire + glowing lower caustic + hover core light.
            let bottom_glow = ((ny + 0.15).max(0.0) * (1.0 - nx * nx * 0.55)).clamp(0.0, 1.0);
            let mut col = mix(core_deep, core_mid, 0.30 + 0.55 * bottom_glow);
            col = mix(
                col,
                core_hi,
                (bottom_glow * bottom_glow) * (0.48 + 0.38 * glow_t),
            );

            // Tilted orbital ring inside crystal sphere.
            let rot_x = nx * 0.88 + ny * 0.47;
            let rot_y = -nx * 0.47 + ny * 0.88;
            let e_dist = ((rot_x / 0.72).powi(2) + (rot_y / 0.26).powi(2)).sqrt();
            let ring_aa = (1.0 - ((e_dist - 1.0).abs() * radius * 0.36)).clamp(0.0, 1.0);
            if ring_aa > 0.0 {
                let ring_col = mix(0xa5e8ff, 0xffffff, glow_t * 0.6);
                col = mix(col, ring_col, ring_aa * (0.52 + 0.35 * glow_t));
            }

            // Central 4-point SpaceOS starlight emblem (diamond SDF with soft glow).
            let star_d = (nx.abs().sqrt() + ny.abs().sqrt()) * 0.82;
            let star_core = (1.0 - star_d * 1.48).clamp(0.0, 1.0);
            let star_halo = (1.0 - (dist * 1.45)).clamp(0.0, 1.0);
            col = mix(
                col,
                0x9ae6ff,
                star_halo * star_halo * (0.35 + 0.30 * glow_t),
            );
            if star_core > 0.0 {
                let star_s = star_core * star_core * (3.0 - 2.0 * star_core);
                col = mix(col, 0xffffff, star_s * (0.85 + 0.15 * glow_t));
            }

            // Upper glass crescent specular cap (classic Aero orb shine).
            let dx_cap = nx;
            let dy_cap = (ny + 0.42) * 1.55;
            let cap_r = (dx_cap * dx_cap + dy_cap * dy_cap).sqrt();
            if cap_r < 0.86 && ny < 0.06 {
                let cap_t = ((1.0 - cap_r / 0.86) * (0.55 - ny * 0.65)).clamp(0.0, 1.0);
                col = mix(col, 0xffffff, cap_t * (0.66 - 0.18 * press_t));
            }

            // Crisp Fresnel crystal rim & dark outer anti-aliased edge.
            if dist > 0.82 {
                let rim_t = ((dist - 0.82) / 0.16).clamp(0.0, 1.0);
                let rim_glow = (1.0 - (dist - 0.94).abs() * 14.0).clamp(0.0, 1.0);
                col = mix(col, rim_col, rim_glow * (0.65 + 0.30 * glow_t));
                col = mix(col, 0x05101d, rim_t * 0.45);
            }

            let a = (edge_aa * 255.0 + 0.5) as u8;
            let slot = &mut row[px as usize];
            *slot = if argb {
                src_over_u8(*slot, col, a)
            } else {
                blend_u8(*slot, col, a)
            };
        }
    }
}

/// Render a Windows 7 style Aurora luminous wave background (`power_splash`, `greeter`,
/// `oobe`, `login` glass wizard backdrop).
pub fn draw_aero_aurora(canvas: &mut Canvas, rect: Rect, accent: u32, is_dark: bool, phase: f32) {
    let top = if is_dark {
        mix(0x061221, accent, 0.22)
    } else {
        mix(0x0d365c, accent, 0.34)
    };
    let bot = if is_dark {
        mix(0x0d2138, accent, 0.32)
    } else {
        mix(0x1f6aa2, accent, 0.45)
    };
    canvas.vertical_gradient(rect, top, bot);

    // Sweeping soft radial cyan/emerald aurora blooms matching Win7 boot & logon screen.
    let shift = (phase * std::f32::consts::TAU).sin() * 0.04;
    let w = rect.w as f32;
    let h = rect.h as f32;
    canvas.with_clip(rect, |c| {
        c.radial_glow(
            rect.x as f32 + w * (0.28 + shift),
            rect.y as f32 + h * 0.26,
            w.max(h) * 0.55,
            mix(0x38bdf8, accent, 0.40),
            if is_dark { 0.22 } else { 0.34 },
        );
        c.radial_glow(
            rect.x as f32 + w * (0.76 - shift),
            rect.y as f32 + h * 0.72,
            w.max(h) * 0.52,
            mix(0x2dd4bf, accent, 0.35),
            if is_dark { 0.18 } else { 0.28 },
        );
        c.diagonal_shimmer(rect, 0.42 + shift * 0.5, w * 0.26, 0x9ee7ff, 0.12);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vortex_assets_decode_from_workspace() {
        let assets = VortexAssets::load();
        assert_eq!(
            assets.top.as_ref().map(|s| (s.width, s.height)),
            Some((336, 27))
        );
        assert_eq!(
            assets.left.as_ref().map(|s| (s.width, s.height)),
            Some((AERO_FRAME_SIDE as u32, 600))
        );
        assert_eq!(
            assets.right.as_ref().map(|s| (s.width, s.height)),
            Some((AERO_FRAME_SIDE as u32, 600))
        );
        assert_eq!(
            assets.bottom.as_ref().map(|s| (s.width, s.height)),
            Some((181, AERO_FRAME_BOTTOM as u32))
        );
        assert!(assets.reflection.is_some(), "reflection.png should decode");
        assert_eq!(
            assets.btn_close[0].as_ref().map(|s| (s.width, s.height)),
            Some((41, 18))
        );
        assert_eq!(
            assets.btn_max[0].as_ref().map(|s| (s.width, s.height)),
            Some((26, 18))
        );
        assert_eq!(
            assets.btn_min[0].as_ref().map(|s| (s.width, s.height)),
            Some((26, 18))
        );
        assert!(
            assets.btn_rst[0].is_some(),
            "btn_rst fallback should decode"
        );
    }

    #[test]
    fn space_start_orb_and_fog_render_cleanly() {
        let mut c = Canvas::new(96, 96, 0x102030);
        draw_space_start_orb(&mut c, 48.0, 48.0, 18.0, 0x5898d8, 0.8, 0.0, 0.25, false);
        assert_ne!(c.pixels()[48 * 96 + 48], 0x102030);

        let mut argb_c = Canvas::new(80, 32, 0);
        argb_c.set_argb(true);
        draw_vortexglass_fog(&mut argb_c, Rect::new(16, 6, 40, 16), 0xffffff, 0.75);
        assert!((argb_c.pixels()[14 * 80 + 36] >> 24) > 120);
    }
}
