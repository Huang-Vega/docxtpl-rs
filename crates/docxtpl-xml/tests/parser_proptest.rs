//! P7c property tests: arbitrary UTF-8/XML-like input must not panic on
//! either the strict or recover path.

use docxtpl_xml::{XmlDocument, XmlLimits};
use proptest::prelude::*;

const LIMITS: XmlLimits = XmlLimits { max_depth: 64 };

#[test]
fn malformed_bang_with_multibyte_characters_returns_error() {
    for src in ["<!aAῖપ", "<root><!aAῖપ</root>"] {
        assert!(XmlDocument::parse_strict(src, &LIMITS).is_err());
        let _ = XmlDocument::parse_lenient(src, &LIMITS);
    }
}

#[test]
fn recovery_does_not_slice_through_utf8_after_bom() {
    let src = "<f>\n\u{feff}~08<?";
    assert!(XmlDocument::parse_strict(src, &LIMITS).is_err());
    let _ = XmlDocument::parse_lenient(src, &LIMITS);
}

#[test]
fn recovery_rejects_overflowing_numeric_reference_without_panicking() {
    let src = "<R>>#!\n&#70000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000009";
    assert!(XmlDocument::parse_strict(src, &LIMITS).is_err());
    let _ = XmlDocument::parse_lenient(src, &LIMITS);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn parsers_never_panic(src in any::<String>()) {
        let _ = XmlDocument::parse_strict(&src, &LIMITS);
        let _ = XmlDocument::parse_lenient(&src, &LIMITS);
    }

    #[test]
    fn parsers_never_panic_on_xml_like_input(
        chars in proptest::collection::vec(
            proptest::sample::select(vec![
                '<', '>', '/', '?', '!', '[', ']', '&', ';', '=', '"', '\'',
                ':', 'a', 'x', 'm', 'l', 'n', 's', ' ', '\n', '\r', '\t',
            ]),
            0..4096,
        )
    ) {
        let src: String = chars.into_iter().collect();
        let _ = XmlDocument::parse_strict(&src, &LIMITS);
        let _ = XmlDocument::parse_lenient(&src, &LIMITS);
    }
}
