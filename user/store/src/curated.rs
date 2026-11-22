use crate::model::{CatalogJson, StoreCategory, StoreMeta, StorePackage};
use spui_ui::icons::Icon;

/// Presentation for one package. Everything here comes either from the
/// repository's `store.json` (authored by the packager) or from a generic
/// fallback; no package name is special-cased in the client.
#[derive(Clone, Debug)]
pub struct PackagePresentation {
    pub display_name: String,
    pub subtitle: String,
    pub description: String,
    pub highlights: Vec<String>,
    pub category: StoreCategory,
    pub icon: Icon,
    pub accent: [u32; 2],
    pub banner: Option<String>,
    pub screenshots: Vec<String>,
    pub is_theme: bool,
    pub featured_rank: i32,
}

#[derive(Clone, Debug)]
pub struct HeroSlide {
    pub overline: String,
    pub title: String,
    pub subtitle: String,
    pub cta_label: String,
    pub target_package: Option<String>,
    pub go_updates: bool,
    pub grad_left: u32,
    pub grad_right: u32,
    pub accent_pill: u32,
    pub icon: Icon,
    pub banner: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FeaturedSection {
    pub title: String,
    pub subtitle: String,
    pub packages: Vec<String>,
}

/// A package is a theme if it says so, provides the theme capability, or is
/// named like one. This is a structural rule, not a per-package table.
pub fn is_theme_package(pkg: &StorePackage) -> bool {
    pkg.name.starts_with("theme-") || pkg.provides.iter().any(|p| p == "spui-theme")
}

pub fn classify_category(pkg: &StorePackage, meta: Option<&StoreMeta>) -> StoreCategory {
    if let Some(meta) = meta {
        match meta.category.as_deref() {
            Some("themes") => return StoreCategory::Themes,
            Some("system") => return StoreCategory::System,
            Some("utilities") | Some("development") | Some("multimedia") => {
                return StoreCategory::Utilities
            }
            _ => {}
        }
    }
    if is_theme_package(pkg) {
        StoreCategory::Themes
    } else if pkg.provides.iter().any(|p| p == "base-core" || p == "spk-runtime") {
        StoreCategory::System
    } else {
        StoreCategory::Utilities
    }
}

pub fn icon_from_name(name: Option<&str>, fallback: Icon) -> Icon {
    match name.unwrap_or_default() {
        "store" => Icon::Store,
        "appearance" => Icon::Appearance,
        "wallpaper" => Icon::Wallpaper,
        "monitor" => Icon::Monitor,
        "settings" => Icon::Settings,
        "login" => Icon::Login,
        "terminal" => Icon::Terminal,
        "applications" => Icon::Applications,
        "browser" => Icon::Browser,
        "files" => Icon::Files,
        "editor" => Icon::Editor,
        "document" => Icon::Document,
        "music" => Icon::Music,
        "calculator" => Icon::Calculator,
        "help" => Icon::Help,
        "network" => Icon::Network,
        "display" => Icon::Display,
        "panel" => Icon::Panel,
        "about" => Icon::About,
        "folder" => Icon::Folder,
        "user" => Icon::UserHome,
        "shield" => Icon::Shield,
        _ => fallback,
    }
}

fn parse_hex_color(text: &str) -> Option<u32> {
    let rest = text.strip_prefix('#')?;
    if rest.len() != 6 {
        return None;
    }
    u32::from_str_radix(rest, 16).ok()
}

fn default_accent(category: StoreCategory) -> [u32; 2] {
    match category {
        StoreCategory::Themes => [0xec4899, 0xbe185d],
        StoreCategory::System => [0x3b82f6, 0x1e40af],
        StoreCategory::Utilities => [0x14b8a6, 0x0f766e],
        StoreCategory::All => [0x64748b, 0x334155],
    }
}

pub fn accent_colors(meta: Option<&StoreMeta>, category: StoreCategory) -> [u32; 2] {
    let mut colors = default_accent(category);
    if let Some(meta) = meta {
        if let Some(top) = meta.accent.first().and_then(|c| parse_hex_color(c)) {
            colors[0] = top;
            colors[1] = meta
                .accent
                .get(1)
                .and_then(|c| parse_hex_color(c))
                .unwrap_or_else(|| shade(colors[0], 0.72));
        }
    }
    colors
}

/// Scale a colour's brightness by `factor`, clamping each RGB channel to 0..=255.
fn shade(color: u32, factor: f32) -> u32 {
    let r = (((color >> 16) & 0xff) as f32 * factor).clamp(0.0, 255.0).round() as u32;
    let g = (((color >> 8) & 0xff) as f32 * factor).clamp(0.0, 255.0).round() as u32;
    let b = ((color & 0xff) as f32 * factor).clamp(0.0, 255.0).round() as u32;
    (r << 16) | (g << 8) | b
}

pub fn present_package(pkg: &StorePackage, meta: Option<&StoreMeta>) -> PackagePresentation {
    let category = classify_category(pkg, meta);
    let is_theme = is_theme_package(pkg);
    let fallback_icon = match category {
        StoreCategory::Themes => Icon::Appearance,
        StoreCategory::System => Icon::Settings,
        _ => Icon::Applications,
    };
    let display_name = meta
        .and_then(|m| m.display_name.clone())
        .unwrap_or_else(|| pkg.name.clone());
    let subtitle = meta
        .and_then(|m| m.subtitle.clone())
        .unwrap_or_else(|| {
            if pkg.summary.is_empty() {
                format!("{} 架构 · {}", pkg.arch, pkg.display_version())
            } else {
                pkg.summary.clone()
            }
        });
    let description = meta
        .and_then(|m| m.description.clone())
        .unwrap_or_else(|| {
            if pkg.summary.is_empty() {
                format!("SpaceOS 软件包 `{}`。", pkg.name)
            } else {
                pkg.summary.clone()
            }
        });
    let highlights = meta
        .map(|m| m.highlights.clone())
        .unwrap_or_else(|| {
            if is_theme {
                vec!["安装后可在系统设置的外观画廊一键启用".to_string()]
            } else {
                vec!["由 Ed25519 签名仓库安全分发".to_string()]
            }
        });
    let banner = meta.and_then(|m| m.banner.clone());
    let mut screenshots = meta.map(|m| m.screenshots.clone()).unwrap_or_default();
    if screenshots.is_empty() {
        if let Some(b) = banner.clone() {
            screenshots.push(b);
        }
    }
    PackagePresentation {
        display_name,
        subtitle,
        description,
        highlights,
        category,
        icon: icon_from_name(meta.and_then(|m| m.icon.as_deref()), fallback_icon),
        accent: accent_colors(meta, category),
        banner,
        screenshots,
        is_theme,
        featured_rank: meta.and_then(|m| m.featured_rank).unwrap_or(10),
    }
}

/// Slides come from the repository's store document. When a slide names a
/// package the client does not have (or the repository publishes no slides at
/// all), the client falls back to a neutral catalogue.
pub fn build_hero_slides(catalog: &CatalogJson) -> Vec<HeroSlide> {
    let mut slides = Vec::new();

    if !catalog.updates.is_empty() {
        let count = catalog.updates.len();
        slides.push(HeroSlide {
            overline: "系统维护 · 待处理更新".into(),
            title: format!("{count} 个软件包有新版本可用"),
            subtitle: "一键完成全量事务升级，升级过程中可随时查看实时进度与日志。".into(),
            cta_label: "立即查看更新".into(),
            target_package: None,
            go_updates: true,
            grad_left: 0x0f172a,
            grad_right: 0x1d4ed8,
            accent_pill: 0x38bdf8,
            icon: Icon::Refresh,
            banner: None,
        });
    }

    if let Some(store) = &catalog.store {
        for slide in &store.hero {
            if slide.title.trim().is_empty() {
                continue;
            }
            let package = match &slide.package {
                Some(name) if catalog.packages.iter().any(|p| &p.name == name) => {
                    Some(name.clone())
                }
                Some(_) => continue,
                None => None,
            };
            let installed = package
                .as_ref()
                .and_then(|name| catalog.packages.iter().find(|p| &p.name == name))
                .is_some_and(|p| p.installed);
            let mut accent = [0x1e293b, 0x2563eb];
            if let Some(top) = slide.accent.first().and_then(|c| parse_hex_color(c)) {
                accent[0] = top;
                accent[1] = slide
                    .accent
                    .get(1)
                    .and_then(|c| parse_hex_color(c))
                    .unwrap_or_else(|| shade(top, 0.72));
            }
            let banner = slide.banner.clone().or_else(|| {
                package
                    .as_deref()
                    .and_then(|name| catalog.meta_for(name))
                    .and_then(|m| m.banner.clone())
            });
            slides.push(HeroSlide {
                overline: slide.overline.clone(),
                title: slide.title.clone(),
                subtitle: slide.subtitle.clone(),
                cta_label: if slide.cta.is_empty() {
                    if installed { "查看详情" } else { "获取" }.into()
                } else {
                    slide.cta.clone()
                },
                target_package: package,
                go_updates: slide.go_updates,
                grad_left: accent[0],
                grad_right: accent[1],
                accent_pill: shade(accent[0], 1.25),
                icon: icon_from_name(slide.icon.as_deref(), Icon::Store),
                banner,
            });
        }
    }

    if slides.is_empty() {
        slides.push(HeroSlide {
            overline: "SpaceOS-9 软件中心".into(),
            title: "安全、可复现的原子软件包管理".into(),
            subtitle: "基于 Ed25519 签名验证与事务回滚日志，轻松管理桌面主题与系统组件。".into(),
            cta_label: "同步软件源".into(),
            target_package: None,
            go_updates: false,
            grad_left: 0x1e293b,
            grad_right: 0x2563eb,
            accent_pill: 0x38bdf8,
            icon: Icon::Store,
            banner: None,
        });
    }

    slides
}

/// Featured sections: the repository's own grouping when present, otherwise
/// the highest-ranked packages of each category.
pub fn build_sections(catalog: &CatalogJson) -> Vec<FeaturedSection> {
    let mut sections = Vec::new();
    if let Some(store) = &catalog.store {
        for section in &store.sections {
            let packages: Vec<String> = section
                .packages
                .iter()
                .filter(|name| catalog.packages.iter().any(|p| &p.name == *name))
                .cloned()
                .collect();
            if !packages.is_empty() {
                sections.push(FeaturedSection {
                    title: section.title.clone(),
                    subtitle: section.subtitle.clone(),
                    packages,
                });
            }
        }
    }
    if sections.is_empty() && !catalog.packages.is_empty() {
        let mut ranked: Vec<&StorePackage> = catalog.packages.iter().collect();
        ranked.sort_by_key(|pkg| {
            std::cmp::Reverse(present_package(pkg, catalog.meta_for(&pkg.name)).featured_rank)
        });
        let packages: Vec<String> = ranked.iter().take(6).map(|p| p.name.clone()).collect();
        sections.push(FeaturedSection {
            title: "推荐软件".into(),
            subtitle: String::new(),
            packages,
        });
    }
    sections
}

pub fn category_label(catalog: &CatalogJson, category: StoreCategory) -> String {
    if let Some(store) = &catalog.store {
        for entry in &store.categories {
            if StoreCategory::from_slug(&entry.slug) == category && !entry.label.is_empty() {
                return entry.label.clone();
            }
        }
    }
    category.label().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, provides: &[&str], summary: &str) -> StorePackage {
        StorePackage {
            name: name.into(),
            provides: provides.iter().map(|s| s.to_string()).collect(),
            summary: summary.into(),
            arch: "x86_64".into(),
            abi: "spaceos-1".into(),
            available_version: Some("1.0.0-1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn metadata_wins_and_fallback_stays_generic() {
        let meta = StoreMeta {
            display_name: Some("自定义名称".into()),
            category: Some("system".into()),
            accent: vec!["#112233".into(), "#445566".into()],
            featured_rank: Some(42),
            ..Default::default()
        };
        let with = present_package(&pkg("foo", &[], "bar"), Some(&meta));
        assert_eq!(with.display_name, "自定义名称");
        assert_eq!(with.category, StoreCategory::System);
        assert_eq!(with.accent, [0x112233, 0x445566]);
        assert_eq!(with.featured_rank, 42);

        let bare = present_package(&pkg("theme-blue", &[], "蓝色主题"), None);
        assert_eq!(bare.display_name, "theme-blue");
        assert_eq!(bare.category, StoreCategory::Themes);
        assert!(bare.is_theme);
        assert!(!bare.highlights.is_empty());
    }

    #[test]
    fn hero_slides_come_from_the_repository() {
        let mut catalog = CatalogJson::default();
        catalog.packages.push(pkg("theme-x", &["spui-theme"], "x"));
        catalog.store = Some(crate::model::StoreDoc {
            hero: vec![
                crate::model::StoreHeroJson {
                    title: "来自仓库".into(),
                    package: Some("theme-x".into()),
                    ..Default::default()
                },
                crate::model::StoreHeroJson {
                    title: "指向不存在的包".into(),
                    package: Some("missing".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let slides = build_hero_slides(&catalog);
        assert_eq!(slides.len(), 1);
        assert_eq!(slides[0].title, "来自仓库");
        assert_eq!(slides[0].target_package.as_deref(), Some("theme-x"));
    }

    #[test]
    fn update_slide_precedes_editorial_slides() {
        let mut catalog = CatalogJson::default();
        catalog.updates.push("hello".into());
        catalog.store = Some(crate::model::StoreDoc {
            hero: vec![crate::model::StoreHeroJson {
                title: "编辑推荐".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let slides = build_hero_slides(&catalog);
        assert_eq!(slides.len(), 2);
        assert!(slides[0].go_updates);
    }

    #[test]
    fn sections_fall_back_to_ranked_packages() {
        let mut catalog = CatalogJson::default();
        catalog.packages.push(pkg("hello", &[], "hi"));
        let sections = build_sections(&catalog);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].packages, vec!["hello".to_string()]);
    }
}
