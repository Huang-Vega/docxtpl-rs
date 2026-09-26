//! docxtpl-rs: a Rust implementation that treats docx files as Jinja2 templates.
//!
//! Public facade: [`DocxTemplate::open`] -> [`DocxTemplate::render`] /
//! [`DocxTemplate::render_ctx`] -> [`RenderedDocument::save`]. Semantics track the fixed
//! baseline Python docxtpl 0.20.2 (see docs/compatibility.md and ADR-001/005).
//!
//! Plain JSON context:
//!
//! ```no_run
//! use docxtpl_rs::{DocxTemplate, RenderOptions};
//! use serde_json::json;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let tpl = DocxTemplate::open("template.docx")?;
//! let doc = tpl.render(
//!     &json!({"name": "Vega", "items": [{"name": "Apple"}]}),
//!     &RenderOptions::compat(),
//! )?;
//! doc.save("output.docx")?;
//! # Ok(()) }
//! ```
//!
//! Rich content (RichText/Listing/InlineImage) context:
//!
//! ```no_run
//! use docxtpl_rs::{DocxTemplate, InlineImage, RenderContext, RenderOptions};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let tpl = DocxTemplate::open("template.docx")?;
//! let mut ctx = RenderContext::new();
//! ctx.insert("img", InlineImage::from_path("photo.png", None, None, None)?);
//! let doc = tpl.render_ctx(&ctx, &RenderOptions::compat())?;
//! doc.save("output.docx")?;
//! # Ok(()) }
//! ```
//!
//! When an external hyperlink rId is needed before rendering (upstream
//! `tpl.build_url_id(url)`), use [`DocxTemplate::render_session`] to open a one-shot session:
//!
//! ```no_run
//! use docxtpl_rs::{DocxTemplate, RenderContext, RenderOptions, RichText, RichTextProps};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let tpl = DocxTemplate::open("template.docx")?;
//! let mut session = tpl.render_session(&RenderOptions::compat())?;
//! let url_id = session.build_url_id("https://example.com/");
//! let mut props = RichTextProps::new();
//! props.url_id = Some(url_id);
//! let mut ctx = RenderContext::new();
//! ctx.insert("rt", RichText::text_with("link", &props));
//! let doc = session.finish(&ctx)?;
//! doc.save("output.docx")?;
//! # Ok(()) }
//! ```
//!
//! [`DocxTemplate`] is read-only and reusable: the file entry reopens the source path on every render, and the bytes/reader
//! entries reopen the package from the retained original bytes; transient render state never leaks across renders.

use std::collections::BTreeSet;
use std::io::{Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};

pub use docxtpl_opc::PackageLimits;
use docxtpl_opc::{resolve_part_target, OpcError, Package, PartUri, Relationship, TargetMode};
use docxtpl_template::{
    find_undeclared_variables, normalize_part_xml, render_core_properties_ctx_with_options,
    render_document_xml_ctx, render_footnotes_xml_ctx, render_story_xml_ctx, RenderError,
};
// Serves both as internal types and public re-exports (the pub use list at the bottom does not repeat them).
pub use docxtpl_template::{JsonContextError, RenderContext};

mod images;
mod replacements;
mod subdoc;

use images::ImageInjections;
use replacements::{picture_map, Replacements};

const DEFAULT_DOCUMENT_BYTES: u64 = 600 * 1024 * 1024;

/// Content type of the footnotes part (upstream render_footnotes filters
/// package.parts by it; this CT is not registered in the python-docx PartFactory, so it is handled as a generic
/// binary part whose rendered output is written back as-is without re-serialization, ADR-006).
const CT_FOOTNOTES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";
/// Content type of the endnotes part. Endnotes use the same generic-part
/// string rendering path as footnotes.
const CT_ENDNOTES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml";

/// Transitional OOXML story relationship types used by python-docx/docxtpl.
/// Match the complete URI: a custom relationship whose type merely ends in
/// `/header` or `/footer` is not a Word story relationship.
const REL_TYPE_HEADER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const REL_TYPE_FOOTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

/// Part content types registered in the python-docx PartFactory as known XmlPart subclasses and **always re-serialized via the lxml tree**
/// on save (P7b B1). Even when render is not run and only
/// save is called, document/header/footer/core/comments/styles/settings/numbering still undergo
/// tree-form normalization (real Word templates typically use double-quoted declarations plus CRLF in Word form).
/// Unregistered generic parts such as fontTable/webSettings/theme/footnotes/endnotes/customXml
/// continue to pass through as blobs (see compatibility.md for the DEV classification).
const PYTHON_DOCX_XML_CTS: &[&str] = &[
    "application/vnd.openxmlformats-package.core-properties+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
];

/// A normal render already rewrites document/header/footer/core through their respective upstream-compatible pipelines;
/// strip-blank-text must not run again at save time, otherwise it would remove observable whitespace inside InlineImage XML kept by
/// python-docx templates. comments/styles/settings/numbering do not go through
/// the render pipeline and must still be tree-serialized unconditionally like a python-docx save.
const XML_CTS_TO_NORMALIZE_AFTER_RENDER: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
];

/// Limits for document reading, OPC and rendering resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    max_input_docx_bytes: u64,
    package: PackageLimits,
    max_rendered_xml_bytes: usize,
    template_fuel: u64,
}

impl ResourceLimits {
    /// Default large-document profile: a 600 MiB byte budget for document-related data, retaining the compression-ratio and fuel guards.
    #[must_use]
    pub fn large_document() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_max_input_docx_bytes(mut self, max: u64) -> Self {
        self.max_input_docx_bytes = max;
        self
    }

    #[must_use]
    pub fn with_package_limits(mut self, limits: PackageLimits) -> Self {
        self.package = limits;
        self
    }

    #[must_use]
    pub fn with_max_rendered_xml_bytes(mut self, max: usize) -> Self {
        self.max_rendered_xml_bytes = max;
        self
    }

    #[must_use]
    pub fn with_template_fuel(mut self, fuel: u64) -> Self {
        self.template_fuel = fuel;
        self
    }

    #[must_use]
    pub fn max_input_docx_bytes(&self) -> u64 {
        self.max_input_docx_bytes
    }

    #[must_use]
    pub fn package_limits(&self) -> &PackageLimits {
        &self.package
    }

    #[must_use]
    pub fn max_rendered_xml_bytes(&self) -> usize {
        self.max_rendered_xml_bytes
    }

    #[must_use]
    pub fn template_fuel(&self) -> u64 {
        self.template_fuel
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_input_docx_bytes: DEFAULT_DOCUMENT_BYTES,
            package: PackageLimits::default(),
            max_rendered_xml_bytes: DEFAULT_DOCUMENT_BYTES as usize,
            template_fuel: 10_000_000,
        }
    }
}

#[derive(Debug)]
enum TemplateSource {
    /// File templates reopen on demand, avoiding a permanently resident copy of the full compressed DOCX bytes.
    Path(PathBuf),
    /// The reader/bytes entries have no stable source to reopen from, so the input bytes are retained.
    Bytes(Vec<u8>),
}

/// A reusable docx template.
#[derive(Debug)]
pub struct DocxTemplate {
    source: TemplateSource,
    limits: ResourceLimits,
}

impl DocxTemplate {
    /// Open a template from a file (reads all bytes and performs basic readability validation, without rendering).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::open_with_limits(path, ResourceLimits::default())
    }

    /// Open a file template with explicit resource limits. The file entry only stores the path; at render time the OPC package is built directly from the file,
    /// without keeping an extra resident copy of the compressed DOCX bytes.
    pub fn open_with_limits(path: impl AsRef<Path>, limits: ResourceLimits) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let length = std::fs::metadata(&path)?.len();
        if length > limits.max_input_docx_bytes {
            return Err(Error::InputTooLarge {
                max: limits.max_input_docx_bytes,
            });
        }
        let pkg = Package::open(&path, &limits.package)?;
        pkg.validate()?;
        Ok(Self {
            source: TemplateSource::Path(path),
            limits,
        })
    }

    /// Read a template from an arbitrary reader.
    pub fn from_reader(reader: impl Read) -> Result<Self, Error> {
        Self::from_reader_with_limits(reader, ResourceLimits::default())
    }

    pub fn from_reader_with_limits(
        reader: impl Read,
        limits: ResourceLimits,
    ) -> Result<Self, Error> {
        let mut data = Vec::new();
        reader
            .take(limits.max_input_docx_bytes.saturating_add(1))
            .read_to_end(&mut data)?;
        Self::from_bytes_with_limits(data, limits)
    }

    /// Construct a template from existing bytes; a full parse and validation against the OPC limits runs immediately,
    /// ensuring later renders cannot fail due to package-structure problems.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, Error> {
        Self::from_bytes_with_limits(data, ResourceLimits::default())
    }

    pub fn from_bytes_with_limits(data: Vec<u8>, limits: ResourceLimits) -> Result<Self, Error> {
        if data.len() as u64 > limits.max_input_docx_bytes {
            return Err(Error::InputTooLarge {
                max: limits.max_input_docx_bytes,
            });
        }
        let pkg = Package::from_reader(Cursor::new(&data), &limits.package)?;
        pkg.validate()?;
        Ok(Self {
            source: TemplateSource::Bytes(data),
            limits,
        })
    }

    /// Render with a plain JSON context, returning an independent [`RenderedDocument`].
    ///
    /// Render every part in the fixed upstream order (P5, ADR-006): body -> headers -> footers ->
    /// core properties -> footnotes; unchanged parts are preserved as-is. Failures return a [`RenderError`] carrying part and line-number
    /// context.
    pub fn render(
        &self,
        context: &serde_json::Value,
        options: &RenderOptions,
    ) -> Result<RenderedDocument, Error> {
        let options = self.effective_render_options(options);
        // Open a fresh package per render: DocxTemplate is reusable and state does not leak across renders (spec §2.2).
        let mut pkg = self.open_package()?;
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let context =
            RenderContext::try_from_json(context).map_err(|source| RenderError::Template {
                kind: docxtpl_template::TemplateErrorKind::InvalidArgument,
                part: main_name.clone(),
                line: None,
                message: source.to_string(),
                context: Vec::new(),
            })?;
        let mut injections = ImageInjections::new(&pkg, &main_name)?;

        render_all_parts(&mut pkg, &context, &options, &mut injections)?;
        // A JSON context contains no images/external links, so apply is effectively a no-op (no dirty scopes).
        injections.apply(&mut pkg)?;
        canonicalize_content_types(&mut pkg, false)?;

        // Validate the package once more before writing out: no dangling relationships or missing parts allowed.
        pkg.validate()?;

        Ok(RenderedDocument { pkg })
    }

    /// Render with a rich-content context (P4, ADR-005): the context may contain RichText /
    /// RichTextParagraph / Listing / InlineImage。
    ///
    /// Image media parts, document rels and `[Content_Types].xml` changes are committed once after
    /// rendering finishes; when no images/external links are involved, these parts keep their original bytes.
    /// When external hyperlinks must be pre-registered before rendering (upstream `tpl.build_url_id`), use instead
    /// [`DocxTemplate::render_session`]。
    pub fn render_ctx(
        &self,
        context: &RenderContext,
        options: &RenderOptions,
    ) -> Result<RenderedDocument, Error> {
        self.render_session(options)?.finish(context)
    }

    /// Open a one-shot rich-content render session: inside the session one may first call [`RenderSession::build_url_id`]
    /// to pre-register external hyperlink relationships (mirroring the upstream `tpl.build_url_id` call before rendering),
    /// then call [`RenderSession::finish`] to complete the render.
    pub fn render_session(&self, options: &RenderOptions) -> Result<RenderSession, Error> {
        let pkg = self.open_package()?;
        let options = self.effective_render_options(options);
        RenderSession::new(pkg, &options)
    }

    /// Upstream `DocxTemplate.get_undeclared_template_variables()` (P7):
    /// After applying patch_xml to the template body and every non-empty header/footer referenced by the main rels, performs
    /// jinja meta-analysis and returns the set of variable names referenced by the template but not declared via `{% for %}`/macro parameters etc.
    /// (returned sorted for stable presentation).
    ///
    /// Pure static introspection: it does not modify the template or produce an output document. This method is equivalent to upstream
    /// `context=None`; to exclude keys already present in the context, use
    /// [`DocxTemplate::undeclared_variables_with_context`] or
    /// [`DocxTemplate::undeclared_variables_with_keys`]。
    pub fn undeclared_variables(&self) -> Result<BTreeSet<String>, Error> {
        let pkg = self.open_package()?;
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let doc_xml = read_xml_part(&pkg, &main_name)?;
        // Reuse the render story enumeration: headers before footers, internal targets, non-empty blobs.
        // Upstream rels traversal is not deduplicated, but deduplicating while taking the union does not change the result.
        let story_xmls = story_parts(&pkg, &main_name)?
            .iter()
            .map(|name| read_xml_part(&pkg, name))
            .collect::<Result<Vec<_>, _>>()?;
        find_undeclared_variables(&doc_xml, &story_xmls).map_err(Error::Render)
    }

    /// Typed-context entry point for upstream `get_undeclared_template_variables(context=...)`.
    ///
    /// Performs the same static analysis as [`DocxTemplate::undeclared_variables`], then removes
    /// all top-level keys of `context` from the result; context values are neither evaluated nor checked.
    pub fn undeclared_variables_with_context(
        &self,
        context: &RenderContext,
    ) -> Result<BTreeSet<String>, Error> {
        self.undeclared_variables_with_keys(context.iter().map(|(key, _)| key))
    }

    /// Keys-only entry point for upstream `get_undeclared_template_variables(context=...)`.
    ///
    /// For callers that already have a JSON/object key set and do not need to build a [`RenderContext`];
    /// keys not present in the template are ignored, and duplicate keys do not affect the result.
    pub fn undeclared_variables_with_keys<I, K>(
        &self,
        context_keys: I,
    ) -> Result<BTreeSet<String>, Error>
    where
        I: IntoIterator<Item = K>,
        K: AsRef<str>,
    {
        let mut variables = self.undeclared_variables()?;
        for key in context_keys {
            variables.remove(key.as_ref());
        }
        Ok(variables)
    }

    /// Read-only equivalent of upstream `DocxTemplate.get_pic_map()`.
    ///
    /// Returns an ordered map from the image `cNvPr@name` to the image relationship
    /// relative target (e.g. `image.png -> media/image1.png`) for images in the template body and its headers/footers.
    /// Pathological image structures, external images and dangling relationships are skipped just like the upstream scan.
    pub fn picture_map(&self) -> Result<std::collections::BTreeMap<String, String>, Error> {
        let pkg = self.open_package()?;
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        picture_map(&pkg, &main_name)
    }

    /// Opens and validates a fresh package per render.
    fn open_package(&self) -> Result<Package, Error> {
        let pkg = match &self.source {
            TemplateSource::Path(path) => {
                if std::fs::metadata(path)?.len() > self.limits.max_input_docx_bytes {
                    return Err(Error::InputTooLarge {
                        max: self.limits.max_input_docx_bytes,
                    });
                }
                Package::open(path, &self.limits.package)?
            }
            TemplateSource::Bytes(data) => {
                Package::from_reader(Cursor::new(data), &self.limits.package)?
            }
        };
        pkg.validate()?;
        Ok(pkg)
    }

    fn effective_render_options(&self, options: &RenderOptions) -> RenderOptions {
        options
            .clone()
            .with_max_rendered_xml_bytes(self.limits.max_rendered_xml_bytes)
            .with_template_fuel(self.limits.template_fuel)
    }
}

/// A one-shot rich-content render session (ADR-005): holds the opened package and the image/relationship injection state.
///
/// Mirrors the mutability of an upstream `DocxTemplate` instance within a single render:
/// the external-link entries pre-registered by `build_url_id` and the images resolved during rendering share the same document
/// rels, with rIds filling holes/appended in call order. The session is consumed once ([`RenderSession::finish`]
/// takes ownership) and is not reused across renders.
pub struct RenderSession {
    pkg: Package,
    options: RenderOptions,
    injections: ImageInjections,
    /// P7 media/embedded replacement registry (the replace_* family, committed at finish).
    replacements: Replacements,
}

impl RenderSession {
    /// Locates the main document and builds the image injection registry (raises an OPC error when the main-document rels is missing).
    fn new(pkg: Package, options: &RenderOptions) -> Result<Self, Error> {
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let injections = ImageInjections::new(&pkg, &main_name)?;
        Ok(Self {
            pkg,
            options: options.clone(),
            injections,
            replacements: Replacements::new(),
        })
    }

    /// Upstream `DocxTemplate.build_url_id`: register (or reuse) an external hyperlink relationship and
    /// return its rId. The same URL (reltype+External) idempotently reuses the existing relationship.
    ///
    /// Must be called before [`RenderSession::finish`] and before constructing the
    /// [`RichText`] that references this rId.
    #[must_use]
    pub fn build_url_id(&mut self, url: &str) -> String {
        self.injections.build_url_id(url)
    }

    /// Upstream `DocxTemplate.new_subdoc(docpath)` (P6, ADR-007): opens an external
    /// docx and merges its parts into the current main package, returning a Subdoc value that can be inserted directly into the context
    /// (template syntax `{{p sd }}`).
    ///
    /// Merged content: referenced-part copying (rels/Content Types), three-branch style merging,
    /// numbering copying, image media deduplication/merging, and main-document bookmark/docPr/cNvPr renumbering.
    /// Tree-part changes land in the package immediately (unchanged parts keep their original bytes); image and main-rels changes are committed
    /// together with the rendered result at [`RenderSession::finish`].
    ///
    /// Must be called before [`RenderSession::finish`]; it may be called multiple times
    /// within a render session (mirroring the idempotence of upstream construction-time merging).
    ///
    /// The parameterless borrowed-Subdoc mode is not part of the public API (DEV-0008); callers must
    /// supply an external DOCX path through this method, or use
    /// [`RenderSession::new_subdoc_from_reader`] /
    /// [`RenderSession::new_subdoc_from_bytes`]：
    ///
    /// ```compile_fail,E0061
    /// fn borrowing_mode_is_not_available(session: &mut docxtpl_rs::RenderSession) {
    ///     let _ = session.new_subdoc();
    /// }
    /// ```
    pub fn new_subdoc(
        &mut self,
        docpath: impl AsRef<std::path::Path>,
    ) -> Result<RenderValue, Error> {
        let limits = self.pkg.limits().clone();
        subdoc::new_subdoc(
            &mut self.pkg,
            &mut self.injections,
            docpath.as_ref(),
            &limits,
        )
    }

    /// Construct a Subdoc from any seekable DOCX input stream.
    ///
    /// Merge semantics are identical to [`RenderSession::new_subdoc`]; this entry mirrors
    /// python-docx `Document(file_like)` and is suitable for upload streams or in-memory buffers,
    /// without first landing in a temporary file.
    pub fn new_subdoc_from_reader<R: Read + Seek>(
        &mut self,
        reader: R,
    ) -> Result<RenderValue, Error> {
        let limits = self.pkg.limits().clone();
        subdoc::new_subdoc_from_reader(&mut self.pkg, &mut self.injections, reader, &limits)
    }

    /// Construct a Subdoc from complete DOCX bytes.
    ///
    /// Convenience entry over [`RenderSession::new_subdoc_from_reader`].
    pub fn new_subdoc_from_bytes(&mut self, bytes: impl AsRef<[u8]>) -> Result<RenderValue, Error> {
        self.new_subdoc_from_reader(Cursor::new(bytes.as_ref()))
    }

    /// Upstream `DocxTemplate.replace_media` (P7): registers a media replacement matched by source-byte CRC32
    /// (the `word/media/` entries, including body/header/footer images).
    ///
    /// Only the final zip entry bytes are swapped: the part name, `[Content_Types].xml`, rels and
    /// wp:extent layout all remain as in the template. Pass already-read bytes for source/target (upstream's
    /// file-path and file-like forms are both unified to bytes here).
    pub fn replace_media(&mut self, src: impl AsRef<[u8]>, dst: impl AsRef<[u8]>) -> &mut Self {
        self.replacements.replace_media(src.as_ref(), dst.as_ref());
        self
    }

    /// Upstream `DocxTemplate.replace_embedded` (P7): registers an embedded-object replacement matched by source-byte CRC32
    /// (the `word/embeddings/` entries).
    pub fn replace_embedded(&mut self, src: impl AsRef<[u8]>, dst: impl AsRef<[u8]>) -> &mut Self {
        self.replacements
            .replace_embedded(src.as_ref(), dst.as_ref());
        self
    }

    /// Upstream `DocxTemplate.replace_zipname` (P7): replaces exactly by the full zip entry name
    /// (without a leading `/`, e.g. `word/embeddings/x.xlsx`).
    pub fn replace_zipname(&mut self, zipname: &str, dst: impl AsRef<[u8]>) -> &mut Self {
        self.replacements.replace_zipname(zipname, dst.as_ref());
        self
    }

    /// Upstream `DocxTemplate.replace_pic` (P7): registers a replacement keyed by the image cNvPr
    /// name/title/descr identifier; it applies to the media part blobs referenced by
    /// the pic shapes in the rendered body and headers/footers.
    ///
    /// When the identifier matches nothing across all scanned parts, [`RenderSession::finish`] /
    /// [`RenderSession::finish_without_render`] return a
    /// `ValueError`-like error ([`docxtpl_template::TemplateErrorKind::InvalidArgument`]).
    pub fn replace_pic(&mut self, pic_id: &str, dst: impl AsRef<[u8]>) -> &mut Self {
        self.replacements.replace_pic(pic_id, dst.as_ref());
        self
    }

    /// Sets whether an identifier registered via [`RenderSession::replace_pic`] is allowed to match nothing in the template.
    ///
    /// The default is `false`: on a miss, [`RenderSession::finish`] and
    /// [`RenderSession::finish_without_render`] return a `ValueError`-like error,
    /// matching the upstream default. When set to `true`, matched images are still replaced normally while unmatched
    /// registrations are silently ignored. This session policy is unaffected by [`RenderSession::reset_replacements`]
    /// .
    pub fn allow_missing_pics(&mut self, allow: bool) -> &mut Self {
        self.replacements.set_allow_missing_pics(allow);
        self
    }

    /// Upstream `DocxTemplate.reset_replacements` (P7): clears all
    /// media/embedded/zipname/pic replacement registrations of this session.
    pub fn reset_replacements(&mut self) -> &mut Self {
        self.replacements.reset();
        self
    }

    /// Renders every part (body -> headers -> footers -> core properties -> footnotes, P5),
    /// commits the media part / per-scope rels / Content Types changes, and performs final package validation.
    pub fn finish(mut self, context: &RenderContext) -> Result<RenderedDocument, Error> {
        render_all_parts(&mut self.pkg, context, &self.options, &mut self.injections)?;

        // Write media parts first, then write back each owner's rels/CT, guaranteeing the final validation finds no dangling relationships/types.
        self.injections.apply(&mut self.pkg)?;
        self.finish_replacements(false)
    }

    /// Upstream "save without render" path (P7, save() L887-889): the
    /// template rendering pipeline does not run (no patch/render, no fix_tables/fix_docpr_ids;
    /// docPr ids and other semantics stay unchanged); only pre/post replacements and the python-docx
    /// save-time XML/relationship/content-types normalization run before writing to disk.
    ///
    /// Mirrors the upstream semantics where save reopens the template when `is_rendered=False`: this
    /// exit must not be mixed with [`RenderSession::build_url_id`] /
    /// [`RenderSession::new_subdoc`] (its staged/merged content must not be applied).
    pub fn finish_without_render(self) -> Result<RenderedDocument, Error> {
        self.finish_replacements(true)
    }

    /// pre_processing (replace_pic) -> CT normalization (python-docx rebuilds the CT on every save)
    /// -> post_processing (CRC/zipname byte replacement) -> final validation.
    fn finish_replacements(
        mut self,
        normalize_all_known_xml_parts: bool,
    ) -> Result<RenderedDocument, Error> {
        let main_name = self.pkg.main_document_uri()?.as_str().to_string();
        // pre_processing: swap image part blobs on the final XML (before docx.save).
        self.replacements
            .apply_pic_replacements(&mut self.pkg, &main_name)?;
        canonicalize_content_types(&mut self.pkg, normalize_all_known_xml_parts)?;
        // post_processing: CRC/zipname byte replacement over the final part set.
        self.replacements.apply_byte_replacements(&mut self.pkg)?;
        self.pkg.validate()?;

        Ok(RenderedDocument { pkg: self.pkg })
    }
}

/// Part orchestration of upstream `DocxTemplate.render` (fixed order, ADR-006):
/// body -> headers (main-document rels order) -> footers (main-document rels order) ->
/// core properties -> footnotes (filtered by CT over the package part enumeration).
///
/// Header/footer image relationships are allocated in each part's own scope
/// ([`ImageInjections::begin_owner`]); footnotes disallow images and their output is not re-serialized.
/// Changes are written back only to parts that were rendered; the rels/CT commit is performed by the caller after rendering via
/// [`ImageInjections::apply`].
fn render_all_parts(
    pkg: &mut Package,
    context: &RenderContext,
    options: &RenderOptions,
    injections: &mut ImageInjections,
) -> Result<(), Error> {
    let main_name = pkg.main_document_uri()?.as_str().to_string();

    // 1. Body: fix_tables + fix_docpr_ids; image relationships belong to the main document.
    injections.begin_owner(pkg, &main_name)?;
    let body_src = read_xml_part(pkg, &main_name)?;
    let body = render_document_xml_ctx(&body_src, context, options, injections)?;
    pkg.set_part_bytes(&main_name, body.xml.into_bytes())?;

    // 2/3. Headers and footers (two rels enumeration passes, ordered like
    // build_headers_footers_xml(HEADER_URI) then (FOOTER_URI)): the lxml round trip and
    // resolve_listing still run, but without fix_tables / fix_docpr_ids.
    let stories = story_parts(pkg, &main_name)?;
    for name in stories {
        injections.begin_owner(pkg, &name)?;
        let src = read_xml_part(pkg, &name)?;
        let outcome = render_story_xml_ctx(&src, context, options, injections, &name)?;
        pkg.set_part_bytes(&name, outcome.xml.into_bytes())?;
    }

    // 4. Core properties: upstream render() unconditionally runs render_properties. The target resolves via the root
    // rels core-properties relationship; when missing, python-docx creates the default part.
    let core_name = ensure_core_properties_part(pkg)?;
    let core_src = read_xml_part(pkg, &core_name)?;
    let rendered =
        render_core_properties_ctx_with_options(&core_src, context, options, injections)?;
    pkg.set_part_bytes(&core_name, rendered.into_bytes())?;

    // 5. Notes: generic binary parts; the rendered strings are written back
    // as-is (preserving their XML declarations).
    for name in note_parts(pkg) {
        // Note parts are generic python-docx Parts. Their blobs are decoded as
        // UTF-8 and written back verbatim, so unlike the XmlPart paths above
        // they intentionally remain UTF-8-only.
        let src = read_part_utf8(pkg, &name)?;
        // Python docxtpl does not enumerate endnotes. Keep ordinary endnote
        // parts byte-for-byte untouched, while allowing the Rust extension to
        // opt in when an actual Jinja marker is present.
        if pkg.content_types().content_type_of(&PartUri::new(&name)?) == Some(CT_ENDNOTES)
            && !contains_template_marker(&src)
        {
            continue;
        }
        let rendered = render_footnotes_xml_ctx(&src, context, options, &name)?;
        pkg.set_part_bytes(&name, rendered.into_bytes())?;
    }

    Ok(())
}

fn contains_template_marker(xml: &str) -> bool {
    xml.contains("{{") || xml.contains("{%") || xml.contains("{#")
}

/// Package-level form normalization before saving (P7b B1, mirrors python-docx `PackageWriter.write`):
///
/// 1. Known XmlParts are re-serialized from the lxml tree: saving without rendering handles the full
///    [`PYTHON_DOCX_XML_CTS`] set; a normal render only handles members of
///    [`XML_CTS_TO_NORMALIZE_AFTER_RENDER`] not yet rewritten by the render pipeline, avoiding a second strip of image XML whitespace;
///    generic Parts pass through as blobs;
/// 2. Every `.rels` (including the root rels and customXml child rels) is always rewritten to the model's canonical
///    XML (single-quoted declaration);
/// 3. `[Content_Types].xml` is rebuilt per `_ContentTypesItem.from_parts`:
///    rels Overrides disappear, the rels/xml Defaults always exist, and parts whose extension hits the default table land in
///    Default while the rest land in Override, written back after sorting.
///
/// All three normalizations skip the write-back when the bytes are unchanged.
fn canonicalize_content_types(
    pkg: &mut Package,
    normalize_all_known_xml_parts: bool,
) -> Result<(), Error> {
    normalize_known_xml_parts(pkg, normalize_all_known_xml_parts)?;
    pkg.normalize_relationships()?;

    pkg.rebuild_content_types();
    let canonical = pkg.content_types().to_xml().into_bytes();
    let unchanged = match pkg.part("[Content_Types].xml") {
        Some(part) => part.bytes()? == canonical.as_slice(),
        None => false,
    };
    if !unchanged {
        pkg.set_part_bytes("[Content_Types].xml", canonical)?;
    }
    Ok(())
}

/// Unconditionally normalize the tree form of known XmlParts registered with python-docx
/// (python-docx always re-serializes these parts on save, even when rendering never touched them).
fn normalize_known_xml_parts(
    pkg: &mut Package,
    normalize_all_known_xml_parts: bool,
) -> Result<(), Error> {
    let content_types = if normalize_all_known_xml_parts {
        PYTHON_DOCX_XML_CTS
    } else {
        XML_CTS_TO_NORMALIZE_AFTER_RENDER
    };
    // Locate the target part name first, then read and normalize, avoiding a borrow conflict between traversal and set_part_bytes.
    let names: Vec<String> = pkg
        .parts()
        .filter(|part| {
            !part.is_dir()
                && pkg
                    .content_types()
                    .content_type_of(part.uri())
                    .is_some_and(|ct| content_types.contains(&ct))
        })
        .map(|part| part.name().to_string())
        .collect();
    let mut pending: Vec<(String, Vec<u8>)> = Vec::with_capacity(names.len());
    for name in &names {
        let src = read_xml_part(pkg, name)?;
        let normalized = normalize_part_xml(&src, name).map_err(Error::Render)?;
        pending.push((name.clone(), normalized.into_bytes()));
    }
    for (name, bytes) in pending {
        let unchanged = match pkg.part(&name) {
            Some(part) => part.bytes()? == bytes.as_slice(),
            None => false,
        };
        if !unchanged {
            pkg.set_part_bytes(&name, bytes)?;
        }
    }
    Ok(())
}

/// Enumerate internal header/footer targets in the main-document rels (P5).
///
/// Mirrors upstream `get_headers_footers` plus two build passes: the order is the main-document rels resolution order,
/// all headers before all footers; only internal relationships whose target part exists and has a
/// non-empty blob are collected; the same target part is deduplicated (see DEV-0007 for the pathological double-rel case).
fn story_parts(pkg: &Package, main_name: &str) -> Result<Vec<String>, Error> {
    let main_uri = PartUri::new(main_name)?;
    // A relative Target is resolved against the directory of the main-document part (word/), not the part path itself.
    let base_dir = main_uri.parent();
    let Some(rels) = pkg.relationships_of(main_name) else {
        return Ok(Vec::new());
    };
    let mut parts = Vec::new();
    for rel_type in [REL_TYPE_HEADER, REL_TYPE_FOOTER] {
        for rel in rels.iter() {
            if rel.target_mode != TargetMode::Internal || rel.rel_type != rel_type {
                continue;
            }
            let Some(target) = resolve_part_target(base_dir.as_ref(), &rel.target) else {
                continue;
            };
            let name = target.as_str();
            let Some(part) = pkg.part(name) else {
                continue;
            };
            // Upstream `if val.target_part.blob`: skip empty blobs.
            if part.bytes()?.is_empty() || parts.iter().any(|existing| existing == name) {
                continue;
            }
            parts.push(name.to_string());
        }
    }
    Ok(parts)
}

/// Enumerate footnote and endnote parts by content type. Both are generic
/// python-docx parts and therefore use the raw UTF-8 rendering path.
fn note_parts(pkg: &Package) -> Vec<String> {
    pkg.parts()
        .map(|part| part.uri())
        .filter(|uri| {
            pkg.content_types()
                .content_type_of(uri)
                .is_some_and(|ct| ct == CT_FOOTNOTES || ct == CT_ENDNOTES)
        })
        .map(|uri| uri.as_str().to_string())
        .collect()
}

/// Read a python-docx XmlPart and decode UTF-8 / UTF-16 / UTF-32 per XML auto-detection rules.
///
/// The BOM and leading-byte patterns mandated by XML 1.0 suffice to distinguish
/// UTF-16/32 LE/BE before parsing the declaration. These parts are all serialized via the XML tree afterwards,
/// so the output is uniformly UTF-8 bytes with a UTF-8 declaration.
fn read_xml_part(pkg: &Package, name: &str) -> Result<String, Error> {
    let bytes = pkg
        .part(name)
        .ok_or_else(|| OpcError::MissingPart {
            uri: name.to_string(),
        })?
        .bytes()?;
    decode_xml_bytes(bytes, name)
}

/// Upstream generic Part `blob.decode()` path: accepts only UTF-8, with no tree round trip.
fn read_part_utf8(pkg: &Package, name: &str) -> Result<String, Error> {
    let bytes = pkg
        .part(name)
        .ok_or_else(|| OpcError::MissingPart {
            uri: name.to_string(),
        })?
        .bytes()?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|source| Error::NotUtf8 {
            part: name.to_string(),
            source,
        })
}

/// Decode by the initial-byte patterns of XML 1.0 section 4.3.3. Supports
/// UTF-8, UTF-16LE/BE and UTF-32LE/BE; unsupported encoding declarations are rejected before entering the string-XML
/// parser, so declarations are never silently ignored.
pub(crate) fn decode_xml_bytes(bytes: &[u8], part: &str) -> Result<String, Error> {
    #[derive(Clone, Copy)]
    enum Encoding {
        Utf8,
        Utf16 { little_endian: bool },
        Utf32 { little_endian: bool },
    }

    // The UTF-32LE BOM is prefixed by the UTF-16LE BOM, so four-byte patterns must be checked first.
    let (encoding, offset) = if bytes.starts_with(&[0xff, 0xfe, 0x00, 0x00]) {
        (
            Encoding::Utf32 {
                little_endian: true,
            },
            4,
        )
    } else if bytes.starts_with(&[0x00, 0x00, 0xfe, 0xff]) {
        (
            Encoding::Utf32 {
                little_endian: false,
            },
            4,
        )
    } else if bytes.starts_with(&[0x3c, 0x00, 0x00, 0x00]) {
        (
            Encoding::Utf32 {
                little_endian: true,
            },
            0,
        )
    } else if bytes.starts_with(&[0x00, 0x00, 0x00, 0x3c]) {
        (
            Encoding::Utf32 {
                little_endian: false,
            },
            0,
        )
    } else if bytes.starts_with(&[0x00, 0x00, 0x3c, 0x00])
        || bytes.starts_with(&[0x00, 0x3c, 0x00, 0x00])
    {
        return Err(Error::InvalidXmlEncoding {
            part: part.to_string(),
            reason: "UCS-4 XML with a non-LE/BE byte order is not supported".to_string(),
        });
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (
            Encoding::Utf16 {
                little_endian: true,
            },
            2,
        )
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (
            Encoding::Utf16 {
                little_endian: false,
            },
            2,
        )
    } else if bytes.starts_with(&[0x3c, 0x00, 0x3f, 0x00]) {
        (
            Encoding::Utf16 {
                little_endian: true,
            },
            0,
        )
    } else if bytes.starts_with(&[0x00, 0x3c, 0x00, 0x3f]) {
        (
            Encoding::Utf16 {
                little_endian: false,
            },
            0,
        )
    } else {
        (
            Encoding::Utf8,
            usize::from(bytes.starts_with(&[0xef, 0xbb, 0xbf])) * 3,
        )
    };

    let payload = &bytes[offset..];
    let utf8_wire_encoding = matches!(encoding, Encoding::Utf8);
    let decoded = match encoding {
        Encoding::Utf8 => std::str::from_utf8(payload)
            .map(str::to_owned)
            .map_err(|source| Error::NotUtf8 {
                part: part.to_string(),
                source,
            })?,
        Encoding::Utf16 { little_endian } => decode_utf16(payload, little_endian, part)?,
        Encoding::Utf32 { little_endian } => decode_utf32(payload, little_endian, part)?,
    };
    validate_xml_encoding_declaration(&decoded, part, utf8_wire_encoding)?;
    Ok(decoded)
}

fn decode_utf16(payload: &[u8], little_endian: bool, part: &str) -> Result<String, Error> {
    if payload.len() % 2 != 0 {
        return Err(Error::InvalidXmlEncoding {
            part: part.to_string(),
            reason: "UTF-16 has an odd number of bytes".to_string(),
        });
    }
    let units = payload.chunks_exact(2).map(|pair| {
        if little_endian {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        }
    });
    let mut decoded = String::with_capacity(payload.len());
    for scalar in char::decode_utf16(units) {
        match scalar {
            Ok(ch) => decoded.push(ch),
            Err(source) => {
                return Err(Error::InvalidXmlEncoding {
                    part: part.to_string(),
                    reason: source.to_string(),
                });
            }
        }
    }
    Ok(decoded)
}

fn decode_utf32(payload: &[u8], little_endian: bool, part: &str) -> Result<String, Error> {
    if payload.len() % 4 != 0 {
        return Err(Error::InvalidXmlEncoding {
            part: part.to_string(),
            reason: "UTF-32 byte count is not a multiple of 4".to_string(),
        });
    }

    let mut decoded = String::with_capacity(payload.len());
    for (index, chunk) in payload.chunks_exact(4).enumerate() {
        let scalar = if little_endian {
            u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
        } else {
            u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
        };
        let Some(ch) = char::from_u32(scalar) else {
            return Err(Error::InvalidXmlEncoding {
                part: part.to_string(),
                reason: format!("UTF-32 code point #{} U+{scalar:08X} is invalid", index + 1),
            });
        };
        decoded.push(ch);
    }
    Ok(decoded)
}

/// Return the `encoding` pseudo-attribute of the XML declaration, if present.
/// Only the bounded scan needed by the decoding layer happens here; the rest of declaration syntax is still handled by the XML parser.
fn xml_declared_encoding(xml: &str) -> Option<&str> {
    // Stay consistent with the prolog entry of the internal XML parser: it allows a decoded
    // U+FEFF and XML whitespace before the declaration.
    let xml = xml.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    let declaration = xml.strip_prefix("<?xml")?;
    let declaration = declaration.get(..declaration.find("?>")?)?;
    let bytes = declaration.as_bytes();
    let mut pos = 0usize;

    while pos < bytes.len() {
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let name_start = pos;
        while bytes.get(pos).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b':' | b'-' | b'.')
        }) {
            pos += 1;
        }
        if pos == name_start {
            pos += 1;
            continue;
        }
        let name = &declaration[name_start..pos];
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if bytes.get(pos) != Some(&b'=') {
            continue;
        }
        pos += 1;
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let quote = *bytes.get(pos)?;
        if !matches!(quote, b'\'' | b'"') {
            continue;
        }
        pos += 1;
        let value_start = pos;
        while bytes.get(pos).is_some_and(|byte| *byte != quote) {
            pos += 1;
        }
        let value = declaration.get(value_start..pos)?;
        if name == "encoding" {
            return Some(value);
        }
        pos += usize::from(pos < bytes.len());
    }
    None
}

fn validate_xml_encoding_declaration(
    xml: &str,
    part: &str,
    utf8_wire_encoding: bool,
) -> Result<(), Error> {
    let Some(label) = xml_declared_encoding(xml) else {
        return Ok(());
    };
    let mut label_bytes = label.bytes();
    if label.len() > 128
        || !label_bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        || !label_bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Error::InvalidXmlEncoding {
            part: part.to_string(),
            reason: format!("invalid encoding name in XML declaration: {label:?}"),
        });
    }
    let normalized: String = label
        .bytes()
        .filter(|byte| !matches!(*byte, b'-' | b'_'))
        .map(|byte| byte.to_ascii_uppercase() as char)
        .collect();
    if matches!(
        normalized.as_str(),
        "UTF8" | "UTF16" | "UTF16LE" | "UTF16BE" | "UTF32" | "UTF32LE" | "UTF32BE"
    ) {
        // libxml2 rejects UTF-8 bytes declared as UTF-16/32; whereas the BOM/four-byte
        // initial patterns of UTF-16/32 already determine the byte order, and libxml2 defers to the detected result.
        if utf8_wire_encoding && normalized != "UTF8" {
            return Err(Error::InvalidXmlEncoding {
                part: part.to_string(),
                reason: format!("XML declaration declares encoding {label:?}, but the actual bytes decode as UTF-8"),
            });
        }
        return Ok(());
    }
    Err(Error::InvalidXmlEncoding {
        part: part.to_string(),
        reason: format!("unsupported encoding {label:?} in XML declaration"),
    })
}

/// Locate the core properties part via the root rels; when the relationship is missing, mirror python-docx and create the default
/// `/docProps/core.xml`, its content type and the root relationship.
fn ensure_core_properties_part(pkg: &mut Package) -> Result<String, Error> {
    const CORE_NAME: &str = "docProps/core.xml";
    const CORE_CONTENT_TYPE: &str = "application/vnd.openxmlformats-package.core-properties+xml";
    const CORE_REL_TYPE: &str =
        "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";

    if let Some(name) = pkg
        .root_relationships()
        .iter()
        .find(|rel| {
            rel.rel_type.ends_with("/metadata/core-properties")
                && rel.target_mode == TargetMode::Internal
        })
        .and_then(|rel| resolve_part_target(None, &rel.target))
        .map(|uri| uri.as_str().to_string())
        .filter(|name| pkg.contains(name))
    {
        return Ok(name);
    }

    let default_xml = default_core_properties_xml().into_bytes();
    if pkg.contains(CORE_NAME) {
        // A same-named part not referenced by a relationship is invisible in python-docx; creating the default part
        // is equivalent to replacing that orphan entry.
        pkg.set_part_bytes(CORE_NAME, default_xml)?;
    } else {
        pkg.add_part(CORE_NAME, default_xml)?;
    }

    let mut content_types = pkg.content_types().clone();
    content_types.add_override(CORE_NAME, CORE_CONTENT_TYPE);
    pkg.set_part_bytes("[Content_Types].xml", content_types.to_xml().into_bytes())?;

    let mut relationships = pkg.root_relationships().clone();
    relationships.push(Relationship {
        id: relationships.next_r_id(),
        rel_type: CORE_REL_TYPE.to_string(),
        target: CORE_NAME.to_string(),
        target_mode: TargetMode::Internal,
    });
    pkg.set_part_bytes("_rels/.rels", relationships.to_xml().into_bytes())?;
    Ok(CORE_NAME.to_string())
}

/// Default XML for python-docx `CorePropertiesPart.default()`. Time precision matches upstream
/// at UTC seconds (W3CDTF).
fn default_core_properties_xml() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let timestamp = unix_seconds_to_w3cdtf(seconds);
    format!(
        "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
         <cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:dcterms=\"http://purl.org/dc/terms/\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
         <dc:title>Word Document</dc:title>\
         <cp:lastModifiedBy xmlns:cp=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\">python-docx</cp:lastModifiedBy>\
         <cp:revision xmlns:cp=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\">1</cp:revision>\
         <dcterms:modified xsi:type=\"dcterms:W3CDTF\">{timestamp}</dcterms:modified>\
         </cp:coreProperties>"
    )
}

fn unix_seconds_to_w3cdtf(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;

    // Howard Hinnant's civil_from_days; the Unix epoch corresponds to 1970-01-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);

    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The document resulting from a render.
#[derive(Debug)]
pub struct RenderedDocument {
    pkg: Package,
}

impl RenderedDocument {
    /// Save to a file (unchanged parts keep their original bytes, see ADR-002 DEV-0004).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        self.pkg.save(path)?;
        Ok(())
    }

    /// Write to any seekable writer.
    pub fn write_to(&self, writer: impl Write + std::io::Seek) -> Result<(), Error> {
        self.pkg.write_to(writer)?;
        Ok(())
    }

    /// Serialize to in-memory docx bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut buf = Cursor::new(Vec::new());
        self.pkg.write_to(&mut buf)?;
        Ok(buf.into_inner())
    }
}

impl Error {
    /// Stable category of a template error (None for OPC/IO and similar errors).
    #[must_use]
    pub fn kind(&self) -> Option<TemplateErrorKind> {
        match self {
            Error::Render(e) => e.kind(),
            _ => None,
        }
    }
}

/// Facade-level errors: OPC, template rendering or encoding problems.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The compressed input file exceeds the default read limit.
    #[error("input DOCX exceeds the {max} byte limit")]
    InputTooLarge { max: u64 },
    /// ZIP/OPC package error (limits, URIs, relationships, etc.).
    #[error(transparent)]
    Opc(#[from] OpcError),

    /// Template rendering error (syntax/context/XML healing/post-processing).
    #[error(transparent)]
    Render(#[from] RenderError),

    /// A UTF-8-only generic part is not valid UTF-8.
    #[error("part {part} is not valid UTF-8: {source}")]
    NotUtf8 {
        /// Name of the failing part.
        part: String,
        /// Underlying encoding error.
        #[source]
        source: std::str::Utf8Error,
    },

    /// The XmlPart's Unicode byte sequence or encoding declaration is invalid.
    #[error("part {part} is not valid XML text encoding: {reason}")]
    InvalidXmlEncoding {
        /// Name of the failing part.
        part: String,
        /// Stable description of the decoding error.
        reason: String,
    },

    /// File IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub use docxtpl_rich::{InlineImage, Listing, RichText, RichTextParagraph, RichTextProps};
pub use docxtpl_template::{
    minijinja, EnvironmentConfigurator, RenderOptions, RenderValue, TemplateErrorKind,
};
