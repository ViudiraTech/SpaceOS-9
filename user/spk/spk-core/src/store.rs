//
//       user/spk/spk-core/src/store.rs
//       Repository store metadata: per-package presentation and editorial picks
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::manifest::PackageStoreMeta;
use crate::{Error, Result};

pub const STORE_SCHEMA: u32 = 1;

/// `store.json`: the merchandising document a repository publishes next to its
/// index. Package entries are generated from each package's `[store]` table;
/// `hero`/`sections`/`categories` come from the repository's authored
/// `store.in.json`. The whole document is pinned by `repo.json`, so it needs no
/// signature of its own.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct StoreDoc {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub packages: BTreeMap<String, PackageStoreMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hero: Vec<StoreHero>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<StoreSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<StoreCategory>,
}

fn default_schema() -> u32 {
    STORE_SCHEMA
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct StoreHero {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub overline: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cta: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub go_updates: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accent: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct StoreSection {
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct StoreCategory {
    pub slug: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl StoreDoc {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let doc: StoreDoc = serde_json::from_slice(bytes)?;
        doc.validate()?;
        Ok(doc)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != STORE_SCHEMA {
            return Err(Error::Index(format!(
                "unsupported store schema {}",
                self.schema
            )));
        }
        for (name, meta) in &self.packages {
            meta.validate(name)?;
        }
        if self.hero.len() > 8 {
            return Err(Error::Index("store may define at most 8 hero slides".into()));
        }
        for slide in &self.hero {
            if slide.title.trim().is_empty() || slide.title.chars().count() > 80 {
                return Err(Error::Index(
                    "store hero title must be 1..=80 characters".into(),
                ));
            }
            if slide.accent.len() > 2 || slide.accent.iter().any(|color| !is_hex_color(color)) {
                return Err(Error::Index(
                    "store hero accent must be one or two #rrggbb colours".into(),
                ));
            }
            if let Some(package) = &slide.package {
                crate::manifest::validate_package_name(package)?;
            }
        }
        let mut section_titles = BTreeSet::new();
        for section in &self.sections {
            if section.title.trim().is_empty() || !section_titles.insert(section.title.clone()) {
                return Err(Error::Index(
                    "store sections need a unique non-empty title".into(),
                ));
            }
            if section.packages.len() > 24 {
                return Err(Error::Index(format!(
                    "store section '{}' lists too many packages",
                    section.title
                )));
            }
            for package in &section.packages {
                crate::manifest::validate_package_name(package)?;
            }
        }
        let mut slugs = BTreeSet::new();
        for category in &self.categories {
            if !slugs.insert(category.slug.clone()) {
                return Err(Error::Index(format!(
                    "duplicate store category '{}'",
                    category.slug
                )));
            }
            if category.label.trim().is_empty() {
                return Err(Error::Index(format!(
                    "store category '{}' needs a label",
                    category.slug
                )));
            }
        }
        Ok(())
    }
}

fn is_hex_color(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('#') else {
        return false;
    };
    rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let doc = StoreDoc {
            schema: STORE_SCHEMA,
            packages: BTreeMap::from([(
                "theme-aqua".into(),
                PackageStoreMeta {
                    display_name: Some("Aqua Classic".into()),
                    category: Some("themes".into()),
                    accent: vec!["#38bdf8".into(), "#0284c7".into()],
                    ..Default::default()
                },
            )]),
            hero: vec![StoreHero {
                title: "Aqua".into(),
                package: Some("theme-aqua".into()),
                go_updates: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = doc.to_bytes().unwrap();
        let parsed = StoreDoc::from_slice(&bytes).unwrap();
        assert_eq!(parsed.packages["theme-aqua"].display_name.as_deref(), Some("Aqua Classic"));
        assert!(!bytes.windows(11).any(|w| w == b"\"go_updates\""));
    }

    #[test]
    fn rejects_bad_colours_and_duplicate_categories() {
        let mut doc = StoreDoc::default();
        doc.hero.push(StoreHero {
            title: "x".into(),
            accent: vec!["blue".into()],
            ..Default::default()
        });
        assert!(doc.validate().is_err());

        let mut doc = StoreDoc::default();
        doc.categories.push(StoreCategory {
            slug: "themes".into(),
            label: "主题".into(),
            subtitle: String::new(),
        });
        doc.categories.push(StoreCategory {
            slug: "themes".into(),
            label: "主题".into(),
            subtitle: String::new(),
        });
        assert!(doc.validate().is_err());
    }
}
