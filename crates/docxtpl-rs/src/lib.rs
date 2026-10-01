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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use docxtpl_opc::{resolve_part_target, OpcError, PartUri, Relationship, TargetMode};
pub use docxtpl_opc::{
    InterruptibleWriteError, MediaCompression, Package, PackageEvictionReport, PackageLimits,
    PackageResidency, PackageTransaction, PackageWriteReport, WriteOptions,
};
use docxtpl_template::{
    find_undeclared_variables, normalize_part_xml, prepare_document_xml_template,
    prepare_footnotes_xml_template, prepare_render_context, prepare_story_xml_template,
    render_core_properties_prepared, render_document_xml_from_template,
    render_footnotes_xml_from_template, render_story_xml_from_template, PreparedXmlTemplate,
    RenderError,
};
// Serves both as internal types and public re-exports (the pub use list at the bottom does not repeat them).
pub use docxtpl_template::{JsonContextError, RenderContext};

mod async_render;
mod images;
mod persistent_cache;
mod replacements;
mod subdoc;
mod text_index;

pub use async_render::{
    AsyncDispatchError, AsyncRenderDispatcher, BlockingExecutor, BlockingTask, RenderTask,
};
pub use persistent_cache::{PreparedCachePolicy, PreparedCacheStats, PreparedTemplateCache};

pub use text_index::{
    FormattingPolicy, RunFormatOverrides, RunTextEditError, RunTextIndex, RunTextLimits,
    TextFragment, TextIndexError, TextMatch,
};

use images::ImageInjections;
use replacements::{picture_map, Replacements};

const DEFAULT_DOCUMENT_BYTES: u64 = 600 * 1024 * 1024;

/// Cloneable cooperative cancellation signal for rendering, post-processing,
/// validation, and ZIP output.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. The operation stops at its next checkpoint.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Return [`CancellationError::Cancelled`] when cancellation has been requested.
    pub fn check(&self) -> Result<(), CancellationError> {
        if self.is_cancelled() {
            Err(CancellationError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Error returned by opt-in operations that carry a [`CancellationToken`].
#[derive(Debug, thiserror::Error)]
pub enum CancellationError {
    /// The cancellation token requested that the operation stop.
    #[error("operation cancelled")]
    Cancelled,
    /// An ordinary rendering, editing, validation, or output failure.
    #[error(transparent)]
    Operation(#[from] Error),
}

impl CancellationError {
    fn from_operation(error: Error, cancellation: &CancellationToken) -> Self {
        if cancellation.is_cancelled() {
            Self::Cancelled
        } else {
            Self::Operation(error)
        }
    }
}

/// Resource-budget name used by the unified 1.3 execution-control API.
///
/// This alias deliberately preserves the complete 1.2 [`ResourceLimits`]
/// builder and constructor compatibility.
pub type RenderLimits = ResourceLimits;

/// Why a controlled operation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStopReason {
    Cancelled,
    DeadlineExceeded,
}

/// Cloneable cooperative execution control for rendering, editing, and ZIP
/// output. It creates no worker threads or timers; callers and library
/// checkpoints observe the token and deadline.
#[derive(Debug, Clone, Default)]
pub struct RenderControl {
    cancellation: CancellationToken,
    deadline: Option<Instant>,
}

impl RenderControl {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }

    #[must_use]
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.deadline = Instant::now().checked_add(timeout);
        self
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    #[must_use]
    pub fn stop_reason(&self) -> Option<RenderStopReason> {
        if self.cancellation.is_cancelled() {
            Some(RenderStopReason::Cancelled)
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Some(RenderStopReason::DeadlineExceeded)
        } else {
            None
        }
    }

    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stop_reason().is_some()
    }

    pub fn check(&self) -> Result<(), RenderControlError> {
        match self.stop_reason() {
            Some(RenderStopReason::Cancelled) => Err(RenderControlError::Cancelled),
            Some(RenderStopReason::DeadlineExceeded) => Err(RenderControlError::DeadlineExceeded),
            None => Ok(()),
        }
    }

    fn from_cancellation(cancellation: &CancellationToken) -> Self {
        Self::new().with_cancellation(cancellation.clone())
    }
}

/// Error returned by operations using [`RenderControl`].
#[derive(Debug, thiserror::Error)]
pub enum RenderControlError {
    #[error("operation cancelled")]
    Cancelled,
    #[error("render deadline exceeded")]
    DeadlineExceeded,
    #[error(transparent)]
    Operation(#[from] Error),
}

impl RenderControlError {
    fn from_operation(error: Error, control: &RenderControl) -> Self {
        match control.stop_reason() {
            Some(RenderStopReason::Cancelled) => Self::Cancelled,
            Some(RenderStopReason::DeadlineExceeded) => Self::DeadlineExceeded,
            None => Self::Operation(error),
        }
    }
}

fn check_render_control(control: Option<&RenderControl>) -> Result<(), Error> {
    if let Some(control) = control {
        if let Some(reason) = control.stop_reason() {
            let message = match reason {
                RenderStopReason::Cancelled => "operation cancelled",
                RenderStopReason::DeadlineExceeded => "render deadline exceeded",
            };
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                message,
            )));
        }
    }
    Ok(())
}

/// Content type of the footnotes part (upstream render_footnotes filters
/// package.parts by it; this CT is not registered in the python-docx PartFactory, so it is handled as a generic
/// binary part whose rendered output is written back as-is without re-serialization, ADR-006).
const CT_FOOTNOTES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";
/// Content type of the endnotes part. Endnotes use the same generic-part
/// string rendering path as footnotes.
const CT_ENDNOTES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml";
const CT_COMMENTS: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml";
const IMAGE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const HYPERLINK_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
enum CachedPartKind {
    Document,
    Story,
    Notes,
}

#[derive(Debug)]
struct CachedPartTemplate {
    source: String,
    template: Arc<PreparedXmlTemplate>,
}

#[derive(Debug, Default)]
struct TemplateRenderCache {
    parts: Mutex<HashMap<(String, CachedPartKind, usize), CachedPartTemplate>>,
    persistent: Option<PreparedTemplateCache>,
}

impl TemplateRenderCache {
    fn new(persistent: Option<PreparedTemplateCache>) -> Self {
        Self {
            parts: Mutex::new(HashMap::new()),
            persistent,
        }
    }

    fn uses_persistent_storage(&self) -> bool {
        self.persistent.is_some()
    }

    fn get_or_prepare(
        &self,
        template_digest: &[u8; 32],
        part_name: &str,
        kind: CachedPartKind,
        source: &str,
        max_rendered_xml_bytes: usize,
        prepare: impl FnOnce() -> Result<PreparedXmlTemplate, RenderError>,
    ) -> Result<Arc<PreparedXmlTemplate>, Error> {
        let key = (part_name.to_string(), kind, max_rendered_xml_bytes);
        {
            let parts = self.parts.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(cached) = parts.get(&key) {
                if cached.source == source {
                    return Ok(Arc::clone(&cached.template));
                }
            }
        }

        let options_fingerprint = persistent_cache::digest(
            &u64::try_from(max_rendered_xml_bytes)
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        let template = if let Some(cache) = &self.persistent {
            cache
                .load(
                    template_digest,
                    &options_fingerprint,
                    kind as u8,
                    part_name,
                    source,
                )
                .map(Arc::new)
        } else {
            None
        };
        let template = match template {
            Some(template) => template,
            None => {
                let template = Arc::new(prepare()?);
                if let Some(cache) = &self.persistent {
                    cache.store(
                        template_digest,
                        &options_fingerprint,
                        kind as u8,
                        part_name,
                        source,
                        &template,
                    );
                }
                template
            }
        };
        let mut parts = self.parts.lock().unwrap_or_else(|error| error.into_inner());
        parts.insert(
            key,
            CachedPartTemplate {
                source: source.to_string(),
                template: Arc::clone(&template),
            },
        );
        Ok(template)
    }
}

/// A reusable docx template.
///
/// Context-independent XML preprocessing is cached per part after the first
/// render. Path-backed templates compare the complete current part source on
/// every render, so changed template content invalidates the affected entry.
#[derive(Debug)]
pub struct DocxTemplate {
    source: TemplateSource,
    limits: ResourceLimits,
    render_cache: Arc<TemplateRenderCache>,
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
            render_cache: Arc::new(TemplateRenderCache::new(None)),
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
            render_cache: Arc::new(TemplateRenderCache::new(None)),
        })
    }

    /// Attach an explicit persistent preprocessing cache. Existing in-memory
    /// entries are discarded so subsequent renders use the selected policy.
    #[must_use]
    pub fn with_prepared_cache(mut self, cache: PreparedTemplateCache) -> Self {
        self.render_cache = Arc::new(TemplateRenderCache::new(Some(cache)));
        self
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
        self.render_checked(context, options, None)
    }

    /// Render with cooperative cancellation checkpoints between package open,
    /// each story phase, canonicalization, and final validation.
    pub fn render_with_cancellation(
        &self,
        context: &serde_json::Value,
        options: &RenderOptions,
        cancellation: &CancellationToken,
    ) -> Result<RenderedDocument, CancellationError> {
        let control = RenderControl::from_cancellation(cancellation);
        self.render_checked(context, options, Some(&control))
            .map_err(|error| CancellationError::from_operation(error, cancellation))
    }

    /// Render with unified cancellation and deadline control.
    pub fn render_with_control(
        &self,
        context: &serde_json::Value,
        options: &RenderOptions,
        control: &RenderControl,
    ) -> Result<RenderedDocument, RenderControlError> {
        self.render_checked(context, options, Some(control))
            .map_err(|error| RenderControlError::from_operation(error, control))
    }

    fn render_checked(
        &self,
        context: &serde_json::Value,
        options: &RenderOptions,
        control: Option<&RenderControl>,
    ) -> Result<RenderedDocument, Error> {
        check_render_control(control)?;
        let options = self.effective_render_options(options);
        // Open a fresh package per render: DocxTemplate is reusable and state does not leak across renders (spec §2.2).
        let package_open_started = Instant::now();
        let template_digest = if self.render_cache.uses_persistent_storage() {
            self.template_digest()?
        } else {
            [0; 32]
        };
        let mut pkg = self.open_package()?;
        check_render_control(control)?;
        let package_open_elapsed = package_open_started.elapsed();
        let render_started = Instant::now();
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

        render_all_parts(
            &mut pkg,
            &context,
            &options,
            &mut injections,
            &self.render_cache,
            &template_digest,
            control,
        )?;
        check_render_control(control)?;
        // A JSON context contains no images/external links, so apply is effectively a no-op (no dirty scopes).
        injections.apply(&mut pkg)?;
        canonicalize_content_types(&mut pkg, false)?;

        // Validate the package once more before writing out: no dangling relationships or missing parts allowed.
        check_render_control(control)?;
        pkg.validate()?;
        check_render_control(control)?;

        Ok(RenderedDocument {
            pkg,
            edited: false,
            max_rendered_xml_bytes: options.max_rendered_xml_bytes(),
            package_open_elapsed,
            render_elapsed: render_started.elapsed(),
            postprocess_passes: Vec::new(),
        })
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

    /// Render a rich context with cooperative cancellation.
    pub fn render_ctx_with_cancellation(
        &self,
        context: &RenderContext,
        options: &RenderOptions,
        cancellation: &CancellationToken,
    ) -> Result<RenderedDocument, CancellationError> {
        self.render_session(options)
            .map_err(|error| CancellationError::from_operation(error, cancellation))?
            .finish_with_cancellation(context, cancellation)
    }

    /// Render a rich context with unified cancellation and deadline control.
    pub fn render_ctx_with_control(
        &self,
        context: &RenderContext,
        options: &RenderOptions,
        control: &RenderControl,
    ) -> Result<RenderedDocument, RenderControlError> {
        self.render_session_with_control(options, control)?
            .finish_with_control(context, control)
    }

    /// Open a one-shot rich-content render session: inside the session one may first call [`RenderSession::build_url_id`]
    /// to pre-register external hyperlink relationships (mirroring the upstream `tpl.build_url_id` call before rendering),
    /// then call [`RenderSession::finish`] to complete the render.
    pub fn render_session(&self, options: &RenderOptions) -> Result<RenderSession, Error> {
        let package_open_started = Instant::now();
        let template_digest = if self.render_cache.uses_persistent_storage() {
            self.template_digest()?
        } else {
            [0; 32]
        };
        let pkg = self.open_package()?;
        let package_open_elapsed = package_open_started.elapsed();
        let options = self.effective_render_options(options);
        RenderSession::new(
            pkg,
            &options,
            Arc::clone(&self.render_cache),
            template_digest,
            package_open_elapsed,
        )
    }

    /// Open a rich render session after checking unified execution control.
    pub fn render_session_with_control(
        &self,
        options: &RenderOptions,
        control: &RenderControl,
    ) -> Result<RenderSession, RenderControlError> {
        control.check()?;
        let mut session = self
            .render_session(options)
            .map_err(|error| RenderControlError::from_operation(error, control))?;
        session.control = Some(control.clone());
        Ok(session)
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

    fn template_digest(&self) -> Result<[u8; 32], Error> {
        match &self.source {
            TemplateSource::Path(path) => Ok(persistent_cache::digest_path(path)?),
            TemplateSource::Bytes(data) => Ok(persistent_cache::digest(data)),
        }
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
    render_cache: Arc<TemplateRenderCache>,
    template_digest: [u8; 32],
    package_open_elapsed: Duration,
    control: Option<RenderControl>,
}

impl RenderSession {
    /// Locates the main document and builds the image injection registry (raises an OPC error when the main-document rels is missing).
    fn new(
        pkg: Package,
        options: &RenderOptions,
        render_cache: Arc<TemplateRenderCache>,
        template_digest: [u8; 32],
        package_open_elapsed: Duration,
    ) -> Result<Self, Error> {
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let injections = ImageInjections::new(&pkg, &main_name)?;
        Ok(Self {
            pkg,
            options: options.clone(),
            injections,
            replacements: Replacements::new(),
            render_cache,
            template_digest,
            package_open_elapsed,
            control: None,
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
        check_render_control(self.control.as_ref())?;
        let render_started = Instant::now();
        render_all_parts(
            &mut self.pkg,
            context,
            &self.options,
            &mut self.injections,
            &self.render_cache,
            &self.template_digest,
            self.control.as_ref(),
        )?;

        // Write media parts first, then write back each owner's rels/CT, guaranteeing the final validation finds no dangling relationships/types.
        check_render_control(self.control.as_ref())?;
        self.injections.apply(&mut self.pkg)?;
        self.finish_replacements(false, render_started.elapsed())
    }

    /// Finish a rich-content session with cooperative cancellation. This is
    /// the cancellable counterpart for sessions that pre-register hyperlinks,
    /// subdocuments, or replacement operations before rendering.
    pub fn finish_with_cancellation(
        mut self,
        context: &RenderContext,
        cancellation: &CancellationToken,
    ) -> Result<RenderedDocument, CancellationError> {
        self.control = Some(RenderControl::from_cancellation(cancellation));
        self.finish(context)
            .map_err(|error| CancellationError::from_operation(error, cancellation))
    }

    /// Finish this session with unified cancellation and deadline control.
    pub fn finish_with_control(
        mut self,
        context: &RenderContext,
        control: &RenderControl,
    ) -> Result<RenderedDocument, RenderControlError> {
        self.control = Some(control.clone());
        self.finish(context)
            .map_err(|error| RenderControlError::from_operation(error, control))
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
        let render_started = Instant::now();
        self.finish_replacements(true, render_started.elapsed())
    }

    /// Finish the replacement-only path with unified execution control.
    pub fn finish_without_render_with_control(
        mut self,
        control: &RenderControl,
    ) -> Result<RenderedDocument, RenderControlError> {
        self.control = Some(control.clone());
        let render_started = Instant::now();
        self.finish_replacements(true, render_started.elapsed())
            .map_err(|error| RenderControlError::from_operation(error, control))
    }

    /// pre_processing (replace_pic) -> CT normalization (python-docx rebuilds the CT on every save)
    /// -> post_processing (CRC/zipname byte replacement) -> final validation.
    fn finish_replacements(
        mut self,
        normalize_all_known_xml_parts: bool,
        prior_render_elapsed: Duration,
    ) -> Result<RenderedDocument, Error> {
        check_render_control(self.control.as_ref())?;
        let replacements_started = Instant::now();
        let main_name = self.pkg.main_document_uri()?.as_str().to_string();
        // pre_processing: swap image part blobs on the final XML (before docx.save).
        self.replacements
            .apply_pic_replacements(&mut self.pkg, &main_name)?;
        check_render_control(self.control.as_ref())?;
        canonicalize_content_types(&mut self.pkg, normalize_all_known_xml_parts)?;
        // post_processing: CRC/zipname byte replacement over the final part set.
        check_render_control(self.control.as_ref())?;
        self.replacements.apply_byte_replacements(&mut self.pkg)?;
        check_render_control(self.control.as_ref())?;
        self.pkg.validate()?;
        check_render_control(self.control.as_ref())?;

        let max_rendered_xml_bytes = self.options.max_rendered_xml_bytes();
        Ok(RenderedDocument {
            pkg: self.pkg,
            edited: false,
            max_rendered_xml_bytes,
            package_open_elapsed: self.package_open_elapsed,
            render_elapsed: prior_render_elapsed.saturating_add(replacements_started.elapsed()),
            postprocess_passes: Vec::new(),
        })
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
    render_cache: &TemplateRenderCache,
    template_digest: &[u8; 32],
    control: Option<&RenderControl>,
) -> Result<(), Error> {
    check_render_control(control)?;
    let main_name = pkg.main_document_uri()?.as_str().to_string();
    let prepared_context = prepare_render_context(context, &main_name)?;

    // 1. Body: fix_tables + fix_docpr_ids; image relationships belong to the main document.
    check_render_control(control)?;
    injections.begin_owner(pkg, &main_name)?;
    let body_src = read_xml_part(pkg, &main_name)?;
    let body_template = render_cache.get_or_prepare(
        template_digest,
        &main_name,
        CachedPartKind::Document,
        &body_src,
        options.max_rendered_xml_bytes(),
        || prepare_document_xml_template(&body_src, options),
    )?;
    let body =
        render_document_xml_from_template(&body_template, &prepared_context, options, injections)?;
    pkg.set_part_bytes(&main_name, body.xml.into_bytes())?;

    // 2/3. Headers and footers (two rels enumeration passes, ordered like
    // build_headers_footers_xml(HEADER_URI) then (FOOTER_URI)): the lxml round trip and
    // resolve_listing still run, but without fix_tables / fix_docpr_ids.
    let stories = story_parts(pkg, &main_name)?;
    for name in stories {
        check_render_control(control)?;
        injections.begin_owner(pkg, &name)?;
        let src = read_xml_part(pkg, &name)?;
        let template = render_cache.get_or_prepare(
            template_digest,
            &name,
            CachedPartKind::Story,
            &src,
            options.max_rendered_xml_bytes(),
            || prepare_story_xml_template(&src, options, &name),
        )?;
        let outcome = render_story_xml_from_template(
            &template,
            &prepared_context,
            options,
            injections,
            &name,
        )?;
        pkg.set_part_bytes(&name, outcome.xml.into_bytes())?;
    }

    // 4. Core properties: upstream render() unconditionally runs render_properties. The target resolves via the root
    // rels core-properties relationship; when missing, python-docx creates the default part.
    check_render_control(control)?;
    let core_name = ensure_core_properties_part(pkg)?;
    let core_src = read_xml_part(pkg, &core_name)?;
    let rendered =
        render_core_properties_prepared(&core_src, &prepared_context, options, injections)?;
    pkg.set_part_bytes(&core_name, rendered.into_bytes())?;

    // 5. Notes: generic binary parts; the rendered strings are written back
    // as-is (preserving their XML declarations).
    for name in note_parts(pkg) {
        check_render_control(control)?;
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
        let template = render_cache.get_or_prepare(
            template_digest,
            &name,
            CachedPartKind::Notes,
            &src,
            options.max_rendered_xml_bytes(),
            || prepare_footnotes_xml_template(&src, options, &name),
        )?;
        let rendered =
            render_footnotes_xml_from_template(&template, &prepared_context, options, &name)?;
        pkg.set_part_bytes(&name, rendered.into_bytes())?;
    }

    check_render_control(control)?;
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

/// Enumerate editable stories in stable body/header/footer order.
fn editable_story_parts(
    pkg: &Package,
    scope: StoryScope,
) -> Result<Vec<(String, StoryKind)>, Error> {
    let main_name = pkg.main_document_uri()?.as_str().to_string();
    let mut stories = Vec::new();
    if matches!(scope, StoryScope::Body | StoryScope::BodyHeadersFooters) {
        stories.push((main_name.clone(), StoryKind::Body));
    }
    if matches!(scope, StoryScope::Body) {
        return Ok(stories);
    }

    let main_uri = PartUri::new(&main_name)?;
    let base_dir = main_uri.parent();
    let Some(rels) = pkg.relationships_of(&main_name) else {
        return Ok(stories);
    };
    for (rel_type, kind) in [
        (REL_TYPE_HEADER, StoryKind::Header),
        (REL_TYPE_FOOTER, StoryKind::Footer),
    ] {
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
            if part.bytes()?.is_empty()
                || stories
                    .iter()
                    .any(|(existing, _)| existing.as_str() == name)
            {
                continue;
            }
            stories.push((name.to_string(), kind));
        }
    }
    Ok(stories)
}

/// Enumerate every selected editable Word story in a deterministic order.
fn unified_editable_story_parts(
    pkg: &Package,
    selection: EditableStorySelection,
) -> Result<Vec<(String, EditableStoryKind)>, Error> {
    let main_name = pkg.main_document_uri()?.as_str().to_string();
    let mut stories = Vec::new();
    if selection.contains(EditableStoryKind::Body) {
        stories.push((main_name.clone(), EditableStoryKind::Body));
    }

    let main_uri = PartUri::new(&main_name)?;
    let base_dir = main_uri.parent();
    if let Some(rels) = pkg.relationships_of(&main_name) {
        for (rel_type, kind) in [
            (REL_TYPE_HEADER, EditableStoryKind::Header),
            (REL_TYPE_FOOTER, EditableStoryKind::Footer),
        ] {
            if !selection.contains(kind) {
                continue;
            }
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
                if part.bytes()?.is_empty()
                    || stories
                        .iter()
                        .any(|(existing, _)| existing.as_str() == name)
                {
                    continue;
                }
                stories.push((name.to_string(), kind));
            }
        }
    }

    for (content_type, kind) in [
        (CT_FOOTNOTES, EditableStoryKind::Footnote),
        (CT_ENDNOTES, EditableStoryKind::Endnote),
        (CT_COMMENTS, EditableStoryKind::Comment),
    ] {
        if !selection.contains(kind) {
            continue;
        }
        let mut names: Vec<_> = pkg
            .parts()
            .filter(|part| pkg.content_types().content_type_of(part.uri()) == Some(content_type))
            .map(|part| part.name().to_string())
            .collect();
        names.sort();
        for name in names {
            let part = pkg
                .part(&name)
                .ok_or_else(|| OpcError::MissingPart { uri: name.clone() })?;
            if !part.bytes()?.is_empty() {
                stories.push((name, kind));
            }
        }
    }
    Ok(stories)
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
    edited: bool,
    max_rendered_xml_bytes: usize,
    package_open_elapsed: Duration,
    render_elapsed: Duration,
    postprocess_passes: Vec<PassReport>,
}

/// Structured timings and ZIP metrics for one completed output write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderReport {
    /// Time spent opening the template package for this render.
    pub package_open_elapsed: Duration,
    /// Time spent rendering and applying render-session replacements.
    pub render_elapsed: Duration,
    /// Sum of completed post-processing pass durations.
    pub postprocess_elapsed: Duration,
    /// Final OPC integrity validation time immediately before output.
    pub validation_elapsed: Duration,
    /// Final ZIP serialization time.
    pub zip_write_elapsed: Duration,
    /// Completed post-processing passes in execution order.
    pub postprocess_passes: Vec<PassReport>,
    /// Aggregate metrics from the single final ZIP serialization.
    pub package_write: PackageWriteReport,
}

/// Which Word story parts to visit in a post-processing operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryScope {
    /// Visit only the main document part.
    Body,
    /// Visit internal header and footer parts referenced by the main document.
    HeadersFooters,
    /// Visit the main document followed by referenced headers and footers.
    BodyHeadersFooters,
}

/// Kind of Word story currently being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryKind {
    /// Main document body.
    Body,
    /// Header part.
    Header,
    /// Footer part.
    Footer,
}

/// Kind of editable Word story exposed by the unified 1.3 editing API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditableStoryKind {
    /// Main document body.
    Body,
    /// Header part referenced by the main document.
    Header,
    /// Footer part referenced by the main document.
    Footer,
    /// Footnotes part selected by its registered content type.
    Footnote,
    /// Endnotes part selected by its registered content type.
    Endnote,
    /// Word comments part selected by its registered content type.
    Comment,
}

/// Bit-set selecting story kinds for the unified 1.3 editing API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EditableStorySelection(u8);

impl EditableStorySelection {
    /// No stories.
    pub const NONE: Self = Self(0);
    /// Main document body.
    pub const BODY: Self = Self(1 << 0);
    /// Header parts.
    pub const HEADERS: Self = Self(1 << 1);
    /// Footer parts.
    pub const FOOTERS: Self = Self(1 << 2);
    /// Footnotes parts.
    pub const FOOTNOTES: Self = Self(1 << 3);
    /// Endnotes parts.
    pub const ENDNOTES: Self = Self(1 << 4);
    /// Word comments parts.
    pub const COMMENTS: Self = Self(1 << 5);
    /// Body, headers, and footers (equivalent to the legacy broad scope).
    pub const BODY_HEADERS_FOOTERS: Self = Self(Self::BODY.0 | Self::HEADERS.0 | Self::FOOTERS.0);
    /// Footnotes and endnotes.
    pub const NOTES: Self = Self(Self::FOOTNOTES.0 | Self::ENDNOTES.0);
    /// Every supported editable Word story.
    pub const ALL: Self = Self(Self::BODY_HEADERS_FOOTERS.0 | Self::NOTES.0 | Self::COMMENTS.0);

    /// Combine two selections.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether this selection contains `kind`.
    #[must_use]
    pub const fn contains(self, kind: EditableStoryKind) -> bool {
        let bit = match kind {
            EditableStoryKind::Body => Self::BODY.0,
            EditableStoryKind::Header => Self::HEADERS.0,
            EditableStoryKind::Footer => Self::FOOTERS.0,
            EditableStoryKind::Footnote => Self::FOOTNOTES.0,
            EditableStoryKind::Endnote => Self::ENDNOTES.0,
            EditableStoryKind::Comment => Self::COMMENTS.0,
        };
        self.0 & bit != 0
    }
}

impl std::ops::BitOr for EditableStorySelection {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for EditableStorySelection {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

impl From<StoryScope> for EditableStorySelection {
    fn from(scope: StoryScope) -> Self {
        match scope {
            StoryScope::Body => Self::BODY,
            StoryScope::HeadersFooters => Self::HEADERS | Self::FOOTERS,
            StoryScope::BodyHeadersFooters => Self::BODY_HEADERS_FOOTERS,
        }
    }
}

/// One parsed Word story DOM.
pub struct StoryEditor {
    name: String,
    kind: StoryKind,
    document: docxtpl_xml::XmlDocument,
    changed: bool,
    validate_internal_links: bool,
    external_hyperlink_rids: HashSet<String>,
}

/// One parsed Word story DOM from the unified 1.3 editing API.
///
/// This type extends story editing to footnotes, endnotes, and comments while
/// leaving the 1.2 [`StoryEditor`] and its exhaustively matchable enums intact.
pub struct EditableStoryEditor {
    kind: EditableStoryKind,
    pub(crate) inner: StoryEditor,
}

impl EditableStoryEditor {
    /// OPC part name of this story.
    #[must_use]
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Unified story kind.
    #[must_use]
    pub const fn kind(&self) -> EditableStoryKind {
        self.kind
    }

    /// Read the parsed DOM without marking it for serialization.
    #[must_use]
    pub const fn document(&self) -> &docxtpl_xml::XmlDocument {
        self.inner.document()
    }

    /// Mutably access the DOM and mark this story for one final serialization.
    pub fn document_mut(&mut self) -> &mut docxtpl_xml::XmlDocument {
        self.inner.document_mut()
    }

    /// Whether mutable DOM access has requested serialization.
    #[must_use]
    pub const fn is_changed(&self) -> bool {
        self.inner.is_changed()
    }

    /// Find a bookmark by name, or create one around `target`.
    pub fn get_or_create_bookmark(
        &mut self,
        target: docxtpl_xml::NodeId,
        preferred_name: &str,
    ) -> Result<Bookmark, Error> {
        self.inner.get_or_create_bookmark(target, preferred_name)
    }

    /// Attach an internal hyperlink to an existing run.
    pub fn attach_internal_link(
        &mut self,
        target: docxtpl_xml::NodeId,
        bookmark: &Bookmark,
    ) -> Result<docxtpl_xml::NodeId, Error> {
        self.inner.attach_internal_link(target, bookmark)
    }

    /// Attach an external hyperlink relationship id to an existing DrawingML
    /// drawing in this story.
    pub fn attach_drawing_external_link(
        &mut self,
        drawing: docxtpl_xml::NodeId,
        relationship_id: &str,
    ) -> Result<(), Error> {
        self.inner
            .attach_drawing_external_link(drawing, relationship_id)
    }
}

/// A bookmark target created or found inside one Word story.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// Numeric `w:id` shared by `bookmarkStart` and `bookmarkEnd`.
    pub id: String,
    /// Unique `w:name` used by internal hyperlink anchors.
    pub name: String,
}

impl StoryEditor {
    /// OPC part name of this story.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Story kind, which selects the correct serialization behavior.
    #[must_use]
    pub const fn kind(&self) -> StoryKind {
        self.kind
    }

    /// Read the parsed DOM without marking it for serialization.
    #[must_use]
    pub const fn document(&self) -> &docxtpl_xml::XmlDocument {
        &self.document
    }

    /// Mutably access the DOM and mark this story for one final serialization.
    pub fn document_mut(&mut self) -> &mut docxtpl_xml::XmlDocument {
        self.changed = true;
        &mut self.document
    }

    /// Whether mutable DOM access has requested serialization.
    #[must_use]
    pub const fn is_changed(&self) -> bool {
        self.changed
    }

    /// Find a bookmark by name, or create one around `target`.
    ///
    /// New ids fill the first non-negative numeric gap. Names are normalized
    /// to Word-safe characters, limited to 40 characters, and suffixed
    /// deterministically when needed. Repeating the call with an existing
    /// normalized name returns that bookmark without changing the DOM.
    pub fn get_or_create_bookmark(
        &mut self,
        target: docxtpl_xml::NodeId,
        preferred_name: &str,
    ) -> Result<Bookmark, Error> {
        self.validate_internal_links = true;
        let starts = bookmark_starts(&self.document);
        let base_name = normalize_bookmark_name(preferred_name);
        if let Some((_, id, name)) = starts.iter().find(|(_, _, name)| *name == base_name) {
            return Ok(Bookmark {
                id: id.clone(),
                name: name.clone(),
            });
        }

        let used_ids: HashSet<u64> = starts
            .iter()
            .filter_map(|(_, id, _)| id.parse().ok())
            .collect();
        let mut numeric_id = 0u64;
        while used_ids.contains(&numeric_id) {
            numeric_id = numeric_id.saturating_add(1);
        }
        let used_names: HashSet<&str> = starts.iter().map(|(_, _, name)| name.as_str()).collect();
        let name = unique_bookmark_name(&base_name, &used_names);
        let parent = self
            .document
            .parent(target)
            .ok_or_else(|| OpcError::Malformed {
                reason: "bookmark target must be attached below the story root".to_string(),
            })?;
        let position = self
            .document
            .children(parent)
            .iter()
            .position(|node| *node == target)
            .ok_or_else(|| OpcError::Malformed {
                reason: "bookmark target is not attached to its reported parent".to_string(),
            })?;
        let id = numeric_id.to_string();
        let start = self
            .document
            .new_w_element(
                "bookmarkStart",
                vec![("id".into(), id.clone()), ("name".into(), name.clone())],
            )
            .map_err(|error| OpcError::Malformed {
                reason: error.to_string(),
            })?;
        let end = self
            .document
            .new_w_element("bookmarkEnd", vec![("id".into(), id.clone())])
            .map_err(|error| OpcError::Malformed {
                reason: error.to_string(),
            })?;
        self.document.insert_child_at(parent, position, start);
        self.document.insert_child_at(parent, position + 2, end);
        self.changed = true;
        Ok(Bookmark { id, name })
    }

    /// Wrap `target` in an internal `w:hyperlink` to `bookmark`.
    ///
    /// An existing parent hyperlink with the same anchor is reused. Nesting a
    /// hyperlink inside a different hyperlink is rejected.
    pub fn attach_internal_link(
        &mut self,
        target: docxtpl_xml::NodeId,
        bookmark: &Bookmark,
    ) -> Result<docxtpl_xml::NodeId, Error> {
        self.validate_internal_links = true;
        let parent = self
            .document
            .parent(target)
            .ok_or_else(|| OpcError::Malformed {
                reason: "internal-link target must be attached below the story root".to_string(),
            })?;
        if is_word_element(&self.document, parent, "hyperlink") {
            if self.document.attr(parent, docxtpl_xml::ns_uri::W, "anchor")
                == Some(bookmark.name.as_str())
            {
                return Ok(parent);
            }
            return Err(OpcError::Malformed {
                reason: "cannot nest an internal hyperlink inside another hyperlink".to_string(),
            }
            .into());
        }
        let position = self
            .document
            .children(parent)
            .iter()
            .position(|node| *node == target)
            .ok_or_else(|| OpcError::Malformed {
                reason: "internal-link target is not attached to its reported parent".to_string(),
            })?;
        let hyperlink = self
            .document
            .new_w_element("hyperlink", vec![("anchor".into(), bookmark.name.clone())])
            .map_err(|error| OpcError::Malformed {
                reason: error.to_string(),
            })?;
        self.document.insert_child_at(parent, position, hyperlink);
        self.document.append_child(hyperlink, target);
        self.changed = true;
        Ok(hyperlink)
    }

    /// Attach or replace the external hyperlink on an existing `w:drawing`.
    ///
    /// The relationship id must be an external hyperlink relationship owned
    /// by this story. The link is written to `wp:docPr` and every picture
    /// `pic:cNvPr` below the drawing, matching Word's clickable-picture shape.
    /// Repeating the call with the same id does not change the DOM.
    pub fn attach_drawing_external_link(
        &mut self,
        drawing: docxtpl_xml::NodeId,
        relationship_id: &str,
    ) -> Result<(), Error> {
        if !is_word_element(&self.document, drawing, "drawing") {
            return Err(OpcError::Malformed {
                reason: "external-link target must be a w:drawing element".to_string(),
            }
            .into());
        }
        if !self.external_hyperlink_rids.contains(relationship_id) {
            return Err(OpcError::Malformed {
                reason: format!(
                    "story {:?} has no external hyperlink relationship {relationship_id:?}",
                    self.name
                ),
            }
            .into());
        }
        let properties: Vec<_> = self
            .document
            .descendants(drawing)
            .into_iter()
            .filter(|node| {
                self.document.tag(*node).is_some_and(|tag| {
                    (tag.ns == docxtpl_xml::ns_uri::WP && tag.local == "docPr")
                        || (tag.ns == docxtpl_xml::ns_uri::PIC && tag.local == "cNvPr")
                })
            })
            .collect();
        if !properties.iter().any(|node| {
            self.document
                .tag(*node)
                .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::WP && tag.local == "docPr")
        }) {
            return Err(OpcError::Malformed {
                reason: "w:drawing has no wp:docPr element".to_string(),
            }
            .into());
        }

        let mut changed = false;
        for property in properties {
            let links: Vec<_> = self
                .document
                .children(property)
                .iter()
                .copied()
                .filter(|node| {
                    self.document.tag(*node).is_some_and(|tag| {
                        tag.ns == docxtpl_xml::ns_uri::A && tag.local == "hlinkClick"
                    })
                })
                .collect();
            if links.len() > 1 {
                return Err(OpcError::Malformed {
                    reason: "drawing properties contain duplicate a:hlinkClick elements"
                        .to_string(),
                }
                .into());
            }
            if let Some(link) = links.first().copied() {
                if self.document.attr(link, docxtpl_xml::ns_uri::R, "id") != Some(relationship_id) {
                    self.document.set_attr(
                        link,
                        docxtpl_xml::ns_uri::R,
                        "id",
                        relationship_id.to_string(),
                    );
                    changed = true;
                }
            } else {
                let link = self
                    .document
                    .new_prefixed_element("a", docxtpl_xml::ns_uri::A, "hlinkClick", Vec::new())
                    .map_err(|error| OpcError::Malformed {
                        reason: error.to_string(),
                    })?;
                self.document.set_attr(
                    link,
                    docxtpl_xml::ns_uri::R,
                    "id",
                    relationship_id.to_string(),
                );
                self.document.append_child(property, link);
                changed = true;
            }
        }
        self.changed |= changed;
        Ok(())
    }

    fn validate_bookmarks_and_internal_links(&self) -> Result<(), Error> {
        let starts = bookmark_starts(&self.document);
        let start_count = self
            .document
            .descendants(self.document.root())
            .into_iter()
            .filter(|node| is_word_element(&self.document, *node, "bookmarkStart"))
            .count();
        if starts.len() != start_count {
            return Err(OpcError::Malformed {
                reason: format!(
                    "story {:?} has bookmarkStart without w:id or w:name",
                    self.name
                ),
            }
            .into());
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for (_, id, name) in &starts {
            if !ids.insert(id.as_str()) {
                return Err(OpcError::Malformed {
                    reason: format!(
                        "story {:?} contains duplicate bookmark id {id:?}",
                        self.name
                    ),
                }
                .into());
            }
            if !names.insert(name.as_str()) {
                return Err(OpcError::Malformed {
                    reason: format!(
                        "story {:?} contains duplicate bookmark name {name:?}",
                        self.name
                    ),
                }
                .into());
            }
        }
        let mut end_ids = HashSet::new();
        for node in self.document.descendants(self.document.root()) {
            if is_word_element(&self.document, node, "bookmarkEnd") {
                let id = self
                    .document
                    .attr(node, docxtpl_xml::ns_uri::W, "id")
                    .ok_or_else(|| OpcError::Malformed {
                        reason: format!("story {:?} has bookmarkEnd without w:id", self.name),
                    })?;
                if !end_ids.insert(id) || !ids.contains(id) {
                    return Err(OpcError::Malformed {
                        reason: format!(
                            "story {:?} has unmatched bookmarkEnd id {id:?}",
                            self.name
                        ),
                    }
                    .into());
                }
            }
        }
        if let Some(id) = ids.iter().find(|id| !end_ids.contains(**id)) {
            return Err(OpcError::Malformed {
                reason: format!(
                    "story {:?} has unmatched bookmarkStart id {id:?}",
                    self.name
                ),
            }
            .into());
        }
        for node in self.document.descendants(self.document.root()) {
            if is_word_element(&self.document, node, "hyperlink") {
                if let Some(anchor) = self.document.attr(node, docxtpl_xml::ns_uri::W, "anchor") {
                    if !names.contains(anchor) {
                        return Err(OpcError::Malformed {
                            reason: format!(
                                "story {:?} has internal hyperlink to missing bookmark {anchor:?}",
                                self.name
                            ),
                        }
                        .into());
                    }
                }
            }
        }
        Ok(())
    }
}

fn is_word_element(
    document: &docxtpl_xml::XmlDocument,
    node: docxtpl_xml::NodeId,
    local: &str,
) -> bool {
    document
        .tag(node)
        .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == local)
}

fn bookmark_starts(
    document: &docxtpl_xml::XmlDocument,
) -> Vec<(docxtpl_xml::NodeId, String, String)> {
    document
        .descendants(document.root())
        .into_iter()
        .filter(|node| is_word_element(document, *node, "bookmarkStart"))
        .filter_map(|node| {
            Some((
                node,
                document
                    .attr(node, docxtpl_xml::ns_uri::W, "id")?
                    .to_string(),
                document
                    .attr(node, docxtpl_xml::ns_uri::W, "name")?
                    .to_string(),
            ))
        })
        .collect()
}

fn normalize_bookmark_name(preferred: &str) -> String {
    let mut name: String = preferred
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
    if name.is_empty() {
        name.push_str("bookmark");
    }
    if !name
        .chars()
        .next()
        .is_some_and(|character| character.is_alphabetic() || character == '_')
    {
        name.insert(0, '_');
        name.truncate(40);
    }
    name
}

fn unique_bookmark_name(base: &str, used: &HashSet<&str>) -> String {
    if !used.contains(base) {
        return base.to_string();
    }
    for number in 2u64.. {
        let suffix = format!("_{number}");
        let keep = 40usize.saturating_sub(suffix.chars().count());
        let mut candidate: String = base.chars().take(keep).collect();
        candidate.push_str(&suffix);
        if !used.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!("u64 bookmark suffix space exhausted")
}

/// Counters from one `for_each_story` operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryEditReport {
    /// Parts parsed into a DOM.
    pub parsed_parts: usize,
    /// Parts serialized after mutable access.
    pub serialized_parts: usize,
    /// Parts whose serialized bytes differed and were written to the transaction.
    pub changed_parts: Vec<String>,
}

/// Behavior when a post-processing pass returns an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePolicy {
    /// Roll back the failing pass and return its error immediately.
    Abort,
    /// Roll back the failing pass, record a warning, and continue the pipeline.
    WarnAndRollback,
}

/// A recoverable post-processing warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostprocessWarning {
    /// Name of the pass that failed.
    pub pass: String,
    /// Stable warning category.
    pub code: &'static str,
    /// Human-readable error detail.
    pub message: String,
}

/// Result of one successfully committed or recoverably rolled-back pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassReport {
    /// Caller-assigned pass name.
    pub name: String,
    /// Whether changes from this pass were committed.
    pub changed: bool,
    /// Whether attempted changes were rolled back.
    pub rolled_back: bool,
    /// Existing or newly-added parts touched by the pass.
    pub touched_parts: Vec<String>,
    /// Recoverable warnings emitted by this pass.
    pub warnings: Vec<PostprocessWarning>,
    /// Wall-clock duration of the pass, including rollback when applicable.
    pub elapsed: Duration,
}

/// Aggregate result of a post-processing pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostprocessReport {
    /// Pass reports in execution order.
    pub passes: Vec<PassReport>,
}

/// A sequential, pass-transactional editor for one rendered document.
pub struct PostprocessPipeline<'a> {
    package: &'a mut Package,
    max_rendered_xml_bytes: usize,
    reports: Vec<PassReport>,
    control: Option<RenderControl>,
}

/// Transactional operations available to one post-processing pass.
pub struct PostprocessTransaction<'transaction, 'package> {
    transaction: &'transaction mut PackageTransaction<'package>,
    max_rendered_xml_bytes: usize,
    media_registry: Option<MediaRegistry>,
    control: Option<RenderControl>,
}

#[derive(Default)]
struct MediaRegistry {
    by_digest: HashMap<docxtpl_rich::ImageDigest, String>,
    used_numbers: BTreeSet<u64>,
}

/// Result of registering an image in `word/media`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRegistration {
    /// Package part name, such as `word/media/image3.png`.
    pub part_name: String,
    /// Whether the image bytes already existed in the package or this pass.
    pub reused: bool,
}

impl PostprocessTransaction<'_, '_> {
    /// Check unified cancellation and deadline control. Long-running custom
    /// passes should call this at their own natural checkpoints.
    pub fn check_control(&self) -> Result<(), Error> {
        check_render_control(self.control.as_ref())
    }

    /// Check the pipeline's cooperative cancellation token. Long-running
    /// custom passes should call this at their own natural checkpoints. This
    /// compatibility method also observes a configured deadline.
    pub fn check_cancelled(&self) -> Result<(), Error> {
        self.check_control()
    }

    /// Read a part without marking it as touched.
    pub fn part(&self, name: &str) -> Option<&docxtpl_opc::Part> {
        self.transaction.part(name)
    }

    /// Replace an existing part with in-memory bytes.
    pub fn set_part_bytes(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), Error> {
        self.transaction.set_part_bytes(name, bytes)?;
        Ok(())
    }

    /// Replace an existing part with a verified file-backed source.
    pub fn set_file_backed_part(
        &mut self,
        name: &str,
        source: docxtpl_opc::FilePartSource,
    ) -> Result<(), Error> {
        self.transaction.set_file_backed_part(name, source)?;
        Ok(())
    }

    /// Append a byte-backed part.
    pub fn add_part(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), Error> {
        self.transaction.add_part(name, bytes)?;
        Ok(())
    }

    /// Append a file-backed part.
    pub fn add_file_backed_part(
        &mut self,
        name: &str,
        source: docxtpl_opc::FilePartSource,
    ) -> Result<(), Error> {
        self.transaction.add_file_backed_part(name, source)?;
        Ok(())
    }

    /// Return an existing relationship id or register a new one for `owner`.
    pub fn relate(
        &mut self,
        owner: &str,
        rel_type: &str,
        target: &str,
        target_mode: TargetMode,
    ) -> Result<String, Error> {
        Ok(self
            .transaction
            .get_or_add_relationship(owner, rel_type, target, target_mode)?)
    }

    /// Register or reuse an external hyperlink relationship for one story.
    pub fn relate_external_hyperlink(&mut self, owner: &str, url: &str) -> Result<String, Error> {
        if url.is_empty() {
            return Err(OpcError::Malformed {
                reason: "external hyperlink URL must not be empty".to_string(),
            }
            .into());
        }
        self.relate(owner, HYPERLINK_REL_TYPE, url, TargetMode::External)
    }

    /// Register a path-backed image as a file-backed media part.
    ///
    /// Existing media is deduplicated by SHA-1, while new names fill the first
    /// available `imageN` slot deterministically. Content Types are updated in
    /// the same transaction.
    pub fn register_media_path(&mut self, path: &str) -> Result<MediaRegistration, Error> {
        let image = InlineImage::from_path_lazy(path, None, None, None)?;
        let (info, digest) = image.probe_with_digest()?;
        if self.media_registry.is_none() {
            let mut registry = MediaRegistry::default();
            for part in self.transaction.package().parts() {
                if !part.name().starts_with("word/media/") {
                    continue;
                }
                registry
                    .by_digest
                    .entry(docxtpl_rich::sha1_digest(part.bytes()?))
                    .or_insert_with(|| part.name().to_string());
                if let Some(number) = images::image_number(part.name()) {
                    registry.used_numbers.insert(number);
                }
            }
            self.media_registry = Some(registry);
        }
        let registry = self.media_registry.as_mut().expect("initialized above");
        if let Some(part_name) = registry.by_digest.get(&digest) {
            return Ok(MediaRegistration {
                part_name: part_name.clone(),
                reused: true,
            });
        }

        let mut number = 1u64;
        while registry.used_numbers.contains(&number) {
            number = number.saturating_add(1);
        }
        let part_name = format!("word/media/image{number}.{}", info.ext);
        let source = docxtpl_opc::FilePartSource::snapshot_with_digest(path, digest)?;
        self.transaction.add_file_backed_part(&part_name, source)?;
        self.transaction
            .register_content_type(&part_name, info.content_type)?;
        registry.used_numbers.insert(number);
        registry.by_digest.insert(digest, part_name.clone());
        Ok(MediaRegistration {
            part_name,
            reused: false,
        })
    }

    /// Relate an owner part to registered image media, reusing an exact
    /// existing relationship when present.
    pub fn relate_image(
        &mut self,
        owner: &str,
        media: &MediaRegistration,
    ) -> Result<String, Error> {
        let target = images::relative_to_owner(owner, &media.part_name);
        self.relate(owner, IMAGE_REL_TYPE, &target, TargetMode::Internal)
    }

    /// Validate the current pass state before it is committed.
    pub fn validate(&self) -> Result<(), Error> {
        self.transaction.validate()?;
        Ok(())
    }

    /// Parse each selected story once, run all caller edits on its shared DOM,
    /// and serialize that story at most once.
    pub fn for_each_story(
        &mut self,
        scope: StoryScope,
        mut edit: impl FnMut(&mut StoryEditor) -> Result<(), Error>,
    ) -> Result<StoryEditReport, Error> {
        self.check_cancelled()?;
        let stories = editable_story_parts(self.transaction.package(), scope)?;
        let mut report = StoryEditReport::default();
        for (name, kind) in stories {
            self.check_cancelled()?;
            let external_hyperlink_rids = self
                .transaction
                .package()
                .relationships_of(&name)
                .into_iter()
                .flat_map(|relationships| relationships.iter())
                .filter(|relationship| {
                    relationship.rel_type == HYPERLINK_REL_TYPE
                        && relationship.target_mode == TargetMode::External
                })
                .map(|relationship| relationship.id.clone())
                .collect();
            let bytes = self
                .transaction
                .part(&name)
                .ok_or_else(|| OpcError::MissingPart { uri: name.clone() })?
                .bytes()?;
            let xml = decode_xml_bytes(bytes, &name)?;
            let document =
                docxtpl_xml::XmlDocument::parse_strict(&xml, &docxtpl_xml::XmlLimits::default())
                    .map_err(|source| RenderError::Xml {
                        part: name.clone(),
                        source,
                    })?;
            report.parsed_parts += 1;
            let mut story = StoryEditor {
                name: name.clone(),
                kind,
                document,
                changed: false,
                validate_internal_links: false,
                external_hyperlink_rids,
            };
            edit(&mut story)?;
            self.check_cancelled()?;
            if story.validate_internal_links {
                story.validate_bookmarks_and_internal_links()?;
            }
            if !story.changed {
                continue;
            }
            let serialized = match kind {
                StoryKind::Body => story.document.try_serialize(self.max_rendered_xml_bytes),
                StoryKind::Header | StoryKind::Footer => story
                    .document
                    .try_serialize_story(self.max_rendered_xml_bytes),
            }
            .map_err(|_| RenderError::Limit {
                part: name.clone(),
                kind: "rendered_xml_bytes",
                max: self.max_rendered_xml_bytes as u64,
            })?;
            report.serialized_parts += 1;
            if serialized.as_bytes() != bytes {
                self.transaction
                    .set_part_bytes(&name, serialized.into_bytes())?;
                report.changed_parts.push(name);
            }
        }
        self.check_cancelled()?;
        Ok(report)
    }

    /// Parse each selected Word story once, run all caller edits on its shared
    /// DOM, and serialize that story at most once.
    ///
    /// Unlike the legacy [`Self::for_each_story`] entry point, this unified
    /// 1.3 API can also select footnotes, endnotes, and Word comments. Stories
    /// are visited in body, header, footer, footnote, endnote, comment order;
    /// multiple parts of one kind use deterministic package names.
    pub fn for_each_editable_story(
        &mut self,
        selection: EditableStorySelection,
        mut edit: impl FnMut(&mut EditableStoryEditor) -> Result<(), Error>,
    ) -> Result<StoryEditReport, Error> {
        self.check_cancelled()?;
        let stories = unified_editable_story_parts(self.transaction.package(), selection)?;
        let mut report = StoryEditReport::default();
        for (name, kind) in stories {
            self.check_cancelled()?;
            let external_hyperlink_rids = self
                .transaction
                .package()
                .relationships_of(&name)
                .into_iter()
                .flat_map(|relationships| relationships.iter())
                .filter(|relationship| {
                    relationship.rel_type == HYPERLINK_REL_TYPE
                        && relationship.target_mode == TargetMode::External
                })
                .map(|relationship| relationship.id.clone())
                .collect();
            let bytes = self
                .transaction
                .part(&name)
                .ok_or_else(|| OpcError::MissingPart { uri: name.clone() })?
                .bytes()?;
            let xml = decode_xml_bytes(bytes, &name)?;
            let document =
                docxtpl_xml::XmlDocument::parse_strict(&xml, &docxtpl_xml::XmlLimits::default())
                    .map_err(|source| RenderError::Xml {
                        part: name.clone(),
                        source,
                    })?;
            report.parsed_parts += 1;
            let legacy_kind = match kind {
                EditableStoryKind::Header => StoryKind::Header,
                EditableStoryKind::Footer => StoryKind::Footer,
                EditableStoryKind::Body
                | EditableStoryKind::Footnote
                | EditableStoryKind::Endnote
                | EditableStoryKind::Comment => StoryKind::Body,
            };
            let mut story = EditableStoryEditor {
                kind,
                inner: StoryEditor {
                    name: name.clone(),
                    kind: legacy_kind,
                    document,
                    changed: false,
                    validate_internal_links: false,
                    external_hyperlink_rids,
                },
            };
            edit(&mut story)?;
            self.check_cancelled()?;
            if story.inner.validate_internal_links {
                story.inner.validate_bookmarks_and_internal_links()?;
            }
            if !story.inner.changed {
                continue;
            }
            let serialized = match kind {
                EditableStoryKind::Body => story
                    .inner
                    .document
                    .try_serialize(self.max_rendered_xml_bytes),
                EditableStoryKind::Header
                | EditableStoryKind::Footer
                | EditableStoryKind::Footnote
                | EditableStoryKind::Endnote
                | EditableStoryKind::Comment => story
                    .inner
                    .document
                    .try_serialize_story(self.max_rendered_xml_bytes),
            }
            .map_err(|_| RenderError::Limit {
                part: name.clone(),
                kind: "rendered_xml_bytes",
                max: self.max_rendered_xml_bytes as u64,
            })?;
            report.serialized_parts += 1;
            if serialized.as_bytes() != bytes {
                self.transaction
                    .set_part_bytes(&name, serialized.into_bytes())?;
                report.changed_parts.push(name);
            }
        }
        self.check_cancelled()?;
        Ok(report)
    }
}

impl PostprocessPipeline<'_> {
    /// Execute one pass in an isolated package transaction.
    pub fn pass(
        &mut self,
        name: impl Into<String>,
        policy: FailurePolicy,
        pass: impl FnOnce(&mut PostprocessTransaction<'_, '_>) -> Result<(), Error>,
    ) -> Result<&mut Self, Error> {
        check_render_control(self.control.as_ref())?;
        let name = name.into();
        let started = Instant::now();
        let mut transaction = self.package.transaction();
        let result = {
            let mut postprocess = PostprocessTransaction {
                transaction: &mut transaction,
                max_rendered_xml_bytes: self.max_rendered_xml_bytes,
                media_registry: None,
                control: self.control.clone(),
            };
            pass(&mut postprocess).and_then(|()| postprocess.check_cancelled())
        };
        match result {
            Ok(_) => {
                let changed = transaction.changed();
                let touched_parts = transaction.touched_parts();
                transaction.commit();
                self.reports.push(PassReport {
                    name,
                    changed,
                    rolled_back: false,
                    touched_parts,
                    warnings: Vec::new(),
                    elapsed: started.elapsed(),
                });
                Ok(self)
            }
            Err(error) => {
                let touched_parts = transaction.touched_parts();
                transaction.rollback();
                if self.control.as_ref().is_some_and(RenderControl::is_stopped) {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "render control stopped the operation",
                    )));
                }
                match policy {
                    FailurePolicy::Abort => Err(error),
                    FailurePolicy::WarnAndRollback => {
                        let warning = PostprocessWarning {
                            pass: name.clone(),
                            code: "pass_rolled_back",
                            message: error.to_string(),
                        };
                        self.reports.push(PassReport {
                            name,
                            changed: false,
                            rolled_back: true,
                            touched_parts,
                            warnings: vec![warning],
                            elapsed: started.elapsed(),
                        });
                        Ok(self)
                    }
                }
            }
        }
    }
}

impl RenderedDocument {
    /// Return current package part-buffer residency without materializing
    /// additional parts.
    #[must_use]
    pub fn residency(&self) -> PackageResidency {
        self.pkg.residency()
    }

    /// Release clean lazy part buffers that remain reloadable from the source
    /// template ZIP.
    pub fn evict_clean_part_caches(&mut self) -> PackageEvictionReport {
        self.pkg.evict_clean_part_caches()
    }

    /// Run a sequence of rollback-capable post-processing passes.
    ///
    /// Successfully completed passes remain committed. A failing pass always
    /// rolls back its own changes; [`FailurePolicy`] controls whether the
    /// pipeline aborts or records a warning and continues.
    pub fn postprocess(
        &mut self,
        configure: impl FnOnce(&mut PostprocessPipeline<'_>) -> Result<(), Error>,
    ) -> Result<PostprocessReport, Error> {
        self.postprocess_checked(configure, None)
    }

    /// Run rollback-capable passes with cooperative cancellation. A cancelled
    /// active pass is always rolled back, regardless of its failure policy.
    pub fn postprocess_with_cancellation(
        &mut self,
        cancellation: &CancellationToken,
        configure: impl FnOnce(&mut PostprocessPipeline<'_>) -> Result<(), Error>,
    ) -> Result<PostprocessReport, CancellationError> {
        self.postprocess_checked(
            configure,
            Some(RenderControl::from_cancellation(cancellation)),
        )
        .map_err(|error| CancellationError::from_operation(error, cancellation))
    }

    /// Run rollback-capable passes with unified cancellation and deadline
    /// control. A stopped active pass is always rolled back.
    pub fn postprocess_with_control(
        &mut self,
        control: &RenderControl,
        configure: impl FnOnce(&mut PostprocessPipeline<'_>) -> Result<(), Error>,
    ) -> Result<PostprocessReport, RenderControlError> {
        self.postprocess_checked(configure, Some(control.clone()))
            .map_err(|error| RenderControlError::from_operation(error, control))
    }

    fn postprocess_checked(
        &mut self,
        configure: impl FnOnce(&mut PostprocessPipeline<'_>) -> Result<(), Error>,
        control: Option<RenderControl>,
    ) -> Result<PostprocessReport, Error> {
        check_render_control(control.as_ref())?;
        self.edited = true;
        let mut pipeline = PostprocessPipeline {
            package: &mut self.pkg,
            max_rendered_xml_bytes: self.max_rendered_xml_bytes,
            reports: Vec::new(),
            control,
        };
        let result = configure(&mut pipeline);
        let reports = std::mem::take(&mut pipeline.reports);
        drop(pipeline);
        self.postprocess_passes.extend(reports.clone());
        result?;
        Ok(PostprocessReport { passes: reports })
    }

    /// Edit the rendered OPC package before its final serialization.
    ///
    /// The document is marked as edited before the callback runs, including
    /// when the callback returns an error after making partial changes. Every
    /// subsequent output operation revalidates package integrity first.
    /// This low-level entry point does not provide rollback; use a transactional
    /// post-processing pipeline when pass-level rollback is required.
    pub fn edit_package<T>(
        &mut self,
        edit: impl FnOnce(&mut Package) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.edited = true;
        edit(&mut self.pkg)
    }

    /// Whether the rendered package has entered the explicit editing path.
    #[must_use]
    pub const fn is_edited(&self) -> bool {
        self.edited
    }

    fn validate_for_output(&self) -> Result<(), Error> {
        self.pkg.validate()?;
        Ok(())
    }

    /// Save to a file (unchanged parts keep their original bytes, see ADR-002 DEV-0004).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        self.validate_for_output()?;
        self.pkg.save(path)?;
        Ok(())
    }

    /// Save to a file using explicit ZIP serialization options.
    pub fn save_with_options(
        &self,
        path: impl AsRef<Path>,
        options: &WriteOptions,
    ) -> Result<(), Error> {
        self.validate_for_output()?;
        self.pkg.save_with_options(path, options)?;
        Ok(())
    }

    /// Validate and atomically save with cooperative cancellation. The old
    /// destination remains untouched when cancellation interrupts ZIP output.
    pub fn save_with_cancellation(
        &self,
        path: impl AsRef<Path>,
        cancellation: &CancellationToken,
    ) -> Result<(), CancellationError> {
        self.save_with_options_and_cancellation(path, &WriteOptions::compatible(), cancellation)
    }

    /// Save with explicit ZIP options and cooperative cancellation.
    pub fn save_with_options_and_cancellation(
        &self,
        path: impl AsRef<Path>,
        options: &WriteOptions,
        cancellation: &CancellationToken,
    ) -> Result<(), CancellationError> {
        self.save_with_report_and_cancellation(path, options, cancellation)
            .map(|_| ())
    }

    /// Validate and atomically save with unified cancellation and deadline
    /// control. The old destination remains untouched when output stops.
    pub fn save_with_control(
        &self,
        path: impl AsRef<Path>,
        control: &RenderControl,
    ) -> Result<(), RenderControlError> {
        self.save_with_options_and_control(path, &WriteOptions::compatible(), control)
    }

    /// Save with explicit ZIP options and unified execution control.
    pub fn save_with_options_and_control(
        &self,
        path: impl AsRef<Path>,
        options: &WriteOptions,
        control: &RenderControl,
    ) -> Result<(), RenderControlError> {
        self.save_with_report_and_control(path, options, control)
            .map(|_| ())
    }

    /// Validate and save the document while returning structured render and
    /// ZIP serialization metrics.
    pub fn save_with_report(
        &self,
        path: impl AsRef<Path>,
        options: &WriteOptions,
    ) -> Result<RenderReport, Error> {
        let validation_started = Instant::now();
        self.pkg.validate()?;
        let validation_elapsed = validation_started.elapsed();
        let write_started = Instant::now();
        let package_write = self.pkg.save_with_report(path, options)?;
        let zip_write_elapsed = write_started.elapsed();
        Ok(self.render_report(validation_elapsed, zip_write_elapsed, package_write))
    }

    /// Validate and atomically save with structured metrics and cooperative
    /// cancellation checkpoints through ZIP serialization.
    pub fn save_with_report_and_cancellation(
        &self,
        path: impl AsRef<Path>,
        options: &WriteOptions,
        cancellation: &CancellationToken,
    ) -> Result<RenderReport, CancellationError> {
        cancellation.check()?;
        let validation_started = Instant::now();
        self.pkg
            .validate()
            .map_err(|error| CancellationError::Operation(Error::Opc(error)))?;
        cancellation.check()?;
        let validation_elapsed = validation_started.elapsed();
        let write_started = Instant::now();
        let package_write = self
            .pkg
            .save_with_report_interruptible(path, options, &|| cancellation.is_cancelled())
            .map_err(map_interruptible_opc_error)?;
        let zip_write_elapsed = write_started.elapsed();
        cancellation.check()?;
        Ok(self.render_report(validation_elapsed, zip_write_elapsed, package_write))
    }

    /// Validate and atomically save with metrics and unified execution
    /// control.
    pub fn save_with_report_and_control(
        &self,
        path: impl AsRef<Path>,
        options: &WriteOptions,
        control: &RenderControl,
    ) -> Result<RenderReport, RenderControlError> {
        control.check()?;
        let validation_started = Instant::now();
        self.pkg
            .validate()
            .map_err(|error| RenderControlError::Operation(Error::Opc(error)))?;
        control.check()?;
        let validation_elapsed = validation_started.elapsed();
        let write_started = Instant::now();
        let package_write = self
            .pkg
            .save_with_report_interruptible(path, options, &|| control.is_stopped())
            .map_err(|error| map_interruptible_control_error(error, control))?;
        let zip_write_elapsed = write_started.elapsed();
        control.check()?;
        Ok(self.render_report(validation_elapsed, zip_write_elapsed, package_write))
    }

    /// Validate and write to a seekable stream while returning structured metrics.
    pub fn write_to_with_report(
        &self,
        writer: impl Write + std::io::Seek,
        options: &WriteOptions,
    ) -> Result<RenderReport, Error> {
        let validation_started = Instant::now();
        self.pkg.validate()?;
        let validation_elapsed = validation_started.elapsed();
        let write_started = Instant::now();
        let package_write = self.pkg.write_to_with_report(writer, options)?;
        let zip_write_elapsed = write_started.elapsed();
        Ok(self.render_report(validation_elapsed, zip_write_elapsed, package_write))
    }

    /// Validate and write to a stream with metrics and cooperative
    /// cancellation. Unlike file saving, a caller-provided stream can contain
    /// a partial ZIP after cancellation.
    pub fn write_to_with_report_and_cancellation(
        &self,
        writer: impl Write + std::io::Seek,
        options: &WriteOptions,
        cancellation: &CancellationToken,
    ) -> Result<RenderReport, CancellationError> {
        cancellation.check()?;
        let validation_started = Instant::now();
        self.pkg
            .validate()
            .map_err(|error| CancellationError::Operation(Error::Opc(error)))?;
        cancellation.check()?;
        let validation_elapsed = validation_started.elapsed();
        let write_started = Instant::now();
        let package_write = self
            .pkg
            .write_to_with_report_interruptible(writer, options, &|| cancellation.is_cancelled())
            .map_err(map_interruptible_opc_error)?;
        let zip_write_elapsed = write_started.elapsed();
        cancellation.check()?;
        Ok(self.render_report(validation_elapsed, zip_write_elapsed, package_write))
    }

    /// Validate and write to a seekable stream with metrics and unified
    /// execution control. The stream may contain a partial ZIP after stopping.
    pub fn write_to_with_report_and_control(
        &self,
        writer: impl Write + std::io::Seek,
        options: &WriteOptions,
        control: &RenderControl,
    ) -> Result<RenderReport, RenderControlError> {
        control.check()?;
        let validation_started = Instant::now();
        self.pkg
            .validate()
            .map_err(|error| RenderControlError::Operation(Error::Opc(error)))?;
        control.check()?;
        let validation_elapsed = validation_started.elapsed();
        let write_started = Instant::now();
        let package_write = self
            .pkg
            .write_to_with_report_interruptible(writer, options, &|| control.is_stopped())
            .map_err(|error| map_interruptible_control_error(error, control))?;
        let zip_write_elapsed = write_started.elapsed();
        control.check()?;
        Ok(self.render_report(validation_elapsed, zip_write_elapsed, package_write))
    }

    fn render_report(
        &self,
        validation_elapsed: Duration,
        zip_write_elapsed: Duration,
        package_write: PackageWriteReport,
    ) -> RenderReport {
        RenderReport {
            package_open_elapsed: self.package_open_elapsed,
            render_elapsed: self.render_elapsed,
            postprocess_elapsed: self
                .postprocess_passes
                .iter()
                .map(|pass| pass.elapsed)
                .sum(),
            validation_elapsed,
            zip_write_elapsed,
            postprocess_passes: self.postprocess_passes.clone(),
            package_write,
        }
    }

    /// Write to any seekable writer.
    pub fn write_to(&self, writer: impl Write + std::io::Seek) -> Result<(), Error> {
        self.validate_for_output()?;
        self.pkg.write_to(writer)?;
        Ok(())
    }

    /// Write to any seekable writer using explicit ZIP serialization options.
    pub fn write_to_with_options(
        &self,
        writer: impl Write + std::io::Seek,
        options: &WriteOptions,
    ) -> Result<(), Error> {
        self.validate_for_output()?;
        self.pkg.write_to_with_options(writer, options)?;
        Ok(())
    }

    /// Serialize to in-memory docx bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        self.validate_for_output()?;
        let mut buf = Cursor::new(Vec::new());
        self.pkg.write_to(&mut buf)?;
        Ok(buf.into_inner())
    }

    /// Serialize to in-memory DOCX bytes using explicit ZIP options.
    pub fn to_bytes_with_options(&self, options: &WriteOptions) -> Result<Vec<u8>, Error> {
        self.validate_for_output()?;
        let mut buf = Cursor::new(Vec::new());
        self.pkg.write_to_with_options(&mut buf, options)?;
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

fn map_interruptible_opc_error(error: InterruptibleWriteError) -> CancellationError {
    match error {
        InterruptibleWriteError::Cancelled => CancellationError::Cancelled,
        InterruptibleWriteError::Opc(error) => CancellationError::Operation(Error::Opc(error)),
    }
}

fn map_interruptible_control_error(
    error: InterruptibleWriteError,
    control: &RenderControl,
) -> RenderControlError {
    match error {
        InterruptibleWriteError::Cancelled => match control.stop_reason() {
            Some(RenderStopReason::DeadlineExceeded) => RenderControlError::DeadlineExceeded,
            _ => RenderControlError::Cancelled,
        },
        InterruptibleWriteError::Opc(error) => RenderControlError::Operation(Error::Opc(error)),
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

    /// A path-backed image could not be loaded or recognized.
    #[error(transparent)]
    InlineImage(#[from] docxtpl_rich::InlineImageLoadError),
}

pub use docxtpl_rich::{InlineImage, Listing, RichText, RichTextParagraph, RichTextProps};
pub use docxtpl_template::{
    minijinja, EnvironmentConfigurator, RenderOptions, RenderValue, TemplateErrorKind,
};

#[cfg(test)]
mod render_cache_tests {
    use super::*;
    use std::cell::Cell;

    const SOURCE_ONE: &str = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{{ value }}</w:t></w:r></w:p></w:body></w:document>"#;
    const SOURCE_TWO: &str = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{{ other }}</w:t></w:r></w:p></w:body></w:document>"#;

    #[test]
    fn render_cache_reuses_unchanged_source_and_invalidates_changed_source() {
        let cache = TemplateRenderCache::default();
        let options = RenderOptions::compat();
        let template_digest = persistent_cache::digest(b"template");
        let max = options.max_rendered_xml_bytes();
        let preparations = Cell::new(0usize);
        let prepare = |source: &str| {
            preparations.set(preparations.get() + 1);
            prepare_document_xml_template(source, &options)
        };

        let first = cache
            .get_or_prepare(
                &template_digest,
                "word/document.xml",
                CachedPartKind::Document,
                SOURCE_ONE,
                max,
                || prepare(SOURCE_ONE),
            )
            .expect("prepare first source");
        let second = cache
            .get_or_prepare(
                &template_digest,
                "word/document.xml",
                CachedPartKind::Document,
                SOURCE_ONE,
                max,
                || prepare(SOURCE_ONE),
            )
            .expect("reuse first source");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(preparations.get(), 1);

        let changed = cache
            .get_or_prepare(
                &template_digest,
                "word/document.xml",
                CachedPartKind::Document,
                SOURCE_TWO,
                max,
                || prepare(SOURCE_TWO),
            )
            .expect("prepare changed source");
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(preparations.get(), 2);
    }
}
