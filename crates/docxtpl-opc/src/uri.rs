//! part URI：校验、规范化与关系目标解析。

use std::sync::Arc;

use crate::error::OpcError;

/// part URI：规范化的 ZIP 条目名（无前导 `/`，以 `/` 分隔）。
///
/// 构造时即校验，进入 [`crate::Package`] 的所有条目名都保证满足：
///
/// - 非空，且不含反斜杠 `\`；
/// - 不是绝对路径（无前导 `/`，无 `X:` 盘符前缀）；
/// - 不含 `..` 段或单独的 `.` 段。
///
/// 允许空的路径段（如 `a//b`，重复检测按 `a/b` 规范化口径比较）与以 `/`
/// 结尾的目录条目名（如 `word/`）。存储保留原始条目名；百分号编码只用于
/// 重复检测，不解码存储。
///
/// # 失败情况
///
/// [`OpcError::InvalidUri`]：名称违反上述规则。
///
/// # 示例
///
/// ```
/// use docxtpl_opc::PartUri;
///
/// let uri = PartUri::new("word/document.xml")?;
/// assert_eq!(uri.as_str(), "word/document.xml");
/// assert_eq!(uri.file_name(), "document.xml");
/// assert!(PartUri::new("../evil.txt").is_err());
/// # Ok::<(), docxtpl_opc::OpcError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PartUri(Arc<str>);

impl PartUri {
    /// 校验并构造 part URI。
    ///
    /// # 失败情况
    ///
    /// [`OpcError::InvalidUri`]：空名、含反斜杠、`..`/`.` 段或绝对路径。
    ///
    /// ```
    /// # use docxtpl_opc::PartUri;
    /// assert!(PartUri::new("customXml/item1.xml").is_ok());
    /// assert!(PartUri::new("C:\\x").is_err());
    /// ```
    pub fn new(name: &str) -> Result<Self, OpcError> {
        validate_part_uri(name)?;
        Ok(Self(Arc::from(name)))
    }

    /// URI 字符串（即 ZIP 条目名，原样保留）。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::PartUri;
    /// assert_eq!(PartUri::new("word/document.xml")?.as_str(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 所在目录；位于包根（无目录）时返回 `None`。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::PartUri;
    /// let uri = PartUri::new("word/document.xml")?;
    /// assert_eq!(uri.parent().unwrap().as_str(), "word");
    /// assert!(PartUri::new("document.xml")?.parent().is_none());
    /// // 目录条目的所在目录是包根
    /// assert!(PartUri::new("word/")?.parent().is_none());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn parent(&self) -> Option<PartUri> {
        let mut segments: Vec<&str> = self.segments().collect();
        segments.pop()?;
        if segments.is_empty() {
            return None;
        }
        Some(PartUri(Arc::from(segments.join("/"))))
    }

    /// 最后一个路径段（目录条目 `word/` 的文件名为 `word`）。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::PartUri;
    /// assert_eq!(PartUri::new("word/document.xml")?.file_name(), "document.xml");
    /// assert_eq!(PartUri::new("word/")?.file_name(), "word");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn file_name(&self) -> &str {
        self.segments().next_back().unwrap_or_default()
    }

    fn segments(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.0.split('/').filter(|segment| !segment.is_empty())
    }
}

/// 校验 ZIP 条目名是否为合法 part URI（规则见 [`PartUri`] 类型级文档）。
pub(crate) fn validate_part_uri(name: &str) -> Result<(), OpcError> {
    let invalid = |reason: &str| OpcError::InvalidUri {
        uri: name.to_string(),
        reason: reason.to_string(),
    };
    if name.is_empty() {
        return Err(invalid("空条目名"));
    }
    if name.contains('\\') {
        return Err(invalid("包含反斜杠"));
    }
    let bytes = name.as_bytes();
    if bytes[0] == b'/' {
        return Err(invalid("绝对路径（前导 /）"));
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(invalid("绝对路径（盘符 X:）"));
    }
    for segment in name.split('/') {
        if segment == ".." {
            return Err(invalid("包含 .. 路径段"));
        }
        if segment == "." {
            return Err(invalid("包含 . 路径段"));
        }
    }
    Ok(())
}

/// 规范化：折叠空路径段（`a//b` → `a/b`）。仅用于重复检测等比较口径，
/// 不改变存储的原始条目名。
pub(crate) fn collapse_segments(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for segment in name.split('/').filter(|segment| !segment.is_empty()) {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(segment);
    }
    out
}

/// 简易百分号解码：仅解码合法的 `%XX` 序列，其余字节原样保留。
///
/// 解码结果不保证是合法 UTF-8（调用方用 lossy 转换做比较，不用于存储）。
pub(crate) fn percent_decode(name: &str) -> Vec<u8> {
    fn hex_value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// 解析内部关系目标：
///
/// - 以 `/` 开头 → 包内绝对路径；
/// - 否则相对 `base` 所指目录（`None` 表示包根）；
/// - `.` 与空段折叠，`..` 上溯一级；
/// - 逃出包根、目标为空、含反斜杠或无法构成合法 URI 时返回 `None`。
pub(crate) fn resolve_relative_to(base: Option<&PartUri>, target: &str) -> Option<PartUri> {
    if target.is_empty() || target.contains('\\') {
        return None;
    }
    let mut stack: Vec<&str> = Vec::new();
    if !target.starts_with('/') {
        if let Some(base) = base {
            stack.extend(
                base.as_str()
                    .split('/')
                    .filter(|segment| !segment.is_empty()),
            );
        }
    }
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            name => stack.push(name),
        }
    }
    if stack.is_empty() {
        return None;
    }
    PartUri::new(&stack.join("/")).ok()
}

/// part 的关系文件路径：`word/document.xml` → `word/_rels/document.xml.rels`。
pub(crate) fn rels_path_for(uri: &PartUri) -> String {
    match uri.parent() {
        Some(dir) => format!("{}/_rels/{}.rels", dir.as_str(), uri.file_name()),
        None => format!("_rels/{}.rels", uri.file_name()),
    }
}

/// 是否是关系文件（父目录名为 `_rels`）。
pub(crate) fn is_rels_path(uri: &PartUri) -> bool {
    uri.parent().is_some_and(|dir| dir.file_name() == "_rels")
}

/// 关系文件归属的 part：`word/_rels/document.xml.rels` → `word/document.xml`。
/// 根关系文件 `_rels/.rels` 没有归属 part，返回 `None`。
pub(crate) fn owner_of_rels_path(uri: &PartUri) -> Option<String> {
    let dir = uri.parent()?;
    if dir.file_name() != "_rels" {
        return None;
    }
    let owned = uri.file_name().strip_suffix(".rels")?;
    if owned.is_empty() {
        return None;
    }
    match dir.parent() {
        Some(parent) => Some(format!("{}/{}", parent.as_str(), owned)),
        None => Some(owned.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// 好样例（≥15 组好/坏样例之一部分；集成测试补充 ZIP 层面的用例）。
    const GOOD: &[&str] = &[
        "word/document.xml",
        "a/b.xml",
        "customXml/item1.xml",
        "a//b",
        "word/",
        "[Content_Types].xml",
        "_rels/.rels",
        "docProps/core.xml",
        "media/image1.png",
        "x",
        "a/b:c",
        "1:x",
    ];

    /// 坏样例。
    const BAD: &[&str] = &[
        "",
        "../x",
        "a/../b",
        "..",
        "a\\b",
        "a/../..\\x",
        "C:\\x",
        "C:x",
        "/abs",
        "a/./b",
        "./a",
        "a/.",
    ];

    #[test]
    fn accepts_good_uris() {
        for name in GOOD {
            PartUri::new(name).unwrap_or_else(|err| panic!("{name} 应合法: {err:?}"));
        }
    }

    #[test]
    fn rejects_bad_uris() {
        for name in BAD {
            let err = PartUri::new(name).expect_err(name);
            assert!(
                matches!(err, OpcError::InvalidUri { .. }),
                "{name} 应为 InvalidUri，实际 {err:?}"
            );
        }
    }

    #[test]
    fn parent_and_file_name() {
        let uri = PartUri::new("word/document.xml").unwrap();
        assert_eq!(uri.parent().unwrap().as_str(), "word");
        assert_eq!(uri.file_name(), "document.xml");
        assert!(PartUri::new("document.xml").unwrap().parent().is_none());

        let dir = PartUri::new("word/").unwrap();
        assert!(dir.parent().is_none());
        assert_eq!(dir.file_name(), "word");

        let weird = PartUri::new("a//b.xml").unwrap();
        assert_eq!(weird.parent().unwrap().as_str(), "a");
        assert_eq!(weird.file_name(), "b.xml");

        let deep = PartUri::new("a/b/c.xml").unwrap();
        assert_eq!(deep.parent().unwrap().as_str(), "a/b");
    }

    #[test]
    fn collapsing_segments() {
        assert_eq!(collapse_segments("a//b"), "a/b");
        assert_eq!(collapse_segments("word/"), "word");
        assert_eq!(collapse_segments("a/b"), "a/b");
        assert_eq!(collapse_segments(""), "");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b"), b"a b".to_vec());
        assert_eq!(
            percent_decode("word%2Fdocument.xml"),
            b"word/document.xml".to_vec()
        );
        assert_eq!(
            percent_decode("word%2fdocument.xml"),
            b"word/document.xml".to_vec()
        );
        assert_eq!(percent_decode("100%"), b"100%".to_vec());
        assert_eq!(percent_decode("%zz"), b"%zz".to_vec());
        assert_eq!(percent_decode("%2"), b"%2".to_vec());
        assert_eq!(percent_decode("a%2Gx"), b"a%2Gx".to_vec());
    }

    #[test]
    fn resolving_relative_targets() {
        let word = PartUri::new("word/document.xml").unwrap().parent().unwrap();
        assert_eq!(
            resolve_relative_to(Some(&word), "styles.xml")
                .unwrap()
                .as_str(),
            "word/styles.xml"
        );
        assert_eq!(
            resolve_relative_to(Some(&word), "../customXml/item1.xml")
                .unwrap()
                .as_str(),
            "customXml/item1.xml"
        );
        assert_eq!(
            resolve_relative_to(Some(&word), "sub/../../root.xml")
                .unwrap()
                .as_str(),
            "root.xml"
        );
        assert_eq!(
            resolve_relative_to(None, "/word/document.xml")
                .unwrap()
                .as_str(),
            "word/document.xml"
        );
        assert_eq!(
            resolve_relative_to(None, "word/document.xml")
                .unwrap()
                .as_str(),
            "word/document.xml"
        );
        // 逃出包根 / 非法 / 空
        assert!(resolve_relative_to(Some(&word), "../../escape.xml").is_none());
        assert!(resolve_relative_to(Some(&word), "a\\b").is_none());
        assert!(resolve_relative_to(Some(&word), "").is_none());
        assert!(resolve_relative_to(Some(&word), "/").is_none());
    }

    #[test]
    fn rels_paths_and_owners() {
        let doc = PartUri::new("word/document.xml").unwrap();
        assert_eq!(rels_path_for(&doc), "word/_rels/document.xml.rels");
        assert_eq!(
            rels_path_for(&PartUri::new("custom.xml").unwrap()),
            "_rels/custom.xml.rels"
        );

        let doc_rels = PartUri::new("word/_rels/document.xml.rels").unwrap();
        assert_eq!(
            owner_of_rels_path(&doc_rels).as_deref(),
            Some("word/document.xml")
        );
        assert_eq!(
            owner_of_rels_path(&PartUri::new("_rels/.rels").unwrap()),
            None
        );
        assert_eq!(
            owner_of_rels_path(&PartUri::new("_rels/custom.xml.rels").unwrap()).as_deref(),
            Some("custom.xml")
        );

        assert!(is_rels_path(&PartUri::new("_rels/.rels").unwrap()));
        assert!(is_rels_path(&doc_rels));
        assert!(!is_rels_path(&doc));
    }

    proptest! {
        /// 任意 UTF-8 字符串过校验/规范化/解析函数都不 panic（只测性质，不断言结果）。
        #[test]
        fn uri_helpers_never_panics(s in any::<String>()) {
            let _ = validate_part_uri(&s);
            let _ = collapse_segments(&s);
            let _ = percent_decode(&s);
            let _ = resolve_relative_to(None, &s);
            if let Ok(uri) = PartUri::new(&s) {
                let _ = uri.parent();
                let _ = uri.file_name();
                let _ = uri.as_str();
                let _ = resolve_relative_to(uri.parent().as_ref(), &s);
            }
        }
    }
}
