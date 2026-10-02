//! Transactional import of paragraph/table WordML fragments.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use docxtpl_opc::{resolve_part_target, OpcError, Package, PartUri, TargetMode};
use docxtpl_xml::{ns_uri, NodeId, XmlDocument, XmlLimits};

use super::{
    allocate_positive_id, decode_xml_bytes, is_word_element, Error, MediaRegistration,
    StoryEditContext, HYPERLINK_REL_TYPE, IMAGE_REL_TYPE,
};

const NUMBERING_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const NUMBERING_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
const EMPTY_NUMBERING: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#,
);
const CHART_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
const DIAGRAM_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/diagram";
const OFFICE_NS: &str = "urn:schemas-microsoft-com:office:office";
const VML_NS: &str = "urn:schemas-microsoft-com:vml";

/// Where imported top-level nodes are attached relative to the target node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FragmentPlacement {
    /// Insert immediately before the target, preserving source order.
    #[default]
    Before,
    /// Insert immediately after the target, preserving source order.
    After,
    /// Append as children of the target.
    Append,
}

/// Options for importing a WordML fragment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FragmentImportOptions {
    /// Attachment position for the imported top-level nodes.
    pub placement: FragmentPlacement,
}

/// Summary of one fragment import.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FragmentImportReport {
    /// Number of inserted top-level paragraphs/tables.
    pub inserted_nodes: usize,
    /// Number of distinct source image relationships remapped.
    pub image_relationships: usize,
    /// Number of distinct source external hyperlink relationships remapped.
    pub hyperlink_relationships: usize,
    /// Number of source numbering instances copied/remapped.
    pub numbering_instances: usize,
    /// Inserted root node ids in source order.
    pub roots: Vec<NodeId>,
}

/// A selected set of top-level paragraphs/tables from one source DOCX part.
///
/// Construction parses the source part once. By default, a main-document
/// source selects all direct `w:p`/`w:tbl` children of `w:body`; other parts
/// select direct paragraph/table children of their root.
pub struct WordFragment {
    package: Package,
    source_part: String,
    document: XmlDocument,
    roots: Vec<NodeId>,
    available_roots: HashSet<NodeId>,
}

impl WordFragment {
    /// Parse and select all supported top-level nodes from `source_part`.
    pub fn from_package(package: Package, source_part: impl Into<String>) -> Result<Self, Error> {
        let source_part = source_part.into();
        let bytes = package
            .part(&source_part)
            .ok_or_else(|| OpcError::MissingPart {
                uri: source_part.clone(),
            })?
            .bytes()?;
        let xml = decode_xml_bytes(bytes, &source_part)?;
        let document =
            XmlDocument::parse_strict(&xml, &XmlLimits::default()).map_err(|source| {
                OpcError::Malformed {
                    reason: format!(
                        "fragment source part {source_part:?} is invalid XML: {source}"
                    ),
                }
            })?;
        let container = document
            .descendants(document.root())
            .into_iter()
            .find(|node| is_word_element_document(&document, *node, "body"))
            .unwrap_or(document.root());
        let roots: Vec<NodeId> = document
            .children(container)
            .iter()
            .copied()
            .filter(|node| {
                is_word_element_document(&document, *node, "p")
                    || is_word_element_document(&document, *node, "tbl")
            })
            .collect();
        let available_roots = roots.iter().copied().collect();
        Ok(Self {
            package,
            source_part,
            document,
            roots,
            available_roots,
        })
    }

    /// Parsed source XML, used to choose a subset without reparsing.
    #[must_use]
    pub const fn document(&self) -> &XmlDocument {
        &self.document
    }

    /// Currently selected top-level nodes.
    #[must_use]
    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    /// Replace the selection with attached `w:p`/`w:tbl` nodes.
    pub fn select_roots(&mut self, roots: impl IntoIterator<Item = NodeId>) -> Result<(), Error> {
        let roots: Vec<_> = roots.into_iter().collect();
        let unique: HashSet<_> = roots.iter().copied().collect();
        if unique.len() != roots.len()
            || roots
                .iter()
                .any(|root| !self.available_roots.contains(root))
        {
            return Err(OpcError::Malformed {
                reason: "fragment selection must contain unique top-level w:p or w:tbl elements"
                    .to_string(),
            }
            .into());
        }
        self.roots = roots;
        Ok(())
    }
}

impl StoryEditContext<'_, '_, '_> {
    /// Import selected paragraphs/tables and remap their package resources.
    pub fn import_fragment(
        &mut self,
        target: NodeId,
        mut fragment: WordFragment,
        options: FragmentImportOptions,
    ) -> Result<FragmentImportReport, Error> {
        self.transaction.check_control()?;
        validate_target(self.story.document(), target, options.placement)?;
        validate_fragment_features(&fragment)?;

        let numbering_instances = self.remap_fragment_numbering(&mut fragment)?;
        let mut image_mapping = HashMap::new();
        let mut hyperlink_mapping = HashMap::new();
        self.remap_fragment_relationships(
            &mut fragment,
            &mut image_mapping,
            &mut hyperlink_mapping,
        )?;
        self.remap_fragment_bookmarks(&mut fragment)?;

        let mut inserted = Vec::with_capacity(fragment.roots.len());
        for source_root in fragment.roots.iter().copied() {
            self.transaction.check_control()?;
            let copied = self
                .story
                .document_mut()
                .deepcopy_element(&fragment.document, source_root)
                .map_err(|source| OpcError::Malformed {
                    reason: format!("could not copy WordML fragment node: {source}"),
                })?;
            self.renumber_imported_drawings(copied);
            inserted.push(copied);
        }
        attach_roots(
            self.story.document_mut(),
            target,
            &inserted,
            options.placement,
        )?;

        Ok(FragmentImportReport {
            inserted_nodes: inserted.len(),
            image_relationships: image_mapping.len(),
            hyperlink_relationships: hyperlink_mapping.len(),
            numbering_instances,
            roots: inserted,
        })
    }

    fn remap_fragment_relationships(
        &mut self,
        fragment: &mut WordFragment,
        image_mapping: &mut HashMap<String, String>,
        hyperlink_mapping: &mut HashMap<String, String>,
    ) -> Result<(), Error> {
        let relationship_nodes = selected_descendants(fragment);
        for node in relationship_nodes {
            self.transaction.check_control()?;
            let Some(tag) = fragment.document.tag(node) else {
                continue;
            };
            let tag_ns = tag.ns.clone();
            let tag_local = tag.local.clone();
            let attrs: Vec<_> = fragment
                .document
                .attrs(node)
                .iter()
                .filter(|(name, _)| name.ns == ns_uri::R)
                .map(|(name, value)| (name.local.clone(), value.clone()))
                .collect();
            for (attribute, old_rid) in attrs {
                let is_image = tag_ns == ns_uri::A && tag_local == "blip" && attribute == "embed";
                let is_hyperlink = ((tag_ns == ns_uri::W && tag_local == "hyperlink")
                    || (tag_ns == ns_uri::A && tag_local == "hlinkClick"))
                    && attribute == "id";
                let new_rid = if is_image {
                    if let Some(mapped) = image_mapping.get(&old_rid) {
                        mapped.clone()
                    } else {
                        let media = register_fragment_image(self, fragment, &old_rid)?;
                        let mapped = self.transaction.relate_image(&self.owner, &media)?;
                        self.story.inner.image_rids.insert(mapped.clone());
                        image_mapping.insert(old_rid.clone(), mapped.clone());
                        mapped
                    }
                } else if is_hyperlink {
                    if let Some(mapped) = hyperlink_mapping.get(&old_rid) {
                        mapped.clone()
                    } else {
                        let mapped = register_fragment_hyperlink(self, fragment, &old_rid)?;
                        self.story
                            .inner
                            .external_hyperlink_rids
                            .insert(mapped.clone());
                        hyperlink_mapping.insert(old_rid.clone(), mapped.clone());
                        mapped
                    }
                } else {
                    return Err(unsupported(format!(
                        "relationship attribute {}:{}@r:{attribute}",
                        tag_ns, tag_local
                    )));
                };
                fragment
                    .document
                    .set_attr(node, ns_uri::R, &attribute, new_rid);
            }
        }
        Ok(())
    }

    fn remap_fragment_bookmarks(&mut self, fragment: &mut WordFragment) -> Result<(), Error> {
        let target_starts = bookmark_starts_in(self.story.document(), self.story.document().root());
        let mut used_ids: HashSet<u64> = target_starts
            .iter()
            .filter_map(|(_, id, _)| id.parse().ok())
            .collect();
        let mut used_names: HashSet<String> =
            target_starts.into_iter().map(|(_, _, name)| name).collect();
        let mut id_mapping = HashMap::new();
        let mut name_mapping = HashMap::new();

        for root in fragment.roots.clone() {
            self.transaction.check_control()?;
            for node in fragment.document.descendants(root) {
                if !is_word_element_document(&fragment.document, node, "bookmarkStart") {
                    continue;
                }
                let old_id = fragment
                    .document
                    .attr(node, ns_uri::W, "id")
                    .ok_or_else(|| malformed("fragment bookmarkStart is missing w:id"))?
                    .to_string();
                let old_name = fragment
                    .document
                    .attr(node, ns_uri::W, "name")
                    .ok_or_else(|| malformed("fragment bookmarkStart is missing w:name"))?
                    .to_string();
                let new_id = allocate_positive_id(&mut used_ids).to_string();
                let new_name = unique_fragment_bookmark_name(&old_name, &mut used_names);
                id_mapping.insert(old_id, new_id.clone());
                name_mapping.insert(old_name, new_name.clone());
                fragment.document.set_attr(node, ns_uri::W, "id", new_id);
                fragment
                    .document
                    .set_attr(node, ns_uri::W, "name", new_name);
            }
        }
        for root in fragment.roots.clone() {
            self.transaction.check_control()?;
            for node in fragment.document.descendants(root) {
                if is_word_element_document(&fragment.document, node, "bookmarkEnd") {
                    let old_id = fragment
                        .document
                        .attr(node, ns_uri::W, "id")
                        .ok_or_else(|| malformed("fragment bookmarkEnd is missing w:id"))?;
                    let new_id = id_mapping.get(old_id).ok_or_else(|| {
                        malformed(format!(
                            "fragment bookmarkEnd id {old_id:?} has no selected start"
                        ))
                    })?;
                    fragment
                        .document
                        .set_attr(node, ns_uri::W, "id", new_id.clone());
                }
                if is_word_element_document(&fragment.document, node, "hyperlink") {
                    if let Some(anchor) = fragment
                        .document
                        .attr(node, ns_uri::W, "anchor")
                        .map(str::to_string)
                    {
                        if let Some(mapped) = name_mapping.get(&anchor) {
                            fragment
                                .document
                                .set_attr(node, ns_uri::W, "anchor", mapped.clone());
                        } else if !used_names.contains(&anchor) {
                            return Err(malformed(format!(
                                "fragment hyperlink anchor {anchor:?} has no selected or target bookmark"
                            )));
                        }
                    }
                }
            }
        }
        if !id_mapping.is_empty() {
            self.story.inner.validate_internal_links = true;
        }
        Ok(())
    }

    fn renumber_imported_drawings(&mut self, root: NodeId) {
        for node in self.story.document().descendants(root) {
            let Some(tag) = self.story.document().tag(node) else {
                continue;
            };
            let id = if tag.ns == ns_uri::WP && tag.local == "docPr" {
                Some(self.drawing_ids.next_doc_pr())
            } else if tag.ns == ns_uri::PIC && tag.local == "cNvPr" {
                Some(self.drawing_ids.next_picture())
            } else {
                None
            };
            if let Some(id) = id {
                self.story
                    .document_mut()
                    .set_attr(node, "", "id", id.to_string());
            }
            if is_word_element(self.story.document(), node, "drawing") {
                self.drawing_ids.managed_drawings.push(node);
            }
        }
    }

    fn remap_fragment_numbering(&mut self, fragment: &mut WordFragment) -> Result<usize, Error> {
        let old_ids = selected_numbering_ids(fragment)?;
        if old_ids.is_empty() {
            return Ok(0);
        }
        let source_numbering = load_related_numbering(&fragment.package, &fragment.source_part)?
            .ok_or_else(|| malformed("fragment uses numbering but source has no numbering part"))?;
        validate_numbering_features(&source_numbering)?;
        let (target_name, mut target_numbering) = self.load_or_create_target_numbering()?;
        let mut num_mapping = HashMap::new();
        let mut abstract_mapping = HashMap::new();

        for old_num_id in old_ids {
            self.transaction.check_control()?;
            let source_num = find_numbering_node(&source_numbering, "num", "numId", old_num_id)
                .ok_or_else(|| malformed(format!("source numbering has no w:num {old_num_id}")))?;
            let source_abstract_id = source_numbering
                .descendants(source_num)
                .into_iter()
                .find(|node| is_word_element_document(&source_numbering, *node, "abstractNumId"))
                .and_then(|node| source_numbering.attr(node, ns_uri::W, "val"))
                .and_then(|value| value.parse::<i64>().ok())
                .ok_or_else(|| malformed("source w:num has no valid w:abstractNumId"))?;
            let new_abstract_id = if let Some(mapped) = abstract_mapping.get(&source_abstract_id) {
                *mapped
            } else {
                let source_abstract = find_numbering_node(
                    &source_numbering,
                    "abstractNum",
                    "abstractNumId",
                    source_abstract_id,
                )
                .ok_or_else(|| malformed("source numbering has no referenced w:abstractNum"))?;
                let new_id =
                    next_numbering_id(&target_numbering, "abstractNum", "abstractNumId", 0)?;
                let copied = target_numbering
                    .deepcopy_element(&source_numbering, source_abstract)
                    .map_err(|source| {
                        malformed(format!("could not copy abstract numbering: {source}"))
                    })?;
                target_numbering.set_attr(copied, ns_uri::W, "abstractNumId", new_id.to_string());
                insert_before_first_num(&mut target_numbering, copied);
                abstract_mapping.insert(source_abstract_id, new_id);
                new_id
            };
            let new_num_id = next_numbering_id(&target_numbering, "num", "numId", 1)?;
            let copied = target_numbering
                .deepcopy_element(&source_numbering, source_num)
                .map_err(|source| {
                    malformed(format!("could not copy numbering instance: {source}"))
                })?;
            target_numbering.set_attr(copied, ns_uri::W, "numId", new_num_id.to_string());
            let abstract_ref = target_numbering
                .descendants(copied)
                .into_iter()
                .find(|node| is_word_element_document(&target_numbering, *node, "abstractNumId"))
                .ok_or_else(|| malformed("copied w:num has no w:abstractNumId"))?;
            target_numbering.set_attr(abstract_ref, ns_uri::W, "val", new_abstract_id.to_string());
            target_numbering.append_child(target_numbering.root(), copied);
            num_mapping.insert(old_num_id, new_num_id);
        }

        for root in fragment.roots.clone() {
            self.transaction.check_control()?;
            for node in fragment.document.descendants(root) {
                if is_word_element_document(&fragment.document, node, "numId") {
                    if let Some(old) = fragment
                        .document
                        .attr(node, ns_uri::W, "val")
                        .and_then(|value| value.parse::<i64>().ok())
                    {
                        if let Some(new) = num_mapping.get(&old) {
                            fragment
                                .document
                                .set_attr(node, ns_uri::W, "val", new.to_string());
                        }
                    }
                }
            }
        }
        let serialized = target_numbering
            .try_serialize_story(self.transaction.max_rendered_xml_bytes)
            .map_err(|_| OpcError::LimitExceeded {
                kind: "rendered_xml_bytes",
                value: self.transaction.max_rendered_xml_bytes as u64 + 1,
                max: self.transaction.max_rendered_xml_bytes as u64,
            })?;
        self.transaction
            .set_part_bytes(&target_name, serialized.into_bytes())?;
        Ok(num_mapping.len())
    }

    fn load_or_create_target_numbering(&mut self) -> Result<(String, XmlDocument), Error> {
        let main_name = self
            .transaction
            .transaction
            .package()
            .main_document_uri()?
            .as_str()
            .to_string();
        let existing = related_part_name(
            self.transaction.transaction.package(),
            &main_name,
            NUMBERING_REL_TYPE,
        )?;
        let name = existing.unwrap_or_else(|| "word/numbering.xml".to_string());
        if !self.transaction.transaction.package().contains(&name) {
            self.transaction
                .add_part(&name, EMPTY_NUMBERING.as_bytes().to_vec())?;
            self.transaction
                .transaction
                .register_content_type(&name, NUMBERING_CONTENT_TYPE)?;
        }
        let target = super::images::relative_to_owner(&main_name, &name);
        self.transaction.relate(
            &main_name,
            NUMBERING_REL_TYPE,
            &target,
            TargetMode::Internal,
        )?;
        let bytes = self
            .transaction
            .part(&name)
            .ok_or_else(|| OpcError::MissingPart { uri: name.clone() })?
            .bytes()?;
        let xml = decode_xml_bytes(bytes, &name)?;
        let document =
            XmlDocument::parse_strict(&xml, &XmlLimits::default()).map_err(|source| {
                malformed(format!("target numbering part is invalid XML: {source}"))
            })?;
        Ok((name, document))
    }
}

fn register_fragment_image(
    context: &mut StoryEditContext<'_, '_, '_>,
    fragment: &WordFragment,
    rid: &str,
) -> Result<MediaRegistration, Error> {
    let relationship = source_relationship(fragment, rid)?;
    if relationship.rel_type != IMAGE_REL_TYPE || relationship.target_mode != TargetMode::Internal {
        return Err(unsupported("non-embedded-image blip relationship"));
    }
    let owner = PartUri::new(&fragment.source_part)?;
    let target =
        resolve_part_target(owner.parent().as_ref(), &relationship.target).ok_or_else(|| {
            malformed(format!(
                "fragment image relationship {rid:?} has an invalid target"
            ))
        })?;
    let bytes = fragment
        .package
        .part(target.as_str())
        .ok_or_else(|| OpcError::MissingPart {
            uri: target.as_str().to_string(),
        })?
        .bytes()?;
    context
        .transaction
        .register_media_bytes(target.file_name(), Arc::from(bytes))
}

fn register_fragment_hyperlink(
    context: &mut StoryEditContext<'_, '_, '_>,
    fragment: &WordFragment,
    rid: &str,
) -> Result<String, Error> {
    let relationship = source_relationship(fragment, rid)?;
    if relationship.rel_type != HYPERLINK_REL_TYPE
        || relationship.target_mode != TargetMode::External
    {
        return Err(unsupported("non-external hyperlink relationship"));
    }
    context
        .transaction
        .relate_external_hyperlink(&context.owner, &relationship.target)
}

fn source_relationship<'a>(
    fragment: &'a WordFragment,
    rid: &str,
) -> Result<&'a docxtpl_opc::Relationship, Error> {
    fragment
        .package
        .relationships_of(&fragment.source_part)
        .and_then(|relationships| relationships.get(rid))
        .ok_or_else(|| malformed(format!("fragment relationship {rid:?} does not exist")))
}

fn validate_fragment_features(fragment: &WordFragment) -> Result<(), Error> {
    if fragment.roots.is_empty() {
        return Err(malformed("WordML fragment selection is empty"));
    }
    for root in fragment.roots.iter().copied() {
        if !is_word_element_document(&fragment.document, root, "p")
            && !is_word_element_document(&fragment.document, root, "tbl")
        {
            return Err(malformed("WordML fragment roots must be w:p or w:tbl"));
        }
        for node in fragment.document.descendants(root) {
            let Some(tag) = fragment.document.tag(node) else {
                continue;
            };
            let feature = if tag.ns == CHART_NS {
                Some("chart")
            } else if tag.ns == DIAGRAM_NS {
                Some("smartart")
            } else if (tag.ns == OFFICE_NS && tag.local.eq_ignore_ascii_case("OLEObject"))
                || (tag.ns == ns_uri::W && tag.local == "object")
            {
                Some("ole")
            } else if tag.ns == VML_NS && tag.local == "imagedata" {
                Some("vml-image")
            } else if tag.ns == ns_uri::W
                && matches!(
                    tag.local.as_str(),
                    "altChunk"
                        | "footnoteReference"
                        | "endnoteReference"
                        | "commentReference"
                        | "headerReference"
                        | "footerReference"
                )
            {
                Some(tag.local.as_str())
            } else {
                None
            };
            if let Some(feature) = feature {
                return Err(unsupported(feature));
            }
        }
    }
    Ok(())
}

fn validate_numbering_features(numbering: &XmlDocument) -> Result<(), Error> {
    for node in numbering.descendants(numbering.root()) {
        if is_word_element_document(numbering, node, "numPicBullet") {
            return Err(unsupported("picture-numbering"));
        }
        if numbering
            .attrs(node)
            .iter()
            .any(|(name, _)| name.ns == ns_uri::R)
        {
            return Err(unsupported("relationship-bearing-numbering"));
        }
    }
    Ok(())
}

fn selected_descendants(fragment: &WordFragment) -> Vec<NodeId> {
    fragment
        .roots
        .iter()
        .flat_map(|root| fragment.document.descendants(*root))
        .collect()
}

fn selected_numbering_ids(fragment: &WordFragment) -> Result<Vec<i64>, Error> {
    let mut ids = HashSet::new();
    for node in selected_descendants(fragment) {
        if is_word_element_document(&fragment.document, node, "numId") {
            let value = fragment
                .document
                .attr(node, ns_uri::W, "val")
                .ok_or_else(|| malformed("fragment w:numId is missing w:val"))?
                .parse::<i64>()
                .map_err(|_| malformed("fragment w:numId has a non-integer w:val"))?;
            ids.insert(value);
        }
    }
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort_unstable();
    Ok(ids)
}

fn load_related_numbering(
    package: &Package,
    source_part: &str,
) -> Result<Option<XmlDocument>, Error> {
    let source_main = package.main_document_uri()?.as_str().to_string();
    let owner = if source_part == source_main {
        source_part
    } else {
        source_main.as_str()
    };
    let Some(name) = related_part_name(package, owner, NUMBERING_REL_TYPE)? else {
        return Ok(None);
    };
    let bytes = package
        .part(&name)
        .ok_or_else(|| OpcError::MissingPart { uri: name.clone() })?
        .bytes()?;
    let xml = decode_xml_bytes(bytes, &name)?;
    let document = XmlDocument::parse_strict(&xml, &XmlLimits::default())
        .map_err(|source| malformed(format!("source numbering part is invalid XML: {source}")))?;
    Ok(Some(document))
}

fn related_part_name(
    package: &Package,
    owner: &str,
    relationship_type: &str,
) -> Result<Option<String>, Error> {
    let Some(relationship) = package
        .relationships_of(owner)
        .into_iter()
        .flat_map(|relationships| relationships.iter())
        .find(|relationship| {
            relationship.rel_type == relationship_type
                && relationship.target_mode == TargetMode::Internal
        })
    else {
        return Ok(None);
    };
    let owner = PartUri::new(owner)?;
    Ok(
        resolve_part_target(owner.parent().as_ref(), &relationship.target)
            .map(|part| part.as_str().to_string()),
    )
}

fn find_numbering_node(
    document: &XmlDocument,
    local: &str,
    id_attribute: &str,
    id: i64,
) -> Option<NodeId> {
    let id = id.to_string();
    document
        .children(document.root())
        .iter()
        .copied()
        .find(|node| {
            is_word_element_document(document, *node, local)
                && document.attr(*node, ns_uri::W, id_attribute) == Some(id.as_str())
        })
}

fn next_numbering_id(
    document: &XmlDocument,
    local: &str,
    attribute: &str,
    minimum: i64,
) -> Result<i64, Error> {
    let mut next = minimum;
    for node in document.children(document.root()) {
        if is_word_element_document(document, *node, local) {
            if let Some(value) = document
                .attr(*node, ns_uri::W, attribute)
                .and_then(|value| value.parse::<i64>().ok())
            {
                next = next.max(
                    value
                        .checked_add(1)
                        .ok_or_else(|| malformed("numbering id space is exhausted"))?,
                );
            }
        }
    }
    Ok(next)
}

fn insert_before_first_num(document: &mut XmlDocument, node: NodeId) {
    let root = document.root();
    if let Some((position, _)) = document
        .children(root)
        .iter()
        .enumerate()
        .find(|(_, child)| is_word_element_document(document, **child, "num"))
    {
        document.insert_child_at(root, position, node);
    } else {
        document.append_child(root, node);
    }
}

fn bookmark_starts_in(document: &XmlDocument, root: NodeId) -> Vec<(NodeId, String, String)> {
    document
        .descendants(root)
        .into_iter()
        .filter(|node| is_word_element_document(document, *node, "bookmarkStart"))
        .filter_map(|node| {
            Some((
                node,
                document.attr(node, ns_uri::W, "id")?.to_string(),
                document.attr(node, ns_uri::W, "name")?.to_string(),
            ))
        })
        .collect()
}

fn unique_fragment_bookmark_name(preferred: &str, used: &mut HashSet<String>) -> String {
    let normalized: String = preferred
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    let base = if normalized.is_empty() || normalized.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{normalized}")
    } else {
        normalized
    };
    if used.insert(base.clone()) {
        return base;
    }
    for suffix in 1u64.. {
        let suffix = format!("_{suffix}");
        let prefix_len = 40usize.saturating_sub(suffix.len());
        let candidate = format!(
            "{}{}",
            base.chars().take(prefix_len).collect::<String>(),
            suffix
        );
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("u64 bookmark suffix space is sufficient")
}

fn attach_roots(
    document: &mut XmlDocument,
    target: NodeId,
    roots: &[NodeId],
    placement: FragmentPlacement,
) -> Result<(), Error> {
    match placement {
        FragmentPlacement::Append => {
            for root in roots {
                document.append_child(target, *root);
            }
        }
        FragmentPlacement::Before | FragmentPlacement::After => {
            let parent = document
                .parent(target)
                .ok_or_else(|| malformed("fragment target has no parent"))?;
            let target_position = document
                .children(parent)
                .iter()
                .position(|node| *node == target)
                .ok_or_else(|| malformed("fragment target is not attached to its parent"))?;
            let first_position =
                target_position + usize::from(matches!(placement, FragmentPlacement::After));
            for (offset, root) in roots.iter().enumerate() {
                document.insert_child_at(parent, first_position + offset, *root);
            }
        }
    }
    Ok(())
}

fn validate_target(
    document: &XmlDocument,
    target: NodeId,
    placement: FragmentPlacement,
) -> Result<(), Error> {
    if document.tag(target).is_none() {
        return Err(malformed("fragment target must be an element"));
    }
    if !matches!(placement, FragmentPlacement::Append) && document.parent(target).is_none() {
        return Err(malformed(
            "fragment target must be attached below the Story root",
        ));
    }
    Ok(())
}

fn is_word_element_document(document: &XmlDocument, node: NodeId, local: &str) -> bool {
    document
        .tag(node)
        .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == local)
}

fn malformed(reason: impl Into<String>) -> Error {
    OpcError::Malformed {
        reason: reason.into(),
    }
    .into()
}

fn unsupported(feature: impl Into<String>) -> Error {
    Error::Opc(OpcError::Malformed {
        reason: format!("unsupported WordML fragment feature {:?}", feature.into()),
    })
}
