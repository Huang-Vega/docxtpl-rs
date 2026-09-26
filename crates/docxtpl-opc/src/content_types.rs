//! Parsing and querying [Content_Types].xml.

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::error::OpcError;
use crate::uri::PartUri;
use crate::xmlutil::required_attr;

/// Content Types: `Default` (by extension) + `Override` (by part name).
///
/// Lookup order: `Override` is checked first (exact, case-sensitive part-name
/// match); if nothing matches, `Default` is checked by file extension
/// (case-insensitive extension).
///
/// # Failure cases
///
/// [`ContentTypes::parse`] returns [`OpcError::InvalidContentTypes`] when the
/// XML is malformed or a required attribute is missing; `content_type_of`
/// returns `None` when nothing matches.
///
/// # Examples
///
/// ```
/// use docxtpl_opc::{ContentTypes, PartUri};
///
/// let xml = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
///   <Default Extension="xml" ContentType="application/xml"/>
///   <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
/// </Types>"#;
/// let ct = ContentTypes::parse(xml)?;
/// let doc = PartUri::new("word/document.xml")?;
/// assert_eq!(
///     ct.content_type_of(&doc),
///     Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml")
/// );
/// assert_eq!(ct.content_type_of(&PartUri::new("word/styles.XML")?), Some("application/xml"));
/// assert_eq!(ct.content_type_of(&PartUri::new("media/image.png")?), None);
/// # Ok::<(), docxtpl_opc::OpcError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentTypes {
    /// (lowercase extension, content type), kept in file order.
    defaults: Vec<(String, String)>,
    /// (part name without leading /, content type), kept in file order.
    overrides: Vec<(String, String)>,
}

/// Table of (lowercase extension, content type) for content types that can be
/// expressed via extension-based `Default` entries.
///
/// A 1:1 port of python-docx `docx.opc.spec.default_content_types` (0.20.2):
/// when `_ContentTypesItem.from_parts` rebuilds the CT on save, parts matching
/// this table land in `Default`; all others land in `Override`. The `rels` and
/// `xml` rows are also always prepopulated.
const DEFAULT_CONTENT_TYPES: &[(&str, &str)] = &[
    (
        "bin",
        "application/vnd.openxmlformats-officedocument.presentationml.printerSettings",
    ),
    (
        "bin",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings",
    ),
    (
        "bin",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.printerSettings",
    ),
    ("bmp", "image/bmp"),
    ("emf", "image/x-emf"),
    ("fntdata", "application/x-fontdata"),
    ("gif", "image/gif"),
    ("jpe", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("png", "image/png"),
    (
        "rels",
        "application/vnd.openxmlformats-package.relationships+xml",
    ),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("wdp", "image/vnd.ms-photo"),
    ("wmf", "image/x-wmf"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("xml", "application/xml"),
];

/// Content type of OPC relationship parts (`Default Extension="rels"`).
const OPC_RELATIONSHIPS_CT: &str = "application/vnd.openxmlformats-package.relationships+xml";
/// Content type of generic XML parts (`Default Extension="xml"`).
const XML_CT: &str = "application/xml";

impl ContentTypes {
    /// Parse the `[Content_Types].xml` text.
    ///
    /// # Failure cases
    ///
    /// Returns [`OpcError::InvalidContentTypes`] on malformed XML, or when a
    /// `Default`/`Override` is missing a required attribute
    /// (Extension/ContentType/PartName) or has an empty attribute value.
    pub fn parse(xml: &str) -> Result<Self, OpcError> {
        fn invalid(reason: String) -> OpcError {
            OpcError::InvalidContentTypes { reason }
        }

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut types = ContentTypes::default();
        loop {
            let event = reader
                .read_event()
                .map_err(|err| invalid(format!("XML parse failed: {err}")))?;
            let element = match event {
                Event::Start(element) | Event::Empty(element) => element,
                Event::Eof => break,
                _ => continue,
            };
            match element.local_name().as_ref() {
                b"Default" => {
                    let extension = required_attr(&element, "Extension").map_err(invalid)?;
                    let content_type = required_attr(&element, "ContentType").map_err(invalid)?;
                    if extension.is_empty() || content_type.is_empty() {
                        return Err(invalid(
                            "Default Extension/ContentType must not be empty".to_string(),
                        ));
                    }
                    // Extension matching is case-insensitive (OPC rule); store
                    // lowercased for uniform comparison.
                    types
                        .defaults
                        .push((extension.to_lowercase(), content_type));
                }
                // OPC PartName often has a leading /, while the part name itself does not.
                b"Override" => {
                    let part_name = required_attr(&element, "PartName").map_err(invalid)?;
                    let content_type = required_attr(&element, "ContentType").map_err(invalid)?;
                    if part_name.is_empty() || content_type.is_empty() {
                        return Err(invalid(
                            "Override PartName/ContentType must not be empty".to_string(),
                        ));
                    }
                    let name = part_name.strip_prefix('/').unwrap_or(part_name.as_str());
                    types.overrides.push((name.to_string(), content_type));
                }
                _ => {}
            }
        }
        Ok(types)
    }

    /// Look up the content type of a part: `Override` first, then `Default`
    /// by extension.
    ///
    /// Returns `None` when nothing matches.
    pub fn content_type_of(&self, uri: &PartUri) -> Option<&str> {
        let name = uri.as_str();
        for (part, content_type) in &self.overrides {
            if part == name {
                return Some(content_type);
            }
        }
        let file_name = uri.file_name();
        let extension = file_name.rfind('.').map(|dot| &file_name[dot + 1..])?;
        let extension = extension.to_lowercase();
        for (known, content_type) in &self.defaults {
            if *known == extension {
                return Some(content_type);
            }
        }
        None
    }

    /// Whether a `Default` for the given extension is registered
    /// (case-insensitive).
    pub fn has_default(&self, extension: &str) -> bool {
        let extension = extension.to_lowercase();
        self.defaults.iter().any(|(known, _)| *known == extension)
    }

    /// All registered `Default` entries (lowercase extension, in file order).
    pub fn defaults(&self) -> impl Iterator<Item = (&str, &str)> {
        self.defaults
            .iter()
            .map(|(ext, ct)| (ext.as_str(), ct.as_str()))
    }

    /// Append a `Default` (the extension is lowercased); an entry for the same
    /// extension is ignored, matching the override-dedup behavior of upstream
    /// `CaseInsensitiveDict.__setitem__` (content types inside a template are
    /// fixed, so the same extension never maps to different content types).
    pub fn add_default(&mut self, extension: &str, content_type: &str) {
        let extension = extension.to_lowercase();
        if self.has_default(&extension) {
            return;
        }
        self.defaults.push((extension, content_type.to_string()));
    }

    /// Add or replace an Override declaration (`part_name` may include or omit
    /// the leading `/`).
    ///
    /// Used when merging Subdoc parts: external docx parts moved in
    /// (styles/numbering/header/footer, etc.) declare their content types by
    /// part name.
    pub fn add_override(&mut self, part_name: &str, content_type: &str) {
        let name = part_name.strip_prefix('/').unwrap_or(part_name);
        if let Some(slot) = self.overrides.iter_mut().find(|(p, _)| p == name) {
            slot.1 = content_type.to_string();
            return;
        }
        self.overrides
            .push((name.to_string(), content_type.to_string()));
    }

    /// Rebuild the registration from the package's parts, following
    /// python-docx `_ContentTypesItem.from_parts` (0.20.2): always prepopulate
    /// `Default rels/xml`; for every other part whose (lowercase extension,
    /// content type) matches the `DEFAULT_CONTENT_TYPES` table, emit a
    /// `Default`; otherwise emit an `Override`.
    ///
    /// Real Word templates often register `_rels/*.rels` and
    /// `customXml/item*.xml` as Override; on save python-docx does not include
    /// rels in the parts enumeration (they always use the prepopulated Default
    /// rels) and `application/xml` uses Default xml — those Overrides vanish
    /// during the rebuild. The input must not contain
    /// `[Content_Types].xml`, `.rels`, or directory entries (the caller,
    /// [`crate::Package::rebuild_content_types`], already filters them out).
    /// Sorting happens in [`ContentTypes::to_xml`]; this method only
    /// reassigns Default/Override ownership.
    pub fn rebuild_from_parts<'a>(&mut self, parts: impl Iterator<Item = (&'a str, &'a str)>) {
        let mut fresh = ContentTypes::default();
        fresh.add_default("rels", OPC_RELATIONSHIPS_CT);
        fresh.add_default("xml", XML_CT);
        for (uri, content_type) in parts {
            let extension = uri
                .rsplit('/')
                .next()
                .and_then(|file_name| file_name.rfind('.').map(|dot| &file_name[dot + 1..]))
                .unwrap_or("")
                .to_lowercase();
            let is_default = DEFAULT_CONTENT_TYPES
                .iter()
                .any(|(ext, ct)| *ext == extension && *ct == content_type);
            if is_default {
                fresh.add_default(&extension, content_type);
            } else {
                fresh.add_override(uri, content_type);
            }
        }
        *self = fresh;
    }

    /// Serialize to the `[Content_Types].xml` bytes python-docx produces on save.
    ///
    /// The format is pinned to lxml output: single-quoted XML declaration plus
    /// `\n`; root element `Types` (the package content-type namespace);
    /// `Default` entries sorted by Extension and `Override` entries by PartName
    /// in ASCII order (upstream `sorted(self._defaults)`/
    /// `sorted(self._overrides)`); no whitespace between child elements; the
    /// PartName of `Override` carries a leading `/`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::ContentTypes;
    /// let mut ct = ContentTypes::default();
    /// ct.add_default("xml", "application/xml");
    /// ct.add_default("rels", "application/vnd.openxmlformats-package.relationships+xml");
    /// assert_eq!(
    ///     ct.to_xml(),
    ///     "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
    ///      <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
    ///      <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
    ///      <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
    ///      </Types>"
    /// );
    /// ```
    pub fn to_xml(&self) -> String {
        use crate::xmlutil::{escape_attribute, LXML_XML_DECLARATION};

        const CONTENT_TYPES_NS: &str =
            "http://schemas.openxmlformats.org/package/2006/content-types";

        let mut defaults = self.defaults.iter().collect::<Vec<_>>();
        defaults.sort_by(|a, b| a.0.cmp(&b.0));
        let mut overrides = self.overrides.iter().collect::<Vec<_>>();
        overrides.sort_by(|a, b| a.0.cmp(&b.0));

        let empty = defaults.is_empty() && overrides.is_empty();
        let mut out = String::from(LXML_XML_DECLARATION);
        if empty {
            out.push_str(&format!(r#"<Types xmlns="{CONTENT_TYPES_NS}"/>"#));
            return out;
        }
        out.push_str(&format!(r#"<Types xmlns="{CONTENT_TYPES_NS}">"#));
        for (extension, content_type) in defaults {
            out.push_str(&format!(
                r#"<Default Extension="{}" ContentType="{}"/>"#,
                escape_attribute(extension),
                escape_attribute(content_type),
            ));
        }
        for (part_name, content_type) in overrides {
            out.push_str(&format!(
                r#"<Override PartName="/{}" ContentType="{}"/>"#,
                escape_attribute(part_name),
                escape_attribute(content_type),
            ));
        }
        out.push_str("</Types>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="XML" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    #[test]
    fn parses_and_looks_up() {
        let ct = ContentTypes::parse(SAMPLE).unwrap();
        let doc = PartUri::new("word/document.xml").unwrap();
        assert_eq!(
            ct.content_type_of(&doc),
            Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            )
        );
        // When Override misses, fall back to Default by extension (case-insensitive)
        let styles = PartUri::new("word/styles.xml").unwrap();
        assert_eq!(ct.content_type_of(&styles), Some("application/xml"));
        let rels = PartUri::new("_rels/.rels").unwrap();
        assert_eq!(
            ct.content_type_of(&rels),
            Some("application/vnd.openxmlformats-package.relationships+xml")
        );
        assert_eq!(
            ct.content_type_of(&PartUri::new("media/image.png").unwrap()),
            None
        );
        // No extension
        assert_eq!(ct.content_type_of(&PartUri::new("README").unwrap()), None);
    }

    #[test]
    fn rejects_missing_or_empty_attributes() {
        for bad in [
            "<Types><Default ContentType=\"application/xml\"/></Types>",
            "<Types><Default Extension=\"xml\"/></Types>",
            "<Types><Default Extension=\"\" ContentType=\"application/xml\"/></Types>",
            "<Types><Override PartName=\"/a.xml\"/></Types>",
            "<Types><Override ContentType=\"application/xml\"/></Types>",
            "<Types><Override PartName=\"\" ContentType=\"application/xml\"/></Types>",
        ] {
            let err = ContentTypes::parse(bad).unwrap_err();
            assert!(
                matches!(err, OpcError::InvalidContentTypes { .. }),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_broken_xml() {
        let err =
            ContentTypes::parse("<Types><Default Extension=\"xml\" ContentType=\"a\"></Wrong>")
                .unwrap_err();
        assert!(matches!(err, OpcError::InvalidContentTypes { .. }));
    }

    #[test]
    fn add_default_dedupes_case_insensitively() {
        let mut ct = ContentTypes::default();
        ct.add_default("PNG", "image/png");
        ct.add_default("png", "image/png-other");
        // The second add is ignored: only one Default, and the content type keeps the first value.
        assert_eq!(ct.to_xml().matches("<Default ").count(), 1);
        assert!(ct.has_default("pNg"));
        assert_eq!(
            ct.content_type_of(&PartUri::new("a.png").unwrap()),
            Some("image/png")
        );
    }

    #[test]
    fn to_xml_sorts_defaults_and_overrides() {
        // Match the CT of p4_img_wh: jpeg must sort before jpg (ASCII 'e' < 'g')
        // and after png; Override entries sort by PartName, which keeps a leading /.
        let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes'?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="jpg" ContentType="image/jpeg"/>
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="jpeg" ContentType="image/jpeg"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#;
        let mut ct = ContentTypes::parse(xml).unwrap();
        ct.add_default("png", "image/png");
        assert_eq!(
            ct.to_xml(),
            "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
<Default Extension=\"jpg\" ContentType=\"image/jpeg\"/>\
<Default Extension=\"png\" ContentType=\"image/png\"/>\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
</Types>"
        );

        // Parse it back: the logical content is identical (parsing keeps file
        // order; sorting only happens in to_xml).
        let reparsed = ContentTypes::parse(&ct.to_xml()).unwrap();
        for ext in ["jpeg", "jpg", "png", "rels", "xml"] {
            assert!(reparsed.has_default(ext), "{ext}");
        }
        assert_eq!(
            reparsed.content_type_of(&PartUri::new("word/document.xml").unwrap()),
            Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            )
        );
    }

    #[test]
    fn empty_types_serialize_self_closing() {
        assert_eq!(
            ContentTypes::default().to_xml(),
            "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>"
        );
    }
}
