//! P6 Subdoc merging (ADR-007): an equivalent implementation of `tpl.new_subdoc(docpath)`.
//!
//! Upstream chain: docxtpl `Template.new_subdoc` -> `Subdoc(tpl, docpath)` at construction time
//! runs `SubdocComposer.attach_parts` (docxtpl/subdoc.py, with semantics derived from
//! docxcompose 2.2.0's `Composer`), **merging the parts of the external docx into the main document**:
//! the body fragment is left for jinja injection (the `{{p sd }}` template slot), while styles/numbering/relationships/
//! media and other parts modify the main package directly. This module implements the same orchestration step by step:
//!
//! 1. Open the sub package and parse its document/styles/numbering/footnotes trees
//!    (mirroring the python-docx oxml parser `remove_blank_text`: strip all whitespace);
//! 2. For each direct child of the sub body (skipping `w:sectPr`): referenced-part copying
//!    (add_referenced_parts, including the recursive whole-graph copy in add_relationship) ->
//!    three-branch style merging (add_styles) -> numbering copying (add_numberings) ->
//!    list numbering restart (restart_first_numbering) -> image merging (add_images) ->
//!    SmartArt/VML/footnotes merging -> remove header/footer references;
//! 3. Style merging from the sub footnotes (add_styles_from_other_parts);
//! 4. Renumber main-document bookmark/docPr/cNvPr (the three renumber siblings, acting on
//!    the main package body and header/footer parts, matching the upstream order: bookmarkStart and bookmarkEnd
//!    each count from 0, while docPr/cNvPr continue across parts after the body);
//! 5. Section-type fix (fix_section_types: when both sides have multiple sections, change the main section start type);
//! 6. Build the fragment: after removing the body's direct sectPr, the remaining children are concatenated in order without namespace
//!    declarations (upstream `_get_xml` drops the promoted declarations when stripping the body tags),
//!    and returned as [`RenderValue::Subdoc`].
//!
//! Write-back: the main document/styles/numbering and header/footer parts are written in python-docx serialized form only when their trees are actually
//! modified (dirty gating; unchanged bytes are preserved as-is);
//! copied non-image parts and their rels/Content Types changes land in the package immediately; images and the main-document
//! rels are staged via [`ImageInjections`] and committed uniformly by `RenderSession::finish`
//! (render-time images share the same numbering/deduplication state).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};
use std::path::Path;

use docxtpl_opc::{
    relationships_path_of, resolve_part_target, ContentTypes, OpcError, Package, PackageLimits,
    PartUri, Relationship, Relationships, TargetMode,
};
use docxtpl_template::{RenderError, RenderValue, SubdocFragment};
use docxtpl_xml::{ns_uri, NodeId, XmlDocument, XmlLimits};

use crate::images::{relative_to_owner, ImageInjections};
use crate::Error;

/// Image relationship type (consistent with images.rs).
const RT_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
/// Header relationship type.
const RT_HEADER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
/// Footer relationship type.
const RT_FOOTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
/// Footnotes relationship type.
const RT_FOOTNOTES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
/// Styles relationship type.
const RT_STYLES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
/// Numbering relationship type.
const RT_NUMBERING: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
/// Custom-properties relationship type.
const RT_CUSTOM_PROPERTIES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties";
/// SmartArt data/layout/quick-style/colors relationship types.
const RT_DIAGRAM_DATA: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData";
const RT_DIAGRAM_LAYOUT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramLayout";
const RT_DIAGRAM_QUICK_STYLE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramQuickStyle";
const RT_DIAGRAM_COLORS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramColors";

/// Content type of a rels part (registered when creating a new `.rels` part).
const CT_RELS: &str = "application/vnd.openxmlformats-package.relationships+xml";
/// Content type of the numbering part.
const CT_NUMBERING: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
/// Content type of the footnotes part.
const CT_FOOTNOTES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";

/// Empty template used by docxcompose 2.2.0 when the main document has no numbering part.
///
/// Keeps the full set of namespaces, both matching the form of upstream newly created parts and ensuring that
/// Word 2010+ numbering attributes copied in later can reuse the existing prefixes. At save time XML canonicalization runs again as in python-docx,
/// so keeping the double-quoted declarations of the upstream template here causes no difference in final output.
const EMPTY_NUMBERING_XML: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:numbering xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" xmlns:mo="http://schemas.microsoft.com/office/mac/office/2008/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:mv="urn:schemas-microsoft-com:mac:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup" xmlns:wpi="http://schemas.microsoft.com/office/word/2010/wordprocessingInk" xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" mc:Ignorable="w14 w15 wp14"></w:numbering>"#,
);

/// Empty template used by docxcompose 2.2.0 when the main document has no footnotes part.
const EMPTY_FOOTNOTES_XML: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:footnotes xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" xmlns:mo="http://schemas.microsoft.com/office/mac/office/2008/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:mv="urn:schemas-microsoft-com:mac:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup" xmlns:wpi="http://schemas.microsoft.com/office/word/2010/wordprocessingInk" xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" mc:Ignorable="w14 w15 wp14"></w:footnotes>"#,
);

/// The `asvg` prefix (SVG extension blip, upstream add_images' `asvg:svgBlip`).
const NS_ASVG: &str = "http://schemas.microsoft.com/office/drawing/2016/SVG/main";
/// The `dgm` prefix (SmartArt relationship references).
const NS_DGM: &str = "http://schemas.openxmlformats.org/drawingml/2006/diagram";
/// custom document properties namespace.
const NS_CUSTOM_PROPERTIES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties";

/// `tpl.new_subdoc(docpath)` entry: opens the external docx and performs part merging,
/// returning a Subdoc fragment value that can be passed directly to `ctx.insert("sd", ...)`.
pub(crate) fn new_subdoc(
    pkg: &mut Package,
    injections: &mut ImageInjections,
    docpath: &Path,
    limits: &PackageLimits,
) -> Result<RenderValue, Error> {
    // Upstream `Document(docpath)`: a malformed docx fails immediately.
    let sub_pkg = Package::open(docpath, limits)?;
    new_subdoc_from_package(pkg, injections, sub_pkg)
}

/// Merge a subdocument from any seekable DOCX input stream.
///
/// Python's `Document()` accepts both paths and file-likes; the Rust facade splits the two forms
/// into strongly typed entries, so callers do not have to land uploaded content in a temporary file first.
pub(crate) fn new_subdoc_from_reader<R: Read + Seek>(
    pkg: &mut Package,
    injections: &mut ImageInjections,
    reader: R,
    limits: &PackageLimits,
) -> Result<RenderValue, Error> {
    let sub_pkg = Package::from_reader(reader, limits)?;
    new_subdoc_from_package(pkg, injections, sub_pkg)
}

fn new_subdoc_from_package(
    pkg: &mut Package,
    injections: &mut ImageInjections,
    sub_pkg: Package,
) -> Result<RenderValue, Error> {
    sub_pkg.validate()?;
    let mut ctx = SubdocComposer::new(pkg, injections, sub_pkg)?;
    ctx.attach_parts()
}

/// All state for one Subdoc merge (mirrors upstream `SubdocComposer` instance state:
/// the mapping is reset once at the start of attach_parts and shared throughout).
struct SubdocComposer<'a> {
    pkg: &'a mut Package,
    injections: &'a mut ImageInjections,
    /// The sub package (owned; its parts/rels/Content Types are read from it).
    sub_pkg: Package,
    /// Main-document part name (e.g. `word/document.xml`).
    main_name: String,
    /// Sub-document part name.
    sub_main_name: String,
    /// Relationship set of the sub main-document part (upstream `doc.part.rels`).
    sub_rels: Relationships,
    // ---- Numbering mappings (upstream reset_reference_mapping) ----
    /// sub numId -> main numId.
    num_id_mapping: HashMap<i64, i64>,
    /// sub abstractNumId -> main abstractNumId.
    anum_id_mapping: HashMap<i64, i64>,
    /// Style ids whose numbering has been restarted (upstream `self._numbering_restarted`).
    numbering_restarted: HashSet<String>,
    /// SmartArt source part + target relationship type -> main document rId.
    ///
    /// `Part.relate_to` deduplicates the same Part object; this table prevents repeatedly
    /// copying the whole part group when multiple `dgm:relIds` share the same SmartArt.
    diagram_rel_mapping: HashMap<(String, String), String>,
    /// Copied non-image source parts -> main-package part names.
    ///
    /// Besides reusing the shared relationship graph, this map registers target names before recursing into child relationships, so
    /// legitimate OPC relationship cycles such as A -> B -> A close safely without infinite recursion.
    copied_part_mapping: HashMap<String, String>,
    /// sub footnotes old rId -> main footnotes new rId.
    footnote_rel_mapping: HashMap<String, String>,
    // ---- Style mappings (upstream _create_style_id_mapping) ----
    /// sub style id -> name.
    style_id2name: HashMap<String, String>,
    /// Main styles name -> id.
    style_name2id: HashMap<String, String>,
    // ---- Trees (all preloaded, equivalent to python-docx building trees and stripping whitespace on open) ----
    sub_doc: XmlDocument,
    sub_styles: XmlDocument,
    /// Sub footnotes part tree (None when missing, equivalent to upstream KeyError -> pass).
    sub_footnotes: Option<XmlDocument>,
    sub_footnotes_name: Option<String>,
    /// Sub numbering part tree (None when missing: upstream creates an empty part on demand, so queries never match).
    sub_numbering: Option<XmlDocument>,
    main_doc: XmlDocument,
    main_styles: XmlDocument,
    /// Main numbering part tree (None when missing; created from docxcompose's
    /// empty template and attached via a relationship when first needed).
    main_numbering: Option<XmlDocument>,
    /// The main footnotes part and its independent relationship scope.
    main_footnotes: Option<XmlDocument>,
    main_footnotes_rels: Option<Relationships>,
    /// Header/footer part tree cache (docPr/cNvPr renumbering; a part referenced by multiple relationships is parsed once).
    hf_trees: HashMap<String, XmlDocument>,
    // ---- Part names ----
    main_styles_name: String,
    main_numbering_name: Option<String>,
    main_footnotes_name: Option<String>,
    // ---- Content Types changes (python-docx rebuilds them wholesale from parts on save; this implementation
    //      clones then modifies them and writes them back once at the end) ----
    content_types: ContentTypes,
    ct_dirty: bool,
    // ---- Dirty gating: if the tree is unchanged the part bytes are preserved as-is ----
    main_doc_dirty: bool,
    main_styles_dirty: bool,
    main_numbering_dirty: bool,
    main_footnotes_dirty: bool,
    main_footnotes_rels_dirty: bool,
    dirty_hf: HashSet<String>,
}

impl<'a> SubdocComposer<'a> {
    fn new(
        pkg: &'a mut Package,
        injections: &'a mut ImageInjections,
        sub_pkg: Package,
    ) -> Result<Self, Error> {
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let sub_main_name = sub_pkg.main_document_uri()?.as_str().to_string();

        // The sub main document must have a rels file (python-docx's `doc.part.rels` lazily empties, which
        // does not match a standard docx; malformed packages fail immediately).
        let sub_rels = sub_pkg
            .relationships_of(&sub_main_name)
            .cloned()
            .ok_or_else(|| {
                malformed(format!(
                    "subdocument is missing its relationships file (part {sub_main_name})"
                ))
            })?;

        // Tree preload: after python-docx opens a package, every XML part is parsed via remove_blank_text,
        // so serialization (write-back/render input) is always in whitespace-stripped form.
        let main_doc = load_xml_tree(pkg, &main_name)?;
        let sub_doc = load_xml_tree(&sub_pkg, &sub_main_name)?;

        // Main package missing styles.xml: python-docx `doc.styles` raises KeyError directly, treated as main-package unsupported.
        let main_styles_name = related_part(pkg, &main_name, RT_STYLES).ok_or_else(|| {
            malformed("main document is missing the styles part (word/styles.xml), cannot merge subdocument styles")
        })?;
        let main_styles = load_xml_tree(pkg, &main_styles_name)?;

        // Sub missing styles.xml: python-docx likewise raises KeyError, and the subdocument is treated as malformed.
        let sub_styles_name = related_part(&sub_pkg, &sub_main_name, RT_STYLES)
            .ok_or_else(|| malformed("subdocument is missing the styles part (word/styles.xml)"))?;
        let sub_styles = load_xml_tree(&sub_pkg, &sub_styles_name)?;

        let main_numbering_name = related_part(pkg, &main_name, RT_NUMBERING);
        let main_numbering = match &main_numbering_name {
            Some(name) => Some(load_xml_tree(pkg, name)?),
            None => None,
        };
        let sub_numbering = related_part(&sub_pkg, &sub_main_name, RT_NUMBERING)
            .map(|name| load_xml_tree(&sub_pkg, &name))
            .transpose()?;
        let sub_footnotes_name = related_part(&sub_pkg, &sub_main_name, RT_FOOTNOTES);
        let sub_footnotes = sub_footnotes_name
            .as_ref()
            .map(|name| load_xml_tree(&sub_pkg, name))
            .transpose()?;
        let main_footnotes_name = related_part(pkg, &main_name, RT_FOOTNOTES);
        let main_footnotes = main_footnotes_name
            .as_ref()
            .map(|name| load_xml_tree(pkg, name))
            .transpose()?;
        let main_footnotes_rels = main_footnotes_name
            .as_ref()
            .map(|name| pkg.relationships_of(name).cloned().unwrap_or_default());

        // Upstream `_create_style_id_mapping`: style ids are language-dependent while names are relatively stable,
        // so names bridge sub ids to the main document's ids of styles with the same name.
        let style_id2name = style_id_name_map(&sub_styles);
        let style_name2id = style_name_id_map(&main_styles);

        // Content Types are cloned, modified, and written back once at the end (python-docx rebuilds them wholesale from parts on save;
        // here the same declaration table is maintained equivalently).
        let content_types = pkg.content_types().clone();

        Ok(Self {
            pkg,
            injections,
            sub_pkg,
            main_name,
            sub_main_name,
            sub_rels,
            num_id_mapping: HashMap::new(),
            anum_id_mapping: HashMap::new(),
            numbering_restarted: HashSet::new(),
            diagram_rel_mapping: HashMap::new(),
            copied_part_mapping: HashMap::new(),
            footnote_rel_mapping: HashMap::new(),
            style_id2name,
            style_name2id,
            sub_doc,
            sub_styles,
            sub_footnotes,
            sub_footnotes_name,
            sub_numbering,
            main_doc,
            main_styles,
            main_numbering,
            main_footnotes,
            main_footnotes_rels,
            hf_trees: HashMap::new(),
            main_styles_name,
            main_numbering_name,
            main_footnotes_name,
            content_types,
            ct_dirty: false,
            main_doc_dirty: false,
            main_styles_dirty: false,
            main_numbering_dirty: false,
            main_footnotes_dirty: false,
            main_footnotes_rels_dirty: false,
            dirty_hf: HashSet::new(),
        })
    }

    /// Upstream `SubdocComposer.attach_parts` orchestration (docxtpl/subdoc.py).
    fn attach_parts(&mut self) -> Result<RenderValue, Error> {
        // CustomProperties are read from the **package-root relationships**; it does not merge custom.xml
        // into the main package, only resolving same-named DOCPROPERTY fields in the subdocument to cached values.
        self.dissolve_custom_property_fields()?;

        // Iterate direct children of the sub body (skipping w:sectPr), ordered as upstream.
        let sub_body = body_of(&self.sub_doc)?;
        let elements: Vec<NodeId> = self.sub_doc.children(sub_body).to_vec();
        for element in elements {
            if is_tag(&self.sub_doc, element, ns_uri::W, "sectPr") {
                continue;
            }
            self.add_referenced_parts(element)?;
            self.add_styles_in_subdoc(element)?;
            self.add_numberings_in_subdoc(element)?;
            self.restart_first_numbering(element)?;
            self.add_images(element)?;
            self.add_diagrams(element)?;
            self.add_shapes(element)?;
            self.add_footnotes(element)?;
            self.remove_header_and_footer_references(element);
        }

        // Fixed-order tail segment after the loop.
        self.add_styles_from_other_parts()?;
        self.renumber_bookmarks();
        self.renumber_ids(ns_uri::WP, "docPr")?;
        self.renumber_ids(ns_uri::PIC, "cNvPr")?;
        self.fix_section_types()?;

        // Main-tree write-back (dirty gating) and Content Types write-back.
        self.flush_main_parts()?;
        self.flush_content_types()?;

        let main_body = body_of(&self.main_doc)?;
        let namespaces = namespaces_in_scope(&self.main_doc, main_body);
        let fragment =
            SubdocFragment::parse_in_namespace_context(self.build_fragment(), &namespaces)
                .map_err(|source| {
                    Error::Render(RenderError::Xml {
                        part: self.main_name.clone(),
                        source,
                    })
                })?;
        Ok(RenderValue::Subdoc(fragment))
    }

    // ---------------------------------------------------------------
    // CustomProperties.dissolve_fields
    // ---------------------------------------------------------------

    /// Upstream `CustomProperties(doc)` locates `docProps/custom.xml` via the package-root `_rels/.rels`,
    /// then resolves same-named fields in the subdocument using only property names.
    /// The custom part itself is never copied into the main package.
    fn dissolve_custom_property_fields(&mut self) -> Result<(), Error> {
        let Some(custom_name) = root_related_part(
            &self.sub_pkg,
            RT_CUSTOM_PROPERTIES,
            "subdocument custom properties",
        )?
        else {
            return Ok(());
        };
        let properties = load_xml_tree(&self.sub_pkg, &custom_name)?;
        let names: Vec<String> = properties
            .descendants(properties.root())
            .into_iter()
            .filter(|&node| is_tag(&properties, node, NS_CUSTOM_PROPERTIES, "property"))
            .filter_map(|node| properties.attr(node, "", "name").map(str::to_owned))
            .collect();
        if names.is_empty() {
            return Ok(());
        }

        let body = body_of(&self.sub_doc)?;
        for name in names {
            dissolve_simple_fields(&mut self.sub_doc, body, &name)?;
            dissolve_complex_fields(&mut self.sub_doc, body, &name)?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // add_referenced_parts / add_relationship: referenced-part copying
    // ---------------------------------------------------------------

    /// Upstream `add_referenced_parts`: every element with an `r:id` in the element subtree
    /// (in document order); IMAGE/HEADER/FOOTER relationships are skipped (their r:id is kept dangling and handled by
    /// add_images / remove_header_and_footer_references), while the rest get new relationships registered
    /// on the main document with their r:id rewritten.
    fn add_referenced_parts(&mut self, element: NodeId) -> Result<(), Error> {
        let mut rid_elements: Vec<(NodeId, String)> = Vec::new();
        {
            let sub_doc = &self.sub_doc;
            for node in sub_doc.descendants(element).into_iter().skip(1) {
                if sub_doc.tag(node).is_some() {
                    if let Some(rid) = sub_doc.attr(node, ns_uri::R, "id") {
                        rid_elements.push((node, rid.to_owned()));
                    }
                }
            }
        }
        for (node, rid) in rid_elements {
            let rel = self.sub_rels.get(&rid).cloned().ok_or_else(|| {
                malformed(format!(
                    "relationship {rid:?} referenced by a subdocument element does not exist (part {})",
                    self.sub_main_name
                ))
            })?;
            if rel.rel_type == RT_IMAGE || rel.rel_type == RT_HEADER || rel.rel_type == RT_FOOTER {
                continue;
            }
            let new_rid = self.add_relationship(&rel)?;
            self.sub_doc.set_attr(node, ns_uri::R, "id", new_rid);
        }
        Ok(())
    }

    /// Upstream `add_relationship`: external relationships are deduplicated and reused in the main
    /// rels by (reltype, target); internal relationships copy the target part into the main package (including the recursive whole graph)
    /// and are then registered.
    fn add_relationship(&mut self, rel: &Relationship) -> Result<String, Error> {
        if rel.target_mode == TargetMode::External {
            return Ok(self.injections.main_get_or_add(
                &rel.rel_type,
                &rel.target,
                TargetMode::External,
            ));
        }
        let abs_uri = self.resolve_sub_target(&rel.target).ok_or_else(|| {
            malformed(format!(
                "subdocument relationship target {:?} cannot be resolved to an in-package part (part {})",
                rel.target, self.sub_main_name
            ))
        })?;
        let abs_name = abs_uri.as_str().to_string();
        let part = self.sub_pkg.part(&abs_name).ok_or_else(|| {
            malformed(format!("subdocument relationship target part {abs_name} does not exist (dangling relationship)"))
        })?;
        let blob = part.bytes()?.to_vec();
        let content_type = self
            .sub_pkg
            .content_types()
            .content_type_of(&abs_uri)
            .ok_or_else(|| {
                malformed(format!(
                    "subdocument part {abs_name} is missing its content type declaration"
                ))
            })?
            .to_string();
        let new_name = self.copy_part(
            abs_uri.as_str(),
            blob,
            content_type,
            rel.rel_type == RT_IMAGE,
        )?;
        let rel_target = relative_to_owner(&self.main_name, &new_name);
        Ok(self
            .injections
            .main_get_or_add(&rel.rel_type, &rel_target, TargetMode::Internal))
    }

    /// Copy a sub-package part (including its rels subgraph, recursively) into the main package, returning the new part name.
    ///
    /// Mirrors the internal branch of upstream `add_relationship`:
    /// - the partname is renumbered by prefix: the `FILENAME_IDX_RE` prefix plus filling holes among numbers already used by
    ///   main-package parts with the same prefix (first unused in `range(1, len+2)`);
    /// - the source part's rels are copied one by one in original insertion order (external ones registered as-is, internal
    ///   target parts copied recursively); copies keep any legal source Relationship.id so that
    ///   r:* references inside part blobs copied verbatim keep resolving, with only internal Targets recomputed;
    /// - Content Types are registered per python-docx `_ContentTypesItem._add_part`.
    fn copy_part(
        &mut self,
        src_abs: &str,
        blob: Vec<u8>,
        content_type: String,
        is_image_relation: bool,
    ) -> Result<String, Error> {
        let ext = extension_of(src_abs);
        // Images must share ImageInjections' package-level sha1 and imageN allocation state with
        // the body/VML/render-time InlineImages. If copy_part landed in the package directly, it could only see
        // the snapshot at session creation and not the pending media not yet applied; conversely the
        // pending path would not see parts newly added here, and they would eventually contend for the same partname.
        // Parent parts/rels may reference pending media first; at finish, images land before rels.
        if is_image_relation {
            return Ok(self
                .injections
                .add_subdoc_image_part(&blob, &ext, &content_type));
        }
        if let Some(existing) = self.copied_part_mapping.get(src_abs) {
            return Ok(existing.clone());
        }

        let prefix = filename_idx_prefix(src_abs).ok_or_else(|| {
            malformed(format!(
                "part name {src_abs:?} does not match FILENAME_IDX_RE (does not start with a letter), cannot copy part"
            ))
        })?;

        // Occupied numbers of same-prefix parts in the main package (including ones copied and landed this round).
        let mut used: HashSet<i64> = HashSet::new();
        for part in self.pkg.parts() {
            let name = part.uri().as_str();
            if name.starts_with(&prefix) {
                if let Some(number) = filename_idx_number(name, prefix.len()) {
                    used.insert(number);
                }
            }
        }
        for name in self.copied_part_mapping.values() {
            if name.starts_with(&prefix) {
                if let Some(number) = filename_idx_number(name, prefix.len()) {
                    used.insert(number);
                }
            }
        }
        // Upstream first unused number in `range(1, len(used)+2)`.
        let next_number = (1..=(used.len() as i64 + 1))
            .find(|n| !used.contains(n))
            .ok_or_else(|| malformed("part number allocation failed (unreachable)"))?;
        let new_name = format!("{prefix}{next_number}.{ext}");
        // Must register before recursion: the relationship graph may point back at the current part. In-package parts may land on disk only after the whole frame
        // completes; relationship Targets only need to exist at final save.
        self.copied_part_mapping
            .insert(src_abs.to_string(), new_name.clone());

        // Recursively copy the source part's rels. A Relationship Id is any legal xsd:ID and
        // is not required to use rIdN; keep the source insertion order and preserve the Ids verbatim.
        let src_uri = PartUri::new(src_abs)?;
        let mut new_rels = Relationships::default();
        if let Some(rels) = self.sub_pkg.relationships_of(src_abs) {
            let source_rels: Vec<Relationship> = rels.iter().cloned().collect();
            for rel in source_rels {
                match rel.target_mode {
                    TargetMode::External => {
                        // The r:* attributes inside the part blob still reference source rIds; when copying rels
                        // Ids must be kept verbatim and cannot be renumbered via get_or_add.
                        new_rels.push(rel);
                    }
                    TargetMode::Internal => {
                        let base = src_uri.parent();
                        let grand_uri = resolve_part_target(base.as_ref(), &rel.target)
                            .ok_or_else(|| {
                                malformed(format!(
                                    "subdocument part {src_abs} relationship target {:?} cannot be resolved",
                                    rel.target
                                ))
                            })?;
                        let grand_part =
                            self.sub_pkg.part(grand_uri.as_str()).ok_or_else(|| {
                                malformed(format!(
                                    "subdocument relationship target part {} does not exist (dangling relationship)",
                                    grand_uri.as_str()
                                ))
                            })?;
                        let grand_ct = self
                            .sub_pkg
                            .content_types()
                            .content_type_of(&grand_uri)
                            .ok_or_else(|| {
                                malformed(format!(
                                    "subdocument part {} is missing its content type declaration",
                                    grand_uri.as_str()
                                ))
                            })?
                            .to_string();
                        let grand_new = self.copy_part(
                            grand_uri.as_str(),
                            grand_part.bytes()?.to_vec(),
                            grand_ct,
                            rel.rel_type == RT_IMAGE,
                        )?;
                        // Recompute the copied rels Target relative to the new part directory
                        // (python-docx relate_to receives a Part object and computes relative references
                        //  from both partnames at serialization).
                        let rel_target = relative_to_owner(&new_name, &grand_new);
                        new_rels.push(Relationship {
                            id: rel.id,
                            rel_type: rel.rel_type,
                            target: rel_target,
                            target_mode: TargetMode::Internal,
                        });
                    }
                }
            }
        }

        // Land in the package: copy part bytes verbatim; register Content Types; materialize rels.
        self.pkg.add_part(&new_name, blob)?;
        self.register_content_type(&new_name, &content_type);
        if !new_rels.is_empty() {
            let rels_name = relationships_path_of(&PartUri::new(&new_name)?);
            self.pkg
                .add_part(&rels_name, new_rels.to_xml().into_bytes())?;
            self.register_content_type(&rels_name, CT_RELS);
        }
        Ok(new_name)
    }

    /// python-docx `_ContentTypesItem._add_part`: when the Default for the same extension has an identical
    /// content type, leave it untouched; a different content type for the same extension writes an Override;
    /// an extension without a Default gets a new Default.
    ///
    /// (Upstream also has a "leave an existing Override untouched" branch: new part numbers are unique and cannot
    /// collide with existing Overrides, so that branch is unreachable and omitted.)
    fn register_content_type(&mut self, part_name: &str, content_type: &str) {
        let ext = extension_of(part_name);
        let existing_default = self
            .content_types
            .defaults()
            .find(|(known, _)| known.eq_ignore_ascii_case(&ext))
            .map(|(_, ct)| ct.to_string());
        match existing_default {
            Some(ct) if ct == content_type => {}
            Some(_) => {
                self.content_types.add_override(part_name, content_type);
                self.ct_dirty = true;
            }
            None => {
                self.content_types.add_default(&ext, content_type);
                self.ct_dirty = true;
            }
        }
    }

    /// Resolve a sub relationship target to an absolute part name inside the sub package.
    fn resolve_sub_target(&self, target: &str) -> Option<PartUri> {
        let base = PartUri::new(&self.sub_main_name).ok()?.parent();
        resolve_part_target(base.as_ref(), target)
    }

    // ---------------------------------------------------------------
    // add_styles: three-branch style merging
    // ---------------------------------------------------------------

    /// docxcompose `numbering_part()`: when the main document first needs numbering, create an empty
    /// `/word/numbering.xml`, register its content type, and establish a numbering relationship from the main document.
    /// The relationship is still staged via `ImageInjections`' main-owner state,
    /// so it shares the exact same rId hole-filling rules with images/hyperlinks.
    fn ensure_main_numbering(&mut self) -> Result<(), Error> {
        if self.main_numbering.is_some() {
            return Ok(());
        }

        let name = "word/numbering.xml".to_string();
        let tree = if self.pkg.contains(&name) {
            // A few packages leave an orphan numbering part not referenced by the document rels;
            // reusing it is safer than creating a same-named part and also satisfies the OPC URI uniqueness constraint.
            load_xml_tree(self.pkg, &name)?
        } else {
            self.pkg
                .add_part(&name, EMPTY_NUMBERING_XML.as_bytes().to_vec())?;
            let mut tree = XmlDocument::parse_strict(EMPTY_NUMBERING_XML, &XmlLimits::default())
                .map_err(|source| {
                    Error::Render(RenderError::Xml {
                        part: name.clone(),
                        source,
                    })
                })?;
            tree.strip_blank_text();
            tree
        };

        self.register_content_type(&name, CT_NUMBERING);
        let target = relative_to_owner(&self.main_name, &name);
        self.injections
            .main_get_or_add(RT_NUMBERING, &target, TargetMode::Internal);
        self.main_numbering_name = Some(name);
        self.main_numbering = Some(tree);
        Ok(())
    }

    /// add_styles over body children (element is in the sub document tree).
    fn add_styles_in_subdoc(&mut self, element: NodeId) -> Result<(), Error> {
        if style_merge_needs_main_numbering(
            &self.sub_doc,
            element,
            &self.sub_styles,
            &self.main_styles,
            &self.style_id2name,
            &self.style_name2id,
            self.sub_numbering.as_ref(),
        ) {
            self.ensure_main_numbering()?;
        }
        let sub_doc = &mut self.sub_doc;
        merge_styles(
            sub_doc,
            element,
            &self.sub_styles,
            &mut self.main_styles,
            &self.style_id2name,
            &self.style_name2id,
            self.sub_numbering.as_ref(),
            &mut self.main_numbering,
            &mut self.num_id_mapping,
            &mut self.anum_id_mapping,
            &mut self.main_styles_dirty,
            &mut self.main_numbering_dirty,
        )
    }

    /// Upstream `add_styles_from_other_parts`: style merging on the root element of the sub footnotes part
    /// (when the sub has no footnotes part, upstream KeyError -> pass).
    fn add_styles_from_other_parts(&mut self) -> Result<(), Error> {
        let needs_numbering = self.sub_footnotes.as_ref().is_some_and(|tree| {
            style_merge_needs_main_numbering(
                tree,
                tree.root(),
                &self.sub_styles,
                &self.main_styles,
                &self.style_id2name,
                &self.style_name2id,
                self.sub_numbering.as_ref(),
            )
        });
        if needs_numbering {
            self.ensure_main_numbering()?;
        }
        let Some(footnotes_tree) = self.sub_footnotes.as_mut() else {
            return Ok(());
        };
        let element = footnotes_tree.root();
        merge_styles(
            footnotes_tree,
            element,
            &self.sub_styles,
            &mut self.main_styles,
            &self.style_id2name,
            &self.style_name2id,
            self.sub_numbering.as_ref(),
            &mut self.main_numbering,
            &mut self.num_id_mapping,
            &mut self.anum_id_mapping,
            &mut self.main_styles_dirty,
            &mut self.main_numbering_dirty,
        )
    }

    // ---------------------------------------------------------------
    // add_numberings: numbering copying
    // ---------------------------------------------------------------

    /// add_numberings over body children (element is in the sub document tree).
    fn add_numberings_in_subdoc(&mut self, element: NodeId) -> Result<(), Error> {
        if !tag_descendants(&self.sub_doc, element, ns_uri::W, "numId").is_empty() {
            self.ensure_main_numbering()?;
        }
        let sub_doc = &mut self.sub_doc;
        merge_numberings(
            sub_doc,
            element,
            self.sub_numbering.as_ref(),
            &mut self.main_numbering,
            &mut self.num_id_mapping,
            &mut self.anum_id_mapping,
            &mut self.main_numbering_dirty,
        )
    }

    // ---------------------------------------------------------------
    // restart_first_numbering: list numbering restart
    // ---------------------------------------------------------------

    /// Upstream `restart_first_numbering`: the first list paragraph of the same style that is neither a heading nor a bullet
    /// gets a copied `w:num` with a level-0 start override appended, and the paragraph's
    /// `w:numPr` (along with the corresponding numId mapping) switches to the new numbering instance.
    fn restart_first_numbering(&mut self, element: NodeId) -> Result<(), Error> {
        // No pStyle -> return (the normal path).
        let Some(style_id) = tag_descendants(&self.sub_doc, element, ns_uri::W, "pStyle")
            .iter()
            .find_map(|node| {
                self.sub_doc
                    .attr(*node, ns_uri::W, "val")
                    .map(str::to_owned)
            })
        else {
            return Ok(());
        };
        // Already restarted for the same style -> return.
        if self.numbering_restarted.contains(&style_id) {
            return Ok(());
        }
        // Upstream looks up this style_id in the **main** stylesheet (note: no name mapping is done);
        // absent in the main stylesheet -> return.
        let Some(style_el) = style_by_id(&self.main_styles, &style_id) else {
            return Ok(());
        };
        // outlineLvl (heading kind) -> do not restart, return.
        if !tag_descendants(&self.main_styles, style_el, ns_uri::W, "outlineLvl").is_empty() {
            return Ok(());
        }

        // Prefer the numId directly on the paragraph, then the one in its style; neither -> return.
        let local_num_id = first_num_id_under_num_pr(&self.sub_doc, element);
        let num_id = match local_num_id {
            Some(value) => value,
            None => match tag_descendants(&self.main_styles, style_el, ns_uri::W, "numId")
                .iter()
                .find_map(|node| {
                    self.main_styles
                        .attr(*node, ns_uri::W, "val")
                        .map(str::to_owned)
                }) {
                Some(value) => value,
                None => return Ok(()),
            },
        };

        // `numbering_part()` is lazy: even when no num is ultimately found, it is created here.
        self.ensure_main_numbering()?;
        let main_numbering = self
            .main_numbering
            .as_ref()
            .ok_or_else(|| malformed("internal error: numbering tree is missing"))?;
        // No corresponding w:num in the main numbering -> return (style has no numbering element).
        let Some(num_el) = num_by_id(main_numbering, &num_id) else {
            return Ok(());
        };
        // w:num missing abstractNumId: upstream xpath(...)[0] raises IndexError directly.
        let Some(anum_val) = tag_descendants(main_numbering, num_el, ns_uri::W, "abstractNumId")
            .iter()
            .find_map(|node| {
                main_numbering
                    .attr(*node, ns_uri::W, "val")
                    .map(str::to_owned)
            })
        else {
            return Err(malformed(format!(
                "main numbering part w:num[@w:numId={num_id}] is missing w:abstractNumId (part word/numbering.xml)"
            )));
        };
        // No corresponding w:abstractNum: upstream anum_element[0] raises IndexError directly.
        let Some(anum_el) = anum_by_id(main_numbering, &anum_val) else {
            return Err(malformed(format!(
                "main numbering part is missing w:abstractNum[@w:abstractNumId={anum_val}] (part word/numbering.xml)"
            )));
        };
        // bullets are not restarted -> return; when numFmt is missing upstream still enters the modification block.
        let num_fmt = tag_descendants(main_numbering, anum_el, ns_uri::W, "lvl")
            .into_iter()
            .filter(|lvl| main_numbering.attr(*lvl, ns_uri::W, "ilvl") == Some("0"))
            .find_map(|lvl| {
                tag_descendants(main_numbering, lvl, ns_uri::W, "numFmt")
                    .iter()
                    .find_map(|node| {
                        main_numbering
                            .attr(*node, ns_uri::W, "val")
                            .map(str::to_owned)
                    })
            });
        if num_fmt.as_deref() == Some("bullet") {
            return Ok(());
        }

        // Modification block: copy the existing w:num and append a level-0 start override.
        let next_num_id = {
            let main_numbering = self
                .main_numbering
                .as_mut()
                .ok_or_else(|| malformed("internal error: numbering tree is missing"))?;
            let new_num = clone_element_within(main_numbering, num_el)?;
            let lvl_override = parse_element(
                r#"<w:lvlOverride xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:ilvl="0"><w:startOverride w:val="1"/></w:lvlOverride>"#,
            )?;
            let lvl_override = main_numbering
                .deepcopy_element(&lvl_override, lvl_override.root())
                .map_err(copy_error)?;
            main_numbering.append_child(new_num, lvl_override);
            let (next_num_id, _) = next_numbering_ids(main_numbering)?;
            main_numbering.set_attr(new_num, ns_uri::W, "numId", next_num_id.to_string());
            insert_num(main_numbering, new_num, &mut self.main_numbering_dirty);
            next_num_id
        };

        // The pPr holding pStyle is the rewrite target. Upstream takes the first xpath item directly here; keep
        // the same strict failure semantics, avoiding numbering being mistakenly added to other nested paragraph properties.
        let paragraph_props = paragraph_props_for_style(&self.sub_doc, element, &style_id)
            .ok_or_else(|| {
                malformed(format!(
                    "the w:pStyle of subdocument style {style_id:?} is not a direct child of w:pPr"
                ))
            })?;
        let existing_num_pr = tag_descendants(&self.sub_doc, paragraph_props, ns_uri::W, "numPr")
            .into_iter()
            .next();
        if let Some(num_pr) = existing_num_pr {
            let num_id_node = tag_descendants(&self.sub_doc, num_pr, ns_uri::W, "numId")
                .into_iter()
                .next()
                .ok_or_else(|| {
                    malformed("w:numPr is missing w:numId, cannot restart list numbering")
                })?;
            let previous_num_id: i64 = self
                .sub_doc
                .attr(num_id_node, ns_uri::W, "val")
                .ok_or_else(|| {
                    malformed("w:numId is missing w:val, cannot restart list numbering")
                })?
                .parse()
                .map_err(|_| malformed("w:numId/@w:val is not an integer"))?;

            // `_replace_mapped_num_id` replaces only the first value hit. HashMap has no
            // insertion order, so sort by key to keep even extreme many-to-one mappings deterministic.
            let mut mapped_keys: Vec<i64> = self
                .num_id_mapping
                .iter()
                .filter_map(|(&key, &value)| (value == previous_num_id).then_some(key))
                .collect();
            mapped_keys.sort_unstable();
            if let Some(key) = mapped_keys.first() {
                self.num_id_mapping.insert(*key, next_num_id);
            }
            self.sub_doc
                .set_attr(num_id_node, ns_uri::W, "val", next_num_id.to_string());
        } else {
            let num_pr_src = parse_element(&format!(
                r#"<w:numPr xmlns:w="{}"><w:ilvl w:val="0"/><w:numId w:val="{}"/></w:numPr>"#,
                ns_uri::W,
                next_num_id
            ))?;
            let num_pr = self
                .sub_doc
                .deepcopy_element(&num_pr_src, num_pr_src.root())
                .map_err(copy_error)?;
            self.sub_doc.append_child(paragraph_props, num_pr);
        }
        self.numbering_restarted.insert(style_id);
        Ok(())
    }

    // ---------------------------------------------------------------
    // add_images: image merging
    // ---------------------------------------------------------------

    /// Upstream `add_images`: `(.//a:blip|.//asvg:svgBlip)[@r:embed]` in document order;
    /// reuse/create main-package image parts by sha1 of the source part bytes (the `ImageWrapper` extension
    /// comes from the source part filename suffix and its content type from the source package declaration); r:embed
    /// is rewritten to the main-rels rId; `r:link` external links are registered via add_relationship.
    fn add_images(&mut self, element: NodeId) -> Result<(), Error> {
        let mut blips = Vec::new();
        {
            let sub_doc = &self.sub_doc;
            for node in sub_doc.descendants(element).into_iter().skip(1) {
                let is_blip = is_tag(sub_doc, node, ns_uri::A, "blip")
                    || is_tag(sub_doc, node, NS_ASVG, "svgBlip");
                if is_blip && sub_doc.attr(node, ns_uri::R, "embed").is_some() {
                    blips.push(node);
                }
            }
        }
        for blip in blips {
            let rid = self
                .sub_doc
                .attr(blip, ns_uri::R, "embed")
                .map(str::to_owned)
                .unwrap_or_default();
            let rel = self.sub_rels.get(&rid).cloned().ok_or_else(|| {
                malformed(format!(
                    "relationship {rid:?} referenced by a subdocument image does not exist (part {})",
                    self.sub_main_name
                ))
            })?;
            if rel.target_mode == TargetMode::External {
                return Err(malformed(format!(
                    "subdocument image relationship {rid:?} is an external reference; cross-package external images are not supported"
                )));
            }
            let abs_uri = self.resolve_sub_target(&rel.target).ok_or_else(|| {
                malformed(format!(
                    "subdocument image relationship target {:?} cannot be resolved to an in-package part (part {})",
                    rel.target, self.sub_main_name
                ))
            })?;
            let abs_name = abs_uri.as_str().to_string();
            let part = self.sub_pkg.part(&abs_name).ok_or_else(|| {
                malformed(format!(
                    "subdocument image part {abs_name} does not exist (dangling relationship)"
                ))
            })?;
            let content_type = self
                .sub_pkg
                .content_types()
                .content_type_of(&abs_uri)
                .ok_or_else(|| {
                    malformed(format!(
                        "subdocument image part {abs_name} is missing its content type declaration"
                    ))
                })?
                .to_string();
            let ext = extension_of(&abs_name);
            let new_rid = self
                .injections
                .add_subdoc_image(part.bytes()?, &ext, &content_type);
            self.sub_doc.set_attr(blip, ns_uri::R, "embed", new_rid);

            // r:link: an image may carry both an embed and an external link (upstream goes through add_relationship).
            if let Some(link_rid) = self
                .sub_doc
                .attr(blip, ns_uri::R, "link")
                .map(str::to_owned)
            {
                let rel = self.sub_rels.get(&link_rid).cloned().ok_or_else(|| {
                    malformed(format!(
                        "subdocument image external-link relationship {link_rid:?} does not exist (part {})",
                        self.sub_main_name
                    ))
                })?;
                let new_link = self.add_relationship(&rel)?;
                self.sub_doc.set_attr(blip, ns_uri::R, "link", new_link);
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // add_diagrams / add_shapes / add_footnotes
    // ---------------------------------------------------------------

    /// Upstream `SubdocComposer.add_diagrams`: the four relationships of `dgm:relIds`
    /// are copied with fixed types to the main document and their rIds rewritten.
    fn add_diagrams(&mut self, element: NodeId) -> Result<(), Error> {
        let nodes: Vec<NodeId> = self
            .sub_doc
            .descendants(element)
            .into_iter()
            .skip(1)
            .filter(|&node| {
                is_tag(&self.sub_doc, node, NS_DGM, "relIds")
                    && self.sub_doc.attr(node, ns_uri::R, "dm").is_some()
            })
            .collect();
        for node in nodes {
            for (attr, rel_type) in [
                ("dm", RT_DIAGRAM_DATA),
                ("lo", RT_DIAGRAM_LAYOUT),
                ("qs", RT_DIAGRAM_QUICK_STYLE),
                ("cs", RT_DIAGRAM_COLORS),
            ] {
                let rid = self
                    .sub_doc
                    .attr(node, ns_uri::R, attr)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        malformed(format!(
                            "SmartArt dgm:relIds is missing r:{attr} (part {})",
                            self.sub_main_name
                        ))
                    })?;
                let source_rel = self.sub_rels.get(&rid).cloned().ok_or_else(|| {
                    malformed(format!(
                        "SmartArt relationship {rid:?} referenced by r:{attr} does not exist (part {})",
                        self.sub_main_name
                    ))
                })?;
                if source_rel.target_mode != TargetMode::Internal {
                    return Err(malformed(format!(
                        "SmartArt r:{attr} relationship {rid:?} must point to an in-package part"
                    )));
                }
                let source_uri = self.resolve_sub_target(&source_rel.target).ok_or_else(|| {
                    malformed(format!(
                        "SmartArt r:{attr} relationship target {:?} cannot be resolved",
                        source_rel.target
                    ))
                })?;
                let key = (source_uri.as_str().to_string(), rel_type.to_string());
                let new_rid = match self.diagram_rel_mapping.get(&key) {
                    Some(existing) => existing.clone(),
                    None => {
                        let mut forced_rel = source_rel;
                        forced_rel.rel_type = rel_type.to_string();
                        let allocated = self.add_relationship(&forced_rel)?;
                        self.diagram_rel_mapping.insert(key, allocated.clone());
                        allocated
                    }
                };
                self.sub_doc.set_attr(node, ns_uri::R, attr, new_rid);
            }
        }
        Ok(())
    }

    /// Upstream `add_shapes`: VML `v:shape/v:imagedata` and DrawingML
    /// images share package-level sha1 deduplication and media numbering.
    fn add_shapes(&mut self, element: NodeId) -> Result<(), Error> {
        let image_nodes: Vec<NodeId> = self
            .sub_doc
            .descendants(element)
            .into_iter()
            .skip(1)
            .filter(|&node| {
                is_tag(&self.sub_doc, node, ns_uri::V, "imagedata")
                    && self
                        .sub_doc
                        .parent(node)
                        .is_some_and(|parent| is_tag(&self.sub_doc, parent, ns_uri::V, "shape"))
            })
            .collect();
        for image_node in image_nodes {
            let rid = self
                .sub_doc
                .attr(image_node, ns_uri::R, "id")
                .map(str::to_owned)
                .ok_or_else(|| malformed("VML v:imagedata is missing r:id"))?;
            let rel = self.sub_rels.get(&rid).cloned().ok_or_else(|| {
                malformed(format!(
                    "VML relationship {rid:?} referenced by the image does not exist (part {})",
                    self.sub_main_name
                ))
            })?;
            if rel.target_mode != TargetMode::Internal {
                return Err(malformed(format!(
                    "VML image relationship {rid:?} is an external relationship"
                )));
            }
            let abs_uri = self.resolve_sub_target(&rel.target).ok_or_else(|| {
                malformed(format!(
                    "VML image relationship target {:?} cannot be resolved",
                    rel.target
                ))
            })?;
            let abs_name = abs_uri.as_str().to_string();
            let part = self.sub_pkg.part(&abs_name).ok_or_else(|| {
                malformed(format!(
                    "VML image part {abs_name} does not exist (dangling relationship)"
                ))
            })?;
            let content_type = self
                .sub_pkg
                .content_types()
                .content_type_of(&abs_uri)
                .ok_or_else(|| {
                    malformed(format!(
                        "VML image part {abs_name} is missing its content type"
                    ))
                })?
                .to_string();
            let ext = extension_of(&abs_name);
            let new_rid = self
                .injections
                .add_subdoc_image(part.bytes()?, &ext, &content_type);
            self.sub_doc.set_attr(image_node, ns_uri::R, "id", new_rid);
        }
        Ok(())
    }

    /// When the main document has no footnotes part, create one from docxcompose's built-in template.
    fn ensure_main_footnotes(&mut self) -> Result<(), Error> {
        if self.main_footnotes.is_some() {
            return Ok(());
        }
        let name = "word/footnotes.xml".to_string();
        let tree = if self.pkg.contains(&name) {
            load_xml_tree(self.pkg, &name)?
        } else {
            self.pkg
                .add_part(&name, EMPTY_FOOTNOTES_XML.as_bytes().to_vec())?;
            let mut tree = XmlDocument::parse_strict(EMPTY_FOOTNOTES_XML, &XmlLimits::default())
                .map_err(|source| {
                    Error::Render(RenderError::Xml {
                        part: name.clone(),
                        source,
                    })
                })?;
            tree.strip_blank_text();
            tree
        };
        self.register_content_type(&name, CT_FOOTNOTES);
        let target = relative_to_owner(&self.main_name, &name);
        self.injections
            .main_get_or_add(RT_FOOTNOTES, &target, TargetMode::Internal);
        self.main_footnotes_rels = Some(
            self.pkg
                .relationships_of(&name)
                .cloned()
                .unwrap_or_default(),
        );
        self.main_footnotes_name = Some(name);
        self.main_footnotes = Some(tree);
        Ok(())
    }

    /// Copy one sub footnotes relationship into the main footnotes scope.
    fn copy_footnote_relationship(&mut self, old_rid: &str) -> Result<String, Error> {
        if let Some(existing) = self.footnote_rel_mapping.get(old_rid) {
            return Ok(existing.clone());
        }
        let source_name = self
            .sub_footnotes_name
            .clone()
            .ok_or_else(|| malformed("subdocument is missing the footnotes part"))?;
        let rel = self
            .sub_pkg
            .relationships_of(&source_name)
            .and_then(|rels| rels.get(old_rid))
            .cloned()
            .ok_or_else(|| {
                malformed(format!(
                    "relationship {old_rid:?} referenced by a subdocument footnote does not exist (part {source_name})"
                ))
            })?;
        let destination_name = self
            .main_footnotes_name
            .clone()
            .ok_or_else(|| malformed("internal error: main footnotes part is not initialized"))?;

        let target = match rel.target_mode {
            TargetMode::External => rel.target.clone(),
            TargetMode::Internal => {
                let source_uri = PartUri::new(&source_name)?;
                let target_uri = resolve_part_target(source_uri.parent().as_ref(), &rel.target)
                    .ok_or_else(|| {
                        malformed(format!(
                            "subdocument footnote relationship target {:?} cannot be resolved (part {source_name})",
                            rel.target
                        ))
                    })?;
                let source_part = self.sub_pkg.part(target_uri.as_str()).ok_or_else(|| {
                    malformed(format!(
                        "subdocument footnote relationship target part {} does not exist",
                        target_uri.as_str()
                    ))
                })?;
                let content_type = self
                    .sub_pkg
                    .content_types()
                    .content_type_of(&target_uri)
                    .ok_or_else(|| {
                        malformed(format!(
                            "subdocument footnote relationship target part {} is missing its content type",
                            target_uri.as_str()
                        ))
                    })?
                    .to_string();
                let copied = self.copy_part(
                    target_uri.as_str(),
                    source_part.bytes()?.to_vec(),
                    content_type,
                    rel.rel_type == RT_IMAGE,
                )?;
                relative_to_owner(&destination_name, &copied)
            }
        };
        let rels = self
            .main_footnotes_rels
            .as_mut()
            .ok_or_else(|| malformed("internal error: main footnotes rels is not initialized"))?;
        let before = rels.len();
        let new_rid = get_or_add_rel(rels, &rel.rel_type, &target, rel.target_mode);
        if rels.len() != before {
            self.main_footnotes_rels_dirty = true;
        }
        self.footnote_rel_mapping
            .insert(old_rid.to_string(), new_rid.clone());
        Ok(new_rid)
    }

    /// Upstream `add_footnotes`: copy referenced footnotes one by one and renumber their ids.
    /// Relationship rewriting acts on the clones actually appended, working around a docxcompose
    /// 2.2.0 issue where dangling rIds could be produced when the target already had rels.
    fn add_footnotes(&mut self, element: NodeId) -> Result<(), Error> {
        let refs = tag_descendants(&self.sub_doc, element, ns_uri::W, "footnoteReference");
        if refs.is_empty() {
            return Ok(());
        }
        if self.sub_footnotes.is_none() || self.sub_footnotes_name.is_none() {
            return Err(malformed(
                "subdocument contains w:footnoteReference but is missing the footnotes part relationship",
            ));
        }
        self.ensure_main_footnotes()?;

        let first_new_id = self
            .main_footnotes
            .as_ref()
            .map(|tree| tree.children(tree.root()).len() as i64 + 1)
            .ok_or_else(|| malformed("internal error: main footnotes tree is not initialized"))?;
        for (offset, reference) in refs.into_iter().enumerate() {
            let next_id = first_new_id
                + i64::try_from(offset)
                    .map_err(|_| malformed("footnote count exceeds the i64 representable range"))?;
            let old_id = self
                .sub_doc
                .attr(reference, ns_uri::W, "id")
                .map(str::to_owned)
                .ok_or_else(|| malformed("w:footnoteReference is missing w:id"))?;
            let source_node = {
                let source = self
                    .sub_footnotes
                    .as_ref()
                    .ok_or_else(|| malformed("internal error: sub footnotes tree is missing"))?;
                source
                    .descendants(source.root())
                    .into_iter()
                    .skip(1)
                    .find(|&node| {
                        is_tag(source, node, ns_uri::W, "footnote")
                            && source.attr(node, ns_uri::W, "id") == Some(old_id.as_str())
                    })
                    .ok_or_else(|| {
                        malformed(format!(
                            "w:footnote[@w:id={old_id:?}] does not exist in the subdocument footnotes part"
                        ))
                    })?
            };
            let copied =
                {
                    let source = self.sub_footnotes.as_ref().ok_or_else(|| {
                        malformed("internal error: sub footnotes tree is missing")
                    })?;
                    let destination = self.main_footnotes.as_mut().ok_or_else(|| {
                        malformed("internal error: main footnotes tree is missing")
                    })?;
                    destination
                        .deepcopy_element(source, source_node)
                        .map_err(copy_error)?
                };

            let relation_attrs = {
                let destination = self
                    .main_footnotes
                    .as_ref()
                    .ok_or_else(|| malformed("internal error: main footnotes tree is missing"))?;
                relationship_attributes(destination, copied)
            };
            for (node, attr_local, old_rid) in relation_attrs {
                let new_rid = self.copy_footnote_relationship(&old_rid)?;
                self.main_footnotes
                    .as_mut()
                    .ok_or_else(|| malformed("internal error: main footnotes tree is missing"))?
                    .set_attr(node, ns_uri::R, &attr_local, new_rid);
            }

            let destination = self
                .main_footnotes
                .as_mut()
                .ok_or_else(|| malformed("internal error: main footnotes tree is missing"))?;
            destination.set_attr(copied, ns_uri::W, "id", next_id.to_string());
            destination.append_child(destination.root(), copied);
            self.sub_doc
                .set_attr(reference, ns_uri::W, "id", next_id.to_string());
            self.main_footnotes_dirty = true;
        }
        Ok(())
    }

    /// Upstream `remove_header_and_footer_references`: remove every
    /// `w:headerReference` / `w:footerReference` in the subtree (references left in the fragment would dangle).
    fn remove_header_and_footer_references(&mut self, element: NodeId) {
        let sub_doc = &mut self.sub_doc;
        let mut refs = Vec::new();
        for node in sub_doc.descendants(element).into_iter().skip(1) {
            if is_tag(sub_doc, node, ns_uri::W, "headerReference")
                || is_tag(sub_doc, node, ns_uri::W, "footerReference")
            {
                refs.push(node);
            }
        }
        for node in refs {
            sub_doc.detach(node);
        }
    }

    // ---------------------------------------------------------------
    // The three renumber siblings + fix_section_types (acting on the main document)
    // ---------------------------------------------------------------

    /// Upstream `renumber_bookmarks`: `w:bookmarkStart` and `w:bookmarkEnd` of the main body
    /// are **each independently** renumbered consecutively from 0 (in document order).
    fn renumber_bookmarks(&mut self) {
        let main_body = match body_of(&self.main_doc) {
            Ok(body) => body,
            Err(_) => return,
        };
        let mut dirty = false;
        for local in ["bookmarkStart", "bookmarkEnd"] {
            for (index, node) in tag_descendants(&self.main_doc, main_body, ns_uri::W, local)
                .into_iter()
                .enumerate()
            {
                if set_attr_if_changed(
                    &mut self.main_doc,
                    node,
                    ns_uri::W,
                    "id",
                    &(index as i64).to_string(),
                ) {
                    dirty = true;
                }
            }
        }
        if dirty {
            self.main_doc_dirty = true;
        }
    }

    /// Upstream `renumber_docpr_ids` / `renumber_nvpicpr_ids` share a skeleton:
    /// renumber the main body consecutively from 1, then follow the HEADER/FOOTER relationships in the main rels (insertion order,
    /// without deduplication) and **continue the same counter** on each header/footer part (the counter persists across loops;
    /// enumerate does not apply).
    #[allow(clippy::explicit_counter_loop)]
    fn renumber_ids(&mut self, ns: &str, local: &str) -> Result<(), Error> {
        let main_body = body_of(&self.main_doc)?;
        let mut next_id = 1i64;
        let mut body_dirty = false;
        for node in tag_descendants(&self.main_doc, main_body, ns, local) {
            if set_attr_if_changed(&mut self.main_doc, node, "", "id", &next_id.to_string()) {
                body_dirty = true;
            }
            next_id += 1;
        }
        if body_dirty {
            self.main_doc_dirty = true;
        }

        for part_name in self.injections.main_header_footer_parts() {
            let tree = self.hf_tree(&part_name)?;
            let mut dirty = false;
            for node in tag_descendants(tree, tree.root(), ns, local) {
                if set_attr_if_changed(tree, node, "", "id", &next_id.to_string()) {
                    dirty = true;
                }
                next_id += 1;
            }
            if dirty {
                self.dirty_hf.insert(part_name);
            }
        }
        Ok(())
    }

    /// Header/footer part trees (parsed once and shared across renumbering; when the same part has multiple relationships upstream
    /// sets them repeatedly, here the cached tree guarantees idempotent consistency).
    fn hf_tree(&mut self, name: &str) -> Result<&mut XmlDocument, Error> {
        if !self.hf_trees.contains_key(name) {
            let tree = load_xml_tree(self.pkg, name)?;
            self.hf_trees.insert(name.to_string(), tree);
        }
        match self.hf_trees.get_mut(name) {
            Some(tree) => Ok(tree),
            // Unreachable: just inserted above.
            None => Err(malformed(
                "internal error: header/footer tree cache is missing",
            )),
        }
    }

    /// Upstream `fix_section_types`: return immediately if either side has a single section; when both sides have multiple sections, change the first
    /// new section's type to the last-section type of the original main document, and the last section type after combining to the subdocument's
    /// last-section type. A missing `w:type` is equivalent to `nextPage`; when writing back nextPage the node is removed.
    fn fix_section_types(&mut self) -> Result<(), Error> {
        let main_body = body_of(&self.main_doc)?;
        let sub_body = body_of(&self.sub_doc)?;
        let main_sections = section_nodes(&self.main_doc, main_body);
        let sub_sections = section_nodes(&self.sub_doc, sub_body);
        if main_sections.len() <= 1 || sub_sections.len() <= 1 {
            return Ok(());
        }

        let first_new_idx = main_sections.len() as isize - sub_sections.len() as isize;
        let normalized_idx = if first_new_idx < 0 {
            main_sections.len() as isize + first_new_idx
        } else {
            first_new_idx
        };
        if normalized_idx < 0 || normalized_idx >= main_sections.len() as isize {
            return Err(malformed(format!(
                "fix_section_types first-new-section index out of bounds: main document has {} sections, subdocument has {}",
                main_sections.len(),
                sub_sections.len()
            )));
        }

        let first_new = main_sections[normalized_idx as usize];
        let main_last = *main_sections
            .last()
            .ok_or_else(|| malformed("main document is missing section properties"))?;
        let sub_last = *sub_sections
            .last()
            .ok_or_else(|| malformed("subdocument is missing section properties"))?;
        // Both source values must be read before either write; first_new and main_last may be the same
        // node under negative indexing, which matches Python's chained-assignment semantics.
        let main_last_type = section_start_type(&self.main_doc, main_last);
        let sub_last_type = section_start_type(&self.sub_doc, sub_last);

        let mut dirty = set_section_start_type(&mut self.main_doc, first_new, &main_last_type)?;
        dirty |= set_section_start_type(&mut self.main_doc, main_last, &sub_last_type)?;
        if dirty {
            self.main_doc_dirty = true;
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // Write-back and fragments
    // ---------------------------------------------------------------

    /// Main-tree write-back: write only parts actually modified (python-docx serialized form:
    /// whitespace-stripped tree + lxml single-quoted declaration; the template itself is already in this form, and unchanged parts have
    /// identical re-serialized bytes, so dirty gating does not change the output but only saves write-back).
    fn flush_main_parts(&mut self) -> Result<(), Error> {
        if self.main_doc_dirty {
            let bytes = self.main_doc.serialize().into_bytes();
            self.pkg.set_part_bytes(&self.main_name, bytes)?;
        }
        if self.main_styles_dirty {
            let bytes = self.main_styles.serialize().into_bytes();
            self.pkg.set_part_bytes(&self.main_styles_name, bytes)?;
        }
        if self.main_numbering_dirty {
            let name = self.main_numbering_name.clone().ok_or_else(|| {
                malformed("main document is missing the numbering part (word/numbering.xml), cannot write back numbering changes")
            })?;
            let bytes = self
                .main_numbering
                .as_ref()
                .ok_or_else(|| malformed("internal error: numbering tree is missing"))?
                .serialize()
                .into_bytes();
            self.pkg.set_part_bytes(&name, bytes)?;
        }
        if self.main_footnotes_dirty {
            let name = self
                .main_footnotes_name
                .clone()
                .ok_or_else(|| malformed("main document is missing the footnotes part, cannot write back footnote changes"))?;
            let bytes = self
                .main_footnotes
                .as_ref()
                .ok_or_else(|| malformed("internal error: main footnotes tree is missing"))?
                .serialize()
                .into_bytes();
            self.pkg.set_part_bytes(&name, bytes)?;
        }
        if self.main_footnotes_rels_dirty {
            let owner = self
                .main_footnotes_name
                .clone()
                .ok_or_else(|| malformed("main document is missing the footnotes part, cannot write back footnote relationships"))?;
            let rels_name = relationships_path_of(&PartUri::new(&owner)?);
            let bytes = self
                .main_footnotes_rels
                .as_ref()
                .ok_or_else(|| malformed("internal error: main footnotes rels is missing"))?
                .to_xml()
                .into_bytes();
            if !self.pkg.contains(&rels_name) {
                self.pkg.add_part(&rels_name, bytes.clone())?;
            }
            // add_part itself does not parse rels; set_part_bytes mounts them onto the owner synchronously.
            self.pkg.set_part_bytes(&rels_name, bytes)?;
            self.register_content_type(&rels_name, CT_RELS);
        }
        for (name, tree) in &self.hf_trees {
            if self.dirty_hf.contains(name) {
                let bytes = tree.serialize().into_bytes();
                self.pkg.set_part_bytes(name, bytes)?;
            }
        }
        Ok(())
    }

    /// Write back Content Types changes (Defaults for image extensions land via ImageInjections
    /// at finish; here only entries produced by part copying are written).
    fn flush_content_types(&mut self) -> Result<(), Error> {
        if self.ct_dirty {
            let bytes = self.content_types.to_xml().into_bytes();
            self.pkg.set_part_bytes("[Content_Types].xml", bytes)?;
        }
        Ok(())
    }

    /// Upstream `Subdoc._get_xml`: remove the body's direct `w:sectPr`; the remaining children are concatenated in
    /// document order into a fragment (no XML declaration, no namespace declarations -- upstream tostring
    /// then strips the body open/close tags by regex, and declarations promoted onto the body tag are lost as well).
    fn build_fragment(&mut self) -> String {
        let Ok(sub_body) = body_of(&self.sub_doc) else {
            return String::new();
        };
        // Remove the body's direct sectPr (the first step of _get_xml).
        let sectprs: Vec<NodeId> = self
            .sub_doc
            .children(sub_body)
            .iter()
            .copied()
            .filter(|&child| is_tag(&self.sub_doc, child, ns_uri::W, "sectPr"))
            .collect();
        for node in sectprs {
            self.sub_doc.detach(node);
        }
        let mut fragment = String::new();
        for child in self.sub_doc.children(sub_body).to_vec() {
            fragment.push_str(&self.sub_doc.serialize_subtree(child));
        }
        fragment
    }
}

// =====================================================================
// Tree-level free functions (they operate on multiple trees at once; parameters are passed explicitly to avoid borrow conflicts)
// =====================================================================

/// Upstream `add_styles` (docxcompose composer.py):
/// - `used_style_ids`: the `w:val`s of `w:tblStyle|w:pStyle|w:rStyle` in the element subtree,
///   deduplicated while preserving document order;
/// - for each style: after computing `mapped_style_id` (sub id -> name -> main id), take a branch:
///   the main stylesheet lacks the id -> deep-copy the sub style and append it to the main styles (branch B), followed by
///   `add_numberings` (on the copy) and `add_linked_styles`; the main stylesheet already has it -> no copy,
///   and an anum mapping is established only when the sub style carries a numId (branch C);
/// - when `our_style_id != style_id`, rewrite every style reference in the element subtree with
///   val==style_id across the three style-reference kinds to the main id;
/// - `our_style_ids` is refreshed at the tail of each loop iteration (newly copied styles enter the main table).
#[allow(clippy::too_many_arguments)]
fn merge_styles(
    element_tree: &mut XmlDocument,
    element: NodeId,
    sub_styles: &XmlDocument,
    main_styles: &mut XmlDocument,
    style_id2name: &HashMap<String, String>,
    style_name2id: &HashMap<String, String>,
    sub_numbering: Option<&XmlDocument>,
    main_numbering: &mut Option<XmlDocument>,
    num_id_mapping: &mut HashMap<i64, i64>,
    anum_id_mapping: &mut HashMap<i64, i64>,
    main_styles_dirty: &mut bool,
    main_numbering_dirty: &mut bool,
) -> Result<(), Error> {
    let main_styles_root = main_styles.root();
    let mut our_style_ids = style_ids_of(main_styles);

    // Order-preserving deduplication (OrderedDict.fromkeys).
    let mut used_style_ids: Vec<String> = Vec::new();
    for node in element_tree.descendants(element).into_iter().skip(1) {
        let is_ref = is_tag(element_tree, node, ns_uri::W, "tblStyle")
            || is_tag(element_tree, node, ns_uri::W, "pStyle")
            || is_tag(element_tree, node, ns_uri::W, "rStyle");
        if is_ref {
            if let Some(val) = element_tree.attr(node, ns_uri::W, "val") {
                let val = val.to_owned();
                if !used_style_ids.contains(&val) {
                    used_style_ids.push(val);
                }
            }
        }
    }

    for style_id in used_style_ids {
        let our_style_id = mapped_style_id(&style_id, style_id2name, style_name2id);
        if !our_style_ids.contains(&our_style_id) {
            // Branch B: the main stylesheet lacks the style -> deep-copy and append (when get_by_id has no hit, upstream
            // deepcopy(None) is None and the if guard skips it).
            if let Some(src) = style_by_id(sub_styles, &style_id) {
                let copy = main_styles
                    .deepcopy_element(sub_styles, src)
                    .map_err(copy_error)?;
                main_styles.append_child(main_styles_root, copy);
                *main_styles_dirty = true;
                // Followed by add_numberings (on the copy) and add_linked_styles (on the copy).
                merge_numberings(
                    main_styles,
                    copy,
                    sub_numbering,
                    main_numbering,
                    num_id_mapping,
                    anum_id_mapping,
                    main_numbering_dirty,
                )?;
                merge_linked_styles(
                    main_styles,
                    sub_styles,
                    copy,
                    style_id2name,
                    style_name2id,
                    main_styles_dirty,
                )?;
            }
        } else if let Some(sub_style) = style_by_id(sub_styles, &style_id) {
            // Branch C: the main stylesheet already has the style -> the anum mapping chain. Any missing link skips it;
            // expanded in nested form (matching the upstream if chain), ensuring the tail rewrite always runs.
            let first_sub_num = tag_descendants(sub_styles, sub_style, ns_uri::W, "numId")
                .iter()
                .find_map(|node| sub_styles.attr(*node, ns_uri::W, "val").map(str::to_owned));
            if let Some(first_sub_num) = first_sub_num {
                // Look up numId -> abstractNumId in the sub numbering (when the sub lacks the numbering part,
                // upstream creates an empty part and queries never match).
                let sub_anum = sub_numbering.and_then(|tree| {
                    num_by_id(tree, &first_sub_num).and_then(|num_el| {
                        tag_descendants(tree, num_el, ns_uri::W, "abstractNumId")
                            .iter()
                            .find_map(|node| tree.attr(*node, ns_uri::W, "val").map(str::to_owned))
                    })
                });
                if let Some(sub_anum) = sub_anum {
                    // Main style (our_style_id is in our_style_ids, so get_by_id
                    // always hits) -> main numId.
                    if let Some(main_style) = style_by_id(main_styles, &our_style_id) {
                        let first_our_num =
                            tag_descendants(main_styles, main_style, ns_uri::W, "numId")
                                .iter()
                                .find_map(|node| {
                                    main_styles.attr(*node, ns_uri::W, "val").map(str::to_owned)
                                });
                        if let Some(first_our_num) = first_our_num {
                            // Main numbering: when missing, upstream creates it from the built-in template,
                            // while this implementation treats it as main-package unsupported.
                            let main_numbering = main_numbering.as_ref().ok_or_else(|| {
                                malformed(
                                    "main document is missing the numbering part (word/numbering.xml), cannot build the subdocument style numbering mapping",
                                )
                            })?;
                            let our_anum =
                                num_by_id(main_numbering, &first_our_num).and_then(|num_el| {
                                    tag_descendants(
                                        main_numbering,
                                        num_el,
                                        ns_uri::W,
                                        "abstractNumId",
                                    )
                                    .iter()
                                    .find_map(|node| {
                                        main_numbering
                                            .attr(*node, ns_uri::W, "val")
                                            .map(str::to_owned)
                                    })
                                });
                            if let Some(our_anum) = our_anum {
                                let sub_key: i64 = sub_anum.parse().map_err(|_| {
                                    malformed(format!(
                                        "abstractNumId {sub_anum:?} is not an integer"
                                    ))
                                })?;
                                let main_key: i64 = our_anum.parse().map_err(|_| {
                                    malformed(format!(
                                        "abstractNumId {our_anum:?} is not an integer"
                                    ))
                                })?;
                                anum_id_mapping.insert(sub_key, main_key);
                            }
                        }
                    }
                }
            }
        }
        finish_style_refs(element_tree, element, &style_id, &our_style_id);
        // Refresh the main stylesheet at the loop tail.
        our_style_ids = style_ids_of(main_styles);
    }
    Ok(())
}

/// Style-reference rewriting (the tail of the upstream add_styles loop body): when `our_style_id != style_id`,
/// rewrite every tblStyle/pStyle/rStyle with val==style_id in the element subtree.
fn finish_style_refs(
    element_tree: &mut XmlDocument,
    element: NodeId,
    style_id: &str,
    our_style_id: &str,
) {
    if our_style_id == style_id {
        return;
    }
    for node in element_tree.descendants(element).into_iter().skip(1) {
        let is_ref = is_tag(element_tree, node, ns_uri::W, "tblStyle")
            || is_tag(element_tree, node, ns_uri::W, "pStyle")
            || is_tag(element_tree, node, ns_uri::W, "rStyle");
        if is_ref && element_tree.attr(node, ns_uri::W, "val") == Some(style_id) {
            element_tree.set_attr(node, ns_uri::W, "val", our_style_id.to_string());
        }
    }
}

/// Upstream `add_linked_styles`: when the first `w:link/@w:val` value of element (the style copy) is not
/// in the main table after mapping, deep-copy the sub linked style and append it to the main styles.
fn merge_linked_styles(
    main_styles: &mut XmlDocument,
    sub_styles: &XmlDocument,
    element: NodeId,
    style_id2name: &HashMap<String, String>,
    style_name2id: &HashMap<String, String>,
    main_styles_dirty: &mut bool,
) -> Result<(), Error> {
    let Some(linked_id) = tag_descendants(main_styles, element, ns_uri::W, "link")
        .iter()
        .find_map(|node| main_styles.attr(*node, ns_uri::W, "val").map(str::to_owned))
    else {
        return Ok(());
    };
    let our_linked_id = mapped_style_id(&linked_id, style_id2name, style_name2id);
    let our_style_ids = style_ids_of(main_styles);
    if our_style_ids.contains(&our_linked_id) {
        return Ok(());
    }
    // Upstream get_by_id looks up the sub's original id and only deep-copies/appends on a hit.
    if let Some(src) = style_by_id(sub_styles, &linked_id) {
        let copy = main_styles
            .deepcopy_element(sub_styles, src)
            .map_err(copy_error)?;
        main_styles.append_child(main_styles.root(), copy);
        *main_styles_dirty = true;
    }
    Ok(())
}

/// Upstream `add_numberings`:
/// - num_ids: deduplicated `w:numId/@w:val`s in the element subtree (an int set; Rust iterates in
///   ascending order; CPython iterates small-int sets in ascending order too);
/// - an empty set returns immediately (the main numbering part is not touched);
/// - `_next_numbering_ids` is called once **before** the loop: multiple numIds on the same element all
///   map to the same next values (upstream behavior, copied verbatim);
/// - for each numId: a sub lookup of `w:num[@w:numId=X]` with no hit continues (a sub missing the
///   numbering part is equivalent to an empty query); deep copy -> rewrite numId -> record num_id_mapping ->
///   the first `w:abstractNumId` in the copy (upstream IndexError when absent) -> when anum is unmapped,
///   look it up in the sub `w:abstractNum`: no hit **continues** (the num_id_mapping
///   residue stays, the w:num is not inserted, and the tail still rewrites references per the mapping -- the dangling semantics are copied verbatim),
///   on a hit record the anum mapping, rewrite the copy and the abstractNum copy, assign nsid a
///   deterministic collision-free new value, and `_insert_abstract_num`;
///   if already mapped, only rewrite the copy's anum reference; `_insert_num` runs each round;
/// - tail: every `w:numId` in the element subtree is rewritten per the mapping (unmapped ones stay as-is).
#[allow(clippy::too_many_arguments)]
fn merge_numberings(
    element_tree: &mut XmlDocument,
    element: NodeId,
    sub_numbering: Option<&XmlDocument>,
    main_numbering: &mut Option<XmlDocument>,
    num_id_mapping: &mut HashMap<i64, i64>,
    anum_id_mapping: &mut HashMap<i64, i64>,
    main_numbering_dirty: &mut bool,
) -> Result<(), Error> {
    // num_ids: int set (ascending iteration).
    let mut num_ids: Vec<i64> = Vec::new();
    for node in tag_descendants(element_tree, element, ns_uri::W, "numId") {
        if let Some(val) = element_tree.attr(node, ns_uri::W, "val") {
            let value: i64 = val
                .parse()
                .map_err(|_| malformed(format!("w:numId/@w:val {val:?} is not an integer")))?;
            if !num_ids.contains(&value) {
                num_ids.push(value);
            }
        }
    }
    if num_ids.is_empty() {
        return Ok(());
    }
    num_ids.sort_unstable();

    // The caller has already lazily created the main numbering part per docxcompose `numbering_part()` semantics.
    let main_numbering = main_numbering
        .as_mut()
        .ok_or_else(|| malformed("main document is missing the numbering part (word/numbering.xml), cannot merge subdocument numbering"))?;
    let (next_num_id, next_anum_id) = next_numbering_ids(main_numbering)?;

    for num_id in num_ids {
        if num_id_mapping.contains_key(&num_id) {
            continue;
        }
        // Look up w:num in the sub (sub missing the numbering part -> empty query -> continue).
        let Some(sub_tree) = sub_numbering else {
            continue;
        };
        let Some(num_src) = num_by_id_value(sub_tree, num_id) else {
            continue;
        };
        let num_copy = main_numbering
            .deepcopy_element(sub_tree, num_src)
            .map_err(copy_error)?;
        main_numbering.set_attr(num_copy, ns_uri::W, "numId", next_num_id.to_string());
        num_id_mapping.insert(num_id, next_num_id);

        // First w:abstractNumId in the copy (upstream //w:abstractNumId on the detached copy is
        // equivalent to the first descendant; its absence raises IndexError directly).
        let anum_node = tag_descendants(main_numbering, num_copy, ns_uri::W, "abstractNumId")
            .into_iter()
            .next();
        let Some(anum_node) = anum_node else {
            return Err(malformed(format!(
                "subdocument w:num[@w:numId={num_id}] is missing the w:abstractNumId child element (part word/numbering.xml)"
            )));
        };
        let anum_val: i64 = main_numbering
            .attr(anum_node, ns_uri::W, "val")
            .ok_or_else(|| malformed("w:abstractNumId is missing the w:val attribute"))?
            .parse()
            .map_err(|_| malformed("w:abstractNumId/@w:val is not an integer"))?;

        match anum_id_mapping.get(&anum_val) {
            None => {
                // Look up w:abstractNum in the sub: no hit continues (the mapping-residue semantics are copied verbatim).
                let Some(anum_src) = anum_by_id_value(sub_tree, anum_val) else {
                    continue;
                };
                let anum_copy = main_numbering
                    .deepcopy_element(sub_tree, anum_src)
                    .map_err(copy_error)?;
                anum_id_mapping.insert(anum_val, next_anum_id);
                main_numbering.set_attr(anum_node, ns_uri::W, "val", next_anum_id.to_string());
                main_numbering.set_attr(
                    anum_copy,
                    ns_uri::W,
                    "abstractNumId",
                    next_anum_id.to_string(),
                );
                // Upstream uses random numbers to ensure nsid uniqueness. Rust output must be reproducible, so use
                // a stable hash of the copy's content and the old/new abstractNumIds, then linearly probe among existing values;
                // the semantics match upstream and there is no byte drift across builds.
                if let Some(nsid) = tag_descendants(main_numbering, anum_copy, ns_uri::W, "nsid")
                    .into_iter()
                    .next()
                {
                    let old = main_numbering
                        .attr(nsid, ns_uri::W, "val")
                        .unwrap_or_default()
                        .to_string();
                    let value =
                        unique_nsid(main_numbering, anum_copy, anum_val, next_anum_id, &old);
                    main_numbering.set_attr(nsid, ns_uri::W, "val", value);
                }
                insert_abstract_num(main_numbering, anum_copy, main_numbering_dirty);
            }
            Some(mapped) => {
                main_numbering.set_attr(anum_node, ns_uri::W, "val", mapped.to_string());
            }
        }
        insert_num(main_numbering, num_copy, main_numbering_dirty);
    }

    // Tail: rewrite every w:numId in the element subtree per the mapping (unmapped ones stay as-is).
    for node in tag_descendants(element_tree, element, ns_uri::W, "numId") {
        if let Some(val) = element_tree
            .attr(node, ns_uri::W, "val")
            .and_then(|v| v.parse::<i64>().ok())
        {
            if let Some(mapped) = num_id_mapping.get(&val) {
                element_tree.set_attr(node, ns_uri::W, "val", mapped.to_string());
            }
        }
    }
    Ok(())
}

/// Upstream `_next_numbering_ids`: the maximum numId among the main numbering part's `w:num` plus 1
/// (or 1 if none); the maximum abstractNumId among `w:abstractNum` plus 1 (or 0 if none).
fn next_numbering_ids(tree: &XmlDocument) -> Result<(i64, i64), Error> {
    let root = tree.root();
    let mut next_num_id = 1i64;
    for node in tag_descendants(tree, root, ns_uri::W, "num") {
        if let Some(value) = tree
            .attr(node, ns_uri::W, "numId")
            .and_then(|v| v.parse::<i64>().ok())
        {
            let candidate = value.checked_add(1).ok_or_else(|| {
                malformed(format!(
                    "w:num/@w:numId {value} has reached the i64 maximum, cannot allocate the next number"
                ))
            })?;
            next_num_id = next_num_id.max(candidate);
        }
    }
    let mut next_anum_id = 0i64;
    for node in tag_descendants(tree, root, ns_uri::W, "abstractNum") {
        if let Some(value) = tree
            .attr(node, ns_uri::W, "abstractNumId")
            .and_then(|v| v.parse::<i64>().ok())
        {
            let candidate = value.checked_add(1).ok_or_else(|| {
                malformed(format!(
                    "w:abstractNum/@w:abstractNumId {value} has reached the i64 maximum, cannot allocate the next number"
                ))
            })?;
            next_anum_id = next_anum_id.max(candidate);
        }
    }
    Ok((next_num_id, next_anum_id))
}

/// Generate a stable, package-unique eight-digit hexadecimal nsid for a copied abstract numbering.
/// docxcompose uses random numbers; the stable hash makes Rust builds reproducible while preserving the actually required
/// "do not share the nsid with existing numbering instances" semantics.
fn unique_nsid(
    tree: &XmlDocument,
    detached_anum: NodeId,
    source_anum_id: i64,
    target_anum_id: i64,
    old_value: &str,
) -> String {
    let mut hash = 0x811C_9DC5u32; // FNV-1a offset basis
    for byte in tree
        .serialize_subtree(detached_anum)
        .bytes()
        .chain(source_anum_id.to_le_bytes())
        .chain(target_anum_id.to_le_bytes())
    {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }

    let mut used: HashSet<String> = tag_descendants(tree, tree.root(), ns_uri::W, "nsid")
        .into_iter()
        .filter_map(|node| tree.attr(node, ns_uri::W, "val"))
        .map(|value| value.to_ascii_uppercase())
        .collect();
    // "Rewriting" requires not only no collision with already mounted nodes, but also no accidental retention of the source value.
    used.insert(old_value.to_ascii_uppercase());
    loop {
        let candidate = format!("{hash:08X}");
        if !used.contains(&candidate) {
            return candidate;
        }
        hash = hash.wrapping_add(0x9E37_79B9);
    }
}

/// Upstream `_insert_num`: when there is a `w:num`, insert **before the last num** (the upstream comment says
/// "after", opposite to the code -- align with the code); when there is no num, append to the root.
fn insert_num(tree: &mut XmlDocument, element: NodeId, dirty: &mut bool) {
    let root = tree.root();
    let nums = tag_descendants(tree, root, ns_uri::W, "num");
    if let Some(last) = nums.last().copied() {
        if let Some(parent) = tree.parent(last) {
            if let Some(position) = tree.children(parent).iter().position(|&c| c == last) {
                tree.insert_child_at(parent, position, element);
                *dirty = true;
                return;
            }
        }
    }
    tree.append_child(root, element);
    *dirty = true;
}

/// Upstream `_insert_abstract_num`: when there is a `w:num`, insert **before the first num**;
/// when there is no num, insert at the first root position (`insert(0)`).
fn insert_abstract_num(tree: &mut XmlDocument, element: NodeId, dirty: &mut bool) {
    let root = tree.root();
    let nums = tag_descendants(tree, root, ns_uri::W, "num");
    if let Some(first) = nums.first().copied() {
        if let Some(parent) = tree.parent(first) {
            if let Some(position) = tree.children(parent).iter().position(|&c| c == first) {
                tree.insert_child_at(parent, position, element);
                *dirty = true;
                return;
            }
        }
    }
    tree.insert_child_at(root, 0, element);
    *dirty = true;
}

// =====================================================================
// Common small utilities
// =====================================================================

/// Construct a main-package-unsupported DEV error (ADR-007 compatibility boundary, with a context explanation).
fn malformed(reason: impl Into<String>) -> Error {
    Error::Opc(OpcError::Malformed {
        reason: reason.into(),
    })
}

/// Cross-document subtree copy failure: uniformly collapsed into a main-package-unsupported error (namespace prefix conflicts etc.:
/// upstream lxml deepcopy + append can evade them via automatic renaming, while this implementation keeps lexical prefixes and
/// cannot adapt when prefixes conflict).
fn copy_error(source: docxtpl_xml::XmlError) -> Error {
    malformed(format!(
        "failed to copy the subdocument part tree: {source}"
    ))
}

/// Whether a node has the given namespace-qualified name.
fn is_tag(doc: &XmlDocument, id: NodeId, ns: &str, local: &str) -> bool {
    doc.tag(id).is_some_and(|q| q.ns == ns && q.local == local)
}

/// Nodes with the given qualified name on the XPath descendant axis (excluding the starting node itself), in document order.
fn tag_descendants(doc: &XmlDocument, root: NodeId, ns: &str, local: &str) -> Vec<NodeId> {
    doc.descendants(root)
        .into_iter()
        .skip(1)
        .filter(|&id| is_tag(doc, id, ns, local))
        .collect()
}

/// Collect all `r:*` relationship attributes in a subtree. Under the r namespace, OOXML's
/// `id/embed/link/dm/lo/qs/cs` are all relationship Ids, so no separate whitelist is needed for footnote images,
/// hyperlinks or SmartArt.
fn relationship_attributes(doc: &XmlDocument, root: NodeId) -> Vec<(NodeId, String, String)> {
    let mut out = Vec::new();
    for node in doc.descendants(root) {
        if doc.tag(node).is_none() {
            continue;
        }
        for (name, value) in doc.attrs(node) {
            if name.ns == ns_uri::R {
                out.push((node, name.local.clone(), value.clone()));
            }
        }
    }
    out
}

/// Equivalent parsing of docxcompose `FieldBase.fieldname_and_format_search_expr`.
/// Must be uppercase `DOCPROPERTY` and end with `\* MERGEFORMAT`; the part before the date
/// `\@` switch is the property name.
fn docproperty_name(instruction: &str) -> Option<String> {
    let marker = "DOCPROPERTY";
    let marker_at = instruction.find(marker)?;
    let mut rest = instruction.get(marker_at + marker.len()..)?;
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    rest = rest.trim_start();
    if !rest.contains(r"\* MERGEFORMAT") {
        return None;
    }
    let name_part = rest.split('\\').next()?.trim();
    let name = if let Some(quoted) = name_part.strip_prefix('"') {
        let end = quoted.rfind('"')?;
        quoted[..end].to_string()
    } else {
        name_part.trim_end_matches('"').trim().to_string()
    };
    (!name.is_empty()).then_some(name)
}

fn dissolve_simple_fields(
    doc: &mut XmlDocument,
    root: NodeId,
    property_name: &str,
) -> Result<(), Error> {
    let fields = tag_descendants(doc, root, ns_uri::W, "fldSimple");
    for field in fields {
        let matches = doc
            .attr(field, ns_uri::W, "instr")
            .and_then(docproperty_name)
            .is_some_and(|name| name == property_name);
        if !matches {
            continue;
        }
        let Some(parent) = doc.parent(field) else {
            continue;
        };
        let index = doc
            .children(parent)
            .iter()
            .position(|&child| child == field)
            .ok_or_else(|| malformed("the w:fldSimple parent does not contain this field"))?;
        let value = doc
            .children(field)
            .iter()
            .copied()
            .find(|&child| doc.tag(child).is_some())
            .ok_or_else(|| malformed("w:fldSimple has no cached value element to preserve"))?;
        // Upstream deletes the field after deepcopy; moving it directly out of the only retained subtree is equivalent and
        // avoids an extra serialization round trip in the same arena.
        doc.detach(value);
        doc.detach(field);
        doc.insert_child_at(parent, index, value);
    }
    Ok(())
}

fn run_has_field_char(doc: &XmlDocument, run: NodeId, kind: &str) -> bool {
    doc.children(run).iter().any(|&child| {
        is_tag(doc, child, ns_uri::W, "fldChar")
            && doc.attr(child, ns_uri::W, "fldCharType") == Some(kind)
    })
}

fn dissolve_complex_fields(
    doc: &mut XmlDocument,
    root: NodeId,
    property_name: &str,
) -> Result<(), Error> {
    let candidates: Vec<NodeId> = tag_descendants(doc, root, ns_uri::W, "instrText")
        .into_iter()
        .filter(|&node| {
            doc.element_text(node)
                .is_some_and(|text| text.contains("DOCPROPERTY "))
        })
        .collect();

    for instruction_node in candidates {
        let Some(instruction_run) = doc.parent(instruction_node) else {
            continue;
        };
        if !is_tag(doc, instruction_run, ns_uri::W, "r") {
            continue;
        }
        let Some(paragraph) = doc.parent(instruction_run) else {
            continue;
        };
        let siblings = doc.children(paragraph).to_vec();
        let Some(instruction_index) = siblings.iter().position(|&node| node == instruction_run)
        else {
            continue;
        };
        let begin_index = (0..instruction_index)
            .rev()
            .find(|&index| {
                is_tag(doc, siblings[index], ns_uri::W, "r")
                    && run_has_field_char(doc, siblings[index], "begin")
            })
            .ok_or_else(|| malformed("complex DOCPROPERTY field is missing its begin run"))?;
        let end_index = ((instruction_index + 1)..siblings.len())
            .find(|&index| {
                is_tag(doc, siblings[index], ns_uri::W, "r")
                    && run_has_field_char(doc, siblings[index], "end")
            })
            .ok_or_else(|| malformed("complex DOCPROPERTY field is missing its end run"))?;
        let separate_index = ((instruction_index + 1)..end_index).find(|&index| {
            is_tag(doc, siblings[index], ns_uri::W, "r")
                && run_has_field_char(doc, siblings[index], "separate")
        });
        let instruction_end = separate_index.unwrap_or(end_index);
        let mut combined = String::new();
        for &run in &siblings[(begin_index + 1)..instruction_end] {
            if !is_tag(doc, run, ns_uri::W, "r") {
                continue;
            }
            for &child in doc.children(run) {
                if is_tag(doc, child, ns_uri::W, "instrText") {
                    if let Some(text) = doc.element_text(child) {
                        combined.push_str(text);
                    }
                }
            }
        }
        if docproperty_name(&combined).as_deref() != Some(property_name) {
            continue;
        }

        let mut remove = vec![siblings[begin_index], siblings[end_index]];
        let through = separate_index.unwrap_or(end_index.saturating_sub(1));
        for &run in &siblings[(begin_index + 1)..=through] {
            if is_tag(doc, run, ns_uri::W, "r") {
                remove.push(run);
            }
        }
        let mut seen = HashSet::new();
        for run in remove {
            if seen.insert(run) {
                doc.detach(run);
            }
        }
    }
    Ok(())
}

/// Collect namespace declarations visible at an insertion point. Ancestors are applied from root inward; inner same-prefix declarations
/// override the URI but keep the first occurrence's position, keeping the attribute order of the fragment wrapper stable.
fn namespaces_in_scope(doc: &XmlDocument, node: NodeId) -> Vec<(String, String)> {
    let mut ancestors = Vec::new();
    let mut current = Some(node);
    while let Some(id) = current {
        ancestors.push(id);
        current = doc.parent(id);
    }
    ancestors.reverse();

    let mut namespaces: Vec<(String, String)> = Vec::new();
    for id in ancestors {
        for (prefix, uri) in doc.ns_decls(id) {
            if let Some((_, known_uri)) = namespaces
                .iter_mut()
                .find(|(known_prefix, _)| known_prefix == prefix)
            {
                *known_uri = uri.clone();
            } else {
                namespaces.push((prefix.clone(), uri.clone()));
            }
        }
    }
    namespaces
}

/// Parse a standalone XML element, uniformly mapping to Subdoc's public malformed error.
fn parse_element(xml: &str) -> Result<XmlDocument, Error> {
    XmlDocument::parse_strict(xml, &XmlLimits::default()).map_err(copy_error)
}

/// `deepcopy` within the same document. Transiting via a separately parsed tree avoids borrowing the same `XmlDocument` mutably and immutably
/// at the same time; the qualified-name/namespace semantics are unchanged after serialization.
fn clone_element_within(tree: &mut XmlDocument, element: NodeId) -> Result<NodeId, Error> {
    let serialized = tree.serialize_subtree(element);
    // Like lxml tostring(element), serialize_subtree does not hoist all ancestor declarations out of thin air;
    // wrap a temporary root in the current scope so inherited prefixes such as `w:num` stay resolvable.
    let mut wrapper = String::from("<copy-root");
    for (prefix, uri) in namespaces_in_scope(tree, element) {
        if prefix.is_empty() {
            wrapper.push_str(" xmlns=\"");
        } else {
            wrapper.push_str(" xmlns:");
            wrapper.push_str(&prefix);
            wrapper.push_str("=\"");
        }
        wrapper.push_str(&escape_xml_attribute(&uri));
        wrapper.push('"');
    }
    wrapper.push('>');
    wrapper.push_str(&serialized);
    wrapper.push_str("</copy-root>");
    let source = parse_element(&wrapper)?;
    let source_element = source
        .children(source.root())
        .iter()
        .copied()
        .find(|&node| source.tag(node).is_some())
        .ok_or_else(|| {
            malformed(
                "internal error: the numbering-element copy wrapper is missing its child element",
            )
        })?;
    tree.deepcopy_element(&source, source_element)
        .map_err(copy_error)
}

fn escape_xml_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            _ => out.push(ch),
        }
    }
    out
}

/// First value of `.//w:numPr/w:numId/@w:val` (accepts only a numId that is a direct child of numPr).
fn first_num_id_under_num_pr(doc: &XmlDocument, element: NodeId) -> Option<String> {
    for num_pr in tag_descendants(doc, element, ns_uri::W, "numPr") {
        for &child in doc.children(num_pr) {
            if is_tag(doc, child, ns_uri::W, "numId") {
                if let Some(value) = doc.attr(child, ns_uri::W, "val") {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// First node of `.//w:pPr/w:pStyle[@w:val=...]/parent::w:pPr`.
fn paragraph_props_for_style(doc: &XmlDocument, element: NodeId, style_id: &str) -> Option<NodeId> {
    doc.descendants(element)
        .into_iter()
        .skip(1)
        .find(|&candidate| {
            is_tag(doc, candidate, ns_uri::W, "pPr")
                && doc.children(candidate).iter().any(|&child| {
                    is_tag(doc, child, ns_uri::W, "pStyle")
                        && doc.attr(child, ns_uri::W, "val") == Some(style_id)
                })
        })
}

/// Before calling the free function `merge_styles`, determine whether its path will lazily touch the main numbering
/// part, so the Composer holding the Package/relationship state can create it.
#[allow(clippy::too_many_arguments)]
fn style_merge_needs_main_numbering(
    element_tree: &XmlDocument,
    element: NodeId,
    sub_styles: &XmlDocument,
    main_styles: &XmlDocument,
    style_id2name: &HashMap<String, String>,
    style_name2id: &HashMap<String, String>,
    sub_numbering: Option<&XmlDocument>,
) -> bool {
    let main_ids = style_ids_of(main_styles);
    let mut seen = HashSet::new();
    for node in element_tree.descendants(element).into_iter().skip(1) {
        let is_ref = is_tag(element_tree, node, ns_uri::W, "tblStyle")
            || is_tag(element_tree, node, ns_uri::W, "pStyle")
            || is_tag(element_tree, node, ns_uri::W, "rStyle");
        if !is_ref {
            continue;
        }
        let Some(style_id) = element_tree.attr(node, ns_uri::W, "val") else {
            continue;
        };
        if !seen.insert(style_id.to_string()) {
            continue;
        }
        let mapped = mapped_style_id(style_id, style_id2name, style_name2id);
        let Some(sub_style) = style_by_id(sub_styles, style_id) else {
            continue;
        };
        let sub_num_id = tag_descendants(sub_styles, sub_style, ns_uri::W, "numId")
            .into_iter()
            .find_map(|id| sub_styles.attr(id, ns_uri::W, "val"));
        let Some(sub_num_id) = sub_num_id else {
            continue;
        };

        if !main_ids.contains(&mapped) {
            // The new style copy then calls add_numberings unconditionally; if a numId exists it first
            // calls `_next_numbering_ids()`, creating the main numbering part.
            return true;
        }

        // The existing-style branch calls numbering_part() only when the sub numId resolves to an abstractNumId and the main style
        // itself references a numId.
        let sub_anum_exists = sub_numbering.is_some_and(|numbering| {
            num_by_id(numbering, sub_num_id).is_some_and(|num| {
                !tag_descendants(numbering, num, ns_uri::W, "abstractNumId").is_empty()
            })
        });
        if !sub_anum_exists {
            continue;
        }
        if let Some(main_style) = style_by_id(main_styles, &mapped) {
            if !tag_descendants(main_styles, main_style, ns_uri::W, "numId").is_empty() {
                return true;
            }
        }
    }
    false
}

/// The direct `w:body` of the document root (always present in a standard docx).
fn body_of(doc: &XmlDocument) -> Result<NodeId, Error> {
    doc.children(doc.root())
        .iter()
        .copied()
        .find(|&child| is_tag(doc, child, ns_uri::W, "body"))
        .ok_or_else(|| malformed("document is missing w:body (part word/document.xml)"))
}

/// Set an attribute and return true when the value changes (dirty detection).
fn set_attr_if_changed(
    doc: &mut XmlDocument,
    id: NodeId,
    ns: &str,
    local: &str,
    value: &str,
) -> bool {
    if doc.attr(id, ns, local) == Some(value) {
        return false;
    }
    doc.set_attr(id, ns, local, value.to_string());
    true
}

/// Direct child `w:style[@w:styleId=X]` of the styles root (python-docx get_by_id).
fn style_by_id(doc: &XmlDocument, style_id: &str) -> Option<NodeId> {
    doc.children(doc.root()).iter().copied().find(|&child| {
        is_tag(doc, child, ns_uri::W, "style")
            && doc.attr(child, ns_uri::W, "styleId") == Some(style_id)
    })
}

/// styleIds of all `w:style`s at the styles root (in document order).
fn style_ids_of(doc: &XmlDocument) -> Vec<String> {
    doc.children(doc.root())
        .iter()
        .filter(|&&child| is_tag(doc, child, ns_uri::W, "style"))
        .filter_map(|&child| doc.attr(child, ns_uri::W, "styleId").map(str::to_owned))
        .collect()
}

/// The style's `w:name/@w:val` (a direct-child `w:name`; returns None when missing).
fn style_name_of(doc: &XmlDocument, style_el: NodeId) -> Option<String> {
    doc.children(style_el)
        .iter()
        .copied()
        .find(|&child| is_tag(doc, child, ns_uri::W, "name"))
        .and_then(|name| doc.attr(name, ns_uri::W, "val").map(str::to_owned))
}

/// Sub stylesheet id -> name (styles without w:name get no mapping and are returned as-is when mapped,
/// matching the upstream behavior of a `None` lookup returning the default).
fn style_id_name_map(doc: &XmlDocument) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for child in style_elements(doc) {
        if let (Some(id), Some(name)) = (
            doc.attr(child, ns_uri::W, "styleId").map(str::to_owned),
            style_name_of(doc, child),
        ) {
            map.insert(id, name);
        }
    }
    map
}

/// Main stylesheet name -> id.
fn style_name_id_map(doc: &XmlDocument) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for child in style_elements(doc) {
        if let (Some(id), Some(name)) = (
            doc.attr(child, ns_uri::W, "styleId").map(str::to_owned),
            style_name_of(doc, child),
        ) {
            map.insert(name, id);
        }
    }
    map
}

/// Set of direct-child `w:style` nodes at the styles root.
fn style_elements(doc: &XmlDocument) -> Vec<NodeId> {
    doc.children(doc.root())
        .iter()
        .copied()
        .filter(|&child| is_tag(doc, child, ns_uri::W, "style"))
        .collect()
}

/// Upstream `mapped_style_id`: sub id -> name -> main id; if any link is missing, return as-is.
fn mapped_style_id(
    style_id: &str,
    style_id2name: &HashMap<String, String>,
    style_name2id: &HashMap<String, String>,
) -> String {
    if let Some(name) = style_id2name.get(style_id) {
        if let Some(our_id) = style_name2id.get(name) {
            return our_id.clone();
        }
    }
    style_id.to_owned()
}

/// Look up `w:num` by string numId (upstream `'.//w:num[@w:numId="%s"]'`, first hit).
fn num_by_id(tree: &XmlDocument, num_id: &str) -> Option<NodeId> {
    tag_descendants(tree, tree.root(), ns_uri::W, "num")
        .into_iter()
        .find(|&node| tree.attr(node, ns_uri::W, "numId") == Some(num_id))
}

/// Look up `w:num` by integer numId.
fn num_by_id_value(tree: &XmlDocument, num_id: i64) -> Option<NodeId> {
    num_by_id(tree, &num_id.to_string())
}

/// Look up `w:abstractNum` by string abstractNumId.
fn anum_by_id(tree: &XmlDocument, anum_id: &str) -> Option<NodeId> {
    tag_descendants(tree, tree.root(), ns_uri::W, "abstractNum")
        .into_iter()
        .find(|&node| tree.attr(node, ns_uri::W, "abstractNumId") == Some(anum_id))
}

/// Look up `w:abstractNum` by integer abstractNumId.
fn anum_by_id_value(tree: &XmlDocument, anum_id: i64) -> Option<NodeId> {
    anum_by_id(tree, &anum_id.to_string())
}

/// python-docx `document.element.sectPr_lst`: the in-paragraph
/// `w:p/w:pPr/w:sectPr` plus the body's direct `w:sectPr`, listed in document order.
fn section_nodes(doc: &XmlDocument, body: NodeId) -> Vec<NodeId> {
    let mut sections = Vec::new();
    for child in doc.children(body) {
        if is_tag(doc, *child, ns_uri::W, "p") {
            for grand in doc.children(*child) {
                if is_tag(doc, *grand, ns_uri::W, "pPr") {
                    sections.extend(
                        doc.children(*grand)
                            .iter()
                            .copied()
                            .filter(|&sect| is_tag(doc, sect, ns_uri::W, "sectPr")),
                    );
                }
            }
        } else if is_tag(doc, *child, ns_uri::W, "sectPr") {
            sections.push(*child);
        }
    }
    sections
}

/// `Section.start_type` getter: default to nextPage when `w:type` or its val is missing.
fn section_start_type(doc: &XmlDocument, sect_pr: NodeId) -> String {
    doc.children(sect_pr)
        .iter()
        .copied()
        .find(|&child| is_tag(doc, child, ns_uri::W, "type"))
        .and_then(|node| doc.attr(node, ns_uri::W, "val"))
        .unwrap_or("nextPage")
        .to_string()
}

/// `Section.start_type` setter. `nextPage` deletes the explicit w:type; other values are inserted
/// before pgSz and its following elements per the CT_SectPr schema order.
fn set_section_start_type(
    doc: &mut XmlDocument,
    sect_pr: NodeId,
    value: &str,
) -> Result<bool, Error> {
    let existing = doc
        .children(sect_pr)
        .iter()
        .copied()
        .find(|&child| is_tag(doc, child, ns_uri::W, "type"));
    if value == "nextPage" {
        if let Some(node) = existing {
            doc.detach(node);
            return Ok(true);
        }
        return Ok(false);
    }
    if let Some(node) = existing {
        return Ok(set_attr_if_changed(doc, node, ns_uri::W, "val", value));
    }

    let source = parse_element(&format!(r#"<w:type xmlns:w="{}"/>"#, ns_uri::W))?;
    let type_node = doc
        .deepcopy_element(&source, source.root())
        .map_err(copy_error)?;
    doc.set_attr(type_node, ns_uri::W, "val", value.to_string());
    const SUCCESSORS: &[&str] = &[
        "pgSz",
        "pgMar",
        "paperSrc",
        "pgBorders",
        "lnNumType",
        "pgNumType",
        "cols",
        "formProt",
        "vAlign",
        "noEndnote",
        "titlePg",
        "textDirection",
        "bidi",
        "rtlGutter",
        "docGrid",
        "printerSettings",
        "sectPrChange",
    ];
    let insertion = doc
        .children(sect_pr)
        .iter()
        .position(|&child| {
            doc.tag(child).is_some_and(|qname| {
                qname.ns == ns_uri::W && SUCCESSORS.contains(&qname.local.as_str())
            })
        })
        .unwrap_or(doc.children(sect_pr).len());
    doc.insert_child_at(sect_pr, insertion, type_node);
    Ok(true)
}

/// Read an XmlPart, sharing the XML UTF-8/UTF-16/UTF-32 decoding rules with the main render path.
fn read_xml_part(pkg: &Package, name: &str) -> Result<String, Error> {
    let bytes = pkg
        .part(name)
        .ok_or_else(|| Error::Opc(OpcError::MissingPart { uri: name.into() }))?
        .bytes()?;
    super::decode_xml_bytes(bytes, name)
}

/// Parse a part into a tree and strip whitespace (the python-docx oxml parser `remove_blank_text`).
fn load_xml_tree(pkg: &Package, name: &str) -> Result<XmlDocument, Error> {
    let xml = read_xml_part(pkg, name)?;
    let mut doc = XmlDocument::parse_strict(&xml, &XmlLimits::default()).map_err(|source| {
        Error::Render(docxtpl_template::RenderError::Xml {
            part: name.to_string(),
            source,
        })
    })?;
    doc.strip_blank_text();
    Ok(doc)
}

/// The part with the given reltype among the package-root relationships. python-docx `package.part_related_by`
/// raises ValueError on multiple relationships of the same type, so here too the first is not silently chosen.
fn root_related_part(
    pkg: &Package,
    rel_type: &str,
    context: &str,
) -> Result<Option<String>, Error> {
    let matches: Vec<&Relationship> = pkg
        .root_relationships()
        .iter()
        .filter(|rel| rel.rel_type == rel_type)
        .collect();
    if matches.len() > 1 {
        return Err(malformed(format!(
            "{context}: the package root has {} relationships of the same type",
            matches.len()
        )));
    }
    let Some(rel) = matches.first() else {
        return Ok(None);
    };
    if rel.target_mode != TargetMode::Internal {
        return Err(malformed(format!(
            "{context}: the relationship must point to an in-package part"
        )));
    }
    let uri = resolve_part_target(None, &rel.target).ok_or_else(|| {
        malformed(format!(
            "{context}: target {:?} cannot be resolved",
            rel.target
        ))
    })?;
    if !pkg.contains(uri.as_str()) {
        return Err(malformed(format!(
            "{context}: target part {} does not exist",
            uri.as_str()
        )));
    }
    Ok(Some(uri.as_str().to_string()))
}

/// Internal target part name with the given reltype in a part's main-document rels
/// (python-docx `part_related_by`; returns None when nothing matches).
fn related_part(pkg: &Package, owner: &str, rel_type: &str) -> Option<String> {
    let owner_uri = PartUri::new(owner).ok()?;
    let base = owner_uri.parent();
    let rels = pkg.relationships_of(owner)?;
    let rel = rels
        .iter()
        .find(|rel| rel.rel_type == rel_type && rel.target_mode == TargetMode::Internal)?;
    resolve_part_target(base.as_ref(), &rel.target).map(|uri| uri.as_str().to_string())
}

/// group(1) of `FILENAME_IDX_RE = ([a-zA-Z/_-]+)([1-9][0-9]*)?`:
/// the letter/slash segment at the start of a part name; when the segment is empty (starts with a digit etc.) upstream match is None and
/// taking the group panics, so return None.
fn filename_idx_prefix(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut end = 0usize;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphabetic() || matches!(bytes[end], b'/' | b'_' | b'-'))
    {
        end += 1;
    }
    (end > 0).then(|| name[..end].to_string())
}

/// group(2) of `FILENAME_IDX_RE`: the `[1-9][0-9]*` digits immediately following the prefix segment.
fn filename_idx_number(name: &str, prefix_len: usize) -> Option<i64> {
    let rest = name.get(prefix_len..)?;
    let bytes = rest.as_bytes();
    if bytes.is_empty() || bytes[0] == b'0' || !bytes[0].is_ascii_digit() {
        return None;
    }
    let end = bytes
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(bytes.len());
    rest[..end].parse().ok()
}

/// Filename extension of a part name (python-docx `PackURI.ext`: if the filename contains a dot take the last segment,
/// otherwise an empty string).
fn extension_of(name: &str) -> String {
    let file_name = name.rsplit('/').next().unwrap_or(name);
    match file_name.rsplit_once('.') {
        Some((_, ext)) => ext.to_string(),
        None => String::new(),
    }
}

/// `rels.get_or_add` semantics (python-docx): an existing rId is reused on an exact
/// (reltype, target, mode) match, otherwise a new one is added filling holes from rId1.
fn get_or_add_rel(
    rels: &mut Relationships,
    rel_type: &str,
    target: &str,
    mode: TargetMode,
) -> String {
    if let Some(rel) = rels.find_matching(rel_type, target, mode) {
        return rel.id.clone();
    }
    let id = rels.next_r_id();
    rels.push(Relationship {
        id: id.clone(),
        rel_type: rel_type.to_string(),
        target: target.to_string(),
        target_mode: mode,
    });
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_idx_matches_upstream_regex() {
        assert_eq!(
            filename_idx_prefix("word/footer1.xml").as_deref(),
            Some("word/footer")
        );
        assert_eq!(
            filename_idx_prefix("word/media/image1.png").as_deref(),
            Some("word/media/image")
        );
        assert_eq!(
            filename_idx_prefix("word/header.xml").as_deref(),
            Some("word/header")
        );
        // Upstream partnames carry a leading / and group(1) contains the /
        assert_eq!(
            filename_idx_prefix("/word/footer1.xml").as_deref(),
            Some("/word/footer")
        );
        // Leading digit: upstream match None -> AttributeError
        assert_eq!(filename_idx_prefix("1word/a.xml"), None);
        // group(2)
        assert_eq!(
            filename_idx_number("word/footer2.xml", "word/footer".len()),
            Some(2)
        );
        assert_eq!(
            filename_idx_number("word/footer.xml", "word/footer".len()),
            None
        );
        assert_eq!(
            filename_idx_number("word/footer01.xml", "word/footer".len()),
            None
        );
        assert_eq!(
            filename_idx_number("word/footerX1.xml", "word/footer".len()),
            None
        );
        assert_eq!(
            filename_idx_number("word/footer3x.png", "word/footer".len()),
            Some(3)
        );
    }

    #[test]
    fn extension_matches_packuri_ext() {
        assert_eq!(extension_of("word/media/image1.png"), "png");
        assert_eq!(extension_of("_rels/.rels"), "rels");
        assert_eq!(extension_of("word/noext"), "");
    }

    #[test]
    fn next_numbering_ids_rejects_i64_overflow() {
        let xml = format!(
            r#"<w:numbering xmlns:w="{}"><w:num w:numId="{}"/></w:numbering>"#,
            ns_uri::W,
            i64::MAX
        );
        let tree = XmlDocument::parse_strict(&xml, &XmlLimits::default()).unwrap();
        let error = next_numbering_ids(&tree).expect_err("overflow must be a controlled error");
        assert!(error.to_string().contains("i64"), "{error}");
    }
}
