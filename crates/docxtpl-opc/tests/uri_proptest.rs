//! proptest：随机 UTF-8 字符串过 URI 校验/规范化不 panic（只测性质，不断言结果）。

use docxtpl_opc::PartUri;
use proptest::prelude::*;

proptest! {
    /// 任意 UTF-8 字符串过 PartUri::new 及派生方法都不 panic。
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

    /// 由路径相关字符组成的长随机串同样不 panic。
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
