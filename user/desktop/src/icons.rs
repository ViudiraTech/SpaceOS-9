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
//! the official `SpaceOS9.png` logo (`/usr/share/spui/logo.png`) and `picsum.photos`
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

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    SpaceLogo,
    Terminal,
    Files,
    Editor,
    Settings,
    Monitor,
    Calculator,
    ImageViewer,
    Help,
    Login,
    Notify,
    Appearance,
    Wallpaper,
    Panel,
    WindowManager,
    Display,
    InputDevices,
    Applications,
    DateTime,
    About,
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
    Info,
}

impl Icon {
    fn svg_name(self) -> Option<&'static str> {
        match self {
            Self::SpaceLogo => None,
            Self::Terminal => Some("terminal.svg"),
            Self::Files => Some("files.svg"),
            Self::Editor => Some("editor.svg"),
            Self::Settings => Some("settings.svg"),
            Self::Monitor => Some("monitor.svg"),
            Self::Calculator => Some("calculator.svg"),
            Self::ImageViewer => Some("image-viewer.svg"),
            Self::Help => Some("help.svg"),
            Self::Login => Some("login.svg"),
            Self::Notify => Some("notify.svg"),
            Self::Appearance => Some("appearance.svg"),
            Self::Wallpaper => Some("wallpaper.svg"),
            Self::Panel => Some("panel.svg"),
            Self::WindowManager => Some("windows.svg"),
            Self::Display => Some("display.svg"),
            Self::InputDevices => Some("peripherals.svg"),
            Self::Applications => Some("applications.svg"),
            Self::DateTime => Some("datetime.svg"),
            Self::About => Some("about.svg"),
            Self::Folder => Some("folder.svg"),
            Self::FolderLarge => Some("folder-large.svg"),
            Self::UserHome => Some("user-home.svg"),
            Self::FolderRoot => Some("folder-root.svg"),
            Self::FolderDocuments => Some("folder-documents.svg"),
            Self::FolderTemp => Some("folder-temp.svg"),
            Self::FolderScript => Some("folder-script.svg"),
            Self::FileText => Some("file-text.svg"),
            Self::FileRust => Some("file-rust.svg"),
            Self::FileC => Some("file-c.svg"),
            Self::FileScript => Some("file-script.svg"),
            Self::FileExec => Some("file-exec.svg"),
            Self::FileBin => Some("file-bin.svg"),
            Self::FileImage => Some("file-image.svg"),
            Self::Search => Some("search.svg"),
            Self::Save => Some("save.svg"),
            Self::NewFile => Some("new-file.svg"),
            Self::NewFolder => Some("new-folder.svg"),
            Self::Refresh => Some("refresh.svg"),
            Self::GoUp => Some("go-up.svg"),
            Self::GoBack => Some("go-back.svg"),
            Self::GoHome => Some("go-home.svg"),
            Self::Logout => Some("logout.svg"),
            Self::Shutdown => Some("shutdown.svg"),
            Self::Bell => Some("bell.svg"),
            Self::BellActive => Some("bell-active.svg"),
            Self::Info => Some("info.svg"),
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
                | Self::Info
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
            if let Some(mut raw) = decode_png(Path::new(LOGO_PATH)) {
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
            let path = format!("{ICON_DIR}/{file}");
            let xml = fs::read_to_string(path).ok()?;
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
    let mut decoder = zune_jpeg::JpegDecoder::new(bytes.as_slice());
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
