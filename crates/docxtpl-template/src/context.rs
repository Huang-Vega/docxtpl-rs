//! P4 rich-content render context (ADR-005).
//!
//! [`RenderContext`] is an ordered table of top-level variables whose values may be:
//!
//! - Plain JSON ([`serde_json::Value]): fully isomorphic with the P2/P3 JSON path;
//! - Rich content: [`RichText`] / [`RichTextParagraph`] / [`Listing`] /
//!   [`InlineImage`] (docxtpl-rich); Subdoc fragments (P6, ADR-007; the
//!   construction entry point is in docxtpl-rs);
//! - Nested arrays / objects (supporting rich values inside `{% for row in rows %}`).
//!
//! Rich text/listings participate in MiniJinja rendering directly via their
//! `to_xml()` strings (aligned with upstream `RichText.__str__` /
//! `Listing.__str__`); images are first resolved to relationship IDs through
//! [`ImageRegistry`], and then produce `wp:inline` XML.
//!
//! Note: images are **lazily resolved** following the upstream
//! `InlineImage.__str__` semantics (P5/ADR-006 revises the P4 eager scheme):
//! only placeholders are written during conversion, and after rendering they
//! are resolved via [`ImageRegistry`] in the order the placeholders appear in
//! the output. Therefore images not referenced by a part's template produce no
//! relationships in that part (the key to multi-part scoping correctness), and
//! broken images not referenced by any template do not raise an error (matching
//! upstream behavior).

use docxtpl_rich::{InlineImage, Listing, RichText, RichTextParagraph};
use serde_json::Value as JsonValue;
use std::fmt;

/// The top-level JSON render context is not an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonContextError;

impl fmt::Display for JsonContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("top-level JSON render context must be an object")
    }
}

impl std::error::Error for JsonContextError {}

/// A validated Subdoc XML fragment.
///
/// On construction the fragment is placed under a temporary root carrying the
/// common WordprocessingML namespaces and parsed as strict XML; DTDs,
/// declarations, multi-root non-well-formed content, and unbound prefixes are
/// rejected. This remains a low-level entry point; regular callers should use
/// `docxtpl_rs::RenderSession::new_subdoc()` to merge parts.
#[derive(Debug, Clone)]
pub struct SubdocFragment(String);

impl SubdocFragment {
    /// Validates and constructs a fragment.
    pub fn parse(fragment: impl Into<String>) -> Result<Self, docxtpl_xml::XmlError> {
        let namespaces = [
            ("w".to_string(), docxtpl_xml::ns_uri::W.to_string()),
            ("r".to_string(), docxtpl_xml::ns_uri::R.to_string()),
            ("a".to_string(), docxtpl_xml::ns_uri::A.to_string()),
            ("pic".to_string(), docxtpl_xml::ns_uri::PIC.to_string()),
            ("wp".to_string(), docxtpl_xml::ns_uri::WP.to_string()),
        ];
        Self::parse_in_namespace_context(fragment, &namespaces)
    }

    /// Validates and constructs a fragment within the given in-scope namespaces.
    ///
    /// After the Subdoc body opening tag is stripped, the fragment itself no
    /// longer carries the `xmlns` declarations inherited from the document
    /// root; the merge path should therefore pass the bindings actually visible
    /// at the insertion point in the target main document. When the same prefix
    /// appears multiple times, the last entry wins (consistent with XML inner
    /// declarations shadowing outer ones).
    pub fn parse_in_namespace_context(
        fragment: impl Into<String>,
        namespaces: &[(String, String)],
    ) -> Result<Self, docxtpl_xml::XmlError> {
        let fragment = fragment.into();
        let mut effective: Vec<(&str, &str)> = Vec::new();
        for (prefix, uri) in namespaces {
            if prefix == "xml" {
                // `xml` is a built-in XML-spec binding and neither needs nor
                // should be redeclared.
                continue;
            }
            if let Some(existing) = effective.iter_mut().find(|(p, _)| *p == prefix) {
                existing.1 = uri;
            } else {
                effective.push((prefix, uri));
            }
        }

        let mut wrapped = String::from("<body");
        for (prefix, uri) in effective {
            if prefix.is_empty() {
                wrapped.push_str(" xmlns=\"");
            } else {
                wrapped.push_str(" xmlns:");
                wrapped.push_str(prefix);
                wrapped.push_str("=\"");
            }
            escape_xml_attribute(uri, &mut wrapped);
            wrapped.push('"');
        }
        wrapped.push('>');
        wrapped.push_str(&fragment);
        wrapped.push_str("</body>");
        docxtpl_xml::XmlDocument::parse_strict(&wrapped, &docxtpl_xml::XmlLimits::default())?;
        Ok(Self(fragment))
    }

    /// The validated raw XML.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn escape_xml_attribute(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            other => out.push(other),
        }
    }
}

/// Relationship IDs resolved for an image (assigned by [`ImageRegistry`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRels {
    /// Image relationship ID for `a:blip r:embed`.
    pub blip_rid: String,
    /// Anchor external-hyperlink relationship ID; `None` when there is no anchor.
    pub hyperlink_rid: Option<String>,
}

/// Image registry: resolves an [`InlineImage`] to relationship IDs (ADR-005).
///
/// The template crate does not depend on the OPC package layer; the concrete
/// implementation (part naming, sha1 deduplication, rId-hole backfill,
/// external-link reuse) lives in docxtpl-rs. Resolving the same image multiple
/// times must be idempotent (aligned with upstream `get_or_add_image` /
/// `get_or_add_ext_rel`).
pub trait ImageRegistry {
    /// Resolves an image: probes headers/converts sizes/allocates parts and
    /// relationships.
    ///
    /// Returns [`ImageResolveError`] when the image bytes are unrecognized;
    /// the rendering pipeline merges it into `TemplateErrorKind::Image`
    /// (oracle exception `UnrecognizedImageError`).
    fn resolve_image(&mut self, image: &InlineImage) -> Result<ImageRels, ImageResolveError>;
}

/// Image registration failure: carries only a message (package details belong
/// to the upper layer).
#[derive(Debug, Clone)]
pub struct ImageResolveError {
    /// Failure reason (e.g. the display text of docxtpl-rich's `ImageError`).
    pub message: String,
}

impl std::fmt::Display for ImageResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ImageResolveError {}

/// Empty registry: images cannot occur on the plain-JSON context path, so any
/// call fails (defensive).
pub struct NullRegistry;

impl ImageRegistry for NullRegistry {
    fn resolve_image(&mut self, _image: &InlineImage) -> Result<ImageRels, ImageResolveError> {
        Err(ImageResolveError {
            message: "InlineImage is not supported in a JSON context; use render_ctx with a rich-content context".to_string(),
        })
    }
}

/// A value in the render context (JSON or rich content; nestable).
#[derive(Debug, Clone)]
pub enum RenderValue {
    /// Plain JSON value (string/number/bool/null/array/object).
    Json(JsonValue),
    /// A sequence of rich-text runs.
    RichText(RichText),
    /// A standalone rich-text paragraph.
    RichTextParagraph(RichTextParagraph),
    /// Escaped plain text (control characters are expanded by the pipeline's resolve_listing).
    Listing(Listing),
    /// Inline image.
    Image(InlineImage),
    /// Subdoc fragment (P6, ADR-007): the XML fragment string assembled from
    /// the body children of an external docx after its parts are merged into
    /// the main package (aligned with upstream `Subdoc.__str__`).
    ///
    /// The fragment contains no namespace declarations (they are lost when
    /// upstream strips the body tag); prefix bindings fall back to the main
    /// document root element's declarations. The construction entry point is
    /// `RenderSession::new_subdoc` in docxtpl-rs.
    Subdoc(SubdocFragment),
    /// Ordered array.
    Array(Vec<RenderValue>),
    /// Ordered object (key-value pairs keep their order).
    Object(Vec<(String, RenderValue)>),
}

impl RenderValue {
    /// Constructs an ordered object value.
    #[must_use]
    pub fn object(entries: Vec<(String, RenderValue)>) -> Self {
        Self::Object(entries)
    }

    /// Constructs an ordered array value.
    #[must_use]
    pub fn array(items: Vec<RenderValue>) -> Self {
        Self::Array(items)
    }
}

impl From<JsonValue> for RenderValue {
    fn from(value: JsonValue) -> Self {
        Self::Json(value)
    }
}

impl From<&str> for RenderValue {
    fn from(value: &str) -> Self {
        Self::Json(JsonValue::String(value.to_owned()))
    }
}

impl From<String> for RenderValue {
    fn from(value: String) -> Self {
        Self::Json(JsonValue::String(value))
    }
}

impl From<bool> for RenderValue {
    fn from(value: bool) -> Self {
        Self::Json(JsonValue::Bool(value))
    }
}

impl From<i64> for RenderValue {
    fn from(value: i64) -> Self {
        Self::Json(JsonValue::from(value))
    }
}

impl From<f64> for RenderValue {
    fn from(value: f64) -> Self {
        Self::Json(JsonValue::from(value))
    }
}

impl From<RichText> for RenderValue {
    fn from(value: RichText) -> Self {
        Self::RichText(value)
    }
}

impl From<RichTextParagraph> for RenderValue {
    fn from(value: RichTextParagraph) -> Self {
        Self::RichTextParagraph(value)
    }
}

impl From<Listing> for RenderValue {
    fn from(value: Listing) -> Self {
        Self::Listing(value)
    }
}

impl From<InlineImage> for RenderValue {
    fn from(value: InlineImage) -> Self {
        Self::Image(value)
    }
}

impl From<Vec<RenderValue>> for RenderValue {
    fn from(value: Vec<RenderValue>) -> Self {
        Self::Array(value)
    }
}

/// Top-level render context: an ordered key-value table (matching Python
/// `dict` insertion order).
#[derive(Debug, Clone, Default)]
pub struct RenderContext {
    entries: Vec<(String, RenderValue)>,
}

impl RenderContext {
    /// Empty context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts/overwrites a top-level variable (chainable).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<RenderValue>) -> &mut Self {
        let key = key.into();
        let value = value.into();
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
        self
    }

    /// Number of top-level variables.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the context is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Reads a value by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&RenderValue> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Iterates in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &RenderValue)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Constructs from JSON; for compatibility with the existing Rust API, a
    /// non-object value yields an empty context.
    ///
    /// New code handling untrusted JSON should prefer [`Self::try_from_json`]
    /// to avoid silently interpreting non-object input as an empty context.
    /// This entry point preserves the existing API behavior.
    #[must_use]
    pub fn from_json(value: &JsonValue) -> Self {
        let mut ctx = Self::new();
        if let Some(map) = value.as_object() {
            for (k, v) in map {
                ctx.entries.push((k.clone(), json_to_value(v)));
            }
        }
        ctx
    }

    /// Constructs from a top-level JSON object; arrays, scalars, and null
    /// produce an explicit error.
    pub fn try_from_json(value: &JsonValue) -> Result<Self, JsonContextError> {
        if !value.is_object() {
            return Err(JsonContextError);
        }
        Ok(Self::from_json(value))
    }
}

/// Recursively maps a JSON value to a [`RenderValue`] (objects/arrays keep order).
fn json_to_value(value: &JsonValue) -> RenderValue {
    match value {
        JsonValue::Array(items) => RenderValue::Array(items.iter().map(json_to_value).collect()),
        JsonValue::Object(map) => RenderValue::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        ),
        other => RenderValue::Json(other.clone()),
    }
}

#[cfg(test)]
mod subdoc_fragment_tests {
    use super::{RenderContext, RenderValue, SubdocFragment};

    #[test]
    fn accepts_well_formed_word_fragment() {
        let fragment = SubdocFragment::parse("<w:p><w:r><w:t>ok</w:t></w:r></w:p>")
            .expect("well-formed Word fragment");
        assert_eq!(fragment.as_str(), "<w:p><w:r><w:t>ok</w:t></w:r></w:p>");
    }

    #[test]
    fn rejects_malformed_or_dtd_fragment() {
        assert!(SubdocFragment::parse("<w:p>").is_err());
        assert!(SubdocFragment::parse("<!DOCTYPE x><w:p/>").is_err());
        assert!(SubdocFragment::parse("<unknown:p/>").is_err());
    }

    #[test]
    fn accepts_fragment_using_target_document_namespace_context() {
        let namespaces = vec![
            ("w".to_string(), docxtpl_xml::ns_uri::W.to_string()),
            ("m".to_string(), docxtpl_xml::ns_uri::M.to_string()),
            ("w14".to_string(), docxtpl_xml::ns_uri::W14.to_string()),
        ];
        let xml = "<w:p w14:paraId=\"00000001\"><m:oMath><m:r/></m:oMath></w:p>";
        let fragment = SubdocFragment::parse_in_namespace_context(xml, &namespaces).expect(
            "extension prefixes declared by the main document should be usable in Subdoc fragments",
        );
        assert_eq!(fragment.as_str(), xml);
    }

    #[test]
    fn namespace_context_rejects_unbound_prefix_and_escapes_uri() {
        let namespaces = vec![
            ("w".to_string(), docxtpl_xml::ns_uri::W.to_string()),
            ("x".to_string(), "urn:a&b\"c".to_string()),
        ];
        SubdocFragment::parse_in_namespace_context("<x:item/>", &namespaces)
            .expect("URI attribute values must be safely escaped before validation");
        assert!(SubdocFragment::parse_in_namespace_context("<m:oMath/>", &namespaces).is_err());
    }

    #[test]
    fn json_objects_keep_source_insertion_order() {
        let json = serde_json::from_str(
            r#"{"z":0,"a":1,"m":2,"nested":{"third":3,"first":1,"second":2}}"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);

        assert_eq!(
            context.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            vec!["z", "a", "m", "nested"]
        );
        let Some(RenderValue::Object(entries)) = context.get("nested") else {
            panic!("nested must be converted to a RenderValue::Object");
        };
        assert_eq!(
            entries
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            vec!["third", "first", "second"]
        );
    }

    #[test]
    fn checked_json_context_rejects_non_objects() {
        for value in [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!("text"),
            serde_json::json!(1),
        ] {
            let error = RenderContext::try_from_json(&value).expect_err("must reject non-object");
            assert_eq!(
                error.to_string(),
                "top-level JSON render context must be an object"
            );
        }
        assert!(RenderContext::try_from_json(&serde_json::json!({})).is_ok());
    }
}
