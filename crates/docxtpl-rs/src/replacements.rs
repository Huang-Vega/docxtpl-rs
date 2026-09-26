//! P7 media/embedded replacement family (ADR-008): mirrors the two independent
//! replacement paths on the docxtpl 0.20.2 save chain.
//!
//! **Path A: [`Replacements::apply_pic_replacements`] (pre_processing,
//! template.py L788-878)** -- `replace_pic`. Before python-docx's full save,
//! it scans the main document and every HEADER/FOOTER part referenced by the main rels for
//! `//a:graphic/a:graphicData[@uri = pic]`, matches registered identifiers by cNvPr
//! name/title/descr, and on a hit directly swaps the blob of the part targeted by r:embed
//! (it then lands on disk with the full save; part name, CT, rels and wp:extent stay unchanged).
//! Any registered identifier that never matches -> `ValueError` ([`TemplateErrorKind::InvalidArgument`]).
//!
//! **Path B: [`Replacements::apply_byte_replacements`] (post_processing,
//! template.py L749–786）**——`replace_media` / `replace_embedded` /
//! `replace_zipname`. During the final package write, entries get their bytes swapped by priority:
//! `exact zipname match > word/media/ prefix with CRC32 match >
//! word/embeddings/ prefix with CRC32 match`; no XML is parsed, and no
//! part names/Content Types/relationships are changed.
//!
//! Registration only stages bytes and does no I/O; it can be repeated within the same [`crate::RenderSession`], and
//! [`Replacements::reset`] mirrors upstream `reset_replacements`.

use std::collections::{BTreeMap, HashMap};

use docxtpl_opc::{resolve_part_target, Package, PartUri, TargetMode};
use docxtpl_template::{RenderError, TemplateErrorKind};
use docxtpl_xml::{ns_uri, NodeId, XmlDocument, XmlLimits};

use crate::{decode_xml_bytes, Error};

/// Image relationship type (r:embed target).
const RT_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
/// Header relationship type (upstream compares `rel.reltype == REL_TYPE.HEADER`).
const RT_HEADER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
/// Footer relationship type.
const RT_FOOTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

/// Media prefix for zip entries (zipname without a leading `/`).
const MEDIA_PREFIX: &str = "word/media/";
/// Embeddings prefix for zip entries.
const EMBEDDINGS_PREFIX: &str = "word/embeddings/";

/// Read-only equivalent of upstream `get_pic_map()`: scans the body and the
/// header/footer reachable from the main-document relationships, mapping cNvPr name to the owner relationship's relative target.
pub(crate) fn picture_map(
    pkg: &Package,
    main_name: &str,
) -> Result<BTreeMap<String, String>, Error> {
    let mut result = BTreeMap::new();
    let mut owners = vec![main_name.to_string()];
    owners.extend(header_footer_targets(pkg, main_name));
    for owner in owners {
        picture_map_in_part(pkg, &owner, &mut result)?;
    }
    Ok(result)
}

fn picture_map_in_part(
    pkg: &Package,
    owner: &str,
    result: &mut BTreeMap<String, String>,
) -> Result<(), Error> {
    let Some(part) = pkg.part(owner) else {
        return Ok(());
    };
    let xml = decode_xml_bytes(part.bytes()?, owner)?;
    let doc = XmlDocument::parse_strict(&xml, &XmlLimits::default()).map_err(|source| {
        Error::Render(RenderError::Xml {
            part: owner.to_string(),
            source,
        })
    })?;
    let Some(rels) = pkg.relationships_of(owner) else {
        return Ok(());
    };
    for graphic in tag_descendants(&doc, doc.root(), ns_uri::A, "graphic") {
        let Some(gd) = direct_child(&doc, graphic, ns_uri::A, "graphicData") else {
            continue;
        };
        let Some((name, rid)) = picture_name_and_rid(&doc, gd) else {
            continue;
        };
        let Some(rel) = rels.iter().find(|rel| {
            rel.id == rid && rel.target_mode == TargetMode::Internal && rel.rel_type == RT_IMAGE
        }) else {
            continue;
        };
        result.insert(name.to_string(), rel.target.clone());
    }
    Ok(())
}

fn picture_name_and_rid(doc: &XmlDocument, gd: NodeId) -> Option<(&str, &str)> {
    if doc.attr(gd, "", "uri") != Some(ns_uri::PIC) {
        return None;
    }
    let pic = direct_child(doc, gd, ns_uri::PIC, "pic")?;
    let blip_fill = direct_child(doc, pic, ns_uri::PIC, "blipFill")?;
    let blip = direct_child(doc, blip_fill, ns_uri::A, "blip")?;
    let rid = doc.attr(blip, ns_uri::R, "embed")?;
    let nvpicpr = direct_child(doc, pic, ns_uri::PIC, "nvPicPr")?;
    let cnvpr = direct_child(doc, nvpicpr, ns_uri::PIC, "cNvPr")?;
    Some((doc.attr(cnvpr, "", "name")?, rid))
}

/// A single replace_pic registration: new bytes plus whether the scan matched it.
struct PicReplacement {
    /// Replacement image bytes (mirrors upstream `pics_to_replace[img_id]`).
    bytes: Vec<u8>,
    /// Hit marker corresponding to replaced_pics in `_replace_pics`.
    hit: bool,
}

/// Registry for the P7 replacement family (session-level, held by [`crate::RenderSession`]).
pub(crate) struct Replacements {
    /// CRC32(source bytes) -> new bytes (`word/media/` entries matched by zip CRC).
    media: HashMap<u32, Vec<u8>>,
    /// CRC32(source bytes) -> new bytes (`word/embeddings/` entries).
    embedded: HashMap<u32, Vec<u8>>,
    /// Full zip entry name (e.g. `word/embeddings/x.bin`) -> new bytes.
    zipnames: HashMap<String, Vec<u8>>,
    /// Picture identifier (cNvPr name/title/descr) -> replacement entry.
    ///
    /// Order-preserving Vec rather than a map: upstream `pics_to_replace` is a dict, and each picture's
    /// graphicData finds the first match in **registration insertion order** and `break`s (an earlier-registered key
    /// shadows later-registered keys for the same picture); error ordering also follows insertion order.
    pics: Vec<(String, PicReplacement)>,
    /// Mirrors upstream `DocxTemplate.allow_missing_pics`: when true, registered
    /// picture identifiers are allowed to miss in the template. Defaults to false, keeping the upstream default error behavior.
    allow_missing_pics: bool,
}

impl Replacements {
    pub(crate) fn new() -> Self {
        Self {
            media: HashMap::new(),
            embedded: HashMap::new(),
            zipnames: HashMap::new(),
            pics: Vec::new(),
            allow_missing_pics: false,
        }
    }

    /// Upstream `replace_media(src_file, dst_file)`: registers a media replacement by source-byte CRC32
    /// (both source and destination are bytes already read into memory).
    pub(crate) fn replace_media(&mut self, src: &[u8], dst: &[u8]) {
        self.media.insert(crc32fast::hash(src), dst.to_vec());
    }

    /// Upstream `replace_embedded(src_file, dst_file)`: registers the
    /// embeddings replacement by source-byte CRC32.
    pub(crate) fn replace_embedded(&mut self, src: &[u8], dst: &[u8]) {
        self.embedded.insert(crc32fast::hash(src), dst.to_vec());
    }

    /// Upstream `replace_zipname(zipname, dst_file)`: registers exactly by the full zip entry name.
    ///
    /// `zipname` is the in-package entry name (without a leading `/`, e.g.
    /// `word/embeddings/Feuille1.xlsx`）。
    pub(crate) fn replace_zipname(&mut self, zipname: &str, dst: &[u8]) {
        self.zipnames.insert(zipname.to_string(), dst.to_vec());
    }

    /// Upstream `replace_pic(embedded_file, dst_file)`: registers a picture replacement by cNvPr
    /// name/title/descr. Re-registering the same identifier follows dict semantics and
    /// overwrites the bytes (keeping the first registration position).
    pub(crate) fn replace_pic(&mut self, pic_id: &str, dst: &[u8]) {
        if let Some(slot) = self.pics.iter_mut().find(|(id, _)| id == pic_id) {
            slot.1.bytes = dst.to_vec();
            return;
        }
        self.pics.push((
            pic_id.to_string(),
            PicReplacement {
                bytes: dst.to_vec(),
                hit: false,
            },
        ));
    }

    /// Sets whether `replace_pic` registrations are allowed to miss in every scanned part.
    pub(crate) fn set_allow_missing_pics(&mut self, allow: bool) {
        self.allow_missing_pics = allow;
    }

    /// Upstream `reset_replacements`: clears all four kinds of replacement registrations.
    ///
    /// `allow_missing_pics` is a session policy rather than a replacement registration, so its current value is kept.
    pub(crate) fn reset(&mut self) {
        self.media.clear();
        self.embedded.clear();
        self.zipnames.clear();
        self.pics.clear();
    }

    /// Path A: replace picture part blobs in the main document and every HEADER/FOOTER part.
    ///
    /// Must be called **after** all rendering/image injections have landed and before Content Types normalization
    /// (mirrors the upstream save: pre_processing runs before docx.save; what is scanned here is the
    /// final rendered XML).
    pub(crate) fn apply_pic_replacements(
        &mut self,
        pkg: &mut Package,
        main_name: &str,
    ) -> Result<(), Error> {
        if self.pics.is_empty() {
            return Ok(());
        }

        // Main document (upstream `part = self.docx.part`).
        self.replace_pics_in_part(pkg, main_name)?;

        // Header/Footer: two passes over the main rels (headers before footers), in rels
        // occurrence order, without deduplication (rescanning the same part is idempotent; upstream does not deduplicate either).
        for part_name in header_footer_targets(pkg, main_name) {
            self.replace_pics_in_part(pkg, &part_name)?;
        }

        // Default mirrors upstream allow_missing_pics=False: any unmatched identifier raises an error
        // (in registration insertion order, matching upstream dict iteration). When lenient mode is explicitly enabled,
        // matched entries are still replaced normally; only the final missing check is skipped.
        if !self.allow_missing_pics {
            for (img_id, pic) in &self.pics {
                if !pic.hit {
                    return Err(Error::Render(RenderError::Template {
                        kind: TemplateErrorKind::InvalidArgument,
                        part: main_name.to_string(),
                        line: None,
                        message: format!("Picture {img_id} not found in the docx template"),
                        context: Vec::new(),
                    }));
                }
            }
        }
        Ok(())
    }

    /// Path B: traverse the final package parts and swap bytes by the priority
    /// zipname/media CRC/embeddings CRC. Should run after Content Types normalization and before final validation.
    pub(crate) fn apply_byte_replacements(&mut self, pkg: &mut Package) -> Result<(), Error> {
        if self.media.is_empty() && self.embedded.is_empty() && self.zipnames.is_empty() {
            return Ok(());
        }

        // Separate iteration from write-back (pkg.parts() holds an immutable borrow).
        let mut hits: Vec<(String, Vec<u8>)> = Vec::new();
        for part in pkg.parts() {
            let abs_name = part.uri().as_str();
            let zipname = abs_name.strip_prefix('/').unwrap_or(abs_name);
            if let Some(bytes) = self.zipnames.get(zipname) {
                hits.push((abs_name.to_string(), bytes.clone()));
            } else if zipname.starts_with(MEDIA_PREFIX) {
                let crc = crc32fast::hash(part.bytes()?);
                if let Some(bytes) = self.media.get(&crc) {
                    hits.push((abs_name.to_string(), bytes.clone()));
                }
            } else if zipname.starts_with(EMBEDDINGS_PREFIX) {
                let crc = crc32fast::hash(part.bytes()?);
                if let Some(bytes) = self.embedded.get(&crc) {
                    hits.push((abs_name.to_string(), bytes.clone()));
                }
            }
        }

        for (name, bytes) in hits {
            pkg.set_part_bytes(&name, bytes)?;
        }
        Ok(())
    }

    /// Mirrors upstream `_replace_docx_part_pics`: scans a single document/story part's
    /// pic graphicData and replaces the blob of the part targeted by its r:embed.
    fn replace_pics_in_part(&mut self, pkg: &mut Package, owner: &str) -> Result<(), Error> {
        let Some(part) = pkg.part(owner) else {
            return Ok(());
        };
        // Rendered output is UTF-8; the skip-render path also accepts valid UTF-16 XmlParts.
        let xml = decode_xml_bytes(part.bytes()?, owner)?;
        let rels = pkg.relationships_of(owner).cloned();
        let base_dir = PartUri::new(owner).ok().and_then(|uri| uri.parent());

        // Upstream etree.fromstring(doc_part.blob): strict parsing; this blob comes from
        // our own render/serialize pipeline, so well-formedness is an invariant and failures are reported as XML errors.
        let doc = XmlDocument::parse_strict(&xml, &XmlLimits::default()).map_err(|source| {
            Error::Render(RenderError::Xml {
                part: owner.to_string(),
                source,
            })
        })?;

        // xpath //a:graphic/a:graphicData: direct a:graphicData children of descendant a:graphic nodes.
        let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
        for graphic in tag_descendants(&doc, doc.root(), ns_uri::A, "graphic") {
            let Some(gd) = direct_child(&doc, graphic, ns_uri::A, "graphicData") else {
                continue;
            };
            process_graphic_data(
                &doc,
                gd,
                rels.as_ref(),
                base_dir.as_ref(),
                &mut self.pics,
                &mut pending,
            );
        }

        // Swap the blob of the matched media part (multiple blip references to the same part write only once,
        // and rewriting the same bytes is idempotent).
        for (target, bytes) in pending {
            if pkg.contains(&target) {
                pkg.set_part_bytes(&target, bytes)?;
            }
        }
        Ok(())
    }
}

/// Handle a single a:graphicData (mirrors the upstream per-gd try/except: any missing structure
/// (no uri attribute, no blip, no cNvPr@name, dangling r:id, etc.) skips the whole element).
#[allow(clippy::too_many_arguments)]
fn process_graphic_data(
    doc: &XmlDocument,
    gd: NodeId,
    rels: Option<&docxtpl_opc::Relationships>,
    base_dir: Option<&PartUri>,
    pics: &mut [(String, PicReplacement)],
    pending: &mut Vec<(String, Vec<u8>)>,
) {
    // Only graphicData in pic:pic form (others are chart/SmartArt etc.; continue).
    if doc.attr(gd, "", "uri") != Some(ns_uri::PIC) {
        return;
    }

    // pic:pic/pic:blipFill/a:blip: no blip (xpath[0] out of bounds) -> skip.
    let Some(pic) = direct_child(doc, gd, ns_uri::PIC, "pic") else {
        return;
    };
    let Some(blip_fill) = direct_child(doc, pic, ns_uri::PIC, "blipFill") else {
        return;
    };
    let Some(blip) = direct_child(doc, blip_fill, ns_uri::A, "blip") else {
        return;
    };
    // r:link without r:embed (LINKED_PICTURE) -> upstream continue.
    let Some(embed_rid) = doc.attr(blip, ns_uri::R, "embed") else {
        return;
    };

    // name/title/descr of pic:pic/pic:nvPicPr/pic:cNvPr (when name is missing,
    // the upstream xpath[0] out-of-bounds is swallowed).
    let Some(nvpicpr) = direct_child(doc, pic, ns_uri::PIC, "nvPicPr") else {
        return;
    };
    let Some(cnvpr) = direct_child(doc, nvpicpr, ns_uri::PIC, "cNvPr") else {
        return;
    };
    let Some(filename) = doc.attr(cnvpr, "", "name") else {
        return;
    };
    let title = doc.attr(cnvpr, "", "title").unwrap_or("");
    let description = doc.attr(cnvpr, "", "descr").unwrap_or("");

    // doc_part.rels[r:embed]: internal relationships only, with a resolvable target and existing part;
    // external/dangling r:ids raise upstream and are swallowed.
    let Some(rels) = rels else {
        return;
    };
    let Some(rel) = rels.iter().find(|rel| {
        rel.id == embed_rid && rel.target_mode == TargetMode::Internal && rel.rel_type == RT_IMAGE
    }) else {
        return;
    };
    let Some(target_uri) = resolve_part_target(base_dir, &rel.target) else {
        return;
    };
    let target = target_uri.as_str().to_string();

    // An identifier equal to any of name/title/descr is a hit; compare in pics registration insertion order,
    // and break after a hit (each gd is swapped only once).
    for (img_id, pic_repl) in pics.iter_mut() {
        if img_id == filename || img_id == title || img_id == description {
            pending.push((target.clone(), pic_repl.bytes.clone()));
            pic_repl.hit = true;
            break;
        }
    }
}

/// Internal HEADER/FOOTER target part names in the main-document rels: headers before footers,
/// two passes, in rels occurrence order, without deduplication (mirrors the parts collection in `_replace_pics`).
fn header_footer_targets(pkg: &Package, main_name: &str) -> Vec<String> {
    let Ok(main_uri) = PartUri::new(main_name) else {
        return Vec::new();
    };
    let base_dir = main_uri.parent();
    let Some(rels) = pkg.relationships_of(main_name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for rel_type in [RT_HEADER, RT_FOOTER] {
        for rel in rels.iter() {
            if rel.target_mode != TargetMode::Internal || rel.rel_type != rel_type {
                continue;
            }
            if let Some(target) = resolve_part_target(base_dir.as_ref(), &rel.target) {
                let name = target.as_str();
                // python-docx rels match the package parts graph; a missing target means a malformed package, so skip.
                if pkg.part(name).is_some() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// Whether a node is an element with the given qualified name.
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

/// The first node with the given qualified name among the element's direct children.
fn direct_child(doc: &XmlDocument, parent: NodeId, ns: &str, local: &str) -> Option<NodeId> {
    doc.children(parent)
        .iter()
        .copied()
        .find(|&id| is_tag(doc, id, ns, local))
}
