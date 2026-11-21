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

use std::fs;
use std::path::PathBuf;

use crate::draw::mix;
use uxconf::{DesktopConfig, ThemeMode, ThemeStyle};

pub const TITLEBAR_HEIGHT: i32 = 34;
pub const VORTEX_TITLEBAR_HEIGHT: i32 = 30;
#[allow(dead_code)]
pub const TASKBAR_HEIGHT: i32 = 46;
#[allow(dead_code)]
pub const RADIUS_WINDOW: i32 = 6;
pub const RADIUS_POPUP: i32 = 8;
pub const RADIUS_CONTROL: i32 = 5;

const SYSTEM_THEME_DIR: &str = "/usr/share/spui/themes";
const USER_THEME_SUBDIR: &str = ".local/share/spui/themes";

#[derive(Clone, Debug)]
struct ExternalTheme {
    base_style: Option<ThemeStyle>,
    mode: Option<ThemeMode>,
    accent: Option<u32>,
    colors: toml::value::Table,
}

fn theme_dir_candidates(id: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("SPUI_THEME_DIR") {
        candidates.push(PathBuf::from(dir).join(id).join("theme.toml"));
    }
    candidates.push(PathBuf::from(SYSTEM_THEME_DIR).join(id).join("theme.toml"));
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(USER_THEME_SUBDIR)
                .join(id)
                .join("theme.toml"),
        );
    }
    candidates
}

fn valid_theme_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn load_external_theme(id: &str) -> Option<ExternalTheme> {
    if !valid_theme_id(id) {
        return None;
    }
    let text = theme_dir_candidates(id)
        .into_iter()
        .find_map(|path| fs::read_to_string(path).ok())?;
    let root: toml::Value = toml::from_str(&text).ok()?;
    let base_style = root
        .get("base_style")
        .and_then(toml::Value::as_str)
        .map(ThemeStyle::parse);
    let mode = root
        .get("mode")
        .and_then(toml::Value::as_str)
        .map(ThemeMode::parse);
    let accent = root.get("accent").and_then(parse_color);
    let colors = root
        .get("colors")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    Some(ExternalTheme {
        base_style,
        mode,
        accent,
        colors,
    })
}

fn parse_color(value: &toml::Value) -> Option<u32> {
    let text = value.as_str()?.trim().trim_start_matches('#');
    if text.len() != 6 {
        return None;
    }
    u32::from_str_radix(text, 16).ok()
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
}

impl Theme {
    pub fn from_config(config: &DesktopConfig) -> Self {
        let external = load_external_theme(&config.theme_id);
        let style = external
            .as_ref()
            .and_then(|theme| theme.base_style)
            .unwrap_or(config.theme_style);
        let mode = external
            .as_ref()
            .and_then(|theme| theme.mode)
            .unwrap_or(config.mode);
        let accent = external
            .as_ref()
            .and_then(|theme| theme.accent)
            .unwrap_or(config.accent);
        let mut theme = match (style, mode) {
            (ThemeStyle::VortexGlass, mode) => Self::vortexglass_with_accent(accent, mode),
            (ThemeStyle::Win98, mode) => Self::win98_with_accent(accent, mode),
            (ThemeStyle::Spui, ThemeMode::Dark) => Self::dark_with_accent(accent),
            (ThemeStyle::Spui, ThemeMode::Light) => Self::light_with_accent(accent),
        };
        if let Some(external) = external {
            apply_external_colors(&mut theme, &external.colors);
        }
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
        }
    }

    pub const fn is_dark(&self) -> bool {
        matches!(self.mode, ThemeMode::Dark)
    }

    pub const fn is_win98(&self) -> bool {
        matches!(self.style, ThemeStyle::Win98)
    }

    pub const fn is_vortexglass(&self) -> bool {
        matches!(self.style, ThemeStyle::VortexGlass)
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
        if self.is_win98() {
            26
        } else if self.is_vortexglass() {
            VORTEX_TITLEBAR_HEIGHT
        } else {
            TITLEBAR_HEIGHT
        }
    }

    pub const fn window_border_width(&self) -> i32 {
        if self.is_win98() {
            3
        } else if self.is_vortexglass() {
            0
        } else {
            1
        }
    }

    pub fn panel_height(&self, config: &DesktopConfig) -> i32 {
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
        if self.is_win98() {
            0
        } else {
            RADIUS_WINDOW
        }
    }

    pub const fn radius_popup(&self) -> i32 {
        if self.is_win98() {
            0
        } else if self.is_vortexglass() {
            4
        } else {
            RADIUS_POPUP
        }
    }

    pub const fn radius_control(&self) -> i32 {
        if self.is_win98() {
            0
        } else if self.is_vortexglass() {
            3
        } else {
            RADIUS_CONTROL
        }
    }

    pub const fn radius_card(&self) -> i32 {
        if self.is_win98() {
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

    #[test]
    fn external_theme_pack_overrides_palette() {
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
            "schema = 1\nid = \"testpack\"\nbase_style = \"spui\"\nmode = \"light\"\naccent = \"#0054e3\"\n[colors]\ntitlebar = \"#0058e6\"\npanel = \"#ece9d8\"\ntext = \"#000000\"\n",
        )
        .expect("write theme");

        let previous = std::env::var_os("SPUI_THEME_DIR");
        std::env::set_var("SPUI_THEME_DIR", &base);
        let mut config = DesktopConfig::default();
        config.theme_id = "testpack".to_string();
        config.theme_style = ThemeStyle::Spui;
        config.mode = ThemeMode::Light;
        let theme = Theme::from_config(&config);
        match previous {
            Some(value) => std::env::set_var("SPUI_THEME_DIR", value),
            None => std::env::remove_var("SPUI_THEME_DIR"),
        }
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(theme.titlebar, 0x0058e6);
        assert_eq!(theme.panel, 0xece9d8);
        assert_eq!(theme.text, 0x000000);
        assert_eq!(theme.accent, 0x0054e3);
    }
}

pub const FONT_UI: &str = "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc";
pub const FONT_UI_BOLD: &str = "/usr/share/fonts/noto/NotoSansCJK-Bold.ttc";
pub const FONT_MONO: &str = "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc";
pub const FONT_FALLBACK: &str = "/usr/share/fonts/dejavu/DejaVuSans.ttf";
pub const FONT_CYBER_DISPLAY: &str = "/usr/share/spui/fonts/Audiowide-Regular.ttf";
pub const FONT_CYBER_MONO: &str = "/usr/share/spui/fonts/ShareTechMono-Regular.ttf";
