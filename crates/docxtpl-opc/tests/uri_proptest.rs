//! proptest: random UTF-8 strings passing through URI validation/normalization
//! must not panic (only the property is tested; results are not asserted).

use docxtpl_opc::PartUri;
use proptest::prelude::*;

proptest! {
    /// Any UTF-8 string passing through PartUri::new and derived methods must
    /// not panic.
    #[test]
    fn part_uri_never_panics(name in any::<String>()) {
        if let Ok(uri) = PartUri::new(&name) {
            let _ = uri.as_str();
            let _ = uri.file_name();
            if let Some(parent) = uri.parent() {
                let _ = parent.as_str();
                let _ = parent.file_name();
            }
        }
    }

    /// Long random strings made of path-related characters must also not
    /// panic.
    #[test]
    fn part_uri_never_panics_on_pathlike(
        chars in proptest::collection::vec(
            proptest::sample::select(vec!['/', '.', '%', ':', '\\', 'a', 'B', '2', 'F', 'x']),
            0..128,
        )
    ) {
        let name: String = chars.into_iter().collect();
        if let Ok(uri) = PartUri::new(&name) {
            let _ = uri.parent();
            let _ = uri.file_name();
        }
    }
}
