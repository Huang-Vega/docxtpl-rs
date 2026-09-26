//! Part URIs: validation, normalization, and relationship target resolution.

use std::sync::Arc;

use crate::error::OpcError;

/// A part URI: a normalized ZIP entry name (no leading `/`, `/`-separated).
///
/// Validated at construction, so every entry name entering
/// [`crate::Package`] is guaranteed to:
///
/// - be non-empty and contain no backslash `\`;
/// - not be an absolute path (no leading `/`, no `X:` drive prefix);
/// - contain no `..` segment or a bare `.` segment.
///
/// Empty path segments are allowed (e.g. `a//b`; duplicate detection compares
/// using the `a/b` normalized form), as are directory entry names ending with
/// `/` (e.g. `word/`). Storage keeps the original entry name; percent-encoding
/// is used only for duplicate detection and is never decoded for storage.
///
/// # Failure cases
///
/// Returns [`OpcError::InvalidUri`] when the name violates one of the rules
/// above.
///
/// # Examples
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
    /// Validate and construct a part URI.
    ///
    /// # Failure cases
    ///
    /// Returns [`OpcError::InvalidUri`] on an empty name, a backslash, a
    /// `..`/`.` segment, or an absolute path.
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

    /// The URI string (i.e. the ZIP entry name, kept as-is).
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::PartUri;
    /// assert_eq!(PartUri::new("word/document.xml")?.as_str(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The containing directory; `None` when the part is at the package root
    /// (no directory).
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::PartUri;
    /// let uri = PartUri::new("word/document.xml")?;
    /// assert_eq!(uri.parent().unwrap().as_str(), "word");
    /// assert!(PartUri::new("document.xml")?.parent().is_none());
    /// // The containing directory of a directory entry is the package root
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

    /// The last path segment (the file name of a directory entry `word/` is
    /// `word`).
    ///
    /// # Examples
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

/// Resolve an internal relationship target to a package-internal part URI
/// (public entry point).
///
/// - `base` is the **directory containing** the owner part (the result of
///   [`PartUri::parent`]); `None` means the package root (the root-rels case);
/// - a target starting with `/` is a package-absolute path and ignores `base`;
/// - escaping the package root, an empty target, or an invalid URI returns
///   `None`.
///
/// # Examples
///
/// ```
/// # use docxtpl_opc::{PartUri, resolve_part_target};
/// let doc = PartUri::new("word/document.xml")?;
/// let base = doc.parent();
/// assert_eq!(
///     resolve_part_target(base.as_ref(), "media/image1.png").unwrap().as_str(),
///     "word/media/image1.png"
/// );
/// assert_eq!(
///     resolve_part_target(None, "word/document.xml").unwrap().as_str(),
///     "word/document.xml"
/// );
/// # Ok::<(), docxtpl_opc::OpcError>(())
/// ```
#[must_use]
pub fn resolve_part_target(base: Option<&PartUri>, target: &str) -> Option<PartUri> {
    resolve_relative_to(base, target)
}

/// Relationships file path of a part (public entry point):
/// `word/document.xml` → `word/_rels/document.xml.rels`.
#[must_use]
pub fn relationships_path_of(part: &PartUri) -> String {
    rels_path_for(part)
}

/// Validate that a ZIP entry name is a valid part URI (rules are in the
/// type-level documentation of [`PartUri`]).
pub(crate) fn validate_part_uri(name: &str) -> Result<(), OpcError> {
    let invalid = |reason: &str| OpcError::InvalidUri {
        uri: name.to_string(),
        reason: reason.to_string(),
    };
    if name.is_empty() {
        return Err(invalid("empty entry name"));
    }
    if name.contains('\\') {
        return Err(invalid("contains a backslash"));
    }
    let bytes = name.as_bytes();
    if bytes[0] == b'/' {
        return Err(invalid("absolute path (leading /)"));
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(invalid("absolute path (drive letter X:)"));
    }
    for segment in name.split('/') {
        if segment == ".." {
            return Err(invalid("contains a .. path segment"));
        }
        if segment == "." {
            return Err(invalid("contains a . path segment"));
        }
    }
    Ok(())
}

/// Normalize by collapsing empty path segments (`a//b` → `a/b`). Used only for
/// comparison purposes such as duplicate detection; the stored original entry
/// name is unchanged.
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

/// Simple percent decoding: only valid `%XX` sequences are decoded; all other
/// bytes are kept as-is.
///
/// The decoded result is not guaranteed to be valid UTF-8 (callers convert
/// lossily for comparison; it is never stored).
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

/// Resolve an internal relationship target:
///
/// - a leading `/` → a package-absolute path;
/// - otherwise relative to the directory denoted by `base` (`None` means the
///   package root);
/// - `.` and empty segments collapse, `..` moves up one level;
/// - returns `None` when the target escapes the package root, is empty,
///   contains a backslash, or cannot form a valid URI.
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

/// Relationships file path of a part:
/// `word/document.xml` → `word/_rels/document.xml.rels`.
pub(crate) fn rels_path_for(uri: &PartUri) -> String {
    match uri.parent() {
        Some(dir) => format!("{}/_rels/{}.rels", dir.as_str(), uri.file_name()),
        None => format!("_rels/{}.rels", uri.file_name()),
    }
}

/// Whether the path is a relationships file (parent directory named `_rels`).
pub(crate) fn is_rels_path(uri: &PartUri) -> bool {
    uri.parent().is_some_and(|dir| dir.file_name() == "_rels")
}

/// The part owning a relationships file:
/// `word/_rels/document.xml.rels` → `word/document.xml`.
/// The root relationships file `_rels/.rels` has no owning part; returns
/// `None`.
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

    /// Valid samples (part of the ≥15 good/bad samples; integration tests add
    /// ZIP-level cases).
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

    /// Invalid samples.
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
            PartUri::new(name).unwrap_or_else(|err| panic!("{name} should be valid: {err:?}"));
        }
    }

    #[test]
    fn rejects_bad_uris() {
        for name in BAD {
            let err = PartUri::new(name).expect_err(name);
            assert!(
                matches!(err, OpcError::InvalidUri { .. }),
                "{name} should be InvalidUri, got {err:?}"
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
        // Escapes the package root / invalid / empty
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
        /// Arbitrary UTF-8 strings must not make the validation/normalization/
        /// resolution functions panic (property-only; results are not asserted).
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
