//! Transactional import of paragraph/table WordML fragments.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;

use docxtpl_opc::{
    resolve_part_target, OpcError, Package, PackageLimits, PartUri, Relationship, TargetMode,
};
use docxtpl_xml::{ns_uri, NodeId, QName, XmlDocument, XmlLimits};

use super::{
    allocate_positive_id, decode_xml_bytes, is_word_element, Error, MediaRegistration,
    StoryEditContext, StoryResources, HYPERLINK_REL_TYPE, IMAGE_REL_TYPE,
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

/// Resource limits applied while opening and importing a fragment source.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FragmentImportLimits {
    /// Maximum compressed source DOCX bytes accepted by [`FragmentDocument::from_docx_bytes`].
    pub max_source_docx_bytes: u64,
    /// OPC ZIP entry and expansion limits.
    pub package: PackageLimits,
    /// Maximum XML bytes for one selected Story part.
    pub max_story_xml_bytes: u64,
    /// Maximum selected top-level paragraph/table nodes.
    pub max_root_nodes: usize,
    /// Maximum total nodes across the selected subtrees.
    pub max_total_nodes: usize,
    /// Maximum distinct relationship references resolved by one import.
    pub max_relationships: usize,
    /// Maximum source bytes read across default media imports.
    pub max_media_bytes: u64,
    /// Maximum resolver invocations for one import.
    pub max_resolver_calls: usize,
    /// Maximum relationship/bookmark mappings retained in a detailed report.
    pub max_report_mappings: usize,
}

impl Default for FragmentImportLimits {
    fn default() -> Self {
        Self {
            max_source_docx_bytes: 128 * 1024 * 1024,
            package: PackageLimits::default(),
            max_story_xml_bytes: 64 * 1024 * 1024,
            max_root_nodes: 4_096,
            max_total_nodes: 1_000_000,
            max_relationships: 16_384,
            max_media_bytes: 512 * 1024 * 1024,
            max_resolver_calls: 16_384,
            max_report_mappings: 4_096,
        }
    }
}

/// A reusable, limits-bound source DOCX for selecting one or more Stories.
pub struct FragmentDocument {
    package: Arc<Package>,
    limits: FragmentImportLimits,
}

/// Node handle scoped by the owning [`WordFragment`] source DOM.
pub type FragmentNodeId = NodeId;

/// Parsed source Story returned by [`FragmentDocument::story`].
pub type FragmentStory = WordFragment;

impl FragmentDocument {
    /// Open a source DOCX from immutable shared bytes under explicit limits.
    pub fn from_docx_bytes(bytes: Arc<[u8]>, limits: &FragmentImportLimits) -> Result<Self, Error> {
        if bytes.len() as u64 > limits.max_source_docx_bytes {
            return Err(OpcError::LimitExceeded {
                kind: "fragment_source_docx",
                value: bytes.len() as u64,
                max: limits.max_source_docx_bytes,
            }
            .into());
        }
        let package = Package::from_reader(Cursor::new(bytes), &limits.package)?;
        Self::from_package(package, limits)
    }

    /// Wrap an already-opened source package.
    pub fn from_package(package: Package, limits: &FragmentImportLimits) -> Result<Self, Error> {
        package.validate()?;
        Ok(Self {
            package: Arc::new(package),
            limits: limits.clone(),
        })
    }

    /// Parse one source Story and select its direct paragraph/table children.
    pub fn story(&self, part_name: &str) -> Result<WordFragment, Error> {
        WordFragment::from_shared_package(
            Arc::clone(&self.package),
            part_name.to_string(),
            self.limits.clone(),
        )
    }
}

/// Target insertion point for the additive fragment-import API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FragmentInsertion {
    /// Append imported roots as children of the target element.
    AppendTo { parent: NodeId },
    /// Insert imported roots immediately before the anchor.
    Before { anchor: NodeId },
    /// Insert imported roots immediately after the anchor.
    After { anchor: NodeId },
    /// Replace the target element with imported roots.
    Replace { target: NodeId },
}

/// Handling for an otherwise unsupported relationship-bearing node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnsupportedRelationshipPolicy {
    /// Reject the import before attaching nodes.
    #[default]
    Reject,
    /// Remove the relationship-bearing source element and record a warning.
    Skip,
}

/// External hyperlink allow-list policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExternalLinkPolicy {
    /// Permit only HTTP and HTTPS targets.
    #[default]
    HttpHttps,
    /// Reject all external hyperlinks.
    Reject,
    /// Permit any non-empty URI scheme explicitly supplied by the source/resolver.
    AllowAnyScheme,
}

/// Options for the resolver-capable import entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FragmentImportSettings {
    /// Policy for unknown relationship-bearing elements.
    pub unsupported_relationships: UnsupportedRelationshipPolicy,
    /// Policy for external hyperlink schemes.
    pub external_links: ExternalLinkPolicy,
    /// Validate the target Story's bookmark/internal-link integrity after import.
    pub validate_after_import: bool,
    /// Per-import resource limits.
    pub limits: FragmentImportLimits,
}

impl Default for FragmentImportSettings {
    fn default() -> Self {
        Self {
            unsupported_relationships: UnsupportedRelationshipPolicy::Reject,
            external_links: ExternalLinkPolicy::HttpHttps,
            validate_after_import: true,
            limits: FragmentImportLimits::default(),
        }
    }
}

/// Read-only description of one source relationship presented to a resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FragmentRelationshipView {
    /// Source Story owner part.
    pub source_owner: String,
    /// Source relationship id referenced by WordML.
    pub relationship_id: String,
    /// OPC relationship type URI.
    pub relationship_type: String,
    /// Relationship target exactly as stored in the source package.
    pub target: String,
    /// Internal or external target mode.
    pub target_mode: TargetMode,
    /// Safely resolved internal part URI, when applicable.
    pub resolved_part: Option<PartUri>,
    /// Source content type, when the internal part has one.
    pub content_type: Option<String>,
}

/// Source payload available to a fragment resource resolver.
#[non_exhaustive]
pub enum FragmentResourceSource<'a> {
    /// Relationship has no package-part payload, such as an external hyperlink.
    None,
    /// Bounded bytes of an internal source package part.
    Part { name: &'a PartUri, bytes: &'a [u8] },
}

/// Resolver decision for one distinct source relationship.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FragmentResourceDecision {
    /// Apply the library's default image/hyperlink behavior.
    ImportDefault,
    /// Use caller-registered media instead of the source image part.
    UseMedia(MediaRegistration),
    /// Use a caller-selected external hyperlink target.
    UseExternalHyperlink(String),
    /// Use a caller-selected relationship. The first implementation accepts
    /// external hyperlink registrations; package-part relationships remain
    /// subject to the built-in safe import paths.
    UseRelationship(RelationshipRegistration),
    /// Remove the relationship-bearing element when skipping is enabled.
    Skip,
    /// Reject the import with a safe diagnostic.
    Reject { reason: String },
}

/// Caller-selected relationship registration returned by a resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RelationshipRegistration {
    /// OPC relationship type URI.
    pub relationship_type: String,
    /// Target exactly as it should appear in the target Story's `.rels` part.
    pub target: String,
    /// Internal or external target mode.
    pub target_mode: TargetMode,
}

impl RelationshipRegistration {
    /// Construct an external hyperlink registration.
    #[must_use]
    pub fn external_hyperlink(target: impl Into<String>) -> Self {
        Self {
            relationship_type: HYPERLINK_REL_TYPE.to_string(),
            target: target.into(),
            target_mode: TargetMode::External,
        }
    }
}

/// Resolves or replaces resources referenced by imported WordML.
pub trait FragmentResourceResolver {
    /// Resolve one distinct source relationship. Repeated references reuse the decision.
    fn resolve(
        &mut self,
        relationship: &FragmentRelationshipView,
        source: &FragmentResourceSource<'_>,
        resources: &mut StoryResources<'_, '_, '_>,
    ) -> Result<FragmentResourceDecision, Error>;
}

/// Default resolver that imports supported source resources unchanged.
#[derive(Debug, Default)]
pub struct DefaultFragmentResourceResolver;

impl FragmentResourceResolver for DefaultFragmentResourceResolver {
    fn resolve(
        &mut self,
        _relationship: &FragmentRelationshipView,
        _source: &FragmentResourceSource<'_>,
        _resources: &mut StoryResources<'_, '_, '_>,
    ) -> Result<FragmentResourceDecision, Error> {
        Ok(FragmentResourceDecision::ImportDefault)
    }
}

/// One source-to-target relationship id mapping retained in a report.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RelationshipIdMapping {
    /// Source relationship id.
    pub source_id: String,
    /// Target relationship id, or `None` when skipped.
    pub target_id: Option<String>,
}

/// Non-fatal fragment import diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FragmentImportWarning {
    /// Stable warning category.
    pub code: &'static str,
    /// Safe human-readable detail.
    pub message: String,
}

/// Detailed report from the resolver-capable import API.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DetailedFragmentImportReport {
    /// Compatibility-preserving summary.
    pub summary: FragmentImportReport,
    /// Total nodes copied across all roots.
    pub imported_total_nodes: usize,
    /// Source media bytes read by default imports.
    pub media_bytes_read: u64,
    /// Source-to-target relationship mappings, bounded by import limits.
    pub relationship_map: Vec<RelationshipIdMapping>,
    /// Whether mapping entries were truncated.
    pub mappings_truncated: bool,
    /// Non-fatal decisions such as explicitly skipped relationships.
    pub warnings: Vec<FragmentImportWarning>,
}

/// A selected set of top-level paragraphs/tables from one source DOCX part.
///
/// Construction parses the source part once. By default, a main-document
/// source selects all direct `w:p`/`w:tbl` children of `w:body`; other parts
/// select direct paragraph/table children of their root.
pub struct WordFragment {
    package: Arc<Package>,
    source_part: String,
    document: XmlDocument,
    roots: Vec<NodeId>,
    all_roots: Vec<NodeId>,
    available_roots: HashSet<NodeId>,
    limits: FragmentImportLimits,
}

impl WordFragment {
    /// Parse and select all supported top-level nodes from `source_part`.
    #[deprecated(
        since = "1.3.2",
        note = "use FragmentDocument::from_package with FragmentDocument::story"
    )]
    pub fn from_package(package: Package, source_part: impl Into<String>) -> Result<Self, Error> {
        Self::from_shared_package(
            Arc::new(package),
            source_part.into(),
            FragmentImportLimits::default(),
        )
    }

    /// Open a source DOCX from shared bytes and select one Story under explicit limits.
    pub fn from_docx_bytes(
        bytes: Arc<[u8]>,
        source_part: impl Into<String>,
        limits: &FragmentImportLimits,
    ) -> Result<Self, Error> {
        FragmentDocument::from_docx_bytes(bytes, limits)?.story(&source_part.into())
    }

    fn from_shared_package(
        package: Arc<Package>,
        source_part: String,
        limits: FragmentImportLimits,
    ) -> Result<Self, Error> {
        let bytes = package
            .part(&source_part)
            .ok_or_else(|| OpcError::MissingPart {
                uri: source_part.clone(),
            })?
            .bytes()?;
        if bytes.len() as u64 > limits.max_story_xml_bytes {
            return Err(OpcError::LimitExceeded {
                kind: "fragment_story_xml",
                value: bytes.len() as u64,
                max: limits.max_story_xml_bytes,
            }
            .into());
        }
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
        if roots.len() > limits.max_root_nodes {
            return Err(OpcError::LimitExceeded {
                kind: "fragment_root_nodes",
                value: roots.len() as u64,
                max: limits.max_root_nodes as u64,
            }
            .into());
        }
        let available_roots = roots.iter().copied().collect();
        let all_roots = roots.clone();
        Ok(Self {
            package,
            source_part,
            document,
            roots,
            all_roots,
            available_roots,
            limits,
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

    /// Direct paragraph/table children selected from the Story container.
    #[must_use]
    pub fn body_children(&self) -> Vec<NodeId> {
        self.all_roots.clone()
    }

    /// Direct element children of a source node.
    #[must_use]
    pub fn element_children(&self, parent: NodeId) -> Vec<NodeId> {
        self.document
            .children(parent)
            .iter()
            .copied()
            .filter(|node| self.document.tag(*node).is_some())
            .collect()
    }

    /// Expanded source element name.
    #[must_use]
    pub fn tag(&self, node: NodeId) -> Option<&QName> {
        self.document.tag(node)
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
        if roots.len() > self.limits.max_root_nodes {
            return Err(OpcError::LimitExceeded {
                kind: "fragment_root_nodes",
                value: roots.len() as u64,
                max: self.limits.max_root_nodes as u64,
            }
            .into());
        }
        let total_nodes = roots
            .iter()
            .map(|root| self.document.descendants(*root).len())
            .sum::<usize>();
        if total_nodes > self.limits.max_total_nodes {
            return Err(OpcError::LimitExceeded {
                kind: "fragment_total_nodes",
                value: total_nodes as u64,
                max: self.limits.max_total_nodes as u64,
            }
            .into());
        }
        self.roots = roots;
        Ok(())
    }
}

impl StoryEditContext<'_, '_, '_> {
    /// Import selected paragraphs/tables and remap their package resources.
    #[deprecated(
        since = "1.3.2",
        note = "use import_fragment_with_resolver with FragmentInsertion and FragmentImportSettings"
    )]
    pub fn import_fragment(
        &mut self,
        target: NodeId,
        fragment: WordFragment,
        options: FragmentImportOptions,
    ) -> Result<FragmentImportReport, Error> {
        let insertion = match options.placement {
            FragmentPlacement::Before => FragmentInsertion::Before { anchor: target },
            FragmentPlacement::After => FragmentInsertion::After { anchor: target },
            FragmentPlacement::Append => FragmentInsertion::AppendTo { parent: target },
        };
        let settings = FragmentImportSettings {
            external_links: ExternalLinkPolicy::AllowAnyScheme,
            limits: fragment.limits.clone(),
            ..FragmentImportSettings::default()
        };
        self.import_fragment_with_resolver(
            fragment,
            insertion,
            &settings,
            &mut DefaultFragmentResourceResolver,
        )
        .map(|report| report.summary)
    }

    /// Import a selected fragment with explicit insertion, policies, limits,
    /// and caller-controlled resource replacement.
    pub fn import_fragment_with_resolver<R: FragmentResourceResolver>(
        &mut self,
        mut fragment: WordFragment,
        insertion: FragmentInsertion,
        settings: &FragmentImportSettings,
        resolver: &mut R,
    ) -> Result<DetailedFragmentImportReport, Error> {
        self.transaction.check_control()?;
        let (target, placement, replace) = match insertion {
            FragmentInsertion::AppendTo { parent } => (parent, FragmentPlacement::Append, false),
            FragmentInsertion::Before { anchor } => (anchor, FragmentPlacement::Before, false),
            FragmentInsertion::After { anchor } => (anchor, FragmentPlacement::After, false),
            FragmentInsertion::Replace { target } => (target, FragmentPlacement::Before, true),
        };
        validate_target(self.story.document(), target, placement)?;
        validate_import_limits(&fragment, &settings.limits)?;
        validate_fragment_features(&fragment)?;

        let numbering_instances = self.remap_fragment_numbering(&mut fragment)?;
        let mut image_mapping = HashMap::new();
        let mut hyperlink_mapping = HashMap::new();
        let mut details = RelationshipImportDetails::default();
        self.remap_fragment_relationships_with_resolver(
            &mut fragment,
            &mut image_mapping,
            &mut hyperlink_mapping,
            settings,
            resolver,
            &mut details,
        )?;
        self.remap_fragment_bookmarks(&mut fragment)?;

        let imported_total_nodes = fragment
            .roots
            .iter()
            .map(|root| fragment.document.descendants(*root).len())
            .sum();
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
        attach_roots(self.story.document_mut(), target, &inserted, placement)?;
        if replace {
            self.story.document_mut().detach(target);
        }
        if settings.validate_after_import {
            self.story.inner.validate_internal_links = true;
            self.story.inner.validate_bookmarks_and_internal_links()?;
        }

        Ok(DetailedFragmentImportReport {
            summary: FragmentImportReport {
                inserted_nodes: inserted.len(),
                image_relationships: image_mapping.len(),
                hyperlink_relationships: hyperlink_mapping.len(),
                numbering_instances,
                roots: inserted,
            },
            imported_total_nodes,
            media_bytes_read: details.media_bytes_read,
            relationship_map: details.relationship_map,
            mappings_truncated: details.mappings_truncated,
            warnings: details.warnings,
        })
    }

    fn remap_fragment_relationships_with_resolver<R: FragmentResourceResolver>(
        &mut self,
        fragment: &mut WordFragment,
        image_mapping: &mut HashMap<String, String>,
        hyperlink_mapping: &mut HashMap<String, String>,
        settings: &FragmentImportSettings,
        resolver: &mut R,
        details: &mut RelationshipImportDetails,
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
                if !is_image && !is_hyperlink {
                    match settings.unsupported_relationships {
                        UnsupportedRelationshipPolicy::Reject => {
                            return Err(unsupported(format!(
                                "relationship attribute {}:{}@r:{attribute}",
                                tag_ns, tag_local
                            )));
                        }
                        UnsupportedRelationshipPolicy::Skip => {
                            let distinct_relationships = details
                                .resolved_relationships
                                .len()
                                .saturating_add(details.skipped_relationships.len());
                            if !details.skipped_relationships.contains(&old_rid)
                                && distinct_relationships >= settings.limits.max_relationships
                            {
                                return Err(OpcError::LimitExceeded {
                                    kind: "fragment_relationships",
                                    value: (distinct_relationships + 1) as u64,
                                    max: settings.limits.max_relationships as u64,
                                }
                                .into());
                            }
                            fragment.document.detach(node);
                            details.skipped_relationships.insert(old_rid.clone());
                            details.warnings.push(FragmentImportWarning {
                                code: "fragment.relationship_skipped",
                                message: format!(
                                    "skipped unsupported relationship attribute {}:{}@r:{attribute}",
                                    tag_ns, tag_local
                                ),
                            });
                            details.record_mapping(&old_rid, None, &settings.limits);
                            continue;
                        }
                    }
                }

                let existing = if is_image {
                    image_mapping.get(&old_rid)
                } else {
                    hyperlink_mapping.get(&old_rid)
                };
                if let Some(mapped) = existing {
                    fragment
                        .document
                        .set_attr(node, ns_uri::R, &attribute, mapped.clone());
                    continue;
                }
                if details.skipped_relationships.contains(&old_rid) {
                    fragment.document.detach(node);
                    continue;
                }
                let distinct_relationships = details
                    .resolved_relationships
                    .len()
                    .saturating_add(details.skipped_relationships.len());
                if distinct_relationships >= settings.limits.max_relationships {
                    return Err(OpcError::LimitExceeded {
                        kind: "fragment_relationships",
                        value: (distinct_relationships + 1) as u64,
                        max: settings.limits.max_relationships as u64,
                    }
                    .into());
                }
                if details.resolver_calls >= settings.limits.max_resolver_calls {
                    return Err(OpcError::LimitExceeded {
                        kind: "fragment_resolver_calls",
                        value: (details.resolver_calls + 1) as u64,
                        max: settings.limits.max_resolver_calls as u64,
                    }
                    .into());
                }

                let relationship = source_relationship(fragment, &old_rid)?.clone();
                let resolved = resolve_source_relationship(fragment, &relationship)?;
                if let Some((_, bytes)) = resolved.as_ref() {
                    details.source_bytes_exposed = details
                        .source_bytes_exposed
                        .checked_add(bytes.len() as u64)
                        .ok_or_else(|| malformed("fragment source byte count overflow"))?;
                    if details.source_bytes_exposed > settings.limits.max_media_bytes {
                        return Err(OpcError::LimitExceeded {
                            kind: "fragment_media_bytes",
                            value: details.source_bytes_exposed,
                            max: settings.limits.max_media_bytes,
                        }
                        .into());
                    }
                }
                let view = FragmentRelationshipView {
                    source_owner: fragment.source_part.clone(),
                    relationship_id: old_rid.clone(),
                    relationship_type: relationship.rel_type.clone(),
                    target: relationship.target.clone(),
                    target_mode: relationship.target_mode,
                    resolved_part: resolved.as_ref().map(|(part, _)| part.clone()),
                    content_type: resolved.as_ref().and_then(|(part, _)| {
                        fragment
                            .package
                            .content_types()
                            .content_type_of(part)
                            .map(str::to_string)
                    }),
                };
                let source = match resolved.as_ref() {
                    Some((part, bytes)) => FragmentResourceSource::Part { name: part, bytes },
                    None => FragmentResourceSource::None,
                };
                details.resolver_calls += 1;
                let decision = {
                    let mut resources = self.resources();
                    resolver.resolve(&view, &source, &mut resources)?
                };

                let new_rid = if is_image {
                    if relationship.rel_type != IMAGE_REL_TYPE
                        || relationship.target_mode != TargetMode::Internal
                    {
                        return Err(unsupported("non-embedded-image blip relationship"));
                    }
                    let media = match decision {
                        FragmentResourceDecision::ImportDefault => {
                            let FragmentResourceSource::Part { name, bytes } = source else {
                                return Err(malformed(format!(
                                    "fragment image relationship {old_rid:?} has no source part"
                                )));
                            };
                            details.media_bytes_read = details
                                .media_bytes_read
                                .checked_add(bytes.len() as u64)
                                .ok_or_else(|| malformed("fragment media byte count overflow"))?;
                            if details.media_bytes_read > settings.limits.max_media_bytes {
                                return Err(OpcError::LimitExceeded {
                                    kind: "fragment_media_bytes",
                                    value: details.media_bytes_read,
                                    max: settings.limits.max_media_bytes,
                                }
                                .into());
                            }
                            let mut resources = self.resources();
                            resources.register_media_bytes(name.file_name(), Arc::from(bytes))?
                        }
                        FragmentResourceDecision::UseMedia(media) => media,
                        FragmentResourceDecision::Skip => {
                            ensure_skip_enabled(settings)?;
                            fragment.document.detach(node);
                            details.skipped_relationships.insert(old_rid.clone());
                            details.warnings.push(FragmentImportWarning {
                                code: "fragment.relationship_skipped",
                                message: format!("skipped image relationship {old_rid:?}"),
                            });
                            details.record_mapping(&old_rid, None, &settings.limits);
                            continue;
                        }
                        FragmentResourceDecision::Reject { reason } => {
                            return Err(malformed(format!(
                                "fragment resolver rejected relationship {old_rid:?}: {reason}"
                            )));
                        }
                        FragmentResourceDecision::UseExternalHyperlink(_) => {
                            return Err(malformed(
                                "fragment resolver returned a hyperlink for an image relationship",
                            ));
                        }
                        FragmentResourceDecision::UseRelationship(_) => {
                            return Err(malformed(
                                "fragment resolver returned a raw relationship for an image",
                            ));
                        }
                    };
                    let mapped = {
                        let mut resources = self.resources();
                        resources.relate_image(&media)?
                    };
                    image_mapping.insert(old_rid.clone(), mapped.clone());
                    mapped
                } else if is_hyperlink {
                    if relationship.rel_type != HYPERLINK_REL_TYPE
                        || relationship.target_mode != TargetMode::External
                    {
                        return Err(unsupported("non-external hyperlink relationship"));
                    }
                    let target = match decision {
                        FragmentResourceDecision::ImportDefault => relationship.target.clone(),
                        FragmentResourceDecision::UseExternalHyperlink(target) => target,
                        FragmentResourceDecision::UseRelationship(registration) => {
                            if registration.relationship_type != HYPERLINK_REL_TYPE
                                || registration.target_mode != TargetMode::External
                            {
                                return Err(malformed(
                                    "fragment resolver relationship is not an external hyperlink",
                                ));
                            }
                            registration.target
                        }
                        FragmentResourceDecision::Skip => {
                            ensure_skip_enabled(settings)?;
                            fragment.document.detach(node);
                            details.skipped_relationships.insert(old_rid.clone());
                            details.warnings.push(FragmentImportWarning {
                                code: "fragment.relationship_skipped",
                                message: format!("skipped hyperlink relationship {old_rid:?}"),
                            });
                            details.record_mapping(&old_rid, None, &settings.limits);
                            continue;
                        }
                        FragmentResourceDecision::Reject { reason } => {
                            return Err(malformed(format!(
                                "fragment resolver rejected relationship {old_rid:?}: {reason}"
                            )));
                        }
                        FragmentResourceDecision::UseMedia(_) => {
                            return Err(malformed(
                                "fragment resolver returned media for a hyperlink relationship",
                            ));
                        }
                    };
                    validate_external_target(&target, settings.external_links)?;
                    let mapped = {
                        let mut resources = self.resources();
                        resources.relate_external_hyperlink(&target)?
                    };
                    hyperlink_mapping.insert(old_rid.clone(), mapped.clone());
                    mapped
                } else {
                    unreachable!("relationship kind checked above")
                };
                details.resolved_relationships.insert(old_rid.clone());
                details.record_mapping(&old_rid, Some(new_rid.clone()), &settings.limits);
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

#[derive(Default)]
struct RelationshipImportDetails {
    media_bytes_read: u64,
    source_bytes_exposed: u64,
    resolver_calls: usize,
    resolved_relationships: HashSet<String>,
    skipped_relationships: HashSet<String>,
    relationship_map: Vec<RelationshipIdMapping>,
    mappings_truncated: bool,
    warnings: Vec<FragmentImportWarning>,
}

impl RelationshipImportDetails {
    fn record_mapping(
        &mut self,
        source_id: &str,
        target_id: Option<String>,
        limits: &FragmentImportLimits,
    ) {
        if self.relationship_map.len() < limits.max_report_mappings {
            self.relationship_map.push(RelationshipIdMapping {
                source_id: source_id.to_string(),
                target_id,
            });
        } else {
            self.mappings_truncated = true;
        }
    }
}

fn resolve_source_relationship<'a>(
    fragment: &'a WordFragment,
    relationship: &Relationship,
) -> Result<Option<(PartUri, &'a [u8])>, Error> {
    if relationship.target_mode == TargetMode::External {
        return Ok(None);
    }
    let owner = PartUri::new(&fragment.source_part)?;
    let target =
        resolve_part_target(owner.parent().as_ref(), &relationship.target).ok_or_else(|| {
            malformed(format!(
                "fragment relationship {:?} has an unsafe or invalid target {:?}",
                relationship.id, relationship.target
            ))
        })?;
    let bytes = fragment
        .package
        .part(target.as_str())
        .ok_or_else(|| OpcError::MissingPart {
            uri: target.as_str().to_string(),
        })?
        .bytes()?;
    Ok(Some((target, bytes)))
}

fn ensure_skip_enabled(settings: &FragmentImportSettings) -> Result<(), Error> {
    if settings.unsupported_relationships == UnsupportedRelationshipPolicy::Skip {
        Ok(())
    } else {
        Err(malformed(
            "fragment resolver returned Skip but relationship skipping is disabled",
        ))
    }
}

fn validate_external_target(target: &str, policy: ExternalLinkPolicy) -> Result<(), Error> {
    let scheme = target
        .split_once(':')
        .map(|(scheme, _)| scheme)
        .filter(|scheme| {
            !scheme.is_empty()
                && scheme.chars().enumerate().all(|(index, character)| {
                    if index == 0 {
                        character.is_ascii_alphabetic()
                    } else {
                        character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
                    }
                })
        })
        .ok_or_else(|| malformed("fragment external hyperlink has no valid URI scheme"))?;
    match policy {
        ExternalLinkPolicy::Reject => Err(unsupported("external-hyperlink")),
        ExternalLinkPolicy::HttpHttps
            if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") =>
        {
            Err(unsupported(format!("external hyperlink scheme {scheme:?}")))
        }
        ExternalLinkPolicy::HttpHttps | ExternalLinkPolicy::AllowAnyScheme => Ok(()),
    }
}

fn validate_import_limits(
    fragment: &WordFragment,
    limits: &FragmentImportLimits,
) -> Result<(), Error> {
    if fragment.roots.len() > limits.max_root_nodes {
        return Err(OpcError::LimitExceeded {
            kind: "fragment_root_nodes",
            value: fragment.roots.len() as u64,
            max: limits.max_root_nodes as u64,
        }
        .into());
    }
    let total_nodes = fragment
        .roots
        .iter()
        .map(|root| fragment.document.descendants(*root).len())
        .sum::<usize>();
    if total_nodes > limits.max_total_nodes {
        return Err(OpcError::LimitExceeded {
            kind: "fragment_total_nodes",
            value: total_nodes as u64,
            max: limits.max_total_nodes as u64,
        }
        .into());
    }
    Ok(())
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
