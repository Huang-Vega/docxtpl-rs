//! P7c 属性测试：任意 UTF-8/XML-like 输入在严格和 recover 路径均不得 panic。

use docxtpl_xml::{XmlDocument, XmlLimits};
use proptest::prelude::*;

const LIMITS: XmlLimits = XmlLimits { max_depth: 64 };

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
