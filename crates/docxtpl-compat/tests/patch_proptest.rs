//! P7c 属性测试：畸形/任意模板片段经过兼容补丁不得 panic。

use docxtpl_compat::{patch_xml, resolve_listing};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn patch_xml_never_panics(src in any::<String>()) {
        let patched = patch_xml(&src);
        let _ = resolve_listing(&patched);
    }

    #[test]
    fn patch_xml_handles_marker_heavy_inputs(
        chars in proptest::collection::vec(
            proptest::sample::select(vec![
                '<', '>', '/', '{', '}', '%', '#', '_', '"', '\'', '=',
                'w', ':', 't', 'r', 'p', ' ', '\n', '\r', '&', ';',
            ]),
            0..1024,
        )
    ) {
        let src: String = chars.into_iter().collect();
        let patched = patch_xml(&src);
        let _ = resolve_listing(&patched);
    }
}
