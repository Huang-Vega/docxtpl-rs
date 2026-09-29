//! Package-level image registry (P4 ADR-005; P5 ADR-006 adds multi-part
//! relationship scopes).
//!
//! [`ImageInjections`] implements docxtpl-template's [`ImageRegistry`], turning
//! the [`InlineImage`]s encountered during rendering into OPC package changes.
//! Its semantics mirror docxtpl 0.20.2 / python-docx 1.2.0 one by one:
//!
//! - **Independent relationship scope per owner (P5)**: the body and every
//!   header/footer keeps its own cloned copy of the part rels; image rIds and
//!   anchor external-link entries are added only to the scope of the **part
//!   currently being rendered** (`current_rendering_part.relate_to`); parts
//!   whose template has no rels file (e.g. a newly created header) get a new
//!   `word/_rels/headerN.xml.rels` created on demand during rendering;
//! - **Whole-package DFS** collects existing image relationship targets
//!   (`OpcPackage.iter_rels`, skipping external links and deduplicating via
//!   visited) as the baseline for numbering and sha1 deduplication;
//! - **Cross-part sha1 deduplication**: identical image bytes in the body and a
//!   header reuse the same media part
//!   (`Package.get_or_add_image_part` deduplicates package-wide), and each
//!   owner then creates its own relationship;
//! - **Numbering**: `word/media/imageN.ext`, where N fills holes starting at 1
//!   and is counted across extensions (`ImageParts._next_image_partname`, the
//!   PackURI.idx regex `^([a-zA-Z]+)([1-9][0-9]*)?`);
//! - **rId**: each scope fills the first hole starting at rId1 (counting
//!   external relationships); an identical `reltype + target + mode` is reused
//!   (`_Relationships.get_or_add(_ext)`);
//! - **Ordering**: the image rId precedes the external hyperlink entry of its
//!   anchor (the upstream allocation order in `new_pic_anchor`);
//! - **[Content_Types].xml**: new extensions append a Default (the rels/xml
//!   Defaults always exist; images whose extension is whitelisted use Default
//!   rather than Override).
//!
//! Changes are only staged in this struct and written into the [`Package`] once
//! by [`ImageInjections::apply`] after rendering finishes: untouched parts
//! (including the original rels/CT bytes when no image is rendered) are kept
//! as-is.

use std::collections::{HashMap, HashSet};

use docxtpl_opc::{
    relationships_path_of, resolve_part_target, OpcError, Package, PartUri, Relationship,
    Relationships, TargetMode,
};
use docxtpl_rich::{sha1_digest, ImageDigest, InlineImage, LazyImageFile};
use docxtpl_template::{ImageRegistry, ImageRels, ImageResolveError, ResolvedImage};

/// Image relationship type.
const IMAGE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

/// Hyperlink external relationship type (shared by `tpl.build_url_id` and image anchors).
const HYPERLINK_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
const HEADER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

/// Relationship scope for a single rendered part (P5, ADR-006).
///
/// Mirrors upstream `current_rendering_part`: image and anchor relationships
/// are allocated on the **current story part's own rels**; the body, headers
/// and footers never share rId counters.
struct OwnerState {
    /// Owner part name (e.g. `word/document.xml`, `word/header1.xml`).
    name: String,
    /// Name of this part's rels part (e.g. `word/_rels/header1.xml.rels`).
    rels_name: String,
    /// Relationship set (cloned when the template has rels; otherwise empty and created on demand).
    rels: Relationships,
    /// Whether the rels part already exists in the template (decides whether write-back uses set_part_bytes or add_part).
    rels_existed: bool,
    /// Whether relationships were added (unchanged rels are not written back, preserving the original bytes).
    dirty: bool,
    /// Existing relationship lookup grouped by mode, type, and target.
    /// The first relationship in file order wins, matching `find_matching`.
    matching: HashMap<TargetMode, HashMap<String, HashMap<String, String>>>,
    /// Relationship IDs already occupied in this owner scope.
    used_rids: HashSet<String>,
    /// Smallest positive numeric relationship ID not currently occupied.
    next_free_rid: u64,
}

/// Image/relationship/Content Types changes staged during rendering (multi-owner version).
pub(crate) struct ImageInjections {
    /// Index of the main-document owner in owners (always 0; build_url_id always targets it).
    main_index: usize,
    /// Relationship scopes of the rendered parts; owners[0] is the main document and the rest are added in render order.
    owners: Vec<OwnerState>,
    /// Scope index of the currently rendering part (used by resolve_image).
    current: usize,
    /// Set of occupied imageN numbers (including pre-existing template images, shared package-wide).
    used_numbers: HashSet<u64>,
    /// Smallest positive image number not currently occupied.
    next_free_image_number: u64,
    /// sha1 -> absolute part name (pre-existing template parts plus newly added ones, shared package-wide).
    by_sha1: HashMap<ImageDigest, String>,
    /// Media parts staged for byte-backed or lazy file-backed insertion.
    pending_parts: Vec<PendingImagePart>,
    /// Content Types to register: (absolute part name, lowercase extension,
    /// content type). At apply time choose Default or Override from the
    /// then-current CT, so a Subdoc eagerly copying other parts cannot make
    /// the session's initial snapshot stale.
    pending_content_types: Vec<(String, String, String)>,
}

enum PendingImagePart {
    Bytes {
        name: String,
        bytes: Vec<u8>,
    },
    File {
        name: String,
        source: LazyImageFile,
        digest: ImageDigest,
    },
}

impl ImageInjections {
    /// Construct from an opened package: clone the main-document rels and DFS-collect the numbers and sha1 of existing image parts.
    pub(crate) fn new(pkg: &Package, main_name: &str) -> Result<Self, OpcError> {
        // The main document must have a rels file (same behavior as P4; its absence means a malformed package).
        if pkg.relationships_of(main_name).is_none() {
            return Err(OpcError::Malformed {
                reason: format!(
                    "main document is missing its relationships file {}",
                    relationships_path_of(&PartUri::new(main_name)?)
                ),
            });
        }
        let main = load_owner(pkg, main_name)?;

        let existing_images = collect_image_parts(pkg);
        let mut used_numbers = HashSet::new();
        let mut by_sha1 = HashMap::new();
        for name in &existing_images {
            if let Some(part) = pkg.part(name) {
                let digest = sha1_digest(part.bytes()?);
                // If the same bytes are referenced by multiple part names, keep the first (dict insertion order).
                by_sha1.entry(digest).or_insert_with(|| name.clone());
                if let Some(number) = image_number(name) {
                    used_numbers.insert(number);
                }
            }
        }

        let next_free_image_number = first_free_number(&used_numbers);

        Ok(Self {
            main_index: 0,
            owners: vec![main],
            current: 0,
            used_numbers,
            next_free_image_number,
            by_sha1,
            pending_parts: Vec::new(),
            pending_content_types: Vec::new(),
        })
    }

    /// Switch to (or register) the relationship scope of the current rendering
    /// part (P5).
    ///
    /// Called before rendering a header/footer; idempotent for the same part
    /// name. Parts whose template has no rels file start with an empty set, and
    /// the rels part is created at the apply stage once relationships are
    /// added.
    pub(crate) fn begin_owner(&mut self, pkg: &Package, part_name: &str) -> Result<(), OpcError> {
        if let Some(index) = self.owners.iter().position(|owner| owner.name == part_name) {
            self.current = index;
            return Ok(());
        }
        let owner = load_owner(pkg, part_name)?;
        self.owners.push(owner);
        self.current = self.owners.len() - 1;
        Ok(())
    }

    /// Upstream `DocxTemplate.build_url_id`: register an external hyperlink
    /// relationship before rendering.
    ///
    /// Always operates on the **main-document rels** (upstream
    /// `docx._part.relate_to`), regardless of which part is currently being
    /// rendered.
    pub(crate) fn build_url_id(&mut self, url: &str) -> String {
        self.current = self.main_index;
        self.current_owner_mut()
            .get_or_add(HYPERLINK_REL_TYPE, url, TargetMode::External)
    }

    /// Register (or reuse) a relationship in the main-document relationship
    /// scope and return its rId (P6, ADR-007: Subdoc part merging).
    ///
    /// Mirrors upstream `docx._part.relate_to` / `rels.get_or_add_ext_rel`
    /// during compose: new relationships always land on the main-document
    /// part's rels and are deduplicated by (reltype, target, mode). Image
    /// relationships are invoked on its behalf by
    /// [`ImageInjections::add_subdoc_image`].
    pub(crate) fn main_get_or_add(
        &mut self,
        rel_type: &str,
        target: &str,
        mode: TargetMode,
    ) -> String {
        self.current = self.main_index;
        self.current_owner_mut().get_or_add(rel_type, target, mode)
    }

    /// Subdoc image part merge (P6, ADR-007): reuse an existing main-package
    /// or already-staged image part by sha1; on a miss create a new one named
    /// `word/media/imageN.ext` (staged, written to the package at finish) and
    /// return the absolute part name without registering an owner relationship.
    ///
    /// `SubdocComposer::copy_part` must also go through this entry when it
    /// recursively meets images inside footnotes/SmartArt. Otherwise copy_part
    /// writes to the package immediately while render images are staged until
    /// finish: the two snapshots would each allocate the same imageN and could
    /// not see each other's newly added sha1.
    pub(crate) fn add_subdoc_image_part(
        &mut self,
        bytes: &[u8],
        ext: &str,
        content_type: &str,
    ) -> String {
        let digest = sha1_digest(bytes);
        self.get_or_add_image_part(bytes, ext, content_type, digest)
    }

    /// Subdoc body/VML image merge: reuse the unified image-part allocator,
    /// register an IMAGE relationship in the main-document rels, and return its
    /// rId.
    ///
    /// Mirrors upstream `add_images`: a hit in
    /// `pkg.image_parts._get_by_sha1` reuses the partname directly (the rId
    /// still goes through main-rels deduplication, so an existing rel for the
    /// same image returns the original rId); on a miss create a new one via
    /// `ImageWrapper` (extension taken from the source part's filename suffix,
    /// content type from the source package declaration) plus
    /// `_add_image_part`. Numbering/CT staging shares the same state as
    /// render-time images, preventing cross-phase number collisions or
    /// duplicate Default pushes.
    pub(crate) fn add_subdoc_image(
        &mut self,
        bytes: &[u8],
        ext: &str,
        content_type: &str,
    ) -> String {
        let target_abs = self.add_subdoc_image_part(bytes, ext, content_type);
        let owner_name = self.owners[self.main_index].name.clone();
        let rel_target = relative_to_owner(&owner_name, &target_abs);
        self.main_get_or_add(IMAGE_REL_TYPE, &rel_target, TargetMode::Internal)
    }

    /// Unified package-level entry for image deduplication and partname/CT staging.
    fn get_or_add_image_part(
        &mut self,
        bytes: &[u8],
        ext: &str,
        content_type: &str,
        digest: ImageDigest,
    ) -> String {
        match self.by_sha1.get(&digest) {
            Some(existing) => existing.clone(),
            None => {
                let number = self.allocate_image_number();
                let name = format!("word/media/image{number}.{ext}");
                self.by_sha1.insert(digest, name.clone());
                self.pending_parts.push(PendingImagePart::Bytes {
                    name: name.clone(),
                    bytes: bytes.to_vec(),
                });
                self.pending_content_types.push((
                    name.clone(),
                    ext.to_ascii_lowercase(),
                    content_type.to_string(),
                ));
                name
            }
        }
    }

    fn get_or_add_inline_image_part(
        &mut self,
        image: &InlineImage,
        ext: &str,
        content_type: &str,
        digest: ImageDigest,
    ) -> String {
        match self.by_sha1.get(&digest) {
            Some(existing) => existing.clone(),
            None => {
                let number = self.allocate_image_number();
                let name = format!("word/media/image{number}.{ext}");
                self.by_sha1.insert(digest, name.clone());
                match image.lazy_file() {
                    Some(source) => self.pending_parts.push(PendingImagePart::File {
                        name: name.clone(),
                        source: source.clone(),
                        digest,
                    }),
                    None => self.pending_parts.push(PendingImagePart::Bytes {
                        name: name.clone(),
                        bytes: image.blob.clone(),
                    }),
                }
                self.pending_content_types.push((
                    name.clone(),
                    ext.to_ascii_lowercase(),
                    content_type.to_string(),
                ));
                name
            }
        }
    }

    /// Internal target part names of HEADER/FOOTER relationships in the
    /// main-document rels (insertion order, not deduplicated, matching the
    /// parts collection in upstream `renumber_docpr_ids` /
    /// `renumber_nvpicpr_ids`).
    ///
    /// Used by Subdoc to continue docPr/cNvPr numbering (P6, ADR-007).
    pub(crate) fn main_header_footer_parts(&self) -> Vec<String> {
        let main = &self.owners[self.main_index];
        let base = PartUri::new(&main.name).ok().and_then(|uri| uri.parent());
        let mut out = Vec::new();
        for rel in main.rels.iter() {
            if rel.target_mode == TargetMode::Internal
                && matches!(rel.rel_type.as_str(), HEADER_REL_TYPE | FOOTER_REL_TYPE)
            {
                if let Some(target) = resolve_part_target(base.as_ref(), &rel.target) {
                    out.push(target.as_str().to_string());
                }
            }
        }
        out
    }

    /// Allocate the next image number: fill holes starting at 1, otherwise the
    /// maximum + 1 (equivalent to upstream `range(1, len+1)` hole filling).
    fn allocate_image_number(&mut self) -> u64 {
        let number = self.next_free_image_number;
        self.used_numbers.insert(number);
        self.next_free_image_number = number.saturating_add(1);
        while self.used_numbers.contains(&self.next_free_image_number) {
            self.next_free_image_number = self.next_free_image_number.saturating_add(1);
        }
        number
    }

    /// Scope of the currently rendering part (mutable).
    fn current_owner_mut(&mut self) -> &mut OwnerState {
        &mut self.owners[self.current]
    }

    /// Write staged changes into the package: add media parts first (removing
    /// dangling relationships), then write rels back per scope (for new rels,
    /// mount via add_part before set_part_bytes), and finally rebuild
    /// [Content_Types].xml (so validate can resolve the new part types).
    pub(crate) fn apply(&mut self, pkg: &mut Package) -> Result<(), OpcError> {
        for pending in self.pending_parts.drain(..) {
            match pending {
                PendingImagePart::Bytes { name, bytes } => pkg.add_part(&name, bytes)?,
                PendingImagePart::File {
                    name,
                    source,
                    digest,
                } => pkg.add_file_backed_part(
                    &name,
                    source.path().to_path_buf(),
                    source.len(),
                    source.modified(),
                    digest,
                )?,
            }
        }
        for owner in &self.owners {
            if !owner.dirty {
                continue;
            }
            let rels_xml = owner.rels.to_xml();
            if !owner.rels_existed && !pkg.contains(&owner.rels_name) {
                // python-docx-equivalent behavior: part.rels materializes into a
                // /word/_rels/xxx.xml.rels part on relate_to (mount a
                // placeholder first; set_part_bytes then binds the parsed
                // relationships to the owner part).
                pkg.add_part(&owner.rels_name, rels_xml.clone().into_bytes())?;
            }
            pkg.set_part_bytes(&owner.rels_name, rels_xml.into_bytes())?;
        }
        if !self.pending_content_types.is_empty() {
            let mut content_types = pkg.content_types().clone();
            for (part_name, extension, content_type) in &self.pending_content_types {
                let existing_default = content_types
                    .defaults()
                    .find(|(known, _)| known.eq_ignore_ascii_case(extension))
                    .map(|(_, known_ct)| known_ct.to_string());
                match existing_default {
                    Some(known_ct) if known_ct == *content_type => {}
                    Some(_) => content_types.add_override(part_name, content_type),
                    None => content_types.add_default(extension, content_type),
                }
            }
            pkg.set_part_bytes("[Content_Types].xml", content_types.to_xml().into_bytes())?;
        }
        Ok(())
    }
}

impl OwnerState {
    /// Upstream `_Relationships.get_or_add` / `get_or_add_ext_rel`.
    fn get_or_add(&mut self, rel_type: &str, target: &str, mode: TargetMode) -> String {
        if let Some(id) = self
            .matching
            .get(&mode)
            .and_then(|types| types.get(rel_type))
            .and_then(|targets| targets.get(target))
        {
            return id.clone();
        }
        let id = format!("rId{}", self.next_free_rid);
        self.used_rids.insert(id.clone());
        self.next_free_rid = self.next_free_rid.saturating_add(1);
        while self
            .used_rids
            .contains(&format!("rId{}", self.next_free_rid))
        {
            self.next_free_rid = self.next_free_rid.saturating_add(1);
        }
        self.rels.push(Relationship {
            id: id.clone(),
            rel_type: rel_type.to_string(),
            target: target.to_string(),
            target_mode: mode,
        });
        self.matching
            .entry(mode)
            .or_default()
            .entry(rel_type.to_string())
            .or_default()
            .insert(target.to_string(), id.clone());
        self.dirty = true;
        id
    }
}

/// Load the relationship scope for a rendering part.
///
/// Clone the rels when the template already has a rels file (returns
/// rels_existed=true); otherwise start with an empty Relationships set
/// (rels_existed=false; a .rels part is created inside the package only when
/// rendering produces relationships).
fn load_owner(pkg: &Package, part_name: &str) -> Result<OwnerState, OpcError> {
    let uri = PartUri::new(part_name)?;
    let rels_name = relationships_path_of(&uri);
    let (rels, rels_existed) = match pkg.relationships_of(part_name) {
        Some(rels) => (rels.clone(), true),
        None => (
            Relationships::parse(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
            )?,
            false,
        ),
    };
    let mut matching = HashMap::<TargetMode, HashMap<String, HashMap<String, String>>>::new();
    let mut used_rids = HashSet::with_capacity(rels.len());
    for rel in rels.iter() {
        used_rids.insert(rel.id.clone());
        matching
            .entry(rel.target_mode)
            .or_default()
            .entry(rel.rel_type.clone())
            .or_default()
            .entry(rel.target.clone())
            .or_insert_with(|| rel.id.clone());
    }
    let next_free_rid = first_free_rid(&used_rids);
    Ok(OwnerState {
        name: part_name.to_string(),
        rels_name,
        rels,
        rels_existed,
        dirty: false,
        matching,
        used_rids,
        next_free_rid,
    })
}

fn first_free_rid(used: &HashSet<String>) -> u64 {
    let mut number = 1u64;
    while used.contains(&format!("rId{number}")) {
        number = number.saturating_add(1);
    }
    number
}

fn first_free_number(used: &HashSet<u64>) -> u64 {
    let mut number = 1u64;
    while used.contains(&number) {
        number = number.saturating_add(1);
    }
    number
}

impl ImageRegistry for ImageInjections {
    fn resolve_image(&mut self, image: &InlineImage) -> Result<ImageRels, ImageResolveError> {
        // 1. probe: image-header parsing inside upstream get_or_add_image; a
        //    bad image raises UnrecognizedImageError here (before any part/rId
        //    allocation).
        let (info, digest) = image.probe_with_digest().map_err(|err| ImageResolveError {
            message: err.to_string(),
        })?;

        Ok(self.resolve_probed_image(image, &info, digest))
    }

    fn resolve_image_with_info(
        &mut self,
        image: &InlineImage,
    ) -> Result<ResolvedImage, ImageResolveError> {
        let (info, digest) = image.probe_with_digest().map_err(|err| ImageResolveError {
            message: err.to_string(),
        })?;
        let rels = self.resolve_probed_image(image, &info, digest);
        Ok(ResolvedImage { rels, info })
    }

    fn resolve_preprobed_image(
        &mut self,
        image: &InlineImage,
        info: &docxtpl_rich::ImageInfo,
        digest: ImageDigest,
    ) -> Result<ResolvedImage, ImageResolveError> {
        let rels = self.resolve_probed_image(image, info, digest);
        Ok(ResolvedImage {
            rels,
            info: info.clone(),
        })
    }
}

impl ImageInjections {
    fn resolve_probed_image(
        &mut self,
        image: &InlineImage,
        info: &docxtpl_rich::ImageInfo,
        digest: ImageDigest,
    ) -> ImageRels {
        // Deduplicate by sha1 or allocate a new part name (numbers span extensions and fill holes).
        let target_abs =
            self.get_or_add_inline_image_part(image, info.ext, info.content_type, digest);

        // Rels of the currently rendering part: the internal image
        //    relationship (Target is relative to that part's directory; in P5
        //    headers/footers and the body each have their own rels, so rIds do
        //    not interfere).
        let owner_name = self.owners[self.current].name.clone();
        let rel_target = relative_to_owner(&owner_name, &target_abs);
        let blip_rid =
            self.current_owner_mut()
                .get_or_add(IMAGE_REL_TYPE, &rel_target, TargetMode::Internal);

        // Anchor external link: upstream allocates it after the image rId
        // (same scope). docxtpl 0.20.2's `if self.anchor:` treats an empty
        // string as no anchor; do not allocate a hyperlink relationship with
        // an empty Target for `Some("")`.
        let hyperlink_rid = image
            .anchor
            .as_deref()
            .filter(|url| !url.is_empty())
            .map(|url| {
                self.current_owner_mut()
                    .get_or_add(HYPERLINK_REL_TYPE, url, TargetMode::External)
            });

        ImageRels {
            blip_rid,
            hyperlink_rid,
        }
    }
}

/// Whole-package DFS collecting the absolute target part names of all
/// **internal** image relationships (mirrors `OpcPackage.iter_rels`: start at
/// the root rels, skip external ones, traverse each part's rels only once;
/// image relationships are deduplicated by occurrence, independent of traversal
/// order).
fn collect_image_parts(pkg: &Package) -> Vec<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut seen_images: HashSet<String> = HashSet::new();
    let mut images: Vec<String> = Vec::new();
    // Stack element: (directory of the owner part, rels). The base of the root rels is None.
    let mut stack: Vec<(Option<String>, Relationships)> =
        vec![(None, pkg.root_relationships().clone())];

    while let Some((base_dir, rels)) = stack.pop() {
        let base_uri = base_dir.as_deref().and_then(|dir| PartUri::new(dir).ok());
        for rel in rels.iter() {
            if rel.target_mode != TargetMode::Internal {
                continue;
            }
            let Some(target) = resolve_part_target(base_uri.as_ref(), &rel.target) else {
                continue;
            };
            let name = target.as_str().to_string();
            if rel.rel_type == IMAGE_REL_TYPE && seen_images.insert(name.clone()) {
                images.push(name.clone());
            }
            // Each part's rels is pushed only once (deduplicated via visited).
            if visited.insert(name.clone()) {
                if let Some(part) = pkg.part(&name) {
                    if let Some(child_rels) = part.relationships() {
                        stack.push((parent_dir(&name), child_rels.clone()));
                    }
                }
            }
        }
    }
    images
}

/// Directory of a part name (`word/document.xml` -> `word`; package root -> `None`).
fn parent_dir(name: &str) -> Option<String> {
    name.rsplit_once('/')
        .and_then(|(dir, _)| (!dir.is_empty()).then(|| dir.to_string()))
}

/// Compute the reference path of an absolute target part relative to the owner
/// part's directory (posix relpath: `word/document.xml` + `word/media/i.png`
/// -> `media/i.png`; emits `..` when it must ascend).
pub(crate) fn relative_to_owner(owner_part: &str, target_abs: &str) -> String {
    fn segments(name: &str) -> Vec<&str> {
        name.split('/')
            .filter(|segment| !segment.is_empty())
            .collect()
    }
    let mut base = segments(owner_part);
    base.pop(); // Drop the owner filename, leaving its directory segments
    let target = segments(target_abs);

    let mut common = 0;
    while common < base.len() && common < target.len() && base[common] == target[common] {
        common += 1;
    }
    let mut out: Vec<&str> = (0..base.len() - common).map(|_| "..").collect();
    out.extend(target[common..].iter().copied());
    out.join("/")
}

/// Upstream PackURI.idx regex `^([a-zA-Z]+)([1-9][0-9]*)?`: the numeric suffix
/// after the filename's alphabetic prefix (first digit cannot be 0), or None.
fn image_number(abs_name: &str) -> Option<u64> {
    let file_name = abs_name.rsplit('/').next().unwrap_or(abs_name);
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    let bytes = stem.as_bytes();
    let digit_at = bytes.iter().position(u8::is_ascii_digit)?;
    // The prefix must be 1+ pure letters.
    if digit_at == 0 || !bytes[..digit_at].iter().all(u8::is_ascii_alphabetic) {
        return None;
    }
    // The first digit of the numeric segment must be 1-9 ([1-9][0-9]*).
    if bytes[digit_at] == b'0' {
        return None;
    }
    let digit_end = bytes[digit_at..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map_or(bytes.len(), |offset| digit_at + offset);
    stem[digit_at..digit_end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/media/p4_dot2x1.png"
    );
    const IMAGE_TEMPLATE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/templates/p4_img_anchor.docx"
    );

    #[test]
    fn image_number_matches_packuri_idx_regex() {
        assert_eq!(image_number("word/media/image1.png"), Some(1));
        assert_eq!(image_number("word/media/image12.jpg"), Some(12));
        assert_eq!(image_number("media/image2.bmp"), Some(2));
        // Leading 0 does not match [1-9]
        assert_eq!(image_number("media/image01.png"), None);
        // No numeric suffix
        assert_eq!(image_number("media/image.png"), None);
        // Non-letter prefix
        assert_eq!(image_number("media/1image.png"), None);
        // re.match is not anchored at the end: a letter prefix followed by a digit is enough to extract the number
        assert_eq!(image_number("media/image3x.png"), Some(3));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_to_owner("word/document.xml", "word/media/image1.png"),
            "media/image1.png"
        );
        assert_eq!(
            relative_to_owner("word/document.xml", "word/media/sub/i.png"),
            "media/sub/i.png"
        );
        assert_eq!(
            relative_to_owner("word/sub/document.xml", "word/media/i.png"),
            "../media/i.png"
        );
        assert_eq!(
            relative_to_owner("document.xml", "media/i.png"),
            "media/i.png"
        );
    }

    #[test]
    fn parent_dirs() {
        assert_eq!(parent_dir("word/document.xml").as_deref(), Some("word"));
        assert_eq!(parent_dir("document.xml"), None);
        assert_eq!(
            parent_dir("word/media/i.png").as_deref(),
            Some("word/media")
        );
    }

    #[test]
    fn allocation_cursors_fill_holes_without_rescanning_from_one() {
        let used_rids = HashSet::from([
            "rId1".to_string(),
            "rId2".to_string(),
            "rId4".to_string(),
            "rId01".to_string(),
        ]);
        assert_eq!(first_free_rid(&used_rids), 3);

        let used_numbers = HashSet::from([1, 2, 4]);
        assert_eq!(first_free_number(&used_numbers), 3);
    }

    #[test]
    fn owner_index_reuses_first_match_and_allocates_unique_ids() -> Result<(), OpcError> {
        let pkg = Package::open(IMAGE_TEMPLATE_PATH, &docxtpl_opc::PackageLimits::default())?;
        let mut owner = load_owner(&pkg, "word/document.xml")?;
        let first = owner.get_or_add(
            "https://example.test/performance",
            "media/generated-0.png",
            TargetMode::Internal,
        );
        assert_eq!(
            owner.get_or_add(
                "https://example.test/performance",
                "media/generated-0.png",
                TargetMode::Internal,
            ),
            first
        );

        let mut ids = HashSet::new();
        ids.insert(first);
        for index in 1..=1_000 {
            let id = owner.get_or_add(
                "https://example.test/performance",
                &format!("media/generated-{index}.png"),
                TargetMode::Internal,
            );
            assert!(ids.insert(id));
        }
        assert_eq!(ids.len(), 1_001);
        Ok(())
    }

    #[test]
    fn empty_anchor_creates_no_hyperlink_relation_aligned_with_upstream_truthiness(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let pkg = Package::open(IMAGE_TEMPLATE_PATH, &docxtpl_opc::PackageLimits::default())?;
        let mut injections = ImageInjections::new(&pkg, "word/document.xml")?;
        let before = injections.owners[0].rels.len();
        let image = InlineImage::from_path(PNG_PATH, None, None, Some(String::new()))?;

        // Python 0.20.2 oracle: with InlineImage(..., anchor=""), the `if
        // self.anchor:` branch does not run, so the image relationship exists
        // but no hyperlink relationship does.
        let resolved = injections.resolve_image(&image)?;
        assert_eq!(resolved.hyperlink_rid, None);
        assert_eq!(injections.owners[0].rels.len(), before + 1);
        assert!(!injections.owners[0].rels.iter().any(|rel| {
            rel.rel_type == HYPERLINK_REL_TYPE
                && rel.target.is_empty()
                && rel.target_mode == TargetMode::External
        }));
        Ok(())
    }
}
