//! Listing: escaped text values (aligned with docxtpl 0.20.2 `listing.py`).

use crate::richtext::escape_html;

/// Escaped plain-text value (upstream `Listing`).
///
/// Performs only the same `html.escape` escaping as upstream; OOXML expansion of control
/// characters such as newlines (`<w:br/>`, etc.) is done by the rendering pipeline's
/// resolve_listing stage (see docxtpl-template / docxtpl-compat).
///
/// # Examples
///
/// ```
/// let l = docxtpl_rich::Listing::new("a&b<c>");
/// assert_eq!(l.to_xml(), "a&amp;b&lt;c&gt;");
/// ```
#[derive(Debug, Clone, Default)]
pub struct Listing {
    /// Escaped XML text.
    xml: String,
}

impl Listing {
    /// Constructs a value by escaping the text exactly as upstream `escape(text)` does.
    ///
    /// Upstream calls `str()` on non-string inputs first; on the Rust side the caller formats
    /// inputs into a string.
    pub fn new(text: &str) -> Self {
        Self {
            xml: escape_html(text),
        }
    }

    /// Escaped XML text.
    pub fn to_xml(&self) -> &str {
        &self.xml
    }
}

#[cfg(test)]
mod tests {
    use super::Listing;

    #[test]
    fn newline_preserved_as_is() {
        // Upstream Listing does not handle \n (resolve_listing expands it); keep it as-is
        assert_eq!(Listing::new("a\nb").to_xml(), "a\nb");
    }

    #[test]
    fn special_chars_escaped() {
        assert_eq!(Listing::new("a&b<c>").to_xml(), "a&amp;b&lt;c&gt;");
    }

    #[test]
    fn quotes_escaped_aligned_with_upstream() {
        // Upstream escape() is called without quote=False (verified by probe)
        assert_eq!(Listing::new("a\"b'c").to_xml(), "a&quot;b&#x27;c");
    }

    #[test]
    fn empty_text() {
        assert_eq!(Listing::new("").to_xml(), "");
    }
}
