//
//       user/spk/spk-core/tests/fuzz.rs
//       Property tests for version, manifest, index and tar path parsers
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use proptest::prelude::*;
use spk_core::index::Index;
use spk_core::manifest::{validate_absolute_path, validate_relative_path, Manifest};
use spk_core::version::{rpmvercmp, Version};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn version_parse_and_display_roundtrip(value in "[0-9A-Za-z.+~:_-]{0,40}") {
        if let Ok(version) = Version::parse(&value) {
            let rendered = version.to_string();
            let reparsed = Version::parse(&rendered).expect("rendered version must parse");
            prop_assert_eq!(reparsed, version);
        }
    }

    #[test]
    fn rpmvercmp_is_antisymmetric(a in "[0-9A-Za-z._+~-]{0,30}", b in "[0-9A-Za-z._+~-]{0,30}") {
        prop_assert_eq!(rpmvercmp(&a, &b), rpmvercmp(&b, &a).reverse());
    }

    #[test]
    fn rpmvercmp_is_transitive_on_well_formed_versions(
        a in "[0-9A-Za-z][0-9A-Za-z._+~-]{0,14}[0-9A-Za-z]",
        b in "[0-9A-Za-z][0-9A-Za-z._+~-]{0,14}[0-9A-Za-z]",
        c in "[0-9A-Za-z][0-9A-Za-z._+~-]{0,14}[0-9A-Za-z]",
    ) {
        if rpmvercmp(&a, &b) != std::cmp::Ordering::Greater
            && rpmvercmp(&b, &c) != std::cmp::Ordering::Greater
        {
            prop_assert_ne!(rpmvercmp(&a, &c), std::cmp::Ordering::Greater);
        }
    }

    #[test]
    fn manifest_and_index_parsing_never_panic(text in ".{0,512}") {
        let _ = Manifest::from_toml(&text);
        let bytes = text.as_bytes();
        let _ = Index::from_slice(bytes);
    }

    #[test]
    fn path_validators_never_panic(path in ".{0,120}") {
        if validate_relative_path(&path).is_ok() {
            prop_assert!(!path.starts_with('/'));
            prop_assert!(!path.split('/').any(|part| part == ".." || part == "."));
        }
        if validate_absolute_path(&path).is_ok() {
            prop_assert!(path.starts_with('/'));
            prop_assert!(!path.split('/').any(|part| part == ".." || part == "."));
        }
    }
}
