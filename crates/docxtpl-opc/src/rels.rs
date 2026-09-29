//! OPC relationships (.rels files).

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::error::OpcError;
use crate::uri::{resolve_relative_to, PartUri};
use crate::xmlutil::{attr_value, required_attr};

/// Relationship target mode.
///
/// # Examples
///
/// ```
/// use docxtpl_opc::TargetMode;
///
/// assert_ne!(TargetMode::Internal, TargetMode::External);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetMode {
    /// Target inside the package (the default value of `TargetMode`).
    Internal,
    /// Target outside the package (`TargetMode="External"`); excluded from
    /// in-package resolution and existence validation.
    External,
}

/// A single relationship (fields are public and can be read directly).
///
/// # Examples
///
/// ```
/// use docxtpl_opc::{Relationship, TargetMode};
///
/// let rel = Relationship {
///     id: "rId1".to_string(),
///     rel_type: "http://example.com/rel".to_string(),
///     target: "styles.xml".to_string(),
///     target_mode: TargetMode::Internal,
/// };
/// assert_eq!(rel.id, "rId1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    /// Relationship Id; should be unique within a single .rels file
    /// (checked by [`crate::Package::validate`]).
    pub id: String,
    /// Relationship type URI.
    pub rel_type: String,
    /// Target: for internal relationships, a path relative to the owner's
    /// directory (or a package-absolute path starting with `/`); for external
    /// relationships, any URI.
    pub target: String,
    /// Target mode; `TargetMode="External"` in XML maps to
    /// [`TargetMode::External`], while a missing or other value is treated as
    /// [`TargetMode::Internal`].
    pub target_mode: TargetMode,
}

/// The .rels set of one part (the original file order is preserved).
///
/// # Failure cases
///
/// [`Relationships::parse`] returns [`OpcError::InvalidRelationships`] when
/// the XML is malformed or an `<Relationship>` is missing an `Id`/`Type`/
/// `Target` attribute.
///
/// # Examples
///
/// ```
/// use docxtpl_opc::{PartUri, Relationships};
///
/// let xml = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
///   <Relationship Id="rId1" Type="http://example.com/rel" Target="styles.xml"/>
///   <Relationship Id="rId2" Type="http://example.com/rel" Target="https://example.com" TargetMode="External"/>
/// </Relationships>"#;
/// let rels = Relationships::parse(xml)?;
/// assert_eq!(rels.len(), 2);
/// let owner = PartUri::new("word/document.xml")?;
/// // The target is normalized relative to the owner's directory
/// assert_eq!(rels.resolve(&owner, "rId1").unwrap().as_str(), "word/styles.xml");
/// // External relationships are not resolved
/// assert!(rels.resolve(&owner, "rId2").is_none());
/// # Ok::<(), docxtpl_opc::OpcError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Relationships(Vec<Relationship>);

impl Relationships {
    /// Parse .rels XML text.
    ///
    /// # Failure cases
    ///
    /// Returns [`OpcError::InvalidRelationships`] on malformed XML or when an
    /// `<Relationship>` is missing an `Id`/`Type`/`Target` attribute.
    pub fn parse(xml: &str) -> Result<Self, OpcError> {
        Self::parse_in(xml, ".rels")
    }

    /// Parse .rels; `source` is used to locate the error in messages (e.g.
    /// the rels file path).
    pub(crate) fn parse_in(xml: &str, source: &str) -> Result<Self, OpcError> {
        let wrap = |reason: String| OpcError::InvalidRelationships {
            reason: format!("{source}: {reason}"),
        };

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut relations = Vec::new();
        loop {
            let event = reader
                .read_event()
                .map_err(|err| wrap(format!("XML parse failed: {err}")))?;
            let element = match event {
                Event::Start(element) | Event::Empty(element) => element,
                Event::Eof => break,
                _ => continue,
            };
            if element.local_name().as_ref() != "Relationship".as_bytes() {
                continue;
            }
            let id = required_attr(&element, "Id").map_err(wrap)?;
            let rel_type = required_attr(&element, "Type").map_err(wrap)?;
            let target = required_attr(&element, "Target").map_err(wrap)?;
            let target_mode = match attr_value(&element, "TargetMode").map_err(wrap)? {
                Some(mode) if mode == "External" => TargetMode::External,
                Some(_) | None => TargetMode::Internal,
            };
            relations.push(Relationship {
                id,
                rel_type,
                target,
                target_mode,
            });
        }
        Ok(Self(relations))
    }

    /// Iterate over all relationships in file order.
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::Relationships;
    /// let rels = Relationships::parse(
    ///     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    ///        <Relationship Id="rId1" Type="http://example.com/rel" Target="a"/>
    ///        <Relationship Id="rId2" Type="http://example.com/rel" Target="b"/>
    ///     </Relationships>"#)?;
    /// let ids: Vec<&str> = rels.iter().map(|rel| rel.id.as_str()).collect();
    /// assert_eq!(ids, ["rId1", "rId2"]);
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = &Relationship> {
        self.0.iter()
    }

    /// Look up by Id (first match).
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::Relationships;
    /// # let rels = Relationships::parse(
    /// #     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    /// #        <Relationship Id="rId1" Type="http://example.com/rel" Target="a"/>
    /// #     </Relationships>"#)?;
    /// assert_eq!(rels.get("rId1").unwrap().target, "a");
    /// assert!(rels.get("missing").is_none());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn get(&self, id: &str) -> Option<&Relationship> {
        self.0.iter().find(|rel| rel.id == id)
    }

    /// Whether there are no relationships.
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::Relationships;
    /// let rels = Relationships::parse(
    ///     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#)?;
    /// assert!(rels.is_empty());
    /// assert_eq!(rels.len(), 0);
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of relationships.
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::Relationships;
    /// # let rels = Relationships::parse(
    /// #     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    /// #        <Relationship Id="rId1" Type="http://example.com/rel" Target="a"/>
    /// #        <Relationship Id="rId2" Type="http://example.com/rel" Target="b"/>
    /// #     </Relationships>"#)?;
    /// assert_eq!(rels.len(), 2);
    /// assert!(!rels.is_empty());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Resolve an internal target by id.
    ///
    /// `target` is normalized relative to `owner`'s directory (`.`/empty
    /// segments collapse, `..` moves up one level, a leading `/` is treated as
    /// a package-absolute path); returns `None` for external relationships,
    /// targets that escape the package root, or targets that cannot be parsed
    /// as a valid URI.
    ///
    /// Note: a returned [`PartUri`] only means path resolution succeeded, not
    /// that the corresponding part exists (existence is checked by
    /// [`crate::Package::validate`]).
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::{PartUri, Relationships};
    /// # let rels = Relationships::parse(
    /// #     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    /// #        <Relationship Id="rId1" Type="http://example.com/rel" Target="../media/logo.png"/>
    /// #     </Relationships>"#)?;
    /// let owner = PartUri::new("word/document.xml")?;
    /// assert_eq!(rels.resolve(&owner, "rId1").unwrap().as_str(), "media/logo.png");
    /// assert!(rels.resolve(&owner, "missing").is_none());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn resolve(&self, owner: &PartUri, id: &str) -> Option<PartUri> {
        let rel = self.get(id)?;
        if rel.target_mode != TargetMode::Internal {
            return None;
        }
        resolve_relative_to(owner.parent().as_ref(), &rel.target)
    }

    /// Append a relationship at the end (keeping the insertion order used when
    /// python-docx saves; no sorting).
    ///
    /// Id uniqueness is the caller's responsibility, ensured via
    /// [`Relationships::next_r_id`]; [`crate::Package::validate`] checks it
    /// again as a safety net.
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::{Relationship, Relationships, TargetMode};
    /// let mut rels = Relationships::default();
    /// rels.push(Relationship {
    ///     id: "rId1".to_string(),
    ///     rel_type: "http://example.com/rel".to_string(),
    ///     target: "a.xml".to_string(),
    ///     target_mode: TargetMode::Internal,
    /// });
    /// assert_eq!(rels.len(), 1);
    /// ```
    pub fn push(&mut self, relationship: Relationship) {
        self.0.push(relationship);
    }

    /// Upstream `_Relationships._next_rId`: starting from `rId1`, backfill the
    /// first unused Id.
    ///
    /// The count includes external relationships (matching upstream
    /// `range(1, len(self)+2)`); when existing Ids are discontinuous the holes
    /// are filled, otherwise returns `rId{len+1}`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::{Relationship, Relationships, TargetMode};
    /// let mut rels = Relationships::default();
    /// assert_eq!(rels.next_r_id(), "rId1");
    /// rels.push(Relationship {
    ///     id: "rId1".to_string(), rel_type: "t".into(),
    ///     target: "a".into(), target_mode: TargetMode::Internal,
    /// });
    /// rels.push(Relationship {
    ///     id: "rId3".to_string(), rel_type: "t".into(),
    ///     target: "b".into(), target_mode: TargetMode::Internal,
    /// });
    /// // Fill the rId2 hole first
    /// assert_eq!(rels.next_r_id(), "rId2");
    /// ```
    pub fn next_r_id(&self) -> String {
        let mut n: u64 = 1;
        loop {
            let candidate = format!("rId{n}");
            if self.0.iter().all(|rel| rel.id != candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Upstream `_get_matching_rel`: reuse an existing relationship when
    /// `rel_type` + target + mode all match.
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::{Relationship, Relationships, TargetMode};
    /// let mut rels = Relationships::default();
    /// rels.push(Relationship {
    ///     id: "rId9".to_string(),
    ///     rel_type: "http://example.com/h".to_string(),
    ///     target: "https://example.com/".to_string(),
    ///     target_mode: TargetMode::External,
    /// });
    /// assert!(rels
    ///     .find_matching("http://example.com/h", "https://example.com/", TargetMode::External)
    ///     .is_some());
    /// assert!(rels
    ///     .find_matching("http://example.com/h", "https://example.com/", TargetMode::Internal)
    ///     .is_none());
    /// ```
    pub fn find_matching(
        &self,
        rel_type: &str,
        target: &str,
        mode: TargetMode,
    ) -> Option<&Relationship> {
        self.0
            .iter()
            .find(|rel| rel.rel_type == rel_type && rel.target == target && rel.target_mode == mode)
    }

    /// Serialize to the `.rels` bytes python-docx produces on save.
    ///
    /// The format is pinned to lxml output: single-quoted XML declaration plus
    /// `\n`; root element `Relationships` (the default OPC rels namespace);
    /// child elements keep insertion order with no whitespace indentation;
    /// attribute order is `Id/Type/Target[/TargetMode]`; an empty set
    /// serializes as a self-closing root element.
    ///
    /// # Examples
    ///
    /// ```
    /// # use docxtpl_opc::{Relationship, Relationships, TargetMode};
    /// let mut rels = Relationships::default();
    /// rels.push(Relationship {
    ///     id: "rId1".to_string(),
    ///     rel_type: "http://example.com/t".to_string(),
    ///     target: "media/image1.png".to_string(),
    ///     target_mode: TargetMode::Internal,
    /// });
    /// assert_eq!(
    ///     rels.to_xml(),
    ///     "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
    ///      <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
    ///      <Relationship Id=\"rId1\" Type=\"http://example.com/t\" Target=\"media/image1.png\"/>\
    ///      </Relationships>"
    /// );
    /// ```
    pub fn to_xml(&self) -> String {
        use crate::xmlutil::{escape_attribute, LXML_XML_DECLARATION};

        const RELATIONSHIPS_NS: &str =
            "http://schemas.openxmlformats.org/package/2006/relationships";

        let mut out = String::from(LXML_XML_DECLARATION);
        if self.0.is_empty() {
            out.push_str(&format!(r#"<Relationships xmlns="{RELATIONSHIPS_NS}"/>"#));
            return out;
        }
        out.push_str(&format!(r#"<Relationships xmlns="{RELATIONSHIPS_NS}">"#));
        for rel in &self.0 {
            out.push_str(&format!(
                r#"<Relationship Id="{}" Type="{}" Target="{}""#,
                escape_attribute(&rel.id),
                escape_attribute(&rel.rel_type),
                escape_attribute(&rel.target),
            ));
            if rel.target_mode == TargetMode::External {
                out.push_str(r#" TargetMode="External""#);
            }
            out.push_str("/>");
        }
        out.push_str("</Relationships>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://example.com/styles" Target="styles.xml"/>
<Relationship Id="rId2" Type="http://example.com/link" Target="https://example.com/doc" TargetMode="External"/>
<Relationship Id="rId3" Type="http://example.com/image" Target="../media/logo.png" TargetMode="Internal"/>
</Relationships>"#;

    #[test]
    fn parses_attributes_and_modes() {
        let rels = Relationships::parse(SAMPLE).unwrap();
        assert_eq!(rels.len(), 3);
        assert!(!rels.is_empty());

        let r1 = rels.get("rId1").unwrap();
        assert_eq!(r1.id, "rId1");
        assert_eq!(r1.rel_type, "http://example.com/styles");
        assert_eq!(r1.target, "styles.xml");
        assert_eq!(r1.target_mode, TargetMode::Internal);

        // TargetMode="External" → External; missing and Internal both map to Internal
        assert_eq!(rels.get("rId2").unwrap().target_mode, TargetMode::External);
        assert_eq!(rels.get("rId3").unwrap().target_mode, TargetMode::Internal);

        assert!(rels.get("missing").is_none());

        // Keep the original file order
        let ids: Vec<&str> = rels.iter().map(|rel| rel.id.as_str()).collect();
        assert_eq!(ids, ["rId1", "rId2", "rId3"]);
    }

    #[test]
    fn resolves_targets_relative_to_owner_directory() {
        let rels = Relationships::parse(SAMPLE).unwrap();
        let owner = PartUri::new("word/document.xml").unwrap();
        assert_eq!(
            rels.resolve(&owner, "rId1").unwrap().as_str(),
            "word/styles.xml"
        );
        assert_eq!(
            rels.resolve(&owner, "rId3").unwrap().as_str(),
            "media/logo.png"
        );
        // External relationships are not resolved
        assert!(rels.resolve(&owner, "rId2").is_none());
        // Unknown Id
        assert!(rels.resolve(&owner, "nope").is_none());
    }

    #[test]
    fn rejects_missing_required_attributes() {
        for bad in [
            r#"<Relationships><Relationship Type="t" Target="a"/></Relationships>"#,
            r#"<Relationships><Relationship Id="1" Target="a"/></Relationships>"#,
            r#"<Relationships><Relationship Id="1" Type="t"/></Relationships>"#,
        ] {
            let err = Relationships::parse(bad).unwrap_err();
            assert!(
                matches!(err, OpcError::InvalidRelationships { .. }),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_broken_xml() {
        let err = Relationships::parse("<Relationships></Wrong>").unwrap_err();
        assert!(matches!(err, OpcError::InvalidRelationships { .. }));
    }

    #[test]
    fn to_xml_matches_lxml_rebuild_byte_for_byte() {
        // Verify the lxml byte format: single-quoted declaration plus newline,
        // insertion order preserved, no indentation, attribute order
        // Id/Type/Target[/TargetMode]. Byte alignment with real p4 rels is
        // guaranteed by docxtpl-rs's oracle differential gate.
        let mut rels = Relationships::default();
        let mut push = |id: &str, ty: &str, target: &str, mode: TargetMode| {
            rels.push(Relationship {
                id: id.to_string(),
                rel_type: ty.to_string(),
                target: target.to_string(),
                target_mode: mode,
            });
        };
        push(
            "rId3",
            "http://example.com/t1",
            "settings.xml",
            TargetMode::Internal,
        );
        push(
            "rId1",
            "http://example.com/t2",
            "theme/theme1.xml",
            TargetMode::Internal,
        );
        push(
            "rId9",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
            "media/image1.png",
            TargetMode::Internal,
        );
        push(
            "rId10",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
            "https://example.com/a?b=1&c=2",
            TargetMode::External,
        );

        let xml = rels.to_xml();
        assert_eq!(
            xml,
            "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId3\" Type=\"http://example.com/t1\" Target=\"settings.xml\"/>\
<Relationship Id=\"rId1\" Type=\"http://example.com/t2\" Target=\"theme/theme1.xml\"/>\
<Relationship Id=\"rId9\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.png\"/>\
<Relationship Id=\"rId10\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"https://example.com/a?b=1&amp;c=2\" TargetMode=\"External\"/>\
</Relationships>"
        );

        // The rebuild can be parsed again with identical fields (order kept;
        // entities in external URLs are decoded back).
        let reparsed = Relationships::parse(&xml).unwrap();
        assert_eq!(reparsed, rels);
    }

    #[test]
    fn empty_relationships_serialize_self_closing() {
        let rels = Relationships::default();
        assert_eq!(
            rels.to_xml(),
            "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"/>"
        );
    }

    #[test]
    fn next_r_id_fills_holes() {
        let mut rels = Relationships::default();
        assert_eq!(rels.next_r_id(), "rId1");
        for id in ["rId1", "rId3", "rId2"] {
            rels.push(Relationship {
                id: id.into(),
                rel_type: "t".into(),
                target: "a".into(),
                target_mode: TargetMode::Internal,
            });
        }
        assert_eq!(rels.next_r_id(), "rId4");
    }
}
