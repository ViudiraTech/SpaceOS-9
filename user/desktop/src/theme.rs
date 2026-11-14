/*
 *
 *       user/desktop/src/theme.rs
 *       SPUI 4.0 neutral graphite dark & crisp light palettes and metrics
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Visual constants for the desktop shell and its applications. Colors are
//! packed `0xRRGGBB`; palettes use neutral zinc/graphite grays (zero dark-navy
//! neon tint) paired with a clean light mode and restrained native metrics.

use crate::draw::mix;
use uxconf::{DesktopConfig, ThemeMode};

pub const TITLEBAR_HEIGHT: i32 = 34;
#[allow(dead_code)]
pub const TASKBAR_HEIGHT: i32 = 46;
#[allow(dead_code)]
pub const RADIUS_WINDOW: i32 = 6;
pub const RADIUS_POPUP: i32 = 8;
pub const RADIUS_CONTROL: i32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
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
    pub titlebar_inactive: u32,
    pub input_bg: u32,
    pub scrollbar_track: u32,
    pub scrollbar_thumb: u32,
    pub danger: u32,
    pub warning: u32,
    pub success: u32,
}

impl Theme {
    pub fn from_config(config: &DesktopConfig) -> Self {
        let mut theme = match config.mode {
            ThemeMode::Dark => Self::dark_with_accent(config.accent),
            ThemeMode::Light => Self::light_with_accent(config.accent),
        };
        theme.font_scale = config.font_scale;
        theme
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
        Self {
            mode: ThemeMode::Dark,
            accent,
            font_scale: 100,
            accent_soft: mix(base_panel, accent, 0.24),
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
            titlebar_inactive: 0x1d1d21,
            input_bg: 0x141417,
            scrollbar_track: 0x19191d,
            scrollbar_thumb: 0x3f3f46,
            danger: 0xe81123,
            warning: 0xf59e0b,
            success: 0x10b981,
        }
    }

    pub fn light_with_accent(accent: u32) -> Self {
        let base_panel = 0xf4f4f6;
        Self {
            mode: ThemeMode::Light,
            accent,
            font_scale: 100,
            accent_soft: mix(base_panel, accent, 0.16),
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
            titlebar_inactive: 0xf5f5f7,
            input_bg: 0xffffff,
            scrollbar_track: 0xededf0,
            scrollbar_thumb: 0xc4c4cc,
            danger: 0xdc2626,
            warning: 0xd97706,
            success: 0x059669,
        }
    }

    pub const fn is_dark(&self) -> bool {
        matches!(self.mode, ThemeMode::Dark)
    }
}

pub const FONT_UI: &str = "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc";
pub const FONT_UI_BOLD: &str = "/usr/share/fonts/noto/NotoSansCJK-Bold.ttc";
pub const FONT_MONO: &str = "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc";
pub const FONT_FALLBACK: &str = "/usr/share/fonts/dejavu/DejaVuSans.ttf";
pub const FONT_CYBER_DISPLAY: &str = "/usr/share/spui/fonts/Audiowide-Regular.ttf";
pub const FONT_CYBER_MONO: &str = "/usr/share/spui/fonts/ShareTechMono-Regular.ttf";
