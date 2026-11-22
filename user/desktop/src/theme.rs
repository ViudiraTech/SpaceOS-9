/*
 *
 *       user/desktop/src/theme.rs
 *       SPUI 4.0 palettes, metrics, package discovery and declarative CSS/SVG theme engine
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Visual constants and declarative theme skin engine for the desktop shell and
//! its applications. Built-in themes (`VortexGlass`, `Spui`, `Win98`) and external
//! `.spk` theme packages under `/usr/share/spui/themes/<id>/theme.toml` share a
//! zero-allocation runtime `Theme` handle. External theme packages can declare
//! multi-stop CSS `linear-gradient` / `radial-gradient` backgrounds, multi-layer
//! `inset` `box-shadow` bevels, per-corner `border-radius`, `text-shadow`, dark/accent
//! sub-variants and `.svg`/`.png` sprites for every window, taskbar, Start menu and
//! widget slot without hardcoding theme-specific renderers into SPUI.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::draw::{build_gradient_lut, mix, Canvas, Rect, Sprite};
use crate::icons;
use uxconf::{
    ButtonLayout, DesktopConfig, PanelPosition, ThemeMode, ThemeStyle, THEME_PRESETS, WALLPAPERS,
    WALLPAPER_DIR,
};

pub const TITLEBAR_HEIGHT: i32 = 34;
pub const VORTEX_TITLEBAR_HEIGHT: i32 = 30;
#[allow(dead_code)]
pub const TASKBAR_HEIGHT: i32 = 46;
#[allow(dead_code)]
pub const RADIUS_WINDOW: i32 = 6;
pub const RADIUS_POPUP: i32 = 8;
pub const RADIUS_CONTROL: i32 = 5;
pub const THEME_RELOAD_STAMP: &str = "/run/spui-theme.stamp";

/// Timestamp touched by the spk system hook after any theme/wallpaper package
/// change.  SPUI reloads its theme when this changes.
pub fn theme_reload_stamp() -> Option<SystemTime> {
    fs::metadata(THEME_RELOAD_STAMP)
        .and_then(|metadata| metadata.modified())
        .ok()
}

const SYSTEM_THEME_DIR: &str = "/usr/share/spui/themes";
const USER_THEME_SUBDIR: &str = ".local/share/spui/themes";

/// Miniature preview specification for an external theme card in System Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePreviewSpec {
    pub desktop_top: u32,
    pub desktop_bottom: u32,
    pub window_bg: u32,
    pub window_border: u32,
    pub window_top_radius: i32,
    pub titlebar_left: u32,
    pub titlebar_right: u32,
    pub close_btn: u32,
    pub min_btn: u32,
    pub max_btn: u32,
    pub taskbar_top: u32,
    pub taskbar_bottom: u32,
    pub start_btn: u32,
    pub tray_bg: u32,
}

/// Metadata for an installed external theme package discovered on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledThemeInfo {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub base_style: ThemeStyle,
    pub mode: ThemeMode,
    pub accent: u32,
    pub wallpaper: Option<String>,
    pub solid_color: Option<u32>,
    pub panel_position: Option<PanelPosition>,
    pub panel_size: Option<i32>,
    pub start_fullscreen: Option<bool>,
    pub button_layout: Option<ButtonLayout>,
    pub preview: ThemePreviewSpec,
}

/// Discovered wallpaper entry combining built-in and theme-installed wallpapers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredWallpaper {
    pub file: String,
    pub label: String,
}

/// Parsed CSS background specification (`solid`, `linear-gradient`, or `radial-gradient`)
/// with pre-baked 256-entry gradient LUTs.
#[derive(Clone, Debug, PartialEq)]
pub enum CssBackground {
    Solid(u32, f32),
    LinearV {
        stops: Vec<(f32, u32)>,
        alpha: f32,
        lut: Box<[u32; 256]>,
    },
    LinearH {
        stops: Vec<(f32, u32)>,
        alpha: f32,
        lut: Box<[u32; 256]>,
    },
    LinearDiag {
        stops: Vec<(f32, u32)>,
        alpha: f32,
        lut: Box<[u32; 256]>,
    },
    Radial {
        cx_pct: f32,
        cy_pct: f32,
        stops: Vec<(f32, u32)>,
        alpha: f32,
        lut: Box<[u32; 256]>,
    },
}

impl CssBackground {
    pub fn linear_v(stops: Vec<(f32, u32)>, alpha: f32) -> Self {
        let lut = Box::new(build_gradient_lut(&stops));
        Self::LinearV { stops, alpha, lut }
    }

    pub fn linear_h(stops: Vec<(f32, u32)>, alpha: f32) -> Self {
        let lut = Box::new(build_gradient_lut(&stops));
        Self::LinearH { stops, alpha, lut }
    }

    pub fn linear_diag(stops: Vec<(f32, u32)>, alpha: f32) -> Self {
        let lut = Box::new(build_gradient_lut(&stops));
        Self::LinearDiag { stops, alpha, lut }
    }

    pub fn radial(cx_pct: f32, cy_pct: f32, stops: Vec<(f32, u32)>, alpha: f32) -> Self {
        let lut = Box::new(build_gradient_lut(&stops));
        Self::Radial {
            cx_pct,
            cy_pct,
            stops,
            alpha,
            lut,
        }
    }

    pub fn primary_color(&self) -> u32 {
        match self {
            Self::Solid(c, _) => *c,
            Self::LinearV { stops, .. }
            | Self::LinearH { stops, .. }
            | Self::LinearDiag { stops, .. }
            | Self::Radial { stops, .. } => stops.first().map_or(0, |x| x.1),
        }
    }

    pub fn tinted(&self, target: u32, weight: f32) -> Self {
        let map_stops = |stops: &[(f32, u32)]| -> Vec<(f32, u32)> {
            stops.iter().map(|&(p, c)| (p, mix(c, target, weight))).collect()
        };
        match self {
            Self::Solid(c, a) => Self::Solid(mix(*c, target, weight), *a),
            Self::LinearV { stops, alpha, .. } => Self::linear_v(map_stops(stops), *alpha),
            Self::LinearH { stops, alpha, .. } => Self::linear_h(map_stops(stops), *alpha),
            Self::LinearDiag { stops, alpha, .. } => Self::linear_diag(map_stops(stops), *alpha),
            Self::Radial {
                cx_pct,
                cy_pct,
                stops,
                alpha,
                ..
            } => Self::radial(*cx_pct, *cy_pct, map_stops(stops), *alpha),
        }
    }

    pub fn paint(&self, canvas: &mut Canvas, rect: Rect, radii: [i32; 4]) {
        if rect.is_empty() {
            return;
        }
        match self {
            Self::Solid(color, alpha) => {
                canvas.rounded_rect_corners(rect, radii, *color, *alpha);
            }
            Self::LinearV { lut, alpha, .. } => {
                canvas.multi_stop_gradient_v_lut(rect, lut, radii, *alpha);
            }
            Self::LinearH { lut, alpha, .. } => {
                canvas.multi_stop_gradient_h_lut(rect, lut, radii, *alpha);
            }
            Self::LinearDiag { lut, alpha, .. } => {
                canvas.multi_stop_gradient_diag_lut(rect, lut, radii, *alpha);
            }
            Self::Radial {
                cx_pct,
                cy_pct,
                lut,
                alpha,
                ..
            } => {
                canvas.multi_stop_radial_gradient_lut(rect, *cx_pct, *cy_pct, lut, radii, *alpha);
            }
        }
    }
}

/// Single `inset` layer from a CSS `box-shadow` declaration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CssInsetShadow {
    pub dx: i32,
    pub dy: i32,
    pub blur: i32,
    pub spread: i32,
    pub color: u32,
    pub alpha: f32,
}

const SURFACE_CACHE_MAX_ENTRIES: usize = 128;
const SURFACE_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;
const SURFACE_CACHE_MAX_ENTRY_BYTES: usize = 1024 * 1024;

static NEXT_SURFACE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SurfaceCacheKey {
    cache_id: u64,
    w: u32,
    h: u32,
    radii: [i32; 4],
}

struct SurfaceCacheEntry {
    sprite: Arc<Sprite>,
    bytes: usize,
    last_used: u64,
}

#[derive(Default)]
struct SurfaceCache {
    entries: HashMap<SurfaceCacheKey, SurfaceCacheEntry>,
    total_bytes: usize,
    tick: u64,
}

impl SurfaceCache {
    fn get(&mut self, key: SurfaceCacheKey) -> Option<Arc<Sprite>> {
        if let Some(entry) = self.entries.get_mut(&key) {
            self.tick = self.tick.wrapping_add(1);
            entry.last_used = self.tick;
            return Some(Arc::clone(&entry.sprite));
        }
        None
    }

    fn insert(&mut self, key: SurfaceCacheKey, sprite: Arc<Sprite>) {
        let bytes = sprite.pixels.len().saturating_mul(std::mem::size_of::<u32>());
        if bytes == 0 || bytes > SURFACE_CACHE_MAX_ENTRY_BYTES {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(old.bytes);
        }
        while !self.entries.is_empty()
            && (self.entries.len() >= SURFACE_CACHE_MAX_ENTRIES
                || self.total_bytes + bytes > SURFACE_CACHE_MAX_BYTES)
        {
            let Some(evict_key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&evict_key) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.bytes);
            }
        }
        self.tick = self.tick.wrapping_add(1);
        self.total_bytes += bytes;
        self.entries.insert(
            key,
            SurfaceCacheEntry {
                sprite,
                bytes,
                last_used: self.tick,
            },
        );
    }
}

fn surface_cache() -> &'static Mutex<SurfaceCache> {
    static CACHE: OnceLock<Mutex<SurfaceCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(SurfaceCache::default()))
}

/// Complete declarative CSS + SVG surface for any UI widget state.
#[derive(Clone, Debug, Default)]
pub struct CssSurface {
    pub cache_id: u64,
    pub bg: Option<CssBackground>,
    pub box_shadows: Vec<CssInsetShadow>,
    pub border: Option<(u32, f32, i32)>,
    pub border_top: Option<(u32, i32)>,
    pub border_right: Option<(u32, i32)>,
    pub border_bottom: Option<(u32, i32)>,
    pub border_left: Option<(u32, i32)>,
    pub radii: Option<[i32; 4]>,
    pub sprite: Option<Sprite>,
}

impl CssSurface {
    pub fn assign_cache_id(&mut self) {
        if self.is_empty() {
            self.cache_id = 0;
        } else {
            self.cache_id = NEXT_SURFACE_ID.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bg.is_none()
            && self.box_shadows.is_empty()
            && self.border.is_none()
            && self.border_top.is_none()
            && self.border_right.is_none()
            && self.border_bottom.is_none()
            && self.border_left.is_none()
            && self.sprite.is_none()
    }

    fn should_cache(&self, rect: Rect, radii: [i32; 4]) -> bool {
        if self.cache_id == 0 || self.is_empty() {
            return false;
        }
        let bytes = (rect.w as usize)
            .saturating_mul(rect.h as usize)
            .saturating_mul(std::mem::size_of::<u32>());
        if bytes == 0 || bytes > SURFACE_CACHE_MAX_ENTRY_BYTES {
            return false;
        }
        // An opaque unrounded flat solid fill is a single canvas memset and is
        // already faster than cache lookup + sprite blit.
        let is_plain_flat_solid = radii == [0, 0, 0, 0]
            && self.box_shadows.is_empty()
            && self.border.is_none()
            && self.border_top.is_none()
            && self.border_right.is_none()
            && self.border_bottom.is_none()
            && self.border_left.is_none()
            && self.sprite.is_none()
            && matches!(self.bg, Some(CssBackground::Solid(_, a)) if a >= 0.999);
        !is_plain_flat_solid
    }

    pub fn paint(&self, canvas: &mut Canvas, rect: Rect, default_radii: [i32; 4]) {
        if rect.is_empty() || canvas.clip_rect(rect).is_empty() {
            return;
        }
        // Complete vector/raster button sprites are self-contained; never paint
        // fallback CSS backgrounds or borders beneath/over their anti-aliased edge.
        if let Some(ref sp) = self.sprite {
            let w = rect.w.max(1) as u32;
            let h = rect.h.max(1) as u32;
            if sp.width == w && sp.height == h {
                canvas.blit_sprite(rect.x, rect.y, sp);
                return;
            }
            if self.cache_id != 0
                && (w as usize)
                    .saturating_mul(h as usize)
                    .saturating_mul(4)
                    <= SURFACE_CACHE_MAX_ENTRY_BYTES
            {
                let key = SurfaceCacheKey {
                    cache_id: self.cache_id,
                    w,
                    h,
                    radii: [0; 4],
                };
                if let Ok(mut guard) = surface_cache().lock() {
                    if let Some(cached) = guard.get(key) {
                        drop(guard);
                        canvas.blit_sprite(rect.x, rect.y, &cached);
                        return;
                    }
                }
                let scaled = Arc::new(sp.scale_bilinear(w, h));
                if let Ok(mut guard) = surface_cache().lock() {
                    guard.insert(key, Arc::clone(&scaled));
                }
                canvas.blit_sprite(rect.x, rect.y, &scaled);
                return;
            }
            let scaled = sp.scale_bilinear(w, h);
            canvas.blit_sprite(rect.x, rect.y, &scaled);
            return;
        }

        let radii = self.radii.unwrap_or(default_radii);
        if self.should_cache(rect, radii) {
            let key = SurfaceCacheKey {
                cache_id: self.cache_id,
                w: rect.w as u32,
                h: rect.h as u32,
                radii,
            };
            if let Ok(mut guard) = surface_cache().lock() {
                if let Some(cached) = guard.get(key) {
                    drop(guard);
                    canvas.blit_sprite(rect.x, rect.y, &cached);
                    return;
                }
            }
            let mut off = Canvas::new(rect.w as usize, rect.h as usize, 0);
            off.set_argb(true);
            self.paint_uncached(&mut off, Rect::new(0, 0, rect.w, rect.h), radii);
            let sprite = Arc::new(off.into_sprite());
            if let Ok(mut guard) = surface_cache().lock() {
                guard.insert(key, Arc::clone(&sprite));
            }
            canvas.blit_sprite(rect.x, rect.y, &sprite);
            return;
        }

        self.paint_uncached(canvas, rect, radii);
    }

    pub fn paint_uncached(&self, canvas: &mut Canvas, rect: Rect, default_radii: [i32; 4]) {
        if rect.is_empty() {
            return;
        }
        if let Some(ref sp) = self.sprite {
            if sp.width == rect.w as u32 && sp.height == rect.h as u32 {
                canvas.blit_sprite(rect.x, rect.y, sp);
            } else {
                let scaled = sp.scale_bilinear(rect.w.max(1) as u32, rect.h.max(1) as u32);
                canvas.blit_sprite(rect.x, rect.y, &scaled);
            }
            return;
        }
        let radii = self.radii.unwrap_or(default_radii);
        if let Some(ref bg) = self.bg {
            bg.paint(canvas, rect, radii);
        }
        let border_inset = self.border.map_or(0, |(_, _, w)| w.max(0));
        let shadow_rect = rect.inset(border_inset);
        if !self.box_shadows.is_empty() && !shadow_rect.is_empty() {
            let inner_radii = [
                (radii[0] - border_inset).max(0),
                (radii[1] - border_inset).max(0),
                (radii[2] - border_inset).max(0),
                (radii[3] - border_inset).max(0),
            ];
            paint_css_inset_shadows(canvas, shadow_rect, &self.box_shadows, inner_radii);
        }
        if let Some((color, alpha, width)) = self.border {
            if width > 0 {
                canvas.rounded_border_corners(rect, radii, color, alpha, width);
            }
        }
        if let Some((color, w)) = self.border_top {
            let w = w.max(1);
            paint_clipped_strip(
                canvas,
                rect,
                radii,
                Rect::new(rect.x, rect.y, rect.w, w),
                color,
                1.0,
            );
        }
        if let Some((color, w)) = self.border_bottom {
            let w = w.max(1);
            paint_clipped_strip(
                canvas,
                rect,
                radii,
                Rect::new(rect.x, rect.bottom() - w, rect.w, w),
                color,
                1.0,
            );
        }
        if let Some((color, w)) = self.border_left {
            let w = w.max(1);
            paint_clipped_strip(
                canvas,
                rect,
                radii,
                Rect::new(rect.x, rect.y, w, rect.h),
                color,
                1.0,
            );
        }
        if let Some((color, w)) = self.border_right {
            let w = w.max(1);
            paint_clipped_strip(
                canvas,
                rect,
                radii,
                Rect::new(rect.right() - w, rect.y, w, rect.h),
                color,
                1.0,
            );
        }
    }
}

fn paint_clipped_strip(
    canvas: &mut Canvas,
    outer: Rect,
    radii: [i32; 4],
    strip: Rect,
    color: u32,
    alpha: f32,
) {
    let target = strip.intersect(outer);
    if target.is_empty() || alpha <= 0.001 {
        return;
    }
    if radii == [0, 0, 0, 0] {
        canvas.fill_alpha(target, color, alpha);
        return;
    }
    let clip = canvas.clip_rect(target);
    if clip.is_empty() {
        return;
    }
    canvas.mark_dirty(clip);

    let is_full_circle =
        outer.w == outer.h && radii.iter().all(|&r| r * 2 >= outer.w) && outer.w >= 4;
    if is_full_circle {
        for y in clip.y..clip.bottom() {
            for x in clip.x..clip.right() {
                let cov = Canvas::corner_point_coverage(outer, radii, x, y);
                if cov > 0.0 {
                    canvas.blend_pixel(x, y, color, alpha * cov);
                }
            }
        }
        return;
    }

    let max_r = (outer.w / 2).min(outer.h / 2).max(0);
    let tl = radii[0].clamp(0, max_r);
    let tr = radii[1].clamp(0, max_r);
    let br = radii[2].clamp(0, max_r);
    let bl = radii[3].clamp(0, max_r);

    let top_end = (outer.y + tl.max(tr)).clamp(clip.y, clip.bottom());
    let bot_start = (outer.bottom() - bl.max(br)).clamp(top_end, clip.bottom());

    if bot_start > top_end {
        canvas.fill_alpha(
            Rect::new(clip.x, top_end, clip.w, bot_start - top_end),
            color,
            alpha,
        );
    }

    let mut paint_corner_row = |y: i32| {
        let l_inset = if y < outer.y + tl {
            tl
        } else if y >= outer.bottom() - bl && bl > 0 {
            bl
        } else {
            0
        };
        let r_inset = if y < outer.y + tr {
            tr
        } else if y >= outer.bottom() - br && br > 0 {
            br
        } else {
            0
        };
        let mid_l = (outer.x + l_inset).max(clip.x);
        let mid_r = (outer.right() - r_inset).min(clip.right());
        if mid_r > mid_l {
            canvas.fill_alpha(Rect::new(mid_l, y, mid_r - mid_l, 1), color, alpha);
        }
        for x in clip.x..mid_l.min(clip.right()) {
            let cov = Canvas::corner_point_coverage(outer, radii, x, y);
            if cov > 0.0 {
                canvas.blend_pixel(x, y, color, alpha * cov);
            }
        }
        for x in mid_r.max(clip.x)..clip.right() {
            let cov = Canvas::corner_point_coverage(outer, radii, x, y);
            if cov > 0.0 {
                canvas.blend_pixel(x, y, color, alpha * cov);
            }
        }
    };

    for y in clip.y..top_end {
        paint_corner_row(y);
    }
    for y in bot_start..clip.bottom() {
        paint_corner_row(y);
    }
}

/// Paint CSS `box-shadow: inset ...` layers in reverse declaration order (first declared is topmost).
pub fn paint_css_inset_shadows(
    canvas: &mut Canvas,
    rect: Rect,
    shadows: &[CssInsetShadow],
    radii: [i32; 4],
) {
    for sh in shadows.iter().rev() {
        if sh.alpha <= 0.001 {
            continue;
        }
        let spread = sh.spread.max(0);
        if sh.dx == 0 && sh.dy == 0 {
            let w = spread.max(sh.blur).max(1);
            canvas.rounded_border_corners(rect, radii, sh.color, sh.alpha, w);
            continue;
        }
        if sh.dx > 0 {
            let w = (sh.dx + spread).clamp(1, rect.w);
            paint_clipped_strip(
                canvas,
                rect,
                radii,
                Rect::new(rect.x, rect.y, w, rect.h),
                sh.color,
                sh.alpha,
            );
        } else if sh.dx < 0 {
            let w = (-sh.dx + spread).clamp(1, rect.w);
            paint_clipped_strip(
                canvas,
                rect,
                radii,
                Rect::new(rect.right() - w, rect.y, w, rect.h),
                sh.color,
                sh.alpha,
            );
        }
        if sh.dy > 0 {
            let h = (sh.dy + spread).clamp(1, rect.h);
            paint_clipped_strip(
                canvas,
                rect,
                radii,
                Rect::new(rect.x, rect.y, rect.w, h),
                sh.color,
                sh.alpha,
            );
        } else if sh.dy < 0 {
            let h = (-sh.dy + spread).clamp(1, rect.h);
            paint_clipped_strip(
                canvas,
                rect,
                radii,
                Rect::new(rect.x, rect.bottom() - h, rect.w, h),
                sh.color,
                sh.alpha,
            );
        }
    }
}

#[derive(Clone, Debug)]
pub struct WindowSkin {
    pub top_radius: i32,
    pub bottom_radius: i32,
    pub border_width: i32,
    pub active_frame: CssSurface,
    pub inactive_frame: CssSurface,
}

/// Title text alignment inside custom theme titlebars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleAlign {
    Left,
    Center,
}

#[derive(Clone, Debug)]
pub struct TitlebarSkin {
    pub height: i32,
    pub title_align: TitleAlign,
    pub active: CssSurface,
    pub inactive: CssSurface,
    pub edge_glow_left: Option<(u32, i32)>,
    pub edge_glow_right: Option<(u32, i32)>,
    pub text_color: u32,
    pub inactive_text_color: u32,
    pub text_shadow: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct CaptionButtonsSkin {
    pub width: i32,
    pub height: i32,
    pub gap: i32,
    pub margin_right: i32,
    pub margin_top: i32,
    pub radius: i32,
    pub close_normal: CssSurface,
    pub close_hover: CssSurface,
    pub close_active: CssSurface,
    pub min_normal: CssSurface,
    pub min_hover: CssSurface,
    pub min_active: CssSurface,
    pub max_normal: CssSurface,
    pub max_hover: CssSurface,
    pub max_active: CssSurface,
    pub restore_normal: CssSurface,
    pub restore_hover: CssSurface,
    pub restore_active: CssSurface,
    pub glyph_color: u32,
    pub inactive_opacity: f32,
}

#[derive(Clone, Debug)]
pub struct TaskbarSkin {
    pub height: Option<i32>,
    pub bg: CssSurface,
    pub button_normal: CssSurface,
    pub button_hover: CssSurface,
    pub button_active: CssSurface,
    pub button_pressed: CssSurface,
    pub button_radius: i32,
    pub button_text: u32,
    pub button_active_text: u32,
    pub button_minimized_text: u32,
    pub tray_bg: Option<CssSurface>,
    pub tray_text: u32,
    pub tray_muted_text: u32,
}

#[derive(Clone, Debug)]
pub struct StartButtonSkin {
    pub width: i32,
    pub full_height: bool,
    pub margin_left: i32,
    pub radii: [i32; 4],
    pub normal: CssSurface,
    pub hover: CssSurface,
    pub active: CssSurface,
    pub label: String,
    pub text_color: u32,
    pub text_shadow: Option<u32>,
    pub icon: Option<Sprite>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMenuLayoutKind {
    Compact,
    TwoColumn,
}

#[derive(Clone, Debug)]
pub struct StartMenuSkin {
    pub layout: StartMenuLayoutKind,
    pub width: i32,
    pub height: i32,
    pub offset_x: i32,
    pub gap_y: i32,
    pub top_radius: i32,
    pub bottom_radius: i32,
    pub frame_border: u32,
    pub header_height: i32,
    pub header_bg: CssSurface,
    pub header_top_sheen: bool,
    pub header_text: u32,
    pub header_text_shadow: Option<u32>,
    pub avatar_border: u32,
    pub orange_divider: Option<u32>,
    pub left_bg: u32,
    pub left_text: u32,
    pub left_subtext: u32,
    pub right_bg: u32,
    pub right_border_left: u32,
    pub right_text: u32,
    pub item_hover_bg: u32,
    pub item_hover_text: u32,
    pub all_programs_label: String,
    pub all_programs_arrow: u32,
    pub footer_height: i32,
    pub footer_bg: CssSurface,
    pub footer_text: u32,
    pub logout_badge: CssSurface,
    pub reboot_badge: CssSurface,
    pub shutdown_badge: CssSurface,
}

#[derive(Clone, Debug)]
pub struct ControlsSkin {
    pub button_normal: CssSurface,
    pub button_hover: CssSurface,
    pub button_active: CssSurface,
    pub button_primary: CssSurface,
    pub button_primary_hover: CssSurface,
    pub button_primary_active: CssSurface,
    pub button_radius: i32,
    pub button_text: u32,
    pub button_primary_text: u32,
    pub icon_button_hover: CssSurface,
    pub icon_button_active: CssSurface,
    pub card: CssSurface,
    pub card_radius: i32,
    pub popup_surface: CssSurface,
    pub popup_radius: i32,
    pub switch_track_off: CssSurface,
    pub switch_track_on: CssSurface,
    pub switch_knob: CssSurface,
    pub switch_radius: i32,
    pub slider_track: CssSurface,
    pub slider_fill: CssSurface,
    pub slider_thumb: CssSurface,
    pub slider_thumb_hover: CssSurface,
    pub slider_thumb_radius: i32,
    pub list_item_selected: CssSurface,
    pub list_item_hover: CssSurface,
    pub list_item_radius: i32,
    pub list_item_selected_text: u32,
    pub toolbar_bg: CssSurface,
    pub tab_normal: CssSurface,
    pub tab_hover: CssSurface,
    pub tab_active: CssSurface,
    pub tab_radii: [i32; 4],
    pub tab_text: u32,
    pub tab_active_text: u32,
    pub input_bg: u32,
    pub input_border: u32,
    pub input_focus_border: u32,
    pub input_radius: i32,
    pub scrollbar_width: i32,
    pub scrollbar_track: CssSurface,
    pub scrollbar_thumb: CssSurface,
    pub scrollbar_thumb_hover: CssSurface,
    pub scrollbar_thumb_active: CssSurface,
    pub scrollbar_grip_light: Option<u32>,
    pub scrollbar_grip_dark: Option<u32>,
    pub progress_track: CssSurface,
    pub progress_fill: CssSurface,
    pub progress_chunk_w: i32,
    pub progress_chunk_gap: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SplashDim {
    Px(i32),
    Pct(f32),
}

impl SplashDim {
    pub fn resolve(self, total: i32) -> i32 {
        match self {
            Self::Px(v) => v,
            Self::Pct(f) => ((total as f32) * f).round() as i32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashAnchor {
    TopLeft,
    TopCenter,
    TopRight,
    Center,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl SplashAnchor {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "top_left" | "topleft" => Self::TopLeft,
            "top_center" | "topcenter" | "top" => Self::TopCenter,
            "top_right" | "topright" => Self::TopRight,
            "bottom_left" | "bottomleft" => Self::BottomLeft,
            "bottom_center" | "bottomcenter" | "bottom" => Self::BottomCenter,
            "bottom_right" | "bottomright" => Self::BottomRight,
            _ => Self::Center,
        }
    }

    pub fn resolve_rect(
        self,
        bounds: Rect,
        x: SplashDim,
        y: SplashDim,
        w: SplashDim,
        h: SplashDim,
    ) -> Rect {
        let dx = x.resolve(bounds.w);
        let dy = y.resolve(bounds.h);
        let rw = w.resolve(bounds.w).max(1);
        let rh = h.resolve(bounds.h).max(1);
        let rx = match self {
            Self::TopLeft | Self::BottomLeft => bounds.x + dx,
            Self::TopCenter | Self::Center | Self::BottomCenter => {
                bounds.x + bounds.w / 2 + dx - rw / 2
            }
            Self::TopRight | Self::BottomRight => bounds.right() + dx - rw,
        };
        let ry = match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => bounds.y + dy,
            Self::Center => bounds.y + bounds.h / 2 + dy - rh / 2,
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight => bounds.bottom() + dy - rh,
        };
        Rect::new(rx, ry, rw, rh)
    }

    pub fn resolve_text_point(self, bounds: Rect, x: SplashDim, y: SplashDim) -> (i32, i32) {
        let dx = x.resolve(bounds.w);
        let dy = y.resolve(bounds.h);
        let px = match self {
            Self::TopLeft | Self::BottomLeft => bounds.x + dx,
            Self::TopCenter | Self::Center | Self::BottomCenter => bounds.x + bounds.w / 2 + dx,
            Self::TopRight | Self::BottomRight => bounds.right() + dx,
        };
        let py = match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => bounds.y + dy,
            Self::Center => bounds.y + bounds.h / 2 + dy,
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight => bounds.bottom() + dy,
        };
        (px, py)
    }
}

#[derive(Clone, Debug)]
pub struct PowerSplashLayer {
    pub anchor: SplashAnchor,
    pub x: SplashDim,
    pub y: SplashDim,
    pub w: SplashDim,
    pub h: SplashDim,
    pub radii: [i32; 4],
    pub surface: CssSurface,
}

#[derive(Clone, Debug)]
pub struct PowerSplashAnim {
    pub frames: Vec<Sprite>,
    pub width: i32,
    pub height: i32,
    pub fps: f32,
    pub anchor: SplashAnchor,
    pub x: SplashDim,
    pub y: SplashDim,
}

#[derive(Clone, Debug)]
pub struct PowerSplashSkin {
    pub bg_color: u32,
    pub layers: Vec<PowerSplashLayer>,
    pub animation: Option<PowerSplashAnim>,
    pub show_space_logo: bool,
    pub logo_anchor: SplashAnchor,
    pub logo_x: SplashDim,
    pub logo_y: SplashDim,
    pub logo_size: i32,
    pub title_anchor: SplashAnchor,
    pub title_x: SplashDim,
    pub title_y: SplashDim,
    pub title_size: f32,
    pub title_color: u32,
    pub title_shadow: Option<u32>,
    pub subtitle_anchor: SplashAnchor,
    pub subtitle_x: SplashDim,
    pub subtitle_y: SplashDim,
    pub subtitle_size: f32,
    pub subtitle_color: u32,
    pub poweroff_title: String,
    pub reboot_title: String,
    pub done_title: String,
    pub poweroff_subtitle: String,
    pub reboot_subtitle: String,
    pub done_subtitle: String,
    pub log_bg: u32,
    pub log_border: u32,
    pub log_text: u32,
    pub log_accent: u32,
    pub hint_color: u32,
}

/// Complete parsed declarative skin for an external theme package.
#[derive(Clone, Debug)]
pub struct CustomThemeSkin {
    pub cache_key: (String, ThemeMode, u32, u64),
    pub window: WindowSkin,
    pub titlebar: TitlebarSkin,
    pub caption_buttons: CaptionButtonsSkin,
    pub taskbar: TaskbarSkin,
    pub start_button: StartButtonSkin,
    pub start_menu: Option<StartMenuSkin>,
    pub controls: ControlsSkin,
    pub power_splash: Option<PowerSplashSkin>,
}

/// Copyable static reference wrapper around [`CustomThemeSkin`] so [`Theme`] stays `Copy + Eq`.
#[derive(Clone, Copy, Debug)]
pub struct SkinRef(pub &'static CustomThemeSkin);

impl PartialEq for SkinRef {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0) || self.0.cache_key == other.0.cache_key
    }
}

impl Eq for SkinRef {}

impl std::ops::Deref for SkinRef {
    type Target = CustomThemeSkin;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

#[derive(Clone, Debug)]
struct ExternalTheme {
    id: String,
    base_style: Option<ThemeStyle>,
    default_mode: Option<ThemeMode>,
    default_accent: Option<u32>,
    colors: toml::value::Table,
    skin: Option<SkinRef>,
}

fn theme_root_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("SPUI_THEME_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    dirs.push(PathBuf::from(SYSTEM_THEME_DIR));
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(USER_THEME_SUBDIR));
    }
    dirs
}

fn theme_dir_candidates(id: &str) -> Vec<PathBuf> {
    theme_root_dirs()
        .into_iter()
        .map(|dir| dir.join(id).join("theme.toml"))
        .collect()
}

pub fn valid_theme_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn theme_pack_available(id: &str) -> bool {
    let lower = id.trim().to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "vortexglass" | "spui" | "spui-4" | "spui4" | "win98" | "windows98"
    ) {
        return true;
    }
    load_external_theme(&lower, ThemeMode::Light, 0x3b82f6).is_some()
}

/// Reset a configuration that points at a removed external theme back to the
/// built-in default, preventing a half-removed theme from leaving a mixed UI.
pub fn fallback_missing_theme(config: &mut DesktopConfig) -> Option<String> {
    let missing = config.theme_id.clone();
    if theme_pack_available(&missing) {
        return None;
    }
    let default = DesktopConfig::default();
    config.theme_id = default.theme_id;
    config.theme_style = default.theme_style;
    config.mode = default.mode;
    config.accent = default.accent;
    config.wallpaper = default.wallpaper;
    config.panel_position = default.panel_position;
    config.panel_size = default.panel_size;
    config.panel_start_fullscreen = default.panel_start_fullscreen;
    config.wm_button_layout = default.wm_button_layout;
    Some(missing)
}

/// Discover all external theme packages installed under `/usr/share/spui/themes`
/// or `$HOME/.local/share/spui/themes`.
pub fn discover_installed_themes() -> Vec<InstalledThemeInfo> {
    let mut found: Vec<InstalledThemeInfo> = Vec::new();
    for root_dir in theme_root_dirs() {
        let Ok(entries) = fs::read_dir(&root_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !valid_theme_id(dir_name) {
                continue;
            }
            let toml_path = path.join("theme.toml");
            let Ok(text) = fs::read_to_string(&toml_path) else {
                continue;
            };
            let Ok(root_val) = toml::from_str::<toml::Value>(&text) else {
                continue;
            };
            let Some(table) = root_val.as_table() else {
                continue;
            };
            let id = table
                .get("id")
                .and_then(toml::Value::as_str)
                .unwrap_or(dir_name)
                .trim()
                .to_string();
            if !valid_theme_id(&id)
                || THEME_PRESETS.iter().any(|p| p.id().eq_ignore_ascii_case(&id))
                || found.iter().any(|item| item.id == id)
            {
                continue;
            }
            let name = table
                .get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or(&id)
                .to_string();
            let subtitle = table
                .get("subtitle")
                .and_then(toml::Value::as_str)
                .unwrap_or("已安装的 SPUI 外部主题软件包")
                .to_string();
            let base_style = table
                .get("base_style")
                .and_then(toml::Value::as_str)
                .map(ThemeStyle::parse)
                .unwrap_or(ThemeStyle::Spui);
            let mode = table
                .get("mode")
                .and_then(toml::Value::as_str)
                .map(ThemeMode::parse)
                .unwrap_or(ThemeMode::Light);
            let accent = table
                .get("accent")
                .and_then(parse_color)
                .unwrap_or(0x0054e3);
            let wallpaper = table
                .get("wallpaper")
                .and_then(toml::Value::as_str)
                .map(str::to_string);
            let solid_color = table.get("solid_color").and_then(parse_color).or_else(|| {
                table
                    .get("colors")
                    .and_then(toml::Value::as_table)
                    .and_then(|c| c.get("bg_top"))
                    .and_then(parse_color)
            });
            let panel_position = table
                .get("panel_position")
                .and_then(toml::Value::as_str)
                .map(PanelPosition::parse);
            let panel_size = table
                .get("panel_size")
                .and_then(toml::Value::as_integer)
                .map(|v| (v as i32).clamp(28, 60));
            let start_fullscreen = table
                .get("start_fullscreen")
                .and_then(toml::Value::as_bool);
            let button_layout = table
                .get("button_layout")
                .and_then(toml::Value::as_str)
                .map(ButtonLayout::parse);
            let preview = parse_preview_spec(table, accent);

            found.push(InstalledThemeInfo {
                id,
                name,
                subtitle,
                base_style,
                mode,
                accent,
                wallpaper,
                solid_color,
                panel_position,
                panel_size,
                start_fullscreen,
                button_layout,
                preview,
            });
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Discover all wallpapers in `/usr/share/spui/wallpapers`, starting with the 4 built-in
/// wallpapers and appending any additional `.jpg`/`.png` wallpapers installed by packages.
pub fn discover_wallpapers() -> Vec<DiscoveredWallpaper> {
    let mut list: Vec<DiscoveredWallpaper> = WALLPAPERS
        .iter()
        .map(|w| DiscoveredWallpaper {
            file: w.file.to_string(),
            label: w.label.to_string(),
        })
        .collect();
    let Ok(entries) = fs::read_dir(WALLPAPER_DIR) else {
        return list;
    };
    let mut extras: Vec<DiscoveredWallpaper> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if list.iter().any(|w| w.file == fname) || extras.iter().any(|w| w.file == fname) {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
            continue;
        }
        let label = match fname {
            "winxp-bliss.jpg" | "xp-bliss.jpg" => "蓝天绿茵 (XP Bliss)".to_string(),
            "aqua-wave.jpg" => "蔚蓝水波 (Aqua Wave)".to_string(),
            "ambiance-wave.jpg" => "暖橙茄紫 (Ambiance)".to_string(),
            other => other
                .rsplit_once('.')
                .map_or(other, |(stem, _)| stem)
                .replace(['-', '_'], " "),
        };
        extras.push(DiscoveredWallpaper {
            file: fname.to_string(),
            label,
        });
    }
    extras.sort_by(|a, b| a.file.cmp(&b.file));
    list.extend(extras);
    list
}

fn parse_preview_spec(table: &toml::value::Table, accent: u32) -> ThemePreviewSpec {
    let colors = table.get("colors").and_then(toml::Value::as_table);
    let preview = table.get("preview").and_then(toml::Value::as_table);
    let get_c = |key: &str, color_key: &str, fallback: u32| -> u32 {
        preview
            .and_then(|p| p.get(key))
            .and_then(parse_color)
            .or_else(|| colors.and_then(|c| c.get(color_key)).and_then(parse_color))
            .unwrap_or(fallback)
    };
    let titlebar_left = get_c("titlebar_left", "titlebar", accent);
    let titlebar_right = get_c("titlebar_right", "titlebar_hi", mix(titlebar_left, 0xffffff, 0.28));
    let window_top_radius = preview
        .and_then(|p| p.get("window_top_radius"))
        .and_then(toml::Value::as_integer)
        .or_else(|| {
            table
                .get("window")
                .and_then(toml::Value::as_table)
                .and_then(|w| w.get("top_radius"))
                .and_then(toml::Value::as_integer)
        })
        .unwrap_or(6) as i32;
    ThemePreviewSpec {
        desktop_top: get_c("desktop_top", "bg_top", 0x3a6ea5),
        desktop_bottom: get_c("desktop_bottom", "bg_bottom", 0x7ba7d7),
        window_bg: get_c("window_bg", "window", 0xece9d8),
        window_border: get_c("window_border", "window_border_active", accent),
        window_top_radius: window_top_radius.clamp(0, 10),
        titlebar_left,
        titlebar_right,
        close_btn: get_c("close_btn", "danger", 0xe0432e),
        min_btn: get_c("min_btn", "titlebar_hi", titlebar_right),
        max_btn: get_c("max_btn", "titlebar_hi", titlebar_right),
        taskbar_top: get_c("taskbar_top", "titlebar", 0x245edb),
        taskbar_bottom: get_c("taskbar_bottom", "window_border_active", 0x1941a5),
        start_btn: get_c("start_btn", "success", 0x3c9c24),
        tray_bg: get_c("tray_bg", "titlebar_hi", 0x0f8ee9),
    }
}

fn merge_toml_tables(base: &mut toml::value::Table, overlay: &toml::value::Table) {
    for (k, v) in overlay {
        if matches!(k.as_str(), "dark" | "accents") {
            continue;
        }
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(base_sub)), toml::Value::Table(over_sub)) => {
                merge_toml_tables(base_sub, over_sub);
            }
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

fn select_accent_variant<'a>(
    accents_table: &'a toml::value::Table,
    accent: u32,
) -> Option<&'a toml::value::Table> {
    let hex = format!("#{:06x}", accent & 0x00ff_ffff);
    if let Some(sub) = accents_table.get(&hex).and_then(toml::Value::as_table) {
        return Some(sub);
    }
    // Check named alias list inside each accent variant subtable (`matches = ["#10b981", "#f59e0b"]`).
    for (_, val) in accents_table {
        if let Some(sub) = val.as_table() {
            if let Some(arr) = sub.get("matches").and_then(toml::Value::as_array) {
                if arr
                    .iter()
                    .filter_map(parse_color)
                    .any(|c| c == (accent & 0x00ff_ffff))
                {
                    return Some(sub);
                }
            }
        }
    }
    None
}

#[inline]
fn fnv_u64(mut h: u64, val: u64) -> u64 {
    for b in val.to_le_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

#[inline]
fn fnv_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    fnv_u64(h, 0xff)
}

fn hash_file_meta(mut h: u64, path: &Path) -> u64 {
    if let Ok(m) = fs::metadata(path) {
        h = fnv_u64(h, m.len());
        if let Ok(t) = m.modified() {
            if let Ok(d) = t.duration_since(SystemTime::UNIX_EPOCH) {
                h = fnv_u64(h, d.as_secs());
                h = fnv_u64(h, u64::from(d.subsec_nanos()));
            }
        }
    }
    h
}

/// Content/metadata fingerprint over every asset in a theme pack directory so
/// upgrading `theme.toml` or any SVG sprite always invalidates `SKIN_CACHE`.
pub fn theme_dir_fingerprint(dir: &Path) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let Ok(rd) = fs::read_dir(dir) else {
        return 0;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    for name in names {
        h = fnv_bytes(h, name.as_bytes());
        h = hash_file_meta(h, &dir.join(&name));
    }
    h
}

/// Fast low-overhead directory/stamp fingerprint across all theme roots so
/// Shell, App windows, and Settings detect installs/upgrades/removals in <= 2s
/// without reading or parsing `theme.toml`.
pub fn installed_themes_fingerprint() -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let stamp_path = Path::new(THEME_RELOAD_STAMP);
    h = hash_file_meta(h, stamp_path);
    if let Ok(raw) = fs::read(stamp_path) {
        let len = raw.len().min(128);
        h = fnv_bytes(h, &raw[..len]);
    }
    for root in theme_root_dirs() {
        h = fnv_bytes(h, root.to_string_lossy().as_bytes());
        let Ok(rd) = fs::read_dir(&root) else {
            h = fnv_u64(h, 0);
            continue;
        };
        let mut dirs: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                if e.path().is_dir() {
                    e.file_name().to_str().map(str::to_string)
                } else {
                    None
                }
            })
            .collect();
        dirs.sort();
        for name in dirs {
            if !valid_theme_id(&name) {
                continue;
            }
            let sub = root.join(&name);
            let toml_p = sub.join("theme.toml");
            if toml_p.is_file() {
                h = fnv_bytes(h, name.as_bytes());
                h = hash_file_meta(h, &toml_p);
                // Also fold SVG filenames/meta lightly in case a theme upgrade replaced SVGs only.
                if let Ok(assets) = fs::read_dir(&sub) {
                    let mut svgs: Vec<String> = assets
                        .flatten()
                        .filter_map(|a| {
                            let s = a.file_name().to_string_lossy().to_string();
                            if s.ends_with(".svg") { Some(s) } else { None }
                        })
                        .collect();
                    svgs.sort();
                    for svg in svgs {
                        h = fnv_bytes(h, svg.as_bytes());
                        h = hash_file_meta(h, &sub.join(&svg));
                    }
                }
            }
        }
    }
    h
}

static SKIN_CACHE: OnceLock<Mutex<HashMap<(String, ThemeMode, u32, u64), &'static CustomThemeSkin>>> =
    OnceLock::new();

fn load_external_theme(id: &str, req_mode: ThemeMode, req_accent: u32) -> Option<ExternalTheme> {
    if !valid_theme_id(id) {
        return None;
    }
    let toml_path = theme_dir_candidates(id)
        .into_iter()
        .find(|path| path.is_file())?;
    let theme_dir = toml_path.parent()?.to_path_buf();
    let dir_fp = theme_dir_fingerprint(&theme_dir);
    let text = fs::read_to_string(&toml_path).ok()?;
    let root_val: toml::Value = toml::from_str(&text).ok()?;
    let mut table = root_val.as_table()?.clone();

    let base_style = table
        .get("base_style")
        .and_then(toml::Value::as_str)
        .map(ThemeStyle::parse);
    let default_mode = table
        .get("mode")
        .and_then(toml::Value::as_str)
        .map(ThemeMode::parse);
    let default_accent = table.get("accent").and_then(parse_color);

    // Apply accent color variant tables and dark mode overlay defined in `theme.toml`.
    if let Some(accents_tbl) = table.get("accents").and_then(toml::Value::as_table).cloned() {
        if let Some(acc_sub) = select_accent_variant(&accents_tbl, req_accent) {
            merge_toml_tables(&mut table, acc_sub);
        }
    }
    if req_mode == ThemeMode::Dark {
        if let Some(dark_tbl) = table.get("dark").and_then(toml::Value::as_table).cloned() {
            merge_toml_tables(&mut table, &dark_tbl);
        }
    }

    let colors = table
        .get("colors")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();

    let base_style_resolved = base_style.unwrap_or(ThemeStyle::Spui);
    let mode_resolved = if req_mode == ThemeMode::Dark {
        ThemeMode::Dark
    } else {
        default_mode.unwrap_or(req_mode)
    };
    let accent_resolved = if req_accent == uxconf::ACCENT_PRESETS[0].0 {
        default_accent.unwrap_or(req_accent)
    } else {
        req_accent
    };
    let mut base_palette = match (base_style_resolved, mode_resolved) {
        (ThemeStyle::VortexGlass, m) => Theme::vortexglass_with_accent(accent_resolved, m),
        (ThemeStyle::Win98, m) => Theme::win98_with_accent(accent_resolved, m),
        (ThemeStyle::Spui, ThemeMode::Dark) => Theme::dark_with_accent(accent_resolved),
        (ThemeStyle::Spui, ThemeMode::Light) => Theme::light_with_accent(accent_resolved),
    };
    apply_external_colors(&mut base_palette, &colors);

    let has_skin_sections = table.contains_key("window")
        || table.contains_key("titlebar")
        || table.contains_key("taskbar")
        || table.contains_key("controls")
        || table.contains_key("power_splash")
        || table.get("schema").and_then(toml::Value::as_integer).unwrap_or(1) >= 2;

    let skin = if has_skin_sections {
        let key = (id.to_string(), req_mode, req_accent & 0x00ff_ffff, dir_fp);
        let cache = SKIN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = cache.lock().ok()?;
        if let Some(&cached) = guard.get(&key) {
            Some(SkinRef(cached))
        } else {
            let parsed = parse_custom_skin(id, key.clone(), &table, &theme_dir, &base_palette);
            let leaked: &'static CustomThemeSkin = Box::leak(Box::new(parsed));
            guard.insert(key, leaked);
            Some(SkinRef(leaked))
        }
    } else {
        None
    };

    Some(ExternalTheme {
        id: id.to_string(),
        base_style,
        default_mode,
        default_accent,
        colors,
        skin,
    })
}

/// Parse a CSS color string (`#RRGGBB`, `#RGB`, `rgb(r,g,b)`, `rgba(r,g,b,a)`, or named literal).
pub fn parse_css_color_str(raw: &str) -> Option<(u32, f32)> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix('#') {
        let hex = hex.trim();
        if hex.len() == 6 {
            let rgb = u32::from_str_radix(hex, 16).ok()?;
            return Some((rgb, 1.0));
        } else if hex.len() == 3 {
            let r = u32::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u32::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u32::from_str_radix(&hex[2..3], 16).ok()?;
            let rgb = ((r * 17) << 16) | ((g * 17) << 8) | (b * 17);
            return Some((rgb, 1.0));
        } else if hex.len() == 8 {
            let rgba = u32::from_str_radix(hex, 16).ok()?;
            let rgb = (rgba >> 8) & 0x00ff_ffff;
            let a = (rgba & 0xff) as f32 / 255.0;
            return Some((rgb, a));
        }
    }
    let lower = s.to_ascii_lowercase();
    if let Some(inner) = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() >= 3 {
            let r = parts[0].parse::<f32>().ok()?.clamp(0.0, 255.0) as u32;
            let g = parts[1].parse::<f32>().ok()?.clamp(0.0, 255.0) as u32;
            let b = parts[2].parse::<f32>().ok()?.clamp(0.0, 255.0) as u32;
            let a = if parts.len() >= 4 {
                parts[3].parse::<f32>().ok()?.clamp(0.0, 1.0)
            } else {
                1.0
            };
            return Some(((r << 16) | (g << 8) | b, a));
        }
    }
    match lower.as_str() {
        "white" => Some((0xffffff, 1.0)),
        "black" => Some((0x000000, 1.0)),
        "silver" => Some((0xc0c0c0, 1.0)),
        "gray" | "grey" => Some((0x808080, 1.0)),
        "transparent" => Some((0x000000, 0.0)),
        _ => None,
    }
}

fn parse_color(value: &toml::Value) -> Option<u32> {
    parse_css_color_str(value.as_str()?).map(|(c, _)| c)
}

fn split_top_level_commas(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (idx, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                out.push(input[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    let tail = input[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn parse_gradient_stop_tokens(tokens: &[&str]) -> Vec<(f32, u32)> {
    let mut raw_stops: Vec<(Option<f32>, u32)> = Vec::new();
    for &tok in tokens {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        // A token may be `<color>`, `<color> <pct>%`, or `<color> <pct1>% <pct2>%`.
        // Note that `rgb(r, g, b)` may contain spaces inside parentheses.
        let color_end = if let Some(close_idx) = tok.find(')') {
            close_idx + 1
        } else {
            tok.find(char::is_whitespace).unwrap_or(tok.len())
        };
        let color_part = &tok[..color_end];
        let Some((rgb, _)) = parse_css_color_str(color_part) else {
            continue;
        };
        let rest = tok[color_end..].trim();
        if rest.is_empty() {
            raw_stops.push((None, rgb));
        } else {
            let mut pushed = false;
            for pos_tok in rest.split_whitespace() {
                if let Some(pct_str) = pos_tok.strip_suffix('%') {
                    if let Ok(pct) = pct_str.parse::<f32>() {
                        raw_stops.push((Some((pct / 100.0).clamp(0.0, 1.0)), rgb));
                        pushed = true;
                    }
                } else if let Ok(val) = pos_tok.trim_end_matches("px").parse::<f32>() {
                    raw_stops.push((Some(val.clamp(0.0, 1.0)), rgb));
                    pushed = true;
                }
            }
            if !pushed {
                raw_stops.push((None, rgb));
            }
        }
    }
    if raw_stops.is_empty() {
        return Vec::new();
    }
    if raw_stops[0].0.is_none() {
        raw_stops[0].0 = Some(0.0);
    }
    let last_idx = raw_stops.len() - 1;
    if raw_stops[last_idx].0.is_none() {
        raw_stops[last_idx].0 = Some(1.0);
    }
    // Fill any interior `None` positions linearly between known neighbors.
    let mut i = 0;
    while i < raw_stops.len() {
        if raw_stops[i].0.is_none() {
            let prev_pos = raw_stops[i.saturating_sub(1)].0.unwrap_or(0.0);
            let mut j = i + 1;
            while j < raw_stops.len() && raw_stops[j].0.is_none() {
                j += 1;
            }
            let next_pos = raw_stops
                .get(j)
                .and_then(|s| s.0)
                .unwrap_or(1.0)
                .max(prev_pos);
            let span = (j - i + 1) as f32;
            for k in i..j {
                let frac = (k - i + 1) as f32 / span;
                raw_stops[k].0 = Some(prev_pos + (next_pos - prev_pos) * frac);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    raw_stops
        .into_iter()
        .map(|(p, c)| (p.unwrap_or(0.0), c))
        .collect()
}

/// Parse a CSS `background` / `background-image` string (`#hex`, `linear-gradient(...)`, `radial-gradient(...)`).
pub fn parse_css_background(raw: &str) -> Option<CssBackground> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if let Some(inner) = lower
        .strip_prefix("linear-gradient(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts = split_top_level_commas(inner);
        if parts.is_empty() {
            return None;
        }
        let first = parts[0].trim();
        let (dir, stop_slice) = if first == "to bottom" || first == "180deg" {
            (0, &parts[1..])
        } else if first == "to right" || first == "90deg" {
            (1, &parts[1..])
        } else if first == "135deg" || first == "to bottom right" {
            (2, &parts[1..])
        } else if first.ends_with("deg") || first.starts_with("to ") {
            (0, &parts[1..])
        } else {
            (0, &parts[..])
        };
        let stops = parse_gradient_stop_tokens(stop_slice);
        if stops.is_empty() {
            return None;
        }
        return Some(match dir {
            1 => CssBackground::linear_h(stops, 1.0),
            2 => CssBackground::linear_diag(stops, 1.0),
            _ => CssBackground::linear_v(stops, 1.0),
        });
    }
    if let Some(inner) = lower
        .strip_prefix("radial-gradient(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let parts = split_top_level_commas(inner);
        if parts.is_empty() {
            return None;
        }
        let first = parts[0].trim();
        let mut cx_pct = 0.5f32;
        let mut cy_pct = 0.5f32;
        let stop_slice = if let Some(at_pos) = first.find("at ") {
            let coords = &first[at_pos + 3..];
            let mut nums = coords
                .split_whitespace()
                .filter_map(|t| t.strip_suffix('%').and_then(|n| n.parse::<f32>().ok()));
            if let Some(cx) = nums.next() {
                cx_pct = (cx / 100.0).clamp(0.0, 1.0);
            }
            if let Some(cy) = nums.next() {
                cy_pct = (cy / 100.0).clamp(0.0, 1.0);
            }
            &parts[1..]
        } else if first.starts_with("circle") || first.starts_with("ellipse") {
            &parts[1..]
        } else {
            &parts[..]
        };
        let stops = parse_gradient_stop_tokens(stop_slice);
        if stops.is_empty() {
            return None;
        }
        return Some(CssBackground::radial(cx_pct, cy_pct, stops, 1.0));
    }
    parse_css_color_str(s).map(|(c, a)| CssBackground::Solid(c, a))
}

/// Parse a CSS `box-shadow` string into [`CssInsetShadow`] layers.
pub fn parse_css_box_shadow(raw: &str) -> Vec<CssInsetShadow> {
    let s = raw.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("unset") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for item in split_top_level_commas(s) {
        let item = item.trim();
        if !item.contains("inset") {
            continue;
        }
        // Tokenize while keeping `rgb(...)` / `rgba(...)` intact as a single token.
        let mut tokens: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut paren = 0i32;
        for ch in item.chars() {
            match ch {
                '(' => {
                    paren += 1;
                    cur.push(ch);
                }
                ')' => {
                    paren = (paren - 1).max(0);
                    cur.push(ch);
                }
                c if c.is_whitespace() && paren == 0 => {
                    if !cur.is_empty() {
                        tokens.push(std::mem::take(&mut cur));
                    }
                }
                other => cur.push(other),
            }
        }
        if !cur.is_empty() {
            tokens.push(cur);
        }

        let mut nums: Vec<i32> = Vec::new();
        let mut color: Option<(u32, f32)> = None;
        for tok in &tokens {
            if tok.eq_ignore_ascii_case("inset") {
                continue;
            }
            let clean = tok.trim_end_matches("!important");
            if let Some(c) = parse_css_color_str(clean) {
                color = Some(c);
            } else if let Ok(n) = clean.trim_end_matches("px").parse::<i32>() {
                nums.push(n);
            }
        }
        if let Some((rgb, alpha)) = color {
            if nums.len() >= 2 {
                let dx = nums[0];
                let dy = nums[1];
                let blur = nums.get(2).copied().unwrap_or(0);
                let spread = nums.get(3).copied().unwrap_or(0);
                out.push(CssInsetShadow {
                    dx,
                    dy,
                    blur,
                    spread,
                    color: rgb,
                    alpha,
                });
            }
        }
    }
    out
}

fn parse_radii_val(val: &toml::Value) -> Option<[i32; 4]> {
    if let Some(n) = val.as_integer() {
        let r = (n as i32).max(0);
        return Some([r, r, r, r]);
    }
    if let Some(arr) = val.as_array() {
        if arr.len() == 4 {
            return Some([
                arr[0].as_integer().unwrap_or(0) as i32,
                arr[1].as_integer().unwrap_or(0) as i32,
                arr[2].as_integer().unwrap_or(0) as i32,
                arr[3].as_integer().unwrap_or(0) as i32,
            ]);
        } else if arr.len() == 2 {
            let t = arr[0].as_integer().unwrap_or(0) as i32;
            let b = arr[1].as_integer().unwrap_or(0) as i32;
            return Some([t, t, b, b]);
        }
    }
    None
}

fn parse_border_spec(val: &toml::Value) -> Option<(u32, f32, i32)> {
    let s = val.as_str()?.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return None;
    }
    // Supports either `"#003c74"` or `"1px solid #003c74"`
    if let Some((c, a)) = parse_css_color_str(s) {
        return Some((c, a, 1));
    }
    let mut width = 1i32;
    let mut color = None;
    for tok in s.split_whitespace() {
        if let Some(px) = tok.strip_suffix("px") {
            if let Ok(w) = px.parse::<i32>() {
                width = w.max(0);
            }
        } else if !tok.eq_ignore_ascii_case("solid") {
            if let Some(c) = parse_css_color_str(tok) {
                color = Some(c);
            }
        }
    }
    color.map(|(c, a)| (c, a, width))
}

fn parse_surface(
    tbl: Option<&toml::value::Table>,
    prefix: &str,
    theme_dir: &Path,
    target_size: Option<(u32, u32)>,
) -> CssSurface {
    let Some(tbl) = tbl else {
        return CssSurface::default();
    };
    let get_key = |suffix: &str| -> Option<&toml::Value> {
        if prefix.is_empty() {
            tbl.get(suffix)
        } else {
            let full = format!("{prefix}_{suffix}");
            tbl.get(&full).or_else(|| {
                if suffix == "bg" {
                    tbl.get(prefix)
                } else {
                    None
                }
            })
        }
    };
    let bg = get_key("bg")
        .or_else(|| get_key("background"))
        .and_then(toml::Value::as_str)
        .and_then(parse_css_background);
    let box_shadows = get_key("box_shadow")
        .or_else(|| get_key("shadow"))
        .and_then(toml::Value::as_str)
        .map(parse_css_box_shadow)
        .unwrap_or_default();
    let border = get_key("border").and_then(parse_border_spec);
    let border_top = get_key("border_top")
        .and_then(parse_border_spec)
        .map(|(c, _, w)| (c, w));
    let border_right = get_key("border_right")
        .and_then(parse_border_spec)
        .map(|(c, _, w)| (c, w));
    let border_bottom = get_key("border_bottom")
        .and_then(parse_border_spec)
        .map(|(c, _, w)| (c, w));
    let border_left = get_key("border_left")
        .and_then(parse_border_spec)
        .map(|(c, _, w)| (c, w));
    let radii = get_key("radius").and_then(parse_radii_val);
    let sprite = get_key("svg")
        .or_else(|| get_key("image"))
        .and_then(toml::Value::as_str)
        .and_then(|rel| {
            if rel.is_empty() || rel.contains("..") || rel.starts_with('/') {
                None
            } else {
                icons::decode_theme_asset(&theme_dir.join(rel), target_size)
            }
        });
    let mut out = CssSurface {
        cache_id: 0,
        bg,
        box_shadows,
        border,
        border_top,
        border_right,
        border_bottom,
        border_left,
        radii,
        sprite,
    };
    out.assign_cache_id();
    out
}

fn parse_splash_dim(val: &toml::Value) -> Option<SplashDim> {
    if let Some(n) = val.as_integer() {
        return Some(SplashDim::Px(n as i32));
    }
    if let Some(f) = val.as_float() {
        return Some(SplashDim::Px(f.round() as i32));
    }
    let s = val.as_str()?.trim();
    if let Some(pct) = s.strip_suffix('%') {
        let p = pct.trim().parse::<f32>().ok()?;
        return Some(SplashDim::Pct(p / 100.0));
    }
    let px_str = s.strip_suffix("px").unwrap_or(s).trim();
    px_str.parse::<i32>().ok().map(SplashDim::Px)
}

fn parse_splash_dim_key(
    tbl: Option<&toml::value::Table>,
    key: &str,
    default: SplashDim,
) -> SplashDim {
    tbl.and_then(|t| t.get(key))
        .and_then(parse_splash_dim)
        .unwrap_or(default)
}

fn parse_splash_anchor_key(
    tbl: Option<&toml::value::Table>,
    key: &str,
    default: SplashAnchor,
) -> SplashAnchor {
    tbl.and_then(|t| t.get(key))
        .and_then(toml::Value::as_str)
        .map(SplashAnchor::parse)
        .unwrap_or(default)
}

fn tint_surface(src: &CssSurface, target: u32, weight: f32) -> CssSurface {
    let mut out = src.clone();
    out.bg = out.bg.map(|b| b.tinted(target, weight));
    out.assign_cache_id();
    out
}

fn slice_sprite_horizontal(sheet: &Sprite, frame_w: u32, frame_h: u32, count: usize) -> Vec<Sprite> {
    let fw = frame_w.max(1) as usize;
    let fh = frame_h.max(1) as usize;
    let sw = sheet.width as usize;
    let sh = sheet.height as usize;
    let rows = fh.min(sh);
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        let x0 = i * fw;
        if x0 + fw > sw {
            break;
        }
        let mut pixels = Vec::with_capacity(fw * rows);
        for row in 0..rows {
            let start = row * sw + x0;
            pixels.extend_from_slice(&sheet.pixels[start..start + fw]);
        }
        frames.push(Sprite {
            width: fw as u32,
            height: rows as u32,
            pixels,
        });
    }
    frames
}

fn parse_power_splash(
    root: &toml::value::Table,
    theme_dir: &Path,
    pal: &Theme,
) -> Option<PowerSplashSkin> {
    let sp_tbl = root.get("power_splash")?.as_table()?;
    let bg_color = sp_tbl
        .get("bg_color")
        .and_then(parse_color)
        .unwrap_or(pal.bg_top);

    let mut layers = Vec::new();
    if let Some(arr) = sp_tbl.get("layers").and_then(toml::Value::as_array) {
        for item in arr {
            let Some(ltbl) = item.as_table() else {
                continue;
            };
            let surface = parse_surface(Some(ltbl), "", theme_dir, None);
            if surface.is_empty() {
                continue;
            }
            let radii = ltbl
                .get("radius")
                .and_then(parse_radii_val)
                .unwrap_or([0; 4]);
            layers.push(PowerSplashLayer {
                anchor: parse_splash_anchor_key(Some(ltbl), "anchor", SplashAnchor::TopLeft),
                x: parse_splash_dim_key(Some(ltbl), "x", SplashDim::Px(0)),
                y: parse_splash_dim_key(Some(ltbl), "y", SplashDim::Px(0)),
                w: parse_splash_dim_key(Some(ltbl), "w", SplashDim::Pct(1.0)),
                h: parse_splash_dim_key(Some(ltbl), "h", SplashDim::Pct(1.0)),
                radii,
                surface,
            });
        }
    }

    let animation = sp_tbl
        .get("animation")
        .and_then(toml::Value::as_table)
        .and_then(|atbl| {
            let width = atbl
                .get("width")
                .and_then(toml::Value::as_integer)
                .unwrap_or(48)
                .clamp(8, 640) as i32;
            let height = atbl
                .get("height")
                .and_then(toml::Value::as_integer)
                .unwrap_or(48)
                .clamp(8, 320) as i32;
            let fps = atbl
                .get("fps")
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                .unwrap_or(12.0)
                .clamp(1.0, 60.0) as f32;
            let mut frames = Vec::new();
            if let Some(sheet_rel) = atbl
                .get("sprite_sheet")
                .or_else(|| atbl.get("sheet"))
                .and_then(toml::Value::as_str)
            {
                let count = atbl
                    .get("frame_count")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(1)
                    .clamp(1, 64) as usize;
                if !sheet_rel.is_empty()
                    && !sheet_rel.contains("..")
                    && !sheet_rel.starts_with('/')
                {
                    if let Some(sheet) = icons::decode_theme_asset(
                        &theme_dir.join(sheet_rel),
                        Some(((width * count as i32) as u32, height as u32)),
                    ) {
                        frames = slice_sprite_horizontal(&sheet, width as u32, height as u32, count);
                    }
                }
            } else if let Some(list) = atbl.get("frames").and_then(toml::Value::as_array) {
                for item in list {
                    let Some(rel) = item.as_str() else {
                        continue;
                    };
                    if !rel.is_empty() && !rel.contains("..") && !rel.starts_with('/') {
                        if let Some(sp) = icons::decode_theme_asset(
                            &theme_dir.join(rel),
                            Some((width as u32, height as u32)),
                        ) {
                            frames.push(sp);
                        }
                    }
                }
            }
            if frames.is_empty() {
                None
            } else {
                Some(PowerSplashAnim {
                    frames,
                    width,
                    height,
                    fps,
                    anchor: parse_splash_anchor_key(Some(atbl), "anchor", SplashAnchor::Center),
                    x: parse_splash_dim_key(Some(atbl), "x", SplashDim::Px(0)),
                    y: parse_splash_dim_key(Some(atbl), "y", SplashDim::Px(60)),
                })
            }
        });

    let opt = Some(sp_tbl);
    Some(PowerSplashSkin {
        bg_color,
        layers,
        animation,
        show_space_logo: sp_tbl
            .get("show_space_logo")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true),
        logo_anchor: parse_splash_anchor_key(opt, "logo_anchor", SplashAnchor::Center),
        logo_x: parse_splash_dim_key(opt, "logo_x", SplashDim::Px(0)),
        logo_y: parse_splash_dim_key(opt, "logo_y", SplashDim::Px(-64)),
        logo_size: sp_tbl
            .get("logo_size")
            .and_then(toml::Value::as_integer)
            .unwrap_or(68)
            .clamp(24, 160) as i32,
        title_anchor: parse_splash_anchor_key(opt, "title_anchor", SplashAnchor::Center),
        title_x: parse_splash_dim_key(opt, "title_x", SplashDim::Px(0)),
        title_y: parse_splash_dim_key(opt, "title_y", SplashDim::Px(12)),
        title_size: sp_tbl
            .get("title_size")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(22.0) as f32,
        title_color: sp_tbl
            .get("title_color")
            .and_then(parse_color)
            .unwrap_or(pal.titlebar_text),
        title_shadow: sp_tbl.get("title_shadow").and_then(parse_color),
        subtitle_anchor: parse_splash_anchor_key(opt, "subtitle_anchor", SplashAnchor::Center),
        subtitle_x: parse_splash_dim_key(opt, "subtitle_x", SplashDim::Px(0)),
        subtitle_y: parse_splash_dim_key(opt, "subtitle_y", SplashDim::Px(96)),
        subtitle_size: sp_tbl
            .get("subtitle_size")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(13.0) as f32,
        subtitle_color: sp_tbl
            .get("subtitle_color")
            .and_then(parse_color)
            .unwrap_or(pal.text_dim),
        poweroff_title: sp_tbl
            .get("poweroff_title")
            .and_then(toml::Value::as_str)
            .unwrap_or("正在关闭计算机")
            .to_string(),
        reboot_title: sp_tbl
            .get("reboot_title")
            .and_then(toml::Value::as_str)
            .unwrap_or("正在重新启动")
            .to_string(),
        done_title: sp_tbl
            .get("done_title")
            .and_then(toml::Value::as_str)
            .unwrap_or("即将完成，请稍候")
            .to_string(),
        poweroff_subtitle: sp_tbl
            .get("poweroff_subtitle")
            .and_then(toml::Value::as_str)
            .unwrap_or("正在保存设置并停止系统服务...")
            .to_string(),
        reboot_subtitle: sp_tbl
            .get("reboot_subtitle")
            .and_then(toml::Value::as_str)
            .unwrap_or("正在同步文件系统并准备重启...")
            .to_string(),
        done_subtitle: sp_tbl
            .get("done_subtitle")
            .and_then(toml::Value::as_str)
            .unwrap_or("正在移交控制权至系统内核...")
            .to_string(),
        log_bg: sp_tbl
            .get("log_bg")
            .and_then(parse_color)
            .unwrap_or(0x0b101b),
        log_border: sp_tbl
            .get("log_border")
            .and_then(parse_color)
            .unwrap_or(pal.accent),
        log_text: sp_tbl
            .get("log_text")
            .and_then(parse_color)
            .unwrap_or(0xe2e8f0),
        log_accent: sp_tbl
            .get("log_accent")
            .and_then(parse_color)
            .unwrap_or(pal.accent),
        hint_color: sp_tbl
            .get("hint_color")
            .and_then(parse_color)
            .unwrap_or(pal.text_muted),
    })
}

fn parse_custom_skin(
    _id: &str,
    cache_key: (String, ThemeMode, u32, u64),
    root: &toml::value::Table,
    theme_dir: &Path,
    pal: &Theme,
) -> CustomThemeSkin {
    let win_tbl = root.get("window").and_then(toml::Value::as_table);
    let top_radius = win_tbl
        .and_then(|t| t.get("top_radius"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(8) as i32;
    let bottom_radius = win_tbl
        .and_then(|t| t.get("bottom_radius"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(0) as i32;
    let border_width = win_tbl
        .and_then(|t| t.get("border_width"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(3) as i32;
    let mut active_frame = parse_surface(win_tbl, "active", theme_dir, None);
    if active_frame.is_empty() {
        active_frame.bg = Some(CssBackground::Solid(0x0831d9, 1.0));
        active_frame.box_shadows = parse_css_box_shadow(
            "inset -1px -1px #00138c, inset 1px 1px #0831d9, inset -2px -2px #001ea0, inset 2px 2px #166aee, inset -3px -3px #003bda, inset 3px 3px #0855dd",
        );
    }
    let mut inactive_frame = parse_surface(win_tbl, "inactive", theme_dir, None);
    if inactive_frame.is_empty() {
        inactive_frame.bg = Some(CssBackground::Solid(0x6582f5, 1.0));
        inactive_frame.box_shadows = parse_css_box_shadow(
            "inset -1px -1px #3953b8, inset 1px 1px #7697e7, inset -2px -2px #4c68c8, inset 2px 2px #8aa8f0, inset -3px -3px #5c79d8, inset 3px 3px #7a96df",
        );
    }
    let window = WindowSkin {
        top_radius: top_radius.clamp(0, 16),
        bottom_radius: bottom_radius.clamp(0, 16),
        border_width: border_width.clamp(1, 8),
        active_frame,
        inactive_frame,
    };

    let tb_tbl = root.get("titlebar").and_then(toml::Value::as_table);
    let tb_height = tb_tbl
        .and_then(|t| t.get("height"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(28) as i32;
    let title_align = match tb_tbl
        .and_then(|t| t.get("title_align"))
        .and_then(toml::Value::as_str)
        .unwrap_or("left")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "center" => TitleAlign::Center,
        _ => TitleAlign::Left,
    };
    let mut tb_active = parse_surface(tb_tbl, "active", theme_dir, None);
    if tb_active.is_empty() {
        tb_active.bg = parse_css_background(
            "linear-gradient(to bottom, #0058ee 0%, #3593ff 4%, #288eff 6%, #127dff 8%, #036ffc 10%, #0262ee 14%, #0057e5 20%, #0054e3 24%, #0055eb 56%, #005bf5 66%, #026afe 76%, #0062ef 86%, #0052d6 92%, #0040ab 94%, #003092 100%)",
        );
    }
    let mut tb_inactive = parse_surface(tb_tbl, "inactive", theme_dir, None);
    if tb_inactive.is_empty() {
        tb_inactive.bg = parse_css_background(
            "linear-gradient(to bottom, #7697e7 0%, #7e9ee3 3%, #94afe8 6%, #97b4e9 8%, #82a5e4 14%, #7c9fe2 17%, #7996de 25%, #7b99e1 56%, #82a9e9 81%, #80a5e7 89%, #7b96e1 94%, #7a93df 97%, #abbae3 100%)",
        );
    }
    let glow_w = tb_tbl
        .and_then(|t| t.get("edge_glow_width"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(15) as i32;
    let edge_glow_left = tb_tbl
        .and_then(|t| t.get("edge_glow_left"))
        .and_then(parse_color)
        .map(|c| (c, glow_w));
    let edge_glow_right = tb_tbl
        .and_then(|t| t.get("edge_glow_right"))
        .and_then(parse_color)
        .map(|c| (c, glow_w));
    let titlebar = TitlebarSkin {
        height: tb_height.clamp(22, 44),
        title_align,
        active: tb_active,
        inactive: tb_inactive,
        edge_glow_left,
        edge_glow_right,
        text_color: tb_tbl
            .and_then(|t| t.get("text_color"))
            .and_then(parse_color)
            .unwrap_or(0xffffff),
        inactive_text_color: tb_tbl
            .and_then(|t| t.get("inactive_text_color"))
            .and_then(parse_color)
            .unwrap_or(0xd8e4f8),
        text_shadow: tb_tbl
            .and_then(|t| t.get("text_shadow"))
            .and_then(parse_color),
    };

    let cb_tbl = root.get("caption_buttons").and_then(toml::Value::as_table);
    let cb_w = cb_tbl
        .and_then(|t| t.get("width"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(21) as i32;
    let cb_h = cb_tbl
        .and_then(|t| t.get("height"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(21) as i32;
    let cb_sz = Some((cb_w.max(12) as u32, cb_h.max(12) as u32));
    let default_blue_btn = || {
        let mut s = CssSurface::default();
        s.bg = parse_css_background(
            "radial-gradient(circle at 90% 90%, #0054e9 0%, #2263d5 55%, #4479e4 70%, #a3bbec 90%, #ffffff 100%)",
        );
        s.box_shadows = parse_css_box_shadow("inset 0 -1px 2px 1px #1d3fa8");
        s.border = Some((0xffffff, 0.92, 1));
        s
    };
    let default_close_btn = || {
        let mut s = CssSurface::default();
        s.bg = parse_css_background(
            "radial-gradient(circle at 90% 90%, #cc4600 0%, #dc6527 55%, #cd7546 70%, #ffccb2 90%, #ffffff 100%)",
        );
        s.box_shadows = parse_css_box_shadow("inset 0 -1px 2px 1px #9c2400");
        s.border = Some((0xffffff, 0.92, 1));
        s
    };
    let mut close_normal = parse_surface(cb_tbl, "close", theme_dir, cb_sz);
    if close_normal.is_empty() {
        close_normal = default_close_btn();
    }
    let mut close_hover = parse_surface(cb_tbl, "close_hover", theme_dir, cb_sz);
    if close_hover.is_empty() {
        close_hover = close_normal.clone();
    }
    let mut close_active = parse_surface(cb_tbl, "close_active", theme_dir, cb_sz);
    if close_active.is_empty() {
        close_active = close_normal.clone();
    }
    let mut min_normal = parse_surface(cb_tbl, "min", theme_dir, cb_sz);
    if min_normal.is_empty() {
        min_normal = default_blue_btn();
    }
    let mut min_hover = parse_surface(cb_tbl, "min_hover", theme_dir, cb_sz);
    if min_hover.is_empty() {
        min_hover = min_normal.clone();
    }
    let mut min_active = parse_surface(cb_tbl, "min_active", theme_dir, cb_sz);
    if min_active.is_empty() {
        min_active = min_normal.clone();
    }
    let mut max_normal = parse_surface(cb_tbl, "max", theme_dir, cb_sz);
    if max_normal.is_empty() {
        max_normal = min_normal.clone();
    }
    let mut max_hover = parse_surface(cb_tbl, "max_hover", theme_dir, cb_sz);
    if max_hover.is_empty() {
        max_hover = max_normal.clone();
    }
    let mut max_active = parse_surface(cb_tbl, "max_active", theme_dir, cb_sz);
    if max_active.is_empty() {
        max_active = max_normal.clone();
    }
    let mut restore_normal = parse_surface(cb_tbl, "restore", theme_dir, cb_sz);
    if restore_normal.is_empty() {
        restore_normal = max_normal.clone();
    }
    let mut restore_hover = parse_surface(cb_tbl, "restore_hover", theme_dir, cb_sz);
    if restore_hover.is_empty() {
        restore_hover = max_hover.clone();
    }
    let mut restore_active = parse_surface(cb_tbl, "restore_active", theme_dir, cb_sz);
    if restore_active.is_empty() {
        restore_active = max_active.clone();
    }
    let caption_buttons = CaptionButtonsSkin {
        width: cb_w.clamp(14, 36),
        height: cb_h.clamp(14, 32),
        gap: cb_tbl
            .and_then(|t| t.get("gap"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(2) as i32,
        margin_right: cb_tbl
            .and_then(|t| t.get("margin_right"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(5) as i32,
        margin_top: cb_tbl
            .and_then(|t| t.get("margin_top"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(4) as i32,
        radius: cb_tbl
            .and_then(|t| t.get("radius"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(3) as i32,
        close_normal,
        close_hover,
        close_active,
        min_normal,
        min_hover,
        min_active,
        max_normal,
        max_hover,
        max_active,
        restore_normal,
        restore_hover,
        restore_active,
        glyph_color: cb_tbl
            .and_then(|t| t.get("glyph_color"))
            .and_then(parse_color)
            .unwrap_or(0xffffff),
        inactive_opacity: cb_tbl
            .and_then(|t| t.get("inactive_opacity"))
            .and_then(toml::Value::as_float)
            .unwrap_or(0.68) as f32,
    };

    let task_tbl = root.get("taskbar").and_then(toml::Value::as_table);
    let mut task_bg = parse_surface(task_tbl, "", theme_dir, None);
    if task_bg.is_empty() {
        task_bg.bg = parse_css_background(
            "linear-gradient(to bottom, #1f2f86 0%, #3165c4 3%, #3682e5 6%, #4490e6 10%, #3883e5 12%, #2b71e0 15%, #2663da 18%, #235bd6 20%, #2258d5 23%, #2157d6 38%, #245ddb 54%, #2562df 86%, #245fdc 89%, #2158d4 92%, #1d4ec0 95%, #1941a5 98%)",
        );
    }
    let mut btn_norm = parse_surface(task_tbl, "button_normal", theme_dir, None);
    if btn_norm.is_empty() {
        btn_norm.bg = parse_css_background("linear-gradient(to bottom, #4b92f7 0%, #3c81f3 45%, #2b6ee3 100%)");
        btn_norm.box_shadows = parse_css_box_shadow("inset -1px -1px #1746a2, inset 1px 1px #7db4ff");
    }
    let mut btn_hov = parse_surface(task_tbl, "button_hover", theme_dir, None);
    if btn_hov.is_empty() {
        btn_hov.bg = parse_css_background("linear-gradient(to bottom, #68abff 0%, #53a3ff 50%, #3783eb 100%)");
        btn_hov.box_shadows = parse_css_box_shadow("inset -1px -1px #1a50b8, inset 1px 1px #9ec9ff");
    }
    let mut btn_act = parse_surface(task_tbl, "button_active", theme_dir, None);
    if btn_act.is_empty() {
        btn_act.bg = parse_css_background("linear-gradient(to bottom, #173f94 0%, #1e52b7 55%, #1b4aa8 100%)");
        btn_act.box_shadows = parse_css_box_shadow("inset 1px 1px #0d2358, inset -1px -1px #2e67cf, inset 2px 2px #13337c");
    }
    let mut btn_press = parse_surface(task_tbl, "button_pressed", theme_dir, None);
    if btn_press.is_empty() {
        btn_press = btn_act.clone();
    }
    let tray_surface = parse_surface(task_tbl, "tray", theme_dir, None);
    let tray_bg = if tray_surface.is_empty() {
        None
    } else {
        Some(tray_surface)
    };
    let taskbar = TaskbarSkin {
        height: task_tbl
            .and_then(|t| t.get("height"))
            .and_then(toml::Value::as_integer)
            .map(|v| (v as i32).clamp(28, 56)),
        bg: task_bg,
        button_normal: btn_norm,
        button_hover: btn_hov,
        button_active: btn_act,
        button_pressed: btn_press,
        button_radius: task_tbl
            .and_then(|t| t.get("button_radius"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(3) as i32,
        button_text: task_tbl
            .and_then(|t| t.get("button_text"))
            .and_then(parse_color)
            .unwrap_or(0xffffff),
        button_active_text: task_tbl
            .and_then(|t| t.get("button_active_text"))
            .and_then(parse_color)
            .unwrap_or(0xffffff),
        button_minimized_text: task_tbl
            .and_then(|t| t.get("button_minimized_text"))
            .and_then(parse_color)
            .unwrap_or(0xd6e6ff),
        tray_bg,
        tray_text: task_tbl
            .and_then(|t| t.get("tray_text"))
            .and_then(parse_color)
            .unwrap_or(0xffffff),
        tray_muted_text: task_tbl
            .and_then(|t| t.get("tray_muted_text"))
            .and_then(parse_color)
            .unwrap_or(0xd8eeff),
    };

    let sb_tbl = root.get("start_button").and_then(toml::Value::as_table);
    let sb_radii = sb_tbl
        .and_then(|t| t.get("radius"))
        .and_then(parse_radii_val)
        .unwrap_or([0, 13, 13, 0]);
    let mut sb_norm = parse_surface(sb_tbl, "normal", theme_dir, None);
    if sb_norm.is_empty() {
        sb_norm.bg = parse_css_background(
            "linear-gradient(to bottom, #74c56d 0%, #43a13d 12%, #379231 48%, #2e8428 54%, #257820 88%, #185814 100%)",
        );
        sb_norm.box_shadows =
            parse_css_box_shadow("inset 0 1px #9ce096, inset -1px -1px #175214");
    }
    let mut sb_hov = parse_surface(sb_tbl, "hover", theme_dir, None);
    if sb_hov.is_empty() {
        sb_hov.bg = parse_css_background(
            "linear-gradient(to bottom, #8be084 0%, #52b44b 12%, #43a23d 48%, #379531 54%, #2d8727 88%, #1e681a 100%)",
        );
        sb_hov.box_shadows =
            parse_css_box_shadow("inset 0 1px #b8f2b2, inset -1px -1px #1d6419");
    }
    let mut sb_act = parse_surface(sb_tbl, "active", theme_dir, None);
    if sb_act.is_empty() {
        sb_act.bg = parse_css_background(
            "linear-gradient(to bottom, #1e5e1a 0%, #267021 20%, #2c7c26 80%, #378e31 100%)",
        );
        sb_act.box_shadows =
            parse_css_box_shadow("inset 1px 1px #113c0e, inset -1px -1px #429e3b");
    }
    let sb_icon = sb_tbl
        .and_then(|t| t.get("icon_svg").or_else(|| t.get("icon")))
        .and_then(toml::Value::as_str)
        .and_then(|rel| {
            if rel.is_empty() || rel.contains("..") || rel.starts_with('/') {
                None
            } else {
                icons::decode_theme_asset(&theme_dir.join(rel), Some((20, 20)))
            }
        });
    let start_button = StartButtonSkin {
        width: sb_tbl
            .and_then(|t| t.get("width"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(98) as i32,
        full_height: sb_tbl
            .and_then(|t| t.get("full_height"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(true),
        margin_left: sb_tbl
            .and_then(|t| t.get("margin_left"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(0) as i32,
        radii: sb_radii,
        normal: sb_norm,
        hover: sb_hov,
        active: sb_act,
        label: sb_tbl
            .and_then(|t| t.get("label"))
            .and_then(toml::Value::as_str)
            .unwrap_or("开始")
            .to_string(),
        text_color: sb_tbl
            .and_then(|t| t.get("text_color"))
            .and_then(parse_color)
            .unwrap_or(0xffffff),
        text_shadow: sb_tbl
            .and_then(|t| t.get("text_shadow"))
            .and_then(parse_color)
            .or(Some(0x123e10)),
        icon: sb_icon,
    };

    let start_menu = root
        .get("start_menu")
        .and_then(toml::Value::as_table)
        .map(|sm| {
            let layout = match sm.get("layout").and_then(toml::Value::as_str).unwrap_or("two_column")
            {
                "compact" => StartMenuLayoutKind::Compact,
                _ => StartMenuLayoutKind::TwoColumn,
            };
            let mut header_bg = parse_surface(Some(sm), "header", theme_dir, None);
            if header_bg.is_empty() {
                header_bg.bg = parse_css_background(
                    "linear-gradient(to bottom, #1868ce 0%, #0e60cb 12%, #0e60cb 20%, #1164cf 32%, #1667cf 33%, #1b6cd3 47%, #1e70d9 54%, #2476dc 60%, #297ae0 65%, #3482e3 77%, #3786e5 79%, #428ee9 90%, #4791eb 100%)",
                );
            }
            let mut footer_bg = parse_surface(Some(sm), "footer", theme_dir, None);
            if footer_bg.is_empty() {
                footer_bg.bg = parse_css_background(
                    "linear-gradient(to bottom, #4282d6 0%, #3b85e0 3%, #418ae3 5%, #418ae3 17%, #3c87e2 21%, #3786e4 26%, #3482e3 29%, #2e7ee1 39%, #2374df 49%, #2072db 57%, #196edb 62%, #176bd8 72%, #1468d5 75%, #1165d2 83%, #0f61cb 88%)",
                );
            }
            let mut logout_badge = parse_surface(Some(sm), "logout_badge", theme_dir, Some((22, 22)));
            if logout_badge.is_empty() {
                logout_badge.bg = parse_css_background("linear-gradient(to bottom, #f8b64c 0%, #e58716 55%, #c56606 100%)");
                logout_badge.border = Some((0xffffff, 0.9, 1));
            }
            let mut reboot_badge = parse_surface(Some(sm), "reboot_badge", theme_dir, Some((22, 22)));
            if reboot_badge.is_empty() {
                reboot_badge.bg = parse_css_background("linear-gradient(to bottom, #64b65c 0%, #3c9635 55%, #24741e 100%)");
                reboot_badge.border = Some((0xffffff, 0.9, 1));
            }
            let mut shutdown_badge =
                parse_surface(Some(sm), "shutdown_badge", theme_dir, Some((22, 22)));
            if shutdown_badge.is_empty() {
                shutdown_badge.bg = parse_css_background("linear-gradient(to bottom, #f46a52 0%, #d83b22 55%, #a81c07 100%)");
                shutdown_badge.border = Some((0xffffff, 0.9, 1));
            }
            StartMenuSkin {
                layout,
                width: sm
                    .get("width")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(416) as i32,
                height: sm
                    .get("height")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(508) as i32,
                offset_x: sm
                    .get("offset_x")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(0) as i32,
                gap_y: sm
                    .get("gap_y")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(0) as i32,
                top_radius: sm
                    .get("top_radius")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(6) as i32,
                bottom_radius: sm
                    .get("bottom_radius")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(0) as i32,
                frame_border: sm
                    .get("frame_border")
                    .and_then(parse_color)
                    .unwrap_or(0x1854b8),
                header_height: sm
                    .get("header_height")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(58) as i32,
                header_bg,
                header_top_sheen: sm
                    .get("header_top_sheen")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(true),
                header_text: sm
                    .get("header_text")
                    .and_then(parse_color)
                    .unwrap_or(0xffffff),
                header_text_shadow: sm
                    .get("header_text_shadow")
                    .and_then(parse_color)
                    .or(Some(0x0a246a)),
                avatar_border: sm
                    .get("avatar_border")
                    .and_then(parse_color)
                    .unwrap_or(0xdde8f8),
                orange_divider: sm.get("orange_divider").and_then(parse_color),
                left_bg: sm
                    .get("left_bg")
                    .and_then(parse_color)
                    .unwrap_or(0xffffff),
                left_text: sm
                    .get("left_text")
                    .and_then(parse_color)
                    .unwrap_or(0x1a1a1a),
                left_subtext: sm
                    .get("left_subtext")
                    .and_then(parse_color)
                    .unwrap_or(0x6e6e6e),
                right_bg: sm
                    .get("right_bg")
                    .and_then(parse_color)
                    .unwrap_or(0xd3e5fa),
                right_border_left: sm
                    .get("right_border_left")
                    .and_then(parse_color)
                    .unwrap_or(0x95bdee),
                right_text: sm
                    .get("right_text")
                    .and_then(parse_color)
                    .unwrap_or(0x0a1e5e),
                item_hover_bg: sm
                    .get("item_hover_bg")
                    .and_then(parse_color)
                    .unwrap_or(0x2f71cd),
                item_hover_text: sm
                    .get("item_hover_text")
                    .and_then(parse_color)
                    .unwrap_or(0xffffff),
                all_programs_label: sm
                    .get("all_programs_label")
                    .and_then(toml::Value::as_str)
                    .unwrap_or("所有程序")
                    .to_string(),
                all_programs_arrow: sm
                    .get("all_programs_arrow")
                    .and_then(parse_color)
                    .unwrap_or(0x2ca02c),
                footer_height: sm
                    .get("footer_height")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(42) as i32,
                footer_bg,
                footer_text: sm
                    .get("footer_text")
                    .and_then(parse_color)
                    .unwrap_or(0xffffff),
                logout_badge,
                reboot_badge,
                shutdown_badge,
            }
        });

    let ctrl_tbl = root.get("controls").and_then(toml::Value::as_table);
    let mut btn_normal = parse_surface(ctrl_tbl, "button_normal", theme_dir, None);
    if btn_normal.is_empty() {
        btn_normal.bg = parse_css_background("linear-gradient(180deg, #ffffff 0%, #ecebe5 86%, #d8d0c4 100%)");
        btn_normal.border = Some((0x003c74, 1.0, 1));
    }
    let mut btn_hover = parse_surface(ctrl_tbl, "button_hover", theme_dir, None);
    if btn_hover.is_empty() {
        btn_hover.bg = parse_css_background("linear-gradient(180deg, #ffffff 0%, #f5f4ef 86%, #e3ded4 100%)");
        btn_hover.border = Some((0x003c74, 1.0, 1));
        btn_hover.box_shadows = parse_css_box_shadow(
            "inset -1px 1px #fff0cf, inset 1px 2px #fdd889, inset -2px 2px #fbc761, inset 2px -2px #e5a01a",
        );
    }
    let mut btn_active = parse_surface(ctrl_tbl, "button_active", theme_dir, None);
    if btn_active.is_empty() {
        btn_active.bg = parse_css_background(
            "linear-gradient(180deg, #cdcac3 0%, #e3e3db 8%, #e5e5de 94%, #f2f2f1 100%)",
        );
        btn_active.border = Some((0x003c74, 1.0, 1));
    }
    let mut btn_primary = parse_surface(ctrl_tbl, "button_primary", theme_dir, None);
    if btn_primary.is_empty() {
        btn_primary.bg = btn_normal.bg.clone();
        btn_primary.border = Some((0x003c74, 1.0, 1));
        btn_primary.box_shadows = parse_css_box_shadow(
            "inset -1px 1px #cee7ff, inset 1px 2px #98b8ea, inset -2px 2px #bcd4f6, inset 1px -1px #89ade4, inset 2px -2px #89ade4",
        );
    }
    let mut card_surf = parse_surface(ctrl_tbl, "card", theme_dir, None);
    if card_surf.is_empty() {
        card_surf.bg = parse_css_background("linear-gradient(180deg, #fcfcfe 0%, #f4f3ee 100%)");
        card_surf.border = Some((0xd0d0bf, 1.0, 1));
    }
    let mut tab_normal = parse_surface(ctrl_tbl, "tab_normal", theme_dir, None);
    if tab_normal.is_empty() {
        tab_normal.bg = parse_css_background(
            "linear-gradient(180deg, #ffffff 0%, #fafaf9 26%, #f0f0ea 95%, #ecebe5 100%)",
        );
        tab_normal.border = Some((0x91a7b4, 1.0, 1));
    }
    let mut tab_hover = parse_surface(ctrl_tbl, "tab_hover", theme_dir, None);
    if tab_hover.is_empty() {
        tab_hover.bg = tab_normal.bg.clone();
        tab_hover.border = Some((0x919b9c, 1.0, 1));
        tab_hover.border_top = Some((0xe68b2c, 1));
        tab_hover.box_shadows = parse_css_box_shadow("inset 0 2px #ffc73c");
    }
    let mut tab_active = parse_surface(ctrl_tbl, "tab_active", theme_dir, None);
    if tab_active.is_empty() {
        tab_active.bg = Some(CssBackground::Solid(0xfcfcfe, 1.0));
        tab_active.border = Some((0x919b9c, 1.0, 1));
        tab_active.border_top = Some((0xe68b2c, 1));
        tab_active.box_shadows = parse_css_box_shadow("inset 0 2px #ffc73c");
    }
    let mut sb_track = parse_surface(ctrl_tbl, "scrollbar_track", theme_dir, None);
    if sb_track.is_empty() {
        sb_track.bg = parse_css_background("linear-gradient(to right, #eeede5 0%, #f6f5f0 45%, #fefefb 100%)");
    }
    let mut sb_thumb = parse_surface(ctrl_tbl, "scrollbar_thumb", theme_dir, None);
    if sb_thumb.is_empty() {
        sb_thumb.bg = parse_css_background("linear-gradient(to right, #d8e6fe 0%, #c8d6fb 50%, #9bbaf8 100%)");
        sb_thumb.border = Some((0x6b90dc, 1.0, 1));
        sb_thumb.box_shadows = parse_css_box_shadow("inset 1px 1px #ffffff");
    }
    let mut sb_thumb_hov = parse_surface(ctrl_tbl, "scrollbar_thumb_hover", theme_dir, None);
    if sb_thumb_hov.is_empty() {
        sb_thumb_hov.bg = parse_css_background("linear-gradient(to right, #e8f1ff 0%, #d8e5ff 50%, #b0cbfb 100%)");
        sb_thumb_hov.border = Some((0x537dd4, 1.0, 1));
        sb_thumb_hov.box_shadows = parse_css_box_shadow("inset 1px 1px #ffffff");
    }
    let mut sb_thumb_act = parse_surface(ctrl_tbl, "scrollbar_thumb_active", theme_dir, None);
    if sb_thumb_act.is_empty() {
        sb_thumb_act.bg = parse_css_background("linear-gradient(to right, #b6cdfb 0%, #9ab8f7 50%, #7d9fe8 100%)");
        sb_thumb_act.border = Some((0x3b63b8, 1.0, 1));
    }
    let mut prog_track = parse_surface(ctrl_tbl, "progress_track", theme_dir, None);
    if prog_track.is_empty() {
        prog_track.bg = Some(CssBackground::Solid(0xffffff, 1.0));
        prog_track.border = Some((0x686868, 1.0, 1));
    }
    let mut prog_fill = parse_surface(ctrl_tbl, "progress_fill", theme_dir, None);
    if prog_fill.is_empty() {
        prog_fill.bg = parse_css_background(
            "linear-gradient(180deg, #acedad 0%, #7be47d 14%, #4cda50 28%, #2ed330 42%, #42d845 57%, #76e275 71%, #8fe791 85%, #ffffff 100%)",
        );
    }

    let mut btn_primary_hover = parse_surface(ctrl_tbl, "button_primary_hover", theme_dir, None);
    if btn_primary_hover.is_empty() {
        btn_primary_hover = tint_surface(&btn_primary, 0xffffff, 0.14);
        btn_primary_hover.border = btn_primary_hover.border.or(btn_hover.border);
    }
    let mut btn_primary_active = parse_surface(ctrl_tbl, "button_primary_active", theme_dir, None);
    if btn_primary_active.is_empty() {
        btn_primary_active = tint_surface(&btn_primary, 0x000000, 0.18);
        btn_primary_active.border = btn_primary_active.border.or(btn_active.border);
    }
    let mut icon_btn_hover = parse_surface(ctrl_tbl, "icon_button_hover", theme_dir, None);
    if icon_btn_hover.is_empty() {
        icon_btn_hover = btn_hover.clone();
    }
    let mut icon_btn_active = parse_surface(ctrl_tbl, "icon_button_active", theme_dir, None);
    if icon_btn_active.is_empty() {
        icon_btn_active = btn_active.clone();
    }

    let button_radius = ctrl_tbl
        .and_then(|t| t.get("button_radius"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(3) as i32;
    let card_radius = ctrl_tbl
        .and_then(|t| t.get("card_radius"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(4) as i32;
    let input_bg = ctrl_tbl
        .and_then(|t| t.get("input_bg"))
        .and_then(parse_color)
        .unwrap_or(pal.input_bg);
    let input_border = ctrl_tbl
        .and_then(|t| t.get("input_border"))
        .and_then(parse_color)
        .unwrap_or(pal.window_border);
    let input_focus_border = ctrl_tbl
        .and_then(|t| t.get("input_focus_border"))
        .and_then(parse_color)
        .unwrap_or(pal.accent);

    let mut popup_surface = parse_surface(ctrl_tbl, "popup", theme_dir, None);
    if popup_surface.is_empty() {
        popup_surface.bg = Some(CssBackground::Solid(pal.panel, 1.0));
        popup_surface.border = Some((pal.panel_border, 1.0, 1));
        popup_surface.box_shadows = card_surf.box_shadows.clone();
    }
    let popup_radius = ctrl_tbl
        .and_then(|t| t.get("popup_radius"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(i64::from(card_radius)) as i32;

    let mut switch_track_off = parse_surface(ctrl_tbl, "switch_track_off", theme_dir, None);
    if switch_track_off.is_empty() {
        switch_track_off.bg = Some(CssBackground::Solid(mix(input_bg, pal.window_alt, 0.45), 1.0));
        switch_track_off.border = Some((input_border, 1.0, 1));
        switch_track_off.box_shadows = parse_css_box_shadow("inset 0 1px rgba(0, 0, 0, 0.14)");
    }
    let mut switch_track_on = parse_surface(ctrl_tbl, "switch_track_on", theme_dir, None);
    if switch_track_on.is_empty() {
        switch_track_on.bg = Some(CssBackground::linear_v(
            vec![
                (0.0, mix(pal.accent, 0xffffff, 0.22)),
                (1.0, mix(pal.accent, 0x000000, 0.10)),
            ],
            1.0,
        ));
        switch_track_on.border = Some((mix(pal.accent, 0x000000, 0.22), 1.0, 1));
        switch_track_on.box_shadows = parse_css_box_shadow("inset 0 1px rgba(255, 255, 255, 0.28)");
    }
    let mut switch_knob = parse_surface(ctrl_tbl, "switch_knob", theme_dir, None);
    if switch_knob.is_empty() {
        switch_knob = btn_normal.clone();
        switch_knob.border = switch_knob.border.or(Some((input_border, 1.0, 1)));
    }
    let switch_radius = ctrl_tbl
        .and_then(|t| t.get("switch_radius"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(-1) as i32;

    let mut slider_track = parse_surface(ctrl_tbl, "slider_track", theme_dir, None);
    if slider_track.is_empty() {
        slider_track = prog_track.clone();
    }
    let mut slider_fill = parse_surface(ctrl_tbl, "slider_fill", theme_dir, None);
    if slider_fill.is_empty() {
        slider_fill = prog_fill.clone();
    }
    let mut slider_thumb = parse_surface(ctrl_tbl, "slider_thumb", theme_dir, None);
    if slider_thumb.is_empty() {
        slider_thumb = btn_normal.clone();
    }
    let mut slider_thumb_hover = parse_surface(ctrl_tbl, "slider_thumb_hover", theme_dir, None);
    if slider_thumb_hover.is_empty() {
        slider_thumb_hover = btn_hover.clone();
    }
    let slider_thumb_radius = ctrl_tbl
        .and_then(|t| t.get("slider_thumb_radius"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(i64::from(button_radius)) as i32;

    let mut list_item_selected = parse_surface(ctrl_tbl, "list_item_selected", theme_dir, None);
    if list_item_selected.is_empty() {
        list_item_selected.bg = Some(CssBackground::Solid(pal.selection_bg, 1.0));
    }
    let mut list_item_hover = parse_surface(ctrl_tbl, "list_item_hover", theme_dir, None);
    if list_item_hover.is_empty() {
        list_item_hover.bg = Some(CssBackground::Solid(pal.panel_hover, 1.0));
    }
    let list_item_radius = ctrl_tbl
        .and_then(|t| t.get("list_item_radius"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(i64::from(button_radius.min(6))) as i32;
    let list_item_selected_text = ctrl_tbl
        .and_then(|t| t.get("list_item_selected_text"))
        .and_then(parse_color)
        .unwrap_or(pal.selection_text);

    let mut toolbar_bg = parse_surface(ctrl_tbl, "toolbar", theme_dir, None);
    if toolbar_bg.is_empty() {
        toolbar_bg.bg = Some(CssBackground::linear_v(
            vec![(0.0, pal.panel_high), (1.0, pal.panel)],
            1.0,
        ));
        toolbar_bg.border_bottom = Some((pal.window_border, 1));
    }

    let is_dark_rgb = |c: u32| -> bool {
        let r = ((c >> 16) & 0xff) as f32;
        let g = ((c >> 8) & 0xff) as f32;
        let b = (c & 0xff) as f32;
        (0.299 * r + 0.587 * g + 0.114 * b) < 138.0
    };
    let default_tab_active_text = tab_active
        .bg
        .as_ref()
        .map_or(pal.text, |b| {
            if is_dark_rgb(b.primary_color()) { 0xffffff } else { 0x111111 }
        });

    let controls = ControlsSkin {
        button_normal: btn_normal,
        button_hover: btn_hover,
        button_active: btn_active,
        button_primary: btn_primary,
        button_primary_hover: btn_primary_hover,
        button_primary_active: btn_primary_active,
        button_radius,
        button_text: ctrl_tbl
            .and_then(|t| t.get("button_text"))
            .and_then(parse_color)
            .unwrap_or(pal.text),
        button_primary_text: ctrl_tbl
            .and_then(|t| t.get("button_primary_text"))
            .and_then(parse_color)
            .unwrap_or(pal.selection_text),
        icon_button_hover: icon_btn_hover,
        icon_button_active: icon_btn_active,
        card: card_surf,
        card_radius,
        popup_surface,
        popup_radius,
        switch_track_off,
        switch_track_on,
        switch_knob,
        switch_radius,
        slider_track,
        slider_fill,
        slider_thumb,
        slider_thumb_hover,
        slider_thumb_radius,
        list_item_selected,
        list_item_hover,
        list_item_radius,
        list_item_selected_text,
        toolbar_bg,
        tab_normal,
        tab_hover,
        tab_active,
        tab_radii: ctrl_tbl
            .and_then(|t| t.get("tab_radius"))
            .and_then(parse_radii_val)
            .unwrap_or([3, 3, 0, 0]),
        tab_text: ctrl_tbl
            .and_then(|t| t.get("tab_text"))
            .and_then(parse_color)
            .unwrap_or(pal.text_dim),
        tab_active_text: ctrl_tbl
            .and_then(|t| t.get("tab_active_text"))
            .and_then(parse_color)
            .unwrap_or(default_tab_active_text),
        input_bg,
        input_border,
        input_focus_border,
        input_radius: ctrl_tbl
            .and_then(|t| t.get("input_radius"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(0) as i32,
        scrollbar_width: ctrl_tbl
            .and_then(|t| t.get("scrollbar_width"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(15) as i32,
        scrollbar_track: sb_track,
        scrollbar_thumb: sb_thumb,
        scrollbar_thumb_hover: sb_thumb_hov,
        scrollbar_thumb_active: sb_thumb_act,
        scrollbar_grip_light: ctrl_tbl
            .and_then(|t| t.get("scrollbar_grip_light"))
            .and_then(parse_color)
            .or(Some(0xeef4fe)),
        scrollbar_grip_dark: ctrl_tbl
            .and_then(|t| t.get("scrollbar_grip_dark"))
            .and_then(parse_color)
            .or(Some(0x7da4f2)),
        progress_track: prog_track,
        progress_fill: prog_fill,
        progress_chunk_w: ctrl_tbl
            .and_then(|t| t.get("progress_chunk_w"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(8) as i32,
        progress_chunk_gap: ctrl_tbl
            .and_then(|t| t.get("progress_chunk_gap"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(2) as i32,
    };

    let power_splash = parse_power_splash(root, theme_dir, pal);

    let mut skin = CustomThemeSkin {
        cache_key,
        window,
        titlebar,
        caption_buttons,
        taskbar,
        start_button,
        start_menu,
        controls,
        power_splash,
    };
    finalize_skin_cache_ids(&mut skin);
    skin
}

fn finalize_skin_cache_ids(skin: &mut CustomThemeSkin) {
    skin.window.active_frame.assign_cache_id();
    skin.window.inactive_frame.assign_cache_id();
    skin.titlebar.active.assign_cache_id();
    skin.titlebar.inactive.assign_cache_id();
    for s in [
        &mut skin.caption_buttons.close_normal,
        &mut skin.caption_buttons.close_hover,
        &mut skin.caption_buttons.close_active,
        &mut skin.caption_buttons.min_normal,
        &mut skin.caption_buttons.min_hover,
        &mut skin.caption_buttons.min_active,
        &mut skin.caption_buttons.max_normal,
        &mut skin.caption_buttons.max_hover,
        &mut skin.caption_buttons.max_active,
        &mut skin.caption_buttons.restore_normal,
        &mut skin.caption_buttons.restore_hover,
        &mut skin.caption_buttons.restore_active,
    ] {
        s.assign_cache_id();
    }
    skin.taskbar.bg.assign_cache_id();
    skin.taskbar.button_normal.assign_cache_id();
    skin.taskbar.button_hover.assign_cache_id();
    skin.taskbar.button_active.assign_cache_id();
    skin.taskbar.button_pressed.assign_cache_id();
    if let Some(ref mut tray) = skin.taskbar.tray_bg {
        tray.assign_cache_id();
    }
    skin.start_button.normal.assign_cache_id();
    skin.start_button.hover.assign_cache_id();
    skin.start_button.active.assign_cache_id();
    if let Some(ref mut sm) = skin.start_menu {
        sm.header_bg.assign_cache_id();
        sm.footer_bg.assign_cache_id();
        sm.logout_badge.assign_cache_id();
        sm.reboot_badge.assign_cache_id();
        sm.shutdown_badge.assign_cache_id();
    }
    for s in [
        &mut skin.controls.button_normal,
        &mut skin.controls.button_hover,
        &mut skin.controls.button_active,
        &mut skin.controls.button_primary,
        &mut skin.controls.button_primary_hover,
        &mut skin.controls.button_primary_active,
        &mut skin.controls.icon_button_hover,
        &mut skin.controls.icon_button_active,
        &mut skin.controls.card,
        &mut skin.controls.popup_surface,
        &mut skin.controls.switch_track_off,
        &mut skin.controls.switch_track_on,
        &mut skin.controls.switch_knob,
        &mut skin.controls.slider_track,
        &mut skin.controls.slider_fill,
        &mut skin.controls.slider_thumb,
        &mut skin.controls.slider_thumb_hover,
        &mut skin.controls.list_item_selected,
        &mut skin.controls.list_item_hover,
        &mut skin.controls.toolbar_bg,
        &mut skin.controls.tab_normal,
        &mut skin.controls.tab_hover,
        &mut skin.controls.tab_active,
        &mut skin.controls.scrollbar_track,
        &mut skin.controls.scrollbar_thumb,
        &mut skin.controls.scrollbar_thumb_hover,
        &mut skin.controls.scrollbar_thumb_active,
        &mut skin.controls.progress_track,
        &mut skin.controls.progress_fill,
    ] {
        s.assign_cache_id();
    }
    if let Some(ref mut ps) = skin.power_splash {
        for layer in &mut ps.layers {
            layer.surface.assign_cache_id();
        }
    }
}

fn apply_external_colors(theme: &mut Theme, colors: &toml::value::Table) {
    for (key, value) in colors {
        let Some(color) = parse_color(value) else {
            continue;
        };
        match key.as_str() {
            "accent" => theme.accent = color,
            "accent_soft" => theme.accent_soft = color,
            "bg_top" => theme.bg_top = color,
            "bg_bottom" => theme.bg_bottom = color,
            "panel" => theme.panel = color,
            "panel_high" => theme.panel_high = color,
            "panel_hover" => theme.panel_hover = color,
            "panel_border" => theme.panel_border = color,
            "text" => theme.text = color,
            "text_dim" => theme.text_dim = color,
            "text_muted" => theme.text_muted = color,
            "window" => theme.window = color,
            "window_high" => theme.window_high = color,
            "window_alt" => theme.window_alt = color,
            "window_border" => theme.window_border = color,
            "window_border_active" => theme.window_border_active = color,
            "titlebar" => theme.titlebar = color,
            "titlebar_hi" => theme.titlebar_hi = color,
            "titlebar_inactive" => theme.titlebar_inactive = color,
            "titlebar_inactive_hi" => theme.titlebar_inactive_hi = color,
            "titlebar_text" => theme.titlebar_text = color,
            "titlebar_inactive_text" => theme.titlebar_inactive_text = color,
            "input_bg" => theme.input_bg = color,
            "scrollbar_track" => theme.scrollbar_track = color,
            "scrollbar_thumb" => theme.scrollbar_thumb = color,
            "bevel_light" => theme.bevel_light = color,
            "bevel_hi" => theme.bevel_hi = color,
            "bevel_shadow" => theme.bevel_shadow = color,
            "bevel_dark" => theme.bevel_dark = color,
            "selection_bg" => theme.selection_bg = color,
            "selection_text" => theme.selection_text = color,
            "danger" => theme.danger = color,
            "warning" => theme.warning = color,
            "success" => theme.success = color,
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub style: ThemeStyle,
    pub mode: ThemeMode,
    pub accent: u32,
    pub font_scale: i32,
    pub accent_soft: u32,
    pub bg_top: u32,
    pub bg_bottom: u32,
    pub panel: u32,
    pub panel_high: u32,
    pub panel_hover: u32,
    pub panel_border: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_muted: u32,
    pub window: u32,
    pub window_high: u32,
    pub window_alt: u32,
    pub window_border: u32,
    pub window_border_active: u32,
    pub titlebar: u32,
    pub titlebar_hi: u32,
    pub titlebar_inactive: u32,
    pub titlebar_inactive_hi: u32,
    pub titlebar_text: u32,
    pub titlebar_inactive_text: u32,
    pub input_bg: u32,
    pub scrollbar_track: u32,
    pub scrollbar_thumb: u32,
    pub bevel_light: u32,
    pub bevel_hi: u32,
    pub bevel_shadow: u32,
    pub bevel_dark: u32,
    pub selection_bg: u32,
    pub selection_text: u32,
    pub danger: u32,
    pub warning: u32,
    pub success: u32,
    pub custom_skin: Option<SkinRef>,
}

impl Theme {
    pub fn from_config(config: &DesktopConfig) -> Self {
        let external = load_external_theme(&config.theme_id, config.mode, config.accent);
        let style = external
            .as_ref()
            .and_then(|theme| theme.base_style)
            .unwrap_or(config.theme_style);
        let mode = config.mode;
        let accent = external
            .as_ref()
            .and_then(|theme| {
                // Use the user's configured accent unless it is still at a built-in default
                // when loading a newly set external theme id in headless tests.
                if config.accent == uxconf::ACCENT_PRESETS[0].0 {
                    theme.default_accent
                } else {
                    Some(config.accent)
                }
            })
            .unwrap_or(config.accent);
        let mut theme = match (style, mode) {
            (ThemeStyle::VortexGlass, mode) => Self::vortexglass_with_accent(accent, mode),
            (ThemeStyle::Win98, mode) => Self::win98_with_accent(accent, mode),
            (ThemeStyle::Spui, ThemeMode::Dark) => Self::dark_with_accent(accent),
            (ThemeStyle::Spui, ThemeMode::Light) => Self::light_with_accent(accent),
        };
        if let Some(external) = external {
            if let Some(def_mode) = external.default_mode {
                theme.mode = if config.mode == ThemeMode::Dark {
                    ThemeMode::Dark
                } else {
                    def_mode
                };
            }
            apply_external_colors(&mut theme, &external.colors);
            theme.custom_skin = external.skin;
            let _ = external.id;
        }
        theme.font_scale = config.font_scale;
        theme
    }

    pub const fn skin(&self) -> Option<&'static CustomThemeSkin> {
        match self.custom_skin {
            Some(s) => Some(s.0),
            None => None,
        }
    }

    pub fn ui_scale(&self) -> f32 {
        (self.font_scale.clamp(85, 125) as f32) / 100.0
    }

    #[allow(dead_code)]
    pub fn dark() -> Self {
        Self::dark_with_accent(0x3b82f6)
    }

    pub fn dark_with_accent(accent: u32) -> Self {
        let base_panel = 0x18181b;
        let accent_soft = mix(base_panel, accent, 0.24);
        Self {
            style: ThemeStyle::Spui,
            mode: ThemeMode::Dark,
            accent,
            font_scale: 100,
            accent_soft,
            bg_top: 0x18181b,
            bg_bottom: 0x202024,
            panel: 0x18181b,
            panel_high: 0x232328,
            panel_hover: 0x2c2c32,
            panel_border: 0x323238,
            text: 0xf4f4f5,
            text_dim: 0xc4c4cc,
            text_muted: 0x8a8a93,
            window: 0x1e1e22,
            window_high: 0x26262b,
            window_alt: 0x19191d,
            window_border: 0x333339,
            window_border_active: mix(0x333339, accent, 0.55),
            titlebar: 0x18181b,
            titlebar_hi: 0x18181b,
            titlebar_inactive: 0x1d1d21,
            titlebar_inactive_hi: 0x1d1d21,
            titlebar_text: 0xf4f4f5,
            titlebar_inactive_text: 0x8a8a93,
            input_bg: 0x141417,
            scrollbar_track: 0x19191d,
            scrollbar_thumb: 0x3f3f46,
            bevel_light: 0x45454f,
            bevel_hi: 0x333339,
            bevel_shadow: 0x121215,
            bevel_dark: 0x09090b,
            selection_bg: accent_soft,
            selection_text: 0xf4f4f5,
            danger: 0xe81123,
            warning: 0xf59e0b,
            success: 0x10b981,
            custom_skin: None,
        }
    }

    pub fn light_with_accent(accent: u32) -> Self {
        let base_panel = 0xf4f4f6;
        let accent_soft = mix(base_panel, accent, 0.16);
        Self {
            style: ThemeStyle::Spui,
            mode: ThemeMode::Light,
            accent,
            font_scale: 100,
            accent_soft,
            bg_top: 0xe4e4e7,
            bg_bottom: 0xd4d4d8,
            panel: 0xf3f3f5,
            panel_high: 0xffffff,
            panel_hover: 0xe7e7ec,
            panel_border: 0xd4d4d8,
            text: 0x18181b,
            text_dim: 0x3f3f46,
            text_muted: 0x71717a,
            window: 0xfafafb,
            window_high: 0xffffff,
            window_alt: 0xf1f1f4,
            window_border: 0xd4d4d8,
            window_border_active: mix(0xd4d4d8, accent, 0.60),
            titlebar: 0xefeff2,
            titlebar_hi: 0xefeff2,
            titlebar_inactive: 0xf5f5f7,
            titlebar_inactive_hi: 0xf5f5f7,
            titlebar_text: 0x18181b,
            titlebar_inactive_text: 0x71717a,
            input_bg: 0xffffff,
            scrollbar_track: 0xededf0,
            scrollbar_thumb: 0xc4c4cc,
            bevel_light: 0xffffff,
            bevel_hi: 0xf4f4f5,
            bevel_shadow: 0xc4c4cc,
            bevel_dark: 0x8a8a93,
            selection_bg: accent_soft,
            selection_text: 0x18181b,
            danger: 0xdc2626,
            warning: 0xd97706,
            success: 0x059669,
            custom_skin: None,
        }
    }

    /// Classic Windows 98 3D bevel palette (authentic silver `#C0C0C0` or dark-mode variant).
    pub fn win98_with_accent(accent: u32, mode: ThemeMode) -> Self {
        let titlebar_hi = if accent == 0x000080 {
            0x1084d0
        } else {
            mix(accent, 0xffffff, 0.36)
        };
        if mode == ThemeMode::Dark {
            return Self {
                style: ThemeStyle::Win98,
                mode,
                accent,
                font_scale: 100,
                accent_soft: mix(0x2d2d32, accent, 0.32),
                bg_top: 0x004e4e,
                bg_bottom: 0x004e4e,
                panel: 0x2e2e34,
                panel_high: 0x383840,
                panel_hover: 0x41414a,
                panel_border: 0x1a1a1f,
                text: 0xf4f4f5,
                text_dim: 0xc8c8d0,
                text_muted: 0x92929c,
                window: 0x2a2a30,
                window_high: 0x34343b,
                window_alt: 0x242429,
                window_border: 0x18181d,
                window_border_active: 0x585862,
                titlebar: accent,
                titlebar_hi,
                titlebar_inactive: 0x484850,
                titlebar_inactive_hi: 0x62626c,
                titlebar_text: 0xffffff,
                titlebar_inactive_text: 0xd0d0d6,
                input_bg: 0x18181c,
                scrollbar_track: 0x232328,
                scrollbar_thumb: 0x3b3b44,
                bevel_light: 0x5a5a64,
                bevel_hi: 0x42424a,
                bevel_shadow: 0x18181c,
                bevel_dark: 0x09090c,
                selection_bg: accent,
                selection_text: 0xffffff,
                danger: 0xdc2626,
                warning: 0xd97706,
                success: 0x10b981,
                custom_skin: None,
            };
        }
        Self {
            style: ThemeStyle::Win98,
            mode: ThemeMode::Light,
            accent,
            font_scale: 100,
            accent_soft: mix(0xc0c0c0, accent, 0.20),
            bg_top: 0x008080,
            bg_bottom: 0x008080,
            panel: 0xc0c0c0,
            panel_high: 0xc0c0c0,
            panel_hover: 0xd4d0c8,
            panel_border: 0x808080,
            text: 0x000000,
            text_dim: 0x222222,
            text_muted: 0x555555,
            window: 0xc0c0c0,
            window_high: 0xc0c0c0,
            window_alt: 0xb4b4b4,
            window_border: 0x808080,
            window_border_active: 0x000000,
            titlebar: accent,
            titlebar_hi,
            titlebar_inactive: 0x808080,
            titlebar_inactive_hi: 0xb5b5b5,
            titlebar_text: 0xffffff,
            titlebar_inactive_text: 0xd4d0c8,
            input_bg: 0xffffff,
            scrollbar_track: 0xdfdfdf,
            scrollbar_thumb: 0xc0c0c0,
            bevel_light: 0xffffff,
            bevel_hi: 0xdfdfdf,
            bevel_shadow: 0x808080,
            bevel_dark: 0x0a0a0a,
            selection_bg: accent,
            selection_text: 0xffffff,
            danger: 0xc80000,
            warning: 0xc87800,
            success: 0x008000,
            custom_skin: None,
        }
    }

    /// Windows 7 Aero VortexGlass palette (authentic bright Aero controls in Light
    /// mode or smoky crystal dark Aero in Dark mode, with accent glass tinting).
    pub fn vortexglass_with_accent(accent: u32, mode: ThemeMode) -> Self {
        if mode == ThemeMode::Dark {
            let base_panel = 0x161d28;
            let accent_soft = mix(base_panel, accent, 0.30);
            return Self {
                style: ThemeStyle::VortexGlass,
                mode,
                accent,
                font_scale: 100,
                accent_soft,
                bg_top: 0x0b1726,
                bg_bottom: 0x152b44,
                panel: 0x161d28,
                panel_high: 0x202a38,
                panel_hover: mix(0x233042, accent, 0.28),
                panel_border: 0x3b4d63,
                text: 0xf1f5f9,
                text_dim: 0xc8d4e3,
                text_muted: 0x8a9bb0,
                window: 0x1c222b,
                window_high: 0x252d39,
                window_alt: 0x171c24,
                window_border: 0x3b4859,
                window_border_active: mix(0x5da9e9, accent, 0.55),
                titlebar: mix(0x101c2c, accent, 0.40),
                titlebar_hi: mix(0x294262, accent, 0.50),
                titlebar_inactive: 0x1e2632,
                titlebar_inactive_hi: 0x2a3444,
                titlebar_text: 0xf8fafc,
                titlebar_inactive_text: 0xaab7c6,
                input_bg: 0x131820,
                scrollbar_track: 0x171c24,
                scrollbar_thumb: 0x3b4b60,
                bevel_light: 0x526378,
                bevel_hi: 0x364354,
                bevel_shadow: 0x11161d,
                bevel_dark: 0x090c11,
                selection_bg: mix(0x1e3a5f, accent, 0.45),
                selection_text: 0xffffff,
                danger: 0xef4444,
                warning: 0xf59e0b,
                success: 0x22c55e,
                custom_skin: None,
            };
        }

        let base_panel = 0xeaf2fb;
        let accent_soft = mix(base_panel, accent, 0.24);
        Self {
            style: ThemeStyle::VortexGlass,
            mode: ThemeMode::Light,
            accent,
            font_scale: 100,
            accent_soft,
            bg_top: 0x18527a,
            bg_bottom: 0x2b7cb3,
            panel: 0xeaf2fb,
            panel_high: 0xf8fbff,
            panel_hover: 0xd9f0fc,
            panel_border: 0x8ea8c6,
            text: 0x0f172a,
            text_dim: 0x334155,
            text_muted: 0x64748b,
            window: 0xf0f0f0,
            window_high: 0xfbfcfd,
            window_alt: 0xe6edf5,
            window_border: 0x70849c,
            window_border_active: mix(0x4b79a8, accent, 0.50),
            titlebar: mix(0x2d5e8e, accent, 0.55),
            titlebar_hi: mix(0x95c8f2, accent, 0.45),
            titlebar_inactive: 0xcbd8e6,
            titlebar_inactive_hi: 0xe2eaf2,
            titlebar_text: 0x0b1220,
            titlebar_inactive_text: 0x475569,
            input_bg: 0xffffff,
            scrollbar_track: 0xeeeeee,
            scrollbar_thumb: 0xc2d5e8,
            bevel_light: 0xffffff,
            bevel_hi: 0xf4f4f4,
            bevel_shadow: 0x9aa4b2,
            bevel_dark: 0x707070,
            selection_bg: 0xd9f0fc,
            selection_text: 0x0f172a,
            danger: 0xd93025,
            warning: 0xd97706,
            success: 0x18a934,
            custom_skin: None,
        }
    }

    pub const fn is_dark(&self) -> bool {
        matches!(self.mode, ThemeMode::Dark)
    }

    pub const fn is_win98(&self) -> bool {
        self.custom_skin.is_none() && matches!(self.style, ThemeStyle::Win98)
    }

    pub const fn is_vortexglass(&self) -> bool {
        self.custom_skin.is_none() && matches!(self.style, ThemeStyle::VortexGlass)
    }

    pub const fn glass_chrome_enabled(&self) -> bool {
        self.is_vortexglass()
    }

    #[allow(dead_code)]
    pub fn style_label(&self) -> String {
        if let Some(skin) = self.skin() {
            skin.cache_key.0.clone()
        } else if self.is_win98() {
            String::from("Win98")
        } else if self.is_vortexglass() {
            String::from("VortexGlass 4.0")
        } else {
            String::from("SPUI 4.0")
        }
    }

    /// Accent-tinted Aero glass colorization base & opacity for active/inactive chrome.
    pub fn glass_tint(&self, active: bool) -> (u32, u8) {
        if self.is_dark() {
            if active {
                (mix(0x101c2d, self.accent, 0.36), 132)
            } else {
                (0x18212e, 102)
            }
        } else if active {
            (mix(0xd8ecff, self.accent, 0.35), 54)
        } else {
            (mix(0xe8eff8, self.accent, 0.16), 34)
        }
    }

    /// Classic Windows 7 hover cyan-blue glow border color for controls.
    pub fn aero_hover_border(&self) -> u32 {
        if self.is_dark() {
            mix(0x4da6e8, self.accent, 0.35)
        } else {
            0x3c7fb1
        }
    }

    /// Classic Windows 7 pressed border color for controls.
    pub fn aero_press_border(&self) -> u32 {
        if self.is_dark() {
            mix(0x3a84bd, self.accent, 0.45)
        } else {
            0x2c628b
        }
    }

    pub const fn titlebar_height(&self) -> i32 {
        if let Some(skin) = self.skin() {
            skin.titlebar.height
        } else if self.is_win98() {
            26
        } else if self.is_vortexglass() {
            VORTEX_TITLEBAR_HEIGHT
        } else {
            TITLEBAR_HEIGHT
        }
    }

    pub const fn window_border_width(&self) -> i32 {
        if let Some(skin) = self.skin() {
            skin.window.border_width
        } else if self.is_win98() {
            3
        } else if self.is_vortexglass() {
            0
        } else {
            1
        }
    }

    pub fn panel_height(&self, config: &DesktopConfig) -> i32 {
        if let Some(skin) = self.skin() {
            if let Some(h) = skin.taskbar.height {
                return (config.panel_size - 12).clamp(h.min(30), 48);
            }
        }
        if self.is_win98() {
            (config.panel_size - 12).clamp(28, 44)
        } else if self.is_vortexglass() {
            config.panel_size.clamp(38, 56)
        } else {
            config.panel_size.clamp(34, 60)
        }
    }

    #[allow(dead_code)]
    pub const fn radius_window(&self) -> i32 {
        self.radius_window_top()
    }

    pub const fn radius_window_top(&self) -> i32 {
        if let Some(skin) = self.skin() {
            skin.window.top_radius
        } else if self.is_win98() {
            0
        } else {
            RADIUS_WINDOW
        }
    }

    pub const fn radius_window_bottom(&self) -> i32 {
        if let Some(skin) = self.skin() {
            skin.window.bottom_radius
        } else if self.is_win98() {
            0
        } else {
            RADIUS_WINDOW
        }
    }

    pub const fn radius_popup(&self) -> i32 {
        if let Some(skin) = self.skin() {
            return skin.controls.popup_radius;
        }
        if self.is_win98() {
            0
        } else if self.is_vortexglass() {
            4
        } else {
            RADIUS_POPUP
        }
    }

    pub const fn radius_control(&self) -> i32 {
        if let Some(skin) = self.skin() {
            skin.controls.button_radius
        } else if self.is_win98() {
            0
        } else if self.is_vortexglass() {
            3
        } else {
            RADIUS_CONTROL
        }
    }

    pub const fn radius_card(&self) -> i32 {
        if let Some(skin) = self.skin() {
            skin.controls.card_radius
        } else if self.is_win98() {
            0
        } else if self.is_vortexglass() {
            4
        } else {
            6
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uxconf::{ThemePreset, ThemeStyle};

    #[test]
    fn theme_from_config_adapts_spui_and_win98() {
        let mut cfg = DesktopConfig::default();
        let vg = Theme::from_config(&cfg);
        assert!(vg.is_vortexglass());
        assert!(!vg.is_win98());
        assert_eq!(vg.titlebar_height(), VORTEX_TITLEBAR_HEIGHT);
        assert_eq!(vg.window_border_width(), 0);
        assert_eq!(vg.radius_control(), 3);
        assert_eq!(vg.panel_height(&cfg), 44);

        cfg.apply_theme_preset(ThemePreset::Spui4);
        let spui = Theme::from_config(&cfg);
        assert!(!spui.is_win98());
        assert!(!spui.is_vortexglass());
        assert_eq!(spui.titlebar_height(), 34);
        assert_eq!(spui.radius_window(), RADIUS_WINDOW);
        assert_eq!(spui.panel_height(&cfg), 46);

        cfg.apply_theme_preset(ThemePreset::Win98);
        let w98 = Theme::from_config(&cfg);
        assert!(w98.is_win98());
        assert_eq!(w98.titlebar_height(), 26);
        assert_eq!(w98.radius_window(), 0);
        assert_eq!(w98.radius_popup(), 0);
        assert_eq!(w98.radius_control(), 0);
        assert_eq!(w98.panel, 0xc0c0c0);
        assert_eq!(w98.titlebar, 0x000080);
        assert_eq!(w98.titlebar_hi, 0x1084d0);
        assert_eq!(w98.panel_height(&cfg), 34);
    }

    #[test]
    fn theme_preset_from_style_and_readonly_config_load() {
        assert_eq!(
            ThemePreset::from_style(ThemeStyle::VortexGlass),
            ThemePreset::VortexGlass
        );
        assert_eq!(ThemePreset::from_style(ThemeStyle::Spui), ThemePreset::Spui4);
        assert_eq!(ThemePreset::from_style(ThemeStyle::Win98), ThemePreset::Win98);

        let base = std::env::temp_dir().join(format!("spui_theme_ro_{}", std::process::id()));
        let sys_dir = base.join("sys");
        let usr_dir = base.join("usr");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&sys_dir).expect("mkdir sys");
        std::fs::create_dir_all(&usr_dir).expect("mkdir usr");

        let mut store = uxconf::storage::ConfigStore::open(sys_dir.clone(), usr_dir.clone());
        for (ch, prop, val) in ThemePreset::Win98.properties() {
            store.set(ch, prop, val).expect("apply win98 property");
        }

        let prev_sys = std::env::var_os("UXCONF_SYSTEM_DIR");
        let prev_usr = std::env::var_os("UXCONF_USER_DIR");
        std::env::set_var("UXCONF_SYSTEM_DIR", &sys_dir);
        std::env::set_var("UXCONF_USER_DIR", &usr_dir);

        let loaded = uxconf::storage::load_desktop_config_readonly();
        let theme = Theme::from_config(&loaded);

        match prev_sys {
            Some(v) => std::env::set_var("UXCONF_SYSTEM_DIR", v),
            None => std::env::remove_var("UXCONF_SYSTEM_DIR"),
        }
        match prev_usr {
            Some(v) => std::env::set_var("UXCONF_USER_DIR", v),
            None => std::env::remove_var("UXCONF_USER_DIR"),
        }
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(ThemePreset::parse(&loaded.theme_id), ThemePreset::Win98);
        assert_eq!(
            ThemePreset::from_style(loaded.theme_style),
            ThemePreset::Win98
        );
        assert_eq!(loaded.theme_style, ThemeStyle::Win98);
        assert_eq!(loaded.solid_color, 0x008080);
        assert!(theme.is_win98());
        assert_eq!(theme.titlebar_height(), 26);
        assert_eq!(theme.radius_window(), 0);
        assert_eq!(theme.radius_popup(), 0);
        assert_eq!(theme.radius_control(), 0);
        assert_eq!(theme.panel, 0xc0c0c0);
    }

    static THEME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct ThemeDirGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
        temp_base: PathBuf,
    }

    impl ThemeDirGuard {
        fn enter(base: PathBuf) -> Self {
            let lock = THEME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var_os("SPUI_THEME_DIR");
            std::env::set_var("SPUI_THEME_DIR", &base);
            Self {
                _lock: lock,
                previous,
                temp_base: base,
            }
        }
    }

    impl Drop for ThemeDirGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("SPUI_THEME_DIR", value),
                None => std::env::remove_var("SPUI_THEME_DIR"),
            }
            let _ = std::fs::remove_dir_all(&self.temp_base);
        }
    }

    #[test]
    fn external_theme_pack_overrides_palette_and_css_skin() {
        let base = std::env::temp_dir().join(format!(
            "spui_external_theme_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let theme_dir = base.join("testpack");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&theme_dir).expect("mkdir theme");
        std::fs::write(
            theme_dir.join("theme.toml"),
            concat!(
                "schema = 2\n",
                "id = \"testpack\"\n",
                "name = \"Test XP Pack\"\n",
                "base_style = \"spui\"\n",
                "mode = \"light\"\n",
                "accent = \"#0054e3\"\n",
                "[colors]\n",
                "titlebar = \"#0058e6\"\n",
                "panel = \"#ece9d8\"\n",
                "text = \"#000000\"\n",
                "[window]\n",
                "top_radius = 8\n",
                "bottom_radius = 0\n",
                "border_width = 3\n",
                "active_box_shadow = \"inset -1px -1px #00138c, inset 1px 1px #0831d9\"\n",
                "[titlebar]\n",
                "height = 28\n",
                "active_bg = \"linear-gradient(to bottom, #0997ff, #0053ee 8%, #06f 88%, #003dd7)\"\n",
            ),
        )
        .expect("write theme");

        let _env = ThemeDirGuard::enter(base);
        let discovered = discover_installed_themes();
        assert!(discovered.iter().any(|t| t.id == "testpack" && t.name == "Test XP Pack"));

        let mut config = DesktopConfig::default();
        config.theme_id = "testpack".to_string();
        config.theme_style = ThemeStyle::Spui;
        config.mode = ThemeMode::Light;
        let theme = Theme::from_config(&config);

        assert_eq!(theme.titlebar, 0x0058e6);
        assert_eq!(theme.panel, 0xece9d8);
        assert_eq!(theme.text, 0x000000);
        assert_eq!(theme.accent, 0x0054e3);
        assert_eq!(theme.titlebar_height(), 28);
        assert_eq!(theme.window_border_width(), 3);
        assert_eq!(theme.radius_window_top(), 8);
        assert_eq!(theme.radius_window_bottom(), 0);
        assert!(theme.skin().is_some());
    }

    fn stage_real_theme_pkg(pkg_dir_name: &str, theme_id: &str) -> Option<(PathBuf, ThemeDirGuard)> {
        let pkg_src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("packages")
            .join(pkg_dir_name);
        if !pkg_src.join("theme.toml").exists() {
            return None;
        }
        let base = std::env::temp_dir().join(format!(
            "spui_{}_pkg_{}_{}",
            theme_id,
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let theme_dir = base.join(theme_id);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&theme_dir).expect("mkdir theme");
        for entry in std::fs::read_dir(&pkg_src).expect("read package assets") {
            let entry = entry.expect("dir entry");
            let p = entry.path();
            if p.is_file() {
                std::fs::copy(&p, theme_dir.join(entry.file_name())).expect("copy asset");
            }
        }
        let guard = ThemeDirGuard::enter(base);
        Some((pkg_src, guard))
    }

    const CAPTION_SVGS: [&str; 12] = [
        "close.svg",
        "close_hover.svg",
        "close_active.svg",
        "minimize.svg",
        "minimize_hover.svg",
        "minimize_active.svg",
        "maximize.svg",
        "maximize_hover.svg",
        "maximize_active.svg",
        "restore.svg",
        "restore_hover.svg",
        "restore_active.svg",
    ];

    fn assert_caption_svgs_render(pkg_src: &Path, dim: (u32, u32), min_opaque_pixels: usize) {
        for name in CAPTION_SVGS {
            let decoded = icons::decode_theme_asset(&pkg_src.join(name), Some(dim))
                .unwrap_or_else(|| panic!("decode {} at {:?}", name, dim));
            assert_eq!((decoded.width, decoded.height), dim, "{}", name);
            let opaque = decoded.pixels.iter().filter(|&&p| (p >> 24) > 20).count();
            assert!(
                opaque > min_opaque_pixels,
                "{} rendered only {} opaque pixels (expected > {})",
                name,
                opaque,
                min_opaque_pixels
            );
        }
    }

    #[test]
    fn real_winxp_package_loads_luna_olive_silver_and_royale_noir_skins() {
        let Some((pkg_src, _guard)) = stage_real_theme_pkg("theme-winxp", "winxp") else {
            return;
        };
        let discovered = discover_installed_themes();
        assert!(discovered.iter().any(|t| t.id == "winxp" && t.name == "XP Luna"));

        let wp_thumb = icons::decode_theme_asset(&pkg_src.join("winxp-bliss.jpg"), Some((144, 90)))
            .expect("decode winxp-bliss.jpg thumbnail");
        assert_eq!((wp_thumb.width, wp_thumb.height), (144, 90));

        // 1. Default Luna Blue
        let mut cfg = DesktopConfig::default();
        cfg.theme_id = "winxp".to_string();
        cfg.theme_style = ThemeStyle::Spui;
        cfg.mode = ThemeMode::Light;
        cfg.accent = 0x0054e3;
        let luna = Theme::from_config(&cfg);
        let luna_skin = luna.skin().expect("Luna Blue skin");
        assert_eq!(luna.panel, 0xece9d8);
        assert!(luna_skin.caption_buttons.close_normal.sprite.is_some());
        assert!(luna_skin.caption_buttons.close_hover.sprite.is_some());
        assert!(luna_skin.caption_buttons.close_active.sprite.is_some());
        assert!(luna_skin.caption_buttons.min_normal.sprite.is_some());
        assert!(luna_skin.caption_buttons.max_normal.sprite.is_some());
        assert!(luna_skin.caption_buttons.restore_normal.sprite.is_some());
        assert!(luna_skin.start_button.icon.is_none());
        assert!(luna_skin.start_menu.is_some());
        let luna_ps = luna_skin
            .power_splash
            .as_ref()
            .expect("XP Luna power_splash");
        assert!(!luna_ps.layers.is_empty());
        assert_eq!(
            luna_ps
                .animation
                .as_ref()
                .expect("XP Luna splash animation")
                .frames
                .len(),
            12
        );

        // 2. Olive Green (Homestead) via #10b981 accent swatch
        cfg.accent = 0x10b981;
        let olive = Theme::from_config(&cfg);
        assert_eq!(olive.accent, 0x748753);

        // 3. Metallic Silver via #64748b accent swatch
        cfg.accent = 0x64748b;
        let silver = Theme::from_config(&cfg);
        assert_eq!(silver.window, 0xe8e9ed);

        // 4. Royale Noir / Zune Dark via Dark mode
        cfg.mode = ThemeMode::Dark;
        let noir = Theme::from_config(&cfg);
        assert_eq!(noir.window, 0x20242d);
        assert_eq!(noir.text, 0xeef1f7);
        let noir_ps = noir
            .skin()
            .and_then(|s| s.power_splash.as_ref())
            .expect("XP Royale Noir power_splash");
        assert_ne!(noir_ps.bg_color, luna_ps.bg_color);
        assert_eq!(noir_ps.animation.as_ref().unwrap().frames.len(), 12);
    }

    #[test]
    fn real_aqua_package_loads_classic_graphite_and_dark_skins() {
        let Some((pkg_src, _guard)) = stage_real_theme_pkg("theme-aqua", "aqua") else {
            return;
        };
        let discovered = discover_installed_themes();
        let aqua_meta = discovered
            .iter()
            .find(|t| t.id == "aqua")
            .expect("aqua theme discovered");
        assert_eq!(aqua_meta.name, "Aqua Classic");
        assert_eq!(aqua_meta.button_layout, Some(ButtonLayout::Left));
        assert_eq!(aqua_meta.panel_position, Some(PanelPosition::Top));

        let wp_thumb = icons::decode_theme_asset(&pkg_src.join("aqua-wave.jpg"), Some((144, 90)))
            .expect("decode aqua-wave.jpg thumbnail");
        assert_eq!((wp_thumb.width, wp_thumb.height), (144, 90));
        assert_caption_svgs_render(&pkg_src, (14, 14), 50);

        // 1. Aqua Classic (light)
        let mut cfg = DesktopConfig::default();
        cfg.theme_id = "aqua".to_string();
        cfg.theme_style = ThemeStyle::Spui;
        cfg.mode = ThemeMode::Light;
        cfg.accent = 0x1b74e4;
        let classic = Theme::from_config(&cfg);
        let classic_skin = classic.skin().expect("Aqua Classic skin");
        assert_eq!(classic_skin.titlebar.title_align, TitleAlign::Center);
        assert_eq!(classic_skin.controls.progress_chunk_w, 0);
        assert!(classic_skin.start_button.icon.is_none());
        assert!(classic_skin.caption_buttons.close_normal.sprite.is_some());
        assert!(classic_skin.caption_buttons.min_normal.sprite.is_some());
        assert!(classic_skin.caption_buttons.max_normal.sprite.is_some());
        assert!(classic_skin.caption_buttons.restore_normal.sprite.is_some());
        let aqua_ps = classic_skin
            .power_splash
            .as_ref()
            .expect("Aqua power_splash");
        assert!(!aqua_ps.layers.is_empty());
        assert_eq!(
            aqua_ps
                .animation
                .as_ref()
                .expect("Aqua splash animation")
                .frames
                .len(),
            12
        );
        assert_eq!(classic_skin.controls.tab_active_text, 0xffffff);

        // 2. Graphite accent (#64748b) switches traffic lights to Graphite CSS orbs
        cfg.accent = 0x64748b;
        let graphite = Theme::from_config(&cfg);
        assert_eq!(graphite.accent, 0x6a7b8c);
        let graphite_skin = graphite.skin().expect("Graphite skin");
        assert!(graphite_skin.caption_buttons.close_normal.sprite.is_none());
        assert!(graphite_skin.caption_buttons.min_normal.sprite.is_none());
        assert!(graphite_skin.caption_buttons.max_normal.sprite.is_none());
        assert!(graphite_skin.caption_buttons.restore_normal.sprite.is_none());

        // 3. Aqua Dark mode
        cfg.mode = ThemeMode::Dark;
        let dark = Theme::from_config(&cfg);
        assert_eq!(dark.window, 0x252629);
        assert_eq!(dark.text, 0xeef1f6);
        let dark_ps = dark
            .skin()
            .and_then(|s| s.power_splash.as_ref())
            .expect("Aqua Dark power_splash");
        assert_ne!(dark_ps.bg_color, aqua_ps.bg_color);
        assert_eq!(dark_ps.animation.as_ref().unwrap().frames.len(), 12);
    }

    #[test]
    fn real_ambiance_package_loads_default_radiance_mate_and_dark_skins() {
        let Some((pkg_src, _guard)) = stage_real_theme_pkg("theme-ambiance", "ambiance") else {
            return;
        };
        let discovered = discover_installed_themes();
        let meta = discovered
            .iter()
            .find(|t| t.id == "ambiance")
            .expect("ambiance theme discovered");
        assert_eq!(meta.name, "Ambiance");
        assert_eq!(meta.button_layout, Some(ButtonLayout::Left));
        assert_eq!(meta.panel_position, Some(PanelPosition::Top));

        let wp_thumb =
            icons::decode_theme_asset(&pkg_src.join("ambiance-wave.jpg"), Some((144, 90)))
                .expect("decode ambiance-wave.jpg thumbnail");
        assert_eq!((wp_thumb.width, wp_thumb.height), (144, 90));
        assert_caption_svgs_render(&pkg_src, (19, 19), 100);

        // 1. Ambiance default
        let mut cfg = DesktopConfig::default();
        cfg.theme_id = "ambiance".to_string();
        cfg.theme_style = ThemeStyle::Spui;
        cfg.mode = ThemeMode::Light;
        cfg.accent = 0xdd4814;
        let def = Theme::from_config(&cfg);
        let def_skin = def.skin().expect("Ambiance skin");
        assert_eq!(def_skin.titlebar.title_align, TitleAlign::Left);
        assert_eq!(def_skin.controls.progress_chunk_w, 0);
        assert!(def_skin.start_button.icon.is_none());
        assert!(def_skin.caption_buttons.close_normal.sprite.is_some());
        assert!(def_skin.caption_buttons.min_normal.sprite.is_some());
        assert!(def_skin.caption_buttons.max_normal.sprite.is_some());
        assert!(def_skin.caption_buttons.restore_normal.sprite.is_some());
        let amb_ps = def_skin
            .power_splash
            .as_ref()
            .expect("Ambiance power_splash");
        assert!(!amb_ps.layers.is_empty());
        assert_eq!(
            amb_ps
                .animation
                .as_ref()
                .expect("Ambiance splash animation")
                .frames
                .len(),
            10
        );

        // 2. Radiance (#f59e0b) light chrome
        cfg.accent = 0xf59e0b;
        let radiance = Theme::from_config(&cfg);
        assert_eq!(radiance.titlebar_text, 0x3c3c3c);
        let rad_ps = radiance
            .skin()
            .and_then(|s| s.power_splash.as_ref())
            .expect("Radiance power_splash");
        assert_ne!(rad_ps.bg_color, amb_ps.bg_color);

        // 3. MATE green (#10b981)
        cfg.accent = 0x10b981;
        let mate = Theme::from_config(&cfg);
        assert_eq!(mate.accent, 0x6f913d);
        let mate_ps = mate
            .skin()
            .and_then(|s| s.power_splash.as_ref())
            .expect("MATE power_splash");
        assert_ne!(mate_ps.bg_color, amb_ps.bg_color);

        // 4. Ambiance Darkness (Dark mode)
        cfg.mode = ThemeMode::Dark;
        let darkness = Theme::from_config(&cfg);
        assert_eq!(darkness.window, 0x2c2b29);
        assert_eq!(darkness.text, 0xe7e4dc);
        let dark_ps = darkness
            .skin()
            .and_then(|s| s.power_splash.as_ref())
            .expect("Darkness power_splash");
        assert_ne!(dark_ps.bg_color, amb_ps.bg_color);
        assert_eq!(dark_ps.animation.as_ref().unwrap().frames.len(), 10);
    }

    #[test]
    fn sprite_only_surface_skips_css_bg_and_border() {
        let mut pixels = vec![0u32; 8 * 8];
        for y in 2..6 {
            for x in 2..6 {
                pixels[y * 8 + x] = 0xffff_3311;
            }
        }
        let sprite = crate::draw::Sprite {
            width: 8,
            height: 8,
            pixels,
        };
        let surf = CssSurface {
            bg: Some(CssBackground::Solid(0x00ff00, 1.0)),
            border: Some((0xff0000, 1.0, 1)),
            sprite: Some(sprite),
            ..CssSurface::default()
        };
        let mut canvas = Canvas::new(12, 12, 0x223344);
        surf.paint(&mut canvas, Rect::new(2, 2, 8, 8), [0; 4]);
        // Transparent outer ring of sprite must preserve untouched canvas background (zero border/bg halo).
        assert_eq!(canvas.pixels()[2 * 12 + 2] & 0x00ff_ffff, 0x223344);
        assert_eq!(canvas.pixels()[2 * 12 + 9] & 0x00ff_ffff, 0x223344);
        assert_eq!(canvas.pixels()[4 * 12 + 4] & 0x00ff_ffff, 0xff3311);
    }

    #[test]
    fn surface_cache_matches_uncached_paint() {
        let mut surf = CssSurface {
            bg: Some(CssBackground::linear_v(
                vec![(0.0, 0x4488cc), (0.5, 0x225599), (1.0, 0x113366)],
                1.0,
            )),
            border: Some((0x99ccff, 1.0, 1)),
            box_shadows: parse_css_box_shadow("inset 0 1px #ffffff"),
            radii: Some([4, 4, 4, 4]),
            ..CssSurface::default()
        };
        surf.assign_cache_id();
        let rect = Rect::new(4, 3, 24, 18);

        let mut expected = Canvas::new(32, 24, 0x00000000);
        surf.paint_uncached(&mut expected, rect, [4; 4]);

        let mut miss = Canvas::new(32, 24, 0x00000000);
        surf.paint(&mut miss, rect, [4; 4]);
        for (idx, (&cached_px, &direct_px)) in
            miss.pixels().iter().zip(expected.pixels().iter()).enumerate()
        {
            for shift in [16, 8, 0] {
                let c = ((cached_px >> shift) & 0xff) as i32;
                let d = ((direct_px >> shift) & 0xff) as i32;
                assert!(
                    (c - d).abs() <= 2,
                    "pixel {idx} channel shift {shift}: cached={cached_px:#08x} direct={direct_px:#08x}"
                );
            }
        }

        let mut hit = Canvas::new(32, 24, 0x00000000);
        surf.paint(&mut hit, rect, [4; 4]);
        assert_eq!(hit.pixels(), miss.pixels());
    }

    #[test]
    fn gradient_lut_matches_linear_interpolation() {
        let stops = [(0.0f32, 0x102030), (0.5, 0x8090a0), (1.0, 0xf0e0d0)];
        let lut = crate::draw::build_gradient_lut(&stops);
        assert_eq!(lut[0], 0x102030);
        assert_eq!(lut[255], 0xf0e0d0);
        assert_eq!(
            lut[128],
            Canvas::sample_gradient_stops(&stops, 128.0 / 255.0) & 0x00ff_ffff
        );
    }

    #[test]
    fn theme_dir_fingerprint_detects_asset_updates() {
        let dir = std::env::temp_dir().join(format!("spui_fp_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("theme.toml"), "id = \"t\"\n").expect("write toml");
        let fp1 = theme_dir_fingerprint(&dir);
        assert_ne!(fp1, 0);

        std::fs::write(dir.join("splash_anim.svg"), "<svg/>").expect("add asset");
        let fp2 = theme_dir_fingerprint(&dir);
        assert_ne!(fp2, fp1);

        std::fs::remove_file(dir.join("splash_anim.svg")).expect("remove asset");
        let fp3 = theme_dir_fingerprint(&dir);
        assert_eq!(fp3, fp1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub const FONT_UI: &str = "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc";
pub const FONT_UI_BOLD: &str = "/usr/share/fonts/noto/NotoSansCJK-Bold.ttc";
pub const FONT_MONO: &str = "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc";
pub const FONT_FALLBACK: &str = "/usr/share/fonts/dejavu/DejaVuSans.ttf";
pub const FONT_CYBER_DISPLAY: &str = "/usr/share/spui/fonts/Audiowide-Regular.ttf";
pub const FONT_CYBER_MONO: &str = "/usr/share/spui/fonts/ShareTechMono-Regular.ttf";
