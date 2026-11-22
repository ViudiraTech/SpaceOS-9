/*
 *
 *       user/desktop/src/icons.rs
 *       KDE Breeze SVG rasterizer, SpaceOS logo PNG loader and wallpaper JPEG decoder
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Loads and caches KDE Breeze multi-color & symbolic SVGs (`/usr/share/spui/icons/breeze/`),
//! the official `SpaceOS9.png` logo (`/usr/share/spui/logo.png`) and Unsplash
//! landscape JPEGs (`/usr/share/spui/wallpapers/`).

use crate::draw::{Canvas, Rect, Sprite};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

const ICON_DIR: &str = "/usr/share/spui/icons/breeze";
const LOGO_PATH: &str = "/usr/share/spui/logo.png";

fn resolve_sysroot_path(path: &str) -> std::path::PathBuf {
    if path.starts_with("/usr/") {
        if let Ok(root) = std::env::var("SPUI_SYSROOT") {
            let trimmed = root.trim();
            if !trimmed.is_empty() {
                return Path::new(trimmed.trim_end_matches('/')).join(path.trim_start_matches('/'));
            }
        }
    }
    std::path::PathBuf::from(path)
}

const DOCUMENT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">
  <defs>
    <linearGradient id="docbg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#38bdf8"/>
      <stop offset="52%" stop-color="#2563eb"/>
      <stop offset="100%" stop-color="#1e40af"/>
    </linearGradient>
  </defs>
  <rect x="4" y="4" width="40" height="40" rx="11" fill="url(#docbg)"/>
  <path d="M15 11 H28 L34 17 V36 A2 2 0 0 1 32 38 H15 A2 2 0 0 1 13 36 V13 A2 2 0 0 1 15 11 Z" fill="#ffffff" opacity="0.96"/>
  <path d="M28 11 V17 H34 Z" fill="#93c5fd"/>
  <rect x="17" y="21" width="13" height="2.4" rx="1.2" fill="#2563eb"/>
  <rect x="17" y="26" width="13" height="2.2" rx="1.1" fill="#60a5fa"/>
  <rect x="17" y="31" width="9" height="2.2" rx="1.1" fill="#60a5fa"/>
</svg>
"##;

const BROWSER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#38bdf8"/>
      <stop offset="55%" stop-color="#2563eb"/>
      <stop offset="100%" stop-color="#1e3a8a"/>
    </linearGradient>
  </defs>
  <circle cx="24" cy="24" r="20" fill="url(#bg)"/>
  <ellipse cx="24" cy="24" rx="9" ry="20" fill="none" stroke="#e0f2fe" stroke-width="2" opacity="0.85"/>
  <path d="M5 18 H43 M5 30 H43" stroke="#e0f2fe" stroke-width="2" opacity="0.8"/>
  <ellipse cx="24" cy="24" rx="22" ry="7" transform="rotate(-24 24 24)" fill="none" stroke="#7dd3fc" stroke-width="2.4"/>
  <circle cx="38" cy="14" r="3" fill="#ffffff"/>
</svg>
"##;

/// Built-in "check for new software" icon: an indigo shopping bag with a
/// refresh badge. Rendered straight from this string, so no icon theme entry
/// is needed.
const STORE_CHECK_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">
  <defs>
    <linearGradient id="scbg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#a5b4fc"/>
      <stop offset="48%" stop-color="#6366f1"/>
      <stop offset="100%" stop-color="#4338ca"/>
    </linearGradient>
  </defs>
  <rect x="4" y="4" width="40" height="40" rx="11" fill="url(#scbg)"/>
  <path d="M15.5 20h17l-1.6 15.4a3 3 0 0 1-3 2.6h-7.9a3 3 0 0 1-3-2.6z" fill="#ffffff" opacity="0.96"/>
  <path d="M18.6 21.5a5.4 5.4 0 0 1 10.8 0" fill="none" stroke="#ffffff" stroke-width="2.6" stroke-linecap="round"/>
  <circle cx="34" cy="15" r="8.4" fill="#1e1b4b" opacity="0.35"/>
  <path d="M35.6 9.6a6 6 0 1 0 2.6 6.9" fill="none" stroke="#ffffff" stroke-width="2.4" stroke-linecap="round"/>
  <path d="M37.6 8.2l0.7 5.3-5.2-1.4z" fill="#ffffff"/>
</svg>"##;

const SHIELD_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">
  <defs>
    <linearGradient id="shbg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#60a5fa"/>
      <stop offset="52%" stop-color="#2563eb"/>
      <stop offset="100%" stop-color="#1e3a8a"/>
    </linearGradient>
  </defs>
  <path d="M24 4 L40 10.5 V23 C40 33.5 32.8 41.2 24 44.5 C15.2 41.2 8 33.5 8 23 V10.5 Z" fill="url(#shbg)"/>
  <path d="M24 6.5 L37.5 12 V23 C37.5 31.8 31.4 38.4 24 41.5 C16.6 38.4 10.5 31.8 10.5 23 V12 Z" fill="none" stroke="#bae6fd" stroke-width="1.2" opacity="0.55"/>
  <rect x="17.5" y="21.5" width="13" height="10.5" rx="2.4" fill="#ffffff" opacity="0.96"/>
  <path d="M20 21.5 V18.5 a4 4 0 0 1 8 0 V21.5" fill="none" stroke="#ffffff" stroke-width="2.4" stroke-linecap="round"/>
  <circle cx="24" cy="26.5" r="1.6" fill="#1d4ed8"/>
</svg>
"##;

const STORE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">
  <defs>
    <linearGradient id="stbg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#38bdf8"/>
      <stop offset="52%" stop-color="#2563eb"/>
      <stop offset="100%" stop-color="#1d4ed8"/>
    </linearGradient>
  </defs>
  <rect x="4" y="4" width="40" height="40" rx="11" fill="url(#stbg)"/>
  <path d="M24 12 L34 31 M24 12 L14 31 M11 26.5 H37" fill="none" stroke="#ffffff" stroke-width="3.4" stroke-linecap="round" stroke-linejoin="round"/>
  <circle cx="34" cy="31" r="1.4" fill="#e0f2fe"/>
</svg>
"##;

const MINESWEEPER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">
  <rect x="3" y="3" width="42" height="42" rx="4" fill="#c0c0c0"/>
  <path d="M5 5 H41 L39 7 H7 V39 L5 41 Z" fill="#ffffff"/>
  <path d="M43 43 H7 L9 41 H41 V9 L43 7 Z" fill="#6b7280"/>
  <rect x="8" y="8" width="32" height="32" fill="#d1d5db"/>
  <line x1="24" y1="9" x2="24" y2="39" stroke="#111827" stroke-width="3.2" stroke-linecap="round"/>
  <line x1="9" y1="24" x2="39" y2="24" stroke="#111827" stroke-width="3.2" stroke-linecap="round"/>
  <line x1="13" y1="13" x2="35" y2="35" stroke="#111827" stroke-width="2.8" stroke-linecap="round"/>
  <line x1="35" y1="13" x2="13" y2="35" stroke="#111827" stroke-width="2.8" stroke-linecap="round"/>
  <circle cx="24" cy="24" r="10.5" fill="#111827"/>
  <rect x="19" y="19" width="4" height="4" rx="1" fill="#ffffff"/>
  <rect x="30" y="7" width="2.2" height="11" fill="#111827"/>
  <polygon points="32,7 41,10.5 32,14" fill="#ef4444"/>
</svg>"##;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    SpaceLogo,
    Terminal,
    Files,
    Editor,
    Document,
    Settings,
    Monitor,
    Calculator,
    Minesweeper,
    ImageViewer,
    Music,
    Browser,
    Store,
    StoreCheck,
    Help,
    Login,
    Shield,
    Notify,
    Appearance,
    Wallpaper,
    Panel,
    WindowManager,
    Display,
    Sound,
    InputDevices,
    Network,
    Applications,
    DateTime,
    About,
    AudioVolumeMuted,
    AudioVolumeLow,
    AudioVolumeMedium,
    AudioVolumeHigh,
    Folder,
    FolderLarge,
    UserHome,
    FolderRoot,
    FolderDocuments,
    FolderTemp,
    FolderScript,
    FileText,
    FileRust,
    FileC,
    FileScript,
    FileExec,
    FileBin,
    FileImage,
    Search,
    Save,
    NewFile,
    NewFolder,
    Refresh,
    GoUp,
    GoBack,
    GoHome,
    Logout,
    Shutdown,
    Bell,
    BellActive,
    NetworkWired,
    NetworkOffline,
    Info,
}

macro_rules! define_svg_icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        impl Icon {
            fn svg_name(self) -> Option<&'static str> {
                match self {
                    Self::SpaceLogo => None,
                    $(Self::$variant => Some($file),)*
                }
            }

            /// Canonical identifier stored on X11 windows via `_SPUI_ICON`.
            pub fn prop_name(self) -> &'static str {
                self.svg_name().unwrap_or("space-logo")
            }

            /// Parse a `_SPUI_ICON` property value back into an [`Icon`].
            pub fn from_prop_name(name: &str) -> Option<Self> {
                match name.trim() {
                    "space-logo" => Some(Self::SpaceLogo),
                    $($file => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

define_svg_icons! {
    Terminal => "terminal.svg",
    Files => "files.svg",
    Editor => "editor.svg",
    Document => "document.svg",
    Settings => "settings.svg",
    Monitor => "monitor.svg",
    Calculator => "calculator.svg",
    Minesweeper => "minesweeper.svg",
    ImageViewer => "image-viewer.svg",
    Music => "music.svg",
    Browser => "browser.svg",
    Store => "store.svg",
    StoreCheck => "store-check.svg",
    Help => "help.svg",
    Login => "login.svg",
    Shield => "shield.svg",
    Notify => "notify.svg",
    Appearance => "appearance.svg",
    Wallpaper => "wallpaper.svg",
    Panel => "panel.svg",
    WindowManager => "windows.svg",
    Display => "display.svg",
    InputDevices => "peripherals.svg",
    Network => "network.svg",
    Applications => "applications.svg",
    DateTime => "datetime.svg",
    About => "about.svg",
    Folder => "folder.svg",
    FolderLarge => "folder-large.svg",
    UserHome => "user-home.svg",
    FolderRoot => "folder-root.svg",
    FolderDocuments => "folder-documents.svg",
    FolderTemp => "folder-temp.svg",
    FolderScript => "folder-script.svg",
    FileText => "file-text.svg",
    FileRust => "file-rust.svg",
    FileC => "file-c.svg",
    FileScript => "file-script.svg",
    FileExec => "file-exec.svg",
    FileBin => "file-bin.svg",
    FileImage => "file-image.svg",
    Search => "search.svg",
    Save => "save.svg",
    NewFile => "new-file.svg",
    NewFolder => "new-folder.svg",
    Refresh => "refresh.svg",
    GoUp => "go-up.svg",
    GoBack => "go-back.svg",
    GoHome => "go-home.svg",
    Logout => "logout.svg",
    Shutdown => "shutdown.svg",
    Bell => "bell.svg",
    BellActive => "bell-active.svg",
    NetworkWired => "network-wired.svg",
    NetworkOffline => "network-offline.svg",
    Info => "info.svg",
    Sound => "sound.svg",
    AudioVolumeMuted => "audio-volume-muted.svg",
    AudioVolumeLow => "audio-volume-low.svg",
    AudioVolumeMedium => "audio-volume-medium.svg",
    AudioVolumeHigh => "audio-volume-high.svg",
}

impl Icon {
    pub fn for_volume(volume: i32, muted: bool) -> Self {
        if muted || volume <= 0 {
            Self::AudioVolumeMuted
        } else if volume <= 33 {
            Self::AudioVolumeLow
        } else if volume <= 66 {
            Self::AudioVolumeMedium
        } else {
            Self::AudioVolumeHigh
        }
    }

    pub fn is_symbolic(self) -> bool {
        matches!(
            self,
            Self::Search
                | Self::Save
                | Self::NewFile
                | Self::NewFolder
                | Self::Refresh
                | Self::GoUp
                | Self::GoBack
                | Self::GoHome
                | Self::Logout
                | Self::Shutdown
                | Self::Bell
                | Self::BellActive
                | Self::NetworkWired
                | Self::NetworkOffline
                | Self::Info
                | Self::AudioVolumeMuted
                | Self::AudioVolumeLow
                | Self::AudioVolumeMedium
                | Self::AudioVolumeHigh
        )
    }

    pub fn for_file(path: &Path, is_dir: bool) -> Self {
        if is_dir {
            return Self::Folder;
        }
        match path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("rs") => Self::FileRust,
            Some("c" | "h" | "cpp" | "hpp") => Self::FileC,
            Some("sh" | "py") => Self::FileScript,
            Some("png" | "jpg" | "jpeg" | "svg" | "bmp") => Self::FileImage,
            Some("mp3" | "flac" | "wav" | "ogg") => Self::Music,
            Some("docx") => Self::Document,
            Some(
                "txt" | "md" | "toml" | "json" | "yaml" | "yml" | "conf" | "log" | "ini"
                | "service",
            ) => Self::FileText,
            None => Self::FileExec,
            _ => Self::FileBin,
        }
    }
}

struct IconStore {
    svg_sources: HashMap<Icon, String>,
    sprites: HashMap<(Icon, u32, u32), Arc<Sprite>>,
    logo_base: Option<Sprite>,
    logo_tried: bool,
}

impl IconStore {
    fn new() -> Self {
        Self {
            svg_sources: HashMap::new(),
            sprites: HashMap::new(),
            logo_base: None,
            logo_tried: false,
        }
    }

    fn get(&mut self, icon: Icon, size: u32, tint: Option<u32>) -> Option<Arc<Sprite>> {
        let size = size.clamp(8, 256);
        let color_key = if icon.is_symbolic() {
            tint.unwrap_or(0xd4d4d8) & 0x00ff_ffff
        } else {
            0xffff_ffff
        };
        let key = (icon, size, color_key);
        if let Some(sprite) = self.sprites.get(&key) {
            return Some(sprite.clone());
        }

        let rendered = if icon == Icon::SpaceLogo {
            self.render_logo(size)
        } else {
            self.render_svg(
                icon,
                size,
                if icon.is_symbolic() {
                    Some(color_key)
                } else {
                    None
                },
            )
        }?;

        let arc = Arc::new(rendered);
        self.sprites.insert(key, arc.clone());
        Some(arc)
    }

    fn render_logo(&mut self, size: u32) -> Option<Sprite> {
        if !self.logo_tried {
            self.logo_tried = true;
            let logo = resolve_sysroot_path(LOGO_PATH);
            if let Some(mut raw) = decode_png(&logo) {
                // Mipmap down to 128x128 once so all icon sizes (18..72px) are supersampled.
                while raw.width > 256 && raw.height > 256 {
                    raw = box_halve(&raw);
                }
                self.logo_base = Some(raw.scale_bilinear(128, 128));
            }
        }
        let base = self.logo_base.as_ref()?;
        let mut scaled = base.scale_bilinear(size, size);
        apply_rounded_mask(&mut scaled, (size as f32 * 0.22).max(2.0));
        Some(scaled)
    }

    fn render_svg(&mut self, icon: Icon, size: u32, tint: Option<u32>) -> Option<Sprite> {
        let file = icon.svg_name()?;
        if !self.svg_sources.contains_key(&icon) {
            let path = resolve_sysroot_path(&format!("{ICON_DIR}/{file}"));
            let xml = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(_) if icon == Icon::Minesweeper => String::from(MINESWEEPER_SVG),
                Err(_) if icon == Icon::Browser => String::from(BROWSER_SVG),
                Err(_) if icon == Icon::Store => String::from(STORE_SVG),
                Err(_) if icon == Icon::StoreCheck => String::from(STORE_CHECK_SVG),
                Err(_) if icon == Icon::Shield => String::from(SHIELD_SVG),
                Err(_) if icon == Icon::Document => String::from(DOCUMENT_SVG),
                Err(_) => return None,
            };
            self.svg_sources.insert(icon, xml);
        }
        let source = self.svg_sources.get(&icon)?;
        let modified;
        let xml = if let Some(rgb) = tint {
            let hex = format!("#{:06x}", rgb & 0x00ff_ffff);
            modified = source
                .replace("#232629", &hex)
                .replace("#4d4d4d", &hex)
                .replace("#eff0f1", &hex);
            modified.as_str()
        } else {
            source.as_str()
        };
        let tree = Tree::from_str(xml, &Options::default()).ok()?;
        let mut pixmap = Pixmap::new(size, size)?;
        let ts = tree.size();
        let sx = size as f32 / ts.width().max(1.0);
        let sy = size as f32 / ts.height().max(1.0);
        let scale = sx.min(sy);
        let tx = (size as f32 - ts.width() * scale) * 0.5;
        let ty = (size as f32 - ts.height() * scale) * 0.5;
        resvg::render(
            &tree,
            Transform::from_scale(scale, scale).post_translate(tx, ty),
            &mut pixmap.as_mut(),
        );
        Some(Sprite::from_rgba_premul(size, size, pixmap.data()))
    }
}

fn store() -> &'static Mutex<IconStore> {
    static STORE: OnceLock<Mutex<IconStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(IconStore::new()))
}

/// Draw `icon` centered inside `(x, y, size, size)`, with fallback geometry if missing.
pub fn draw(canvas: &mut Canvas, icon: Icon, x: i32, y: i32, size: u32, tint: Option<u32>) {
    let sprite = store()
        .lock()
        .ok()
        .and_then(|mut guard| guard.get(icon, size, tint));
    if let Some(sprite) = sprite {
        canvas.blit_sprite(x, y, &sprite);
    } else {
        let fallback = tint.unwrap_or(0x3b82f6);
        canvas.rounded_rect(
            Rect::new(x + 2, y + 2, size as i32 - 4, size as i32 - 4),
            3,
            fallback,
            0.85,
        );
    }
}

fn box_halve(src: &Sprite) -> Sprite {
    let nw = (src.width / 2).max(1);
    let nh = (src.height / 2).max(1);
    let sw = src.width as usize;
    let mut pixels = Vec::with_capacity((nw * nh) as usize);
    for y in 0..nh as usize {
        let r0 = &src.pixels[(y * 2) * sw..];
        let r1 = &src.pixels[(y * 2 + 1).min(src.height as usize - 1) * sw..];
        for x in 0..nw as usize {
            let x0 = x * 2;
            let x1 = (x0 + 1).min(sw - 1);
            let p00 = r0[x0];
            let p10 = r0[x1];
            let p01 = r1[x0];
            let p11 = r1[x1];
            let mut avg = 0u32;
            for shift in [0, 8, 16, 24] {
                let c = (((p00 >> shift) & 0xff)
                    + ((p10 >> shift) & 0xff)
                    + ((p01 >> shift) & 0xff)
                    + ((p11 >> shift) & 0xff)
                    + 2)
                    >> 2;
                avg |= c << shift;
            }
            pixels.push(avg);
        }
    }
    Sprite {
        width: nw,
        height: nh,
        pixels,
    }
}

fn apply_rounded_mask(sprite: &mut Sprite, radius: f32) {
    let w = sprite.width as i32;
    let h = sprite.height as i32;
    let r = radius.min(w as f32 * 0.5).min(h as f32 * 0.5);
    let ri = r.ceil() as i32;
    for y in 0..h {
        let cy = if y < ri {
            Some(r)
        } else if y >= h - ri {
            Some(h as f32 - r)
        } else {
            None
        };
        let Some(cy) = cy else { continue };
        for x in 0..w {
            let cx = if x < ri {
                Some(r)
            } else if x >= w - ri {
                Some(w as f32 - r)
            } else {
                None
            };
            let Some(cx) = cx else { continue };
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let cov = (r + 0.5 - dist).clamp(0.0, 1.0);
            if cov < 1.0 {
                let idx = (y as usize) * (w as usize) + (x as usize);
                let px = sprite.pixels[idx];
                let scale = (cov * 255.0 + 0.5) as u32;
                let a = (((px >> 24) & 0xff) * scale + 127) / 255;
                let rr = (((px >> 16) & 0xff) * scale + 127) / 255;
                let gg = (((px >> 8) & 0xff) * scale + 127) / 255;
                let bb = ((px & 0xff) * scale + 127) / 255;
                sprite.pixels[idx] = (a << 24) | (rr << 16) | (gg << 8) | bb;
            }
        }
    }
}

pub fn decode_png(path: &Path) -> Option<Sprite> {
    let bytes = fs::read(path).ok()?;
    decode_png_bytes(&bytes)
}

#[allow(dead_code)]
pub fn decode_png_bytes(bytes: &[u8]) -> Option<Sprite> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let slice = &buf[..info.buffer_size()];
    match info.color_type {
        png::ColorType::Rgba => Some(Sprite::from_rgba_straight(info.width, info.height, slice)),
        png::ColorType::Rgb => Some(Sprite::from_rgb(info.width, info.height, slice)),
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
            for ga in slice.chunks_exact(2) {
                rgba.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
            Some(Sprite::from_rgba_straight(info.width, info.height, &rgba))
        }
        png::ColorType::Grayscale => {
            let mut rgb = Vec::with_capacity((info.width * info.height * 3) as usize);
            for &g in slice {
                rgb.extend_from_slice(&[g, g, g]);
            }
            Some(Sprite::from_rgb(info.width, info.height, &rgb))
        }
        png::ColorType::Indexed => None,
    }
}

pub fn decode_jpeg(path: &Path) -> Option<Sprite> {
    let bytes = fs::read(path).ok()?;
    decode_jpeg_bytes(&bytes)
}

#[allow(dead_code)]
pub fn decode_jpeg_bytes(bytes: &[u8]) -> Option<Sprite> {
    let mut decoder = zune_jpeg::JpegDecoder::new(bytes);
    let decoded = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    if decoded.len() == (w * h * 3) as usize {
        Some(Sprite::from_rgb(w, h, &decoded))
    } else if decoded.len() == (w * h) as usize {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for &g in &decoded {
            rgb.extend_from_slice(&[g, g, g]);
        }
        Some(Sprite::from_rgb(w, h, &rgb))
    } else {
        None
    }
}

/// Decode in-memory PNG or JPEG bytes by checking their magic signature.
#[allow(dead_code)]
pub fn decode_image_bytes(bytes: &[u8]) -> Option<Sprite> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png_bytes(bytes)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        decode_jpeg_bytes(bytes)
    } else {
        None
    }
}

/// Decode an SVG file from disk into a premultiplied ARGB [`Sprite`], using either its
/// intrinsic SVG `viewBox` dimensions or `target_size`.
pub fn decode_svg_file(path: &Path, target_size: Option<(u32, u32)>) -> Option<Sprite> {
    let xml = fs::read_to_string(path).ok()?;
    decode_svg_str(&xml, target_size)
}

/// Decode an SVG string into a premultiplied ARGB [`Sprite`].
pub fn decode_svg_str(xml: &str, target_size: Option<(u32, u32)>) -> Option<Sprite> {
    let tree = Tree::from_str(xml, &Options::default()).ok()?;
    let ts = tree.size();
    let (w, h) = target_size.unwrap_or_else(|| {
        (
            ts.width().round().clamp(1.0, 1024.0) as u32,
            ts.height().round().clamp(1.0, 1024.0) as u32,
        )
    });
    let mut pixmap = Pixmap::new(w.max(1), h.max(1))?;
    let sx = w as f32 / ts.width().max(1.0);
    let sy = h as f32 / ts.height().max(1.0);
    resvg::render(&tree, Transform::from_scale(sx, sy), &mut pixmap.as_mut());
    Some(Sprite::from_rgba_premul(w, h, pixmap.data()))
}

/// Decode a theme package sprite asset (`.svg`, `.png`, or `.jpg`/`.jpeg`).
pub fn decode_theme_asset(path: &Path, target_size: Option<(u32, u32)>) -> Option<Sprite> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)?;
    match ext.as_str() {
        "svg" => decode_svg_file(path, target_size),
        "png" => {
            let sp = decode_png(path)?;
            if let Some((tw, th)) = target_size {
                if sp.width != tw || sp.height != th {
                    return Some(sp.scale_bilinear(tw.max(1), th.max(1)));
                }
            }
            Some(sp)
        }
        "jpg" | "jpeg" => {
            let sp = decode_jpeg(path)?;
            if let Some((tw, th)) = target_size {
                if sp.width != tw || sp.height != th {
                    return Some(sp.scale_bilinear(tw.max(1), th.max(1)));
                }
            }
            Some(sp)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minesweeper_builtin_svg_renders_cleanly() {
        let mut store = IconStore::new();
        let sprite = store
            .render_svg(Icon::Minesweeper, 48, None)
            .expect("embedded minesweeper SVG must render");
        assert_eq!((sprite.width, sprite.height), (48, 48));
        assert!(sprite.pixels.iter().any(|&p| (p >> 24) != 0));
    }

    #[test]
    fn browser_builtin_svg_renders_cleanly() {
        let mut store = IconStore::new();
        let sprite = store
            .render_svg(Icon::Browser, 48, None)
            .expect("embedded browser SVG must render");
        assert_eq!((sprite.width, sprite.height), (48, 48));
        assert!(sprite.pixels.iter().any(|&p| (p >> 24) != 0));
    }

    #[test]
    fn store_check_builtin_svg_renders_cleanly() {
        let mut store = IconStore::new();
        let sprite = store
            .render_svg(Icon::StoreCheck, 48, None)
            .expect("embedded store-check SVG must render");
        assert_eq!((sprite.width, sprite.height), (48, 48));
        assert!(sprite.pixels.iter().any(|&p| (p >> 24) != 0));
    }

    #[test]
    fn store_builtin_svg_renders_cleanly() {
        let mut store = IconStore::new();
        let sprite = store
            .render_svg(Icon::Store, 48, None)
            .expect("embedded store SVG must render");
        assert_eq!((sprite.width, sprite.height), (48, 48));
        assert!(sprite.pixels.iter().any(|&p| (p >> 24) != 0));
    }

    #[test]
    fn shield_builtin_svg_renders_cleanly() {
        let mut store = IconStore::new();
        let sprite = store
            .render_svg(Icon::Shield, 48, None)
            .expect("embedded shield SVG must render");
        assert_eq!((sprite.width, sprite.height), (48, 48));
        assert!(sprite.pixels.iter().any(|&p| (p >> 24) != 0));
    }

    #[test]
    fn document_builtin_svg_renders_cleanly() {
        let mut store = IconStore::new();
        let sprite = store
            .render_svg(Icon::Document, 48, None)
            .expect("embedded document SVG must render");
        assert_eq!((sprite.width, sprite.height), (48, 48));
        assert!(sprite.pixels.iter().any(|&p| (p >> 24) != 0));
    }
}
