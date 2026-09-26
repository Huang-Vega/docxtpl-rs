//! Small helpers for quick-xml attribute reading and hand-written XML serialization.

use quick_xml::{events::BytesStart, XmlVersion};

/// Declaration style of lxml `etree.tostring(element, encoding="UTF-8", standalone=True)`:
/// single-quoted attributes, UTF-8, standalone, and one trailing newline.
///
/// python-docx rebuilds `[Content_Types].xml` and `.rels` with lxml when saving
/// a package; byte-for-byte oracle alignment must reuse the same declaration.
pub(crate) const LXML_XML_DECLARATION: &str =
    "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n";

/// Escape an XML attribute value (matching lxml's default serialization:
/// the four entities `&`/`<`/`>`/`"`).
pub(crate) fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Read and decode an optional XML attribute.
///
/// On failure returns an error description string that the caller wraps into
/// the concrete [`crate::OpcError`] variant.
pub(crate) fn attr_value(element: &BytesStart<'_>, name: &str) -> Result<Option<String>, String> {
    match element.try_get_attribute(name) {
        Err(err) => Err(format!("failed to read attribute {name}: {err}")),
        Ok(None) => Ok(None),
        Ok(Some(attribute)) => match attribute.normalized_value(XmlVersion::Implicit1_0) {
            Ok(value) => Ok(Some(value.into_owned())),
            Err(err) => Err(format!("failed to decode attribute {name}: {err}")),
        },
    }
}

/// Read and decode a required XML attribute.
pub(crate) fn required_attr(element: &BytesStart<'_>, name: &str) -> Result<String, String> {
    attr_value(element, name)?.ok_or_else(|| format!("missing required attribute {name}"))
}
